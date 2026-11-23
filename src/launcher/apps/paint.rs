//! Paint: a MacPaint-style 1-bit paint program for the 320 x 170 screen.
//!
//! * A 512 x 342 sheet scrolled in a window; zoom 1x / 2x / 4x (4x shows the pixel grid, "fat bits").
//! * Left: the tool strip (pencil, brush, eraser, hand, line, rectangle, rounded rectangle, ellipse, bucket,
//!   spray, text, select), then the fill pattern, the brush size and the filled-shapes switch.
//! * Mouse: drag with the left button. Right click = context menu (tools over the strip, editing
//!   commands over the sheet). Wheel = scroll.
//! * Keyboard (complete without a mouse): arrows move a cursor (Shift = 8 px), Space puts the pen down /
//!   lifts it (a shape starts / ends the same way), `P B E H L R Q O F Y T M` pick the tools, `G` opens the
//!   patterns, `[` `]` the brush size, `V` fill / outline, `X` pen black / white, `1 2 4` zoom, `Ctrl+Z/Y`
//!   undo / redo, `Ctrl+S/O/N` save / open / new, `Ctrl+A` select all, `Ctrl+C/X/V` copy / cut / paste.
//!   Esc cancels the thing in progress and never leaves the app (hold it to leave).
//! * Files: 1-bit PNG in `~/Pictures` (`HARBOUR_MOCK` uses a scratch folder). Colours are strictly ink and
//!   paper (the theme's), the files are pure black and white.
//!
//! The drawing code lives in [`paint_model`](super::paint_model) (no SDL, unit tested).

use std::path::{Path, PathBuf};

use sdl2::image::{InitFlag, LoadSurface, Sdl2ImageContext};
use sdl2::keyboard::{Keycode, Mod};
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::surface::Surface;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::launcher::alert::{Alert, AlertIcon};
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::apps::paint_model::*;
use crate::launcher::dialogs::{CardChoice, ConfirmAlert, ConfirmChoice, ListPicker, NamePrompt, PickResult, PromptResult, UnsavedCard};
use crate::launcher::widgets::{self, contains, draw_panel, MenuItem, PointerEvent, ScrollBar};
use crate::grid;
use crate::ui;

// ------------------------------------------------------------------------------ geometry

const STRIP_W: i32 = 38;
const CELL_W: i32 = 18;
const CELL_H: i32 = 18;
const TOOL_ROWS: i32 = 6;
/// Top of the tool strip (its cells start 1 px lower).
const STRIP_Y: i32 = grid::BAR_H;
/// The pattern swatch and the size / fill cells under the tools share what is left down to y 168
/// (2 x 22 px), so the strip has no empty band at the bottom.
const SWATCH_H: i32 = (grid::APP_FRAME_BOTTOM + 1 - (STRIP_Y + 1 + TOOL_ROWS * CELL_H)) / 2;
/// The visible sheet sits inside a 1 px frame (`frame_rect`, x 39..312, y 17..162): 1 px right of
/// the strip's rule (x 37), 1 px below the menu strip. Right of it the vertical scroll bar
/// (x 315..318, y 18..161), under it the horizontal one
/// (y 164..167, near the screen's bottom like the framed apps), each 1-2 px away from the line.
/// The sheet's frame stays square: it does not reach the screen's corners.
const VX: i32 = STRIP_W + 2;
const VY: i32 = grid::BAR_H + 2;
const VW: i32 = 272;
const VH: i32 = 144;
const VBAR_X: i32 = grid::SCREEN_W - 1 - grid::SCROLL_W;

fn frame_rect() -> Rect {
    Rect::new(VX - 1, VY - 1, (VW + 2) as u32, (VH + 2) as u32)
}
const SIZES: [i32; 4] = [1, 2, 4, 6];
const ZOOMS: [i32; 3] = [1, 2, 4];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pencil,
    Brush,
    Eraser,
    Hand,
    Line,
    Rect,
    RoundRect,
    Ellipse,
    Fill,
    Spray,
    Text,
    Select,
}

const TOOLS: [(Tool, &str, &str); 12] = [
    (Tool::Pencil, "t_pencil", "Pencil"),
    (Tool::Brush, "t_brush", "Brush"),
    (Tool::Eraser, "t_eraser", "Eraser"),
    (Tool::Hand, "t_hand", "Hand"),
    (Tool::Line, "t_line", "Line"),
    (Tool::Rect, "t_rect", "Rectangle"),
    (Tool::RoundRect, "t_round", "Rounded Rect"),
    (Tool::Ellipse, "t_ellipse", "Ellipse"),
    (Tool::Fill, "t_fill", "Bucket"),
    (Tool::Spray, "t_spray", "Spray"),
    (Tool::Text, "t_text", "Text"),
    (Tool::Select, "t_select", "Select"),
];

fn tool_name(t: Tool) -> &'static str {
    TOOLS.iter().find(|x| x.0 == t).map(|x| x.2).unwrap_or("")
}

fn tool_cell(i: usize) -> Rect {
    Rect::new(1 + (i as i32 % 2) * CELL_W, STRIP_Y + 1 + (i as i32 / 2) * CELL_H, CELL_W as u32, CELL_H as u32)
}

fn pattern_cell() -> Rect {
    Rect::new(1, STRIP_Y + 1 + TOOL_ROWS * CELL_H, (CELL_W * 2) as u32, SWATCH_H as u32)
}

fn size_cell() -> Rect {
    Rect::new(1, pattern_cell().y() + SWATCH_H, CELL_W as u32, SWATCH_H as u32)
}

fn fill_cell() -> Rect {
    Rect::new(1 + CELL_W, pattern_cell().y() + SWATCH_H, CELL_W as u32, SWATCH_H as u32)
}

fn popup_rect() -> Rect {
    Rect::new(STRIP_W + 2, VY + 2, 4 * 28 + 8, 2 * 20 + 8 + 14)
}

fn popup_cell(i: usize) -> Rect {
    let r = popup_rect();
    Rect::new(r.x() + 4 + (i as i32 % 4) * 28, r.y() + 4 + (i as i32 / 4) * 20, 26, 18)
}

// ------------------------------------------------------------------------------ state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    None,
    New,
    Open,
    /// Leave the app (hold Esc / menu Quit with unsaved changes).
    Quit,
}

enum Modal {
    Name { p: NamePrompt, then: Pending },
    Replace { c: ConfirmAlert, path: PathBuf, then: Pending },
    Unsaved { c: UnsavedCard, then: Pending },
    Pick { p: ListPicker, files: Vec<PathBuf> },
    Error(Alert),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stroke {
    Freehand { last: (i32, i32) },
    Spray { last: (i32, i32) },
    Shape { start: (i32, i32) },
    Marquee { start: (i32, i32) },
    Move { grab: (i32, i32) },
    Pan,
}

struct Selection {
    r: (i32, i32, usize, usize),
    /// Lifted pixels, drawn over the sheet at `r` until committed.
    float: Option<Bitmap>,
}

struct TextEdit {
    x: i32,
    y: i32,
    buf: String,
    base: Bitmap,
    dirty: bool,
}

pub struct Paint {
    _img: Option<Sdl2ImageContext>,
    dir: PathBuf,
    bm: Bitmap,
    hist: History,
    base: Option<Bitmap>,
    tool: Tool,
    pattern: Pattern,
    size_i: usize,
    filled: bool,
    paper_pen: bool,
    constrain: bool,
    zoom: i32,
    sx: i32,
    sy: i32,
    stroke: Option<Stroke>,
    cursor: (i32, i32),
    kb: bool,
    popup: Option<usize>,
    modal: Option<Modal>,
    path: Option<PathBuf>,
    dirty: bool,
    text: Option<TextEdit>,
    sel: Option<Selection>,
    clip: Option<Bitmap>,
    seed: u32,
    sbar: Option<()>,
    hbar: bool,
    toast: Option<String>,
}

fn scratch_mode() -> bool {
    matches!(std::env::var("HARBOUR_MOCK").as_deref(), Ok("1" | "true" | "yes" | "on"))
}

pub fn pictures_dir() -> PathBuf {
    if scratch_mode() {
        return std::env::temp_dir().join("harbour-paint-mock");
    }
    crate::launcher::fileops::user_dir(crate::launcher::fileops::UserDir::Pictures)
}

pub fn make() -> Box<dyn LauncherApp> {
    Box::new(Paint::new(pictures_dir()))
}

impl Paint {
    pub fn new(dir: PathBuf) -> Self {
        let img = sdl2::image::init(InitFlag::PNG | InitFlag::JPG).or_else(|_| sdl2::image::init(InitFlag::PNG)).ok();
        Paint {
            _img: img,
            dir,
            bm: Bitmap::new(SHEET_W, SHEET_H),
            hist: History::new(20),
            base: None,
            tool: Tool::Pencil,
            pattern: Pattern(3),
            size_i: 0,
            filled: false,
            paper_pen: false,
            constrain: false,
            zoom: 1,
            sx: 0,
            sy: 0,
            stroke: None,
            cursor: (8, 8),
            kb: false,
            popup: None,
            modal: None,
            path: None,
            dirty: false,
            text: None,
            sel: None,
            clip: None,
            seed: 0x2545_F491,
            sbar: None,
            hbar: false,
            toast: None,
        }
    }

    // ------------------------------------------------------------------ view

    fn max_scroll(&self) -> (i32, i32) {
        ((self.bm.w as i32 - VW / self.zoom).max(0), (self.bm.h as i32 - VH / self.zoom).max(0))
    }

    fn clamp_scroll(&mut self) {
        let (mx, my) = self.max_scroll();
        self.sx = self.sx.clamp(0, mx);
        self.sy = self.sy.clamp(0, my);
    }

    fn set_zoom(&mut self, z: i32) {
        if z == self.zoom || !ZOOMS.contains(&z) {
            return;
        }
        let (cx, cy) = (self.sx + VW / (2 * self.zoom), self.sy + VH / (2 * self.zoom));
        self.zoom = z;
        self.sx = cx - VW / (2 * z);
        self.sy = cy - VH / (2 * z);
        self.clamp_scroll();
    }

    fn to_sheet(&self, x: i32, y: i32) -> (i32, i32) {
        (self.sx + (x - VX).div_euclid(self.zoom), self.sy + (y - VY).div_euclid(self.zoom))
    }

    fn in_view(x: i32, y: i32) -> bool {
        contains(Rect::new(VX, VY, VW as u32, VH as u32), x, y)
    }

    fn follow_cursor(&mut self) {
        let (cw, ch) = (VW / self.zoom, VH / self.zoom);
        let (cx, cy) = self.cursor;
        if cx < self.sx {
            self.sx = cx;
        } else if cx >= self.sx + cw {
            self.sx = cx - cw + 1;
        }
        if cy < self.sy {
            self.sy = cy;
        } else if cy >= self.sy + ch {
            self.sy = cy - ch + 1;
        }
        self.clamp_scroll();
    }

    fn brush_size(&self) -> i32 {
        SIZES[self.size_i]
    }

    fn ink(&self) -> bool {
        !self.paper_pen
    }

    // ------------------------------------------------------------------ editing

    fn checkpoint(&mut self) {
        self.hist.push(&self.bm);
        self.dirty = true;
    }

    /// Put a lifted selection back into the sheet.
    fn commit_float(&mut self) {
        if let Some(s) = self.sel.as_mut() {
            if let Some(f) = s.float.take() {
                self.bm.paste(&f, s.r.0, s.r.1, false);
            }
        }
    }

    fn drop_selection(&mut self) {
        self.commit_float();
        self.sel = None;
    }

    fn commit_text(&mut self) {
        if let Some(t) = self.text.take() {
            if t.buf.is_empty() {
                self.bm.copy_from(&t.base);
            } else {
                // The text is already stamped into `bm`; the history gets the state before it.
                self.hist.push(&t.base);
                self.dirty = true;
            }
        }
    }

    fn cancel_text(&mut self) {
        if let Some(t) = self.text.take() {
            self.bm.copy_from(&t.base);
        }
    }

    /// Anything half done is finished before the tool, the file or the history changes.
    fn settle(&mut self) {
        self.commit_text();
        self.commit_float();
    }

    fn set_tool(&mut self, t: Tool) {
        if t != self.tool {
            self.settle();
            if t != Tool::Select {
                self.sel = None;
            }
            self.tool = t;
        }
        self.stroke = None;
    }

    fn undo(&mut self) -> bool {
        self.settle();
        match self.hist.undo(&self.bm) {
            Some(b) => {
                self.bm = b;
                self.sel = None;
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    fn redo(&mut self) -> bool {
        self.settle();
        match self.hist.redo(&self.bm) {
            Some(b) => {
                self.bm = b;
                self.sel = None;
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    fn begin(&mut self, p: (i32, i32)) {
        self.stroke = None;
        let ink = self.ink();
        match self.tool {
            Tool::Pencil | Tool::Brush | Tool::Eraser => {
                self.settle();
                self.sel = None;
                self.checkpoint();
                self.stamp(p);
                self.stroke = Some(Stroke::Freehand { last: p });
            }
            Tool::Spray => {
                self.settle();
                self.sel = None;
                self.checkpoint();
                spray(&mut self.bm, p.0, p.1, 7, 14, ink, &mut self.seed);
                self.stroke = Some(Stroke::Spray { last: p });
            }
            Tool::Line | Tool::Rect | Tool::RoundRect | Tool::Ellipse => {
                self.settle();
                self.sel = None;
                self.checkpoint();
                self.base = Some(self.bm.clone());
                self.stroke = Some(Stroke::Shape { start: p });
                self.shape_to(p);
            }
            Tool::Fill => {
                self.settle();
                self.sel = None;
                self.checkpoint();
                flood_fill(&mut self.bm, p.0, p.1, if ink { self.pattern } else { Pattern(0) });
            }
            Tool::Text => {
                self.settle();
                self.sel = None;
                self.text = Some(TextEdit { x: p.0, y: p.1, buf: String::new(), base: self.bm.clone(), dirty: false });
            }
            Tool::Select => {
                let inside = self.sel.as_ref().map_or(false, |s| p.0 >= s.r.0 && p.1 >= s.r.1 && p.0 < s.r.0 + s.r.2 as i32 && p.1 < s.r.1 + s.r.3 as i32);
                if inside {
                    self.lift();
                    let s = self.sel.as_ref().unwrap();
                    self.stroke = Some(Stroke::Move { grab: (p.0 - s.r.0, p.1 - s.r.1) });
                } else {
                    self.drop_selection();
                    self.stroke = Some(Stroke::Marquee { start: p });
                }
            }
            Tool::Hand => self.stroke = Some(Stroke::Pan),
        }
    }

    /// Pick up the selected pixels (they leave a hole of paper behind).
    fn lift(&mut self) {
        let Some(s) = self.sel.as_mut() else { return };
        if s.float.is_some() {
            return;
        }
        let (x, y, w, h) = s.r;
        let f = self.bm.crop(x, y, w, h);
        self.hist.push(&self.bm);
        self.dirty = true;
        self.bm.fill_rect(x, y, w, h, false);
        s.float = Some(f);
    }

    fn stamp(&mut self, p: (i32, i32)) {
        let (size, ink) = match self.tool {
            Tool::Pencil => (1, self.ink()),
            Tool::Eraser => ((self.brush_size() * 2).max(6), false),
            _ => (self.brush_size(), self.ink()),
        };
        self.bm.dab(p.0, p.1, size, ink);
    }

    fn shape_to(&mut self, p: (i32, i32)) {
        let (Some(Stroke::Shape { start }), Some(base)) = (self.stroke, self.base.as_ref()) else { return };
        self.bm.copy_from(base);
        let ink = self.ink();
        let size = self.brush_size();
        let mut end = p;
        if self.constrain {
            end = if self.tool == Tool::Line {
                snap45(start.0, start.1, p.0, p.1)
            } else {
                let d = (p.0 - start.0).abs().max((p.1 - start.1).abs());
                (start.0 + d * (p.0 - start.0).signum(), start.1 + d * (p.1 - start.1).signum())
            };
        }
        match self.tool {
            Tool::Line => draw_line(&mut self.bm, start.0, start.1, end.0, end.1, size, ink),
            t => {
                let shape = match t {
                    Tool::Rect => Shape::Rect,
                    Tool::RoundRect => Shape::RoundRect,
                    _ => Shape::Ellipse,
                };
                let fill = if self.filled { Some(self.pattern) } else { None };
                draw_shape(&mut self.bm, shape, bbox(start.0, start.1, end.0, end.1), fill, size, ink);
            }
        }
    }

    fn drag(&mut self, p: (i32, i32)) {
        let ink = self.ink();
        match self.stroke {
            Some(Stroke::Freehand { last }) => {
                for q in line_points(last.0, last.1, p.0, p.1).into_iter().skip(1) {
                    self.stamp(q);
                }
                self.stroke = Some(Stroke::Freehand { last: p });
            }
            Some(Stroke::Spray { last }) => {
                for q in line_points(last.0, last.1, p.0, p.1).into_iter().skip(1).step_by(2) {
                    spray(&mut self.bm, q.0, q.1, 7, 10, ink, &mut self.seed);
                }
                spray(&mut self.bm, p.0, p.1, 7, 8, ink, &mut self.seed);
                self.stroke = Some(Stroke::Spray { last: p });
            }
            Some(Stroke::Shape { .. }) => self.shape_to(p),
            Some(Stroke::Marquee { start }) => {
                let (x0, y0, x1, y1) = bbox(start.0, start.1, p.0, p.1);
                let (x0, y0) = (x0.max(0), y0.max(0));
                let (x1, y1) = (x1.min(self.bm.w as i32 - 1), y1.min(self.bm.h as i32 - 1));
                self.sel = Some(Selection { r: (x0, y0, (x1 - x0 + 1).max(0) as usize, (y1 - y0 + 1).max(0) as usize), float: None });
            }
            Some(Stroke::Move { grab }) => {
                if let Some(s) = self.sel.as_mut() {
                    s.r.0 = p.0 - grab.0;
                    s.r.1 = p.1 - grab.1;
                }
            }
            _ => {}
        }
    }

    fn end(&mut self) {
        match self.stroke.take() {
            Some(Stroke::Shape { .. }) => self.base = None,
            Some(Stroke::Marquee { .. }) => {
                if self.sel.as_ref().map_or(false, |s| s.r.2 < 2 && s.r.3 < 2) {
                    self.sel = None;
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ selection and clipboard

    fn select_all(&mut self) {
        self.settle();
        self.tool = Tool::Select;
        self.sel = Some(Selection { r: (0, 0, self.bm.w, self.bm.h), float: None });
    }

    fn copy(&mut self) -> bool {
        match &self.sel {
            Some(s) => {
                self.clip = Some(match &s.float {
                    Some(f) => f.clone(),
                    None => self.bm.crop(s.r.0, s.r.1, s.r.2, s.r.3),
                });
                true
            }
            None => false,
        }
    }

    fn cut(&mut self) -> bool {
        if !self.copy() {
            return false;
        }
        self.delete_selection();
        true
    }

    fn delete_selection(&mut self) {
        let Some(s) = self.sel.as_mut() else { return };
        if s.float.take().is_some() {
            self.sel = None;
            return;
        }
        let (x, y, w, h) = s.r;
        self.hist.push(&self.bm);
        self.dirty = true;
        self.bm.fill_rect(x, y, w, h, false);
        self.sel = None;
    }

    fn paste(&mut self) -> bool {
        let Some(c) = self.clip.clone() else { return false };
        self.settle();
        self.hist.push(&self.bm);
        self.dirty = true;
        self.tool = Tool::Select;
        self.sel = Some(Selection { r: (self.sx, self.sy, c.w, c.h), float: Some(c) });
        true
    }

    // ------------------------------------------------------------------ files

    fn unique_name(&self) -> String {
        (1..)
            .map(|n| format!("Untitled {n}"))
            .find(|n| !self.dir.join(format!("{n}.png")).exists())
            .unwrap()
    }

    fn file_name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled".into())
    }

    fn save(&mut self, then: Pending) -> AppAction {
        self.settle();
        match self.path.clone() {
            Some(p) => self.write(&p, then),
            None => self.save_as(then),
        }
    }

    fn save_as(&mut self, then: Pending) -> AppAction {
        self.settle();
        let name = self.path.as_ref().map(|_| self.file_name()).unwrap_or_else(|| self.unique_name());
        self.modal = Some(Modal::Name { p: NamePrompt::new("Save as:", &name), then });
        AppAction::None
    }

    fn write(&mut self, path: &Path, then: Pending) -> AppAction {
        let res = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(path, self.bm.to_png()));
        match res {
            Ok(()) => {
                self.path = Some(path.to_path_buf());
                self.dirty = false;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if self.run_pending(then) {
                    return AppAction::Quit;
                }
                if self.modal.is_none() {
                    return AppAction::Toast(crate::launcher::i18n::tr_s(&format!("Saved {name}")));
                }
                AppAction::None
            }
            Err(e) => {
                self.modal = Some(Modal::Error(Alert::new(AlertIcon::Stop, "Cannot save", &e.to_string(), &["OK"])));
                AppAction::None
            }
        }
    }

    /// Do what was waiting for a save / "Don't Save". True when that is leaving the app.
    fn run_pending(&mut self, then: Pending) -> bool {
        match then {
            Pending::None => {}
            Pending::New => self.do_new(),
            Pending::Open => self.do_open(),
            Pending::Quit => return true,
        }
        false
    }

    fn do_new(&mut self) {
        self.settle();
        self.bm = Bitmap::new(SHEET_W, SHEET_H);
        self.hist = History::new(20);
        self.sel = None;
        self.path = None;
        self.dirty = false;
        self.sx = 0;
        self.sy = 0;
    }

    fn guard(&mut self, then: Pending) {
        self.settle();
        if self.dirty {
            self.modal = Some(Modal::Unsaved { c: UnsavedCard::new(&self.file_name()), then });
        } else {
            self.run_pending(then);
        }
    }

    fn do_open(&mut self) {
        let files = crate::launcher::apps::imageviewer::scan(&self.dir);
        if files.is_empty() {
            self.modal = Some(Modal::Error(Alert::new(AlertIcon::Note, "No pictures", &format!("There are no pictures in {} to open.", crate::launcher::fileops::tilde(&self.dir)), &["OK"])));
            return;
        }
        let rows = files.iter().map(|p| p.strip_prefix(&self.dir).unwrap_or(p).to_string_lossy().into_owned()).collect();
        self.modal = Some(Modal::Pick { p: ListPicker::new("Open picture:", rows), files });
    }

    pub fn open_path(&mut self, path: &Path) -> Result<(), String> {
        let surf = Surface::from_file(path)?.convert_format(PixelFormatEnum::RGB24)?;
        let (w, h, pitch) = (surf.width() as usize, surf.height() as usize, surf.pitch() as usize);
        if w > 1024 || h > 1024 {
            return Err(format!("{w} x {h} is larger than the 1024 x 1024 limit"));
        }
        let mut rgb = Vec::with_capacity(w * h * 3);
        surf.with_lock(|px| {
            for y in 0..h {
                rgb.extend_from_slice(&px[y * pitch..y * pitch + w * 3]);
            }
        });
        self.settle();
        self.bm = Bitmap::from_rgb_luma(&rgb, w, h);
        self.hist = History::new(20);
        self.sel = None;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        self.sx = 0;
        self.sy = 0;
        self.clamp_scroll();
        Ok(())
    }

    // ------------------------------------------------------------------ modals

    fn modal_key(&mut self, key: Keycode) -> AppAction {
        let Some(m) = self.modal.take() else { return AppAction::None };
        match m {
            Modal::Error(mut a) => {
                if a.key(key).is_none() {
                    self.modal = Some(Modal::Error(a));
                }
            }
            Modal::Name { mut p, then } => match p.key(key) {
                Some(r) => return self.name_result(r, then),
                None => self.modal = Some(Modal::Name { p, then }),
            },
            Modal::Replace { mut c, path, then } => match c.key(key) {
                Some(r) => return self.replace_result(r, path, then),
                None => self.modal = Some(Modal::Replace { c, path, then }),
            },
            Modal::Unsaved { mut c, then } => match c.key(key) {
                Some(r) => return self.unsaved_result(r, then),
                None => self.modal = Some(Modal::Unsaved { c, then }),
            },
            Modal::Pick { mut p, files } => match p.key(key) {
                Some(r) => self.pick_result(r, &files),
                None => self.modal = Some(Modal::Pick { p, files }),
            },
        }
        AppAction::None
    }

    fn modal_click(&mut self, x: i32, y: i32, ctx: &Ctx) -> AppAction {
        let Some(m) = self.modal.take() else { return AppAction::None };
        match m {
            Modal::Error(mut a) => {
                if a.click(ctx.fs, x, y).is_none() {
                    self.modal = Some(Modal::Error(a));
                }
            }
            Modal::Name { mut p, then } => match p.click(x, y) {
                Some(r) => return self.name_result(r, then),
                None => self.modal = Some(Modal::Name { p, then }),
            },
            Modal::Replace { mut c, path, then } => match c.click(x, y) {
                Some(r) => return self.replace_result(r, path, then),
                None => self.modal = Some(Modal::Replace { c, path, then }),
            },
            Modal::Unsaved { mut c, then } => match c.click(x, y) {
                Some(r) => return self.unsaved_result(r, then),
                None => self.modal = Some(Modal::Unsaved { c, then }),
            },
            Modal::Pick { mut p, files } => match p.click(x, y) {
                Some(r) => self.pick_result(r, &files),
                None => self.modal = Some(Modal::Pick { p, files }),
            },
        }
        AppAction::None
    }

    fn name_result(&mut self, r: PromptResult, then: Pending) -> AppAction {
        let PromptResult::Ok(name) = r else { return AppAction::None };
        let name: String = name.chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '\0')).collect();
        let name = name.trim().trim_start_matches('.').to_string();
        if name.is_empty() {
            return AppAction::None;
        }
        let file = if name.to_ascii_lowercase().ends_with(".png") { name } else { format!("{name}.png") };
        let path = self.dir.join(file);
        if path.exists() && self.path.as_deref() != Some(path.as_path()) {
            let n = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.modal = Some(Modal::Replace { c: ConfirmAlert::new(vec![format!("Replace {n}?"), "A file with that name already exists.".into()], "Replace", false), path, then });
            return AppAction::None;
        }
        self.write(&path, then)
    }

    fn replace_result(&mut self, r: ConfirmChoice, path: PathBuf, then: Pending) -> AppAction {
        if r == ConfirmChoice::Ok {
            return self.write(&path, then);
        }
        AppAction::None
    }

    fn unsaved_result(&mut self, r: CardChoice, then: Pending) -> AppAction {
        match r {
            CardChoice::DontSave => {
                self.dirty = false;
                if self.run_pending(then) {
                    return AppAction::Quit;
                }
            }
            CardChoice::Save => return self.save(then),
            CardChoice::Cancel => {}
        }
        AppAction::None
    }

    fn pick_result(&mut self, r: PickResult, files: &[PathBuf]) {
        if let PickResult::Choose(i) = r {
            if let Err(e) = self.open_path(&files[i]) {
                self.modal = Some(Modal::Error(Alert::new(AlertIcon::Stop, "Cannot open", &e, &["OK"])));
            }
        }
    }

    // ------------------------------------------------------------------ menus

    fn run_command(&mut self, id: &str) -> AppAction {
        if let Some(t) = TOOLS.iter().find(|t| t.1 == id) {
            self.set_tool(t.0);
            return AppAction::None;
        }
        match id {
            "new" => self.guard(Pending::New),
            "open" => self.guard(Pending::Open),
            "save" => return self.save(Pending::None),
            "saveas" => return self.save_as(Pending::None),
            "undo" => {
                if !self.undo() {
                    return AppAction::Toast(crate::launcher::i18n::tr_s("Nothing to undo"));
                }
            }
            "redo" => {
                if !self.redo() {
                    return AppAction::Toast(crate::launcher::i18n::tr_s("Nothing to redo"));
                }
            }
            "clear" => {
                self.settle();
                self.sel = None;
                self.checkpoint();
                self.bm.clear();
            }
            "invert" => {
                self.settle();
                self.checkpoint();
                self.bm.invert();
            }
            "selall" => self.select_all(),
            "cut" => {
                if !self.cut() {
                    return AppAction::Toast(crate::launcher::i18n::tr_s("Select something first"));
                }
            }
            "copy" => {
                if !self.copy() {
                    return AppAction::Toast(crate::launcher::i18n::tr_s("Select something first"));
                }
            }
            "paste" => {
                if !self.paste() {
                    return AppAction::Toast(crate::launcher::i18n::tr_s("Nothing to paste"));
                }
            }
            "z1" => self.set_zoom(1),
            "z2" => self.set_zoom(2),
            "z4" => self.set_zoom(4),
            "zin" => self.set_zoom(ZOOMS.iter().copied().find(|z| *z > self.zoom).unwrap_or(self.zoom)),
            "zout" => self.set_zoom(ZOOMS.iter().rev().copied().find(|z| *z < self.zoom).unwrap_or(self.zoom)),
            "pattern" => self.popup = Some(self.pattern.0 as usize),
            "size" => self.size_i = (self.size_i + 1) % SIZES.len(),
            "filled" => self.filled = !self.filled,
            "paper" => self.paper_pen = !self.paper_pen,
            "snap" => self.constrain = !self.constrain,
            _ => {}
        }
        AppAction::None
    }

    fn pop_key(&mut self, key: Keycode) {
        let Some(i) = self.popup else { return };
        let n = PATTERN_COUNT as usize;
        match crate::platform::keys::arrow_alias(key) {
            Keycode::Escape | Keycode::G => self.popup = None,
            Keycode::Left => self.popup = Some((i + n - 1) % n),
            Keycode::Right => self.popup = Some((i + 1) % n),
            Keycode::Up => self.popup = Some((i + n - 4) % n),
            Keycode::Down => self.popup = Some((i + 4) % n),
            Keycode::Return | Keycode::KpEnter | Keycode::Space => {
                self.pattern = Pattern(i as u8);
                self.popup = None;
            }
            _ => {}
        }
    }

    fn strip_click(&mut self, x: i32, y: i32) -> bool {
        for i in 0..TOOLS.len() {
            if contains(tool_cell(i), x, y) {
                self.set_tool(TOOLS[i].0);
                return true;
            }
        }
        if contains(pattern_cell(), x, y) {
            self.popup = Some(self.pattern.0 as usize);
            return true;
        }
        if contains(size_cell(), x, y) {
            self.run_command("size");
            return true;
        }
        if contains(fill_cell(), x, y) {
            self.filled = !self.filled;
            return true;
        }
        x < STRIP_W
    }

    fn vbar(&self) -> ScrollBar {
        ScrollBar::new(Rect::new(VBAR_X, VY, grid::SCROLL_W as u32, VH as u32), self.bm.h, (VH / self.zoom) as usize, self.sy as usize)
    }

    fn hbar_rect() -> Rect {
        Rect::new(VX, grid::APP_FRAME_BOTTOM - 4, VW as u32, 4)
    }

    fn hbar_set(&mut self, x: i32) {
        let (mx, _) = self.max_scroll();
        self.sx = ((x - VX) * mx / VW.max(1)).clamp(0, mx);
    }

    // ------------------------------------------------------------------ keyboard cursor

    fn kb_pen(&mut self) {
        self.kb = true;
        if self.stroke.is_some() {
            self.end();
        } else {
            let c = self.cursor;
            self.begin(c);
            if self.tool == Tool::Text || self.tool == Tool::Fill {
                self.stroke = None;
            }
        }
    }

    fn kb_move(&mut self, dx: i32, dy: i32) {
        self.kb = true;
        self.cursor.0 = (self.cursor.0 + dx).clamp(0, self.bm.w as i32 - 1);
        self.cursor.1 = (self.cursor.1 + dy).clamp(0, self.bm.h as i32 - 1);
        self.follow_cursor();
        if self.stroke == Some(Stroke::Pan) {
            self.sx += dx;
            self.sy += dy;
            self.clamp_scroll();
            return;
        }
        if self.stroke.is_some() {
            let c = self.cursor;
            self.drag(c);
        }
    }

    fn picked_tool(key: Keycode) -> Option<Tool> {
        Some(match key {
            Keycode::P => Tool::Pencil,
            Keycode::B => Tool::Brush,
            Keycode::E => Tool::Eraser,
            Keycode::H => Tool::Hand,
            Keycode::L => Tool::Line,
            Keycode::R => Tool::Rect,
            Keycode::Q => Tool::RoundRect,
            Keycode::O => Tool::Ellipse,
            Keycode::F => Tool::Fill,
            Keycode::Y => Tool::Spray,
            Keycode::T => Tool::Text,
            Keycode::M => Tool::Select,
            _ => return None,
        })
    }

    fn text_key(&mut self, key: Keycode) {
        match key {
            Keycode::Escape => self.cancel_text(),
            Keycode::Return | Keycode::KpEnter => self.commit_text(),
            Keycode::Backspace => {
                if let Some(t) = self.text.as_mut() {
                    t.buf.pop();
                    t.dirty = true;
                }
            }
            _ => {}
        }
    }
}

impl LauncherApp for Paint {
    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Arrows", "Move the cursor (Shift: 8 px)"),
            k("Space", "Pen down / up"),
            k("P / B / E / Y", "Pencil / brush / eraser / spray"),
            k("L / R / Q / O", "Line / rectangle / rounded / ellipse"),
            k("F / T / M / H", "Bucket / text / select / hand"),
            k("G", "Pattern"),
            k("[ / ]", "Brush size"),
            k("1 / 2 / 4", "Zoom 1x / 2x / 4x"),
            k("Ctrl+Z / Ctrl+Y", "Undo / redo"),
        ]
    }

    fn title(&self) -> String {
        crate::launcher::i18n::app_name("paint")
    }

    /// Hold Esc / menu Quit with unsaved changes: "Save changes?" first (as New and Open do).
    fn request_close(&mut self) -> bool {
        self.settle();
        if !self.dirty {
            return true;
        }
        if self.modal.is_none() {
            self.modal = Some(Modal::Unsaved { c: UnsavedCard::new(&self.file_name()), then: Pending::Quit });
        }
        false
    }

    /// "Edit in Paint" / Files > Open With: load the picture into the canvas.
    fn open_path(&mut self, path: &std::path::Path) -> Result<(), String> {
        Paint::open_path(self, path)
    }

    fn menu(&self) -> Vec<MenuItem> {
        vec![
            MenuItem::new("new", "New"),
            MenuItem::new("open", "Open..."),
            MenuItem::new("save", "Save"),
            MenuItem::new("saveas", "Save As..."),
            MenuItem::new("undo", "Undo"),
            MenuItem::new("redo", "Redo"),
            MenuItem::new("cut", "Cut"),
            MenuItem::new("copy", "Copy"),
            MenuItem::new("paste", "Paste"),
            MenuItem::new("selall", "Select All"),
            MenuItem::new("clear", "Clear Sheet"),
            MenuItem::new("invert", "Invert"),
            MenuItem::new("z1", "Zoom 1x"),
            MenuItem::new("z2", "Zoom 2x"),
            MenuItem::new("z4", "Zoom 4x"),
        ]
    }

    fn context_menu(&mut self, x: i32, _y: i32, _ctx: &Ctx) -> Option<Vec<MenuItem>> {
        if self.modal.is_some() || self.popup.is_some() {
            return None;
        }
        if x < STRIP_W {
            return Some(TOOLS.iter().map(|t| MenuItem::new(t.1, t.2)).collect());
        }
        Some(vec![
            MenuItem::new("pattern", format!("Pattern: {}...", crate::launcher::i18n::tr(self.pattern.name()))),
            MenuItem::new("size", format!("Brush size: {}", self.brush_size())),
            MenuItem::new("filled", if self.filled { "Shapes: filled" } else { "Shapes: outline" }),
            MenuItem::new("paper", if self.paper_pen { "Pen: white" } else { "Pen: black" }),
            MenuItem::new("snap", if self.constrain { "Constrain: on" } else { "Constrain: off" }),
            MenuItem::new("undo", "Undo"),
            MenuItem::new("redo", "Redo"),
            MenuItem::new("zin", "Zoom in"),
            MenuItem::new("zout", "Zoom out"),
        ])
    }

    fn menu_action(&mut self, id: &str, _ctx: &Ctx) -> AppAction {
        self.run_command(id)
    }

    fn wants_tab(&self) -> bool {
        false
    }

    fn describe(&self) -> String {
        format!(
            "Paint: {} tool, pattern {}, {} x {} sheet, zoom {}x, {}{}",
            tool_name(self.tool),
            self.pattern.name(),
            self.bm.w,
            self.bm.h,
            self.zoom,
            self.file_name(),
            if self.dirty { " (modified)" } else { "" }
        )
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, _ctx: &Ctx) -> AppAction {
        if self.modal.is_some() {
            return self.modal_key(key);
        }
        if self.popup.is_some() {
            self.pop_key(key);
            return AppAction::None;
        }
        let ctrl = mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
        let shift = mods.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
        if self.text.is_some() && !ctrl {
            self.text_key(key);
            return AppAction::None;
        }
        if ctrl {
            let id = match key {
                Keycode::Z => "undo",
                Keycode::Y => "redo",
                Keycode::S if shift => "saveas",
                Keycode::S => "save",
                Keycode::O => "open",
                Keycode::N => "new",
                Keycode::A => "selall",
                Keycode::C => "copy",
                Keycode::X => "cut",
                Keycode::V => "paste",
                _ => return AppAction::None,
            };
            return self.run_command(id);
        }
        let step = if shift { 8 } else { 1 };
        match key {
            Keycode::Left => self.kb_move(-step, 0),
            Keycode::Right => self.kb_move(step, 0),
            Keycode::Up => self.kb_move(0, -step),
            Keycode::Down => self.kb_move(0, step),
            _ => match key {
                Keycode::Space => self.kb_pen(),
                Keycode::Escape => {
                    if self.stroke.is_some() {
                        // Cancel the stroke in progress: undo what it drew.
                        self.stroke = None;
                        self.base = None;
                        self.undo();
                    } else if self.sel.is_some() {
                        self.drop_selection();
                    }
                    self.kb = false;
                }
                Keycode::Return | Keycode::KpEnter => self.drop_selection(),
                Keycode::Delete | Keycode::Backspace => self.delete_selection(),
                Keycode::G => self.popup = Some(self.pattern.0 as usize),
                Keycode::V => self.filled = !self.filled,
                Keycode::X => self.paper_pen = !self.paper_pen,
                Keycode::LeftBracket => self.size_i = (self.size_i + SIZES.len() - 1) % SIZES.len(),
                Keycode::RightBracket => self.size_i = (self.size_i + 1) % SIZES.len(),
                Keycode::Num1 => self.set_zoom(1),
                Keycode::Num2 => self.set_zoom(2),
                Keycode::Num4 => self.set_zoom(4),
                Keycode::Equals | Keycode::Plus => { self.run_command("zin"); },
                Keycode::Minus => { self.run_command("zout"); },
                k => {
                    if let Some(t) = Self::picked_tool(k) {
                        self.set_tool(t);
                        return AppAction::Toast(crate::launcher::i18n::tr(tool_name(t)).into());
                    }
                }
            },
        }
        AppAction::None
    }

    fn text(&mut self, text: &str, _ctx: &Ctx) -> AppAction {
        if let Some(Modal::Name { p, .. }) = self.modal.as_mut() {
            p.text(text);
            return AppAction::None;
        }
        if self.modal.is_some() || self.popup.is_some() {
            return AppAction::None;
        }
        if let Some(t) = self.text.as_mut() {
            t.buf.extend(text.chars().filter(|c| !c.is_control()));
            t.dirty = true;
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> AppAction {
        if self.modal.is_some() {
            if let PointerEvent::Down { x, y, button: 0, .. } = ev {
                return self.modal_click(x, y, ctx);
            }
            if let (PointerEvent::Wheel { dy, .. }, Some(Modal::Pick { p, .. })) = (ev, self.modal.as_mut()) {
                p.wheel(dy);
            }
            return AppAction::None;
        }
        if self.popup.is_some() {
            if let PointerEvent::Down { x, y, button: 0, .. } = ev {
                match (0..PATTERN_COUNT as usize).find(|&i| contains(popup_cell(i), x, y)) {
                    Some(i) => {
                        self.pattern = Pattern(i as u8);
                        self.popup = None;
                    }
                    None if !contains(popup_rect(), x, y) => self.popup = None,
                    None => {}
                }
            }
            return AppAction::None;
        }
        match ev {
            PointerEvent::Down { x, y, button: 0, .. } => {
                self.kb = false;
                if x < STRIP_W {
                    self.strip_click(x, y);
                } else if let Some((off, drag)) = self.vbar().press(x, y) {
                    self.sy = off as i32;
                    self.sbar = drag.then_some(());
                    self.clamp_scroll();
                } else if contains(Self::hbar_rect(), x, y) {
                    self.hbar = true;
                    self.hbar_set(x);
                } else if Self::in_view(x, y) {
                    let p = self.to_sheet(x, y);
                    self.cursor = p;
                    self.begin(p);
                }
            }
            PointerEvent::Drag { x, y, dx, dy } => {
                if self.sbar.is_some() {
                    let off = self.vbar().offset_for_y(y);
                    self.sy = off as i32;
                    self.clamp_scroll();
                } else if self.hbar {
                    self.hbar_set(x);
                } else if self.stroke == Some(Stroke::Pan) {
                    self.sx -= dx / self.zoom;
                    self.sy -= dy / self.zoom;
                    self.clamp_scroll();
                } else if self.stroke.is_some() {
                    let p = self.to_sheet(x.clamp(VX, VX + VW - 1), y.clamp(VY, VY + VH - 1));
                    self.cursor = p;
                    self.drag(p);
                }
            }
            PointerEvent::Up { button: 0, .. } => {
                self.sbar = None;
                self.hbar = false;
                self.end();
            }
            PointerEvent::Wheel { dy, .. } => {
                self.sy -= dy.signum() * (24 / self.zoom).max(4);
                self.clamp_scroll();
            }
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, _ctx: &Ctx) -> bool {
        self.toast.take().is_some()
    }

    fn draw(&mut self, c: &mut Canvas<Window>, assets: &mut AssetStore, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let paper = tm.get_color("home_bg");
        let ink_bit = self.ink();

        // Text is rasterised with the launcher's own font before anything else is painted.
        if let Some(t) = self.text.as_mut().filter(|t| t.dirty) {
            t.dirty = false;
            self.bm.copy_from(&t.base);
            if !t.buf.is_empty() {
                let w = fs.measure_text(&t.buf).clamp(1, 300);
                let r = Rect::new(0, grid::BAR_H, w as u32, 16);
                c.set_draw_color(Color::RGB(0, 0, 0));
                let _ = c.fill_rect(r);
                fs.draw_text(c, 0, grid::BAR_H, &t.buf, Color::RGB(255, 255, 255));
                if let Ok(px) = c.read_pixels(r, PixelFormatEnum::RGB24) {
                    for yy in 0..16usize {
                        for xx in 0..w as usize {
                            if px[(yy * w as usize + xx) * 3] > 127 {
                                self.bm.set(t.x + xx as i32, t.y + yy as i32, ink_bit);
                            }
                        }
                    }
                }
            }
        }

        // Backdrop and tool strip.
        ui::fill_rect(c, Rect::new(0, grid::BAR_H, grid::SCREEN_W as u32, (grid::SCREEN_H - grid::BAR_H) as u32), paper);
        self.draw_strip(c, ctx, assets, ink, paper);
        c.set_draw_color(ink);
        let _ = c.draw_line((STRIP_W - 1, STRIP_Y + 1), (STRIP_W - 1, grid::APP_FRAME_BOTTOM));
        ui::draw_rect(c, frame_rect(), ink);

        // The sheet.
        let view = Rect::new(VX, VY, VW as u32, VH as u32);
        c.set_clip_rect(view);
        // The canvas is the theme's white (a lighter sheet than the desktop around it). Invert themes
        // lift "icon_white" to a dark tone; there the plain light tone is used.
        let sheet = {
            let t = widgets::tones(tm);
            let lum = |c: Color| c.r as u32 * 3 + c.g as u32 * 6 + c.b as u32;
            let white = tm.get_color("icon_white");
            if lum(white) * 2 >= lum(t.light) + lum(t.dark) { white } else { t.light }
        };
        // Drawn pixels use the dark tone: on Invert themes the foreground is light and would vanish on it.
        let pen = widgets::tones(tm).dark;
        self.draw_sheet(c, pen, sheet, tm.get_color("line"));
        self.draw_overlays(c, pen, sheet);
        c.set_clip_rect(None);

        // Scroll bars.
        self.vbar().draw(c, tm, if self.sbar.is_some() { Some(widgets::SbPart::Thumb) } else { None });
        let hb = Self::hbar_rect();
        ui::fill_rect(c, hb, tm.get_color("surface_sunken"));
        c.set_draw_color(ink);
        let _ = c.draw_rect(hb);
        let (mx, _) = self.max_scroll();
        if mx > 0 {
            let len = (VW * (VW / self.zoom) / self.bm.w as i32).clamp(8, VW);
            let x = VX + (VW - len) * self.sx / mx;
            ui::fill_rect(c, Rect::new(x, hb.y(), len as u32, 4), ink);
        }

        if self.popup.is_some() {
            self.draw_popup(c, ctx, ink, paper);
        }
        match &self.modal {
            Some(Modal::Name { p, .. }) => p.draw(c, fs, tm),
            Some(Modal::Replace { c: a, .. }) => a.draw(c, fs, tm),
            Some(Modal::Unsaved { c: a, .. }) => a.draw(c, fs, tm),
            Some(Modal::Pick { p, .. }) => p.draw(c, fs, tm),
            Some(Modal::Error(a)) => a.draw(c, fs, tm),
            None => {}
        }
    }
}

impl Paint {
    fn sample(&self, x: i32, y: i32) -> bool {
        if let Some(s) = &self.sel {
            if let Some(f) = &s.float {
                let (fx, fy) = (x - s.r.0, y - s.r.1);
                if fx >= 0 && fy >= 0 && (fx as usize) < f.w && (fy as usize) < f.h {
                    return f.get(fx, fy);
                }
            }
        }
        self.bm.get(x, y)
    }

    fn draw_sheet(&self, c: &mut Canvas<Window>, ink: Color, paper: Color, grid_col: Color) {
        let z = self.zoom;
        let (cols, rows) = ((VW / z + 1).min(self.bm.w as i32 - self.sx), (VH / z + 1).min(self.bm.h as i32 - self.sy));
        c.set_draw_color(paper);
        let _ = c.fill_rect(Rect::new(VX, VY, VW as u32, VH as u32));
        for ry in 0..rows {
            let sy = self.sy + ry;
            let mut rx = 0;
            while rx < cols {
                if self.sample(self.sx + rx, sy) {
                    let start = rx;
                    while rx < cols && self.sample(self.sx + rx, sy) {
                        rx += 1;
                    }
                    c.set_draw_color(ink);
                    let _ = c.fill_rect(Rect::new(VX + start * z, VY + ry * z, ((rx - start) * z) as u32, z as u32));
                } else {
                    if z == 4 {
                        c.set_draw_color(grid_col);
                        let _ = c.draw_point((VX + rx * z, VY + ry * z));
                    }
                    rx += 1;
                }
            }
        }
        // The sheet's border where it ends inside the window.
        let (ex, ey) = (VX + (self.bm.w as i32 - self.sx) * z, VY + (self.bm.h as i32 - self.sy) * z);
        c.set_draw_color(ink);
        // They stop 1 px short of the view's edges so they never run into the frame.
        if ex < VX + VW {
            let _ = c.draw_line((ex, VY + 1), (ex, ey.min(VY + VH - 2)));
        }
        if ey < VY + VH {
            let _ = c.draw_line((VX + 1, ey), (ex.min(VX + VW - 2), ey));
        }
    }

    fn to_screen(&self, x: i32, y: i32) -> (i32, i32) {
        (VX + (x - self.sx) * self.zoom, VY + (y - self.sy) * self.zoom)
    }

    fn draw_overlays(&self, c: &mut Canvas<Window>, ink: Color, paper: Color) {
        let z = self.zoom;
        if let Some(s) = &self.sel {
            let (x0, y0) = self.to_screen(s.r.0, s.r.1);
            let (w, h) = (s.r.2 as i32 * z, s.r.3 as i32 * z);
            // Dashes: two pixels of ink, two of paper, around the selection.
            let mut dash = |x: i32, y: i32, i: i32| {
                c.set_draw_color(if (i / 2) % 2 == 0 { ink } else { paper });
                let _ = c.draw_point((x, y));
            };
            for i in 0..=w {
                dash(x0 + i, y0 - 1, i);
                dash(x0 + i, y0 + h, i);
            }
            for i in 0..=h {
                dash(x0 - 1, y0 + i, i);
                dash(x0 + w, y0 + i, i);
            }
        }
        if let Some(t) = &self.text {
            // Caret after the text.
            let w = t.buf.chars().count() as i32 * 6;
            let (x, y) = self.to_screen(t.x, t.y);
            c.set_draw_color(ink);
            let _ = c.draw_line((x + w * z, y), (x + w * z, y + 12 * z));
        }
        if self.kb {
            let (x, y) = self.to_screen(self.cursor.0, self.cursor.1);
            let (x, y) = (x + z / 2, y + z / 2);
            for (col, off) in [(paper, 1), (ink, 0)] {
                c.set_draw_color(col);
                let _ = c.draw_line((x - 5 - off, y + off), (x - 2 + off, y + off));
                let _ = c.draw_line((x + 2 - off, y + off), (x + 5 + off, y + off));
                let _ = c.draw_line((x + off, y - 5 - off), (x + off, y - 2 + off));
                let _ = c.draw_line((x + off, y + 2 - off), (x + off, y + 5 + off));
            }
        }
    }

    fn draw_strip(&self, c: &mut Canvas<Window>, ctx: &Ctx, assets: &mut AssetStore, ink: Color, paper: Color) {
        let sel = widgets::selection(ctx.tm);
        for (i, t) in TOOLS.iter().enumerate() {
            let r = tool_cell(i);
            let on = t.0 == self.tool;
            let (fg, bg) = if on { (sel.fg, sel.bg) } else { (ink, paper) };
            ui::fill_rect(c, r, bg);
            c.set_draw_color(ink);
            let _ = c.draw_rect(r);
            // A drawn icon `apps/paint/res/tools/<id>.png` (made from the atlas with
            // tools/paint_icons_split.py, see docs/PAINT-ICONS.md): black is the ink, white is the cell's
            // paper, transparent shows the cell through. A tool without one keeps the built-in glyph.
            let key = format!("{}/tools/{}", crate::launcher::packages::res_key("paint"), t.1);
            match assets.size(&key) {
                Some((w, h)) => {
                    let dst = Rect::new(r.x() + (CELL_W - w as i32) / 2, r.y() + (CELL_H - h as i32) / 2, w, h);
                    let pal = crate::assets::IconPalette { ink: fg, muted: None, white: Some(bg) };
                    let _ = assets.blit_icon(c, &key, dst, pal);
                }
                None => self.draw_glyph(c, ctx, t.0, r.x() + 3, r.y() + (CELL_H - 12) / 2, fg),
            }
        }
        // Pattern swatch.
        let pc = pattern_cell();
        c.set_draw_color(ink);
        let _ = c.draw_rect(pc);
        for y in pc.y() + 2..pc.y() + pc.height() as i32 - 2 {
            for x in pc.x() + 2..pc.x() + pc.width() as i32 - 2 {
                c.set_draw_color(if self.pattern.lit(x, y) { ink } else { paper });
                let _ = c.draw_point((x, y));
            }
        }
        // Brush size: a square of that size.
        let sc = size_cell();
        c.set_draw_color(ink);
        let _ = c.draw_rect(sc);
        let s = self.brush_size();
        ui::fill_rect(c, Rect::new(sc.x() + (CELL_W - s) / 2, sc.y() + (SWATCH_H - s) / 2, s as u32, s as u32), ink);
        // Filled / outline.
        let fc = fill_cell();
        c.set_draw_color(ink);
        let _ = c.draw_rect(fc);
        let inner = Rect::new(fc.x() + 4, fc.y() + (SWATCH_H - 8) / 2, 10, 8);
        if self.filled {
            ui::fill_rect(c, inner, ink);
        } else {
            let _ = c.draw_rect(inner);
        }
        // Pen colour marker: a small square in the corner of the size cell.
        if self.paper_pen {
            c.set_draw_color(ink);
            let _ = c.draw_rect(Rect::new(sc.x() + 2, sc.y() + 2, 4, 4));
        } else {
            ui::fill_rect(c, Rect::new(sc.x() + 2, sc.y() + 2, 4, 4), ink);
        }
    }

    /// A 12 x 12 glyph for a tool, built from rectangles and lines.
    fn draw_glyph(&self, c: &mut Canvas<Window>, ctx: &Ctx, t: Tool, x: i32, y: i32, col: Color) {
        c.set_draw_color(col);
        let line = |c: &mut Canvas<Window>, a: (i32, i32), b: (i32, i32)| {
            let _ = c.draw_line((x + a.0, y + a.1), (x + b.0, y + b.1));
        };
        let rect = |c: &mut Canvas<Window>, a: i32, b: i32, w: u32, h: u32, fill: bool| {
            let r = Rect::new(x + a, y + b, w, h);
            let _ = if fill { c.fill_rect(r) } else { c.draw_rect(r) };
        };
        match t {
            Tool::Pencil => {
                line(c, (2, 10), (9, 3));
                line(c, (3, 10), (10, 3));
                rect(c, 9, 1, 3, 3, false);
                let _ = c.draw_point((x + 1, y + 11));
            }
            Tool::Brush => {
                rect(c, 3, 4, 6, 4, true);
                rect(c, 4, 3, 4, 6, true);
                line(c, (6, 9), (6, 11));
            }
            Tool::Eraser => {
                rect(c, 1, 3, 10, 7, false);
                rect(c, 1, 3, 4, 7, true);
            }
            Tool::Hand => {
                line(c, (0, 6), (11, 6));
                line(c, (6, 0), (6, 11));
                line(c, (0, 6), (2, 4));
                line(c, (0, 6), (2, 8));
                line(c, (11, 6), (9, 4));
                line(c, (11, 6), (9, 8));
                line(c, (6, 0), (4, 2));
                line(c, (6, 0), (8, 2));
                line(c, (6, 11), (4, 9));
                line(c, (6, 11), (8, 9));
            }
            Tool::Line => line(c, (1, 10), (10, 1)),
            Tool::Rect => rect(c, 1, 2, 10, 8, false),
            Tool::RoundRect => {
                line(c, (3, 2), (8, 2));
                line(c, (3, 9), (8, 9));
                line(c, (1, 4), (1, 7));
                line(c, (10, 4), (10, 7));
                for (a, b) in [(2, 3), (9, 3), (2, 8), (9, 8)] {
                    let _ = c.draw_point((x + a, y + b));
                }
            }
            Tool::Ellipse => {
                for (px, py) in shape_outline(Shape::Ellipse, (0, 0, 10, 7)) {
                    let _ = c.draw_point((x + 1 + px, y + 2 + py));
                }
            }
            Tool::Fill => {
                rect(c, 2, 4, 7, 6, false);
                line(c, (2, 4), (5, 1));
                line(c, (5, 1), (8, 4));
                rect(c, 10, 7, 2, 3, true);
            }
            Tool::Spray => {
                for (a, b) in [(2, 8), (4, 6), (3, 3), (6, 4), (8, 2), (7, 7), (10, 5), (5, 10), (9, 9), (1, 5)] {
                    let _ = c.draw_point((x + a, y + b));
                }
            }
            Tool::Text => ctx.fs.draw_text(c, x + 2, y - 1, "A", col),
            Tool::Select => {
                for i in (0..11).step_by(2) {
                    for (a, b) in [(i, 1), (i, 10), (1, i), (10, i)] {
                        let _ = c.draw_point((x + a, y + b));
                    }
                }
            }
        }
    }

    fn draw_popup(&self, c: &mut Canvas<Window>, ctx: &Ctx, ink: Color, paper: Color) {
        let r = popup_rect();
        draw_panel(c, ctx.tm, r);
        let cur = self.popup.unwrap_or(0);
        for i in 0..PATTERN_COUNT as usize {
            let cell = popup_cell(i);
            c.set_draw_color(ink);
            let _ = c.draw_rect(cell);
            for y in cell.y() + 2..cell.y() + cell.height() as i32 - 2 {
                for x in cell.x() + 2..cell.x() + cell.width() as i32 - 2 {
                    c.set_draw_color(if Pattern(i as u8).lit(x, y) { ink } else { paper });
                    let _ = c.draw_point((x, y));
                }
            }
            if i == cur {
                let _ = c.draw_rect(Rect::new(cell.x() - 2, cell.y() - 2, cell.width() + 4, cell.height() + 4));
                let _ = c.draw_rect(Rect::new(cell.x() - 1, cell.y() - 1, cell.width() + 2, cell.height() + 2));
            }
        }
        ctx.fs.draw_text(c, r.x() + 6, r.y() + 4 + 40 + 1, crate::launcher::i18n::tr(Pattern(cur as u8).name()), ink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sheet_frame_and_scroll_bars_keep_apart_and_off_the_corners() {
        let (f, hb) = (frame_rect(), Paint::hbar_rect());
        assert_eq!(f.x(), STRIP_W + 1, "1 px right of the strip's rule");
        assert_eq!(hb.y(), f.bottom() + 1, "1 px under the sheet's frame");
        assert_eq!(hb.bottom(), grid::APP_FRAME_BOTTOM, "the bar's last row is y 167, above the framed apps' bottom line");
        let vb = Rect::new(VBAR_X, VY, grid::SCROLL_W as u32, VH as u32);
        assert!(vb.x() >= f.right() + 2 && vb.bottom() <= grid::app_content().bottom(), "clear of the frame and of the horizontal bar");
    }
    use crate::font::FontSystem;
    use crate::system::mock::MockBackend;
    use crate::theme::{Theme, ThemeManager};

    fn tmp(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("harbour_paint_{tag}_{}_{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn run(f: impl FnOnce(&mut Paint, &Ctx)) {
        let fs = FontSystem::new();
        let tm = ThemeManager::new(Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = MockBackend::new();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut p = Paint::new(tmp("x"));
        f(&mut p, &ctx);
        let _ = std::fs::remove_dir_all(&p.dir);
    }

    fn down(x: i32, y: i32) -> PointerEvent {
        PointerEvent::Down { x, y, button: 0, clicks: 1 }
    }
    fn drag(x: i32, y: i32) -> PointerEvent {
        PointerEvent::Drag { x, y, dx: 0, dy: 0 }
    }
    fn up(x: i32, y: i32) -> PointerEvent {
        PointerEvent::Up { x, y, button: 0 }
    }
    fn key(p: &mut Paint, ctx: &Ctx, k: Keycode) -> AppAction {
        p.key(k, Mod::NOMOD, false, ctx)
    }
    fn ctrl(p: &mut Paint, ctx: &Ctx, k: Keycode) -> AppAction {
        p.key(k, Mod::LCTRLMOD, false, ctx)
    }

    #[test]
    fn dragging_with_the_pencil_draws_and_undo_redo_restore() {
        run(|p, ctx| {
            p.pointer(down(VX + 10, VY + 10), ctx);
            p.pointer(drag(VX + 30, VY + 10), ctx);
            p.pointer(up(VX + 30, VY + 10), ctx);
            assert!(p.bm.get(10, 10) && p.bm.get(20, 10) && p.bm.get(30, 10) && !p.bm.get(31, 10));
            assert!(p.dirty);
            ctrl(p, ctx, Keycode::Z);
            assert_eq!(p.bm.ink_count(), 0);
            ctrl(p, ctx, Keycode::Y);
            assert_eq!(p.bm.ink_count(), 21);
        });
    }

    #[test]
    fn the_tool_strip_and_letter_keys_pick_tools() {
        run(|p, ctx| {
            let r = tool_cell(4); // Line
            p.pointer(down(r.x() + 3, r.y() + 3), ctx);
            assert_eq!(p.tool, Tool::Line);
            key(p, ctx, Keycode::O);
            assert_eq!(p.tool, Tool::Ellipse);
            assert_eq!(key(p, ctx, Keycode::E), AppAction::Toast("Eraser".into()));
            // A click in the pattern cell opens the pop-up; choosing a cell picks the pattern.
            let pc = pattern_cell();
            p.pointer(down(pc.x() + 4, pc.y() + 4), ctx);
            assert!(p.popup.is_some());
            let c5 = popup_cell(5);
            p.pointer(down(c5.x() + 3, c5.y() + 3), ctx);
            assert_eq!((p.popup, p.pattern), (None, Pattern(5)));
        });
    }

    #[test]
    fn shapes_rubber_band_and_fill_uses_the_pattern() {
        run(|p, ctx| {
            key(p, ctx, Keycode::R);
            p.pointer(down(VX + 5, VY + 5), ctx);
            p.pointer(drag(VX + 40, VY + 30), ctx);
            p.pointer(drag(VX + 20, VY + 20), ctx); // shrinks again: no leftovers
            p.pointer(up(VX + 20, VY + 20), ctx);
            assert!(p.bm.get(5, 5) && p.bm.get(20, 20) && !p.bm.get(40, 30));
            assert_eq!(p.bm.ink_count(), 16 * 2 + 14 * 2);
            key(p, ctx, Keycode::F);
            p.pattern = Pattern(5);
            p.pointer(down(VX + 10, VY + 10), ctx);
            assert!(p.bm.get(10, 10) && p.bm.get(19, 19));
            assert!(!p.bm.get(100, 100), "outside the rectangle stays paper");
        });
    }

    #[test]
    fn keyboard_only_drawing_with_the_cursor_and_space() {
        run(|p, ctx| {
            p.cursor = (20, 20);
            key(p, ctx, Keycode::Space); // pen down: dots at the cursor
            for _ in 0..5 {
                key(p, ctx, Keycode::Right);
            }
            key(p, ctx, Keycode::Space); // pen up
            key(p, ctx, Keycode::Right);
            assert_eq!(p.bm.ink_count(), 6);
            assert!(p.bm.get(25, 20) && !p.bm.get(26, 20));
            // A rectangle from the keyboard: Space, move, Space.
            key(p, ctx, Keycode::R);
            p.cursor = (50, 50);
            key(p, ctx, Keycode::Space);
            p.key(Keycode::Right, Mod::LSHIFTMOD, false, ctx);
            p.key(Keycode::Down, Mod::LSHIFTMOD, false, ctx);
            key(p, ctx, Keycode::Space);
            assert!(p.bm.get(58, 50) && p.bm.get(58, 58) && p.bm.get(50, 58) && !p.bm.get(54, 54));
        });
    }

    #[test]
    fn esc_cancels_a_stroke_and_never_quits() {
        run(|p, ctx| {
            p.pointer(down(VX + 10, VY + 10), ctx);
            p.pointer(drag(VX + 50, VY + 10), ctx);
            assert_eq!(key(p, ctx, Keycode::Escape), AppAction::None);
            assert_eq!(p.bm.ink_count(), 0, "the stroke in progress is undone");
            assert_eq!(key(p, ctx, Keycode::Escape), AppAction::None);
        });
    }

    #[test]
    fn zoom_keeps_the_view_and_maps_pixels() {
        run(|p, ctx| {
            key(p, ctx, Keycode::Num4);
            assert_eq!(p.zoom, 4);
            let (sx, sy) = (p.sx, p.sy);
            assert_eq!(p.to_sheet(VX + 8, VY + 8), (sx + 2, sy + 2));
            p.pointer(down(VX + 8, VY + 8), ctx);
            p.pointer(up(VX + 8, VY + 8), ctx);
            assert!(p.bm.get(sx + 2, sy + 2) && p.bm.ink_count() == 1);
            for _ in 0..200 {
                key(p, ctx, Keycode::Down);
            }
            assert!(p.sy > 0 && p.sy <= p.max_scroll().1, "the view follows the cursor");
            key(p, ctx, Keycode::Num1);
            assert_eq!(p.zoom, 1);
        });
    }

    #[test]
    fn text_goes_in_on_return_and_undoes_in_one_step() {
        run(|p, ctx| {
            key(p, ctx, Keycode::T);
            p.pointer(down(VX + 10, VY + 10), ctx);
            p.pointer(up(VX + 10, VY + 10), ctx);
            assert!(p.text.is_some());
            key(p, ctx, Keycode::A); // letters are text now, not tools
            assert_eq!(p.tool, Tool::Text);
            p.text("Hi", ctx);
            assert_eq!(p.text.as_ref().unwrap().buf, "Hi");
            key(p, ctx, Keycode::Backspace);
            assert_eq!(p.text.as_ref().unwrap().buf, "H");
            key(p, ctx, Keycode::Return);
            assert!(p.text.is_none() && p.hist.can_undo());
            ctrl(p, ctx, Keycode::Z);
            assert_eq!(p.bm.ink_count(), 0);
        });
    }

    #[test]
    fn selection_move_and_copy() {
        run(|p, ctx| {
            p.bm.fill_rect(10, 10, 6, 6, true);
            key(p, ctx, Keycode::M);
            p.pointer(down(VX + 8, VY + 8), ctx);
            p.pointer(drag(VX + 20, VY + 20), ctx);
            p.pointer(up(VX + 20, VY + 20), ctx);
            assert_eq!(p.sel.as_ref().unwrap().r, (8, 8, 13, 13));
            // Drag it 30 px right: the source is cleared, the copy lands there.
            p.pointer(down(VX + 12, VY + 12), ctx);
            p.pointer(drag(VX + 42, VY + 12), ctx);
            p.pointer(up(VX + 42, VY + 12), ctx);
            key(p, ctx, Keycode::Return);
            assert!(!p.bm.get(10, 10) && p.bm.get(40, 10) && p.bm.get(45, 15));
            assert_eq!(p.bm.ink_count(), 36);
            // Copy and paste keeps both.
            ctrl(p, ctx, Keycode::A);
            ctrl(p, ctx, Keycode::C);
            ctrl(p, ctx, Keycode::V);
            key(p, ctx, Keycode::Return);
            assert_eq!(p.bm.ink_count(), 36);
        });
    }

    #[test]
    fn save_asks_for_a_name_writes_a_png_and_open_reads_it_back() {
        run(|p, ctx| {
            p.bm.fill_rect(3, 3, 9, 5, true);
            p.dirty = true;
            ctrl(p, ctx, Keycode::S);
            assert!(matches!(p.modal, Some(Modal::Name { .. })));
            p.text("cat", ctx);
            let a = key(p, ctx, Keycode::Return);
            assert_eq!(a, AppAction::Toast("Saved cat.png".into()));
            assert!(!p.dirty && p.modal.is_none());
            let file = p.dir.join("cat.png");
            let back = Bitmap::from_png(&std::fs::read(&file).unwrap()).unwrap();
            assert_eq!(back, p.bm);

            // New with no changes starts a blank sheet; Open lists the file and loads it through SDL_image.
            ctrl(p, ctx, Keycode::N);
            assert_eq!(p.bm.ink_count(), 0);
            ctrl(p, ctx, Keycode::O);
            assert!(matches!(p.modal, Some(Modal::Pick { .. })));
            key(p, ctx, Keycode::Return);
            if matches!(p.modal, Some(Modal::Error(_))) {
                eprintln!("skipped the SDL_image read-back: no PNG support here");
                return;
            }
            assert_eq!(p.bm.ink_count(), 45);
            assert_eq!(p.file_name(), "cat");
        });
    }

    #[test]
    fn unsaved_changes_ask_before_new() {
        run(|p, ctx| {
            p.pointer(down(VX + 3, VY + 3), ctx);
            p.pointer(up(VX + 3, VY + 3), ctx);
            ctrl(p, ctx, Keycode::N);
            assert!(matches!(p.modal, Some(Modal::Unsaved { .. })));
            key(p, ctx, Keycode::Escape); // Cancel: nothing lost
            assert!(p.modal.is_none() && p.bm.ink_count() == 1);
            ctrl(p, ctx, Keycode::N);
            key(p, ctx, Keycode::D); // Don't Save
            assert_eq!(p.bm.ink_count(), 0);
        });
    }

    #[test]
    fn leaving_with_unsaved_changes_asks_first() {
        run(|p, ctx| {
            assert!(p.request_close(), "nothing drawn: leave at once");
            p.pointer(down(VX + 3, VY + 3), ctx);
            p.pointer(up(VX + 3, VY + 3), ctx);
            assert!(!p.request_close());
            assert!(matches!(p.modal, Some(Modal::Unsaved { .. })));
            assert!(!p.request_close(), "asking again keeps the one card");
            assert_eq!(key(p, ctx, Keycode::Escape), AppAction::None, "Esc = Cancel: stay");
            assert!(p.modal.is_none() && p.bm.ink_count() == 1);
            assert!(!p.request_close());
            assert_eq!(key(p, ctx, Keycode::D), AppAction::Quit, "Don't Save leaves");
        });
    }

    #[test]
    fn menus_are_labelled_and_context_menus_depend_on_the_place() {
        run(|p, ctx| {
            assert!(p.menu().iter().all(|m| !m.label.is_empty()));
            let tools = p.context_menu(5, 30, ctx).unwrap();
            assert_eq!(tools.len(), 12);
            let sheet = p.context_menu(100, 60, ctx).unwrap();
            assert!(sheet.iter().any(|m| m.id == "undo"));
            p.menu_action("t_ellipse", ctx);
            assert_eq!(p.tool, Tool::Ellipse);
            p.menu_action("filled", ctx);
            assert!(p.filled);
            assert!(p.describe().contains("Ellipse"));
        });
    }

    #[test]
    fn help_rows_are_filled_in() {
        crate::launcher::help::check_app_rows("Paint", &Paint::new(std::env::temp_dir()).help());
    }
}
