//! The desktop: wallpaper, a taskbar, draggable windows and a mouse pointer.
//!
//! It runs as its own task. Each frame it reads the mouse, updates windows,
//! draws everything into an off-screen canvas, and copies that to the screen.
//! It only redraws when something changed, to leave the CPU free.

use crate::console::CONSOLE;
use crate::gfx::{CHAR_WIDTH, Canvas, Colour, LINE_HEIGHT};
use crate::{mouse, task};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use x86_64::instructions::interrupts::without_interrupts;

const WALLPAPER_TOP: Colour = 0x1B2B4B;
const WALLPAPER_BOTTOM: Colour = 0x0B1220;
const TASKBAR: Colour = 0x0E1626;
const TASKBAR_HEIGHT: usize = 32;
const ACCENT: Colour = 0x3A8DFF;
const WINDOW_BG: Colour = 0x141C2C;
const TITLE_ACTIVE: Colour = 0x2A5DB0;
const TITLE_INACTIVE: Colour = 0x2A3242;
const TITLE_HEIGHT: usize = 26;
const BORDER: Colour = 0x4A5468;
const TEXT: Colour = 0xE6E6E6;
const TEXT_DIM: Colour = 0x8A94A8;

/// What a window shows.
enum Content {
    /// The console log, with whatever you type.
    Terminal,
    About,
}

struct Window {
    title: &'static str,
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    content: Content,
}

impl Window {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w as i32 && y < self.y + self.h as i32
    }

    fn in_title_bar(&self, x: i32, y: i32) -> bool {
        self.contains(x, y) && y < self.y + TITLE_HEIGHT as i32
    }
}

const POINTER: [&str; 19] = [
    ".", "..", ".#.", ".##.", ".###.", ".####.", ".#####.", ".######.", ".#######.",
    ".########.", ".#########.", ".##########.", ".######.....", ".###.##.", ".##..##.",
    ".#.  .##.", "..   .##.", "      .##.", "       ..",
];

pub fn run() {
    let (width, height) = without_interrupts(|| {
        let console = CONSOLE.lock();
        let console = console.as_ref().expect("no screen");
        (console.width(), console.height())
    });
    let mut canvas = Canvas::new(width, height);

    // Windows are drawn in list order, so the last one is on top.
    let terminal_w = (width * 3 / 5).max(400);
    let terminal_h = (height * 3 / 5).max(300);
    let mut windows: Vec<Window> = alloc::vec![
        Window { title: "About OsPilot", x: 60, y: 60, w: 360, h: 170, content: Content::About },
        Window {
            title: "Terminal",
            x: (width - terminal_w) as i32 / 2 + 80,
            y: (height - terminal_h) as i32 / 2 - 20,
            w: terminal_w,
            h: terminal_h,
            content: Content::Terminal,
        },
    ];

    let (mut mx, mut my) = (width as i32 / 2, height as i32 / 2);
    let mut was_pressed = false;
    // While dragging: offset from the window corner to the pointer.
    let mut drag: Option<(i32, i32)> = None;
    let mut last_drawn: Option<(u64, u64, i32, i32, i32, i32)> = None;

    without_interrupts(|| {
        if let Some(console) = CONSOLE.lock().as_mut() {
            console.set_desktop_active(true);
        }
    });

    loop {
        let (dx, dy, buttons) = mouse::take_state();
        mx = (mx + dx).clamp(0, width as i32 - 1);
        my = (my + dy).clamp(0, height as i32 - 1);
        let pressed = buttons & mouse::LEFT_BUTTON != 0;

        if pressed && !was_pressed {
            // Click: bring the window under the pointer to the front, and
            // start dragging if the click was on its title bar.
            if let Some(index) = windows.iter().rposition(|w| w.contains(mx, my)) {
                let window = windows.remove(index);
                if window.in_title_bar(mx, my) {
                    drag = Some((mx - window.x, my - window.y));
                }
                windows.push(window);
            }
        } else if !pressed {
            drag = None;
        }
        if let (Some((ox, oy)), Some(top)) = (drag, windows.last_mut()) {
            top.x = (mx - ox).clamp(-(top.w as i32) + 60, width as i32 - 60);
            top.y = (my - oy).clamp(0, (height - TASKBAR_HEIGHT - TITLE_HEIGHT) as i32);
        }
        was_pressed = pressed;

        let log_version = without_interrupts(|| CONSOLE.lock().as_ref().map_or(0, |c| c.version));
        let seconds = task::ticks() / task::TICKS_PER_SECOND;
        let (top_x, top_y) = windows.last().map_or((0, 0), |w| (w.x, w.y));
        let state = (log_version, seconds, mx, my, top_x, top_y);
        if last_drawn != Some(state) {
            draw(&mut canvas, &windows, mx, my, seconds);
            without_interrupts(|| {
                if let Some(console) = CONSOLE.lock().as_mut() {
                    console.present(&canvas.pixels);
                }
            });
            last_drawn = Some(state);
        }
        task::sleep_ms(16);
    }
}

fn draw(canvas: &mut Canvas, windows: &[Window], mx: i32, my: i32, seconds: u64) {
    let (width, height) = (canvas.width, canvas.height);
    canvas.gradient(WALLPAPER_TOP, WALLPAPER_BOTTOM);
    canvas.text(width as i32 - 120, 16, "OsPilot", 0x2A3D60, true, 200);

    for (i, window) in windows.iter().enumerate() {
        draw_window(canvas, window, i + 1 == windows.len());
    }

    // Taskbar along the bottom, with the uptime clock on the right.
    let bar_y = (height - TASKBAR_HEIGHT) as i32;
    canvas.fill_rect(0, bar_y, width, TASKBAR_HEIGHT, TASKBAR);
    canvas.fill_rect(0, bar_y, width, 1, BORDER);
    canvas.fill_rect(8, bar_y + 6, 20, 20, ACCENT);
    canvas.text(36, bar_y + 7, "OsPilot", TEXT, true, 200);
    let mut x = 140;
    for window in windows {
        let w = window.title.len() * CHAR_WIDTH + 20;
        canvas.fill_rect(x, bar_y + 4, w, TASKBAR_HEIGHT - 8, TITLE_INACTIVE);
        canvas.text(x + 10, bar_y + 7, window.title, TEXT, false, w);
        x += w as i32 + 6;
    }
    let clock = format!("up {:02}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60);
    canvas.text(width as i32 - (clock.len() * CHAR_WIDTH) as i32 - 12, bar_y + 7, &clock, TEXT_DIM, false, 200);

    canvas.sprite(mx, my, &POINTER, 0xFFFFFF, 0x000000);
}

fn draw_window(canvas: &mut Canvas, window: &Window, active: bool) {
    let (x, y, w, h) = (window.x, window.y, window.w, window.h);
    // A soft shadow, then the body, title bar and border.
    canvas.fill_rect(x + 4, y + 6, w, h, 0x05080F);
    canvas.fill_rect(x, y, w, h, WINDOW_BG);
    canvas.fill_rect(x, y, w, TITLE_HEIGHT, if active { TITLE_ACTIVE } else { TITLE_INACTIVE });
    canvas.outline(x, y, w, h, BORDER);
    canvas.text(x + 10, y + 5, window.title, TEXT, true, w - 20);

    let inner_x = x + 10;
    let inner_y = y + TITLE_HEIGHT as i32 + 8;
    let inner_w = w - 20;
    let inner_h = h - TITLE_HEIGHT - 16;
    match window.content {
        Content::About => {
            let lines = [
                (format!("OsPilot v{}", env!("CARGO_PKG_VERSION")), TEXT),
                (format!("Screen: {} x {}", canvas.width, canvas.height), TEXT_DIM),
                (format!("Heap in use: {} KiB", crate::allocator::used() / 1024), TEXT_DIM),
                ("Drag windows by their title bar.".into(), TEXT_DIM),
            ];
            for (i, (line, colour)) in lines.iter().enumerate() {
                canvas.text(inner_x, inner_y + (i * LINE_HEIGHT) as i32, line, *colour, i == 0, inner_w);
            }
        }
        Content::Terminal => {
            let cols = inner_w / CHAR_WIDTH;
            let rows = inner_h / LINE_HEIGHT;
            // Copy out just the lines we'll show, so the console isn't
            // locked (with interrupts off) while we draw.
            let lines: Vec<String> = without_interrupts(|| {
                let console = CONSOLE.lock();
                let Some(console) = console.as_ref() else { return Vec::new() };
                let mut wrapped = Vec::new();
                for line in console.log().iter().rev() {
                    let chars: Vec<char> = line.chars().collect();
                    let mut chunks: Vec<String> = chars.chunks(cols.max(1)).map(|c| c.iter().collect()).collect();
                    if chunks.is_empty() {
                        chunks.push(String::new());
                    }
                    wrapped.extend(chunks.into_iter().rev());
                    if wrapped.len() >= rows {
                        break;
                    }
                }
                wrapped.truncate(rows);
                wrapped.reverse();
                wrapped
            });
            for (i, line) in lines.iter().enumerate() {
                canvas.text(inner_x, inner_y + (i * LINE_HEIGHT) as i32, line, TEXT, false, inner_w);
            }
            // A block cursor after the last character.
            if let Some(last) = lines.last() {
                let row = lines.len() - 1;
                let cx = inner_x + (last.chars().count() * CHAR_WIDTH) as i32;
                canvas.fill_rect(cx, inner_y + (row * LINE_HEIGHT) as i32 + 2, CHAR_WIDTH, LINE_HEIGHT - 4, ACCENT);
            }
        }
    }
}
