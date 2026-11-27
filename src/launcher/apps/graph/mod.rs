//! Graph: a mathematical sketchbook. Pictures made from formulas: curves (y = f(x), polar,
//! parametric), terrain from noise, Voronoi cells and vector fields, each from a few formulas
//! and a slider for every number in them.
//!
//! * The canvas is on the left, the formulas and sliders on the right. Changing a slider redraws at
//!   once. `M` (or a double click on the canvas) gives the whole screen to the canvas.
//! * Every mode works the same way: Up / Down choose a row, Left / Right move a slider (Shift:
//!   faster), Return opens the formula editor, Space plays the time `t`, drag / wheel / `+ -`
//!   move and zoom the plane, `P` (or PageUp / PageDown) picks presets, `G` the grid, `R` a new seed.
//! * A name in a formula that is not a variable becomes a slider by itself (`a*sin(b*x)` gives
//!   sliders a and b).
//! * Files: `.graph` text in `~/Documents` (formulas, sliders, seed, view), PNG export to
//!   `~/Pictures`. Ctrl+Z / Ctrl+Y undo and redo every change.
//!
//! The parts: [`expr`] (the formula language), [`doc`] (the document and its file), [`presets`],
//! [`render`] (formulas to a raster, a little each frame) and [`tones`] (raster to theme colours).

pub mod doc;
pub use doc::csv_to_graph;
pub mod expr;
pub mod presets;
pub mod render;
pub mod tones;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::keys::{Keycode, Mod};
use crate::gfx::Color;
use crate::gfx::Rect;
use crate::gfx::Canvas;

use crate::assets::AssetStore;
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp, Sizing};
use crate::launcher::dialogs::{CardChoice, Dialog, DialogKind, DialogOut, ListPicker, SavePanel, UnsavedCard};
use crate::launcher::i18n::{tr, tr_s};
use crate::launcher::lineedit::LineEdit;
use crate::launcher::widgets::{self, contains, MenuItem, PointerEvent, Slider};
use crate::ui;
use doc::{fmt_num, Doc, Mode, View};
use render::{Job, Raster};

// ------------------------------------------------------------------------------ layout

/// The sliders' column (on every surface size).
const PANEL_W: i32 = 104;
/// Top of the canvas and the panel: 2 px under the menu strip.
const TOP: i32 = grid::BAR_H + 2;
/// The panel's title row, then its rule, then the rows.
const ROWS_Y: i32 = TOP + grid::ROW_H + 1;
/// Slider rows: the name, the track, the value.
const NAME_W: i32 = 36;
const TRACK_X: i32 = 39;
const TRACK_W: i32 = 31;

struct Layout {
    /// The canvas's frame (the picture is inside it).
    frame: Rect,
    panel: Option<Rect>,
}

impl Layout {
    fn now(maximized: bool) -> Layout {
        let (w, h) = (grid::sw(), grid::sh());
        if maximized {
            return Layout { frame: Rect::new(1, grid::BAR_H + 1, (w - 2) as u32, (h - grid::BAR_H - 3) as u32), panel: None };
        }
        // Down to y 166 on the 320 x 170 screen, like Calendar's grid.
        let bottom = h - 3;
        let px = w - grid::MARGIN - PANEL_W;
        let frame = Rect::new(grid::MARGIN, TOP, (px - grid::GUTTER - grid::MARGIN) as u32, (bottom - TOP) as u32);
        Layout { frame, panel: Some(Rect::new(px, TOP, PANEL_W as u32, (bottom - TOP) as u32)) }
    }

    /// The picture: inside the frame.
    fn canvas(&self) -> Rect {
        Rect::new(self.frame.x() + 1, self.frame.y() + 1, self.frame.width() - 2, self.frame.height() - 2)
    }

    /// How many panel rows fit.
    fn visible_rows(&self) -> usize {
        self.panel.map_or(0, |p| ((p.bottom() - ROWS_Y) / grid::ROW_H).max(1) as usize)
    }

    fn row_rect(&self, k: usize) -> Rect {
        let p = self.panel.unwrap_or(Rect::new(0, 0, PANEL_W as u32, 1));
        Rect::new(p.x(), ROWS_Y + k as i32 * grid::ROW_H, p.width(), grid::ROW_H as u32)
    }

    fn slider(&self, k: usize) -> Slider {
        let r = self.row_rect(k);
        Slider::new(Rect::new(r.x() + TRACK_X, r.y() + 6, TRACK_W as u32, 3))
    }
}

/// A row of the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Formula(usize),
    Param(usize),
    Time,
}

// ------------------------------------------------------------------------------ state

/// A finished picture and what it was made from.
struct Shown {
    raster: Raster,
    /// Screen pixels per raster pixel (2 for a quick preview while dragging).
    scale: usize,
    view: View,
}

/// A drag in progress, and the document from before it (one Undo step for the whole drag).
enum Drag {
    Slider { row: usize, before: Doc },
    Pan { before: Doc },
}

/// The formula editor: one line per formula of the mode.
struct Editor {
    lines: Vec<LineEdit>,
    focus: usize,
    /// The first formula that does not work: its slot and why.
    error: Option<(usize, String)>,
}

enum Ask {
    Preset,
    SaveAs,
    Open,
    /// "Save changes?" before leaving.
    Unsaved,
}

pub struct Graph {
    doc: Doc,
    /// The view the document came with (Reset view goes back to it).
    home_view: View,
    t: f64,
    playing: bool,
    last_tick: Option<Instant>,
    job: Option<Job>,
    job_scale: usize,
    job_view: View,
    shown: Option<Shown>,
    /// The document (or the canvas size) changed since the last picture was started.
    stale: bool,
    /// A formula that does not compile: its slot and why (the picture stays as it was).
    error: Option<(usize, String)>,
    /// The canvas's size in screen pixels, from the last draw.
    size: (usize, usize),
    cursor: usize,
    scroll: usize,
    maximized: bool,
    drag: Option<Drag>,
    editor: Option<Editor>,
    undo: Vec<Doc>,
    redo: Vec<Doc>,
    /// Repeated changes with the same tag (one slider, by keys) are one Undo step.
    coalesce: Option<String>,
    path: Option<PathBuf>,
    dirty: bool,
    quit_after_save: bool,
    modal: Option<Dialog<Ask>>,
    /// A short note over the maximized canvas ("k = 5").
    flash: Option<(String, Instant)>,
}

pub fn make() -> Box<dyn LauncherApp> {
    Box::new(Graph::new())
}

fn mock() -> bool {
    matches!(std::env::var("HARBOUR_MOCK").as_deref(), Ok("1" | "true" | "yes" | "on"))
}

/// Where `.graph` files go.
fn documents() -> PathBuf {
    if mock() {
        return std::env::temp_dir().join("harbour-graph-mock");
    }
    crate::launcher::fileops::user_dir(crate::launcher::fileops::UserDir::Documents)
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// The modes that fill every pixel (they get a quick half-size preview while dragging).
fn heavy(m: Mode) -> bool {
    matches!(m, Mode::Terrain | Mode::Voronoi | Mode::Field)
}

/// A slider's name as shown: the renderer's own sliders have Japanese names.
fn param_label(name: &str) -> String {
    let key = format!("graph.{name}");
    match tr_s(&key) {
        s if s == key => name.to_string(),
        s => s,
    }
}

/// `text` cut to `w` pixels, with ".." when it had to be cut.
fn fit(fs: &crate::font::FontSystem, text: &str, w: i32) -> String {
    if fs.measure_text(text) <= w {
        return text.to_string();
    }
    let mut s: String = text.to_string();
    while !s.is_empty() && fs.measure_text(&format!("{s}..")) > w {
        s.pop();
    }
    format!("{s}..")
}

impl Graph {
    pub fn new() -> Graph {
        let (doc, t) = presets::load(0);
        let mut g = Graph {
            home_view: doc.view,
            doc,
            t,
            playing: false,
            last_tick: None,
            job: None,
            job_scale: 1,
            job_view: View::default(),
            shown: None,
            stale: true,
            error: None,
            size: (0, 0),
            cursor: 0,
            scroll: 0,
            maximized: false,
            drag: None,
            editor: None,
            undo: Vec::new(),
            redo: Vec::new(),
            coalesce: None,
            path: None,
            dirty: false,
            quit_after_save: false,
            modal: None,
            flash: None,
        };
        g.playing = g.doc.uses_time();
        g
    }

    // -------------------------------------------------------------------------- rows

    fn rows(&self) -> Vec<Row> {
        let mut v: Vec<Row> = (0..self.doc.formulas.len()).map(Row::Formula).collect();
        v.extend((0..self.doc.params.len()).map(Row::Param));
        if self.doc.uses_time() {
            v.push(Row::Time);
        }
        v
    }

    fn row(&self) -> Row {
        let rows = self.rows();
        rows[self.cursor.min(rows.len() - 1)]
    }

    fn set_cursor(&mut self, k: usize) {
        let n = self.rows().len();
        self.cursor = k.min(n - 1);
        let vis = Layout::now(self.maximized).visible_rows().max(1);
        self.scroll = widgets::follow_scroll(self.cursor, self.scroll, vis, n);
        self.coalesce = None;
    }

    /// The row under a point of the panel.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let l = Layout::now(self.maximized);
        let p = l.panel?;
        if !contains(p, x, y) || y < ROWS_Y {
            return None;
        }
        let k = self.scroll + ((y - ROWS_Y) / grid::ROW_H) as usize;
        (k < self.rows().len() && k - self.scroll < l.visible_rows()).then_some(k)
    }

    /// What a row says, for the flash note and the Agent: "k = 5".
    fn row_text(&self, r: Row) -> String {
        match r {
            Row::Formula(i) => format!("{} = {}", self.doc.mode.slots()[i], self.doc.formulas[i]),
            Row::Param(i) => format!("{} = {}", param_label(&self.doc.params[i].name), fmt_num(self.doc.params[i].value)),
            Row::Time => format!("t = {}", fmt_num(self.t)),
        }
    }

    fn note(&mut self, text: String) {
        if self.maximized {
            self.flash = Some((text, Instant::now()));
        }
    }

    // -------------------------------------------------------------------------- changes

    /// Change the document as one Undo step (or, with the same `tag` as the change before, as part
    /// of that step).
    fn change(&mut self, tag: Option<String>, f: impl FnOnce(&mut Doc)) {
        let before = self.doc.clone();
        f(&mut self.doc);
        if self.doc == before {
            return;
        }
        if tag.is_none() || tag != self.coalesce {
            self.push_undo(before);
        }
        self.coalesce = tag;
        self.touched();
    }

    fn push_undo(&mut self, before: Doc) {
        self.undo.push(before);
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn touched(&mut self) {
        self.dirty = true;
        self.stale = true;
        let n = self.rows().len();
        if self.cursor >= n {
            self.set_cursor(n - 1);
        }
    }

    /// Replace the whole document (a preset, a file): one Undo step.
    fn replace(&mut self, d: Doc, t: f64) {
        let before = std::mem::replace(&mut self.doc, d);
        self.push_undo(before);
        self.coalesce = None;
        self.home_view = self.doc.view;
        self.t = t;
        self.playing = self.doc.uses_time();
        self.error = None;
        self.shown = None;
        self.stale = true;
        self.set_cursor(0);
    }

    fn undo(&mut self) -> bool {
        let Some(d) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.doc, d));
        self.after_history();
        true
    }

    fn redo(&mut self) -> bool {
        let Some(d) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.doc, d));
        self.after_history();
        true
    }

    fn after_history(&mut self) {
        self.coalesce = None;
        self.error = None;
        self.dirty = true;
        self.stale = true;
        let n = self.rows().len();
        self.set_cursor(self.cursor.min(n - 1));
    }

    fn load_preset(&mut self, i: usize) {
        let (d, t) = presets::load(i);
        self.replace(d, t);
        self.dirty = false;
        self.path = None;
    }

    fn preset_index(&self) -> Option<usize> {
        presets::PRESETS.iter().position(|(n, _)| *n == self.doc.title)
    }

    fn nudge(&mut self, dir: i32, coarse: bool) {
        match self.row() {
            Row::Param(i) => {
                let tag = format!("key {}", self.doc.params[i].name);
                self.change(Some(tag), |d| d.params[i].nudge(dir, coarse));
            }
            Row::Time => {
                self.playing = false;
                self.t += dir as f64 * if coarse { 1.0 } else { 0.1 };
                self.stale = true;
            }
            Row::Formula(_) => {}
        }
        self.note(self.row_text(self.row()));
    }

    /// Zoom by `factor` (below 1 = in) keeping the plane point under screen (x, y) in place.
    fn zoom_at(&mut self, factor: f64, at: Option<(i32, i32)>) {
        let c = Layout::now(self.maximized).canvas();
        let (w, h) = (c.width() as f64, c.height() as f64);
        let s = self.doc.view.span / w.min(h);
        let (ox, oy) = at.map_or((0.0, 0.0), |(x, y)| (x as f64 - c.x() as f64 - w / 2.0, y as f64 - c.y() as f64 - h / 2.0));
        self.change(Some("view".into()), |d| {
            let (px, py) = (d.view.cx + ox * s, d.view.cy - oy * s);
            d.view.span = (d.view.span * factor).clamp(1e-4, 1e5);
            let s2 = d.view.span / w.min(h);
            d.view.cx = px - ox * s2;
            d.view.cy = py + oy * s2;
        });
    }

    fn reseed(&mut self) {
        self.change(None, |d| d.seed = expr::hash32(d.seed.wrapping_add(1)) % 100_000);
    }

    fn toggle_max(&mut self) {
        self.maximized = !self.maximized;
        self.flash = None;
        self.stale = true;
        self.set_cursor(self.cursor);
    }

    fn switch_mode(&mut self, m: Mode) {
        if self.doc.mode != m {
            self.load_preset(presets::first_of(m));
        }
    }

    // -------------------------------------------------------------------------- the picture

    /// Start a picture if one is due, and work on the one in progress for a few milliseconds.
    /// True when there is something new to show.
    fn render(&mut self, budget: Duration) -> bool {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return false;
        }
        let animating = self.playing && self.doc.uses_time() && self.editor.is_none();
        let preview = self.drag.is_some() && heavy(self.doc.mode);
        let scale = if preview { 2 } else { 1 };
        // A new picture is due when something changed; a running one is dropped for it, unless
        // it is more than half done (so a stream of slider moves still shows pictures).
        let due = self.stale || (animating && self.job.is_none());
        let keep_running = self.job.as_ref().is_some_and(|j| j.progress() > 0.5);
        if due && !keep_running {
            self.stale = false;
            match Job::new(&self.doc, self.t, w.div_ceil(scale), h.div_ceil(scale)) {
                Ok(j) => {
                    self.job = Some(j);
                    self.job_scale = scale;
                    self.job_view = self.doc.view;
                    self.error = None;
                }
                Err((slot, e)) => {
                    self.job = None;
                    self.error = Some((slot, tr_s(&e.msg)));
                    return true;
                }
            }
        }
        let Some(job) = self.job.as_mut() else { return false };
        let start = Instant::now();
        while !job.step(1500) {
            if start.elapsed() >= budget {
                return self.shown.is_none();
            }
        }
        let job = self.job.take().unwrap();
        self.shown = Some(Shown { raster: job.raster, scale: self.job_scale, view: self.job_view });
        true
    }

    /// The finished picture, computed now if there is none (for export).
    fn picture(&mut self) -> Option<Raster> {
        if self.shown.as_ref().is_some_and(|s| s.scale == 1) && !self.stale && self.job.is_none() {
            return self.shown.as_ref().map(|s| s.raster.clone());
        }
        let (w, h) = if self.size.0 > 0 { self.size } else { (320, 170) };
        let mut j = Job::new(&self.doc, self.t, w, h).ok()?;
        while !j.step(100_000) {}
        Some(j.raster)
    }

    // -------------------------------------------------------------------------- editor

    fn open_editor(&mut self) {
        let focus = match self.row() {
            Row::Formula(i) => i,
            _ => 0,
        };
        let lines = self.doc.formulas.iter().map(|f| LineEdit::new(f)).collect();
        self.editor = Some(Editor { lines, focus, error: None });
        self.check_editor();
    }

    /// The document the editor's text would make, or the first formula that does not work.
    fn edited(&self) -> Option<Result<Doc, (usize, String)>> {
        let e = self.editor.as_ref()?;
        let mut d = self.doc.clone();
        d.formulas = e.lines.iter().map(|l| l.text.trim().to_string()).collect();
        d.sync_params();
        Some(match d.compile() {
            Ok(_) => Ok(d),
            Err((slot, err)) => Err((slot, tr_s(&err.msg))),
        })
    }

    fn check_editor(&mut self) {
        let err = self.edited().and_then(|r| r.err());
        if let Some(e) = self.editor.as_mut() {
            e.error = err;
        }
    }

    fn apply_editor(&mut self) {
        match self.edited() {
            Some(Ok(d)) => {
                self.editor = None;
                self.change(None, |doc| *doc = d);
                if self.doc.uses_time() && !self.playing && self.t == 0.0 {
                    self.playing = true;
                }
            }
            Some(Err(e)) => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.focus = e.0;
                    ed.error = Some(e);
                }
            }
            None => {}
        }
    }

    fn editor_key(&mut self, key: Keycode, mods: Mod) {
        let Some(e) = self.editor.as_mut() else { return };
        let n = e.lines.len();
        match key {
            Keycode::Escape => self.editor = None,
            Keycode::Return | Keycode::KpEnter => self.apply_editor(),
            Keycode::Up => e.focus = (e.focus + n - 1) % n,
            Keycode::Down | Keycode::Tab => e.focus = (e.focus + 1) % n,
            _ => {
                if e.lines[e.focus].key(key, mods) {
                    self.check_editor();
                }
            }
        }
    }

    // -------------------------------------------------------------------------- files

    fn write(&mut self, path: &Path) -> AppAction {
        let path = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("graph")) { path.to_path_buf() } else { path.with_extension("graph") };
        let res = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, self.doc.to_text(self.t)));
        match res {
            Ok(()) => {
                self.path = Some(path.clone());
                self.dirty = false;
                if self.quit_after_save {
                    return AppAction::Quit;
                }
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                AppAction::Toast(tr_s(&format!("Saved {name}")))
            }
            Err(e) => {
                self.quit_after_save = false;
                AppAction::Toast(e.to_string())
            }
        }
    }

    fn save(&mut self) -> AppAction {
        match self.path.clone() {
            Some(p) => self.write(&p),
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) -> AppAction {
        let dir = self.path.as_ref().and_then(|p| p.parent()).map(PathBuf::from).unwrap_or_else(documents);
        let _ = std::fs::create_dir_all(&dir);
        let name = format!("{}.graph", self.doc.title);
        self.modal = Some(Dialog::new(DialogKind::Panel(SavePanel::new(&dir, &name, &home())), Ask::SaveAs));
        AppAction::None
    }

    fn ask_open(&mut self) {
        let dir = documents();
        let _ = std::fs::create_dir_all(&dir);
        self.modal = Some(Dialog::new(DialogKind::Panel(SavePanel::open(&dir, &home(), &["graph"])), Ask::Open));
    }

    fn ask_preset(&mut self) {
        let rows = presets::PRESETS
            .iter()
            .enumerate()
            .map(|(i, (name, _))| format!("{} - {}", tr_s(name), tr(presets::load(i).0.mode.name())))
            .collect();
        let mut p = ListPicker::new("Presets", rows).with_button("Open");
        p.sel = self.preset_index().unwrap_or(0);
        p.scroll = p.sel.saturating_sub(2);
        self.modal = Some(Dialog::new(DialogKind::Pick(p), Ask::Preset));
    }

    fn load_file(&mut self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        // A CSV (a Hardware sensor log) opens as a plot through its numbers; saving asks for a
        // new `.graph` name, the CSV itself is never overwritten.
        let is_csv = path.extension().map_or(false, |e| e.eq_ignore_ascii_case("csv"));
        let text = if is_csv {
            let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            csv_to_graph(&text, &title).map_err(|e| tr_s(&e))?
        } else {
            text
        };
        let (d, t) = Doc::from_text(&text).map_err(|e| tr_s(&e))?;
        self.replace(d, t);
        self.path = if is_csv { None } else { Some(path.to_path_buf()) };
        self.dirty = false;
        Ok(())
    }

    fn export(&mut self, ctx: &Ctx) -> AppAction {
        let Some(r) = self.picture() else { return AppAction::Toast(tr_s("Fix the formula first")) };
        let ramp = tones::Ramp::of(ctx.tm, crate::launcher::settings::panel_is_mono());
        let png = crate::launcher::apps::camera::png::encode_rgb(r.w as u32, r.h as u32, &tones::rgb(&r, &ramp));
        let dir = crate::launcher::apps::paint::pictures_dir();
        let base = if self.doc.title.trim().is_empty() { "Graph".to_string() } else { self.doc.title.replace(['/', '\\', ':'], "-") };
        let path = (1..)
            .map(|n| dir.join(if n == 1 { format!("{base}.png") } else { format!("{base} {n}.png") }))
            .find(|p| !p.exists())
            .unwrap();
        match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, png)) {
            Ok(()) => AppAction::Toast(tr_s(&format!("Exported {}", path.file_name().unwrap().to_string_lossy()))),
            Err(e) => AppAction::Toast(e.to_string()),
        }
    }

    fn answer(&mut self, out: DialogOut) -> AppAction {
        let Some(d) = self.modal.take() else { return AppAction::None };
        match (d.tag, out) {
            (Ask::Preset, DialogOut::Pick(i)) => self.load_preset(i),
            (Ask::SaveAs, DialogOut::Path(p)) => return self.write(&p),
            (Ask::Open, DialogOut::Path(p)) => {
                if let Err(e) = self.load_file(&p) {
                    return AppAction::Toast(e);
                }
            }
            (Ask::Unsaved, DialogOut::Card(CardChoice::Save)) => {
                self.quit_after_save = true;
                return self.save();
            }
            (Ask::Unsaved, DialogOut::Card(_)) => {
                self.dirty = false;
                return AppAction::Quit;
            }
            _ => self.quit_after_save = false,
        }
        AppAction::None
    }

    // -------------------------------------------------------------------------- commands

    fn command(&mut self, id: &str, ctx: &Ctx) -> AppAction {
        if let Some(m) = id.strip_prefix("mode_").and_then(Mode::from_id) {
            self.switch_mode(m);
            return AppAction::None;
        }
        match id {
            "presets" => self.ask_preset(),
            "edit" => self.open_editor(),
            "undo" => {
                if !self.undo() {
                    return AppAction::Toast(tr_s("Nothing to undo"));
                }
            }
            "redo" => {
                if !self.redo() {
                    return AppAction::Toast(tr_s("Nothing to redo"));
                }
            }
            "open" => self.ask_open(),
            "save" => return self.save(),
            "saveas" => return self.save_as(),
            "export" => return self.export(ctx),
            "home" => {
                let v = self.home_view;
                self.change(None, |d| d.view = v);
            }
            "grid" => self.change(None, |d| d.grid = !d.grid),
            "max" => self.toggle_max(),
            "seed" => {
                self.reseed();
                return AppAction::Toast(tr_s(&format!("Seed {}", self.doc.seed)));
            }
            "play" => self.playing = !self.playing,
            _ => {}
        }
        AppAction::None
    }

    fn panel_click(&mut self, x: i32, y: i32, clicks: u8) {
        let Some(k) = self.row_at(x, y) else { return };
        self.set_cursor(k);
        let l = Layout::now(self.maximized);
        let row = l.row_rect(k - self.scroll);
        match self.row() {
            Row::Formula(_) => self.open_editor(),
            Row::Param(i) => {
                let s = l.slider(k - self.scroll);
                if s.hit(x, y) || x >= row.x() + TRACK_X - 2 {
                    let before = self.doc.clone();
                    self.doc.params[i].set_frac(s.value_at(x) as f64 / 100.0);
                    self.stale = true;
                    self.drag = Some(Drag::Slider { row: k, before });
                }
            }
            Row::Time => {
                if x >= row.right() - 14 || clicks >= 2 {
                    self.playing = !self.playing;
                }
            }
        }
    }

    /// The end of a drag: the whole drag is one Undo step.
    fn end_drag(&mut self) {
        let before = match self.drag.take() {
            Some(Drag::Slider { before, .. }) | Some(Drag::Pan { before }) => before,
            None => return,
        };
        if before != self.doc {
            self.push_undo(before);
            self.dirty = true;
        }
        self.coalesce = None;
        // Draw again at full size (a drag shows a quick half-size picture).
        self.stale = true;
    }

    // -------------------------------------------------------------------------- drawing

    fn draw_canvas(&self, c: &mut Canvas, ctx: &Ctx, l: &Layout, ramp: &tones::Ramp) {
        let tm = ctx.tm;
        let (ink, paper) = (tm.get_color("foreground"), tm.get_color("widget_face_active"));
        let area = l.canvas();
        ui::draw_rect(c, l.frame, ink);
        // The finished picture; while the first one is still being made, the one in progress.
        let shown = self.shown.as_ref().filter(|s| s.raster.w * s.scale >= area.width() as usize - 1 && s.raster.h * s.scale >= area.height() as usize - 1);
        match (shown, &self.job) {
            (Some(s), _) => {
                // While the plane is dragged the old picture moves along until the new one is ready.
                let px = s.view.span / (area.width().min(area.height()) as f64);
                let same_zoom = (s.view.span - self.doc.view.span).abs() < 1e-12;
                let shift = if same_zoom { (((s.view.cx - self.doc.view.cx) / px).round() as i32, ((self.doc.view.cy - s.view.cy) / px).round() as i32) } else { (0, 0) };
                tones::blit(c, &s.raster, s.scale, area, shift, ramp, paper);
            }
            (None, Some(j)) => tones::blit(c, &j.raster, self.job_scale, area, (0, 0), ramp, paper),
            (None, None) => ui::fill_rect(c, area, paper),
        }
        // A thin dotted bar along the bottom while a picture is being made.
        if let Some(j) = &self.job {
            let w = (area.width() as f32 * j.progress()) as u32;
            if w > 0 && self.shown.is_some() {
                crate::dither::fill(c, Rect::new(area.x(), area.bottom() - 2, w, 2), ink, crate::dither::Pattern::Half);
            }
        }
        // A formula that does not work: say which and why, over the old picture.
        if let Some((slot, msg)) = &self.error {
            let text = fit(ctx.fs, &format!("{} = ? {}", self.doc.mode.slots()[*slot], msg), area.width() as i32 - 12);
            let r = Rect::new(area.x() + 2, area.y() + 2, (ctx.fs.measure_text(&text) + 8) as u32, grid::ROW_H as u32);
            ui::fill_rect(c, r, paper);
            ui::draw_rect(c, r, ink);
            ctx.fs.draw_text(c, r.x() + 4, widgets::text_origin(r.y(), r.height() as i32), &text, ink);
        }
        if let Some((text, at)) = &self.flash {
            if at.elapsed() < Duration::from_millis(1500) {
                let text = fit(ctx.fs, text, area.width() as i32 - 16);
                let r = Rect::new(area.x() + 4, area.bottom() - 4 - grid::ROW_H, (ctx.fs.measure_text(&text) + 8) as u32, grid::ROW_H as u32);
                ui::fill_rect(c, r, paper);
                ui::draw_rect(c, r, ink);
                ctx.fs.draw_text(c, r.x() + 4, widgets::text_origin(r.y(), r.height() as i32), &text, ink);
            }
        }
    }

    fn draw_panel(&self, c: &mut Canvas, ctx: &Ctx, l: &Layout) {
        let Some(p) = l.panel else { return };
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let faint = widgets::secondary_ink(tm);
        let line = tm.get_color("line");
        // Title row: the picture's name, and the mode when there is room.
        let title_y = widgets::text_origin(TOP, grid::ROW_H);
        let title = fit(fs, &tr_s(&self.doc.title), p.width() as i32 - 4);
        fs.draw_text_bold(c, p.x() + 2, title_y, &title, ink);
        let mode = tr(self.doc.mode.name());
        let mw = fs.measure_text(mode);
        if fs.measure_text(&title) + 1 + 8 + mw <= p.width() as i32 - 4 {
            fs.draw_text(c, p.right() - 2 - mw, title_y, mode, faint);
        }
        ui::fill_rect(c, Rect::new(p.x(), ROWS_Y - 1, p.width(), 1), ink);

        let rows = self.rows();
        for (k, row) in rows.iter().enumerate().skip(self.scroll).take(l.visible_rows()) {
            let r = l.row_rect(k - self.scroll);
            let sel = k == self.cursor;
            let col = if sel {
                widgets::draw_row_plate(c, tm, r);
                widgets::selection(tm).fg
            } else {
                ink
            };
            let ty = widgets::text_origin(r.y(), grid::ROW_H);
            match *row {
                Row::Formula(i) => {
                    let text = format!("{}={}", self.doc.mode.slots()[i], self.doc.formulas[i]);
                    let bad = self.error.as_ref().is_some_and(|e| e.0 == i);
                    let text = fit(fs, &text, r.width() as i32 - 4 - if bad { 8 } else { 0 });
                    fs.draw_text(c, r.x() + 2, ty, &text, col);
                    if bad {
                        fs.draw_text(c, r.right() - 8, ty, "!", col);
                    }
                }
                Row::Param(i) => {
                    let prm = &self.doc.params[i];
                    fs.draw_text(c, r.x() + 2, ty, &fit(fs, &param_label(&prm.name), NAME_W), col);
                    let dragging = matches!(self.drag, Some(Drag::Slider { row, .. }) if row == k);
                    l.slider(k - self.scroll).draw(c, tm, (prm.frac() * 100.0).round() as u32, dragging, false);
                    let v = fmt_num(prm.value);
                    fs.draw_text(c, r.right() - 1 - fs.measure_text(&v), ty, &v, col);
                }
                Row::Time => {
                    fs.draw_text(c, r.x() + 2, ty, &fit(fs, &format!("t = {}", fmt_num(self.t)), r.width() as i32 - 20), col);
                    // Play / pause, drawn: two bars while playing, a triangle while paused.
                    let (gx, gy) = (r.right() - 10, r.y() + 4);
                    if self.playing {
                        ui::fill_rect(c, Rect::new(gx, gy, 2, 7), col);
                        ui::fill_rect(c, Rect::new(gx + 4, gy, 2, 7), col);
                    } else {
                        for k in 0..4 {
                            ui::fill_rect(c, Rect::new(gx + k, gy + k, 1, (7 - 2 * k) as u32), col);
                        }
                    }
                }
            }
        }
        // More rows than fit: a scroll bar in the panel's last pixels.
        if rows.len() > l.visible_rows() {
            let sb = widgets::ScrollBar::new(Rect::new(p.right() - 3, ROWS_Y, 3, (l.visible_rows() as i32 * grid::ROW_H) as u32), rows.len(), l.visible_rows(), self.scroll);
            sb.draw(c, tm, None);
        }
        let _ = line;
    }

    fn draw_editor(&self, c: &mut Canvas, ctx: &Ctx, e: &Editor) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let faint = widgets::secondary_ink(tm);
        let (w, h) = (grid::sw(), grid::sh());
        let title_y = widgets::text_origin(TOP, grid::ROW_H);
        fs.draw_text_bold(c, grid::MARGIN, title_y, tr("Edit formulas"), ink);
        let mode = format!("{} - {}", tr_s(&self.doc.title), tr(self.doc.mode.name()));
        let mode = fit(fs, &mode, w - 2 * grid::MARGIN - fs.measure_text(tr("Edit formulas")) - 12);
        fs.draw_text(c, w - grid::MARGIN - fs.measure_text(&mode), title_y, &mode, faint);
        ui::fill_rect(c, Rect::new(grid::MARGIN, ROWS_Y - 1, (w - 2 * grid::MARGIN) as u32, 1), ink);
        let slots = self.doc.mode.slots();
        for (k, le) in e.lines.iter().enumerate() {
            let r = editor_field(k);
            let prefix = format!("{} =", slots[k]);
            let f = widgets::Field { text: &le.text, caret: (k == e.focus).then_some(le.caret), prefix: &prefix, prefix_w: 24, ..Default::default() };
            widgets::draw_field(c, fs, tm, r, &f);
        }
        // Under the fields: what the focused formula is, then the problem (or the functions), then
        // the names it may use.
        let y0 = editor_field(e.lines.len()).y();
        let rw = w - 2 * grid::MARGIN;
        let lines: [(String, Color); 3] = [
            (tr(self.doc.mode.slot_hint(slots[e.focus])).to_string(), faint),
            match &e.error {
                Some((slot, msg)) => (format!("{} = ? {}", slots[*slot], msg), ink),
                None => (format!("{} {}", tr("Functions:"), expr::FUNCS.iter().map(|f| f.0).collect::<Vec<_>>().join(" ")), faint),
            },
            {
                let vars: Vec<&str> = self.doc.mode.vars().iter().map(|v| if *v == "theta" { "θ" } else { v }).collect();
                (format!("{} {}  {} {}", tr("Variables:"), vars.join(" "), tr("Sliders:"), self.doc.params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(" ")), faint)
            },
        ];
        for (i, (text, col)) in lines.iter().enumerate() {
            let y = widgets::text_origin(y0 + i as i32 * grid::ROW_H, grid::ROW_H);
            fs.draw_text(c, grid::MARGIN + 2, y, &fit(fs, text, rw - 4), *col);
        }
        let ok = e.error.is_none();
        let (cancel, apply) = editor_buttons();
        widgets::draw_button(c, fs, tm, cancel, "Cancel", widgets::BtnState::default());
        widgets::draw_button(c, fs, tm, apply, "Apply", widgets::BtnState { default: true, disabled: !ok, ..Default::default() });
        let _ = h;
    }
}

impl Default for Graph {
    fn default() -> Self {
        Graph::new()
    }
}

/// The editor's field for formula `k` (one past the last = where the text under them starts).
fn editor_field(k: usize) -> Rect {
    Rect::new(grid::MARGIN, ROWS_Y + 3 + k as i32 * 20, (grid::sw() - 2 * grid::MARGIN) as u32, widgets::FIELD_H as u32)
}

/// Cancel and Apply, bottom right.
fn editor_buttons() -> (Rect, Rect) {
    let (w, h) = (grid::sw(), grid::sh());
    let (bw, bh) = (64, grid::BTN_H_SM + 4);
    let y = h - 4 - bh;
    let apply = Rect::new(w - grid::MARGIN - bw, y, bw as u32, bh as u32);
    let cancel = Rect::new(apply.x() - grid::GUTTER - bw, y, bw as u32, bh as u32);
    (cancel, apply)
}

impl LauncherApp for Graph {
    fn title(&self) -> String {
        crate::launcher::i18n::app_name("graph")
    }

    fn sizing(&self) -> Sizing {
        Sizing::Flexible
    }

    fn takes_text(&self) -> bool {
        self.editor.is_some() || self.modal.is_some()
    }

    fn wants_tab(&self) -> bool {
        self.editor.is_some()
    }

    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Up / Down", "Choose a formula or slider"),
            k("Left / Right", "Move the slider (Shift: faster)"),
            k("Return", "Edit the formulas"),
            k("Space", "Play / pause time t"),
            k("P", "Presets"),
            k("M", "Maximize the canvas"),
            k("G", "Show / hide the grid"),
            k("R", "New random seed"),
            k("+ / - / 0", "Zoom in / out / reset the view"),
            k("Drag / Wheel", "Move / zoom the plane"),
            k("Ctrl+Z / Ctrl+Y", "Undo / redo"),
            k("Ctrl+S / Ctrl+E", "Save / export PNG"),
        ]
    }

    fn describe(&self) -> String {
        let rows: Vec<String> = self.rows().iter().map(|r| self.row_text(*r)).collect();
        format!("Graph: {} ({} mode). {}", self.doc.title, self.doc.mode.name(), rows.join("; "))
    }

    fn menu(&self) -> Vec<MenuItem> {
        let mut v = vec![MenuItem::new("presets", "Presets..."), MenuItem::new("edit", "Edit formulas...")];
        for (id, m) in [("mode_function", Mode::Function), ("mode_polar", Mode::Polar), ("mode_parametric", Mode::Parametric), ("mode_terrain", Mode::Terrain), ("mode_voronoi", Mode::Voronoi), ("mode_field", Mode::Field)] {
            let mark = if self.doc.mode == m { "* " } else { "" };
            v.push(MenuItem::new(id, format!("{mark}{}", tr(m.name()))));
        }
        v.extend([
            MenuItem::new("undo", "Undo"),
            MenuItem::new("redo", "Redo"),
            MenuItem::new("open", "Open..."),
            MenuItem::new("save", "Save"),
            MenuItem::new("saveas", "Save As..."),
            MenuItem::new("export", "Export PNG"),
        ]);
        v
    }

    fn context_menu(&mut self, x: i32, y: i32, _ctx: &Ctx) -> Option<Vec<MenuItem>> {
        if self.editor.is_some() || !contains(Layout::now(self.maximized).canvas(), x, y) {
            return None;
        }
        Some(vec![
            MenuItem::new("home", "Reset view"),
            MenuItem::new("grid", if self.doc.grid { "Hide grid" } else { "Show grid" }),
            MenuItem::new("max", if self.maximized { "Show sliders" } else { "Maximize" }),
            MenuItem::new("seed", "New seed"),
            MenuItem::new("play", if self.playing { "Pause" } else { "Play" }),
            MenuItem::new("export", "Export PNG"),
        ])
    }

    fn menu_action(&mut self, id: &str, ctx: &Ctx) -> AppAction {
        self.editor = None;
        self.command(id, ctx)
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        self.load_file(path)
    }

    fn request_close(&mut self) -> bool {
        if !self.dirty {
            return true;
        }
        self.editor = None;
        self.modal = Some(Dialog::new(DialogKind::Card(UnsavedCard::new(&self.doc.title)), Ask::Unsaved));
        false
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, ctx: &Ctx) -> AppAction {
        if let Some(d) = self.modal.as_mut() {
            if let Some(out) = d.key(key) {
                return self.answer(out);
            }
            return AppAction::None;
        }
        if self.editor.is_some() {
            self.editor_key(key, mods);
            return AppAction::None;
        }
        let ctrl = mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
        let shift = mods.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
        if ctrl {
            let id = match key {
                Keycode::Z if shift => "redo",
                Keycode::Z => "undo",
                Keycode::Y => "redo",
                Keycode::S => "save",
                Keycode::O => "open",
                Keycode::E => "export",
                _ => return AppAction::None,
            };
            return self.command(id, ctx);
        }
        let n = self.rows().len();
        match crate::platform::keys::arrow_alias(key) {
            Keycode::Escape => {
                if self.maximized {
                    self.toggle_max();
                } else {
                    return AppAction::Quit;
                }
            }
            Keycode::Up => {
                self.set_cursor((self.cursor + n - 1) % n);
                self.note(self.row_text(self.row()));
            }
            Keycode::Down => {
                self.set_cursor((self.cursor + 1) % n);
                self.note(self.row_text(self.row()));
            }
            Keycode::Left => self.nudge(-1, shift),
            Keycode::Right => self.nudge(1, shift),
            Keycode::Return | Keycode::KpEnter | Keycode::E => self.open_editor(),
            Keycode::Space => self.playing = !self.playing,
            Keycode::P => self.ask_preset(),
            Keycode::PageUp | Keycode::PageDown => {
                let m = presets::PRESETS.len();
                let i = self.preset_index().unwrap_or(0);
                self.load_preset(if key == Keycode::PageDown { (i + 1) % m } else { (i + m - 1) % m });
                self.note(tr_s(&self.doc.title));
            }
            Keycode::M => self.toggle_max(),
            Keycode::G => return self.command("grid", ctx),
            Keycode::R => return self.command("seed", ctx),
            Keycode::Equals | Keycode::Plus | Keycode::KpPlus => self.zoom_at(0.8, None),
            Keycode::Minus | Keycode::KpMinus => self.zoom_at(1.25, None),
            Keycode::Num0 | Keycode::Kp0 | Keycode::Home => return self.command("home", ctx),
            _ => {}
        }
        AppAction::None
    }

    fn text(&mut self, text: &str, _ctx: &Ctx) -> AppAction {
        if let Some(d) = self.modal.as_mut() {
            d.text(text);
        } else if let Some(e) = self.editor.as_mut() {
            e.lines[e.focus].insert(text);
            self.check_editor();
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, _ctx: &Ctx) -> AppAction {
        if let Some(d) = self.modal.as_mut() {
            match ev {
                PointerEvent::Down { x, y, button: 0, .. } => {
                    if let Some(out) = d.click(x, y) {
                        return self.answer(out);
                    }
                }
                PointerEvent::Wheel { dy, .. } => d.wheel(dy),
                _ => {}
            }
            return AppAction::None;
        }
        if let Some(e) = self.editor.as_mut() {
            if let PointerEvent::Down { x, y, button: 0, .. } = ev {
                let (cancel, apply) = editor_buttons();
                if let Some(k) = (0..e.lines.len()).find(|k| contains(editor_field(*k), x, y)) {
                    e.focus = k;
                } else if contains(apply, x, y) {
                    self.apply_editor();
                } else if contains(cancel, x, y) {
                    self.editor = None;
                }
            }
            return AppAction::None;
        }
        let l = Layout::now(self.maximized);
        match ev {
            PointerEvent::Down { x, y, button: 0, clicks } => {
                if contains(l.canvas(), x, y) {
                    if clicks >= 2 {
                        self.toggle_max();
                    } else {
                        self.drag = Some(Drag::Pan { before: self.doc.clone() });
                    }
                } else {
                    self.panel_click(x, y, clicks);
                }
            }
            PointerEvent::Drag { x, dx, dy, .. } => match &self.drag {
                Some(Drag::Slider { row, .. }) => {
                    let (row, i) = (*row, match self.rows()[*row] {
                        Row::Param(i) => i,
                        _ => return AppAction::None,
                    });
                    let s = l.slider(row.saturating_sub(self.scroll));
                    self.doc.params[i].set_frac(s.value_at(x) as f64 / 100.0);
                    self.stale = true;
                }
                Some(Drag::Pan { .. }) => {
                    let c = l.canvas();
                    let s = self.doc.view.span / c.width().min(c.height()) as f64;
                    self.doc.view.cx -= dx as f64 * s;
                    self.doc.view.cy += dy as f64 * s;
                    self.stale = true;
                }
                None => {}
            },
            PointerEvent::Up { button: 0, .. } => self.end_drag(),
            PointerEvent::Wheel { x, y, dy } => {
                if contains(l.canvas(), x, y) {
                    self.zoom_at(if dy > 0 { 0.8 } else { 1.25 }, Some((x, y)));
                } else if let Some(k) = self.row_at(x, y) {
                    if matches!(self.rows()[k], Row::Param(_)) {
                        self.set_cursor(k);
                        self.nudge(dy.signum(), false);
                    } else {
                        let n = self.rows().len();
                        let vis = l.visible_rows();
                        self.scroll = (self.scroll as i32 - dy).clamp(0, n.saturating_sub(vis) as i32) as usize;
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, _ctx: &Ctx) -> bool {
        let now = Instant::now();
        let dt = self.last_tick.map_or(0.0, |l| (now - l).as_secs_f64().min(0.25));
        self.last_tick = Some(now);
        let mut repaint = false;
        if self.playing && self.doc.uses_time() && self.editor.is_none() && self.modal.is_none() {
            self.t += dt * self.doc.speed;
            repaint = true;
        }
        if self.flash.as_ref().is_some_and(|(_, at)| at.elapsed() < Duration::from_millis(1600)) {
            repaint = true;
        }
        repaint |= self.render(Duration::from_millis(12));
        repaint || self.job.is_some()
    }

    fn draw(&mut self, c: &mut Canvas, _assets: &mut AssetStore, ctx: &Ctx) {
        let tm = ctx.tm;
        ui::fill_rect(c, Rect::new(0, grid::BAR_H, grid::sw() as u32, (grid::sh() - grid::BAR_H) as u32), tm.get_color("home_bg"));
        if let Some(e) = &self.editor {
            self.draw_editor(c, ctx, e);
        } else {
            let l = Layout::now(self.maximized);
            let area = l.canvas();
            let size = (area.width() as usize, area.height() as usize);
            if size != self.size {
                self.size = size;
                self.stale = true;
                // The first picture of a new size is made at once, so it never shows empty.
                self.render(Duration::from_millis(40));
            }
            let ramp = tones::Ramp::of(tm, crate::launcher::settings::panel_is_mono());
            self.draw_canvas(c, ctx, &l, &ramp);
            self.draw_panel(c, ctx, &l);
        }
        if let Some(d) = &self.modal {
            d.draw(c, ctx.fs, tm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::FontSystem;
    use crate::system::mock::MockBackend;
    use crate::theme::{Theme, ThemeManager};

    fn run(f: impl FnOnce(&mut Graph, &Ctx)) {
        let fs = FontSystem::new();
        let tm = ThemeManager::new(Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = MockBackend::new();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut g = Graph::new();
        g.size = (200, 140);
        f(&mut g, &ctx);
    }

    fn key(g: &mut Graph, ctx: &Ctx, k: Keycode) -> AppAction {
        g.key(k, Mod::NOMOD, false, ctx)
    }

    fn finish(g: &mut Graph) {
        for _ in 0..1000 {
            g.render(Duration::from_millis(50));
            if g.job.is_none() && !g.stale {
                return;
            }
        }
        panic!("never finished");
    }

    #[test]
    fn the_layout_at_320x170_keeps_to_the_grid() {
        let l = Layout::now(false);
        let p = l.panel.unwrap();
        assert_eq!(l.frame, Rect::new(4, 18, 204, 149));
        assert_eq!(p, Rect::new(212, 18, 104, 149));
        assert_eq!(p.right(), grid::SCROLL_X + grid::SCROLL_W, "the panel ends at the right margin");
        assert_eq!(l.frame.bottom() - 1, 166);
        assert_eq!(l.visible_rows(), 8);
        assert!(l.row_rect(7).bottom() <= p.bottom(), "the last row fits");
        // A slider row: name, track and value do not overlap.
        let s = l.slider(0);
        assert!(s.track.x() > p.x() + NAME_W);
        assert!(s.track.right() + 30 <= p.right(), "room for a value like -0.13");
        assert!(s.thumb(100).bottom() <= l.row_rect(0).bottom() - 4, "the thumb and ticks stay in the row");
        let m = Layout::now(true);
        assert!(m.panel.is_none() && m.frame.width() == 318);
    }

    #[test]
    fn it_opens_on_a_preset_and_draws() {
        run(|g, _| {
            assert_eq!(g.doc.title, "Rose");
            finish(g);
            let s = g.shown.as_ref().unwrap();
            assert_eq!((s.raster.w, s.raster.h), (200, 140));
            assert!(s.raster.px.contains(&render::INK));
        });
    }

    #[test]
    fn arrows_move_sliders_and_undo_takes_back_a_run_of_presses_at_once() {
        run(|g, ctx| {
            // Rows: r=..., k, turns, pen.
            assert_eq!(g.rows(), [Row::Formula(0), Row::Param(0), Row::Param(1), Row::Param(2)]);
            key(g, ctx, Keycode::Down);
            assert_eq!(g.row(), Row::Param(0));
            assert_eq!(g.doc.param("k"), Some(5.0));
            for _ in 0..3 {
                key(g, ctx, Keycode::Right);
            }
            assert_eq!(g.doc.param("k"), Some(8.0));
            assert!(g.stale && g.dirty);
            key(g, ctx, Keycode::Down);
            key(g, ctx, Keycode::Right);
            assert_eq!(g.doc.param("turns"), Some(2.0));
            g.key(Keycode::Z, Mod::LCTRLMOD, false, ctx);
            assert_eq!(g.doc.param("turns"), Some(1.0));
            assert_eq!(g.doc.param("k"), Some(8.0));
            g.key(Keycode::Z, Mod::LCTRLMOD, false, ctx);
            assert_eq!(g.doc.param("k"), Some(5.0), "the three presses were one step");
            assert!(matches!(g.key(Keycode::Z, Mod::LCTRLMOD, false, ctx), AppAction::Toast(_)), "nothing more to undo");
            g.key(Keycode::Y, Mod::LCTRLMOD, false, ctx);
            assert_eq!(g.doc.param("k"), Some(8.0));
        });
    }

    #[test]
    fn the_editor_makes_sliders_for_new_names_and_refuses_broken_formulas() {
        run(|g, ctx| {
            key(g, ctx, Keycode::Return);
            assert!(g.editor.is_some() && g.takes_text());
            g.key(Keycode::U, Mod::LCTRLMOD, false, ctx);
            g.text("a*cos(k*θ", ctx);
            assert!(g.editor.as_ref().unwrap().error.is_some());
            key(g, ctx, Keycode::Return);
            assert!(g.editor.is_some(), "a broken formula is not applied");
            g.text(") + b", ctx);
            assert!(g.editor.as_ref().unwrap().error.is_none());
            key(g, ctx, Keycode::Return);
            assert!(g.editor.is_none());
            assert_eq!(g.doc.formulas[0], "a*cos(k*θ) + b");
            let names: Vec<&str> = g.doc.params.iter().map(|p| p.name.as_str()).collect();
            assert_eq!(names, ["a", "k", "b", "turns", "pen"]);
            assert_eq!(g.doc.param("k"), Some(5.0), "k keeps its value");
            finish(g);
            assert!(g.error.is_none());
            // Esc leaves the editor without changing anything.
            key(g, ctx, Keycode::Return);
            g.text("zzz", ctx);
            key(g, ctx, Keycode::Escape);
            assert!(g.editor.is_none());
            assert_eq!(g.doc.formulas[0], "a*cos(k*θ) + b");
            g.key(Keycode::Z, Mod::LCTRLMOD, false, ctx);
            assert_eq!(g.doc.formulas[0], "cos(k*theta)");
        });
    }

    #[test]
    fn every_mode_from_the_menu_shows_a_picture() {
        run(|g, ctx| {
            for m in Mode::ALL {
                g.menu_action(&format!("mode_{}", m.id()), ctx);
                assert_eq!(g.doc.mode, m);
                finish(g);
                assert!(g.error.is_none(), "{m:?}");
                let s = g.shown.as_ref().unwrap();
                let first = s.raster.px[0];
                assert!(s.raster.px.iter().any(|v| *v != first), "{m:?} is blank");
            }
            assert!(g.menu().iter().any(|i| i.label.starts_with("* ")), "the current mode is marked");
            assert!(g.menu().len() + 1 <= 18, "the menu fits in two columns");
        });
    }

    #[test]
    fn presets_step_with_page_keys_and_the_picker() {
        run(|g, ctx| {
            key(g, ctx, Keycode::PageDown);
            assert_eq!(g.doc.title, presets::PRESETS[1].0);
            key(g, ctx, Keycode::PageUp);
            key(g, ctx, Keycode::PageUp);
            assert_eq!(g.doc.title, presets::PRESETS.last().unwrap().0);
            key(g, ctx, Keycode::P);
            assert!(g.modal.is_some());
            key(g, ctx, Keycode::Up);
            key(g, ctx, Keycode::Return);
            assert!(g.modal.is_none());
            let n = presets::PRESETS.len();
            assert_eq!(g.doc.title, presets::PRESETS[n - 2].0, "the picker starts on the current preset");
        });
    }

    #[test]
    fn dragging_a_slider_is_one_undo_step() {
        run(|g, ctx| {
            grid::with_surface((320, 170), || {
                let l = Layout::now(false);
                let s = l.slider(1);
                let y = s.track.y() + 1;
                g.pointer(PointerEvent::Down { x: s.track.x() + 2, y, button: 0, clicks: 1 }, ctx);
                assert_eq!(g.row(), Row::Param(0));
                assert_eq!(g.doc.param("k"), Some(1.0), "the left end");
                g.pointer(PointerEvent::Drag { x: s.track.right(), y, dx: 30, dy: 0 }, ctx);
                assert_eq!(g.doc.param("k"), Some(12.0), "the right end");
                // While dragging a heavy mode would preview at half size; a curve stays sharp.
                g.render(Duration::from_millis(20));
                g.pointer(PointerEvent::Up { x: s.track.right(), y, button: 0 }, ctx);
                assert!(g.drag.is_none() && g.stale);
                g.key(Keycode::Z, Mod::LCTRLMOD, false, ctx);
                assert_eq!(g.doc.param("k"), Some(5.0));
            });
        });
    }

    #[test]
    fn panning_and_zooming_move_the_plane() {
        run(|g, ctx| {
            grid::with_surface((320, 170), || {
                let c = Layout::now(false).canvas();
                let (x, y) = (c.x() + 50, c.y() + 50);
                let before = g.doc.view;
                g.pointer(PointerEvent::Down { x, y, button: 0, clicks: 1 }, ctx);
                g.pointer(PointerEvent::Drag { x: x + 10, y, dx: 10, dy: 0 }, ctx);
                g.pointer(PointerEvent::Up { x: x + 10, y, button: 0 }, ctx);
                assert!(g.doc.view.cx < before.cx, "dragging right shows what is to the left");
                assert_eq!(g.doc.view.cy, before.cy);
                g.pointer(PointerEvent::Wheel { x: c.x() + c.width() as i32 / 2, y: c.y() + c.height() as i32 / 2, dy: 1 }, ctx);
                assert!(g.doc.view.span < before.span);
                key(g, ctx, Keycode::Num0);
                assert_eq!(g.doc.view, before);
                g.pointer(PointerEvent::Down { x, y, button: 0, clicks: 2 }, ctx);
                assert!(g.maximized, "a double click maximizes");
                key(g, ctx, Keycode::Escape);
                assert!(!g.maximized);
            });
        });
    }

    #[test]
    fn files_save_open_and_export() {
        run(|g, ctx| {
            let dir = std::env::temp_dir().join(format!("harbour-graph-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            key(g, ctx, Keycode::Down);
            key(g, ctx, Keycode::Right);
            let path = dir.join("rose");
            assert!(matches!(g.write(&path), AppAction::Toast(_)));
            assert!(!g.dirty);
            let file = dir.join("rose.graph");
            assert!(file.exists(), "the extension is added");
            let text = std::fs::read_to_string(&file).unwrap();
            assert!(text.contains("r = cos(k*theta)") && text.contains("param: k 6 1 12 int"), "{text}");
            g.load_preset(4);
            g.open_path(&file).unwrap();
            assert_eq!(g.doc.param("k"), Some(6.0));
            assert_eq!(g.path.as_deref(), Some(file.as_path()));
            assert!(g.open_path(&dir.join("nothing.graph")).is_err());
            std::fs::write(dir.join("bad.graph"), "hello").unwrap();
            assert!(g.open_path(&dir.join("bad.graph")).is_err());
            assert_eq!(g.doc.param("k"), Some(6.0), "a bad file changes nothing");
            // Export: a PNG of the picture.
            finish(g);
            let png = crate::launcher::apps::camera::png::encode_rgb(2, 1, &[0, 0, 0, 255, 255, 255]);
            assert_eq!(&png[1..4], b"PNG");
            let r = g.picture().unwrap();
            assert_eq!((r.w, r.h), (200, 140));
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn leaving_with_changes_asks_first() {
        run(|g, ctx| {
            assert!(g.request_close(), "an untouched preset just closes");
            key(g, ctx, Keycode::Down);
            key(g, ctx, Keycode::Right);
            assert!(!g.request_close());
            assert!(g.modal.is_some());
            assert_eq!(key(g, ctx, Keycode::D), AppAction::Quit, "Don't Save leaves");
        });
    }

    #[test]
    fn a_broken_formula_keeps_the_old_picture_and_says_why() {
        run(|g, _| {
            finish(g);
            let old = g.shown.as_ref().unwrap().raster.clone();
            g.doc.formulas[0] = "cos(q".into();
            g.stale = true;
            g.render(Duration::from_millis(20));
            assert_eq!(g.error.as_ref().map(|e| e.0), Some(0));
            assert_eq!(g.shown.as_ref().unwrap().raster, old);
        });
    }

    #[test]
    fn time_runs_only_while_playing() {
        run(|g, ctx| {
            g.load_preset(presets::PRESETS.iter().position(|p| p.0 == "Lissajous").unwrap());
            assert!(g.playing, "a moving preset starts playing");
            assert_eq!(*g.rows().last().unwrap(), Row::Time);
            g.last_tick = Some(Instant::now() - Duration::from_millis(100));
            g.tick(ctx);
            assert!(g.t > 0.05);
            key(g, ctx, Keycode::Space);
            let t = g.t;
            g.last_tick = Some(Instant::now() - Duration::from_millis(100));
            g.tick(ctx);
            assert_eq!(g.t, t);
            // On the time row, Left / Right step t.
            g.set_cursor(g.rows().len() - 1);
            key(g, ctx, Keycode::Right);
            assert!((g.t - t - 0.1).abs() < 1e-9);
        });
    }

    #[test]
    fn help_rows_are_filled_in() {
        crate::launcher::help::check_app_rows("Graph", &Graph::new().help());
    }
}
