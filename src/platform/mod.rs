//! Platform layer: keyboard input and LCD output for the Cardputer Zero.

pub mod keys;

use std::sync::atomic::{AtomicBool, AtomicI32};

/// State shared between the main loop and the keyboard thread.
#[derive(Default)]
pub struct Shared {
    /// Process group of the running external app, or 0.
    pub child_pgid: AtomicI32,
    /// While true, key events are not forwarded to the launcher.
    pub suspended: AtomicBool,
    pub quit: AtomicBool,
}

#[cfg(target_os = "linux")]
pub mod evdev;
pub mod mouse;
pub mod gamepad;
pub mod display;
#[cfg(target_os = "linux")]
pub mod drm;
#[cfg(target_os = "linux")]
pub mod fb;

/// Convert little-endian RGB565 bytes (as returned by SDL for
/// `PixelFormatEnum::RGB565`) — kept as a named step so tests can cover it.
pub fn rgb565_len(width: usize, height: usize) -> usize {
    width * height * 2
}
