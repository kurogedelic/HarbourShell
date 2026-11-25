//! Four-tone (2-bit LCD) rendering of pictures, shared by Music covers and the Images app.
//!
//! The four tones are a ramp between the theme's ink (`foreground`) and surface
//! (`widget_face_active`): the end that is darker on screen is tone 0, the lighter end is
//! tone 3, and the two in between are 1/3 and 2/3 mixes. So the same picture reads
//! correctly on a dark theme (amber on black) and on a light one (ink on LCD green).
//!
//! A picture is reduced to 2-bit tone indices once (theme independent) and turned into
//! horizontal runs per tone; colours are applied only when drawing, so a theme change
//! needs no new decoding or quantising.

use crate::gfx::Color;
use crate::gfx::Rect;
use crate::gfx::Canvas;

use crate::launcher::apps::imageviewer::{luma, Picture};
use crate::theme::ThemeManager;

pub const TONES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dither {
    /// 4x4 ordered (Bayer) dither.
    Bayer,
    /// Floyd-Steinberg error diffusion.
    FloydSteinberg,
}

/// The dither used by default. Compared on a 128 x 128 cover: error diffusion keeps faces and
/// edges readable where the ordered dither turns them into texture.
pub const DEFAULT: Dither = Dither::FloydSteinberg;

fn lum(c: Color) -> u32 {
    luma(c.r, c.g, c.b) as u32
}

fn mix(a: Color, b: Color, num: u32, den: u32) -> Color {
    let m = |x: u8, y: u8| ((x as u32 * (den - num) + y as u32 * num + den / 2) / den) as u8;
    Color::RGB(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

/// Tones 0 (darkest) to 3 (lightest) for an ink and a surface colour.
pub fn ramp(ink: Color, surface: Color) -> [Color; TONES] {
    let (dark, light) = if lum(ink) <= lum(surface) { (ink, surface) } else { (surface, ink) };
    [dark, mix(dark, light, 1, 3), mix(dark, light, 2, 3), light]
}

pub fn theme_ramp(tm: &ThemeManager) -> [Color; TONES] {
    ramp(tm.get_color("foreground"), tm.get_color("widget_face_active"))
}

pub fn to_gray(p: &Picture) -> Vec<u8> {
    p.rgb.chunks_exact(3).map(|c| luma(c[0], c[1], c[2])).collect()
}

const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Tone index (0..=3) of one grey value at (x, y) with the ordered dither.
pub fn bayer_tone(grey: u8, x: usize, y: usize) -> u8 {
    let v = grey as f32 / 255.0 * 3.0;
    let base = v.floor().min(2.0);
    let frac = v - base;
    let threshold = (BAYER4[y % 4][x % 4] as f32 + 0.5) / 16.0;
    (base as u8 + (frac > threshold) as u8).min(3)
}

/// Reduce a grey image to tone indices.
pub fn quantize(gray: &[u8], w: usize, h: usize, mode: Dither) -> Vec<u8> {
    match mode {
        Dither::Bayer => (0..w * h).map(|i| bayer_tone(gray[i], i % w, i / w)).collect(),
        Dither::FloydSteinberg => {
            let mut buf: Vec<f32> = gray.iter().map(|&g| g as f32).collect();
            let mut out = vec![0u8; w * h];
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let old = buf[i].clamp(0.0, 255.0);
                    let t = (old / 85.0).round().clamp(0.0, 3.0);
                    out[i] = t as u8;
                    let err = old - t * 85.0;
                    let mut add = |dx: isize, dy: usize, k: f32| {
                        let nx = x as isize + dx;
                        if nx >= 0 && (nx as usize) < w && y + dy < h {
                            buf[(y + dy) * w + nx as usize] += err * k;
                        }
                    };
                    add(1, 0, 7.0 / 16.0);
                    add(-1, 1, 3.0 / 16.0);
                    add(0, 1, 5.0 / 16.0);
                    add(1, 1, 1.0 / 16.0);
                }
            }
            out
        }
    }
}

/// A picture ready to draw: horizontal runs per tone (tone 0 is the picture's background).
pub struct ToneRuns {
    pub bounds: Rect,
    pub runs: [Vec<Rect>; TONES],
}

pub fn build(p: &Picture, mode: Dither, ox: i32, oy: i32) -> ToneRuns {
    let (w, h) = (p.w as usize, p.h as usize);
    let idx = quantize(&to_gray(p), w, h, mode);
    let mut runs: [Vec<Rect>; TONES] = Default::default();
    for y in 0..h {
        let mut start = 0;
        for x in 1..=w {
            if x == w || idx[y * w + x] != idx[y * w + start] {
                runs[idx[y * w + start] as usize].push(Rect::new(ox + start as i32, oy + y as i32, (x - start) as u32, 1));
                start = x;
            }
        }
    }
    ToneRuns { bounds: Rect::new(ox, oy, p.w, p.h), runs }
}

pub fn draw(c: &mut Canvas, t: &ToneRuns, colors: &[Color; TONES]) {
    c.set_draw_color(colors[0]);
    let _ = c.fill_rect(t.bounds);
    for i in 1..TONES {
        if !t.runs[i].is_empty() {
            c.set_draw_color(colors[i]);
            let _ = c.fill_rects(&t.runs[i][..]);
        }
    }
}


// ---------------------------------------------------------------- 1 bit

/// Dither for the 1-bit cover panel.
pub const ART_DITHER: Dither = Dither::FloydSteinberg;

/// Stretch the grey range so the darkest 2 % map to 0 and the brightest 2 % to 255 (flat
/// covers still get contrast before they are reduced to two tones).
pub fn stretch(gray: &mut [u8]) {
    if gray.is_empty() {
        return;
    }
    let mut hist = [0usize; 256];
    for &g in gray.iter() {
        hist[g as usize] += 1;
    }
    let cut = gray.len() / 50;
    let (mut lo, mut acc) = (0usize, 0usize);
    while lo < 255 && acc + hist[lo] <= cut {
        acc += hist[lo];
        lo += 1;
    }
    let (mut hi, mut acc) = (255usize, 0usize);
    while hi > lo && acc + hist[hi] <= cut {
        acc += hist[hi];
        hi -= 1;
    }
    if hi <= lo + 8 {
        return; // already (nearly) flat: leave it
    }
    for g in gray.iter_mut() {
        *g = (((*g as i32 - lo as i32) * 255) / (hi - lo) as i32).clamp(0, 255) as u8;
    }
}

/// Reduce a grey image to two tones: 1 = light, 0 = dark.
pub fn quantize_1bit(gray: &[u8], w: usize, h: usize, mode: Dither) -> Vec<u8> {
    match mode {
        Dither::Bayer => (0..w * h)
            .map(|i| {
                let t = (BAYER4[(i / w) % 4][(i % w) % 4] as f32 + 0.5) / 16.0 * 255.0;
                (gray[i] as f32 > t) as u8
            })
            .collect(),
        Dither::FloydSteinberg => {
            let mut buf: Vec<f32> = gray.iter().map(|&g| g as f32).collect();
            let mut out = vec![0u8; w * h];
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let old = buf[i];
                    let bit = old >= 128.0;
                    out[i] = bit as u8;
                    let err = old - if bit { 255.0 } else { 0.0 };
                    let mut add = |dx: isize, dy: usize, k: f32| {
                        let nx = x as isize + dx;
                        if nx >= 0 && (nx as usize) < w && y + dy < h {
                            buf[(y + dy) * w + nx as usize] += err * k;
                        }
                    };
                    add(1, 0, 7.0 / 16.0);
                    add(-1, 1, 3.0 / 16.0);
                    add(0, 1, 5.0 / 16.0);
                    add(1, 1, 1.0 / 16.0);
                }
            }
            out
        }
    }
}

/// (light, dark) for the cover panel: the lightest and the darkest of the theme's ink and
/// white-ish colours, so a cover is dark on white on every theme (also the inverted ones).
pub fn paper_ink(tm: &ThemeManager) -> (Color, Color) {
    let c: Vec<Color> = ["foreground", "icon_white", "select_bg", "select_fg", "widget_face_active"].iter().map(|k| tm.get_color(k)).collect();
    let light = *c.iter().max_by_key(|x| lum(**x)).unwrap();
    let dark = *c.iter().min_by_key(|x| lum(**x)).unwrap();
    (light, dark)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn the_quantiser_maps_extremes_and_mid_greys() {
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(bayer_tone(0, x, y), 0);
                assert_eq!(bayer_tone(255, x, y), 3);
            }
        }
        // Exactly on a tone level: no dithering needed.
        assert_eq!(bayer_tone(85, 0, 0), 1);
        assert_eq!(bayer_tone(170, 1, 1), 2);
        // Between two tones both neighbours appear, in proportion.
        let mid: Vec<u8> = (0..16).map(|i| bayer_tone(128, i % 4, i / 4)).collect();
        assert!(mid.iter().all(|&t| t == 1 || t == 2), "{mid:?}");
        let twos = mid.iter().filter(|&&t| t == 2).count();
        assert!((6..=10).contains(&twos), "about half: {twos}");
        let flat = quantize(&[128; 64], 8, 8, Dither::FloydSteinberg);
        let avg = flat.iter().map(|&t| t as f32).sum::<f32>() / 64.0;
        assert!((avg - 128.0 / 85.0).abs() < 0.15, "error diffusion keeps the mean: {avg}");
        assert!(quantize(&[0; 16], 4, 4, Dither::FloydSteinberg).iter().all(|&t| t == 0));
        assert!(quantize(&[255; 16], 4, 4, Dither::FloydSteinberg).iter().all(|&t| t == 3));
    }

    #[test]
    fn the_ramp_runs_from_the_darker_to_the_lighter_colour() {
        let r = ramp(Color::RGB(255, 164, 46), Color::RGB(30, 16, 4)); // amber on black
        assert_eq!(r[0], Color::RGB(30, 16, 4));
        assert_eq!(r[3], Color::RGB(255, 164, 46));
        let r = ramp(Color::RGB(31, 42, 26), Color::RGB(199, 211, 168)); // ink on LCD
        assert_eq!(r[0], Color::RGB(31, 42, 26));
        assert_eq!(r[3], Color::RGB(199, 211, 168));
    }

    #[test]
    fn every_theme_has_four_clearly_different_increasing_tones() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/themes");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            if path.extension().map_or(true, |x| x != "yaml") {
                continue;
            }
            let theme = Theme::load_from_yaml(path.to_str().unwrap()).unwrap();
            let r = theme_ramp(&ThemeManager::new(theme));
            let l: Vec<u32> = r.iter().map(|c| lum(*c)).collect();
            for w in l.windows(2) {
                assert!(w[1] > w[0] + 8, "{}: tones {l:?} are not clearly increasing", path.display());
            }
            n += 1;
        }
        assert!(n >= 7, "looked at {n} themes");
    }

    #[test]
    fn runs_cover_every_pixel_exactly_once() {
        let rgb: Vec<u8> = (0..16 * 4).flat_map(|i| { let v = ((i % 16) * 16) as u8; [v, v, v] }).collect();
        let p = Picture { w: 16, h: 4, rgb };
        let t = build(&p, DEFAULT, 10, 20);
        let covered: u32 = t.runs.iter().flatten().map(|r| r.width() * r.height()).sum();
        assert_eq!(covered, 16 * 4);
        assert_eq!(t.bounds, Rect::new(10, 20, 16, 4));
        assert!(t.runs.iter().flatten().all(|r| r.x() >= 10 && r.right() <= 26 && r.y() >= 20 && r.bottom() <= 24));
    }

    #[test]
    fn one_bit_quantiser_and_stretch() {
        for mode in [Dither::Bayer, Dither::FloydSteinberg] {
            assert!(quantize_1bit(&[0; 64], 8, 8, mode).iter().all(|&b| b == 0), "{mode:?} black");
            assert!(quantize_1bit(&[255; 64], 8, 8, mode).iter().all(|&b| b == 1), "{mode:?} white");
            let mid = quantize_1bit(&[128; 64], 8, 8, mode);
            let lit = mid.iter().filter(|&&b| b == 1).count();
            assert!((24..=40).contains(&lit), "{mode:?}: mid grey is about half lit, got {lit}");
        }
        let mut flat_low: Vec<u8> = (0..100).map(|i| 60 + (i % 50) as u8).collect();
        stretch(&mut flat_low);
        assert!(*flat_low.iter().min().unwrap() <= 5 && *flat_low.iter().max().unwrap() >= 250, "range is stretched");
        let mut flat = vec![100u8; 50];
        stretch(&mut flat);
        assert!(flat.iter().all(|&g| g == 100), "a flat image is left alone");
    }

    #[test]
    fn the_cover_panel_is_dark_on_white_on_every_theme() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/themes");
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            if path.extension().map_or(true, |x| x != "yaml") {
                continue;
            }
            let tm = ThemeManager::new(Theme::load_from_yaml(path.to_str().unwrap()).unwrap());
            let (light, dark) = paper_ink(&tm);
            assert!(lum(light) > lum(dark) + 90, "{}: light {light:?} vs dark {dark:?}", path.display());
        }
    }
}
