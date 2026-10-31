//! The kernel heap. Once this is set up we can use `Box`, `Vec`, `String`
//! and the rest of Rust's `alloc` library, just like a normal program.

use crate::memory;
use core::alloc::{GlobalAlloc, Layout};
use linked_list_allocator::LockedHeap;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::VirtAddr;
use x86_64::structures::paging::{Size4KiB, mapper::MapToError};

/// Where the heap lives in virtual memory. Any unused address works; this
/// one is easy to spot in crash reports.
pub const HEAP_START: u64 = 0x_4444_4444_0000;
pub const HEAP_SIZE: u64 = 64 * 1024 * 1024;

#[global_allocator]
static ALLOCATOR: InterruptSafeHeap = InterruptSafeHeap(LockedHeap::empty());

/// Wraps the heap so allocations can't be interrupted by a task switch.
/// Otherwise a task could be paused while holding the heap's lock, and the
/// next task to allocate would wait for it forever.
struct InterruptSafeHeap(LockedHeap);

unsafe impl GlobalAlloc for InterruptSafeHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        without_interrupts(|| unsafe { self.0.alloc(layout) })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        without_interrupts(|| unsafe { self.0.dealloc(ptr, layout) })
    }
}

pub fn init() -> Result<(), MapToError<Size4KiB>> {
    let mut guard = memory::MEMORY.lock();
    let memory = guard.as_mut().expect("memory not initialised");
    memory::map_range(VirtAddr::new(HEAP_START), HEAP_SIZE, &mut memory.mapper, &mut memory.frames)?;
    unsafe {
        ALLOCATOR.0.lock().init(HEAP_START as *mut u8, HEAP_SIZE as usize);
    }
    Ok(())
}

/// Bytes of heap currently in use.
pub fn used() -> usize {
    without_interrupts(|| ALLOCATOR.0.lock().used())
}
