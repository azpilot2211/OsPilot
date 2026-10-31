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
const TIMER_INTERRUPT: u8 = PIC_1_OFFSET;
const KEYBOARD_INTERRUPT: u8 = PIC_1_OFFSET + 1;

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
    idt[TIMER_INTERRUPT].set_handler_fn(timer_handler);
    idt[KEYBOARD_INTERRUPT].set_handler_fn(keyboard_handler);
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
        // Only listen to the timer (IRQ0) and keyboard (IRQ1) for now.
        pics.write_masks(0b1111_1100, 0b1111_1111);
    }
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

extern "x86-interrupt" fn timer_handler(_frame: InterruptStackFrame) {
    unsafe { PICS.lock().notify_end_of_interrupt(TIMER_INTERRUPT) };
}

extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    let scancode: u8 = unsafe { Port::new(0x60).read() };
    let mut keyboard = KEYBOARD.lock();
    if let Ok(Some(event)) = keyboard.add_byte(scancode) {
        if let Some(DecodedKey::Unicode(c)) = keyboard.process_keyevent(event) {
            print!("{c}");
        }
    }
    unsafe { PICS.lock().notify_end_of_interrupt(KEYBOARD_INTERRUPT) };
}
