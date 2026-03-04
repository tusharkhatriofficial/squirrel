use limine::framebuffer::Framebuffer as LimineFramebuffer;
use noto_sans_mono_bitmap::{get_raster, FontWeight, RasterHeight, RasterizedChar};
use spin::Mutex;

const CHAR_WIDTH: usize = 7; // noto-sans-mono size_16 character width
const CHAR_HEIGHT: usize = 16;
const FG_COLOR: u32 = 0x00E8E8E8; // near-white
const BG_COLOR: u32 = 0x00101018; // dark blue-black

static WRITER: Mutex<Option<FramebufferWriter>> = Mutex::new(None);

pub fn init(fb: &LimineFramebuffer) {
    let mut writer = FramebufferWriter {
        base: fb.addr() as *mut u32,
        width: fb.width() as usize,
        height: fb.height() as usize,
        stride: fb.pitch() as usize / 4, // pitch is in bytes; we work in u32 pixels
        cursor_x: 0,
        cursor_y: 0,
    };
    writer.clear();
    *WRITER.lock() = Some(writer);
}

struct FramebufferWriter {
    base: *mut u32,
    width: usize,
    height: usize,
    stride: usize,
    cursor_x: usize,
    cursor_y: usize,
}

unsafe impl Send for FramebufferWriter {}
unsafe impl Sync for FramebufferWriter {}

impl FramebufferWriter {
    fn put_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x < self.width && y < self.height {
            unsafe {
                *self.base.add(y * self.stride + x) = color;
            }
        }
    }

    fn clear(&mut self) {
        for y in 0..self.height {
            for x in 0..self.width {
                unsafe {
                    *self.base.add(y * self.stride + x) = BG_COLOR;
                }
            }
        }
    }

    fn scroll_up(&mut self) {
        let row_bytes = CHAR_HEIGHT * self.stride;
        unsafe {
            core::ptr::copy(
                self.base.add(row_bytes),
                self.base,
                (self.height - CHAR_HEIGHT) * self.stride,
            );
        }
        // Clear the last row
        for y in (self.height - CHAR_HEIGHT)..self.height {
            for x in 0..self.width {
                unsafe {
                    *self.base.add(y * self.stride + x) = BG_COLOR;
                }
            }
        }
        self.cursor_y -= 1;
    }

    fn write_char(&mut self, c: char) {
        if c == '\n' {
            self.cursor_x = 0;
            self.cursor_y += 1;
            let max_rows = self.height / CHAR_HEIGHT;
            if self.cursor_y >= max_rows {
                self.scroll_up();
            }
            return;
        }

        let raster: RasterizedChar = get_raster(c, FontWeight::Regular, RasterHeight::Size16)
            .unwrap_or_else(|| get_raster('?', FontWeight::Regular, RasterHeight::Size16).unwrap());

        let x_base = self.cursor_x * CHAR_WIDTH;
        let y_base = self.cursor_y * CHAR_HEIGHT;

        for (row, row_data) in raster.raster().iter().enumerate() {
            for (col, &intensity) in row_data.iter().enumerate() {
                let color = blend_color(BG_COLOR, FG_COLOR, intensity);
                self.put_pixel(x_base + col, y_base + row, color);
            }
        }

        self.cursor_x += 1;
        if self.cursor_x >= self.width / CHAR_WIDTH {
            self.cursor_x = 0;
            self.cursor_y += 1;
            let max_rows = self.height / CHAR_HEIGHT;
            if self.cursor_y >= max_rows {
                self.scroll_up();
            }
        }
    }
}

fn blend_color(bg: u32, fg: u32, alpha: u8) -> u32 {
    if alpha == 0 {
        return bg;
    }
    if alpha == 255 {
        return fg;
    }
    let a = alpha as u32;
    let r = ((fg >> 16 & 0xFF) * a + (bg >> 16 & 0xFF) * (255 - a)) / 255;
    let g = ((fg >> 8 & 0xFF) * a + (bg >> 8 & 0xFF) * (255 - a)) / 255;
    let b = ((fg & 0xFF) * a + (bg & 0xFF) * (255 - a)) / 255;
    (r << 16) | (g << 8) | b
}

impl core::fmt::Write for FramebufferWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for c in s.chars() {
            self.write_char(c);
        }
        Ok(())
    }
}

pub fn _print(args: core::fmt::Arguments) {
    use core::fmt::Write;
    WRITER.lock().as_mut().unwrap().write_fmt(args).unwrap();
}

#[macro_export]
macro_rules! println {
    ()            => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::display::_print(format_args!($($arg)*)));
}
