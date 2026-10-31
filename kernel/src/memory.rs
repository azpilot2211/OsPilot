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
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
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

/// The page tables and frame allocator, shared by everything that needs to
/// map memory.
pub struct Memory {
    pub mapper: OffsetPageTable<'static>,
    pub frames: BootInfoFrameAllocator,
    /// Next free address in the area we use for device memory.
    next_mmio: u64,
}

pub static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);
static PHYSICAL_MEMORY_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Device registers get mapped here, well away from the heap.
const MMIO_START: u64 = 0x_5555_0000_0000;

/// Take ownership of the active page tables and the free RAM.
///
/// # Safety
/// All physical memory must be mapped at `physical_memory_offset`, the
/// memory map must be accurate, and this must only be called once.
pub unsafe fn init(physical_memory_offset: VirtAddr, regions: &'static MemoryRegions) {
    let (level_4_frame, _) = Cr3::read();
    let virt = physical_memory_offset + level_4_frame.start_address().as_u64();
    let level_4_table = unsafe { &mut *virt.as_mut_ptr::<PageTable>() };
    let mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };
    let frames = unsafe { BootInfoFrameAllocator::new(regions) };
    PHYSICAL_MEMORY_OFFSET.store(physical_memory_offset.as_u64(), Ordering::Relaxed);
    *MEMORY.lock() = Some(Memory { mapper, frames, next_mmio: MMIO_START });
}

/// The virtual address of a physical RAM address (the bootloader maps all
/// of RAM at a fixed offset).
pub fn phys_to_virt(phys: u64) -> VirtAddr {
    VirtAddr::new(PHYSICAL_MEMORY_OFFSET.load(Ordering::Relaxed) + phys)
}

/// Map a device's registers into memory, with caching turned off so every
/// read and write really reaches the device.
pub fn map_mmio(phys: u64, size: u64) -> VirtAddr {
    let mut guard = MEMORY.lock();
    let memory = guard.as_mut().expect("memory not initialised");
    let first = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(phys));
    let last = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(phys + size - 1));
    let virt_base = memory.next_mmio;
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::NO_CACHE
        | PageTableFlags::WRITE_THROUGH
        | PageTableFlags::NO_EXECUTE;
    for (i, frame) in PhysFrame::range_inclusive(first, last).enumerate() {
        let page = Page::containing_address(VirtAddr::new(virt_base + i as u64 * 4096));
        let Memory { mapper, frames, .. } = memory;
        unsafe { mapper.map_to(page, frame, flags, frames).expect("mmio map failed").flush() };
    }
    let pages = (last.start_address() - first.start_address()) / 4096 + 1;
    memory.next_mmio += pages * 4096;
    VirtAddr::new(virt_base + (phys & 0xFFF))
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

/// A block of physically contiguous RAM for a device to read and write
/// directly (DMA). Devices need physical addresses; we use the virtual one.
pub struct DmaBuffer {
    pub phys: u64,
    pub virt: *mut u8,
    pub size: usize,
}

unsafe impl Send for DmaBuffer {}

impl DmaBuffer {
    /// Allocate `size` bytes (rounded up to whole 4 KiB pages), zeroed.
    pub fn new(size: usize) -> Self {
        let pages = size.div_ceil(4096).max(1);
        let mut guard = MEMORY.lock();
        let memory = guard.as_mut().expect("memory not initialised");
        // Our frame allocator hands out frames in address order, so a run of
        // allocations is usually contiguous. If it isn't (we crossed into a
        // new region), start the run again.
        'retry: loop {
            let first = memory.frames.allocate_frame().expect("out of memory").start_address().as_u64();
            for i in 1..pages as u64 {
                let next = memory.frames.allocate_frame().expect("out of memory").start_address().as_u64();
                if next != first + i * 4096 {
                    continue 'retry;
                }
            }
            let virt = phys_to_virt(first).as_mut_ptr::<u8>();
            unsafe { core::ptr::write_bytes(virt, 0, pages * 4096) };
            return Self { phys: first, virt, size: pages * 4096 };
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.virt, self.size) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.virt, self.size) }
    }

    pub fn read_u32(&self, offset: usize) -> u32 {
        unsafe { core::ptr::read_volatile(self.virt.add(offset) as *const u32) }
    }

    pub fn write_u32(&mut self, offset: usize, value: u32) {
        unsafe { core::ptr::write_volatile(self.virt.add(offset) as *mut u32, value) }
    }

    pub fn write_u64(&mut self, offset: usize, value: u64) {
        self.write_u32(offset, value as u32);
        self.write_u32(offset + 4, (value >> 32) as u32);
    }
}
