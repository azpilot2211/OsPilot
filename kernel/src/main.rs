//! OsPilot kernel entry point.
//!
//! `#![no_std]` means we can't use Rust's standard library (it needs an OS,
//! and we *are* the OS). `#![no_main]` means there's no normal `main`; the
//! bootloader jumps straight into `kernel_main` below.
#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

mod console;
mod interrupts;
mod serial;

use bootloader_api::{BootInfo, BootloaderConfig, entry_point};
use core::panic::PanicInfo;

/// Settings handed to the bootloader. Defaults are fine for now.
pub static BOOTLOADER_CONFIG: BootloaderConfig = BootloaderConfig::new_default();

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

/// The first Rust code that runs after the bootloader hands over control.
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial::init();

    // UEFI has already put the graphics card (your Radeon included) into a
    // graphics mode and told the bootloader where its pixels live. We draw
    // straight into that memory.
    if let Some(framebuffer) = boot_info.framebuffer.as_mut() {
        console::init(framebuffer);
    }

    println!("OsPilot v{}", env!("CARGO_PKG_VERSION"));
    println!("Booted via UEFI. Screen is ready.");

    interrupts::init();
    println!("Interrupts are on. Type something:");
    println!();

    loop {
        // Sleep until the next interrupt (a key press or a timer tick).
        x86_64::instructions::hlt();
    }
}

/// Called if anything goes badly wrong. Prints the error and stops.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!();
    println!("KERNEL PANIC: {info}");
    loop {
        x86_64::instructions::hlt();
    }
}
