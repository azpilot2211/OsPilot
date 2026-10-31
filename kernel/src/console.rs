//! A text console drawn pixel by pixel onto the UEFI framebuffer.
//!
//! The framebuffer is a big block of memory where each pixel's colour is
//! stored. Writing to it changes what's on screen. This works on any GPU
//! UEFI supports, without needing a real graphics driver yet.
//!
//! During boot the console draws text straight to the screen. Once the
//! desktop starts it owns the screen instead, and the console just keeps a
//! log of text lines that the desktop shows in its terminal window.

use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};
use spin::Mutex;

const FONT_WEIGHT: FontWeight = FontWeight::Regular;
const FONT_HEIGHT: RasterHeight = RasterHeight::Size16;
const CHAR_WIDTH: usize = get_raster_width(FONT_WEIGHT, FONT_HEIGHT);
const LINE_HEIGHT: usize = FONT_HEIGHT.val() + 2;
const MARGIN: usize = 8;

const BACKGROUND: Rgb = Rgb(0x10, 0x18, 0x28);
const FOREGROUND: Rgb = Rgb(0xE6, 0xE6, 0xE6);
const ACCENT: Rgb = Rgb(0x3A, 0x8D, 0xFF);
const BANNER_HEIGHT: usize = 6;

#[derive(Clone, Copy)]
struct Rgb(u8, u8, u8);

pub struct Console {
    buffer: &'static mut [u8],
    info: FrameBufferInfo,
    x: usize,
    y: usize,
    /// Lines of text printed so far (once the heap exists).
    log: Option<Vec<String>>,
    /// While true, text only goes to the log; the desktop draws the screen.
    desktop_active: bool,
    /// Bumped on every change, so the desktop knows when to redraw.
    pub version: u64,
}

/// Keep at most this many lines of history.
const MAX_LOG_LINES: usize = 500;

pub static CONSOLE: Mutex<Option<Console>> = Mutex::new(None);

pub fn init(framebuffer: &'static mut FrameBuffer) {
    let info = framebuffer.info();
    let mut console = Console {
        buffer: framebuffer.buffer_mut(),
        info,
        x: MARGIN,
        y: MARGIN + BANNER_HEIGHT,
        log: None,
        desktop_active: false,
        version: 0,
    };
    console.clear();
    *CONSOLE.lock() = Some(console);
}

/// Start recording printed text. Needs the heap, so call it after
/// `allocator::init`.
pub fn enable_log() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(console) = CONSOLE.lock().as_mut() {
            console.log = Some(alloc::vec![String::new()]);
        }
    });
}

impl Console {
    pub fn width(&self) -> usize {
        self.info.width
    }

    pub fn height(&self) -> usize {
        self.info.height
    }

    pub fn log(&self) -> &[String] {
        self.log.as_deref().unwrap_or(&[])
    }

    /// Hand the screen over to the desktop (or take it back, e.g. on panic).
    pub fn set_desktop_active(&mut self, active: bool) {
        self.desktop_active = active;
        if !active {
            self.clear();
        }
    }

    /// Copy a full-screen image (one `0x00RRGGBB` value per pixel) to the
    /// framebuffer.
    pub fn present(&mut self, pixels: &[u32]) {
        let (width, height) = (self.info.width, self.info.height);
        let (stride, bpp) = (self.info.stride, self.info.bytes_per_pixel);
        let bgr = self.info.pixel_format == PixelFormat::Bgr;
        if bpp == 4 {
            // Fast path: 4 bytes per pixel is what almost every GPU uses.
            for y in 0..height {
                let src = &pixels[y * width..(y + 1) * width];
                let dst = &mut self.buffer[y * stride * 4..(y * stride + width) * 4];
                for (out, &p) in dst.chunks_exact_mut(4).zip(src) {
                    let [b, g, r, _] = p.to_le_bytes();
                    out.copy_from_slice(&if bgr { [b, g, r, 0] } else { [r, g, b, 0] });
                }
            }
        } else {
            for y in 0..height {
                for x in 0..width {
                    let [b, g, r, _] = pixels[y * width + x].to_le_bytes();
                    self.put_pixel(x, y, Rgb(r, g, b));
                }
            }
        }
    }

    fn record(&mut self, c: char) {
        self.version += 1;
        let Some(log) = self.log.as_mut() else { return };
        match c {
            '\n' => {
                if log.len() >= MAX_LOG_LINES {
                    log.remove(0);
                }
                log.push(String::new());
            }
            '\u{8}' => {
                log.last_mut().map(String::pop);
            }
            '\r' => {}
            c => log.last_mut().unwrap().push(c),
        }
    }

    fn clear(&mut self) {
        for y in 0..self.info.height {
            let colour = if y < BANNER_HEIGHT { ACCENT } else { BACKGROUND };
            for x in 0..self.info.width {
                self.put_pixel(x, y, colour);
            }
        }
        self.x = MARGIN;
        self.y = MARGIN + BANNER_HEIGHT;
    }

    fn put_pixel(&mut self, x: usize, y: usize, Rgb(r, g, b): Rgb) {
        let bpp = self.info.bytes_per_pixel;
        let offset = (y * self.info.stride + x) * bpp;
        let bytes = match self.info.pixel_format {
            PixelFormat::Rgb => [r, g, b, 0],
            PixelFormat::Bgr => [b, g, r, 0],
            PixelFormat::U8 => [((r as u16 + g as u16 + b as u16) / 3) as u8, 0, 0, 0],
            other => panic!("unsupported pixel format {other:?}"),
        };
        let len = bpp.min(4);
        self.buffer[offset..offset + len].copy_from_slice(&bytes[..len]);
    }

    fn newline(&mut self) {
        self.x = MARGIN;
        self.y += LINE_HEIGHT;
        if self.y + LINE_HEIGHT + MARGIN > self.info.height {
            self.scroll();
        }
    }

    /// Move every text row up by one line and blank the bottom line.
    fn scroll(&mut self) {
        let row_bytes = self.info.stride * self.info.bytes_per_pixel;
        let text_top = (MARGIN + BANNER_HEIGHT) * row_bytes;
        let shift = LINE_HEIGHT * row_bytes;
        let end = self.info.height * row_bytes;
        self.buffer.copy_within(text_top + shift..end, text_top);
        self.y -= LINE_HEIGHT;
        for y in self.y..self.info.height {
            for x in 0..self.info.width {
                self.put_pixel(x, y, BACKGROUND);
            }
        }
    }

    fn backspace(&mut self) {
        if self.x < MARGIN + CHAR_WIDTH {
            return;
        }
        self.x -= CHAR_WIDTH;
        for dy in 0..LINE_HEIGHT {
            for dx in 0..CHAR_WIDTH {
                self.put_pixel(self.x + dx, self.y + dy, BACKGROUND);
            }
        }
    }

    fn write_char(&mut self, c: char) {
        self.record(c);
        if self.desktop_active {
            return;
        }
        match c {
            '\n' => self.newline(),
            '\r' => self.x = MARGIN,
            '\u{8}' => self.backspace(),
            c => {
                if self.x + CHAR_WIDTH + MARGIN > self.info.width {
                    self.newline();
                }
                let raster = get_raster(c, FONT_WEIGHT, FONT_HEIGHT)
                    .unwrap_or_else(|| get_raster('?', FONT_WEIGHT, FONT_HEIGHT).unwrap());
                for (dy, row) in raster.raster().iter().enumerate() {
                    for (dx, &intensity) in row.iter().enumerate() {
                        let colour = blend(BACKGROUND, FOREGROUND, intensity);
                        self.put_pixel(self.x + dx, self.y + dy, colour);
                    }
                }
                self.x += raster.width();
            }
        }
    }
}

/// Mix two colours; the font gives us anti-aliased edges as 0..=255 values.
fn blend(bg: Rgb, fg: Rgb, t: u8) -> Rgb {
    let mix = |a: u8, b: u8| ((a as u16 * (255 - t as u16) + b as u16 * t as u16) / 255) as u8;
    Rgb(mix(bg.0, fg.0), mix(bg.1, fg.1), mix(bg.2, fg.2))
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            self.write_char(c);
        }
        Ok(())
    }
}

/// Used by the `print!` macros. Writes to both the screen and serial port.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    // Turn interrupts off while holding the lock, otherwise a key press could
    // try to print while we're mid-print and the two would deadlock.
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(console) = CONSOLE.lock().as_mut() {
            let _ = console.write_fmt(args);
        }
        struct Serial;
        impl fmt::Write for Serial {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                crate::serial::write_str(s);
                Ok(())
            }
        }
        let _ = Serial.write_fmt(args);
    });
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
