//! `Canvas`: the drawing calls, named and shaped like `sdl2::render::Canvas`.
//!
//! A canvas owns a `Surface`. Coordinates are relative to the viewport; the clip
//! rectangle is relative to the viewport too. `clear` ignores both.
//!
//! `set_scale(k)` makes every logical pixel a k x k block (whole-number scale only,
//! as `SDL_RenderSetScale` with a whole number does): coordinates, sizes, the viewport
//! and the clip rectangle you set afterwards are in logical units. The viewport and
//! the clip are kept in surface pixels from the moment they are set.

use super::alloc::string::String;
use super::alloc::vec::Vec;

use super::blit;
use super::color::Color;
use super::geom::{ClipRect, Point, Rect};
use super::raster;
use super::surface::{PixelFormat, Source, Surface};

pub struct Canvas {
    surface: Surface,
    color: Color,
    /// In surface pixels.
    viewport: Rect,
    /// In surface pixels, relative to the viewport.
    clip: ClipRect,
    scale: i32,
}

impl Canvas {
    pub fn new(surface: Surface) -> Canvas {
        let viewport = Rect::new(0, 0, surface.width(), surface.height());
        Canvas { surface, color: Color::RGB(0, 0, 0), viewport, clip: ClipRect::None, scale: 1 }
    }

    pub fn surface(&self) -> &Surface {
        &self.surface
    }
    pub fn surface_mut(&mut self) -> &mut Surface {
        &mut self.surface
    }
    pub fn into_surface(self) -> Surface {
        self.surface
    }

    // ----- state -----

    pub fn set_draw_color<C: Into<Color>>(&mut self, color: C) {
        self.color = color.into();
    }
    pub fn draw_color(&self) -> Color {
        self.color
    }

    /// Make each logical pixel a `k` x `k` block from now on (1 is the normal size).
    /// The viewport and clip rectangle that are already set stay where they are.
    pub fn set_scale(&mut self, k: u32) {
        self.scale = k.max(1) as i32;
    }
    pub fn scale(&self) -> u32 {
        self.scale as u32
    }

    fn up(&self, r: Rect) -> Rect {
        let k = self.scale;
        Rect::new(r.x() * k, r.y() * k, r.width() * k as u32, r.height() * k as u32)
    }
    fn down(&self, r: Rect) -> Rect {
        let k = self.scale;
        Rect::new(r.x().div_euclid(k), r.y().div_euclid(k), (r.width() as i32 + k - 1) as u32 / k as u32, (r.height() as i32 + k - 1) as u32 / k as u32)
    }

    /// Set the drawing origin and area (logical units); `None` is the whole surface.
    pub fn set_viewport<R: Into<Option<Rect>>>(&mut self, rect: R) {
        self.viewport = match rect.into() {
            Some(r) => self.up(r),
            None => Rect::new(0, 0, self.surface.width(), self.surface.height()),
        };
    }
    pub fn viewport(&self) -> Rect {
        self.down(self.viewport)
    }

    /// Set the clip rectangle (logical units, relative to the viewport).
    pub fn set_clip_rect<R: Into<ClipRect>>(&mut self, clip: R) {
        self.clip = match clip.into() {
            ClipRect::Some(r) => ClipRect::Some(self.up(r)),
            other => other,
        };
    }
    pub fn clip_rect(&self) -> ClipRect {
        match self.clip {
            ClipRect::Some(r) => ClipRect::Some(self.down(r)),
            other => other,
        }
    }
    pub fn is_clip_enabled(&self) -> bool {
        self.clip != ClipRect::None
    }

    /// Where drawing may land, in surface pixels. None means nothing can be drawn.
    fn target_clip(&self) -> Option<Rect> {
        let bounds = Rect::new(0, 0, self.surface.width(), self.surface.height());
        let area = match self.clip {
            ClipRect::None => self.viewport,
            ClipRect::Zero => return None,
            ClipRect::Some(c) => {
                let mut c = c;
                c.offset(self.viewport.x(), self.viewport.y());
                c.intersection(self.viewport)?
            }
        };
        area.intersection(bounds)
    }

    /// A logical rectangle in surface pixels.
    fn to_surface_rect(&self, r: Rect) -> Rect {
        let mut r = self.up(r);
        r.offset(self.viewport.x(), self.viewport.y());
        r
    }

    /// Draw one logical pixel (a block when scaled), inside `clip`.
    fn plot(&mut self, clip: Rect, x: i32, y: i32, v: u16) {
        if self.scale == 1 {
            let (sx, sy) = (x + self.viewport.x(), y + self.viewport.y());
            if clip.contains_point((sx, sy)) {
                self.surface.put(sx, sy, v);
            }
            return;
        }
        let block = self.to_surface_rect(Rect::new(x, y, 1, 1));
        if let Some(vis) = block.intersection(clip) {
            for yy in vis.top()..vis.bottom() {
                for xx in vis.left()..vis.right() {
                    self.surface.put(xx, yy, v);
                }
            }
        }
    }

    // ----- drawing -----

    /// Fill the whole surface with the draw colour (ignores viewport and clip).
    pub fn clear(&mut self) {
        let v = self.color.to_rgb565();
        let (w, h) = (self.surface.width() as usize, self.surface.height() as usize);
        let stride = self.surface.stride();
        let px = self.surface.pixels_mut();
        for y in 0..h {
            px[y * stride..y * stride + w].fill(v);
        }
    }

    pub fn fill_rect<R: Into<Option<Rect>>>(&mut self, rect: R) -> Result<(), String> {
        let Some(clip) = self.target_clip() else { return Ok(()) };
        let r = match rect.into() {
            Some(r) => self.to_surface_rect(r),
            None => self.viewport,
        };
        if let Some(vis) = r.intersection(clip) {
            let v = self.color.to_rgb565();
            for y in vis.top()..vis.bottom() {
                for x in vis.left()..vis.right() {
                    self.surface.put(x, y, v);
                }
            }
        }
        Ok(())
    }

    pub fn fill_rects(&mut self, rects: &[Rect]) -> Result<(), String> {
        for r in rects {
            self.fill_rect(*r)?;
        }
        Ok(())
    }

    /// One-pixel outline of `rect` (inside it); `None` is the whole viewport.
    pub fn draw_rect(&mut self, rect: Rect) -> Result<(), String> {
        let (x0, y0) = (rect.x(), rect.y());
        let (x1, y1) = (rect.right() - 1, rect.bottom() - 1);
        let pts = [Point::new(x0, y0), Point::new(x1, y0), Point::new(x1, y1), Point::new(x0, y1), Point::new(x0, y0)];
        self.draw_lines(&pts[..])
    }

    pub fn draw_rects(&mut self, rects: &[Rect]) -> Result<(), String> {
        for r in rects {
            self.draw_rect(*r)?;
        }
        Ok(())
    }

    pub fn draw_point<P: Into<Point>>(&mut self, p: P) -> Result<(), String> {
        let p = p.into();
        if let Some(clip) = self.target_clip() {
            let v = self.color.to_rgb565();
            self.plot(clip, p.x(), p.y(), v);
        }
        Ok(())
    }

    pub fn draw_points(&mut self, points: &[Point]) -> Result<(), String> {
        for p in points {
            self.draw_point(*p)?;
        }
        Ok(())
    }

    pub fn draw_line<P: Into<Point>, Q: Into<Point>>(&mut self, start: P, end: Q) -> Result<(), String> {
        let pts = [start.into(), end.into()];
        self.draw_lines(&pts[..])
    }

    /// A connected path, drawn exactly like SDL's software `SDL_DrawLines`.
    pub fn draw_lines(&mut self, points: &[Point]) -> Result<(), String> {
        let Some(clip) = self.target_clip() else { return Ok(()) };
        let v = self.color.to_rgb565();
        if self.scale == 1 {
            let (ox, oy) = (self.viewport.x(), self.viewport.y());
            let moved: Vec<Point> = points.iter().map(|p| Point::new(p.x() + ox, p.y() + oy)).collect();
            let surface = &mut self.surface;
            raster::polyline(clip, &moved, |x, y| surface.put(x, y, v));
        } else {
            // Scaled: the whole line is walked in logical pixels (no clipping of the line
            // itself) and each pixel becomes a block that the clip then cuts.
            let mut walked: Vec<(i32, i32)> = Vec::new();
            // The line is clipped in logical units first (SDL's scaled path does, against the
            // size of the viewport in surface pixels; compared with SDL for lines that are not
            // cut at an edge, which is all the shell draws).
            let k = self.scale;
            let lc = Rect::new(0, 0, self.viewport.width(), self.viewport.height());
            raster::polyline(lc, points, |x, y| walked.push((x, y)));
            for (x, y) in walked {
                self.plot(clip, x, y, v);
            }
        }
        Ok(())
    }

    // ----- images -----

    /// Copy part of `src` to the canvas. `src_rect` None is all of `src`; `dst_rect`
    /// None is the whole viewport. The image is scaled (nearest neighbour) to fit
    /// `dst_rect` and each pixel is multiplied by `tint` per channel (white = as is).
    /// Source pixels with alpha 0 are skipped.
    pub fn blit<S, A, B>(&mut self, src: &S, src_rect: A, dst_rect: B, tint: Color) -> Result<(), String>
    where
        S: Source + ?Sized,
        A: Into<Option<Rect>>,
        B: Into<Option<Rect>>,
    {
        let whole = Rect::new(0, 0, src.width(), src.height());
        let src_rect = match src_rect.into() {
            Some(r) => match r.intersection(whole) {
                Some(r) => r,
                None => return Ok(()),
            },
            None => whole,
        };
        let dst_rect = match dst_rect.into() {
            Some(r) => self.to_surface_rect(r),
            None => self.viewport,
        };
        let Some(clip) = self.target_clip() else { return Ok(()) };
        blit::blit(&mut self.surface, clip, src, src_rect, dst_rect, tint);
        Ok(())
    }

    /// Draw a 1-bit bitmap in the draw colour: set bits are drawn, clear bits are not.
    /// Rows are `stride` bytes apart and the leftmost pixel is the top bit of each
    /// byte (the layout `font.rs` uses). `bits` must hold `stride * h` bytes.
    pub fn draw_bitmap(&mut self, x: i32, y: i32, w: u32, h: u32, bits: &[u8], stride: usize) -> Result<(), String> {
        for row in 0..h as usize {
            for col in 0..w as usize {
                if bits[row * stride + col / 8] & (0x80 >> (col % 8)) != 0 {
                    self.draw_point((x + col as i32, y + row as i32))?;
                }
            }
        }
        Ok(())
    }

    /// Draw a glyph of up to 16 columns, one `u16` per row with the leftmost pixel in
    /// the top bit (the layout of `UnicodeFont`). Set bits are drawn in the draw colour.
    pub fn draw_glyph(&mut self, x: i32, y: i32, w: u32, rows: &[u16]) -> Result<(), String> {
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..w.min(16) {
                if bits & (0x8000 >> col) != 0 {
                    self.draw_point((x + col as i32, y + row as i32))?;
                }
            }
        }
        Ok(())
    }

    /// Fill `rect` with the draw colour wherever `lit(x, y)` is true (x, y are the
    /// viewport coordinates, so patterns line up across neighbouring fills). The
    /// patterns in `crate::dither::Pattern::lit` fit this.
    pub fn dither_fill(&mut self, rect: Rect, lit: impl Fn(i32, i32) -> bool) -> Result<(), String> {
        for y in rect.top()..rect.bottom() {
            for x in rect.left()..rect.right() {
                if lit(x, y) {
                    self.draw_point((x, y))?;
                }
            }
        }
        Ok(())
    }

    // ----- reading back -----

    /// Copy pixels out. `rect` is in surface pixels (not relative to the viewport, which
    /// is what SDL does); None is the viewport. The area must lie inside the surface.
    /// Rows are tightly packed in `format` (little-endian words).
    pub fn read_pixels<R: Into<Option<Rect>>>(&self, rect: R, format: PixelFormat) -> Result<Vec<u8>, String> {
        let r = rect.into().unwrap_or(self.viewport);
        let bounds = Rect::new(0, 0, self.surface.width(), self.surface.height());
        if !bounds.contains_rect(r) {
            return Err(String::from("read_pixels: rectangle is outside the surface"));
        }
        let mut out = Vec::with_capacity(r.width() as usize * r.height() as usize * format.bytes_per_pixel());
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                let v = self.surface.get(x, y).unwrap_or(0);
                let c = Color::from_rgb565(v);
                match format {
                    PixelFormat::Rgb565 => out.extend_from_slice(&v.to_le_bytes()),
                    PixelFormat::Rgb24 => out.extend_from_slice(&[c.r, c.g, c.b]),
                    PixelFormat::Argb8888 => {
                        // SDL's 565 -> ARGB8888 table widens green as g * 4 + g / 24
                        // (it never reaches 255), unlike the RGB24 path.
                        let g6 = (v >> 5) & 63;
                        out.extend_from_slice(&[c.b, (g6 * 4 + g6 / 24) as u8, c.r, 255])
                    }
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_pixels_none_is_the_viewport_and_a_rect_is_in_surface_pixels() {
        let mut c = Canvas::new(Surface::new(10, 10));
        c.set_draw_color(Color::RGB(255, 0, 0));
        c.set_viewport(Rect::new(4, 4, 3, 2));
        c.fill_rect(None).unwrap();
        assert_eq!(c.read_pixels(None, PixelFormat::Rgb24).unwrap().len(), 3 * 2 * 3);
        assert_eq!(c.read_pixels(None, PixelFormat::Rgb24).unwrap()[..3], [255, 0, 0]);
        assert_eq!(c.read_pixels(Rect::new(0, 0, 1, 1), PixelFormat::Rgb24).unwrap(), [0, 0, 0]);
        assert!(c.read_pixels(Rect::new(8, 8, 5, 5), PixelFormat::Rgb24).is_err());
    }

    #[test]
    fn clear_ignores_clip_and_viewport() {
        let mut c = Canvas::new(Surface::new(4, 4));
        c.set_viewport(Rect::new(1, 1, 1, 1));
        c.set_clip_rect(ClipRect::Zero);
        c.set_draw_color(Color::RGB(255, 255, 255));
        c.clear();
        assert!(c.surface().pixels().iter().all(|&p| p == 0xFFFF));
    }
}
