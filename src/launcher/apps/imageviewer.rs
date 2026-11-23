//! Images: browse `~/Pictures` (png, jpg, bmp, gif), one picture fitted to the
//! 320 x 154 area, in colour or as a 4-tone ordered dither in the theme's
//! ink and paper.
//!
//! Left/Right previous/next, Enter or Up toggles colour / 4-tone, Down shows the
//! file name, Esc quits. Mouse: click left / right third = previous / next,
//! middle = toggle, wheel = previous / next.
//!
//! File operations (menu, or F2 / Delete): Rename asks for a new name; Delete asks first (default
//! Cancel) and moves the picture to the Trash; "Edit in Paint" shows up only when the Paint app is
//! installed. Files opens a single picture here with `open_path` (its folder becomes the list).
//!
//! Decoding uses SDL2_image (already linked; no new crate). The host only
//! enables PNG, so this app asks for JPG as well and falls back to whatever the
//! library supports. Pictures are scaled down while loading, so a big photo
//! costs one temporary decode, not a full-size copy on screen.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sdl2::image::{InitFlag, LoadSurface, Sdl2ImageContext};
use sdl2::keyboard::{Keycode, Mod};
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::surface::Surface;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome;
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::dialogs::{Dialog, DialogOut};
use crate::launcher::fileops;
use crate::launcher::widgets::{contains, MenuItem, PointerEvent};
use crate::platform::keys::arrow_alias;
use crate::ui;

pub const EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "bmp", "gif", "jpe"];
const MAX_FILES: usize = 5000;
const MAX_DEPTH: usize = 4;

/// The picture area: inside the app frame with 1 px clear of its line on every side, down to
/// y 166 (the frame's bottom line is y 168, as on the other framed screens): (3, 19, 314, 148).
pub const AREA_X: i32 = 3;
pub const AREA_Y: i32 = grid::BAR_H + 3;
pub const AREA_W: i32 = grid::SCREEN_W - 2 * AREA_X;
pub const AREA_H: i32 = grid::APP_FRAME_BOTTOM - 1 - AREA_Y;

pub fn is_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false)
}

pub fn scan(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > MAX_DEPTH || out.len() >= MAX_FILES {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => walk(&p, depth + 1, out),
                Ok(t) if (t.is_file() || t.is_symlink()) && is_image(&p) => out.push(p),
                _ => {}
            }
        }
    }
    let mut v = Vec::new();
    walk(root, 0, &mut v);
    v.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    v
}

/// Largest size with the picture's aspect ratio that fits `aw` x `ah`.
pub fn fit(w: u32, h: u32, aw: u32, ah: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (1, 1);
    }
    let s = (aw as f64 / w as f64).min(ah as f64 / h as f64);
    (((w as f64 * s).round() as u32).clamp(1, aw), ((h as f64 * s).round() as u32).clamp(1, ah))
}

/// Box-filter resize of tightly packed RGB (nearest-ish when enlarging).
pub fn resize_box(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh * 3];
    for dy in 0..dh {
        let (y0, y1) = (dy * sh / dh, ((dy + 1) * sh / dh).max(dy * sh / dh + 1).min(sh));
        for dx in 0..dw {
            let (x0, x1) = (dx * sw / dw, ((dx + 1) * sw / dw).max(dx * sw / dw + 1).min(sw));
            let mut acc = [0u32; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * sw + x) * 3;
                    acc[0] += src[i] as u32;
                    acc[1] += src[i + 1] as u32;
                    acc[2] += src[i + 2] as u32;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = (dy * dw + dx) * 3;
            for k in 0..3 {
                out[o + k] = (acc[k] / n) as u8;
            }
        }
    }
    out
}

const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

pub fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((299 * r as u32 + 587 * g as u32 + 114 * b as u32) / 1000) as u8
}

/// Is this pixel lit in the 4x4 ordered (Bayer) dither? Bright pixels are lit, as the
/// themes are light-on-dark: lit = ink.
pub fn bayer_lit(lum: u8, x: usize, y: usize) -> bool {
    let threshold = BAYER4[y % 4][x % 4] as u32 * 16 + 8;
    lum as u32 >= threshold
}

pub struct Picture {
    pub w: u32,
    pub h: u32,
    pub rgb: Vec<u8>,
}

/// Decode a file and scale it to fit the area.
pub fn load(path: &Path) -> Result<Picture, String> {
    from_surface(Surface::from_file(path)?, AREA_W as u32, AREA_H as u32)
}

/// Decode in-memory image data (an embedded cover) and scale it to fit `max_w` x `max_h`.
pub fn decode_bytes(bytes: &[u8], max_w: u32, max_h: u32) -> Result<Picture, String> {
    use sdl2::image::ImageRWops;
    let surf = sdl2::rwops::RWops::from_bytes(bytes)?.load()?;
    from_surface(surf, max_w, max_h)
}

fn from_surface(surf: Surface, max_w: u32, max_h: u32) -> Result<Picture, String> {
    let surf = surf.convert_format(PixelFormatEnum::RGB24)?;
    let (sw, sh, pitch) = (surf.width() as usize, surf.height() as usize, surf.pitch() as usize);
    let mut tight = Vec::with_capacity(sw * sh * 3);
    surf.with_lock(|px| {
        for y in 0..sh {
            tight.extend_from_slice(&px[y * pitch..y * pitch + sw * 3]);
        }
    });
    let (dw, dh) = fit(sw as u32, sh as u32, max_w, max_h);
    let rgb = if (dw as usize, dh as usize) == (sw, sh) { tight } else { resize_box(&tight, sw, sh, dw as usize, dh as usize) };
    Ok(Picture { w: dw, h: dh, rgb })
}

/// Pre-computed drawing runs for one picture in one mode (a run is a horizontal span).
pub enum Runs {
    Colour(Vec<(Rect, Color)>),
    /// Four tones from the theme (ink to surface), ordered-dithered.
    Tone(crate::launcher::apps::tone::ToneRuns),
}

pub fn build_runs(p: &Picture, mono: bool, ox: i32, oy: i32) -> Runs {
    let at = |x: usize, y: usize| {
        let i = (y as usize * p.w as usize + x) * 3;
        (p.rgb[i], p.rgb[i + 1], p.rgb[i + 2])
    };
    if mono {
        Runs::Tone(crate::launcher::apps::tone::build(p, crate::launcher::apps::tone::DEFAULT, ox, oy))
    } else {
        let mut v = Vec::new();
        for y in 0..p.h as usize {
            let q = |c: u8| (c & 0xF0) | (c >> 4); // 16 levels per channel keeps the runs long
            let mut start = 0usize;
            let mut cur = at(0, y);
            for x in 1..=p.w as usize {
                let next = if x < p.w as usize { at(x, y) } else { (0, 0, 0) };
                let (a, b) = ((q(cur.0), q(cur.1), q(cur.2)), (q(next.0), q(next.1), q(next.2)));
                if x == p.w as usize || a != b {
                    v.push((Rect::new(ox + start as i32, oy + y as i32, (x - start) as u32, 1), Color::RGB(a.0, a.1, a.2)));
                    start = x;
                }
                cur = next;
            }
        }
        Runs::Colour(v)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    Rename,
    Delete,
}

pub struct ImageViewer {
    modal: Option<Dialog<Ask>>,
    trash: Option<PathBuf>,
    _img: Option<Sdl2ImageContext>,
    files: Vec<PathBuf>,
    index: usize,
    pic: Option<Result<Picture, String>>,
    runs: Option<(bool, Runs)>,
    mono: bool,
    caption_until: Option<Instant>,
}

pub fn make() -> Box<dyn LauncherApp> {
    let root = fileops::user_dir(fileops::UserDir::Pictures);
    Box::new(ImageViewer::new(scan(&root)))
}

impl ImageViewer {
    pub fn new(files: Vec<PathBuf>) -> Self {
        // The host initialises PNG only; JPG is requested here when the library has it.
        let ctx = sdl2::image::init(InitFlag::PNG | InitFlag::JPG).or_else(|_| sdl2::image::init(InitFlag::PNG)).ok();
        ImageViewer { modal: None, trash: fileops::trash_dir(), _img: ctx, files, index: 0, pic: None, runs: None, mono: false, caption_until: None }
    }

    fn show(&mut self, index: usize) {
        if self.files.is_empty() {
            return;
        }
        self.index = index % self.files.len();
        self.pic = None; // loaded lazily by draw so the old frame stays until then
        self.runs = None;
        self.caption_until = Some(Instant::now() + Duration::from_millis(1800));
    }

    fn step(&mut self, delta: isize) {
        let n = self.files.len() as isize;
        if n > 0 {
            self.show((self.index as isize + delta).rem_euclid(n) as usize);
        }
    }

    fn current(&self) -> Option<PathBuf> {
        self.files.get(self.index).cloned()
    }

    fn ask_rename(&mut self) {
        if let Some(p) = self.current() {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.modal = Some(Dialog::name("Rename picture to:", &name, Ask::Rename));
        }
    }

    fn ask_delete(&mut self) {
        if let Some(p) = self.current() {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let lines = vec![format!("Move \"{name}\" to the Trash?"), "Get it back from Files > Go to Trash.".to_string()];
            self.modal = Some(Dialog::confirm(lines, "Trash", Ask::Delete));
        }
    }

    fn finish(&mut self, out: DialogOut) -> AppAction {
        let Some(d) = self.modal.take() else { return AppAction::None };
        let Some(path) = self.current() else { return AppAction::None };
        match (d.tag, out) {
            (Ask::Rename, DialogOut::Name(n)) => match fileops::rename(&path, &n) {
                Ok(new) => {
                    // A picture must keep a picture extension, or it drops out of the list.
                    if !is_image(&new) {
                        let _ = fileops::rename(&new, path.file_name().and_then(|n| n.to_str()).unwrap_or(""));
                        return AppAction::Toast(crate::launcher::i18n::tr_s("Keep a picture extension (.png, .jpg...)"));
                    }
                    self.files[self.index] = new;
                    self.caption_until = Some(Instant::now() + Duration::from_secs(2));
                    return AppAction::Toast(crate::launcher::i18n::tr_s("Renamed"));
                }
                Err(e) => {
                    self.modal = Some(Dialog::name("Rename picture to:", &n, Ask::Rename));
                    return AppAction::Toast(crate::launcher::i18n::tr_s(&e));
                }
            },
            (Ask::Delete, DialogOut::Ok) => {
                let Some(t) = &self.trash else { return AppAction::Toast(crate::launcher::i18n::tr_s("No Trash folder (HOME is not set)")) };
                return match fileops::trash(&path, t) {
                    Ok(_) => {
                        self.files.remove(self.index);
                        self.pic = None;
                        self.runs = None;
                        if self.index >= self.files.len() {
                            self.index = self.files.len().saturating_sub(1);
                        }
                        AppAction::Toast(crate::launcher::i18n::tr_s("Moved to the Trash"))
                    }
                    Err(e) => AppAction::Toast(crate::launcher::i18n::tr_s(&e)),
                };
            }
            _ => {}
        }
        AppAction::None
    }

    fn modal_key(&mut self, key: Keycode) -> AppAction {
        let out = self.modal.as_mut().and_then(|d| d.key(key));
        match out {
            Some(o) => self.finish(o),
            None => AppAction::None,
        }
    }

    fn toggle_mono(&mut self) {
        self.mono = !self.mono;
        self.runs = None;
    }
}

impl LauncherApp for ImageViewer {
    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Left / Right", "Previous / next picture"),
            k("Return / Up", "Colour / 4-tone"),
            k("Down", "Show file name"),
            k("F2", "Rename"),
            k("Click the edges", "Previous / next picture"),
            k("Click the middle", "Colour / 4-tone"),
        ]
    }

    fn title(&self) -> String {
        crate::launcher::i18n::app_name("images")
    }

    fn menu(&self) -> Vec<MenuItem> {
        let mut v = vec![MenuItem::new("mono", "Colour / 4-tone"), MenuItem::new("name", "Show file name")];
        if !self.files.is_empty() {
            if crate::launcher::apps::files::paint_installed() {
                v.push(MenuItem::new("paint", "Edit in Paint"));
            }
            v.push(MenuItem::new("rename", "Rename..."));
            v.push(MenuItem::new("delete", "Delete..."));
        }
        v
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        if !is_image(path) {
            return Err("Not a picture Images can show".into());
        }
        let dir = path.parent().ok_or("No folder")?;
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().is_file() && is_image(&e.path()))
            .map(|e| e.path())
            .collect();
        v.sort_by_key(|p| p.to_string_lossy().to_lowercase());
        let i = v.iter().position(|p| p == path).ok_or("That picture is gone")?;
        self.files = v;
        self.show(i);
        Ok(())
    }

    fn menu_action(&mut self, id: &str, _ctx: &Ctx) -> AppAction {
        match id {
            "rename" => self.ask_rename(),
            "delete" => self.ask_delete(),
            "paint" => {
                if let Some(path) = self.current() {
                    return AppAction::Open { app: Some("Paint".into()), path };
                }
            }
            "mono" => self.toggle_mono(),
            "name" => self.caption_until = Some(Instant::now() + Duration::from_secs(3)),
            _ => {}
        }
        AppAction::None
    }

    fn text(&mut self, text: &str, _ctx: &Ctx) -> AppAction {
        if let Some(d) = &mut self.modal {
            d.text(text);
        }
        AppAction::None
    }

    fn key(&mut self, key: Keycode, _mods: Mod, _repeat: bool, _ctx: &Ctx) -> AppAction {
        if self.modal.is_some() {
            return self.modal_key(key);
        }
        match arrow_alias(key) {
            Keycode::F2 => self.ask_rename(),
            Keycode::Delete => self.ask_delete(),
            Keycode::Escape => return AppAction::Quit,
            Keycode::Left => self.step(-1),
            Keycode::Right => self.step(1),
            Keycode::Up | Keycode::Return | Keycode::KpEnter => self.toggle_mono(),
            Keycode::Down => self.caption_until = Some(Instant::now() + Duration::from_secs(3)),
            _ => {}
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, _ctx: &Ctx) -> AppAction {
        if let Some(d) = &mut self.modal {
            if let PointerEvent::Down { x, y, button: 0, .. } = ev {
                if let Some(o) = d.click(x, y) {
                    return self.finish(o);
                }
            }
            return AppAction::None;
        }
        match ev {
            PointerEvent::Wheel { dy, .. } => self.step(if dy > 0 { -1 } else { 1 }),
            PointerEvent::Down { x, y, button: 0, .. } if contains(grid::app_frame(), x, y) => {
                if x < grid::SCREEN_W / 3 {
                    self.step(-1)
                } else if x >= grid::SCREEN_W * 2 / 3 {
                    self.step(1)
                } else {
                    self.toggle_mono()
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, _ctx: &Ctx) -> bool {
        match self.caption_until {
            Some(t) if Instant::now() >= t => {
                self.caption_until = None;
                true
            }
            Some(_) => true,
            None => false,
        }
    }

    fn draw(&mut self, c: &mut Canvas<Window>, _assets: &mut AssetStore, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let paper = if self.mono { tm.get_color("home_bg") } else { Color::RGB(0, 0, 0) };
        let faint = crate::launcher::widgets::secondary_ink(tm);
        let area = Rect::new(AREA_X, AREA_Y, AREA_W as u32, AREA_H as u32);
        ui::fill_rect(c, Rect::new(0, grid::BAR_H, grid::SCREEN_W as u32, (grid::SCREEN_H - grid::BAR_H) as u32), if self.mono { paper } else { tm.get_color("home_bg") });
        grid::draw_app_frame(c, grid::app_frame().y(), ink, None);

        if self.files.is_empty() {
            fs.draw_text(c, grid::MARGIN + 6, grid::BAR_H + grid::MARGIN + 4, &crate::launcher::i18n::tr_s(&format!("No pictures in {}", crate::launcher::fileops::tilde(&crate::launcher::fileops::user_dir(crate::launcher::fileops::UserDir::Pictures)))), faint);
            return;
        }
        if self.pic.is_none() {
            self.pic = Some(load(&self.files[self.index]));
        }
        match self.pic.as_ref().unwrap() {
            Err(e) => {
                fs.draw_text(c, grid::MARGIN + 6, grid::BAR_H + grid::MARGIN + 4, &chrome::fit_text(fs, &crate::launcher::i18n::tr_s(&format!("Cannot open: {e}")), AREA_W - 20), faint);
            }
            Ok(p) => {
                let (ox, oy) = (area.x() + (AREA_W - p.w as i32) / 2, area.y() + (AREA_H - p.h as i32) / 2);
                if self.runs.as_ref().map(|r| r.0) != Some(self.mono) {
                    self.runs = Some((self.mono, build_runs(p, self.mono, ox, oy)));
                }
                if !self.mono {
                    ui::fill_rect(c, Rect::new(ox, oy, p.w, p.h), Color::RGB(0, 0, 0));
                }
                match &self.runs.as_ref().unwrap().1 {
                    Runs::Colour(v) => {
                        for (r, col) in v {
                            c.set_draw_color(*col);
                            let _ = c.fill_rect(*r);
                        }
                    }
                    Runs::Tone(t) => crate::launcher::apps::tone::draw(c, t, &crate::launcher::apps::tone::theme_ramp(tm)),
                }
            }
        }
        if self.caption_until.is_some() {
            let name = self.files[self.index].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let text = chrome::fit_text(fs, &format!("{}/{}  {}", self.index + 1, self.files.len(), name), AREA_W - 16);
            let w = fs.measure_text(&text) + 8;
            let r = Rect::new(4, grid::CONTENT_BOTTOM - 20, w as u32, 16);
            ui::fill_rect(c, r, tm.get_color("home_bg"));
            ui::draw_rect(c, r, ink);
            fs.draw_text(c, r.x() + 4, r.y() + 1, &text, ink);
        }
        if let Some(d) = &self.modal {
            d.draw(c, fs, tm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_area_keeps_1_px_clear_of_the_frame_on_every_side() {
        let f = grid::app_frame_inner();
        let inset = Rect::new(f.x() + 1, f.y() + 1, f.width() - 2, f.height() - 2);
        assert_eq!(inset, Rect::new(AREA_X, AREA_Y, AREA_W as u32, AREA_H as u32));
        assert_eq!(AREA_Y + AREA_H, grid::APP_FRAME_BOTTOM - 1, "last row y 166, the frame's line on y 168");
    }

    #[test]
    fn fitting_keeps_the_aspect_ratio() {
        assert_eq!(fit(640, 480, 320, 154), (205, 154));
        assert_eq!(fit(100, 400, 320, 154), (39, 154));
        assert_eq!(fit(32, 15, 320, 154), (320, 150));
        assert_eq!(fit(320, 154, 320, 154), (320, 154));
        assert_eq!(fit(0, 0, 320, 154), (1, 1));
    }

    #[test]
    fn box_resize_averages() {
        let src = [0, 0, 0, 100, 100, 100, 200, 200, 200, 100, 100, 100];
        assert_eq!(resize_box(&src, 2, 2, 1, 1), vec![100, 100, 100]);
        assert_eq!(resize_box(&src, 2, 2, 2, 2), src.to_vec());
        assert_eq!(resize_box(&[9, 8, 7], 1, 1, 2, 2).len(), 12, "enlarging repeats pixels");
    }

    #[test]
    fn bayer_is_empty_for_black_full_for_white_and_about_half_for_grey() {
        let count = |l: u8| (0..16).filter(|i| bayer_lit(l, i % 4, i / 4)).count();
        assert_eq!(count(0), 0);
        assert_eq!(count(255), 16);
        assert_eq!(count(128), 8);
        assert!(count(64) < count(128) && count(128) < count(192));
    }

    #[test]
    fn luma_weights() {
        assert_eq!(luma(255, 255, 255), 255);
        assert_eq!(luma(0, 0, 0), 0);
        assert!(luma(0, 255, 0) > luma(255, 0, 0) && luma(255, 0, 0) > luma(0, 0, 255));
    }

    #[test]
    fn mono_runs_merge_neighbours() {
        let p = Picture { w: 8, h: 1, rgb: vec![255; 24] };
        // All white: the lightest tone everywhere, one run per row.
        match build_runs(&p, true, 10, 20) {
            Runs::Tone(t) => assert_eq!(t.runs[3], vec![Rect::new(10, 20, 8, 1)]),
            _ => panic!(),
        }
        match build_runs(&p, false, 0, 0) {
            Runs::Colour(v) => assert_eq!(v.len(), 1),
            _ => panic!(),
        }
    }

    #[test]
    fn loads_and_scales_a_bmp() {
        // 640 x 2 24-bit BMP: red left half, blue right half.
        let (w, h) = (640u32, 2u32);
        let row = (w * 3 + 3) & !3;
        let mut f = vec![b'B', b'M'];
        f.extend((54 + row * h).to_le_bytes());
        f.extend([0, 0, 0, 0]);
        f.extend(54u32.to_le_bytes());
        f.extend(40u32.to_le_bytes());
        f.extend(w.to_le_bytes());
        f.extend(h.to_le_bytes());
        f.extend([1, 0, 24, 0]);
        f.extend([0u8; 24]);
        for _ in 0..h {
            let mut r = Vec::new();
            for x in 0..w {
                r.extend(if x < w / 2 { [0, 0, 255] } else { [255, 0, 0] }); // BGR
            }
            r.resize(row as usize, 0);
            f.extend(r);
        }
        let path = std::env::temp_dir().join(format!("harbour_img_{}.bmp", std::process::id()));
        std::fs::write(&path, f).unwrap();
        let p = load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((p.w, p.h), (AREA_W as u32, 1));
        assert_eq!(&p.rgb[0..3], &[255, 0, 0], "left is red");
        assert_eq!(&p.rgb[(p.w as usize - 1) * 3..p.w as usize * 3], &[0, 0, 255], "right is blue");
    }

    #[test]
    fn scans_pictures_and_skips_others() {
        let base = std::env::temp_dir().join(format!("harbour_pics_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        for f in ["b.PNG", "a.jpg", "x.txt", "sub/c.gif", "sub/d.bmp"] {
            std::fs::write(base.join(f), b"x").unwrap();
        }
        let names: Vec<_> = scan(&base).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.jpg", "b.PNG", "c.gif", "d.bmp"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn rename_and_delete_ask_first_and_keep_the_list_in_step() {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("harbour_imgops_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        for f in ["a.png", "b.png", "c.png"] {
            std::fs::write(base.join(f), b"x").unwrap();
        }
        let mut v = ImageViewer::new(vec![base.join("a.png"), base.join("b.png"), base.join("c.png")]);
        v.trash = Some(base.join("Trash"));
        v.index = 1;
        v.ask_rename();
        v.modal.as_mut().unwrap().text("日本.png");
        assert!(matches!(v.modal_key(Keycode::Return), AppAction::Toast(t) if t == "Renamed"));
        assert!(base.join("日本.png").exists() && v.files[1] == base.join("日本.png"));
        // Dropping the extension is refused and undone.
        v.ask_rename();
        v.modal.as_mut().unwrap().text("noext");
        assert!(matches!(v.modal_key(Keycode::Return), AppAction::Toast(t) if t.contains("extension")));
        assert!(base.join("日本.png").exists() && !base.join("noext").exists());
        // Delete: Enter cancels, the second time confirms.
        v.ask_delete();
        v.modal_key(Keycode::Return);
        assert!(base.join("日本.png").exists());
        v.ask_delete();
        v.modal_key(Keycode::Right);
        v.modal_key(Keycode::Return);
        assert!(!base.join("日本.png").exists() && base.join("Trash/files/日本.png").exists());
        assert_eq!(v.files.len(), 2);
        assert_eq!(v.index, 1, "the next picture takes its place");
        v.index = 1;
        v.ask_delete();
        v.modal_key(Keycode::Right);
        v.modal_key(Keycode::Return);
        assert_eq!(v.index, 0, "deleting the last one steps back");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn open_path_lists_the_pictures_beside_the_file() {
        let base = std::env::temp_dir().join(format!("harbour_imgopen_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        for f in ["a.png", "b.jpg", "n.txt", ".h.png"] {
            std::fs::write(base.join(f), b"x").unwrap();
        }
        let mut v = ImageViewer::new(vec![]);
        v.open_path(&base.join("b.jpg")).unwrap();
        assert_eq!((v.files.len(), v.index), (2, 1));
        assert!(v.open_path(&base.join("n.txt")).is_err());
        let has_paint = crate::launcher::apps::REGISTRY.iter().any(|d| d.name == "Paint");
        assert_eq!(v.menu().iter().any(|m| m.id == "paint"), has_paint, "Edit in Paint only with a Paint app");
        assert!(v.menu().iter().any(|m| m.id == "rename") && v.menu().iter().any(|m| m.id == "delete"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn help_rows_are_filled_in() {
        crate::launcher::help::check_app_rows("Images", &ImageViewer::new(vec![]).help());
    }
}
