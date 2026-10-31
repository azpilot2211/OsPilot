//! Build helper for OsPilot. Run it with `cargo xtask <command>`:
//!
//!   cargo xtask run        build the kernel and boot it in QEMU
//!   cargo xtask run --serial   same, but with no window (output in terminal)
//!   cargo xtask image      build a disk image you can write to a USB stick
//!
//! Add `--release` to any command for an optimised build.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let release = args.iter().any(|a| a == "--release");
    let serial_only = args.iter().any(|a| a == "--serial");

    match args.first().map(String::as_str) {
        Some("image") => {
            let image = build_image(release)?;
            println!("Disk image: {}", image.display());
            println!("Write it to a USB stick with Rufus (DD mode) or `dd`.");
        }
        Some("run") => {
            let image = build_image(release)?;
            run_qemu(&image, serial_only)?;
        }
        _ => {
            eprintln!("usage: cargo xtask <run|image> [--release] [--serial]");
            exit(1);
        }
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Compile the kernel for bare metal, then wrap it in a UEFI-bootable disk.
fn build_image(release: bool) -> Result<PathBuf> {
    let root = workspace_root();
    let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo
        .current_dir(&root)
        .args(["build", "--package", "kernel", "--target", "x86_64-unknown-none"]);
    if release {
        cargo.arg("--release");
    }
    if !cargo.status()?.success() {
        return Err("kernel build failed".into());
    }

    let profile = if release { "release" } else { "debug" };
    let kernel = root.join("target/x86_64-unknown-none").join(profile).join("kernel");
    let image = root.join("target").join(format!("ospilot-uefi-{profile}.img"));
    bootloader::UefiBoot::new(&kernel).create_disk_image(&image)?;
    Ok(image)
}

/// Boot the image in QEMU using OVMF, an open-source UEFI firmware.
fn run_qemu(image: &Path, serial_only: bool) -> Result<()> {
    use ovmf_prebuilt::{Arch, FileType, Prebuilt, Source};

    let ovmf_dir = workspace_root().join("target/ovmf");
    // Set OVMF_CODE and OVMF_VARS to use firmware you already have;
    // otherwise a prebuilt copy is downloaded once and cached.
    let (code, vars_template) = match (std::env::var_os("OVMF_CODE"), std::env::var_os("OVMF_VARS")) {
        (Some(code), Some(vars)) => (PathBuf::from(code), PathBuf::from(vars)),
        _ => {
            let ovmf = Prebuilt::fetch(Source::LATEST, &ovmf_dir)
                .map_err(|e| format!("couldn't download OVMF firmware: {e:?}"))?;
            (ovmf.get_file(Arch::X64, FileType::Code), ovmf.get_file(Arch::X64, FileType::Vars))
        }
    };
    // The firmware writes its settings into the vars file, so use a copy.
    std::fs::create_dir_all(&ovmf_dir)?;
    let vars = ovmf_dir.join("vars.fd");
    if !vars.exists() {
        std::fs::copy(&vars_template, &vars)?;
    }

    let mut qemu = Command::new("qemu-system-x86_64");
    qemu.arg("-drive")
        .arg(format!("if=pflash,format=raw,readonly=on,file={}", code.display()))
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", vars.display()))
        // Attach the image as a USB stick, like on a real PC.
        .args(["-device", "qemu-xhci,id=xhci"])
        .arg("-drive")
        .arg(format!("if=none,id=stick,format=raw,file={}", image.display()))
        .args(["-device", "usb-storage,bus=xhci.0,drive=stick,bootindex=0"])
        .args(["-m", "512M", "-serial", "stdio"]);
    if serial_only {
        qemu.args(["-display", "none"]);
    }
    let status = qemu
        .status()
        .map_err(|e| format!("couldn't start qemu-system-x86_64 (is QEMU installed?): {e}"))?;
    exit(status.code().unwrap_or(1));
}
