//! Pixel memory: `Surface` (what a `Canvas` draws into) and `Image` (an RGBA source
//! for blits, like an SDL texture).

use super::color::Color;
use super::alloc::borrow::Cow;
use super::alloc::vec;
use super::alloc::vec::Vec;

/// Pixel layouts. A `Surface` is stored as `Rgb565` today; the other two are only
/// used as output formats of `Canvas::read_pixels`. A 1-bit format would be another
/// variant with its own storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// 16 bits per pixel, little-endian when read out as bytes.
    Rgb565,
    /// 3 bytes per pixel: R, G, B.
    Rgb24,
    /// 32 bits per pixel, little-endian: B, G, R, A (alpha is always 255).
    Argb8888,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Rgb565 => 2,
            PixelFormat::Rgb24 => 3,
            PixelFormat::Argb8888 => 4,
        }
    }
}

/// Anything a `Canvas` can blit from.
pub trait Source {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    /// The colour at (x, y); the caller keeps x, y inside the source.
    fn rgba(&self, x: i32, y: i32) -> Color;
}

/// A memory buffer of RGB565 pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    width: u32,
    height: u32,
    /// Pixels per row (may be larger than `width`).
    stride: usize,
    pixels: Vec<u16>,
}

impl Surface {
    /// A black surface.
    pub fn new(width: u32, height: u32) -> Surface {
        let stride = width as usize;
        Surface { width, height, stride, pixels: vec![0; stride * height as usize] }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    /// Pixels per row.
    pub fn stride(&self) -> usize {
        self.stride
    }
    pub fn format(&self) -> PixelFormat {
        PixelFormat::Rgb565
    }
    /// All pixels, row by row (`stride` per row).
    pub fn pixels(&self) -> &[u16] {
        &self.pixels
    }
    pub fn pixels_mut(&mut self) -> &mut [u16] {
        &mut self.pixels
    }
    /// The picture as RGB565 bytes, little-endian, rows tightly packed (what a frame
    /// buffer wants). Borrowed as is on a little-endian machine with no row padding.
    pub fn bytes_le(&self) -> Cow<'_, [u8]> {
        #[cfg(target_endian = "little")]
        if self.stride == self.width as usize {
            // SAFETY: a u16 slice is a valid u8 slice of twice the length (alignment only
            // gets looser) and every bit pattern is a valid u8.
            let b = unsafe { core::slice::from_raw_parts(self.pixels.as_ptr() as *const u8, self.pixels.len() * 2) };
            return Cow::Borrowed(b);
        }
        let mut out = Vec::with_capacity(self.width as usize * self.height as usize * 2);
        for y in 0..self.height {
            for v in self.row(y) {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Cow::Owned(out)
    }
    /// One row.
    pub fn row(&self, y: u32) -> &[u16] {
        let s = y as usize * self.stride;
        &self.pixels[s..s + self.width as usize]
    }
    /// The raw RGB565 value at (x, y); None outside.
    pub fn get(&self, x: i32, y: i32) -> Option<u16> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        Some(self.pixels[y as usize * self.stride + x as usize])
    }
    /// Set a pixel; the caller keeps x, y inside.
    pub(crate) fn put(&mut self, x: i32, y: i32, v: u16) {
        self.pixels[y as usize * self.stride + x as usize] = v;
    }
}

impl Source for Surface {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn rgba(&self, x: i32, y: i32) -> Color {
        Color::from_rgb565(self.pixels[y as usize * self.stride + x as usize])
    }
}

/// An RGBA image (4 bytes per pixel: R, G, B, A). Alpha is treated as 0 (skip) or
/// 255 (copy); that is all the shell uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Image {
    /// From tightly packed RGBA bytes. Returns None if the length is wrong.
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<Image> {
        if rgba.len() != width as usize * height as usize * 4 {
            return None;
        }
        Some(Image { width, height, rgba })
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    /// The RGBA bytes, tightly packed.
    pub fn rgba_bytes(&self) -> &[u8] {
        &self.rgba
    }
}

impl Source for Image {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn rgba(&self, x: i32, y: i32) -> Color {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        Color::RGBA(self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3])
    }
}
