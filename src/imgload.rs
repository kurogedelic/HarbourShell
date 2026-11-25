//! Decoding PNG / JPG files into plain pixels. This is the one place that still
//! calls SDL_image; everything else gets bytes or a `gfx::Image` and never sees SDL.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

use sdl2::image::{ImageRWops, InitFlag, LoadSurface};
use sdl2::pixels::PixelFormatEnum;
use sdl2::rwops::RWops;
use sdl2::surface::Surface;

use crate::gfx::Image;

static JPG: AtomicBool = AtomicBool::new(false);

/// Bring up SDL_image once (PNG, and JPG when the library has it). Safe to call often.
fn ensure_init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let both = sdl2::image::init(InitFlag::PNG | InitFlag::JPG);
        JPG.store(both.is_ok(), Ordering::SeqCst);
        // Never shut it down: images are decoded for as long as the program runs.
        match both.or_else(|_| sdl2::image::init(InitFlag::PNG)) {
            Ok(c) => std::mem::forget(c),
            Err(_) => {}
        }
    });
}

/// Whether JPEG files can be decoded.
pub fn jpg_available() -> bool {
    ensure_init();
    JPG.load(Ordering::SeqCst)
}

fn pack(surface: Surface, format: PixelFormatEnum, bpp: usize) -> Result<(Vec<u8>, usize, usize), String> {
    let surface = surface.convert_format(format)?;
    let (w, h, pitch) = (surface.width() as usize, surface.height() as usize, surface.pitch() as usize);
    let mut out = Vec::with_capacity(w * h * bpp);
    surface.with_lock(|px| {
        for y in 0..h {
            out.extend_from_slice(&px[y * pitch..y * pitch + w * bpp]);
        }
    });
    Ok((out, w, h))
}

/// A file as tightly packed RGBA bytes: (bytes, width, height).
pub fn load_rgba(path: &Path) -> Result<(Vec<u8>, usize, usize), String> {
    ensure_init();
    pack(Surface::from_file(path)?, PixelFormatEnum::RGBA32, 4)
}

/// Encoded bytes (PNG / JPG) as tightly packed RGBA bytes.
pub fn decode_rgba(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    ensure_init();
    pack(RWops::from_bytes(bytes)?.load()?, PixelFormatEnum::RGBA32, 4)
}

/// A file as tightly packed RGB bytes (alpha dropped).
pub fn load_rgb24(path: &Path) -> Result<(Vec<u8>, usize, usize), String> {
    ensure_init();
    pack(Surface::from_file(path)?, PixelFormatEnum::RGB24, 3)
}

/// Encoded bytes (PNG / JPG) as tightly packed RGB bytes.
pub fn decode_rgb24(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    ensure_init();
    pack(RWops::from_bytes(bytes)?.load()?, PixelFormatEnum::RGB24, 3)
}

/// A file as a `gfx::Image`.
pub fn load_image(path: &Path) -> Result<Image, String> {
    let (px, w, h) = load_rgba(path)?;
    Image::from_rgba(w as u32, h as u32, px).ok_or_else(|| "bad image size".to_string())
}
