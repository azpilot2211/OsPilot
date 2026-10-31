# OsPilot handoff

Last updated: 2026-10-31

## Goal

An OS written from scratch in Rust that Michael can boot day to day on his
own PC. It should show graphics on his card, get online, and run a web
browser that can eventually play YouTube.

**His PC:** a normal UEFI Windows 11 machine with AMD Radeon graphics. The
network card is a Realtek RTL8852BE, which is Wi-Fi only.
**Testing:** QEMU first, then a USB stick on the real PC.
**Michael's background:** new to Rust, so explain the Rust as we go.
**Deadline:** none.

## The plan

1. **Boot and draw on screen:** done, merged in PR #1.
2. **Core OS:** done, in PR #2. It still needs to be merged.
3. **Internet:**
   - First a network driver for QEMU (virtio-net or e1000).
   - Then on the real PC, a USB Ethernet adapter (about $10 to $15) or USB
     tethering from a phone.
   - TCP/IP (likely with the `smoltcp` crate), DHCP, DNS, then HTTPS (likely
     with `rustls`).
   - A driver for the built-in RTL8852BE Wi-Fi is a late stretch goal: it
     needs Realtek firmware, the 802.11 protocol, and WPA2/WPA3.
4. **Browser:**
   - HTML and CSS layout with images first.
   - Then JavaScript and video decoding, working toward YouTube. This is the
     long-term finish line, not an early milestone.

A native Radeon GPU driver is only needed later, for speed. Showing a picture
already works through the UEFI framebuffer.

## Completed this session

### Step 1 (PR #1, merged)

- A `no_std` kernel loaded by the `bootloader` crate (v0.11, UEFI only).
- A text console drawn on the UEFI framebuffer, plus serial output.
- GDT/TSS/IDT setup, the 8259 PIC, and PS/2 keyboard input.
- The `xtask` build helper:
  - `cargo xtask run` boots in QEMU.
  - `cargo xtask image --release` builds an image for a USB stick.
- README with Windows 11 setup and Rufus/USB boot steps.

### Step 2 (PR #2, ready for review, not merged yet)

- **Memory** (`memory.rs`, `allocator.rs`):
  - A frame allocator and page table access.
  - A 64 MiB heap that is safe to use while interrupts and task switches happen.
  - Uncached mapping for device registers, and DMA buffers.
- **Multitasking** (`task.rs`):
  - A preemptive round-robin scheduler with a hand-written assembly timer
    handler that switches task stacks.
  - Tasks can sleep.
- **APIC** (`apic.rs`):
  - Reads the ACPI MADT to find the local APIC and I/O APIC.
  - Calibrates the APIC timer against the PIT.
  - Routes the keyboard and mouse through the I/O APIC, then switches off the
    old PIC.
  - Falls back to the PIC if anything is missing.
- **Desktop** (`mouse.rs`, `gfx.rs`, `desktop.rs`):
  - A PS/2 mouse driver.
  - A desktop with a wallpaper, a taskbar with an uptime clock, and Terminal,
    About and Files windows.
  - Windows can be dragged and brought to the front.
- **Storage** (`pci.rs`, `usb/xhci.rs`, `usb/storage.rs`, `fat.rs`, `files.rs`):
  - A PCI scan.
  - An xHCI USB controller driver: takes over from the firmware, uses
    command/event/transfer rings, and polls rather than using interrupts.
  - USB mass storage over bulk-only transport with SCSI commands.
  - GPT and MBR partition tables.
  - A read-only FAT12/16/32 reader with long file names.
- **QEMU:** now boots from an emulated USB stick on xHCI, matching real
  hardware.

### Testing

Everything was tested in QEMU with OVMF:

- Typing and window dragging work.
- The APIC timer keeps correct time.
- A 4.5 MB file read back over USB matched the original byte for byte.

**Nothing has been tested on Michael's real PC yet.**

## Known limits

- **Keyboard and mouse are PS/2 only.** Laptop keyboards usually work. USB
  keyboards and mice, and many touchpads, won't work until there is a USB HID
  driver.
- **The USB driver polls instead of using interrupts,** and only one stick is
  used.
- **The file system is read-only.**
- **Memory frames are never freed,** and tasks only free their stacks when a
  slot is reused.
- **QEMU quirk:** in QEMU monitor tests, the first mouse event after boot is
  dropped. This looks like QEMU, not our driver.

## What's next

1. **Merge PR #2.** Michael asks Claude to do the merge.
2. **Try the real PC (optional but valuable):**
   - Run `cargo xtask image --release`.
   - Write the image to a stick with Rufus in DD mode.
   - Turn off Secure Boot, then boot from the stick.
   - Report what shows on screen.
3. **Start step 3:**
   - A virtio-net or e1000 driver in QEMU (add a network device in
     `xtask/src/main.rs`).
   - Then `smoltcp` with DHCP, a ping or DNS lookup shown in the Terminal
     window, then a plain HTTP fetch.
4. **Then:**
   - A USB Ethernet (CDC-ECM/NCM) driver on top of the existing xHCI code.
   - USB HID keyboard and mouse.

## How to build and run

- `cargo xtask run`: build, then boot in a QEMU window.
- `cargo xtask run --serial`: no window; output goes to the terminal.
- `cargo xtask image --release`: builds `target/ospilot-uefi-release.img` for
  a USB stick.
- To use OVMF firmware you already have, set `OVMF_CODE` and `OVMF_VARS`.
  Otherwise it is downloaded automatically.
- The Rust nightly version is pinned in `rust-toolchain.toml` and installs
  automatically.
