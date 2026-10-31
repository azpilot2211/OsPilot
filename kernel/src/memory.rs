//! Memory management: finding free RAM and mapping it into our address space.
//!
//! The CPU doesn't use physical RAM addresses directly. Every address the
//! kernel uses is "virtual", and page tables translate each 4 KiB page of
//! virtual memory to a 4 KiB "frame" of physical RAM. To give the kernel more
//! memory we grab a free frame and add a page table entry pointing at it.

use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
    Size4KiB, mapper::MapToError,
};
use x86_64::{PhysAddr, VirtAddr};

/// Hands out free 4 KiB frames of physical RAM, using the memory map UEFI
/// gave the bootloader. Frames are never given back yet; that comes later.
pub struct BootInfoFrameAllocator {
    regions: &'static MemoryRegions,
    region: usize,
    next: u64,
    pub allocated: u64,
}

impl BootInfoFrameAllocator {
    /// # Safety
    /// The memory map must be accurate: every region marked `Usable` must
    /// really be unused.
    pub unsafe fn new(regions: &'static MemoryRegions) -> Self {
        Self { regions, region: 0, next: 0, allocated: 0 }
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        while let Some(region) = self.regions.get(self.region) {
            if region.kind == MemoryRegionKind::Usable {
                // Round up to a 4 KiB boundary and skip page 0, which stays
                // unmapped so null pointer bugs crash loudly.
                let start = self.next.max(region.start).max(0x1000).next_multiple_of(4096);
                if start + 4096 <= region.end {
                    self.next = start + 4096;
                    self.allocated += 1;
                    return Some(PhysFrame::containing_address(PhysAddr::new(start)));
                }
            }
            self.region += 1;
        }
        None
    }
}

/// Total usable RAM in bytes, according to the firmware.
pub fn usable_bytes(regions: &MemoryRegions) -> u64 {
    regions
        .iter()
        .filter(|r| r.kind == MemoryRegionKind::Usable)
        .map(|r| r.end - r.start)
        .sum()
}

/// Gives access to the active page tables.
///
/// # Safety
/// All physical memory must be mapped at `physical_memory_offset`, and this
/// must only be called once, so there's a single owner of the page tables.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let (level_4_frame, _) = Cr3::read();
    let virt = physical_memory_offset + level_4_frame.start_address().as_u64();
    let level_4_table = unsafe { &mut *virt.as_mut_ptr::<PageTable>() };
    unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

/// Back a range of virtual memory with fresh, writable physical frames.
pub fn map_range(
    start: VirtAddr,
    size: u64,
    mapper: &mut impl Mapper<Size4KiB>,
    frames: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    let first = Page::containing_address(start);
    let last = Page::containing_address(start + size - 1u64);
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
    for page in Page::range_inclusive(first, last) {
        let frame = frames.allocate_frame().ok_or(MapToError::FrameAllocationFailed)?;
        unsafe { mapper.map_to(page, frame, flags, frames)?.flush() };
    }
    Ok(())
}
