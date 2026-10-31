//! Serial port output. In QEMU this shows up in the terminal you launched it
//! from, which is handy for debugging when the screen isn't working.

use spin::Mutex;
use uart_16550::SerialPort;

/// COM1 lives at I/O port 0x3F8 on PCs.
static SERIAL1: Mutex<SerialPort> = Mutex::new(unsafe { SerialPort::new(0x3F8) });

pub fn init() {
    SERIAL1.lock().init();
}

pub fn write_str(s: &str) {
    use core::fmt::Write;
    let _ = SERIAL1.lock().write_str(s);
}
