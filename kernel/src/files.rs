//! Finds the USB stick, opens its file system, and keeps a listing of it for
//! the desktop's Files window. Runs as its own task so a slow stick doesn't
//! hold anything else up.

use crate::fat::FileSystem;
use crate::usb::{storage::UsbStorage, xhci::Controller};
use crate::println;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

pub struct Listing {
    pub status: String,
    /// (indented display name, size in bytes or `None` for folders)
    pub lines: Vec<(String, Option<u32>)>,
    pub version: u64,
}

pub static LISTING: Mutex<Listing> =
    Mutex::new(Listing { status: String::new(), lines: Vec::new(), version: 0 });

fn set_status(status: String) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut listing = LISTING.lock();
        listing.status = status;
        listing.version += 1;
    });
}

pub fn run() {
    set_status("Looking for a USB stick...".into());
    match open() {
        Ok(mut fs) => {
            println!(
                "USB stick: {} ({} MiB), FAT partition at block {}.",
                fs.disk_name(),
                fs.disk_size() / (1024 * 1024),
                fs.partition_start()
            );
            let mut lines = Vec::new();
            list_tree(&mut fs, "", 0, &mut lines);
            read_speed_test(&mut fs);
            x86_64::instructions::interrupts::without_interrupts(|| {
                let mut listing = LISTING.lock();
                listing.status = format!("{} ({} MiB)", fs.disk_name(), fs.disk_size() / (1024 * 1024));
                listing.lines = lines;
                listing.version += 1;
            });
        }
        Err(e) => {
            println!("USB storage: {e}");
            set_status(format!("No USB stick: {e}"));
        }
    }
}

fn open() -> Result<FileSystem, &'static str> {
    let mut error = "no xHCI USB controller found";
    for controller in Controller::all() {
        match UsbStorage::find(controller).and_then(FileSystem::open) {
            Ok(fs) => return Ok(fs),
            Err(e) => error = e,
        }
    }
    Err(error)
}

/// Read the biggest file in the top folder, to check reading works and
/// see how fast it is.
fn read_speed_test(fs: &mut FileSystem) {
    let Ok(entries) = fs.list("") else { return };
    let Some(biggest) = entries.iter().filter(|e| !e.is_dir).max_by_key(|e| e.size) else { return };
    let start = crate::task::ticks();
    match fs.read_file(&biggest.name) {
        Ok(data) => {
            let ms = (crate::task::ticks() - start) * 1000 / crate::task::TICKS_PER_SECOND;
            println!("Read {} ({} KiB) from the stick in {ms} ms.", biggest.name, data.len() / 1024);
        }
        Err(e) => println!("Reading {} failed: {e}", biggest.name),
    }
}

/// List a folder and (up to a few levels down) its subfolders.
fn list_tree(fs: &mut FileSystem, path: &str, depth: usize, lines: &mut Vec<(String, Option<u32>)>) {
    let Ok(entries) = fs.list(path) else { return };
    for entry in entries {
        let indent = "    ".repeat(depth);
        if entry.is_dir {
            lines.push((format!("{indent}{}/", entry.name), None));
            if depth < 3 {
                list_tree(fs, &format!("{path}/{}", entry.name), depth + 1, lines);
            }
        } else {
            lines.push((format!("{indent}{}", entry.name), Some(entry.size)));
        }
    }
}
