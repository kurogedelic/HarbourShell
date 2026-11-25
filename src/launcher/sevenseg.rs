//! Seven-segment digits (a classic LCD / LED clock look), drawn with rectangles.

use crate::gfx::{Canvas, Color, Image, Rect};

use crate::assets::AssetStore;

/// Texture keys of the two digit atlases (cells are `1 2 3 4 5 6 7 8 9 0`).
pub const BIG_KEY: &str = "seg/big";
pub const SMALL_KEY: &str = "seg/small";

/// Cell index of a digit in the atlas: the sheets run 1..9 then 0.
pub fn atlas_cell(digit: usize) -> usize {
    (digit + 9) % 10
}

/// White-on-transparent RGBA from an atlas image whose lit pixels are dark and
/// opaque (the big sheet has a white background, the small one is transparent).
pub fn atlas_rgba(px: &[u8]) -> Vec<u8> {
    px.chunks(4)
        .flat_map(|p| {
            let lit = p[3] > 127 && (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3 < 128;
            if lit { [255, 255, 255, 255] } else { [0, 0, 0, 0] }
        })
        .collect()
}

pub fn insert_atlas(assets: &mut AssetStore, key: &str, path: &str) -> Result<(), String> {
    let (px, w, h) = crate::launcher::icons::load_rgba(std::path::Path::new(path))?;
    let rgba = atlas_rgba(&px);
    let img = Image::from_rgba(w as u32, h as u32, rgba).ok_or("atlas size mismatch")?;
    assets.insert(key.to_string(), img);
    Ok(())
}

/// Draw one atlas digit (`cw` x `ch` cell) at (x, y).
pub fn draw_atlas_digit(c: &mut Canvas, assets: &mut AssetStore, key: &str, cw: i32, ch: i32, digit: usize, x: i32, y: i32, color: Color) {
    let src = Rect::new(atlas_cell(digit) as i32 * cw, 0, cw as u32, ch as u32);
    let _ = assets.blit_tinted_src(c, key, src, Rect::new(x, y, cw as u32, ch as u32), color);
}

/// Segment bits: a (top), b (top right), c (bottom right), d (bottom),
/// e (bottom left), f (top left), g (middle).
const A: u8 = 1;
const B: u8 = 2;
const C: u8 = 4;
const D: u8 = 8;
const E: u8 = 16;
const F: u8 = 32;
const G: u8 = 64;

pub const SEGMENTS: [u8; 10] = [
    A | B | C | D | E | F,
    B | C,
    A | B | D | E | G,
    A | B | C | D | G,
    B | C | F | G,
    A | C | D | F | G,
    A | C | D | E | F | G,
    A | B | C,
    A | B | C | D | E | F | G,
    A | B | C | D | F | G,
];

/// Italic slant: how far right a row is shifted. The bottom row is not shifted,
/// the top row is shifted by about h/4 pixels (roughly a 14 degree slant).
pub fn shear(h: i32, row: i32) -> i32 {
    ((h - 1 - row).max(0)) / 4
}

/// Fill `r` row by row, each row shifted by the slant of the digit box that
/// starts at `top` and is `h` high.
fn fill_slanted(c: &mut Canvas, r: Rect, top: i32, h: i32) {
    for row in r.y()..r.y() + r.height() as i32 {
        let _ = c.fill_rect(Rect::new(r.x() + shear(h, row - top), row, r.width(), 1));
    }
}

/// One italic digit in a `w` x `h` box with segments `t` pixels thick.
/// The top of the digit leans `shear(h, 0)` pixels to the right of the box.
pub fn draw_digit(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, t: i32, digit: usize, color: Color) {
    c.set_draw_color(color);
    let mask = SEGMENTS[digit % 10];
    let mid = y + (h - t) / 2;
    let vh = (h - 3 * t) / 2; // height of one vertical segment
    let rects = [
        (A, Rect::new(x + t, y, (w - 2 * t) as u32, t as u32)),
        (G, Rect::new(x + t, mid, (w - 2 * t) as u32, t as u32)),
        (D, Rect::new(x + t, y + h - t, (w - 2 * t) as u32, t as u32)),
        (F, Rect::new(x, y + t, t as u32, vh as u32)),
        (B, Rect::new(x + w - t, y + t, t as u32, vh as u32)),
        (E, Rect::new(x, mid + t, t as u32, vh as u32)),
        (C, Rect::new(x + w - t, mid + t, t as u32, vh as u32)),
    ];
    for (bit, r) in rects {
        if mask & bit != 0 {
            fill_slanted(c, r, y, h);
        }
    }
}

/// Two upright (not slanted) dots, vertically placed for digits `h` high.
pub fn draw_colon_upright(c: &mut Canvas, x: i32, y: i32, h: i32, t: i32, color: Color) {
    c.set_draw_color(color);
    let _ = c.fill_rect(Rect::new(x, y + h / 3 - t / 2, t as u32, t as u32));
    let _ = c.fill_rect(Rect::new(x, y + 2 * h / 3 - t / 2, t as u32, t as u32));
}

/// Two italic dots, matching the slant of digits of height `h`.
pub fn draw_colon(c: &mut Canvas, x: i32, y: i32, h: i32, t: i32, color: Color) {
    c.set_draw_color(color);
    for cy in [y + h / 3 - t / 2, y + 2 * h / 3 - t / 2] {
        fill_slanted(c, Rect::new(x, cy, t as u32, t as u32), y, h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_light_the_right_number_of_segments() {
        let lit = |d: usize| SEGMENTS[d].count_ones();
        assert_eq!(lit(8), 7);
        assert_eq!(lit(1), 2);
        assert_eq!(lit(7), 3);
        assert_eq!(lit(0), 6);
        assert_eq!(lit(4), 4);
        // 0 and 8 differ only by the middle bar, 6 and 9 are mirror-like (5 + 1 = 6 segments).
        assert_eq!(SEGMENTS[8] & !SEGMENTS[0], G);
        assert_eq!(lit(6), 6);
        assert_eq!(lit(9), 6);
    }

    #[test]
    fn the_slant_grows_towards_the_top() {
        assert_eq!(shear(23, 22), 0, "the bottom row stays put");
        assert_eq!(shear(23, 0), 5, "the top row leans right by about h/4");
        assert!(shear(23, 5) >= shear(23, 15));
        for row in 0..23 {
            assert!(shear(23, row) >= shear(23, row + 1), "monotonic");
        }
        assert_eq!(shear(13, 0), 3);
    }

    #[test]
    fn the_atlas_runs_one_to_nine_then_zero() {
        assert_eq!(atlas_cell(1), 0);
        assert_eq!(atlas_cell(9), 8);
        assert_eq!(atlas_cell(0), 9);
    }

    #[test]
    fn atlas_pixels_become_white_on_transparent() {
        let px = [0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 0, 40, 40, 40, 255];
        let out = atlas_rgba(&px);
        assert_eq!(&out[0..4], &[255, 255, 255, 255], "black is lit");
        assert_eq!(&out[4..8], &[0, 0, 0, 0], "white background is dropped");
        assert_eq!(&out[8..12], &[0, 0, 0, 0], "transparent is dropped");
        assert_eq!(&out[12..16], &[255, 255, 255, 255], "dark grey is lit");
    }

    #[test]
    fn all_ten_digits_are_distinct() {
        for i in 0..10 {
            for j in (i + 1)..10 {
                assert_ne!(SEGMENTS[i], SEGMENTS[j], "{i} vs {j}");
            }
        }
    }
}
