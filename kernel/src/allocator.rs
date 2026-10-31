//! The kernel heap. Once this is set up we can use `Box`, `Vec`, `String`
//! and the rest of Rust's `alloc` library, just like a normal program.

use crate::memory;
use linked_list_allocator::LockedHeap;
use x86_64::VirtAddr;
use x86_64::structures::paging::{FrameAllocator, Mapper, Size4KiB, mapper::MapToError};

/// Where the heap lives in virtual memory. Any unused address works; this
/// one is easy to spot in crash reports.
pub const HEAP_START: u64 = 0x_4444_4444_0000;
pub const HEAP_SIZE: u64 = 32 * 1024 * 1024;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub fn init(
    mapper: &mut impl Mapper<Size4KiB>,
    frames: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    memory::map_range(VirtAddr::new(HEAP_START), HEAP_SIZE, mapper, frames)?;
    unsafe {
        ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE as usize);
    }
    Ok(())
}

/// Bytes of heap currently in use.
pub fn used() -> usize {
    ALLOCATOR.lock().used()
}
