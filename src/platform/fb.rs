//! Linux framebuffer output for the Cardputer Zero LCD (`/dev/fb0`, RGB565).
//!
//! SDL2 on the device cannot reach the LCD (no fbcon driver, and KMSDRM/Wayland
//! crash without a GPU), so Harbour renders with SDL's software renderer into
//! an off-screen window (SDL "dummy" video driver) and copies the pixels here.
//! Only the rows that changed since the previous frame are written.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;

pub struct Framebuffer {
    file: File,
    path: String,
    pub width: usize,
    pub height: usize,
    /// Bytes per scanline in the device buffer.
    pub stride: usize,
    last: Vec<u8>,
    valid: bool,
}

impl Framebuffer {
    /// Open `path` (e.g. `/dev/fb0`). Geometry comes from sysfs.
    pub fn open(path: &str) -> io::Result<Self> {
        let name = path.rsplit('/').next().unwrap_or("fb0");
        let sys = |f: &str| std::fs::read_to_string(format!("/sys/class/graphics/{name}/{f}"));
        let (width, height) = sys("virtual_size")
            .ok()
            .and_then(|s| {
                let mut it = s.trim().split(',');
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            })
            .unwrap_or((320usize, 170usize));
        let bpp: usize = sys("bits_per_pixel")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(16);
        if bpp != 16 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{path}: {bpp} bpp is not supported (RGB565 only)"),
            ));
        }
        let stride: usize = sys("stride")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(width * 2);
        let file = OpenOptions::new().write(true).open(path)?;
        Ok(Self {
            file,
            path: path.to_string(),
            width,
            height,
            stride,
            last: Vec::new(),
            valid: false,
        })
    }

    /// Forget the previous frame so the next `present` rewrites every row
    /// (use after an external app has drawn over the LCD).
    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    /// True when something other than us has drawn on the LCD since the last
    /// `present` (a few sample rows of the device buffer differ from our copy).
    /// Used to see when a launched app has taken over the screen.
    pub fn taken_over(&self) -> bool {
        if !self.valid {
            return false;
        }
        let Ok(f) = File::open(&self.path) else { return false };
        let row = self.width * 2;
        let mut buf = vec![0u8; row];
        let step = (self.height / 10).max(1);
        let mut y = 1;
        while y < self.height {
            if f.read_exact_at(&mut buf, (y * self.stride) as u64).is_ok() && buf[..] != self.last[y * row..(y + 1) * row] {
                return true;
            }
            y += step;
        }
        false
    }

    /// Write a tightly packed `width*2` bytes-per-row RGB565 image.
    /// Returns the number of bytes sent to the device.
    pub fn present(&mut self, rgb565: &[u8]) -> io::Result<usize> {
        let row = self.width * 2;
        if rgb565.len() < row * self.height {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too small"));
        }
        let mut sent = 0;
        let mut y = 0;
        while y < self.height {
            if self.valid && self.last[y * row..(y + 1) * row] == rgb565[y * row..(y + 1) * row] {
                y += 1;
                continue;
            }
            // Extend over consecutive changed rows and write them in one call.
            let start = y;
            while y < self.height
                && !(self.valid
                    && self.last[y * row..(y + 1) * row] == rgb565[y * row..(y + 1) * row])
            {
                y += 1;
            }
            sent += self.write_rows(start, y, rgb565, row)?;
        }
        self.last.clear();
        self.last.extend_from_slice(&rgb565[..row * self.height]);
        self.valid = true;
        Ok(sent)
    }

    fn write_rows(&self, y0: usize, y1: usize, src: &[u8], row: usize) -> io::Result<usize> {
        if self.stride == row {
            self.file
                .write_all_at(&src[y0 * row..y1 * row], (y0 * self.stride) as u64)?;
        } else {
            for y in y0..y1 {
                self.file
                    .write_all_at(&src[y * row..(y + 1) * row], (y * self.stride) as u64)?;
            }
        }
        Ok((y1 - y0) * row)
    }
}

impl crate::platform::display::Display for Framebuffer {
    fn present(&mut self, rgb565: &[u8]) -> io::Result<usize> {
        Framebuffer::present(self, rgb565)
    }
    fn invalidate(&mut self) {
        Framebuffer::invalidate(self)
    }
    fn taken_over(&self) -> bool {
        Framebuffer::taken_over(self)
    }
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }
    fn backend(&self) -> &'static str {
        "fbdev"
    }
}
