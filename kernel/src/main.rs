//! OsPilot kernel entry point.
//!
//! `#![no_std]` means we can't use Rust's standard library (it needs an OS,
//! and we *are* the OS). `#![no_main]` means there's no normal `main`; the
//! bootloader jumps straight into `kernel_main` below.
#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod allocator;
mod console;
mod desktop;
mod gfx;
mod interrupts;
mod memory;
mod mouse;
mod serial;
mod task;

use alloc::{boxed::Box, string::String, vec::Vec};
use bootloader_api::config::Mapping;
use bootloader_api::{BootInfo, BootloaderConfig, entry_point};
use core::panic::PanicInfo;
use x86_64::VirtAddr;

/// Settings handed to the bootloader.
pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    // Map all of physical RAM somewhere in our address space, so the kernel
    // can read and edit page tables (which live in physical memory).
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config.kernel_stack_size = 256 * 1024;
    config
};

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
    println!("Interrupts are on.");

    let phys_offset = VirtAddr::new(
        boot_info.physical_memory_offset.into_option().expect("physical memory not mapped"),
    );
    let mut mapper = unsafe { memory::init(phys_offset) };
    let mut frames = unsafe { memory::BootInfoFrameAllocator::new(&boot_info.memory_regions) };
    allocator::init(&mut mapper, &mut frames).expect("heap setup failed");
    console::enable_log();
    println!(
        "Memory: {} MiB usable RAM, {} MiB kernel heap.",
        memory::usable_bytes(&boot_info.memory_regions) / (1024 * 1024),
        allocator::HEAP_SIZE / (1024 * 1024),
    );
    heap_self_test();

    task::init();
    task::spawn("desktop", desktop::run);
    print!("Tasks running:");
    task::list(|name| print!(" {name}"));
    println!();

    println!();
    println!("Welcome to OsPilot! Type something:");

    loop {
        // Sleep until the next interrupt (a key press or a timer tick).
        x86_64::instructions::hlt();
    }
}

/// Quick check that heap allocation works, using a few standard types.
fn heap_self_test() {
    let boxed = Box::new(42);
    let numbers: Vec<u64> = (1..=1000).collect();
    let mut text = String::from("heap");
    text.push_str(" works");
    assert_eq!(*boxed, 42);
    assert_eq!(numbers.iter().sum::<u64>(), 500_500);
    println!("Heap self-test: {text} ({} bytes in use).", allocator::used());
}

/// Called if anything goes badly wrong. Prints the error and stops.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    x86_64::instructions::interrupts::disable();
    // Take the screen back from the desktop. The lock may be held by the
    // code that crashed, so break it: nothing else will run after this.
    unsafe { console::CONSOLE.force_unlock() };
    if let Some(console) = console::CONSOLE.lock().as_mut() {
        console.set_desktop_active(false);
    }
    println!();
    println!("KERNEL PANIC: {info}");
    loop {
        x86_64::instructions::hlt();
    }
}
