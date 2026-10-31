//! PS/2 mouse driver.
//!
//! The mouse sends 3-byte packets (buttons, x movement, y movement) through
//! the same controller chip as the keyboard. We add up the movement in
//! atomics so the desktop can read it whenever it redraws.

use core::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

static DX: AtomicI32 = AtomicI32::new(0);
static DY: AtomicI32 = AtomicI32::new(0);
static BUTTONS: AtomicU8 = AtomicU8::new(0);

/// Bytes of the packet received so far.
static PACKET: Mutex<([u8; 3], usize)> = Mutex::new(([0; 3], 0));

pub const LEFT_BUTTON: u8 = 1;

/// Movement since the last call, and which buttons are held now.
pub fn take_state() -> (i32, i32, u8) {
    (DX.swap(0, Ordering::Relaxed), DY.swap(0, Ordering::Relaxed), BUTTONS.load(Ordering::Relaxed))
}

/// Switch the mouse on. Returns false if no PS/2 mouse answered.
pub fn init() -> bool {
    let mut command = Port::<u8>::new(0x64);
    let mut data = Port::<u8>::new(0x60);
    unsafe {
        wait_write();
        command.write(0xA8); // enable the second (mouse) port

        // Turn on the mouse interrupt in the controller's config byte.
        wait_write();
        command.write(0x20);
        if !wait_read() {
            return false;
        }
        let config = (data.read() | 0b10) & !0b10_0000;
        wait_write();
        command.write(0x60);
        wait_write();
        data.write(config);

        // Restore defaults, then start sending movement packets.
        let ok = send_to_mouse(0xF6) && send_to_mouse(0xF4);
        // Throw away any leftover bytes so the first real packet lines up.
        while Port::<u8>::new(0x64).read() & 0b1 != 0 {
            data.read();
        }
        ok
    }
}

/// Called from the IRQ 12 interrupt handler with each byte the mouse sends.
pub fn handle_byte(byte: u8) {
    let mut guard = PACKET.lock();
    let (packet, len) = &mut *guard;
    // The first byte always has bit 3 set. If it doesn't, we're out of step,
    // so drop bytes until we find a valid start.
    if *len == 0 && byte & 0b1000 == 0 {
        return;
    }
    packet[*len] = byte;
    *len += 1;
    if *len < 3 {
        return;
    }
    *len = 0;

    let flags = packet[0];
    // Overflowed packets are garbage; skip them.
    if flags & 0b1100_0000 != 0 {
        return;
    }
    let dx = packet[1] as i32 - if flags & 0b1_0000 != 0 { 256 } else { 0 };
    let dy = packet[2] as i32 - if flags & 0b10_0000 != 0 { 256 } else { 0 };
    DX.fetch_add(dx, Ordering::Relaxed);
    // The mouse counts "up" as positive; the screen counts down.
    DY.fetch_add(-dy, Ordering::Relaxed);
    BUTTONS.store(flags & 0b111, Ordering::Relaxed);
}

unsafe fn send_to_mouse(byte: u8) -> bool {
    unsafe {
        wait_write();
        Port::<u8>::new(0x64).write(0xD4); // "next data byte is for the mouse"
        wait_write();
        Port::<u8>::new(0x60).write(byte);
        // The mouse answers 0xFA ("acknowledged").
        wait_read() && Port::<u8>::new(0x60).read() == 0xFA
    }
}

/// Wait until the controller can take another byte.
fn wait_write() {
    for _ in 0..100_000 {
        if unsafe { Port::<u8>::new(0x64).read() } & 0b10 == 0 {
            return;
        }
    }
}

/// Wait until the controller has a byte for us. False on timeout.
fn wait_read() -> bool {
    for _ in 0..100_000 {
        if unsafe { Port::<u8>::new(0x64).read() } & 0b1 != 0 {
            return true;
        }
    }
    false
}
