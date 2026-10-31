//! PCI: the bus that connects most devices (USB controllers, network cards,
//! graphics cards) to the CPU. Each device has a small "configuration space"
//! saying what it is and where its registers live.

use alloc::vec::Vec;
use x86_64::instructions::port::Port;

#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
}

impl Device {
    pub fn read(&self, offset: u8) -> u32 {
        read(self.bus, self.device, self.function, offset)
    }

    pub fn write(&self, offset: u8, value: u32) {
        let address = address(self.bus, self.device, self.function, offset);
        unsafe {
            Port::<u32>::new(0xCF8).write(address);
            Port::<u32>::new(0xCFC).write(value);
        }
    }

    /// The physical address of a memory BAR (Base Address Register), which
    /// says where the device's registers are. Handles 64-bit BARs.
    pub fn bar_address(&self, index: u8) -> u64 {
        let offset = 0x10 + index * 4;
        let low = self.read(offset);
        let address = (low & !0xF) as u64;
        if (low >> 1) & 0b11 == 0b10 {
            address | (self.read(offset + 4) as u64) << 32
        } else {
            address
        }
    }

    /// Let the device read and write RAM by itself (DMA) and respond to
    /// memory-mapped register accesses.
    pub fn enable_bus_mastering(&self) {
        let command = self.read(0x04);
        self.write(0x04, command | 0b110);
    }
}

fn address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    1 << 31
        | (bus as u32) << 16
        | (device as u32) << 11
        | (function as u32) << 8
        | (offset as u32 & 0xFC)
}

fn read(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    unsafe {
        Port::<u32>::new(0xCF8).write(address(bus, device, function, offset));
        Port::<u32>::new(0xCFC).read()
    }
}

/// List every PCI device in the machine.
pub fn scan() -> Vec<Device> {
    let mut devices = Vec::new();
    for bus in 0..=255u8 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let id = read(bus, device, function, 0);
                if id & 0xFFFF == 0xFFFF {
                    // No device here. If function 0 is missing, the whole
                    // device slot is empty.
                    if function == 0 {
                        break;
                    }
                    continue;
                }
                let class = read(bus, device, function, 0x08);
                devices.push(Device {
                    bus,
                    device,
                    function,
                    vendor_id: id as u16,
                    device_id: (id >> 16) as u16,
                    class: (class >> 24) as u8,
                    subclass: (class >> 16) as u8,
                    prog_if: (class >> 8) as u8,
                });
                // Single-function devices only answer on function 0.
                let header_type = read(bus, device, 0, 0x0C) >> 16 & 0xFF;
                if function == 0 && header_type & 0x80 == 0 {
                    break;
                }
            }
        }
    }
    devices
}
