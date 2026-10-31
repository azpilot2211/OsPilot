//! USB mass storage (USB sticks, card readers, external drives).
//!
//! These use the "Bulk-Only Transport": we send a 31-byte command wrapper
//! containing a SCSI command (the same commands hard drives have used since
//! the 1980s), move the data, then read back a 13-byte status.

use super::xhci::{Controller, Device};
use super::{ControlRequest, read_configuration};
use crate::memory::DmaBuffer;
use alloc::string::String;

const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
/// Largest single transfer we do; bigger reads are split up.
const MAX_TRANSFER: usize = 64 * 1024;

pub struct UsbStorage {
    controller: Controller,
    device: Device,
    bulk_in: u8,
    bulk_out: u8,
    tag: u32,
    command: DmaBuffer,
    data: DmaBuffer,
    pub block_size: u32,
    pub block_count: u64,
    pub name: String,
}

impl UsbStorage {
    /// Look through the USB devices for a storage device and set it up.
    pub fn find(mut controller: Controller) -> Result<Self, &'static str> {
        let devices = controller.enumerate();
        for mut device in devices {
            let Ok((config, interfaces)) = read_configuration(&mut controller, &mut device) else { continue };
            // Class 8 = mass storage, subclass 6 = SCSI, protocol 0x50 = bulk-only.
            let Some(interface) = interfaces.iter().find(|i| i.class == 8 && i.subclass == 6 && i.protocol == 0x50)
            else {
                continue;
            };
            let bulk = |dir_in: bool| {
                interface.endpoints.iter().find(|e| e.2 == 2 && (e.0 & 0x80 != 0) == dir_in).copied()
            };
            let (Some(bulk_in), Some(bulk_out)) = (bulk(true), bulk(false)) else { continue };
            controller.control(&mut device, ControlRequest::set_configuration(config), None)?;
            controller.configure_bulk_endpoints(&mut device, &[(bulk_in.0, bulk_in.1), (bulk_out.0, bulk_out.1)])?;

            let mut storage = Self {
                controller,
                device,
                bulk_in: bulk_in.0,
                bulk_out: bulk_out.0,
                tag: 1,
                command: DmaBuffer::new(64),
                data: DmaBuffer::new(MAX_TRANSFER),
                block_size: 512,
                block_count: 0,
                name: String::new(),
            };
            storage.identify()?;
            return Ok(storage);
        }
        Err("no USB storage device found")
    }

    fn identify(&mut self) -> Result<(), &'static str> {
        // INQUIRY: who are you? Bytes 8..36 hold vendor and product names.
        self.scsi(&[0x12, 0, 0, 0, 36, 0], 36, true)?;
        let raw = &self.data.as_slice()[8..36];
        self.name = raw.iter().map(|&b| if b.is_ascii_graphic() { b as char } else { ' ' }).collect();
        self.name = String::from(self.name.split_whitespace().collect::<alloc::vec::Vec<_>>().join(" ").as_str());

        // Some sticks need a moment after power-up; TEST UNIT READY until
        // they say they're ready.
        for _ in 0..10 {
            if self.scsi(&[0x00, 0, 0, 0, 0, 0], 0, false).is_ok() {
                break;
            }
            crate::task::sleep_ms(100);
        }

        // READ CAPACITY (10): last block number and block size.
        self.scsi(&[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8, true)?;
        let d = self.data.as_slice();
        let last_block = u32::from_be_bytes([d[0], d[1], d[2], d[3]]);
        self.block_size = u32::from_be_bytes([d[4], d[5], d[6], d[7]]);
        self.block_count = last_block as u64 + 1;
        if self.block_size == 0 || self.block_size as usize > MAX_TRANSFER {
            return Err("unsupported block size");
        }
        Ok(())
    }

    /// Read `count` blocks starting at block `lba` into `out`.
    pub fn read_blocks(&mut self, lba: u64, count: usize, out: &mut [u8]) -> Result<(), &'static str> {
        let block_size = self.block_size as usize;
        let per_transfer = MAX_TRANSFER / block_size;
        let mut done = 0;
        while done < count {
            let n = (count - done).min(per_transfer);
            let start = (lba + done as u64) as u32;
            let [a, b, c, d] = start.to_be_bytes();
            let [hi, lo] = (n as u16).to_be_bytes();
            // READ (10)
            self.scsi(&[0x28, 0, a, b, c, d, 0, hi, lo, 0], n * block_size, true)?;
            out[done * block_size..(done + n) * block_size].copy_from_slice(&self.data.as_slice()[..n * block_size]);
            done += n;
        }
        Ok(())
    }

    /// Run one SCSI command. `length` bytes of data move through `self.data`.
    fn scsi(&mut self, command: &[u8], length: usize, data_in: bool) -> Result<(), &'static str> {
        self.tag = self.tag.wrapping_add(1);
        let cbw = self.command.as_mut_slice();
        cbw[..31].fill(0);
        cbw[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        cbw[4..8].copy_from_slice(&self.tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&(length as u32).to_le_bytes());
        cbw[12] = if data_in { 0x80 } else { 0 };
        cbw[14] = command.len() as u8;
        cbw[15..15 + command.len()].copy_from_slice(command);
        self.controller.bulk(&mut self.device, self.bulk_out, &self.command, 31)?;

        if length > 0 {
            let endpoint = if data_in { self.bulk_in } else { self.bulk_out };
            self.controller.bulk(&mut self.device, endpoint, &self.data, length)?;
        }

        self.controller.bulk(&mut self.device, self.bulk_in, &self.command, 13)?;
        let csw = self.command.as_slice();
        let signature = u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]);
        let tag = u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]);
        if signature != CSW_SIGNATURE || tag != self.tag {
            return Err("bad status from USB storage");
        }
        if csw[12] != 0 {
            return Err("USB storage command failed");
        }
        Ok(())
    }
}
