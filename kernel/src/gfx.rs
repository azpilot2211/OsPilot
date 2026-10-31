//! Simple 2D drawing into an off-screen image ("canvas").
//!
//! The desktop draws a whole frame here first, then copies it to the screen
//! in one go. Drawing straight to the screen would flicker.

use alloc::vec::Vec;
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};

/// A colour as `0x00RRGGBB`.
pub type Colour = u32;

pub const FONT_HEIGHT: RasterHeight = RasterHeight::Size16;
pub const CHAR_WIDTH: usize = get_raster_width(FontWeight::Regular, FONT_HEIGHT);
pub const LINE_HEIGHT: usize = FONT_HEIGHT.val() + 2;

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, pixels: alloc::vec![0; width * height] }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: usize, h: usize, colour: Colour) {
        let Some((x0, y0, x1, y1)) = self.clip(x, y, w, h) else { return };
        for row in y0..y1 {
            self.pixels[row * self.width + x0..row * self.width + x1].fill(colour);
        }
    }

    /// A vertical gradient, used for the wallpaper.
    pub fn gradient(&mut self, top: Colour, bottom: Colour) {
        for y in 0..self.height {
            let t = (y * 255 / self.height.max(1)) as u8;
            let colour = blend(top, bottom, t);
            self.pixels[y * self.width..(y + 1) * self.width].fill(colour);
        }
    }

    /// One-pixel outline of a rectangle.
    pub fn outline(&mut self, x: i32, y: i32, w: usize, h: usize, colour: Colour) {
        self.fill_rect(x, y, w, 1, colour);
        self.fill_rect(x, y + h as i32 - 1, w, 1, colour);
        self.fill_rect(x, y, 1, h, colour);
        self.fill_rect(x + w as i32 - 1, y, 1, h, colour);
    }

    /// Draw text with its top-left corner at (x, y). Stops at `max_width`
    /// pixels. Returns the width drawn.
    pub fn text(&mut self, x: i32, y: i32, text: &str, colour: Colour, bold: bool, max_width: usize) -> usize {
        let weight = if bold { FontWeight::Bold } else { FontWeight::Regular };
        let mut cx = x;
        for c in text.chars() {
            if (cx - x) as usize + CHAR_WIDTH > max_width {
                break;
            }
            let raster = get_raster(c, weight, FONT_HEIGHT)
                .unwrap_or_else(|| get_raster('?', weight, FONT_HEIGHT).unwrap());
            for (dy, row) in raster.raster().iter().enumerate() {
                for (dx, &alpha) in row.iter().enumerate() {
                    if alpha > 0 {
                        self.blend_pixel(cx + dx as i32, y + dy as i32, colour, alpha);
                    }
                }
            }
            cx += raster.width() as i32;
        }
        (cx - x) as usize
    }

    /// Draw a small picture given as text rows: `#` is `fg`, `.` is `bg`,
    /// anything else is transparent. Used for the mouse pointer.
    pub fn sprite(&mut self, x: i32, y: i32, rows: &[&str], fg: Colour, bg: Colour) {
        for (dy, row) in rows.iter().enumerate() {
            for (dx, c) in row.chars().enumerate() {
                let colour = match c {
                    '#' => fg,
                    '.' => bg,
                    _ => continue,
                };
                self.fill_rect(x + dx as i32, y + dy as i32, 1, 1, colour);
            }
        }
    }

    fn blend_pixel(&mut self, x: i32, y: i32, colour: Colour, alpha: u8) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        let index = y as usize * self.width + x as usize;
        self.pixels[index] = blend(self.pixels[index], colour, alpha);
    }

    /// Clip a rectangle to the canvas. Returns (x0, y0, x1, y1) or `None`
    /// if nothing is visible.
    fn clip(&self, x: i32, y: i32, w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
        let x0 = x.max(0) as usize;
        let y0 = y.max(0) as usize;
        let x1 = ((x + w as i32).max(0) as usize).min(self.width);
        let y1 = ((y + h as i32).max(0) as usize).min(self.height);
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }
}

/// Mix colour `a` towards `b` by `t` out of 255.
pub fn blend(a: Colour, b: Colour, t: u8) -> Colour {
    let t = t as u32;
    let mix = |shift: u32| {
        let ca = (a >> shift) & 0xFF;
        let cb = (b >> shift) & 0xFF;
        ((ca * (255 - t) + cb * t) / 255) << shift
    };
    mix(16) | mix(8) | mix(0)
}
