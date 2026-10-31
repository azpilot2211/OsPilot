//! Driver for xHCI, the standard USB controller in every PC from the last
//! decade (it handles USB 1, 2 and 3 devices).
//!
//! We talk to the controller through rings of 16-byte "TRBs" (Transfer
//! Request Blocks) in RAM: we write commands into a ring and ring a
//! "doorbell"; the controller writes results into an event ring that we poll.

use super::{ControlRequest, Direction};
use crate::memory::{self, DmaBuffer};
use crate::pci;
use crate::task;
use alloc::vec::Vec;

// TRB types.
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_ENDPOINT: u32 = 12;
const TRB_TRANSFER_EVENT: u32 = 32;
const TRB_COMMAND_COMPLETION: u32 = 33;

const TRB_IOC: u32 = 1 << 5; // interrupt (i.e. post an event) on completion
const TRB_IDT: u32 = 1 << 6; // data is inside the TRB itself

const COMPLETION_SUCCESS: u32 = 1;
const COMPLETION_SHORT_PACKET: u32 = 13;

const RING_SIZE: usize = 256;
const TIMEOUT_MS: u64 = 2000;

/// A ring of TRBs that we produce and the controller consumes.
struct Ring {
    buffer: DmaBuffer,
    index: usize,
    cycle: u32,
}

impl Ring {
    fn new() -> Self {
        Self { buffer: DmaBuffer::new(RING_SIZE * 16), index: 0, cycle: 1 }
    }

    /// Add a TRB. Returns its physical address (events refer to TRBs by it).
    fn push(&mut self, parameter: u64, status: u32, control: u32) -> u64 {
        let address = self.write(parameter, status, control);
        self.index += 1;
        if self.index == RING_SIZE - 1 {
            // Last slot: a link TRB pointing back to the start. The
            // "toggle cycle" bit flips our cycle bit as we wrap around.
            let start = self.buffer.phys;
            self.write(start, 0, TRB_LINK << 10 | 1 << 1);
            self.index = 0;
            self.cycle ^= 1;
        }
        address
    }

    fn write(&mut self, parameter: u64, status: u32, control: u32) -> u64 {
        let offset = self.index * 16;
        self.buffer.write_u64(offset, parameter);
        self.buffer.write_u32(offset + 8, status);
        // The cycle bit goes in last: it's what tells the controller this
        // TRB is ready.
        self.buffer.write_u32(offset + 12, control | self.cycle);
        self.buffer.phys + offset as u64
    }
}

#[derive(Clone, Copy, Debug)]
struct Event {
    parameter: u64,
    status: u32,
    control: u32,
}

impl Event {
    fn kind(&self) -> u32 {
        self.control >> 10 & 0x3F
    }

    fn completion_code(&self) -> u32 {
        self.status >> 24
    }

    fn slot(&self) -> u8 {
        (self.control >> 24) as u8
    }
}

pub struct Controller {
    op: u64,
    runtime: u64,
    doorbells: u64,
    max_ports: u8,
    context_size: usize,
    dcbaa: DmaBuffer,
    commands: Ring,
    events: DmaBuffer,
    event_index: usize,
    event_cycle: u32,
    _event_segment_table: DmaBuffer,
    _scratchpad: Vec<DmaBuffer>,
}

/// One USB device the controller has set up.
pub struct Device {
    pub slot: u8,
    pub port: u8,
    pub speed: u8,
    rings: [Option<Ring>; 32],
    input_context: DmaBuffer,
    _output_context: DmaBuffer,
}

impl Controller {
    /// Find every xHCI controller on the PCI bus and start them. PCs often
    /// have several, one per group of USB sockets.
    pub fn all() -> Vec<Self> {
        let mut controllers = Vec::new();
        let found = pci::scan().into_iter().filter(|d| d.class == 0x0C && d.subclass == 0x03 && d.prog_if == 0x30);
        for pci in found {
            crate::println!("USB controller: PCI {:04x}:{:04x}", pci.vendor_id, pci.device_id);
            pci.enable_bus_mastering();
            let base = memory::map_mmio(pci.bar_address(0), 0x10000).as_u64();
            match Self::init(base) {
                Ok(controller) => controllers.push(controller),
                Err(e) => crate::println!("  couldn't start it: {e}"),
            }
        }
        controllers
    }

    /// `base` must be the mapped register area of an xHCI controller.
    fn init(base: u64) -> Result<Self, &'static str> {
        let cap_length = read32(base) & 0xFF;
        let hcs1 = read32(base + 0x04);
        let hcs2 = read32(base + 0x08);
        let hcc1 = read32(base + 0x10);
        let op = base + cap_length as u64;
        let runtime = base + (read32(base + 0x18) & !0x1F) as u64;
        let doorbells = base + (read32(base + 0x14) & !0x3) as u64;
        let max_slots = (hcs1 & 0xFF) as usize;
        let max_ports = (hcs1 >> 24) as u8;
        let context_size = if hcc1 & 0b100 != 0 { 64 } else { 32 };

        take_ownership_from_firmware(base, hcc1);

        // Stop, then reset the controller.
        write32(op, read32(op) & !1);
        wait_for(|| read32(op + 0x04) & 1 != 0, "controller didn't halt")?;
        write32(op, 1 << 1);
        wait_for(|| read32(op) & (1 << 1) == 0, "controller reset timed out")?;
        wait_for(|| read32(op + 0x04) & (1 << 11) == 0, "controller not ready")?;

        write32(op + 0x38, max_slots as u32); // enable all device slots

        // Device Context Base Address Array: one pointer per slot.
        let mut dcbaa = DmaBuffer::new((max_slots + 1) * 8);

        // Some controllers need RAM of their own ("scratchpad").
        let scratchpad_count = ((hcs2 >> 21 & 0x1F) << 5 | hcs2 >> 27) as usize;
        let mut scratchpad = Vec::new();
        if scratchpad_count > 0 {
            let mut array = DmaBuffer::new(scratchpad_count * 8);
            for i in 0..scratchpad_count {
                let page = DmaBuffer::new(4096);
                array.write_u64(i * 8, page.phys);
                scratchpad.push(page);
            }
            dcbaa.write_u64(0, array.phys);
            scratchpad.push(array);
        }
        write64(op + 0x30, dcbaa.phys);

        let commands = Ring::new();
        write64(op + 0x18, commands.buffer.phys | 1);

        // Event ring, described by a one-entry "segment table".
        let events = DmaBuffer::new(RING_SIZE * 16);
        let mut segment_table = DmaBuffer::new(16);
        segment_table.write_u64(0, events.phys);
        segment_table.write_u32(8, RING_SIZE as u32);
        let interrupter = runtime + 0x20;
        write32(interrupter + 0x08, 1);
        write64(interrupter + 0x18, events.phys);
        write64(interrupter + 0x10, segment_table.phys);

        // Run. We poll for events rather than using interrupts, for now.
        write32(op, 1);
        wait_for(|| read32(op + 0x04) & 1 == 0, "controller didn't start")?;

        Ok(Self {
            op,
            runtime,
            doorbells,
            max_ports,
            context_size,
            dcbaa,
            commands,
            events,
            event_index: 0,
            event_cycle: 1,
            _event_segment_table: segment_table,
            _scratchpad: scratchpad,
        })
    }

    /// Reset every port with something plugged in, and set up the devices.
    pub fn enumerate(&mut self) -> Vec<Device> {
        let mut devices = Vec::new();
        for port in 1..=self.max_ports {
            match self.setup_port(port) {
                Ok(Some(device)) => devices.push(device),
                Ok(None) => {}
                Err(e) => crate::println!("USB port {port}: {e}"),
            }
        }
        devices
    }

    fn portsc(&self, port: u8) -> u64 {
        self.op + 0x400 + 0x10 * (port as u64 - 1)
    }

    fn setup_port(&mut self, port: u8) -> Result<Option<Device>, &'static str> {
        let reg = self.portsc(port);
        // Bits we must write back as 0: "port enabled" (writing 1 disables)
        // and the change flags (writing 1 clears them).
        const KEEP: u32 = !(1 << 1 | 0x7F << 17);
        {
            if read32(reg) & 1 == 0 {
                return Ok(None); // nothing connected
            }
            if read32(reg) & (1 << 1) == 0 {
                // USB 2 devices need a port reset before they work; USB 3
                // ones are enabled automatically.
                write32(reg, read32(reg) & KEEP | 1 << 4);
                wait_for(|| read32(reg) & (1 << 21) != 0, "port reset timed out")?;
                write32(reg, read32(reg) & KEEP | 1 << 21);
            }
            if read32(reg) & (1 << 1) == 0 {
                return Err("port didn't enable");
            }
        }
        let speed = (read32(reg) >> 10 & 0xF) as u8;
        task::sleep_ms(10);
        self.address_device(port, speed).map(Some)
    }

    /// Give a newly reset device a slot and an address.
    fn address_device(&mut self, port: u8, speed: u8) -> Result<Device, &'static str> {
        let event = self.command(0, 0, TRB_ENABLE_SLOT << 10)?;
        let slot = event.slot();

        let ctx = self.context_size;
        let output_context = DmaBuffer::new(32 * ctx);
        self.dcbaa.write_u64(slot as usize * 8, output_context.phys);

        let mut input = DmaBuffer::new(33 * ctx);
        input.write_u32(4, 0b11); // add the slot context and endpoint 0
        // Slot context: speed, one context entry, which root port.
        input.write_u32(ctx, (speed as u32) << 20 | 1 << 27);
        input.write_u32(ctx + 4, (port as u32) << 16);
        // Endpoint 0 (control) context.
        let max_packet = match speed {
            2 => 8,       // low speed
            3 => 64,      // high speed
            4.. => 512,   // super speed
            _ => 8,       // full speed: 8 is always safe to start with
        };
        let ring = Ring::new();
        let ep0 = 2 * ctx;
        input.write_u32(ep0 + 4, 3 << 1 | 4 << 3 | max_packet << 16);
        input.write_u64(ep0 + 8, ring.buffer.phys | 1);
        input.write_u32(ep0 + 16, 8);

        self.command(input.phys, 0, TRB_ADDRESS_DEVICE << 10 | (slot as u32) << 24)?;

        let mut rings: [Option<Ring>; 32] = Default::default();
        rings[1] = Some(ring);
        Ok(Device { slot, port, speed, rings, input_context: input, _output_context: output_context })
    }

    /// Set up bulk endpoints (used for data transfer) after the device has
    /// been configured. Each is (endpoint address, max packet size).
    pub fn configure_bulk_endpoints(&mut self, device: &mut Device, endpoints: &[(u8, u16)]) -> Result<(), &'static str> {
        let ctx = self.context_size;
        let input = &mut device.input_context;
        input.as_mut_slice()[..33 * ctx].fill(0);
        let mut add_flags = 1u32;
        let mut max_dci = 1;
        for &(address, max_packet) in endpoints {
            let dci = endpoint_index(address);
            add_flags |= 1 << dci;
            max_dci = max_dci.max(dci);
            let ring = Ring::new();
            let ep = (dci + 1) * ctx;
            let kind = if address & 0x80 != 0 { 6 } else { 2 }; // bulk in / bulk out
            input.write_u32(ep + 4, 3 << 1 | kind << 3 | (max_packet as u32) << 16);
            input.write_u64(ep + 8, ring.buffer.phys | 1);
            input.write_u32(ep + 16, max_packet as u32);
            device.rings[dci] = Some(ring);
        }
        input.write_u32(4, add_flags);
        input.write_u32(ctx, (device.speed as u32) << 20 | (max_dci as u32) << 27);
        input.write_u32(ctx + 4, (device.port as u32) << 16);
        let phys = input.phys;
        self.command(phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | (device.slot as u32) << 24)?;
        Ok(())
    }

    /// Send a control request on endpoint 0. Returns bytes transferred.
    pub fn control(&mut self, device: &mut Device, request: ControlRequest, data: Option<&mut DmaBuffer>) -> Result<usize, &'static str> {
        let ring = device.rings[1].as_mut().ok_or("no control endpoint")?;
        let length = request.length as u32;
        let has_data = length > 0 && data.is_some();
        let into_host = request.direction == Direction::In;
        let transfer_type = match (has_data, into_host) {
            (false, _) => 0,
            (true, false) => 2,
            (true, true) => 3,
        };
        ring.push(request.setup_packet(), 8, TRB_SETUP << 10 | TRB_IDT | transfer_type << 16);
        if let Some(buffer) = data.filter(|_| has_data) {
            ring.push(buffer.phys, length, TRB_DATA << 10 | (into_host as u32) << 16);
        }
        // The status stage goes the opposite way to the data.
        let status_in = !(has_data && into_host);
        let last = ring.push(0, 0, TRB_STATUS << 10 | TRB_IOC | (status_in as u32) << 16);
        self.ring_doorbell(device.slot, 1);
        let event = self.wait_event(|e| e.kind() == TRB_TRANSFER_EVENT && e.parameter == last)?;
        Ok(length.saturating_sub(event.status & 0xFFFFFF) as usize)
    }

    /// Move data on a bulk endpoint. Returns bytes transferred.
    pub fn bulk(&mut self, device: &mut Device, endpoint: u8, buffer: &DmaBuffer, length: usize) -> Result<usize, &'static str> {
        let dci = endpoint_index(endpoint);
        let ring = device.rings[dci].as_mut().ok_or("endpoint not configured")?;
        // ISP: report a short packet as soon as it happens.
        let trb = ring.push(buffer.phys, length as u32, TRB_NORMAL << 10 | TRB_IOC | 1 << 2);
        self.ring_doorbell(device.slot, dci as u32);
        let event = self.wait_event(|e| e.kind() == TRB_TRANSFER_EVENT && e.parameter == trb)?;
        Ok(length.saturating_sub((event.status & 0xFFFFFF) as usize))
    }

    fn command(&mut self, parameter: u64, status: u32, control: u32) -> Result<Event, &'static str> {
        let trb = self.commands.push(parameter, status, control);
        self.ring_doorbell(0, 0);
        self.wait_event(|e| e.kind() == TRB_COMMAND_COMPLETION && e.parameter == trb)
    }

    fn ring_doorbell(&self, slot: u8, target: u32) {
        write32(self.doorbells + 4 * slot as u64, target);
    }

    /// Poll the event ring until an event matching `wanted` arrives.
    fn wait_event(&mut self, wanted: impl Fn(&Event) -> bool) -> Result<Event, &'static str> {
        let deadline = task::ticks() + TIMEOUT_MS * task::TICKS_PER_SECOND / 1000;
        loop {
            while let Some(event) = self.next_event() {
                if wanted(&event) {
                    return match event.completion_code() {
                        COMPLETION_SUCCESS | COMPLETION_SHORT_PACKET => Ok(event),
                        _ => Err("USB transfer failed"),
                    };
                }
            }
            if task::ticks() > deadline {
                return Err("USB device didn't respond");
            }
            core::hint::spin_loop();
        }
    }

    fn next_event(&mut self) -> Option<Event> {
        let offset = self.event_index * 16;
        let control = self.events.read_u32(offset + 12);
        if control & 1 != self.event_cycle {
            return None;
        }
        let event = Event {
            parameter: self.events.read_u32(offset) as u64 | (self.events.read_u32(offset + 4) as u64) << 32,
            status: self.events.read_u32(offset + 8),
            control,
        };
        self.event_index += 1;
        if self.event_index == RING_SIZE {
            self.event_index = 0;
            self.event_cycle ^= 1;
        }
        // Tell the controller how far we've read (bit 3 clears "busy").
        let dequeue = self.events.phys + (self.event_index * 16) as u64;
        write64(self.runtime + 0x20 + 0x18, dequeue | 1 << 3);
        Some(event)
    }
}

/// The xHCI "device context index" for an endpoint address.
fn endpoint_index(address: u8) -> usize {
    let number = (address & 0x0F) as usize;
    if number == 0 { 1 } else { number * 2 + (address >> 7) as usize }
}

/// Firmware may be using the controller (e.g. to make a USB keyboard look
/// like an old PS/2 one). Ask it to hand over control.
fn take_ownership_from_firmware(base: u64, hcc1: u32) {
    let mut offset = (hcc1 >> 16) as u64 * 4;
    while offset != 0 {
        let cap = base + offset;
        let value = read32(cap);
        if value & 0xFF == 1 {
            write32(cap, value | 1 << 24);
            let _ = wait_for(|| read32(cap) & (1 << 16) == 0, "firmware kept USB");
            // Turn off the firmware's USB interrupts (SMIs).
            // Its enable bits are 0, 4 and 13-15; bits 29-31 are status flags that
            // clear when written as 1.
            write32(cap + 4, read32(cap + 4) & !(1 | 1 << 4 | 0b111 << 13) | 0b111 << 29);
            return;
        }
        let next = (value >> 8 & 0xFF) as u64 * 4;
        offset = if next == 0 { 0 } else { offset + next };
    }
}

fn wait_for(condition: impl Fn() -> bool, error: &'static str) -> Result<(), &'static str> {
    let deadline = task::ticks() + TIMEOUT_MS * task::TICKS_PER_SECOND / 1000;
    while !condition() {
        if task::ticks() > deadline {
            return Err(error);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

fn read32(address: u64) -> u32 {
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

fn write32(address: u64, value: u32) {
    unsafe { core::ptr::write_volatile(address as *mut u32, value) }
}

fn write64(address: u64, value: u64) {
    {
        write32(address, value as u32);
        write32(address + 4, (value >> 32) as u32);
    }
}
