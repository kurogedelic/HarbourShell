//! Harbour's own drawing layer: Color / Point / Rect / Surface / Canvas with an
//! integer software rasterizer that draws into a memory buffer.
//!
//! The names and edge cases follow `sdl2` (`sdl2::rect`, `sdl2::pixels::Color`,
//! `sdl2::render::Canvas`) so apps can switch by changing `use` lines. The tests in
//! `tests/gfx_vs_sdl.rs` compare every pixel with SDL2's software renderer.
//!
//! Rules for this module (so it can be built `no_std` and turned into C later):
//! integer math only, `core` and `alloc` only, no `std::` paths outside tests.

extern crate alloc;

mod blit;
mod canvas;
mod color;
mod geom;
mod raster;
mod surface;

pub use canvas::Canvas;
pub use color::Color;
pub use geom::{ClipRect, Point, Rect};
pub use surface::{Image, PixelFormat, Source, Surface};
