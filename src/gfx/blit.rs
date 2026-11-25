//! Copying a source image onto a surface: nearest-neighbour scaling, a colour tint
//! (SDL's colour mod) and 0/255 alpha. The scaling arithmetic follows SDL2's
//! software blitter (`SDL_BlitScaled`, fixed-point 16.16 stepping) in integers.

use super::color::Color;
use super::geom::Rect;
use super::surface::{Source, Surface};

/// Tint one source pixel and store it. Alpha 0 leaves the destination alone.
fn put_tinted(dst: &mut Surface, x: i32, y: i32, c: Color, tint: Color) {
    if c.a == 0 {
        return;
    }
    let r = (c.r as u32 * tint.r as u32 / 255) as u8;
    let g = (c.g as u32 * tint.g as u32 / 255) as u8;
    let b = (c.b as u32 * tint.b as u32 / 255) as u8;
    dst.put(x, y, Color::RGB(r, g, b).to_rgb565());
}

/// Copy `src_rect` of `src` to `dst_rect` of `dst`, drawing only inside `clip`.
/// `src_rect` must lie inside the source; `dst_rect` and `clip` are in surface pixels.
pub fn blit<S: Source + ?Sized>(dst: &mut Surface, clip: Rect, src: &S, src_rect: Rect, dst_rect: Rect, tint: Color) {
    if src_rect.width() == dst_rect.width() && src_rect.height() == dst_rect.height() {
        blit_unscaled(dst, clip, src, src_rect, dst_rect, tint);
    } else {
        blit_scaled(dst, clip, src, src_rect, dst_rect, tint);
    }
}

fn blit_unscaled<S: Source + ?Sized>(dst: &mut Surface, clip: Rect, src: &S, src_rect: Rect, dst_rect: Rect, tint: Color) {
    let Some(vis) = dst_rect.intersection(clip) else { return };
    let (ox, oy) = (src_rect.x() - dst_rect.x(), src_rect.y() - dst_rect.y());
    for y in vis.top()..vis.bottom() {
        for x in vis.left()..vis.right() {
            put_tinted(dst, x, y, src.rgba(x + ox, y + oy), tint);
        }
    }
}

/// Nearest-neighbour stepping in 16.16 fixed point, sampling at the pixel centre:
/// the source index (0-based, relative to the source start) for destination pixel `i`.
fn step(src_len: i64, dst_len: i64, i: i64) -> i64 {
    let inc = (src_len << 16) / dst_len;
    (inc / 2 + i * inc) >> 16
}

/// SDL treats a scaled copy in two ways, and so do we.
///
/// * The destination reaches outside the surface: the whole picture is stretched
///   as if nothing were cut, and then only the visible part is kept.
/// * The destination is inside the surface and only the clip rectangle (or the
///   viewport) cuts it: the cut is also made in the source, rounded to whole pixels
///   (start rounded half up, length rounded half up but kept inside the image), and
///   only that piece is stretched over what is visible.
///
/// The second way rounds in `double`s in SDL; here it is exact. They differ only when a
/// value falls exactly on .5 and the scale (source size / destination size, reduced)
/// is not a power of two, where SDL's rounding error decides.
fn blit_scaled<S: Source + ?Sized>(dst: &mut Surface, clip: Rect, src: &S, src_rect: Rect, dst_rect: Rect, tint: Color) {
    let (sw, sh) = (src_rect.width() as i64, src_rect.height() as i64);
    let (dw, dh) = (dst_rect.width() as i64, dst_rect.height() as i64);
    let Some(vis) = dst_rect.intersection(clip) else { return };
    let bounds = Rect::new(0, 0, dst.width(), dst.height());
    let (sx0, sy0) = (src_rect.x() as i64, src_rect.y() as i64);
    let (vx, vy) = (vis.x() as i64 - dst_rect.x() as i64, vis.y() as i64 - dst_rect.y() as i64);
    let (vw, vh) = (vis.width() as i64, vis.height() as i64);

    if !bounds.contains_rect(dst_rect) {
        // Stretch everything, keep the visible part.
        for j in 0..vh {
            let sy = sy0 + step(sh, dh, vy + j);
            for i in 0..vw {
                let sx = sx0 + step(sw, dw, vx + i);
                put_tinted(dst, vis.x() + i as i32, vis.y() + j as i32, src.rgba(sx as i32, sy as i32), tint);
            }
        }
        return;
    }

    // Cut the source by the same proportion as the destination was cut.
    // (The piece may reach past `src_rect` but never past the image itself.)
    let cut = |cut_start: i64, visible: i64, s: i64, d: i64, origin: i64, image_len: i64| {
        let start = (2 * cut_start * s + d).div_euclid(2 * d);
        let len = ((2 * s * visible + d).div_euclid(2 * d)).min(image_len - origin - start);
        (start, len)
    };
    let (fx, fw) = cut(vx, vw, sw, dw, sx0, src.width() as i64);
    let (fy, fh) = cut(vy, vh, sh, dh, sy0, src.height() as i64);
    if fw <= 0 || fh <= 0 {
        return;
    }
    for j in 0..vh {
        let sy = sy0 + fy + step(fh, vh, j);
        for i in 0..vw {
            let sx = sx0 + fx + step(fw, vw, i);
            put_tinted(dst, vis.x() + i as i32, vis.y() + j as i32, src.rgba(sx as i32, sy as i32), tint);
        }
    }
}
