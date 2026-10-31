//! Modern interrupt hardware: the local APIC and I/O APIC.
//!
//! The old 8259 PIC we started with is an emulation of 1980s hardware. Real
//! PCs route interrupts through the APIC instead, and some new machines don't
//! wire the PIC up properly at all. ACPI tables (written by the firmware)
//! tell us where the APIC lives and how keyboard and mouse are wired to it.

use crate::{interrupts, memory, println, task};
use acpi::{AcpiHandler, AcpiTables, InterruptModel, PhysicalMapping};
use acpi::platform::interrupt::{Polarity, TriggerMode};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static LOCAL_APIC: AtomicU64 = AtomicU64::new(0);

// Local APIC register offsets.
const LAPIC_ID: u64 = 0x20;
const LAPIC_EOI: u64 = 0xB0;
const LAPIC_SPURIOUS: u64 = 0xF0;
const LAPIC_TIMER: u64 = 0x320;
const LAPIC_TIMER_INITIAL: u64 = 0x380;
const LAPIC_TIMER_CURRENT: u64 = 0x390;
const LAPIC_TIMER_DIVIDE: u64 = 0x3E0;
const SPURIOUS_VECTOR: u32 = 0xFF;

/// Lets the `acpi` crate read firmware tables, which sit in normal RAM.
#[derive(Clone)]
struct Handler;

impl AcpiHandler for Handler {
    unsafe fn map_physical_region<T>(&self, phys: usize, size: usize) -> PhysicalMapping<Self, T> {
        let virt = memory::phys_to_virt(phys as u64);
        unsafe { PhysicalMapping::new(phys, NonNull::new(virt.as_mut_ptr()).unwrap(), size, size, Self) }
    }

    fn unmap_physical_region<T>(_region: &PhysicalMapping<Self, T>) {}
}

/// Is the APIC handling interrupts (rather than the old PIC)?
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Tell the local APIC we've finished handling an interrupt.
pub fn end_of_interrupt() {
    unsafe { write_lapic(LAPIC_EOI, 0) };
}

/// Find the APICs via ACPI and switch interrupts over to them. Needs the
/// heap and the PIC timer running (used to time the APIC timer).
pub fn init(rsdp_addr: Option<u64>) {
    let Some(rsdp) = rsdp_addr else {
        println!("No ACPI tables found; staying on the legacy PIC.");
        return;
    };
    let tables = match unsafe { AcpiTables::from_rsdp(Handler, rsdp as usize) } {
        Ok(tables) => tables,
        Err(e) => return println!("Couldn't read ACPI tables ({e:?}); staying on the PIC."),
    };
    let platform = match tables.platform_info() {
        Ok(platform) => platform,
        Err(e) => return println!("Couldn't read ACPI platform info ({e:?}); staying on the PIC."),
    };
    let InterruptModel::Apic(apic) = platform.interrupt_model else {
        return println!("No APIC described by ACPI; staying on the PIC.");
    };
    let Some(io_apic) = apic.io_apics.first() else {
        return println!("No I/O APIC found; staying on the PIC.");
    };

    // Local APIC: one per CPU core, handles the timer and acknowledgements.
    let lapic = memory::map_mmio(apic.local_apic_address, 4096);
    LOCAL_APIC.store(lapic.as_u64(), Ordering::Relaxed);
    unsafe { write_lapic(LAPIC_SPURIOUS, 0x100 | SPURIOUS_VECTOR) };
    let ticks_per_period = calibrate_timer();

    // I/O APIC: receives device interrupts and forwards them to a core.
    let io = memory::map_mmio(io_apic.address as u64, 4096).as_u64();
    let lapic_id = unsafe { read_lapic(LAPIC_ID) } >> 24;
    for (isa_irq, vector) in [(1u8, interrupts::KEYBOARD_INTERRUPT), (12, interrupts::MOUSE_INTERRUPT)] {
        // The firmware may say an ISA IRQ is wired to a different pin.
        let over = apic.interrupt_source_overrides.iter().find(|o| o.isa_source == isa_irq);
        let gsi = over.map_or(isa_irq as u32, |o| o.global_system_interrupt);
        let mut low = vector as u32;
        if over.is_some_and(|o| o.polarity == Polarity::ActiveLow) {
            low |= 1 << 13;
        }
        if over.is_some_and(|o| o.trigger_mode == TriggerMode::Level) {
            low |= 1 << 15;
        }
        let pin = gsi - io_apic.global_system_interrupt_base;
        unsafe {
            write_ioapic(io, 0x10 + pin * 2 + 1, lapic_id << 24);
            write_ioapic(io, 0x10 + pin * 2, low);
        }
    }

    // Switch over: silence the PIC, then start the APIC timer.
    x86_64::instructions::interrupts::without_interrupts(|| {
        interrupts::disable_pic();
        ACTIVE.store(true, Ordering::Relaxed);
        unsafe {
            write_lapic(LAPIC_TIMER_DIVIDE, 0b0011); // divide by 16
            write_lapic(LAPIC_TIMER, interrupts::TIMER_INTERRUPT as u32 | 1 << 17); // periodic
            write_lapic(LAPIC_TIMER_INITIAL, ticks_per_period);
        }
    });
    println!("APIC enabled (I/O APIC at {:#x}).", io_apic.address);
}

/// Measure how fast the APIC timer counts, using the PIT timer we already
/// trust, so we can make it tick `TICKS_PER_SECOND` times a second.
fn calibrate_timer() -> u32 {
    const SAMPLE_TICKS: u64 = 10;
    unsafe {
        write_lapic(LAPIC_TIMER_DIVIDE, 0b0011);
        write_lapic(LAPIC_TIMER, 1 << 16); // masked: count but don't interrupt
        // Line up with the start of a PIT tick, then count for a while.
        let start = task::ticks();
        while task::ticks() == start {
            x86_64::instructions::hlt();
        }
        write_lapic(LAPIC_TIMER_INITIAL, u32::MAX);
        let start = task::ticks();
        while task::ticks() < start + SAMPLE_TICKS {
            x86_64::instructions::hlt();
        }
        let elapsed = u32::MAX - read_lapic(LAPIC_TIMER_CURRENT);
        write_lapic(LAPIC_TIMER_INITIAL, 0);
        elapsed / SAMPLE_TICKS as u32
    }
}

unsafe fn write_lapic(reg: u64, value: u32) {
    let base = LOCAL_APIC.load(Ordering::Relaxed);
    unsafe { core::ptr::write_volatile((base + reg) as *mut u32, value) };
}

unsafe fn read_lapic(reg: u64) -> u32 {
    let base = LOCAL_APIC.load(Ordering::Relaxed);
    unsafe { core::ptr::read_volatile((base + reg) as *const u32) }
}

/// The I/O APIC has just two registers: pick a register number, then
/// read or write its value.
unsafe fn write_ioapic(base: u64, reg: u32, value: u32) {
    unsafe {
        core::ptr::write_volatile(base as *mut u32, reg);
        core::ptr::write_volatile((base + 0x10) as *mut u32, value);
    }
}
