# OsPilot

An operating system written from scratch in Rust, aiming to be bootable day to
day on a normal UEFI PC, with graphics, internet, and a web browser.

## Roadmap

1. **Boot and draw on screen** (done): boots via UEFI in QEMU or from a USB
   stick, draws text on the screen the firmware sets up, reads the keyboard.
2. **Core OS** (in progress): memory management, multitasking, mouse and a
   simple windowed desktop are done; USB stick file system and real-hardware
   interrupt setup (ACPI/APIC) are next.
3. **Internet**: network drivers (QEMU first, then a USB Ethernet adapter or
   phone tethering), TCP/IP, DNS, HTTPS. Built-in Wi-Fi is a later stretch goal.
4. **Browser**: HTML and CSS with images, then JavaScript and video, working
   toward YouTube.

## Project layout

| Path | What it is |
| --- | --- |
| `kernel/` | The OS itself. Runs on bare metal with no standard library. |
| `xtask/` | A helper program that builds the kernel into a bootable disk image and runs QEMU. |
| `rust-toolchain.toml` | Pins the Rust version (installed automatically). |

## Setting up on Windows 11

1. **Install Rust:** download and run `rustup-init.exe` from
   <https://rustup.rs>. When it asks, let it install the Visual Studio C++
   Build Tools (Rust needs them on Windows).
2. **Install QEMU** (the virtual machine): get the 64-bit installer from
   <https://qemu.weilnetz.de/w64/>, then add `C:\Program Files\qemu` to your
   `PATH` (Start menu, "Edit the system environment variables").
3. **Install Git** from <https://git-scm.com> and clone this repo.
4. Open a terminal in the repo folder and run:

   ```
   cargo xtask run
   ```

   The first run downloads the right Rust version, UEFI firmware for QEMU,
   and all libraries, so it takes a few minutes. A QEMU window opens showing
   OsPilot. Click into it and type.

`cargo xtask run --serial` runs without a window and prints to the terminal.

## Booting on real hardware from a USB stick

> Writing the image **erases everything on the USB stick**.

1. Build a release image: `cargo xtask image --release`. It's written to
   `target\ospilot-uefi-release.img`.
2. Write it to the stick with [Rufus](https://rufus.ie): pick the image, and
   choose **DD mode** when asked.
3. Turn off **Secure Boot** in your PC's UEFI settings (OsPilot isn't
   signed). You can turn it back on afterwards to boot Windows normally.
4. Reboot, open the boot menu (often F12, F11, F8 or Esc), and pick the USB
   stick.

Keyboard and mouse currently use the legacy PS/2 interface. Most laptop
keyboards support it, but an external USB keyboard or mouse, and many laptop
touchpads, may not work until the USB driver arrives. Press the power button to exit.
