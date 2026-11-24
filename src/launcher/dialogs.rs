//! Dialogs the host shows over an app:
//!
//! * [`UnsavedCard`]: the "Save changes before closing?" alert with Don't Save / Cancel / Save.
//! * [`SavePanel`]: a "Save as" panel (name field, folder list, Cancel / Save) that says where a
//!   file goes.
//!
//! Both are plain state machines: the host feeds them keys, text and clicks and draws them on top
//! of whatever is on screen.

use std::path::{Path, PathBuf};

use sdl2::keyboard::Keycode;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::chrome;
use crate::dither::{self, Pattern};
use crate::font::FontSystem;
use crate::grid;
use crate::launcher::alert::AlertIcon;
use crate::launcher::i18n;
use crate::launcher::sysicon;
use crate::launcher::widgets::{self, contains, draw_button, draw_field, draw_panel, BtnState, Field};
use crate::theme::ThemeManager;
use crate::ui;

/// Dim everything behind a dialog with a light dither.
fn dim_backdrop(c: &mut Canvas<Window>, tm: &ThemeManager) {
    let r = Rect::new(0, grid::BAR_H, grid::sw() as u32, (grid::sh() - grid::BAR_H) as u32);
    dither::fill(c, r, tm.get_color("widget_shadow"), Pattern::Eighth);
}

fn frame(c: &mut Canvas<Window>, tm: &ThemeManager, r: Rect) {
    draw_panel(c, tm, r);
}

/// The system icon at a card's top left (when its file exists) and the x the card's text starts at:
/// right of the icon, or where it always was when there is no icon.
/// A two- or three-button alert card, centred under the menu strip: 272 px wide, or 304 when the
/// system icon is there (its 42 px would otherwise squeeze the text).
fn alert_card(icon: AlertIcon, h: u32) -> Rect {
    let w = if sysicon::available(icon) { 304u32 } else { 272 };
    Rect::new((grid::sw() as i32 - w as i32) / 2, grid::BAR_H + (grid::sh() - grid::BAR_H - h as i32) / 2, w, h)
}

fn card_icon(c: &mut Canvas<Window>, tm: &ThemeManager, r: Rect, icon: AlertIcon) -> i32 {
    if sysicon::draw(c, tm, icon, r.x() + 12, r.y() + 10) {
        r.x() + 12 + sysicon::SIZE + 10
    } else {
        r.x() + 14
    }
}

// ---------------------------------------------------------------- "save changes?" alert

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardChoice {
    DontSave,
    Cancel,
    Save,
}

const CARD_BUTTONS: [(&str, CardChoice); 3] = [("Don't Save", CardChoice::DontSave), ("Cancel", CardChoice::Cancel), ("Save", CardChoice::Save)];

pub struct UnsavedCard {
    pub name: String,
    /// Highlighted button (0 = Don't Save, 1 = Cancel, 2 = Save).
    pub sel: usize,
}

impl UnsavedCard {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), sel: 2 }
    }

    pub fn key(&mut self, key: Keycode) -> Option<CardChoice> {
        match key {
            Keycode::Left | Keycode::Z => self.sel = self.sel.saturating_sub(1),
            Keycode::Right | Keycode::C => self.sel = (self.sel + 1).min(2),
            Keycode::Return | Keycode::KpEnter => return Some(CARD_BUTTONS[self.sel].1),
            Keycode::Escape => return Some(CardChoice::Cancel),
            Keycode::D => return Some(CardChoice::DontSave),
            Keycode::S => return Some(CardChoice::Save),
            _ => {}
        }
        None
    }

    fn card_rect() -> Rect {
        alert_card(AlertIcon::Caution, 92)
    }

    fn button_rects() -> [Rect; 3] {
        let r = Self::card_rect();
        let y = r.y() + r.height() as i32 - 32;
        [
            Rect::new(r.x() + 10, y, 84, 24),
            Rect::new(r.x() + r.width() as i32 - 10 - 64 - 8 - 64, y, 64, 24),
            Rect::new(r.x() + r.width() as i32 - 10 - 64, y, 64, 24),
        ]
    }

    pub fn click(&mut self, x: i32, y: i32) -> Option<CardChoice> {
        for (i, b) in Self::button_rects().iter().enumerate() {
            if contains(*b, x, y) {
                self.sel = i;
                return Some(CARD_BUTTONS[i].1);
            }
        }
        None
    }

    pub fn draw(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        dim_backdrop(c, tm);
        let r = Self::card_rect();
        frame(c, tm, r);
        // Caution: unsaved work is about to be lost.
        let tx = card_icon(c, tm, r, AlertIcon::Caution);
        let room = r.right() - 12 - tx;
        let name = chrome::fit_display_text(fs, &self.name, room - 8);
        fs.draw_display_text(c, tx, r.y() + 8, &chrome::fit_display_text(fs, i18n::tr("Save changes to"), room), ink);
        fs.draw_display_text(c, tx, r.y() + 22, &format!("{name}?"), ink);
        fs.draw_text(c, tx, r.y() + 38, &chrome::fit_text(fs, i18n::tr("Your changes are lost if you don't save."), room), crate::launcher::widgets::secondary_ink(tm));
        for (i, b) in Self::button_rects().iter().enumerate() {
            let st = BtnState { default: i == 2, pressed: false, hover: i == self.sel && i != 2, disabled: false };
            draw_button(c, fs, tm, *b, CARD_BUTTONS[i].0, st);
            if i == self.sel {
                // A dotted focus rectangle on the chosen button.
                dither::fill(c, Rect::new(b.x() + 2, b.y() + 2, b.width() - 4, 1), ink, Pattern::Half);
                dither::fill(c, Rect::new(b.x() + 2, b.y() + b.height() as i32 - 3, b.width() - 4, 1), ink, Pattern::Half);
            }
        }
    }
}

// ---------------------------------------------------------------------- "save as" panel

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveResult {
    Cancel,
    Save(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// The file list: whole 16 px rows like every other list (four of them, so the panel still fits
/// between the status bar and the bottom of the screen).
const ROWS: usize = 4;
const ROW_H: i32 = grid::ROW_H;
const NAME_MAX: usize = 48;

pub struct SavePanel {
    pub dir: PathBuf,
    pub name: String,
    pub entries: Vec<Entry>,
    pub sel: usize,
    pub scroll: usize,
    /// A file with this name already exists: the next Enter replaces it.
    pub confirm_replace: bool,
    pub message: Option<String>,
    /// The suggested name is shown selected: the first typed character or Backspace replaces it.
    pub replace_on_type: bool,
    /// Label left of the name field ("Save as:", "Open:", "New file:").
    pub title: String,
    /// Label of the default button.
    pub button: String,
    /// Open mode: pick an existing file. The name field only shows the choice; Enter on a folder
    /// goes into it, Enter on a file returns it. `exts` (lower case, may be empty) filters the files.
    pub open_mode: bool,
    exts: Vec<String>,
    root: PathBuf,
    last_click: Option<(usize, std::time::Instant)>,
}

impl SavePanel {
    /// An "Open" panel: pick an existing file (only those with one of `exts`, if any are given).
    pub fn open(dir: &Path, root: &Path, exts: &[&str]) -> Self {
        let mut p = Self::new(dir, "", root);
        p.title = "Open:".into();
        p.button = "Open".into();
        p.open_mode = true;
        p.replace_on_type = false;
        p.exts = exts.iter().map(|e| e.to_ascii_lowercase()).collect();
        p.refresh();
        p.pick_selected_name();
        p
    }

    /// Change the labels (for example "New file:" / "Create").
    pub fn titled(mut self, title: &str, button: &str) -> Self {
        self.title = title.to_string();
        self.button = button.to_string();
        self
    }

    /// `dir`: the folder to start in; `name`: the suggested file name. `root` is the highest
    /// folder the panel may go up to (the user's home).
    pub fn new(dir: &Path, name: &str, root: &Path) -> Self {
        let mut p = Self {
            dir: dir.to_path_buf(),
            name: name.to_string(),
            entries: Vec::new(),
            sel: 0,
            scroll: 0,
            confirm_replace: false,
            message: None,
            replace_on_type: !name.is_empty(),
            title: "Save as:".into(),
            button: "Save".into(),
            open_mode: false,
            exts: Vec::new(),
            root: root.to_path_buf(),
            last_click: None,
        };
        p.refresh();
        p
    }

    /// Re-read the folder: ".." first (unless at the top), then folders, then files, each sorted.
    pub fn refresh(&mut self) {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let is_dir = e.path().is_dir();
                if is_dir {
                    dirs.push(Entry { name, is_dir });
                } else if self.exts.is_empty() || Self::ext_ok(&self.exts, &name) {
                    files.push(Entry { name, is_dir });
                }
            }
        }
        dirs.sort_by_key(|e| e.name.to_lowercase());
        files.sort_by_key(|e| e.name.to_lowercase());
        self.entries.clear();
        if self.dir != self.root && self.dir.parent().is_some() {
            self.entries.push(Entry { name: "..".into(), is_dir: true });
        }
        self.entries.extend(dirs);
        self.entries.extend(files);
        self.sel = self.sel.min(self.entries.len().saturating_sub(1));
        self.scroll = 0;
        self.keep_selection_visible();
    }

    fn ext_ok(exts: &[String], name: &str) -> bool {
        let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        exts.iter().any(|e| *e == ext)
    }

    fn keep_selection_visible(&mut self) {
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + ROWS {
            self.scroll = self.sel + 1 - ROWS;
        }
    }

    pub fn target(&self) -> PathBuf {
        self.dir.join(self.name.trim())
    }

    fn enter_dir(&mut self, name: &str) {
        if name == ".." {
            if let Some(p) = self.dir.parent() {
                self.dir = p.to_path_buf();
            }
        } else {
            self.dir = self.dir.join(name);
        }
        self.sel = 0;
        self.confirm_replace = false;
        self.message = None;
        self.refresh();
    }

    fn go_up(&mut self) {
        if self.dir != self.root {
            self.enter_dir("..");
        }
    }

    /// Typed text goes into the name field.
    pub fn text(&mut self, t: &str) {
        if self.open_mode {
            return;
        }
        if self.replace_on_type && t.chars().any(|ch| !ch.is_control() && ch != '/') {
            self.name.clear();
            self.replace_on_type = false;
        }
        for ch in t.chars() {
            if !ch.is_control() && ch != '/' && self.name.chars().count() < NAME_MAX {
                self.name.push(ch);
            }
        }
        self.confirm_replace = false;
        self.message = None;
    }

    fn try_open(&mut self) -> Option<SaveResult> {
        match self.entries.get(self.sel).cloned() {
            Some(e) if e.is_dir => {
                self.enter_dir(&e.name);
                None
            }
            Some(e) => Some(SaveResult::Save(self.dir.join(e.name))),
            None => {
                self.message = Some("No files here".into());
                None
            }
        }
    }

    fn try_save(&mut self) -> Option<SaveResult> {
        if self.open_mode {
            return self.try_open();
        }
        let name = self.name.trim();
        if name.is_empty() || name == "." || name == ".." {
            self.message = Some("Type a file name".into());
            return None;
        }
        let target = self.target();
        if target.is_dir() {
            self.message = Some("That name is a folder".into());
            return None;
        }
        if target.exists() && !self.confirm_replace {
            self.confirm_replace = true;
            self.message = Some(format!("\"{}\" exists. Enter again to replace it", name));
            return None;
        }
        Some(SaveResult::Save(target))
    }

    pub fn key(&mut self, key: Keycode) -> Option<SaveResult> {
        match key {
            Keycode::Escape => return Some(SaveResult::Cancel),
            Keycode::Return | Keycode::KpEnter => return self.try_save(),
            Keycode::Up => {
                self.sel = self.sel.saturating_sub(1);
                self.pick_selected_name();
            }
            Keycode::Down => {
                if self.sel + 1 < self.entries.len() {
                    self.sel += 1;
                }
                self.pick_selected_name();
            }
            Keycode::Right => {
                if let Some(e) = self.entries.get(self.sel).cloned() {
                    if e.is_dir {
                        self.enter_dir(&e.name);
                    }
                }
            }
            Keycode::Left => self.go_up(),
            Keycode::Backspace if self.open_mode => self.go_up(),
            Keycode::Backspace => {
                if self.replace_on_type {
                    self.name.clear();
                    self.replace_on_type = false;
                } else {
                    self.name.pop();
                }
                self.confirm_replace = false;
                self.message = None;
            }
            _ => {}
        }
        self.keep_selection_visible();
        None
    }

    /// Choosing a file copies its name into the field (as on the Mac); a folder leaves it alone.
    fn pick_selected_name(&mut self) {
        self.confirm_replace = false;
        self.message = None;
        if let Some(e) = self.entries.get(self.sel) {
            if !e.is_dir {
                self.name = e.name.clone();
                self.replace_on_type = !self.open_mode;
            } else if self.open_mode {
                self.name.clear();
            }
        }
    }

    // ---- geometry (also used for clicks)
    fn panel_rect() -> Rect {
        let (w, h) = (292u32, 142u32);
        Rect::new((grid::sw() as i32 - w as i32) / 2, grid::BAR_H + (grid::sh() - grid::BAR_H - h as i32) / 2, w, h)
    }
    fn name_rect(p: Rect) -> Rect {
        Rect::new(p.x() + 66, p.y() + 8, p.width() - 66 - 10, crate::launcher::widgets::FIELD_H as u32)
    }
    fn list_rect(p: Rect) -> Rect {
        Rect::new(p.x() + 10, p.y() + 46, p.width() - 20 - 4, ROWS as u32 * ROW_H as u32 + 2)
    }
    fn button_rects(p: Rect) -> [Rect; 2] {
        let y = p.y() + p.height() as i32 - 26;
        [Rect::new(p.x() + p.width() as i32 - 10 - 64 - 8 - 64, y, 64, 22), Rect::new(p.x() + p.width() as i32 - 10 - 64, y, 64, 22)]
    }

    pub fn click(&mut self, x: i32, y: i32) -> Option<SaveResult> {
        let p = Self::panel_rect();
        let [cancel, save] = Self::button_rects(p);
        if contains(cancel, x, y) {
            return Some(SaveResult::Cancel);
        }
        if contains(save, x, y) {
            return self.try_save();
        }
        let l = Self::list_rect(p);
        if contains(l, x, y) {
            let row = ((y - l.y() - 1) / ROW_H).max(0) as usize + self.scroll;
            if row < self.entries.len() {
                let double = self.last_click.map(|(r, t)| r == row && t.elapsed() < std::time::Duration::from_millis(450)).unwrap_or(false);
                self.sel = row;
                self.pick_selected_name();
                if double {
                    if let Some(e) = self.entries.get(row).cloned() {
                        if e.is_dir {
                            self.enter_dir(&e.name);
                        } else {
                            return self.try_save();
                        }
                    }
                    self.last_click = None;
                } else {
                    self.last_click = Some((row, std::time::Instant::now()));
                }
            }
        }
        None
    }

    pub fn wheel(&mut self, dy: i32) {
        if dy > 0 {
            self.scroll = self.scroll.saturating_sub(1);
        } else if self.scroll + ROWS < self.entries.len() {
            self.scroll += 1;
        }
    }

    pub fn draw(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        let faint = crate::launcher::widgets::secondary_ink(tm);
        let sunken = tm.get_color("surface_sunken");
        dim_backdrop(c, tm);
        let p = Self::panel_rect();
        frame(c, tm, p);

        // Name field.
        fs.draw_display_text(c, p.x() + 10, p.y() + 9, &chrome::fit_display_text(fs, &i18n::tr_s(&self.title), 54), ink);
        let nr = Self::name_rect(p);
        let field = Field { text: &self.name, caret: (!self.open_mode).then(|| self.name.chars().count()), select_all: self.replace_on_type, ..Default::default() };
        draw_field(c, fs, tm, nr, &field);

        // Folder line.
        let home = std::env::var("HOME").unwrap_or_default();
        let dir = self.dir.to_string_lossy().to_string();
        let dir = match dir.strip_prefix(&home) {
            Some(rest) if !home.is_empty() => format!("~{rest}"),
            _ => dir,
        };
        let line = chrome::fit_display_text(fs, &i18n::tr_s(&format!("in {dir}")), p.width() as i32 - 24);
        fs.draw_text(c, p.x() + 10, p.y() + 29, &line, faint);

        // File list.
        let l = Self::list_rect(p);
        ui::fill_rect(c, l, sunken);
        ui::draw_rect(c, l, ink);
        for i in 0..ROWS {
            let idx = self.scroll + i;
            let Some(e) = self.entries.get(idx) else { break };
            let row = Rect::new(l.x() + 1, l.y() + 1 + i as i32 * ROW_H, l.width() - 2, ROW_H as u32);
            let hi = idx == self.sel;
            if hi {
                ui::fill_rect(c, row, ink);
            }
            let fg = if hi { tm.get_color("widget_face_active") } else if e.is_dir { ink } else { faint };
            // Folder glyph: a small tab-and-body box; files get a page (8 px, centred in the row).
            let (gx, gy) = (row.x() + 3, row.y() + (ROW_H - 8) / 2);
            c.set_draw_color(fg);
            if e.is_dir {
                let _ = c.fill_rect(Rect::new(gx, gy, 4, 2));
                let _ = c.fill_rect(Rect::new(gx, gy + 2, 9, 6));
            } else {
                let _ = c.draw_rect(Rect::new(gx + 1, gy, 6, 8));
            }
            let label = chrome::fit_display_text(fs, &e.name, row.width() as i32 - 24);
            fs.draw_text(c, row.x() + 15, widgets::text_origin(row.y(), ROW_H), &label, fg);
        }
        if self.entries.len() > ROWS {
            let track = Rect::new(l.x() + l.width() as i32 + 1, l.y(), 3, l.height());
            ui::fill_rect(c, track, tm.get_color("line"));
            let th = ((track.height() as usize * ROWS) / self.entries.len()).max(6) as u32;
            let span = track.height() - th;
            let ty = track.y() + (span as usize * self.scroll / (self.entries.len() - ROWS).max(1)) as i32;
            ui::fill_rect(c, Rect::new(track.x(), ty, 3, th), ink);
        }

        // Message + buttons.
        let [cancel, save] = Self::button_rects(p);
        if let Some(m) = &self.message {
            let m = chrome::fit_display_text(fs, &i18n::tr_s(m), cancel.x() - p.x() - 18);
            fs.draw_text(c, p.x() + 10, cancel.y() + 4, &m, ink);
        }
        draw_button(c, fs, tm, cancel, "Cancel", BtnState::default());
        draw_button(c, fs, tm, save, &self.button, BtnState { default: true, ..Default::default() });
    }
}

// ------------------------------------------------------------- name prompt (new folder, rename)

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptResult {
    Cancel,
    Ok(String),
}

/// A small "type a name" box: title, one text field with the suggestion selected, Cancel / OK.
pub struct NamePrompt {
    pub title: String,
    pub name: String,
    /// The suggestion is selected: the first typing replaces it, Backspace clears it.
    pub replace_on_type: bool,
    pub max: usize,
}

impl NamePrompt {
    pub fn new(title: &str, default: &str) -> Self {
        Self { title: title.to_string(), name: default.to_string(), replace_on_type: true, max: 24 }
    }

    pub fn text(&mut self, t: &str) {
        if self.replace_on_type && t.chars().any(|ch| !ch.is_control()) {
            self.name.clear();
            self.replace_on_type = false;
        }
        for ch in t.chars() {
            if !ch.is_control() && self.name.chars().count() < self.max {
                self.name.push(ch);
            }
        }
    }

    pub fn key(&mut self, key: Keycode) -> Option<PromptResult> {
        match key {
            Keycode::Escape => return Some(PromptResult::Cancel),
            Keycode::Return | Keycode::KpEnter => return Some(PromptResult::Ok(self.name.trim().to_string())),
            Keycode::Backspace => {
                if self.replace_on_type {
                    self.name.clear();
                    self.replace_on_type = false;
                } else {
                    self.name.pop();
                }
            }
            _ => {}
        }
        None
    }

    fn card_rect() -> Rect {
        let (w, h) = (272u32, 88u32);
        Rect::new((grid::sw() as i32 - w as i32) / 2, grid::BAR_H + (grid::sh() - grid::BAR_H - h as i32) / 2, w, h)
    }

    fn field_rect(r: Rect) -> Rect {
        Rect::new(r.x() + 12, r.y() + 26, r.width() - 24, crate::launcher::widgets::FIELD_H as u32)
    }

    fn button_rects(r: Rect) -> [Rect; 2] {
        let y = r.y() + r.height() as i32 - 32;
        [Rect::new(r.x() + r.width() as i32 - 12 - 64 - 10 - 64, y, 64, 24), Rect::new(r.x() + r.width() as i32 - 12 - 64, y, 64, 24)]
    }

    pub fn click(&mut self, x: i32, y: i32) -> Option<PromptResult> {
        let [cancel, ok] = Self::button_rects(Self::card_rect());
        if contains(cancel, x, y) {
            Some(PromptResult::Cancel)
        } else if contains(ok, x, y) {
            Some(PromptResult::Ok(self.name.trim().to_string()))
        } else {
            None
        }
    }

    pub fn draw(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        dim_backdrop(c, tm);
        let r = Self::card_rect();
        frame(c, tm, r);
        fs.draw_display_text(c, r.x() + 12, r.y() + 7, &chrome::fit_display_text(fs, &i18n::tr_s(&self.title), r.width() as i32 - 24), ink);
        let nr = Self::field_rect(r);
        draw_field(c, fs, tm, nr, &Field { text: &self.name, caret: Some(self.name.chars().count()), select_all: self.replace_on_type, ..Default::default() });
        let [cancel, ok] = Self::button_rects(r);
        draw_button(c, fs, tm, cancel, "Cancel", BtnState::default());
        draw_button(c, fs, tm, ok, "OK", BtnState { default: true, ..Default::default() });
    }
}

// ------------------------------------------------------------------- confirm alert (two buttons)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmChoice {
    Cancel,
    Ok,
}

/// A two-button alert: a bold first line, explanation lines, [Cancel] [action].
pub struct ConfirmAlert {
    pub lines: Vec<String>,
    pub ok_label: String,
    /// Highlighted button: 0 = Cancel, 1 = the action.
    pub sel: usize,
    /// The system icon at the left (shown when its file exists): caution for a destructive action,
    /// note otherwise (see `new`); `with_icon` changes it.
    pub icon: AlertIcon,
}

impl ConfirmAlert {
    /// `default_ok` says which button Enter chooses (a destructive action defaults to Cancel).
    pub fn new(lines: Vec<String>, ok_label: &str, default_ok: bool) -> Self {
        let icon = if default_ok { AlertIcon::Note } else { AlertIcon::Caution };
        Self { lines, ok_label: ok_label.to_string(), sel: default_ok as usize, icon }
    }

    pub fn with_icon(mut self, icon: AlertIcon) -> Self {
        self.icon = icon;
        self
    }

    pub fn key(&mut self, key: Keycode) -> Option<ConfirmChoice> {
        match key {
            Keycode::Left | Keycode::Z => self.sel = 0,
            Keycode::Right | Keycode::C => self.sel = 1,
            Keycode::Return | Keycode::KpEnter => return Some(if self.sel == 1 { ConfirmChoice::Ok } else { ConfirmChoice::Cancel }),
            Keycode::Escape => return Some(ConfirmChoice::Cancel),
            _ => {}
        }
        None
    }

    fn card_rect(&self) -> Rect {
        alert_card(self.icon, 92)
    }

    fn button_rects(&self) -> [Rect; 2] {
        let r = self.card_rect();
        let y = r.y() + r.height() as i32 - 32;
        [Rect::new(r.x() + r.width() as i32 - 10 - 64 - 14 - 64, y, 64, 24), Rect::new(r.x() + r.width() as i32 - 10 - 64, y, 64, 24)]
    }

    pub fn click(&mut self, x: i32, y: i32) -> Option<ConfirmChoice> {
        let [cancel, ok] = self.button_rects();
        if contains(cancel, x, y) {
            self.sel = 0;
            Some(ConfirmChoice::Cancel)
        } else if contains(ok, x, y) {
            self.sel = 1;
            Some(ConfirmChoice::Ok)
        } else {
            None
        }
    }

    pub fn draw(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        dim_backdrop(c, tm);
        let r = self.card_rect();
        frame(c, tm, r);
        let tx = card_icon(c, tm, r, self.icon);
        let w = r.right() - 14 - tx;
        for (i, l) in self.lines.iter().enumerate() {
            if i == 0 {
                fs.draw_display_text(c, tx, r.y() + 8, &chrome::fit_display_text(fs, &i18n::tr_s(l), w), ink);
            } else {
                fs.draw_text(c, tx, r.y() + 24 + (i as i32 - 1) * 14, &chrome::fit_text(fs, &i18n::tr_s(l), w), crate::launcher::widgets::secondary_ink(tm));
            }
        }
        let [cancel, ok] = self.button_rects();
        for (i, (b, label)) in [(cancel, "Cancel"), (ok, self.ok_label.as_str())].into_iter().enumerate() {
            let st = BtnState { default: i == self.sel, pressed: false, hover: false, disabled: false };
            draw_button(c, fs, tm, b, label, st);
        }
    }
}

// ------------------------------------------------------------------------ list picker

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickResult {
    Cancel,
    Choose(usize),
}

const PICK_ROWS: usize = 4;

/// A short list to choose from (Up/Down, Enter, Esc; click a row, double click chooses).
pub struct ListPicker {
    pub title: String,
    pub rows: Vec<String>,
    pub sel: usize,
    pub scroll: usize,
    /// Label of the default button ("Move" unless changed).
    pub button: String,
    last_click: Option<(usize, std::time::Instant)>,
}

impl ListPicker {
    pub fn new(title: &str, rows: Vec<String>) -> Self {
        Self { title: title.to_string(), rows, sel: 0, scroll: 0, button: "Move".into(), last_click: None }
    }

    pub fn with_button(mut self, label: &str) -> Self {
        self.button = label.to_string();
        self
    }

    fn keep_visible(&mut self) {
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + PICK_ROWS {
            self.scroll = self.sel + 1 - PICK_ROWS;
        }
    }

    pub fn key(&mut self, key: Keycode) -> Option<PickResult> {
        match crate::platform::keys::arrow_alias(key) {
            Keycode::Escape => return Some(PickResult::Cancel),
            Keycode::Return | Keycode::KpEnter => {
                return if self.rows.is_empty() { Some(PickResult::Cancel) } else { Some(PickResult::Choose(self.sel)) };
            }
            Keycode::Up => self.sel = self.sel.saturating_sub(1),
            Keycode::Down => self.sel = (self.sel + 1).min(self.rows.len().saturating_sub(1)),
            _ => {}
        }
        self.keep_visible();
        None
    }

    pub fn wheel(&mut self, dy: i32) {
        if dy > 0 {
            self.scroll = self.scroll.saturating_sub(1);
        } else if self.scroll + PICK_ROWS < self.rows.len() {
            self.scroll += 1;
        }
    }

    fn card_rect() -> Rect {
        let (w, h) = (240u32, 128u32);
        Rect::new((grid::sw() as i32 - w as i32) / 2, grid::BAR_H + (grid::sh() - grid::BAR_H - h as i32) / 2, w, h)
    }

    fn list_rect(r: Rect) -> Rect {
        Rect::new(r.x() + 12, r.y() + 26, r.width() - 24, PICK_ROWS as u32 * 16 + 2)
    }

    fn button_rects(r: Rect) -> [Rect; 2] {
        let y = r.y() + r.height() as i32 - 30;
        [Rect::new(r.x() + r.width() as i32 - 12 - 64 - 10 - 64, y, 64, 22), Rect::new(r.x() + r.width() as i32 - 12 - 64, y, 64, 22)]
    }

    pub fn click(&mut self, x: i32, y: i32) -> Option<PickResult> {
        let r = Self::card_rect();
        let [cancel, ok] = Self::button_rects(r);
        if contains(cancel, x, y) {
            return Some(PickResult::Cancel);
        }
        if contains(ok, x, y) {
            return self.key(Keycode::Return);
        }
        let l = Self::list_rect(r);
        if contains(l, x, y) {
            let row = ((y - l.y() - 1) / 16).max(0) as usize + self.scroll;
            if row < self.rows.len() {
                let double = self.last_click.map_or(false, |(p, t)| p == row && t.elapsed() < std::time::Duration::from_millis(450));
                self.sel = row;
                self.last_click = Some((row, std::time::Instant::now()));
                if double {
                    return Some(PickResult::Choose(row));
                }
            }
        }
        None
    }

    pub fn draw(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        dim_backdrop(c, tm);
        let r = Self::card_rect();
        frame(c, tm, r);
        fs.draw_display_text(c, r.x() + 12, r.y() + 7, &chrome::fit_display_text(fs, &i18n::tr_s(&self.title), r.width() as i32 - 24), ink);
        let l = Self::list_rect(r);
        ui::fill_rect(c, l, tm.get_color("surface_sunken"));
        ui::draw_rect(c, l, ink);
        for i in 0..PICK_ROWS {
            let idx = self.scroll + i;
            let Some(name) = self.rows.get(idx) else { break };
            let row = Rect::new(l.x() + 1, l.y() + 1 + i as i32 * 16, l.width() - 2, 16);
            let hi = idx == self.sel;
            if hi {
                ui::fill_rect(c, row, ink);
            }
            let fg = if hi { tm.get_color("widget_face_active") } else { ink };
            fs.draw_display_text(c, row.x() + 6, row.y(), &chrome::fit_display_text(fs, name, row.width() as i32 - 14), fg);
        }
        if self.rows.len() > PICK_ROWS {
            let track = Rect::new(l.x() + l.width() as i32 - 4, l.y() + 1, 3, l.height() - 2);
            let th = ((track.height() as usize * PICK_ROWS) / self.rows.len()).max(6) as u32;
            let span = track.height() - th;
            let ty = track.y() + (span as usize * self.scroll / (self.rows.len() - PICK_ROWS).max(1)) as i32;
            ui::fill_rect(c, Rect::new(track.x(), ty, 3, th), tm.get_color("line"));
            ui::fill_rect(c, Rect::new(track.x(), ty, 3, th), ink);
        }
        let [cancel, ok] = Self::button_rects(r);
        draw_button(c, fs, tm, cancel, "Cancel", BtnState::default());
        draw_button(c, fs, tm, ok, &self.button, BtnState { default: true, ..Default::default() });
    }
}

// ------------------------------------------------------------------- one dialog, any kind

/// The dialogs an app can have open, behind one interface, so an app keeps a single
/// `Option<Dialog<Tag>>` instead of its own enum. `tag` says what the answer is for
/// (rename this path, delete that one...).
pub enum DialogKind {
    Name(NamePrompt),
    Confirm(ConfirmAlert),
    Panel(SavePanel),
    Card(UnsavedCard),
    Pick(ListPicker),
}

/// What the user decided. Every cancel (Esc, the Cancel button) is `Cancel`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogOut {
    Cancel,
    /// A name prompt's OK (trimmed text).
    Name(String),
    /// A confirm alert's action button.
    Ok,
    /// A file panel's chosen path (Save as: where to write; Open: which file).
    Path(PathBuf),
    /// "Save changes?": Save or Don't Save.
    Card(CardChoice),
    /// A list picker's row.
    Pick(usize),
}

pub struct Dialog<T> {
    pub kind: DialogKind,
    pub tag: T,
}

impl<T> Dialog<T> {
    pub fn new(kind: DialogKind, tag: T) -> Self {
        Self { kind, tag }
    }

    /// A rename / new-folder prompt (names up to 64 characters, so real file names fit).
    pub fn name(title: &str, default: &str, tag: T) -> Self {
        let mut p = NamePrompt::new(title, default);
        p.max = 64;
        Self::new(DialogKind::Name(p), tag)
    }

    /// A confirm alert whose Enter key chooses Cancel (use for anything destructive).
    pub fn confirm(lines: Vec<String>, ok_label: &str, tag: T) -> Self {
        Self::new(DialogKind::Confirm(ConfirmAlert::new(lines, ok_label, false)), tag)
    }

    pub fn key(&mut self, key: Keycode) -> Option<DialogOut> {
        match &mut self.kind {
            DialogKind::Name(p) => p.key(key).map(|r| match r {
                PromptResult::Cancel => DialogOut::Cancel,
                PromptResult::Ok(n) => DialogOut::Name(n),
            }),
            DialogKind::Confirm(a) => a.key(key).map(|r| if r == ConfirmChoice::Ok { DialogOut::Ok } else { DialogOut::Cancel }),
            DialogKind::Panel(p) => p.key(key).map(|r| match r {
                SaveResult::Cancel => DialogOut::Cancel,
                SaveResult::Save(path) => DialogOut::Path(path),
            }),
            DialogKind::Card(c) => c.key(key).map(Self::card_out),
            DialogKind::Pick(p) => p.key(key).map(Self::pick_out),
        }
    }

    pub fn click(&mut self, x: i32, y: i32) -> Option<DialogOut> {
        match &mut self.kind {
            DialogKind::Name(p) => p.click(x, y).map(|r| match r {
                PromptResult::Cancel => DialogOut::Cancel,
                PromptResult::Ok(n) => DialogOut::Name(n),
            }),
            DialogKind::Confirm(a) => a.click(x, y).map(|r| if r == ConfirmChoice::Ok { DialogOut::Ok } else { DialogOut::Cancel }),
            DialogKind::Panel(p) => p.click(x, y).map(|r| match r {
                SaveResult::Cancel => DialogOut::Cancel,
                SaveResult::Save(path) => DialogOut::Path(path),
            }),
            DialogKind::Card(c) => c.click(x, y).map(Self::card_out),
            DialogKind::Pick(p) => p.click(x, y).map(Self::pick_out),
        }
    }

    fn card_out(c: CardChoice) -> DialogOut {
        if c == CardChoice::Cancel { DialogOut::Cancel } else { DialogOut::Card(c) }
    }

    fn pick_out(r: PickResult) -> DialogOut {
        match r {
            PickResult::Cancel => DialogOut::Cancel,
            PickResult::Choose(i) => DialogOut::Pick(i),
        }
    }

    pub fn text(&mut self, t: &str) {
        match &mut self.kind {
            DialogKind::Name(p) => p.text(t),
            DialogKind::Panel(p) => p.text(t),
            _ => {}
        }
    }

    pub fn wheel(&mut self, dy: i32) {
        match &mut self.kind {
            DialogKind::Panel(p) => p.wheel(dy),
            DialogKind::Pick(p) => p.wheel(dy),
            _ => {}
        }
    }

    /// Show a line in a file panel (for example "Cannot save: ...").
    pub fn say(&mut self, msg: &str) {
        if let DialogKind::Panel(p) = &mut self.kind {
            p.message = Some(msg.to_string());
        }
    }

    pub fn draw(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager) {
        match &self.kind {
            DialogKind::Name(p) => p.draw(c, fs, tm),
            DialogKind::Confirm(a) => a.draw(c, fs, tm),
            DialogKind::Panel(p) => p.draw(c, fs, tm),
            DialogKind::Card(k) => k.draw(c, fs, tm),
            DialogKind::Pick(p) => p.draw(c, fs, tm),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_save_panel_has_16_px_rows_and_fits_on_the_screen() {
        let p = SavePanel::panel_rect();
        assert!(p.y() >= grid::BAR_H, "clear of the status bar");
        assert!(p.bottom() + 1 <= grid::sh(), "panel and its 1 px shadow fit");
        let l = SavePanel::list_rect(p);
        assert_eq!(l.height() as i32, ROWS as i32 * 16 + 2);
        let [cancel, _] = SavePanel::button_rects(p);
        assert!(l.bottom() < cancel.y(), "the list ends above the buttons");
        assert!(SavePanel::name_rect(p).bottom() < l.y());
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("harbour-dialogs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Notes")).unwrap();
        std::fs::write(d.join("a.txt"), "a").unwrap();
        std::fs::write(d.join("Notes").join("b.txt"), "b").unwrap();
        d
    }

    #[test]
    fn the_alert_defaults_to_save_and_has_shortcuts() {
        let mut c = UnsavedCard::new("note.txt");
        assert_eq!(c.key(Keycode::Return), Some(CardChoice::Save), "Save is the default button");
        assert_eq!(c.key(Keycode::Left), None);
        assert_eq!(c.key(Keycode::Return), Some(CardChoice::Cancel));
        assert_eq!(c.key(Keycode::Left), None);
        assert_eq!(c.key(Keycode::Return), Some(CardChoice::DontSave));
        assert_eq!(c.key(Keycode::Escape), Some(CardChoice::Cancel));
        assert_eq!(c.key(Keycode::D), Some(CardChoice::DontSave));
        assert_eq!(c.key(Keycode::S), Some(CardChoice::Save));
    }

    #[test]
    fn the_alert_buttons_can_be_clicked() {
        let mut c = UnsavedCard::new("x");
        let b = UnsavedCard::button_rects();
        assert_eq!(c.click(b[0].x() + 3, b[0].y() + 3), Some(CardChoice::DontSave));
        assert_eq!(c.click(b[2].x() + 3, b[2].y() + 3), Some(CardChoice::Save));
        assert_eq!(c.click(0, 0), None);
    }

    #[test]
    fn the_panel_lists_folders_first_and_walks_into_and_out_of_them() {
        let root = temp("walk");
        let mut p = SavePanel::new(&root, "note.txt", &root);
        let names: Vec<_> = p.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Notes", "a.txt"], "no '..' at the top; folders before files");
        p.key(Keycode::Right); // enter Notes (selected)
        assert_eq!(p.dir, root.join("Notes"));
        assert_eq!(p.entries[0].name, "..");
        assert!(p.entries.iter().any(|e| e.name == "b.txt"));
        p.key(Keycode::Left);
        assert_eq!(p.dir, root);
        p.key(Keycode::Left);
        assert_eq!(p.dir, root, "cannot go above the root");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typing_edits_the_name_and_enter_saves_to_the_chosen_folder() {
        let root = temp("save");
        let mut p = SavePanel::new(&root, "", &root);
        assert!(p.key(Keycode::Return).is_none(), "an empty name is refused");
        assert!(p.message.is_some());
        p.text("my");
        p.text("/note.txt"); // a slash is not allowed in a name
        assert_eq!(p.name, "mynote.txt");
        p.key(Keycode::Backspace);
        assert_eq!(p.name, "mynote.tx");
        p.key(Keycode::Right); // into Notes
        assert_eq!(p.key(Keycode::Return), Some(SaveResult::Save(root.join("Notes").join("mynote.tx"))));
        assert_eq!(p.key(Keycode::Escape), Some(SaveResult::Cancel));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_existing_file_asks_before_it_is_replaced() {
        let root = temp("replace");
        let mut p = SavePanel::new(&root, "a.txt", &root);
        assert!(p.key(Keycode::Return).is_none(), "first Enter only warns");
        assert!(p.confirm_replace);
        assert_eq!(p.key(Keycode::Return), Some(SaveResult::Save(root.join("a.txt"))));
        // Typing again cancels the warning.
        let mut p = SavePanel::new(&root, "a.txt", &root);
        p.key(Keycode::Return);
        p.text("x");
        assert!(!p.confirm_replace);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_suggested_name_is_replaced_by_the_first_typing() {
        let root = temp("select");
        let mut p = SavePanel::new(&root, "note.txt", &root);
        assert!(p.replace_on_type);
        p.text("todo.txt");
        assert_eq!(p.name, "todo.txt", "typing replaces the selected suggestion");
        p.text("x");
        assert_eq!(p.name, "todo.txtx", "later typing appends");
        let mut p = SavePanel::new(&root, "note.txt", &root);
        p.key(Keycode::Backspace);
        assert_eq!(p.name, "", "Backspace clears a selected name");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn choosing_a_file_copies_its_name() {
        let root = temp("pick");
        let mut p = SavePanel::new(&root, "", &root);
        p.key(Keycode::Down); // a.txt
        assert_eq!(p.name, "a.txt");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_name_prompt_replaces_the_suggestion_and_returns_the_name() {
        let mut p = NamePrompt::new("New folder", "untitled folder");
        p.text("Games");
        p.text("!");
        assert_eq!(p.name, "Games!");
        p.key(Keycode::Backspace);
        assert_eq!(p.key(Keycode::Return), Some(PromptResult::Ok("Games".into())));
        assert_eq!(p.key(Keycode::Escape), Some(PromptResult::Cancel));
        let mut p = NamePrompt::new("New folder", "untitled folder");
        assert_eq!(p.key(Keycode::Return), Some(PromptResult::Ok("untitled folder".into())), "Enter keeps the suggestion");
        let mut p = NamePrompt::new("x", "abc");
        p.key(Keycode::Backspace);
        assert_eq!(p.name, "", "Backspace clears a selected suggestion");
        p.text(&"y".repeat(50));
        assert_eq!(p.name.chars().count(), 24);
        let b = NamePrompt::button_rects(NamePrompt::card_rect());
        assert_eq!(p.click(b[0].x() + 3, b[0].y() + 3), Some(PromptResult::Cancel));
        assert!(matches!(p.click(b[1].x() + 3, b[1].y() + 3), Some(PromptResult::Ok(_))));
        assert_eq!(p.click(0, 0), None);
    }

    #[test]
    fn a_destructive_confirm_wears_the_caution_icon_and_others_the_note() {
        assert_eq!(ConfirmAlert::new(vec!["Delete?".into()], "Delete", false).icon, AlertIcon::Caution);
        assert_eq!(ConfirmAlert::new(vec!["Organize?".into()], "Organize", true).icon, AlertIcon::Note);
        assert_eq!(ConfirmAlert::new(vec![], "x", true).with_icon(AlertIcon::Stop).icon, AlertIcon::Stop);
        // Wide with an icon, as before without: either way it fits on the screen with room to spare.
        let r = alert_card(AlertIcon::Caution, 92);
        assert!(r.width() == 272 || r.width() == 304);
        assert!(r.x() >= 8 && r.right() <= grid::sw() - 8);
    }

    #[test]
    fn the_confirm_alert_defaults_to_the_safe_button() {
        let mut a = ConfirmAlert::new(vec!["Delete?".into()], "Delete", false);
        assert_eq!(a.key(Keycode::Return), Some(ConfirmChoice::Cancel));
        assert_eq!(a.key(Keycode::Right), None);
        assert_eq!(a.key(Keycode::Return), Some(ConfirmChoice::Ok));
        assert_eq!(a.key(Keycode::Escape), Some(ConfirmChoice::Cancel));
        let b = a.button_rects();
        assert_eq!(a.click(b[1].x() + 3, b[1].y() + 3), Some(ConfirmChoice::Ok));
        assert_eq!(a.click(b[0].x() + 3, b[0].y() + 3), Some(ConfirmChoice::Cancel));
    }

    #[test]
    fn the_list_picker_moves_scrolls_and_chooses() {
        let mut p = ListPicker::new("Move", (0..7).map(|i| format!("F{i}")).collect());
        for _ in 0..5 {
            p.key(Keycode::Down);
        }
        assert_eq!((p.sel, p.scroll), (5, 2));
        assert_eq!(p.key(Keycode::Return), Some(PickResult::Choose(5)));
        p.key(Keycode::Up);
        assert_eq!(p.sel, 4);
        assert_eq!(p.key(Keycode::Escape), Some(PickResult::Cancel));
        let l = ListPicker::list_rect(ListPicker::card_rect());
        p.scroll = 0;
        assert_eq!(p.click(l.x() + 5, l.y() + 1 + 16 * 2 + 3), None);
        assert_eq!(p.sel, 2);
        assert_eq!(p.click(l.x() + 5, l.y() + 1 + 16 * 2 + 3), Some(PickResult::Choose(2)), "double click chooses");
        assert_eq!(ListPicker::new("x", vec![]).key(Keycode::Return), Some(PickResult::Cancel));
    }

    #[test]
    fn the_open_panel_filters_picks_and_enters_folders() {
        let root = temp("open");
        std::fs::write(root.join("x.csv"), "1").unwrap();
        let mut p = SavePanel::open(&root, &root, &["csv"]);
        let names: Vec<_> = p.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Notes", "x.csv"], "a.txt is filtered out, folders stay");
        assert_eq!(p.button, "Open");
        p.text("zzz");
        assert_eq!(p.name, "", "typing does nothing in an open panel");
        assert!(p.key(Keycode::Return).is_none(), "Enter on a folder goes into it");
        assert_eq!(p.dir, root.join("Notes"));
        p.key(Keycode::Backspace);
        assert_eq!(p.dir, root, "Backspace goes up");
        p.key(Keycode::Down);
        assert_eq!(p.name, "x.csv", "the name field shows the choice");
        assert_eq!(p.key(Keycode::Return), Some(SaveResult::Save(root.join("x.csv"))), "no replace warning when opening");
        let empty = temp("openempty");
        std::fs::remove_file(empty.join("a.txt")).unwrap();
        std::fs::remove_dir_all(empty.join("Notes")).unwrap();
        let mut p = SavePanel::open(&empty, &empty, &[]);
        assert!(p.key(Keycode::Return).is_none() && p.message.is_some());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn dialog_wraps_every_kind_with_one_answer_type() {
        let mut d = Dialog::name("Rename:", "a.txt", 7usize);
        d.text("b.txt");
        assert_eq!(d.key(Keycode::Return), Some(DialogOut::Name("b.txt".into())));
        assert_eq!(d.tag, 7);
        let mut d = Dialog::confirm(vec!["Delete?".into()], "Delete", ());
        assert_eq!(d.key(Keycode::Return), Some(DialogOut::Cancel), "destructive: Enter cancels");
        d.key(Keycode::Right);
        assert_eq!(d.key(Keycode::Return), Some(DialogOut::Ok));
        let mut d = Dialog::new(DialogKind::Card(UnsavedCard::new("a")), ());
        assert_eq!(d.key(Keycode::Return), Some(DialogOut::Card(CardChoice::Save)));
        assert_eq!(d.key(Keycode::Escape), Some(DialogOut::Cancel));
        let mut d = Dialog::new(DialogKind::Pick(ListPicker::new("Open with:", vec!["A".into(), "B".into()]).with_button("Open")), ());
        d.key(Keycode::Down);
        assert_eq!(d.key(Keycode::Return), Some(DialogOut::Pick(1)));
        let root = temp("dlgpanel");
        let mut d = Dialog::new(DialogKind::Panel(SavePanel::new(&root, "n.txt", &root).titled("New file:", "Create")), ());
        assert_eq!(d.key(Keycode::Return), Some(DialogOut::Path(root.join("n.txt"))));
        d.say("hi");
        let _ = std::fs::remove_dir_all(&root);
    }
}
