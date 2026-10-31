//! USB: finding devices on the USB controller and talking to them.

pub mod storage;
pub mod xhci;

use crate::memory::DmaBuffer;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq)]
pub enum Direction {
    In,
    Out,
}

/// A standard USB control request (the 8-byte "setup packet").
#[derive(Clone, Copy)]
pub struct ControlRequest {
    pub direction: Direction,
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl ControlRequest {
    pub fn setup_packet(&self) -> u64 {
        let request_type = self.request_type | if self.direction == Direction::In { 0x80 } else { 0 };
        request_type as u64
            | (self.request as u64) << 8
            | (self.value as u64) << 16
            | (self.index as u64) << 32
            | (self.length as u64) << 48
    }

    pub fn get_descriptor(kind: u8, length: u16) -> Self {
        Self { direction: Direction::In, request_type: 0, request: 6, value: (kind as u16) << 8, index: 0, length }
    }

    pub fn set_configuration(value: u8) -> Self {
        Self { direction: Direction::Out, request_type: 0, request: 9, value: value as u16, index: 0, length: 0 }
    }
}

/// The interesting parts of an interface descriptor.
pub struct Interface {
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    /// (endpoint address, max packet size, transfer type)
    pub endpoints: Vec<(u8, u16, u8)>,
}

/// Read a device's configuration descriptor: which "interfaces" (functions)
/// it has and their endpoints. Returns the configuration value too.
pub fn read_configuration(
    controller: &mut xhci::Controller,
    device: &mut xhci::Device,
) -> Result<(u8, Vec<Interface>), &'static str> {
    let mut buffer = DmaBuffer::new(4096);
    // First just the header, to learn the total length.
    controller.control(device, ControlRequest::get_descriptor(2, 9), Some(&mut buffer))?;
    let total = u16::from_le_bytes([buffer.as_slice()[2], buffer.as_slice()[3]]).min(4096);
    controller.control(device, ControlRequest::get_descriptor(2, total), Some(&mut buffer))?;

    let bytes = &buffer.as_slice()[..total as usize];
    let configuration_value = bytes[5];
    let mut interfaces: Vec<Interface> = Vec::new();
    let mut offset = 0;
    while offset + 2 <= bytes.len() {
        let length = bytes[offset] as usize;
        if length == 0 || offset + length > bytes.len() {
            break;
        }
        let d = &bytes[offset..offset + length];
        match d[1] {
            4 if length >= 9 => interfaces.push(Interface {
                class: d[5],
                subclass: d[6],
                protocol: d[7],
                endpoints: Vec::new(),
            }),
            5 if length >= 7 => {
                if let Some(interface) = interfaces.last_mut() {
                    let max_packet = u16::from_le_bytes([d[4], d[5]]) & 0x7FF;
                    interface.endpoints.push((d[2], max_packet, d[3] & 0b11));
                }
            }
            _ => {}
        }
        offset += length;
    }
    Ok((configuration_value, interfaces))
}
