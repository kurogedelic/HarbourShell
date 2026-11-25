//! Icons for external apps.
//!
//! Official APPLaunch icons are full-colour 80–100 px PNGs. Harbour's desktop
//! uses 32 px one-colour icons that are tinted with the theme ink, so each
//! external icon is converted to a 24x24 one-bit glyph inside a classic
//! rounded 32x32 tile (outlined normally, filled with a cut-out glyph when
//! selected). Known apps wear the hand-drawn icons (see [`known_icon_for`]).

use std::path::Path;


use crate::assets::AssetStore;

pub const GLYPH: usize = 24;
pub const TILE: usize = 32;

/// The hand-drawn icon an installed (external) app wears, as an asset key: the icon of the built-in
/// package with that name or alias (`apps/<id>/icon.png`, see `launcher::packages`), else one of the
/// shared icons drawn for official apps Harbour has no app of its own for (`assets/images/icons`:
/// Mail, Dictionary, Notes). Other official apps (Fune...) keep their own `Icon=`.
/// None: the app's own picture is converted (`app_tile_rgba`).
pub fn known_icon_for(name: &str) -> Option<String> {
    if let Some(p) = crate::launcher::packages::icon_for_name(name) {
        return p.icon.is_some().then(|| crate::launcher::packages::icon_key(&p.id));
    }
    let n = crate::launcher::packages::normalize(name);
    Some(match n.as_str() {
        "mail" | "inbox" => "icons/Mail",
        "dictionary" => "icons/Dictionary",
        "notes" => "icons/Notes",
        _ => return None,
    }.to_string())
}

/// Load any image file as tightly packed RGBA bytes.
pub fn load_rgba(path: &Path) -> Result<(Vec<u8>, usize, usize), String> {
    crate::imgload::load_rgba(path)
}

/// Reduce an RGBA image to a 24x24 one-bit glyph.
///
/// Cells are box-filtered, composited on white, and split by Otsu's threshold;
/// the *smaller* class is the ink (a logo is smaller than its background),
/// unless the classes are nearly balanced, in which case dark is ink.
/// Returns all-false when the image has no usable contrast.
pub fn glyph_mask(rgba: &[u8], w: usize, h: usize) -> Vec<bool> {
    let mut lum = vec![255.0f32; GLYPH * GLYPH];
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return vec![false; GLYPH * GLYPH];
    }
    for cy in 0..GLYPH {
        for cx in 0..GLYPH {
            let (x0, x1) = (cx * w / GLYPH, ((cx + 1) * w / GLYPH).max(cx * w / GLYPH + 1).min(w));
            let (y0, y1) = (cy * h / GLYPH, ((cy + 1) * h / GLYPH).max(cy * h / GLYPH + 1).min(h));
            let (mut sum, mut n) = (0.0f32, 0.0f32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    let a = rgba[i + 3] as f32 / 255.0;
                    let l = 0.299 * rgba[i] as f32 + 0.587 * rgba[i + 1] as f32 + 0.114 * rgba[i + 2] as f32;
                    sum += a * l + (1.0 - a) * 255.0;
                    n += 1.0;
                }
            }
            lum[cy * GLYPH + cx] = sum / n.max(1.0);
        }
    }
    // Otsu over 256 bins.
    let mut hist = [0u32; 256];
    for l in &lum {
        hist[(*l as usize).min(255)] += 1;
    }
    let total = lum.len() as f32;
    let sum_all: f32 = hist.iter().enumerate().map(|(i, c)| i as f32 * *c as f32).sum();
    let (mut w_b, mut sum_b, mut best, mut thr) = (0.0f32, 0.0f32, -1.0f32, 128usize);
    for t in 0..256 {
        w_b += hist[t] as f32;
        if w_b == 0.0 {
            continue;
        }
        let w_f = total - w_b;
        if w_f == 0.0 {
            break;
        }
        sum_b += t as f32 * hist[t] as f32;
        let (m_b, m_f) = (sum_b / w_b, (sum_all - sum_b) / w_f);
        let between = w_b * w_f * (m_b - m_f) * (m_b - m_f);
        if between > best {
            best = between;
            thr = t;
        }
    }
    let (min, max) = lum.iter().fold((255.0f32, 0.0f32), |(a, b), l| (a.min(*l), b.max(*l)));
    if max - min < 24.0 {
        return vec![false; GLYPH * GLYPH];
    }
    let dark: Vec<bool> = lum.iter().map(|l| *l as usize <= thr).collect();
    let n_dark = dark.iter().filter(|d| **d).count();
    let frac_dark = n_dark as f32 / total;
    let dark_is_ink = if (0.4..=0.6).contains(&frac_dark) { true } else { frac_dark < 0.5 };
    dark.into_iter().map(|d| d == dark_is_ink).collect()
}

/// Generic "package box" glyph used when an icon is missing or blank.
pub fn fallback_mask() -> Vec<bool> {
    const ROWS: [&str; GLYPH] = [
        "                        ",
        "                        ",
        "                        ",
        "                        ",
        " ###################### ",
        " ###################### ",
        " ###################### ",
        " ###################### ",
        " ###################### ",
        " ###################### ",
        "   #      ####      #   ",
        "   #      ####      #   ",
        "   #      ####      #   ",
        "   #      ####      #   ",
        "   #      ####      #   ",
        "   #      ####      #   ",
        "   ## # # ##### # # #   ",
        "   # # # # # # # # ##   ",
        "   ## # # # # # # # #   ",
        "   # # # # # # # # ##   ",
        "   ## # # # # # # # #   ",
        "   ##################   ",
        "                        ",
        "                        ",
    ];
    ROWS.iter().flat_map(|r| r.bytes().map(|b| b == b'#')).collect()
}

/// White-on-transparent 32x32 RGBA tile for a 24x24 glyph mask (no frame: just the glyph).
pub fn tile_rgba(mask: &[bool]) -> Vec<u8> {
    let mut px = vec![0u8; TILE * TILE * 4];
    let off = (TILE - GLYPH) as i32 / 2;
    for gy in 0..GLYPH {
        for gx in 0..GLYPH {
            if mask.get(gy * GLYPH + gx).copied().unwrap_or(false) {
                let i = ((gy as i32 + off) as usize * TILE + (gx as i32 + off) as usize) * 4;
                px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    px
}

/// The tile for an app's own icon: a clean two-tone card like the hand-drawn icons (black drawing on a white
/// plate with a 1 px black outline). The picture is fitted into 32 x 32 (aspect kept) and box-filtered; its
/// brightness is split by Otsu's threshold into subject and plate: the minority class is the subject, so a dark
/// app plate with a bright logo comes out as a black logo on white. The layers: `black` -> ink (subject and
/// outline), `white` -> the theme's icon white (the plate); transparent parts stay transparent. `None` when the
/// picture is flat and fills the whole tile (nothing to draw).
pub fn gray_rgba(rgba: &[u8], w: usize, h: usize) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let scale = (w.max(h) as f32 / TILE as f32).max(1e-3);
    let (tw, th) = ((w as f32 / scale).round().max(1.0) as usize, (h as f32 / scale).round().max(1.0) as usize);
    let (ox, oy) = ((TILE - tw.min(TILE)) / 2, (TILE - th.min(TILE)) / 2);
    // Pass 1: coverage and brightness (of the visible part) per tile pixel.
    let mut cells: Vec<Option<f32>> = vec![None; TILE * TILE];
    for ty in 0..th.min(TILE) {
        for tx in 0..tw.min(TILE) {
            let (x0, x1) = ((tx as f32 * scale) as usize, (((tx + 1) as f32 * scale).ceil() as usize).min(w).max((tx as f32 * scale) as usize + 1));
            let (y0, y1) = ((ty as f32 * scale) as usize, (((ty + 1) as f32 * scale).ceil() as usize).min(h).max((ty as f32 * scale) as usize + 1));
            let (mut cover, mut lum, mut n) = (0f32, 0f32, 0f32);
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    let p = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
                    let a = p[3] as f32 / 255.0;
                    cover += a;
                    lum += (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) * a;
                    n += 1.0;
                }
            }
            if cover / n.max(1.0) >= 0.5 && cover > 0.0 {
                cells[(ty + oy) * TILE + tx + ox] = Some(lum / cover);
            }
        }
    }
    let vis: Vec<f32> = cells.iter().flatten().copied().collect();
    if vis.is_empty() {
        return None;
    }
    let (lo, hi) = vis.iter().fold((255f32, 0f32), |(a, b), &v| (a.min(v), b.max(v)));
    let flat = hi - lo < 40.0;
    if flat && vis.len() >= tw.min(TILE) * th.min(TILE) {
        return None;
    }
    // Otsu's threshold on the stretched brightness (0..255).
    let stretch = |v: f32| if flat { v } else { (v - lo) / (hi - lo) * 255.0 };
    let mut hist = [0usize; 256];
    for &v in &vis {
        hist[stretch(v).round().clamp(0.0, 255.0) as usize] += 1;
    }
    let total = vis.len() as f64;
    let sum_all: f64 = hist.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum();
    let (mut wb, mut sb, mut best, mut thr) = (0f64, 0f64, -1f64, 128usize);
    for (t, &c) in hist.iter().enumerate() {
        wb += c as f64;
        if wb == 0.0 {
            continue;
        }
        let wf = total - wb;
        if wf <= 0.0 {
            break;
        }
        sb += t as f64 * c as f64;
        let (mb, mf) = (sb / wb, (sum_all - sb) / wf);
        let var = wb * wf * (mb - mf) * (mb - mf);
        if var > best {
            best = var;
            thr = t;
        }
    }
    let dark_share = hist[..=thr].iter().sum::<usize>() as f64 / total;
    // A dark majority is the plate: the bright part is the subject. Otherwise the dark part is the subject.
    let subject = |v: f32| {
        let dark = stretch(v) <= thr as f32;
        if flat { v < 128.0 } else if dark_share > 0.5 { !dark } else { dark }
    };
    let opaque = |x: i32, y: i32| x >= 0 && y >= 0 && (x as usize) < TILE && (y as usize) < TILE && cells[y as usize * TILE + x as usize].is_some();
    let mut out = vec![0u8; TILE * TILE * 4];
    for y in 0..TILE as i32 {
        for x in 0..TILE as i32 {
            let Some(v) = cells[y as usize * TILE + x as usize] else { continue };
            let edge = !opaque(x - 1, y) || !opaque(x + 1, y) || !opaque(x, y - 1) || !opaque(x, y + 1);
            let g = if edge || subject(v) { 0u8 } else { 255u8 };
            let i = (y as usize * TILE + x as usize) * 4;
            out[i..i + 4].copy_from_slice(&[g, g, g, 255]);
        }
    }
    Some(out)
}

/// Register `tile/<id>` as a plain texture and as black / grey / white layers (so the grey-scale picture is drawn
/// in the theme's three tones; the generic box, a one-colour mask, comes out in the ink alone).
pub fn insert_tile_rgba(assets: &mut AssetStore, id: &str, rgba: &[u8]) -> Result<(), String> {
    let img = crate::gfx::Image::from_rgba(TILE as u32, TILE as u32, rgba.to_vec()).ok_or("tile size mismatch")?;
    assets.insert(format!("tile/{id}"), img);
    assets.insert_icon_layers(&format!("tile/{id}"), rgba, TILE, TILE)
}

/// The icon tile for an app that Harbour has no hand-made icon for: its own picture in grey, or the
/// generic package box.
pub fn app_tile_rgba(icon_path: Option<&Path>) -> Vec<u8> {
    icon_path
        .and_then(|p| load_rgba(p).ok())
        .and_then(|(px, w, h)| gray_rgba(&px, w, h))
        .unwrap_or_else(|| tile_rgba(&fallback_mask()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, rgba: [u8; 4]) -> Vec<u8> {
        (0..w * h).flat_map(|_| rgba).collect()
    }

    fn paint(img: &mut [u8], w: usize, x0: usize, y0: usize, x1: usize, y1: usize, c: [u8; 4]) {
        for y in y0..y1 {
            for x in x0..x1 {
                img[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&c);
            }
        }
    }

    fn count(m: &[bool]) -> usize {
        m.iter().filter(|b| **b).count()
    }

    #[test]
    fn dark_glyph_on_white() {
        let mut img = solid(96, 96, [255, 255, 255, 255]);
        paint(&mut img, 96, 24, 24, 72, 72, [0, 0, 0, 255]);
        let m = glyph_mask(&img, 96, 96);
        assert_eq!(count(&m), 12 * 12);
        assert!(m[12 * GLYPH + 12]);
        assert!(!m[0]);
    }

    #[test]
    fn light_glyph_on_dark_uses_minority_as_ink() {
        let mut img = solid(96, 96, [20, 20, 90, 255]);
        paint(&mut img, 96, 24, 24, 72, 72, [255, 255, 255, 255]);
        let m = glyph_mask(&img, 96, 96);
        assert_eq!(count(&m), 12 * 12, "the white square, not the background, is ink");
    }

    #[test]
    fn transparent_background_with_dark_glyph() {
        let mut img = solid(48, 48, [0, 0, 0, 0]);
        paint(&mut img, 48, 12, 12, 36, 36, [10, 10, 10, 255]);
        let m = glyph_mask(&img, 48, 48);
        assert_eq!(count(&m), 12 * 12);
    }

    #[test]
    fn flat_image_has_no_glyph() {
        assert_eq!(count(&glyph_mask(&solid(40, 40, [120, 120, 120, 255]), 40, 40)), 0);
        assert_eq!(count(&glyph_mask(&[], 0, 0)), 0);
    }

    #[test]
    fn tiles_are_outlined_or_filled() {
        let mask = fallback_mask();
        assert_eq!(mask.len(), GLYPH * GLYPH);
        let tile = tile_rgba(&mask);
        let alpha = |x: usize, y: usize| tile[(y * TILE + x) * 4 + 3];
        // No frame: the corners and the edge midpoints are empty, the glyph is drawn.
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(16, 0), 0);
        assert_eq!(alpha(4 + 3, 4 + 4), 255, "fallback glyph lid");
    }

    #[test]
    fn an_apps_own_icon_becomes_a_black_on_white_card() {
        // A dark square on a transparent background, 64x64: the square is the drawing (black), its outline black.
        let mut img = vec![0u8; 64 * 64 * 4];
        paint(&mut img, 64, 16, 16, 48, 48, [10, 10, 10, 255]);
        let t = gray_rgba(&img, 64, 64).expect("has contrast");
        let alpha = |x: usize, y: usize| t[(y * TILE + x) * 4 + 3];
        let level = |x: usize, y: usize| t[(y * TILE + x) * 4];
        assert_eq!(alpha(0, 0), 0, "outside the picture stays transparent");
        assert_eq!((alpha(16, 16), level(16, 16)), (255, 0), "dark artwork is black");
        // A dark plate with a bright logo: the logo becomes the black drawing on a white plate with a black outline.
        let mut plate = solid(64, 64, [20, 20, 30, 255]);
        paint(&mut plate, 64, 24, 24, 40, 40, [250, 250, 250, 255]);
        let t = gray_rgba(&plate, 64, 64).expect("has contrast");
        let level = |x: usize, y: usize| t[(y * TILE + x) * 4];
        assert_eq!(level(16, 16), 0, "the logo is black");
        assert_eq!(level(6, 16), 255, "the plate is white");
        assert_eq!(level(0, 16), 0, "1 px black outline on the plate's edge");
        // A flat picture that fills the whole tile has nothing to show.
        assert!(gray_rgba(&solid(32, 32, [128, 128, 128, 255]), 32, 32).is_none());
    }

    #[test]
    fn known_names_use_drawn_icons() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(crate::launcher::packages::APPS_ROOT);
        crate::launcher::packages::install(crate::launcher::packages::scan(&root));
        assert_eq!(known_icon_for("Files").as_deref(), Some("app/files/icon"));
        assert_eq!(known_icon_for("CLI").as_deref(), Some("app/terminal/icon"), "alias");
        assert_eq!(known_icon_for("Rec").as_deref(), Some("app/voicememo/icon"));
        assert_eq!(known_icon_for("Fune"), None, "an official app keeps its own Icon= (converted like any other)");
        assert_eq!(known_icon_for("MS BASIC"), None);
    }
}
