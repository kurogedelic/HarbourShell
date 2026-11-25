//! System icons: the 32 x 32 pictures dialogs show at their left (caution, stop, note, bomb), a set of
//! their own next to the app icons. They are drawn by hand in the private atlas
//! `assets/icons-src/system-icons.png` and sliced into `assets/images/system/<name>.png` by
//! `tools/icons_atlas.py --system unpack` (see docs/ICON-GUIDELINE.md).
//!
//! An icon whose file is missing (not drawn yet) is simply absent: callers then lay out as if the
//! dialog had no picture (or fall back to their own drawing, like [`crate::launcher::alert`]).
//!
//! Colours follow the app icons: black pixels are the theme's ink, grey ones its muted tone, white
//! ones its icon white, transparent ones are not drawn.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::gfx::Rect;
use crate::gfx::Canvas;

use crate::launcher::alert::AlertIcon;
use crate::theme::ThemeManager;

/// Width and height of a system icon.
pub const SIZE: i32 = 32;
/// Where the sliced icons live (relative to the working directory, like the rest of `assets/`).
pub const DIR: &str = "assets/images/system";

/// The file name (without `.png`) of each icon.
pub fn name(icon: AlertIcon) -> &'static str {
    match icon {
        AlertIcon::Caution => "caution",
        AlertIcon::Stop => "stop",
        AlertIcon::Note => "note",
        AlertIcon::Bomb => "bomb",
    }
}

pub fn path(icon: AlertIcon) -> PathBuf {
    PathBuf::from(DIR).join(format!("{}.png", name(icon)))
}

/// A decoded icon: one class per pixel (0 ink, 1 muted, 2 white, 3 transparent), the same split as
/// `assets::split_layers`.
pub struct Mask {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
}

impl Mask {
    pub fn from_rgba(rgba: &[u8], w: i32, h: i32) -> Self {
        let px = rgba
            .chunks_exact(4)
            .map(|p| {
                if p[3] < 128 {
                    return 3;
                }
                let l = (p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000;
                if l < 64 { 0 } else if l > 192 { 2 } else { 1 }
            })
            .collect();
        Self { w, h, px }
    }

    /// Nothing drawn (an empty cell sliced from the atlas counts as missing).
    pub fn is_blank(&self) -> bool {
        self.px.iter().all(|&c| c == 3)
    }
}

fn load_file(p: &std::path::Path) -> Option<Mask> {
    let (rgba, w, h) = crate::imgload::load_rgba(p).ok()?;
    let m = Mask::from_rgba(&rgba, w as i32, h as i32);
    (!m.is_blank()).then_some(m)
}

fn cache() -> &'static Mutex<HashMap<&'static str, Option<Arc<Mask>>>> {
    static C: OnceLock<Mutex<HashMap<&'static str, Option<Arc<Mask>>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The icon, read once from [`DIR`]; `None` when its file is missing, unreadable or blank.
pub fn get(icon: AlertIcon) -> Option<Arc<Mask>> {
    let key = name(icon);
    let mut c = cache().lock().ok()?;
    c.entry(key).or_insert_with(|| load_file(&path(icon)).map(Arc::new)).clone()
}

/// Whether the icon can be shown (its file exists and has a drawing).
pub fn available(icon: AlertIcon) -> bool {
    get(icon).is_some()
}

/// Draws `m` with its top-left at (x, y) in the theme's icon colours.
pub fn draw_mask(c: &mut Canvas, tm: &ThemeManager, m: &Mask, x: i32, y: i32) {
    let tones = [tm.get_color("foreground"), tm.get_color("muted"), tm.get_color("icon_white")];
    for (k, tone) in tones.into_iter().enumerate() {
        c.set_draw_color(tone);
        for row in 0..m.h {
            let line = &m.px[(row * m.w) as usize..((row + 1) * m.w) as usize];
            let mut col = 0;
            while col < m.w {
                if line[col as usize] as usize == k {
                    let start = col;
                    while col < m.w && line[col as usize] as usize == k {
                        col += 1;
                    }
                    let _ = c.fill_rect(Rect::new(x + start, y + row, (col - start) as u32, 1));
                } else {
                    col += 1;
                }
            }
        }
    }
}

/// Draws the system icon at (x, y) if it exists; returns whether something was drawn.
pub fn draw(c: &mut Canvas, tm: &ThemeManager, icon: AlertIcon, x: i32, y: i32) -> bool {
    match get(icon) {
        Some(m) => {
            draw_mask(c, tm, &m, x, y);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_split_into_ink_muted_white_and_nothing() {
        let rgba = [0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 255, 255, 255, 255, 0];
        let m = Mask::from_rgba(&rgba, 4, 1);
        assert_eq!(m.px, [0, 1, 2, 3]);
        assert!(!m.is_blank());
        assert!(Mask::from_rgba(&[0, 0, 0, 0], 1, 1).is_blank());
    }

    #[test]
    fn every_icon_has_its_own_file_name() {
        let all = [AlertIcon::Caution, AlertIcon::Stop, AlertIcon::Note, AlertIcon::Bomb];
        let names: std::collections::HashSet<_> = all.iter().map(|&i| name(i)).collect();
        assert_eq!(names.len(), 4);
        assert_eq!(path(AlertIcon::Caution), PathBuf::from("assets/images/system/caution.png"));
    }

    #[test]
    fn the_shipped_icons_are_32_px_squares() {
        // Run from the repository root (cargo test does): whatever is in assets/images/system is 32 x 32.
        let Ok(dir) = std::fs::read_dir(DIR) else { return };
        for e in dir.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("png") {
                continue;
            }
            if let Some(m) = load_file(&p) {
                assert_eq!((m.w, m.h), (SIZE, SIZE), "{}", p.display());
            }
        }
    }
}
