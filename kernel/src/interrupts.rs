//! CPU setup for handling interrupts: the GDT, the IDT, and the legacy PIC.
//!
//! An interrupt is the hardware tapping the CPU on the shoulder ("a key was
//! pressed", "the timer ticked"). The IDT is a table telling the CPU which
//! function to run for each kind of interrupt.

use crate::{print, println};
use pc_keyboard::{DecodedKey, HandleControl, Keyboard, ScancodeSet1, layouts};
use pic8259::ChainedPics;
use spin::{Lazy, Mutex};
use x86_64::instructions::port::Port;
use x86_64::instructions::segmentation::{CS, SS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

/// The PIC's interrupts are moved to numbers 32..48 so they don't collide
/// with the CPU's own exception numbers (0..32).
const PIC_1_OFFSET: u8 = 32;
const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;
pub const TIMER_INTERRUPT: u8 = PIC_1_OFFSET;
pub const KEYBOARD_INTERRUPT: u8 = PIC_1_OFFSET + 1;
pub const MOUSE_INTERRUPT: u8 = PIC_2_OFFSET + 4;

/// Which TSS stack the double-fault handler runs on. A separate stack means
/// we can still report a crash even if the normal stack overflowed.
const DOUBLE_FAULT_IST_INDEX: u16 = 0;

static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
        const STACK_SIZE: usize = 4096 * 5;
        static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
        let start = VirtAddr::from_ptr(&raw const STACK);
        start + STACK_SIZE as u64
    };
    tss
});

static GDT: Lazy<(GlobalDescriptorTable, SegmentSelector, SegmentSelector, SegmentSelector)> =
    Lazy::new(|| {
        let mut gdt = GlobalDescriptorTable::new();
        let code = gdt.append(Descriptor::kernel_code_segment());
        let data = gdt.append(Descriptor::kernel_data_segment());
        let tss = gdt.append(Descriptor::tss_segment(&TSS));
        (gdt, code, data, tss)
    });

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.page_fault.set_handler_fn(page_fault_handler);
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(DOUBLE_FAULT_IST_INDEX);
    }
    // The timer handler is hand-written assembly (see task.rs) because it
    // switches between tasks.
    unsafe {
        idt[TIMER_INTERRUPT]
            .set_handler_addr(VirtAddr::new(crate::task::timer_interrupt_entry as *const () as u64));
    }
    idt[KEYBOARD_INTERRUPT].set_handler_fn(keyboard_handler);
    idt[MOUSE_INTERRUPT].set_handler_fn(mouse_handler);
    // "Spurious" interrupts: hardware noise we just ignore.
    idt[PIC_1_OFFSET + 7].set_handler_fn(spurious_handler);
    idt[PIC_2_OFFSET + 7].set_handler_fn(spurious_handler);
    idt[0xFF].set_handler_fn(spurious_handler);
    idt
});

static KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> = Mutex::new(Keyboard::new(
    ScancodeSet1::new(),
    layouts::Us104Key,
    HandleControl::Ignore,
));

pub fn init() {
    let (gdt, code, data, tss) = &*GDT;
    gdt.load();
    unsafe {
        CS::set_reg(*code);
        SS::set_reg(*data);
        load_tss(*tss);
    }
    IDT.load();
    unsafe {
        let mut pics = PICS.lock();
        pics.initialize();
        // Listen to the timer (IRQ0), keyboard (IRQ1), the link to the
        // second PIC (IRQ2) and the mouse (IRQ12, line 4 on the second PIC).
        pics.write_masks(0b1111_1000, 0b1110_1111);
    }
    if !crate::mouse::init() {
        println!("No PS/2 mouse found.");
    }
    set_timer_frequency(crate::task::TICKS_PER_SECOND as u32);
    x86_64::instructions::interrupts::enable();
}

extern "x86-interrupt" fn breakpoint_handler(frame: InterruptStackFrame) {
    println!("EXCEPTION: breakpoint\n{frame:#?}");
}

extern "x86-interrupt" fn page_fault_handler(frame: InterruptStackFrame, code: PageFaultErrorCode) {
    let addr = x86_64::registers::control::Cr2::read();
    panic!("page fault at {addr:?} ({code:?})\n{frame:#?}");
}

extern "x86-interrupt" fn double_fault_handler(frame: InterruptStackFrame, _code: u64) -> ! {
    panic!("double fault\n{frame:#?}");
}

extern "x86-interrupt" fn mouse_handler(_frame: InterruptStackFrame) {
    let byte: u8 = unsafe { Port::new(0x60).read() };
    crate::mouse::handle_byte(byte);
    end_of_interrupt(MOUSE_INTERRUPT);
}

/// Program the PIT (the PC's basic timer chip) to fire `hz` times a second.
fn set_timer_frequency(hz: u32) {
    const PIT_BASE_HZ: u32 = 1_193_182;
    let divisor = (PIT_BASE_HZ / hz) as u16;
    unsafe {
        Port::<u8>::new(0x43).write(0x36); // channel 0, square wave mode
        let mut data = Port::<u8>::new(0x40);
        data.write(divisor as u8);
        data.write((divisor >> 8) as u8);
    }
}

/// Tell the interrupt controller we've handled the timer tick so it sends
/// the next one.
pub fn end_of_timer_interrupt() {
    end_of_interrupt(TIMER_INTERRUPT);
}

fn end_of_interrupt(vector: u8) {
    if crate::apic::active() {
        crate::apic::end_of_interrupt();
    } else {
        unsafe { PICS.lock().notify_end_of_interrupt(vector) };
    }
}

/// Mask every PIC input, once the APIC has taken over.
pub fn disable_pic() {
    unsafe { PICS.lock().write_masks(0xFF, 0xFF) };
}

extern "x86-interrupt" fn spurious_handler(_frame: InterruptStackFrame) {}

extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    let scancode: u8 = unsafe { Port::new(0x60).read() };
    let mut keyboard = KEYBOARD.lock();
    if let Ok(Some(event)) = keyboard.add_byte(scancode) {
        if let Some(DecodedKey::Unicode(c)) = keyboard.process_keyevent(event) {
            print!("{c}");
        }
    }
    end_of_interrupt(KEYBOARD_INTERRUPT);
}
