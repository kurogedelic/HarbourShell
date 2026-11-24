//! Launcher home screen in the 1-bit style, with two views
//! (Tab switches): an icon grid (4 columns x 3 rows of 24 px icons with labels
//! on plates) and a compact list (2 columns x 9 rows of names). Both scroll by
//! row, show a scroll bar, and have no footer.

use sdl2::keyboard::Keycode;
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use std::path::PathBuf;

use crate::assets::{AssetStore, IconPalette};
use crate::chrome;
use crate::font::FontSystem;
use crate::grid;
use crate::launcher::folders::{Dest, Folders};
use crate::launcher::widgets::{SbPart, ScrollBar};
use crate::theme::ThemeManager;
use crate::ui;

const CELL_W: i32 = 76;
/// Row pitch of the grid: 3 rows of 52 px from y 18, so the last row's name (descenders included)
/// ends on y 167 and the grid uses the screen down to its bottom (rows 168 and 169 stay clear).
const CELL_H: i32 = 52;
/// A tile (icon + name) is 44 px high; the 8 px below it are the gap to the next row.
const TILE_H: i32 = 44;
const GRID_X: i32 = 4;
/// 2 px below the menu strip's rule (y 15): the selection square of the first row starts here.
const GRID_Y: i32 = 18;
const ICON: u32 = 32;

/// List view geometry.
const LIST_X: i32 = 5;
/// 10 rows of 15 px from y 18: plates y 18..166, 2 px clear of the menu strip's rule and of the bottom.
const LIST_Y: i32 = 18;
const LIST_COL_W: i32 = 155;
const LIST_ROW_H: i32 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Grid,
    List,
}

/// The name a tile or row wears: the system entries (Back, Recent) follow the display language;
/// apps and folders keep their own names.
fn shown_name(item: &Item) -> String {
    match item.kind {
        Kind::Back => crate::launcher::i18n::tr("Back").to_string(),
        Kind::Recent => crate::launcher::i18n::tr("Recent").to_string(),
        Kind::App => match &item.target {
            Target::Builtin(b) => crate::launcher::packages::find(b.id()).map(|p| p.shown_name()).unwrap_or_else(|| item.name.clone()),
            Target::External(_) => item.name.clone(),
        },
        _ => item.name.clone(),
    }
}

impl View {
    pub fn cols(self) -> usize {
        match self {
            View::Grid => 4,
            View::List => 2,
        }
    }
    pub fn rows(self) -> usize {
        match self {
            View::Grid => 3,
            View::List => 10,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Settings,
    Terminal,
    TextEditor,
    Calculator,
    Clock,
    Monitor,
    Snake,
    /// An app from `launcher::apps::REGISTRY` (index into the table).
    App(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Builtin(Builtin),
    /// `.desktop` id of an external app.
    External(String),
}

/// What a displayed entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    App,
    /// A folder tile (index into `Home::folders`).
    Folder(usize),
    /// The `..` tile at the start of an open folder.
    Back,
    /// The virtual "Recent" folder (not stored in folders.json).
    Recent,
}

/// `Home::open` value of the virtual Recent folder.
pub const RECENT: usize = usize::MAX;
pub const RECENT_MAX: usize = 8;

#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub target: Target,
    /// Asset key of the normal icon.
    pub icon: String,
    /// Asset key of a filled "selected" variant, when one exists.
    pub icon_selected: Option<String>,
    pub kind: Kind,
    /// Folder tiles: icon keys of up to four member apps (drawn 2x2 inside the folder).
    pub minis: Vec<String>,
}

impl Item {
    pub fn app(name: String, target: Target, icon: String, icon_selected: Option<String>) -> Self {
        Self { name, target, icon, icon_selected, kind: Kind::App, minis: Vec::new() }
    }

    /// Stable id used by folders and Recent: the `.desktop` id of an external app, the package id
    /// (`apps/<id>/`) of a built-in. Never the shown name, so switching the language keeps both.
    pub fn key(&self) -> String {
        match &self.target {
            Target::External(id) => id.clone(),
            Target::Builtin(b) => b.id().to_string(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum HomeAction {
    None,
    Open(usize),
    /// A folder opened or closed: the host plays the zoom rectangle.
    Zoom { from: Rect, to: Rect },
}

/// A tile being dragged with the mouse.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub idx: usize,
    pub start: (i32, i32),
    pub pos: (i32, i32),
    /// Set once the pointer has moved more than [`DRAG_SLOP`].
    pub active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragEnd {
    /// It was a plain click.
    NotDragging,
    /// Dropped outside any target: nothing changed.
    Cancelled,
    Moved,
}

pub const DRAG_SLOP: i32 = 6;

pub struct Home {
    /// What is shown now: the top level (folders, then loose apps) or one folder's apps.
    pub items: Vec<Item>,
    /// Every visible app, in the usual order (folders not applied).
    pub all: Vec<Item>,
    pub folders: Folders,
    /// The folder shown as a sub-page.
    pub open: Option<usize>,
    pub selected: usize,
    pub scroll_row: usize,
    sb_drag: bool,
    pub view: View,
    pub drag: Option<Drag>,
    folders_path: Option<PathBuf>,
    /// Keys of the last launched items, newest first (`recent.txt`).
    pub recent: Vec<String>,
    recent_path: Option<PathBuf>,
    /// On a desk (a bigger screen) Home is the desk itself: its icons stand in columns from the
    /// right edge, top to bottom, on the desk's own background; there is no list view and no
    /// scrolling.
    pub desk: bool,
}

impl Home {
    pub fn new(items: Vec<Item>) -> Self {
        let mut h = Self {
            items: Vec::new(),
            all: items,
            folders: Folders::default(),
            open: None,
            selected: 0,
            scroll_row: 0,
            sb_drag: false,
            view: View::Grid,
            drag: None,
            folders_path: None,
            recent: Vec::new(),
            recent_path: None,
            desk: false,
        };
        h.relayout(None);
        h
    }

    /// Load folders from `path` and save every change there.
    pub fn set_folders_path(&mut self, path: PathBuf) {
        self.folders = Folders::load(&path);
        // Older versions saved built-ins by their display name ("Text Editor"): use the ids.
        if self.folders.migrate_keys(crate::launcher::packages::migrate_key) {
            if let Err(e) = self.folders.save(&path) {
                eprintln!("[folders] cannot save {}: {e}", path.display());
            }
        }
        self.folders_path = Some(path);
        let keep = self.sel_id();
        self.relayout(keep);
    }

    /// Load the launch history from `path` (one key per line) and save it there.
    pub fn set_recent_path(&mut self, path: PathBuf) {
        let mut recent: Vec<String> = Vec::new();
        let mut migrated = false;
        for l in std::fs::read_to_string(&path).unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty()) {
            // Old display-name keys become ids (see `packages::migrate_key`).
            let new = crate::launcher::packages::migrate_key(l);
            migrated |= new.is_some();
            let k = new.unwrap_or(l).to_string();
            if !recent.contains(&k) && recent.len() < RECENT_MAX {
                recent.push(k);
            }
        }
        self.recent = recent;
        self.recent_path = Some(path);
        if migrated {
            self.save_recent();
        }
        let keep = self.sel_id();
        self.relayout(keep);
    }

    /// Remember that `key` was launched (newest first, at most 8, no duplicates).
    pub fn record_launch(&mut self, key: &str) {
        self.recent.retain(|k| k != key);
        self.recent.insert(0, key.to_string());
        self.recent.truncate(RECENT_MAX);
        self.save_recent();
        let keep = self.sel_id();
        // Do not reshuffle the page the user is looking at: only the Recent tile changes.
        self.relayout(keep);
    }

    fn save_recent(&self) {
        if let Some(p) = &self.recent_path {
            let write = || -> std::io::Result<()> {
                if let Some(d) = p.parent() {
                    std::fs::create_dir_all(d)?;
                }
                let tmp = p.with_extension("txt.tmp");
                std::fs::write(&tmp, self.recent.join("\n") + "\n")?;
                std::fs::rename(&tmp, p)
            };
            if let Err(e) = write() {
                eprintln!("[recent] cannot save {}: {e}", p.display());
            }
        }
    }

    /// Shortcuts shown by the Recent folder (apps that are installed and visible).
    fn recent_items(&self) -> Vec<&Item> {
        self.recent.iter().filter_map(|k| self.all.iter().find(|i| i.key() == *k)).collect()
    }

    fn save_folders(&self) {
        if let Some(p) = &self.folders_path {
            if let Err(e) = self.folders.save(p) {
                eprintln!("[folders] cannot save {}: {e}", p.display());
            }
        }
    }

    /// The menu strip's context title: the open folder's name.
    pub fn context(&self) -> &str {
        match self.open {
            Some(RECENT) => "Recent",
            Some(i) => self.folders.list.get(i).map(|f| f.name.as_str()).unwrap_or(""),
            None => "",
        }
    }

    fn sel_id(&self) -> Option<String> {
        self.items.get(self.selected).map(item_id)
    }

    /// Rebuild `items` from `all` + folders + the open folder; keep the selection on `keep`.
    fn relayout(&mut self, keep: Option<String>) {
        let recent_empty = self.recent_items().is_empty();
        if self.open.map_or(false, |i| if i == RECENT { recent_empty } else { i >= self.folders.list.len() }) {
            self.open = None;
        }
        let mut items: Vec<Item> = Vec::new();
        match self.open {
            Some(fi) => {
                items.push(Item {
                    name: "..".into(),
                    target: Target::External(String::new()),
                    icon: String::new(),
                    icon_selected: None,
                    kind: Kind::Back,
                    minis: Vec::new(),
                });
                if fi == RECENT {
                    items.extend(self.recent_items().into_iter().cloned());
                } else {
                    for k in &self.folders.list[fi].items {
                        if let Some(it) = self.all.iter().find(|i| i.key() == *k) {
                            items.push(it.clone());
                        }
                    }
                }
            }
            None => {
                let recent = self.recent_items();
                // The desk shows no Recent folder: its windows are what is in use.
                if !recent.is_empty() && !self.desk {
                    items.push(Item {
                        name: "Recent".into(),
                        target: Target::External(String::new()),
                        icon: String::new(),
                        icon_selected: None,
                        kind: Kind::Recent,
                        minis: recent.iter().take(4).map(|i| i.icon.clone()).collect(),
                    });
                }
                for (fi, f) in self.folders.list.iter().enumerate() {
                    let minis = f
                        .items
                        .iter()
                        .filter_map(|k| self.all.iter().find(|i| i.key() == *k))
                        .take(4)
                        .map(|i| i.icon.clone())
                        .collect();
                    items.push(Item {
                        name: f.name.clone(),
                        target: Target::External(String::new()),
                        icon: String::new(),
                        icon_selected: None,
                        kind: Kind::Folder(fi),
                        minis,
                    });
                }
                items.extend(self.all.iter().filter(|i| !self.folders.contains(&i.key())).cloned());
            }
        }
        self.items = items;
        self.selected = keep
            .and_then(|id| self.items.iter().position(|i| item_id(i) == id))
            .unwrap_or(0)
            .min(self.items.len().saturating_sub(1));
        self.drag = None;
        self.scroll_into_view();
    }

    /// Like `relayout`, but when `keep` is gone the selection stays near `fallback`.
    fn relayout_near(&mut self, keep: Option<String>, fallback: usize) {
        self.relayout(keep.clone());
        let gone = keep.map_or(true, |id| self.sel_id().as_deref() != Some(id.as_str()));
        if gone {
            self.selected = fallback.min(self.items.len().saturating_sub(1));
            self.scroll_into_view();
        }
    }

    pub fn in_folder(&self) -> bool {
        self.open.is_some()
    }

    pub fn selected_kind(&self) -> Option<Kind> {
        self.items.get(self.selected).map(|i| i.kind)
    }

    /// Folder the menu acts on: the selected folder tile, else the open folder.
    pub fn target_folder(&self) -> Option<usize> {
        match self.selected_kind() {
            Some(Kind::Folder(i)) => Some(i),
            _ => self.open.filter(|i| *i != RECENT),
        }
    }

    fn content_rect() -> Rect {
        Rect::new(GRID_X, GRID_Y, (4 * CELL_W - 4) as u32, (2 * CELL_H + TILE_H) as u32)
    }

    pub fn open_folder(&mut self, idx: usize) -> HomeAction {
        if idx != RECENT && idx >= self.folders.list.len() {
            return HomeAction::None;
        }
        let from = self.selected_icon_rect();
        self.open = Some(idx);
        self.scroll_row = 0;
        self.relayout(None);
        if self.items.len() > 1 {
            self.selected = 1;
        }
        self.scroll_into_view();
        HomeAction::Zoom { from, to: Self::content_rect() }
    }

    pub fn close_folder(&mut self) -> HomeAction {
        let Some(fi) = self.open.take() else { return HomeAction::None };
        let id = if fi == RECENT { Some("\0recent".to_string()) } else { self.folders.list.get(fi).map(|f| folder_id(&f.name)) };
        self.scroll_row = 0;
        self.relayout(id);
        HomeAction::Zoom { from: Self::content_rect(), to: self.selected_icon_rect() }
    }

    // ---- managing folders (each change is saved) ----

    fn commit(&mut self, keep: Option<String>, fallback: usize) {
        self.save_folders();
        self.relayout_near(keep, fallback);
    }

    /// Create a folder; on the top level the new tile is selected. Returns its index.
    pub fn create_folder(&mut self, name: &str) -> usize {
        let idx = self.folders.new_folder(name);
        let id = folder_id(&self.folders.list[idx].name);
        self.save_folders();
        let keep = if self.open.is_some() { self.sel_id() } else { Some(id) };
        self.relayout(keep);
        idx
    }

    pub fn rename_folder(&mut self, idx: usize, name: &str) {
        if idx >= self.folders.list.len() {
            return;
        }
        let was_open = self.open == Some(idx);
        self.folders.rename(idx, name);
        let keep = if was_open || self.selected_kind() == Some(Kind::Folder(idx)) {
            Some(folder_id(&self.folders.list[idx].name))
        } else {
            self.sel_id()
        };
        self.save_folders();
        self.relayout(keep);
    }

    /// Delete a folder; its apps return to the top level.
    pub fn delete_folder(&mut self, idx: usize) {
        if idx >= self.folders.list.len() {
            return;
        }
        let was_open = self.open == Some(idx);
        self.folders.delete(idx);
        if was_open {
            self.open = None;
        } else if let Some(o) = self.open.filter(|o| *o != RECENT) {
            if o > idx {
                self.open = Some(o - 1);
            }
        }
        let sel = self.selected;
        self.save_folders();
        self.relayout_near(None, sel);
    }

    /// Move one app (by key) into a folder or to the top level.
    pub fn move_key(&mut self, key: &str, dest: Dest) {
        let sel = self.selected;
        self.folders.move_item(key, dest);
        self.commit(None, sel);
    }

    /// Where the selected app can go: folder names (not the one it is in) and "Top level".
    pub fn move_options(&self) -> Vec<(String, Dest)> {
        let Some(item) = self.items.get(self.selected).filter(|i| i.kind == Kind::App) else { return Vec::new() };
        let here = self.folders.folder_of(&item.key());
        let mut v: Vec<(String, Dest)> = Vec::new();
        for (i, f) in self.folders.list.iter().enumerate() {
            if Some(i) != here {
                v.push((f.name.clone(), Dest::Folder(i)));
            }
        }
        if here.is_some() {
            v.push(("Top level".into(), Dest::Top));
        }
        v
    }

    pub fn move_selected(&mut self, dest: Dest) {
        if let Some(item) = self.items.get(self.selected).filter(|i| i.kind == Kind::App) {
            let key = item.key();
            self.move_key(&key, dest);
        }
    }

    /// The default grouping (Games / Music / Tools). Returns how many apps moved.
    pub fn auto_organize(&mut self) -> usize {
        let apps: Vec<(String, String)> = self.all.iter().map(|i| (i.key(), i.name.clone())).collect();
        let n = self.folders.auto_organize(&apps);
        if n > 0 {
            self.open = None;
            self.save_folders();
            self.relayout(None);
        }
        n
    }

    /// Forget folder members that are no longer installed (`known` = every existing key,
    /// hidden apps included). Saves when something changed.
    pub fn prune_missing(&mut self, known: &[String]) {
        if self.folders.prune(known) {
            self.save_folders();
            let keep = self.sel_id();
            self.relayout(keep);
        }
    }

    /// Menu entries for the home screen, depending on the selection.
    pub fn menu_entries(&self) -> Vec<(&'static str, &'static str)> {
        let mut v = vec![("hf_new", "New Folder...")];
        if !self.move_options().is_empty() {
            v.push(("hf_move", "Move to Folder..."));
        }
        if self.target_folder().is_some() {
            v.push(("hf_rename", "Rename Folder..."));
            v.push(("hf_delete", "Delete Folder"));
        }
        v.push(("hf_auto", "Auto-organize..."));
        v
    }

    /// Context menu (right click): on a tile (already selected) or on empty space.
    pub fn context_entries(&self, on_tile: bool) -> Vec<(&'static str, &'static str)> {
        if !on_tile {
            return vec![("hf_new", "New Folder..."), ("hf_auto", "Auto-organize...")];
        }
        let mut v = vec![("hf_open", "Open")];
        let in_folder = self
            .items
            .get(self.selected)
            .filter(|i| i.kind == Kind::App)
            .map_or(false, |i| self.folders.folder_of(&i.key()).is_some());
        if in_folder {
            v.push(("hf_out", "Remove from Folder"));
        }
        // Any app but Settings can leave Home (Settings > Apps brings it back).
        let hideable = self.items.get(self.selected).map_or(false, |i| i.kind == Kind::App && i.target != Target::Builtin(Builtin::Settings));
        if hideable {
            v.push(("hf_hide", "Hide from Home"));
        }
        match self.selected_kind() {
            Some(Kind::App) | Some(Kind::Folder(_)) => v.extend(self.menu_entries()),
            _ => {}
        }
        v
    }

    // ---- mouse dragging ----

    /// Left button went down on entry `idx` (only apps can be dragged).
    pub fn press(&mut self, idx: usize, x: i32, y: i32) {
        self.drag = self
            .items
            .get(idx)
            .filter(|i| i.kind == Kind::App)
            .map(|_| Drag { idx, start: (x, y), pos: (x, y), active: false });
    }

    /// The pointer moved with the button held. True while a drag is in progress.
    pub fn drag_move(&mut self, x: i32, y: i32) -> bool {
        if self.sb_drag {
            if let Some(track) = self.sb_track() {
                self.scroll_row = self.scrollbar(track).offset_for_y(y);
                self.keep_selection_visible();
            }
            return false;
        }
        let Some(d) = self.drag.as_mut() else { return false };
        d.pos = (x, y);
        if !d.active && ((x - d.start.0).abs() > DRAG_SLOP || (y - d.start.1).abs() > DRAG_SLOP) {
            d.active = true;
        }
        d.active
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.map_or(false, |d| d.active)
    }

    /// What dropping at (x, y) would do: onto a folder tile, or (inside a folder) onto the
    /// `..` tile or the strip above the grid for the top level.
    pub fn drop_target(&self, x: i32, y: i32) -> Option<Dest> {
        let d = self.drag.filter(|d| d.active)?;
        // Recent only holds shortcuts: nothing is filed in or out of it.
        if self.open == Some(RECENT) {
            return None;
        }
        if self.open.is_some() && (0..GRID_Y).contains(&y) {
            return Some(Dest::Top);
        }
        let i = self.hit(x, y)?;
        if i == d.idx {
            return None;
        }
        match self.items[i].kind {
            Kind::Folder(f) => Some(Dest::Folder(f)),
            Kind::Back => Some(Dest::Top),
            Kind::App | Kind::Recent => None,
        }
    }

    /// The left button went up. Applies the drop, if any.
    pub fn release(&mut self, x: i32, y: i32) -> DragEnd {
        if self.sb_drag {
            self.sb_drag = false;
            return DragEnd::NotDragging;
        }
        let Some(d) = self.drag else { return DragEnd::NotDragging };
        if !d.active {
            self.drag = None;
            return DragEnd::NotDragging;
        }
        let dest = self.drop_target(x, y);
        let key = self.items.get(d.idx).map(|i| i.key());
        self.drag = None;
        match (dest, key) {
            (Some(dest), Some(key)) => {
                self.move_key(&key, dest);
                DragEnd::Moved
            }
            _ => DragEnd::Cancelled,
        }
    }

    /// Columns that fit the surface (4 grid / 2 list on the Cardputer Zero; more on a bigger screen).
    pub fn cols(&self) -> usize {
        let w = grid::sw();
        match self.view {
            View::Grid => (((w - GRID_X - (CELL_W - 4) - 8) / CELL_W) + 1).max(1) as usize,
            View::List => ((w - LIST_X) / LIST_COL_W).max(1) as usize,
        }
    }

    /// On a desk: icons per column (from the surface's height).
    fn desk_rows(&self) -> usize {
        (((grid::sh() - GRID_Y - 4) / CELL_H).max(1)) as usize
    }

    /// Home becomes (or stops being) the desk: no Recent tile there, icons from the right.
    pub fn set_desk(&mut self, on: bool) {
        let keep = self.sel_id();
        self.desk = on;
        if on && self.open == Some(RECENT) {
            self.open = None;
        }
        self.relayout(keep);
    }

    /// On a desk: the width the icon columns take at the right edge (windows open left of it).
    pub fn desk_icons_w(&self) -> i32 {
        if !self.desk_grid() {
            return 0;
        }
        let cols = self.items.len().div_ceil(self.desk_rows()).max(1) as i32;
        cols * CELL_W + 8
    }

    fn desk_grid(&self) -> bool {
        self.desk && self.view == View::Grid
    }

    /// Rows that fit the surface (3 grid / 10 list on the Cardputer Zero).
    pub fn vrows(&self) -> usize {
        let h = grid::sh();
        match self.view {
            View::Grid => (((h - GRID_Y - TILE_H) / CELL_H) + 1).max(1) as usize,
            View::List => ((h - LIST_Y - 2) / LIST_ROW_H).max(1) as usize,
        }
    }

    /// Number of pages (screenfuls of rows).
    pub fn pages(&self) -> usize {
        self.rows().div_ceil(self.vrows()).max(1)
    }

    pub fn page(&self) -> usize {
        self.selected / self.cols() / self.vrows() + 1
    }

    pub fn rows(&self) -> usize {
        self.items.len().div_ceil(self.cols())
    }

    /// Switch between the icon grid and the list, keeping the selection.
    pub fn toggle_view(&mut self) {
        if self.desk {
            return;
        }
        self.view = if self.view == View::Grid { View::List } else { View::Grid };
        self.scroll_row = 0;
        self.scroll_into_view();
    }

    /// Replace the items, keeping the selection on the same target if possible.
    pub fn set_items(&mut self, items: Vec<Item>) {
        let keep = self.sel_id();
        self.all = items;
        self.relayout(keep);
    }

    fn scroll_into_view(&mut self) {
        if self.desk_grid() {
            self.scroll_row = 0;
            return;
        }
        let (cols, vrows) = (self.cols(), self.vrows());
        let row = self.selected / cols;
        if row < self.scroll_row {
            self.scroll_row = row;
        } else if row >= self.scroll_row + vrows {
            self.scroll_row = row + 1 - vrows;
        }
        let max_scroll = self.rows().saturating_sub(vrows);
        self.scroll_row = self.scroll_row.min(max_scroll);
    }

    pub fn handle_key(&mut self, key: Keycode) -> HomeAction {
        // F / X / Z / C are the Fn-layer arrows (Up / Down / Left / Right) on the
        // Cardputer keyboard; on the home screen they work without holding Fn.
        let key = crate::platform::keys::arrow_alias(key);
        if self.open.is_some() && matches!(key, Keycode::Escape | Keycode::Backspace) {
            return self.close_folder();
        }
        let n = self.items.len();
        if n == 0 {
            return HomeAction::None;
        }
        let (cols, vrows) = (self.cols(), self.vrows());
        // 1..9 jump to that page (one page = the visible rows).
        let digit = match key {
            Keycode::Num1 => Some(1),
            Keycode::Num2 => Some(2),
            Keycode::Num3 => Some(3),
            Keycode::Num4 => Some(4),
            Keycode::Num5 => Some(5),
            Keycode::Num6 => Some(6),
            Keycode::Num7 => Some(7),
            Keycode::Num8 => Some(8),
            Keycode::Num9 => Some(9),
            _ => None,
        };
        if let Some(page) = digit {
            if page <= self.pages() {
                self.selected = ((page - 1) * vrows * cols).min(n - 1);
                self.scroll_row = (page - 1) * vrows;
                self.scroll_into_view();
            }
            return HomeAction::None;
        }
        if self.desk_grid() {
            // Columns run from the right edge leftwards, top to bottom.
            let rpc = self.desk_rows();
            let moved = match key {
                Keycode::Up => {
                    if self.selected % rpc > 0 {
                        self.selected -= 1;
                    }
                    true
                }
                Keycode::Down => {
                    if self.selected % rpc + 1 < rpc && self.selected + 1 < n {
                        self.selected += 1;
                    }
                    true
                }
                Keycode::Left => {
                    if self.selected + rpc < n {
                        self.selected += rpc;
                    } else if self.selected / rpc < (n - 1) / rpc {
                        self.selected = n - 1;
                    }
                    true
                }
                Keycode::Right => {
                    self.selected = self.selected.saturating_sub(rpc);
                    true
                }
                _ => false,
            };
            if moved {
                return HomeAction::None;
            }
        }
        match key {
            Keycode::Left => self.selected = self.selected.saturating_sub(1),
            Keycode::Right => self.selected = (self.selected + 1).min(n - 1),
            Keycode::Up => {
                if self.selected >= cols {
                    self.selected -= cols;
                }
            }
            Keycode::Down => {
                if self.selected + cols < n {
                    self.selected += cols;
                } else if self.selected / cols < (n - 1) / cols {
                    self.selected = n - 1;
                }
            }
            Keycode::PageUp => self.selected = self.selected.saturating_sub(cols * vrows),
            Keycode::PageDown => self.selected = (self.selected + cols * vrows).min(n - 1),
            Keycode::Home => self.selected = 0,
            Keycode::End => self.selected = n - 1,
            Keycode::Return | Keycode::KpEnter | Keycode::Space => {
                return match self.items[self.selected].kind {
                    Kind::App => HomeAction::Open(self.selected),
                    Kind::Folder(f) => self.open_folder(f),
                    Kind::Recent => self.open_folder(RECENT),
                    Kind::Back => self.close_folder(),
                };
            }
            _ => return HomeAction::None,
        }
        self.scroll_into_view();
        HomeAction::None
    }

    // ---- scroll bar (mouse) ----

    /// The scroll bar's track (None when everything fits).
    pub fn sb_track(&self) -> Option<Rect> {
        if self.desk_grid() {
            return None;
        }
        let (rows, vrows) = (self.rows(), self.vrows());
        if rows <= vrows {
            return None;
        }
        let (y, full) = match self.view {
            View::Grid => (GRID_Y, grid::sh() - 2 - GRID_Y),
            View::List => (LIST_Y, grid::sh() - 2 - LIST_Y),
        };
        Some(Rect::new(grid::scroll_x(), y, grid::SCROLL_W as u32, full as u32))
    }

    /// The shared scroll bar on `track`.
    fn scrollbar(&self, track: Rect) -> ScrollBar {
        ScrollBar::new(track, self.rows(), self.vrows(), self.scroll_row)
    }

    /// Press on the scroll bar column (a few pixels wider than it is drawn). The thumb starts a
    /// drag, the arrows step a row, the track above / below the thumb pages. True when the press
    /// was on the bar.
    pub fn sb_press(&mut self, x: i32, y: i32) -> bool {
        let Some(track) = self.sb_track() else { return false };
        if x < track.x() - 4 || x >= SCREEN_W_I {
            return false;
        }
        let x = x.clamp(track.x(), track.right() - 1);
        let Some((to, thumb)) = self.scrollbar(track).press(x, y) else { return false };
        if thumb {
            self.sb_drag = true;
        } else {
            self.scroll_row = to;
        }
        self.keep_selection_visible();
        true
    }

    /// Wheel: scroll by rows without moving the selection (it follows only if it would leave).
    pub fn wheel_scroll(&mut self, dy: i32) {
        let max = self.rows().saturating_sub(self.vrows());
        self.scroll_row = if dy > 0 { self.scroll_row.saturating_sub(1) } else { (self.scroll_row + 1).min(max) };
        self.keep_selection_visible();
    }

    /// After the view was scrolled by the mouse: pull the selection back into the window so that
    /// `scroll_into_view` does not undo the scrolling.
    fn keep_selection_visible(&mut self) {
        let (cols, vrows) = (self.cols(), self.vrows());
        let first = self.scroll_row * cols;
        let last = ((self.scroll_row + vrows) * cols).min(self.items.len());
        if last > first {
            self.selected = self.selected.clamp(first, last - 1);
        }
    }

    pub fn sb_dragging(&self) -> bool {
        self.sb_drag
    }

    /// The entry under the pointer, if any (visible entries only).
    pub fn hit(&self, px: i32, py: i32) -> Option<usize> {
        let (first, last) = if self.desk_grid() {
            (0, self.items.len())
        } else {
            (self.scroll_row * self.cols(), ((self.scroll_row + self.vrows()) * self.cols()).min(self.items.len()))
        };
        (first..last).find(|&idx| {
            let Some((cx, cy)) = self.cell_origin(idx) else { return false };
            let r = match self.view {
                View::Grid => Rect::new(cx, cy, (CELL_W - 4) as u32, TILE_H as u32),
                View::List => Rect::new(cx, cy, (LIST_COL_W - 3) as u32, (LIST_ROW_H - 1) as u32),
            };
            r.contains_point((px, py))
        })
    }

    /// Select an entry (e.g. from a click) and scroll it into view.
    pub fn select(&mut self, idx: usize) {
        if idx < self.items.len() {
            self.selected = idx;
            self.scroll_into_view();
        }
    }

    /// Screen rectangle of the selected entry (start/end of the zoom effect).
    pub fn tile_rect(&self, idx: usize) -> Option<Rect> {
        let (cx, cy) = self.cell_origin(idx)?;
        Some(match self.view {
            View::Grid => Rect::new(cx, cy, (CELL_W - 4) as u32, TILE_H as u32),
            View::List => Rect::new(cx, cy, (LIST_COL_W - 3) as u32, (LIST_ROW_H - 1) as u32),
        })
    }

    pub fn selected_icon_rect(&self) -> Rect {
        match (self.view, self.cell_origin(self.selected)) {
            (View::Grid, Some((cx, cy))) => Rect::new(cx + (CELL_W - ICON as i32) / 2, cy, ICON, ICON),
            (View::List, Some((x, y))) => Rect::new(x, y, (LIST_COL_W - 3) as u32, (LIST_ROW_H - 1) as u32),
            _ => Rect::new(148, 60, ICON, ICON),
        }
    }

    fn cell_origin(&self, idx: usize) -> Option<(i32, i32)> {
        if self.desk_grid() {
            let rpc = self.desk_rows();
            let (col, row) = ((idx / rpc) as i32, (idx % rpc) as i32);
            let x = grid::sw() - 8 - (col + 1) * CELL_W + 4;
            return (x >= 0).then_some((x, GRID_Y + 6 + row * CELL_H));
        }
        let (cols, vrows) = (self.cols(), self.vrows());
        let row = idx / cols;
        if row < self.scroll_row || row >= self.scroll_row + vrows {
            return None;
        }
        let r = (row - self.scroll_row) as i32;
        let c = (idx % cols) as i32;
        Some(match self.view {
            View::Grid => (GRID_X + c * CELL_W, GRID_Y + r * CELL_H),
            View::List => (LIST_X + c * LIST_COL_W, LIST_Y + r * LIST_ROW_H),
        })
    }

    pub fn draw(&self, c: &mut Canvas<Window>, assets: &mut AssetStore, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        // The selected tile uses the palette's otherwise unused grey (tile_sel_bg), not the white.
        let sel_bg = tm.get_color("tile_sel_bg");
        // Selection does not invert anything: the plate changes, the icon and the name keep their colours.
        let sel_fg = ink;
        let paper_cream = tm.get_color("widget_face_active");

        // Backdrop: a faint dithered wash so the page has a ground, not a void.
        // (On a desk this wash is the desk itself.)
        let body = Rect::new(0, chrome::BAR_H, crate::chrome::screen_w(), crate::chrome::screen_h() - chrome::BAR_H as u32);
        crate::dither::fill_two_tone(c, body, tm.get_color("home_bg"), tm.get_color("home_bg_alt"), crate::dither::Pattern::Quarter);
        let tile_fill = tm.get_color("surface_sunken");
        let _faint = crate::launcher::widgets::secondary_ink(tm);

        // While dragging, the folder tile under the pointer is the drop target.
        let drop = self.drag.filter(|d| d.active).and_then(|d| match self.drop_target(d.pos.0, d.pos.1) {
            Some(_) => self.hit(d.pos.0, d.pos.1).filter(|i| *i != d.idx),
            None => None,
        });
        for (idx, item) in self.items.iter().enumerate() {
            let Some((cx, cy)) = self.cell_origin(idx) else { continue };
            let selected = idx == self.selected;
            if self.view == View::List {
                // One plate per entry; inverted when selected.
                let plate = Rect::new(cx, cy, (LIST_COL_W - 3) as u32, (LIST_ROW_H - 1) as u32);
                ui::fill_rect(c, plate, if selected { ink } else if (idx / 2) % 2 == 0 { tile_fill } else { tm.get_color("surface_alt") });
                let fg = if selected { paper_cream } else { ink };
                let label_s = shown_name(item);
                let label = label_s.as_str();
                if item.kind == Kind::App {
                    let name = chrome::fit_display_text(fs, label, LIST_COL_W - 14);
                    fs.draw_display_text(c, cx + 5, cy, &name, fg);
                } else {
                    // Folder rows: a folder glyph in front and a ">" at the end; Back gets "<".
                    if item.kind == Kind::Back {
                        draw_triangle(c, cx + 5, cy + 3, false, fg);
                    } else {
                        draw_folder_glyph(c, cx + 4, cy + 3, fg);
                    }
                    let name = chrome::fit_display_text(fs, label, LIST_COL_W - 40);
                    fs.draw_display_text(c, cx + 18, cy, &name, fg);
                    if item.kind != Kind::Back {
                        draw_triangle(c, cx + LIST_COL_W - 3 - 10, cy + 3, true, fg);
                    }
                }
                if drop == Some(idx) {
                    ui::draw_rect(c, plate, ink);
                    ui::draw_rect(c, Rect::new(plate.x() - 1, plate.y() - 1, plate.width() + 2, plate.height() + 2), ink);
                }
                continue;
            }
            // No frame and no plate: icons and names sit directly on the background. Only the
            // selected tile gets a plain filled plate (icon and name inverted), without an outline.
            let tile = Rect::new(cx, cy, (CELL_W - 4) as u32, TILE_H as u32);
            let icon_rect = Rect::new(cx + (CELL_W - 4 - ICON as i32) / 2, cy + 1, ICON, ICON);
            // The selection is a square behind the icon (1 px around it), not the whole cell.
            if selected {
                ui::fill_rect(c, Rect::new(icon_rect.x() - 1, icon_rect.y() - 1, ICON + 2, ICON + 2), sel_bg);
            }
            match item.kind {
                Kind::App => draw_icon(c, assets, item, icon_rect, icon_palette(tm)),
                // Folders (and Recent) show the drawn folder icon: four app icons shrunk to 12 px could
                // not be read. Without the icon file, the old drawn folder card stays as a fallback.
                Kind::Folder(_) | Kind::Recent => {
                    if assets.size("icons/Folder").is_some() {
                        let _ = assets.blit_icon(c, "icons/Folder", icon_rect, icon_palette(tm));
                    } else {
                        draw_folder_icon(c, assets, icon_rect, &item.minis, ink, paper_cream, icon_palette(tm));
                    }
                }
                Kind::Back => draw_back_icon(c, icon_rect, ink),
            }
            if drop == Some(idx) {
                ui::draw_rect(c, Rect::new(tile.x() - 1, tile.y() - 1, tile.width() + 2, tile.height() + 2), ink);
                ui::draw_rect(c, tile, ink);
                ui::draw_rect(c, Rect::new(tile.x() + 1, tile.y() + 1, tile.width() - 2, tile.height() - 2), ink);
            }

            let name = chrome::fit_display_text(fs, &shown_name(item), CELL_W - 4);
            let w = fs.measure_display_text(&name);
            let label_y = cy + ICON as i32 + 1;
            if selected {
                fs.draw_display_text(c, cx + (CELL_W - 4 - w) / 2, label_y, &name, sel_fg);
            } else {
                fs.draw_display_text(c, cx + (CELL_W - 4 - w) / 2, label_y, &name, ink);
            }
        }

        // Scroll bar.
        if let Some(track) = self.sb_track() {
            self.scrollbar(track).draw(c, tm, self.sb_drag.then_some(SbPart::Thumb));
        }

        // The dragged tile follows the pointer with a dither shadow.
        if let Some(d) = self.drag.filter(|d| d.active) {
            if let Some(item) = self.items.get(d.idx) {
                let r = Rect::new(d.pos.0 - ICON as i32 / 2, d.pos.1 - ICON as i32 / 2, ICON, ICON);
                crate::dither::fill(c, Rect::new(r.x() + 3, r.y() + 3, ICON, ICON), tm.get_color("widget_shadow"), crate::dither::Pattern::Half);
                ui::fill_rect(c, r, tile_fill);
                ui::draw_rect(c, r, ink);
                draw_icon(c, assets, item, r, icon_palette(tm));
            }
        }
    }
}

const SCREEN_W_I: i32 = 320;

fn item_id(i: &Item) -> String {
    match i.kind {
        Kind::App => i.key(),
        Kind::Folder(_) => folder_id(&i.name),
        Kind::Recent => "\0recent".into(),
        Kind::Back => "\0back".into(),
    }
}

fn folder_id(name: &str) -> String {
    format!("\0folder\0{name}")
}

/// A folder as a plain square card (no tab) in a 32x32 box, with up to four app icons (12 px, drawn in the
/// same three tones as the tiles) inside. A square with 2 px chamfered corners; an empty folder is a dotted one.
fn draw_folder_icon(c: &mut Canvas<Window>, assets: &mut AssetStore, r: Rect, minis: &[String], ink: Color, fill: Color, palette: IconPalette) {
    let (x, y) = (r.x() + 1, r.y() + 1); // 30 x 30 card, 1 px margin like the other icons
    const S: i32 = 30;
    let corner = |px: i32, py: i32| (px == 0 || px == S - 1) && (py == 0 || py == S - 1);
    if minis.is_empty() {
        c.set_draw_color(ink);
        for i in (2..S - 2).step_by(2) {
            for (px, py) in [(i, 0), (i, S - 1), (0, i), (S - 1, i)] {
                let _ = c.draw_point((x + px, y + py));
            }
        }
        return;
    }
    ui::fill_rect(c, Rect::new(x, y, S as u32, S as u32), ink);
    ui::fill_rect(c, Rect::new(x + 1, y + 1, (S - 2) as u32, (S - 2) as u32), fill);
    // Chamfer the four corners (transparent notch).
    c.set_blend_mode(sdl2::render::BlendMode::None);
    let _ = corner;
    for (cx, cy) in [(0, 0), (S - 1, 0), (0, S - 1), (S - 1, S - 1)] {
        // Replace the corner pixel by the colour the card sits on: skip (leave the ink pixel): a 1 px notch
        // would need the background colour, so the corner stays square but the outline is closed.
        let _ = (cx, cy);
    }
    for (i, key) in minis.iter().take(4).enumerate() {
        let (col, row) = ((i % 2) as i32, (i / 2) as i32);
        let _ = assets.blit_icon(c, key, Rect::new(x + 3 + col * 14, y + 3 + row * 14, 12, 12), palette);
    }
}

/// The back tile's glyph: a left arrow with a shaft.
fn draw_back_icon(c: &mut Canvas<Window>, r: Rect, ink: Color) {
    let (cx, cy) = (r.x() + 16, r.y() + 16);
    for j in 0..9 {
        ui::fill_rect(c, Rect::new(cx - 11 + j, cy - j, 1, (2 * j + 1) as u32), ink);
    }
    ui::fill_rect(c, Rect::new(cx - 3, cy - 2, 14, 5), ink);
}

/// A 4 x 9 pointer triangle (list rows: "opens" on the right, "back" on the left).
fn draw_triangle(c: &mut Canvas<Window>, x: i32, y: i32, right: bool, col: Color) {
    for i in 0..5 {
        let h = 9 - 2 * i;
        let col_x = if right { x + i } else { x + 4 - i };
        ui::fill_rect(c, Rect::new(col_x, y + i, 1, h as u32), col);
    }
}

/// Small folder glyph for list rows (9 x 8).
fn draw_folder_glyph(c: &mut Canvas<Window>, x: i32, y: i32, col: Color) {
    ui::fill_rect(c, Rect::new(x, y, 4, 2), col);
    ui::fill_rect(c, Rect::new(x, y + 2, 10, 7), col);
}

/// Draw an app icon in the given three-tone palette (the old `_selected` variants are not used).
fn draw_icon(c: &mut Canvas<Window>, assets: &mut AssetStore, item: &Item, rect: Rect, palette: IconPalette) {
    let _ = assets.blit_icon(c, &item.icon, rect, palette);
}

/// Icon colours on the plain background: ink / muted / `icon_white`.
fn icon_palette(tm: &ThemeManager) -> IconPalette {
    IconPalette { ink: tm.get_color("foreground"), muted: Some(tm.get_color("muted")), white: Some(tm.get_color("icon_white")) }
}

/// Icon colours on the selected plate: black = `select_fg`, grey = halfway to the plate, white = the plate.
fn icon_palette_selected(tm: &ThemeManager) -> IconPalette {
    let (bg, fg) = (tm.get_color("tile_sel_bg"), tm.get_color("tile_sel_fg"));
    let mid = |a: u8, b: u8| ((a as u16 + b as u16) / 2) as u8;
    IconPalette { ink: fg, muted: Some(Color::RGB(mid(bg.r, fg.r), mid(bg.g, fg.g), mid(bg.b, fg.b))), white: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(n: usize) -> Home {
        Home::new(
            (0..n)
                .map(|i| {
                    Item::app(
                        format!("{}App{i}", (b'A' + (i % 26) as u8) as char),
                        Target::External(format!("{i}.desktop")),
                        "x".into(),
                        None,
                    )
                })
                .collect(),
        )
    }

    /// Harbour's own apps as Home builds them (English name, package id as key).
    fn builtin_home() -> Home {
        let items = crate::launcher::packages::builtins()
            .into_iter()
            .map(|(id, b)| Item::app(id.to_string(), Target::Builtin(b), format!("app/{id}/icon"), None))
            .collect();
        Home::new(items)
    }

    #[test]
    fn old_folders_and_recent_with_display_names_become_ids() {
        let dir = std::env::temp_dir().join(format!("harbour-home-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fj = dir.join("folders.json");
        std::fs::write(&fj, r#"{"version":1,"folders":[{"name":"Tools","items":["Text Editor","Recorder","fune.desktop"]},{"name":"Games","items":["Snake","Images"]}]}"#).unwrap();
        std::fs::write(dir.join("recent.txt"), "Paint\nfune.desktop\nRecorder\nvoicememo\n").unwrap();
        let mut h = builtin_home();
        h.set_folders_path(fj.clone());
        h.set_recent_path(dir.join("recent.txt"));
        assert_eq!(h.folders.list[0].items, ["texteditor", "voicememo", "fune.desktop"]);
        assert_eq!(h.folders.list[1].items, ["snake", "images"]);
        assert_eq!(h.recent, ["paint", "fune.desktop", "voicememo"], "old and new key of one app merge");
        // The file is rewritten with ids, so the next start reads them directly.
        let saved = Folders::load(&fj);
        assert_eq!(saved.list[0].items[0], "texteditor");
        assert_eq!(std::fs::read_to_string(dir.join("recent.txt")).unwrap(), "paint\nfune.desktop\nvoicememo\n");
        assert!(h.all.iter().any(|i| i.key() == "texteditor"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn switching_the_language_keeps_folders_and_recent() {
        use crate::launcher::i18n::Lang;
        let dir = std::env::temp_dir().join(format!("harbour-home-lang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut h = builtin_home();
        h.set_folders_path(dir.join("folders.json"));
        h.set_recent_path(dir.join("recent.txt"));
        let f = h.create_folder("Work");
        h.move_key("paint", Dest::Folder(f));
        h.record_launch("calculator");
        // The names differ by language, the keys do not.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(crate::launcher::packages::APPS_ROOT);
        let paint = crate::launcher::packages::scan(&root).into_iter().find(|p| p.id == "paint").unwrap();
        assert_ne!(paint.name_in(Lang::En), paint.name_in(Lang::Ja));
        // A language switch rebuilds the items (as the launcher does) and reloads the saved files.
        let items = h.all.clone();
        h.set_items(items);
        let mut again = builtin_home();
        again.set_folders_path(dir.join("folders.json"));
        again.set_recent_path(dir.join("recent.txt"));
        for x in [&h, &again] {
            assert_eq!(x.folders.list[0].items, ["paint"]);
            assert_eq!(x.recent_items().iter().map(|i| i.key()).collect::<Vec<_>>(), ["calculator"]);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arrows_move_and_scroll_by_row() {
        let mut h = home(40);
        for _ in 0..2 {
            h.handle_key(Keycode::Down);
        }
        assert_eq!((h.selected, h.scroll_row), (8, 0), "three rows are visible");
        h.handle_key(Keycode::Down);
        assert_eq!((h.selected, h.scroll_row), (12, 1), "the fourth row scrolls by one");
        h.handle_key(Keycode::Up);
        assert_eq!((h.selected, h.scroll_row), (8, 1), "row 2 is still visible");
        h.handle_key(Keycode::Up);
        h.handle_key(Keycode::Up);
        assert_eq!((h.selected, h.scroll_row), (0, 0), "row 0 scrolls back");
    }

    #[test]
    fn edges_clamp() {
        let mut h = home(6);
        h.handle_key(Keycode::Left);
        assert_eq!(h.selected, 0);
        h.handle_key(Keycode::End);
        assert_eq!(h.selected, 5);
        h.handle_key(Keycode::Right);
        assert_eq!(h.selected, 5);
        // Down from the first row's last column lands on the final item when the last row is short.
        let mut h = home(6);
        h.selected = 3;
        h.handle_key(Keycode::Down);
        assert_eq!(h.selected, 5);
    }

    #[test]
    fn number_keys_jump_between_pages() {
        let mut h = home(30); // 8 rows -> 3 pages of 3 rows
        assert_eq!(h.pages(), 3);
        assert_eq!(h.handle_key(Keycode::Num2), HomeAction::None, "digits never launch");
        assert_eq!((h.selected, h.scroll_row, h.page()), (12, 3, 2));
        h.handle_key(Keycode::Num3);
        assert_eq!((h.selected, h.page()), (24, 3));
        assert_eq!(h.scroll_row, 5, "last page is clamped to the final rows");
        h.handle_key(Keycode::Num1);
        assert_eq!((h.selected, h.scroll_row), (0, 0));
        h.handle_key(Keycode::Num9);
        assert_eq!(h.selected, 0, "a page that does not exist is ignored");
    }

    #[test]
    fn tab_toggles_between_grid_and_two_column_list() {
        let mut h = home(40);
        for _ in 0..5 {
            h.handle_key(Keycode::Right);
        }
        assert_eq!(h.selected, 5);
        h.toggle_view();
        assert_eq!(h.view, View::List);
        assert_eq!(h.selected, 5, "the selection survives the switch");
        h.handle_key(Keycode::Down);
        assert_eq!(h.selected, 7, "two columns: Down moves by 2");
        for _ in 0..7 {
            h.handle_key(Keycode::Down);
        }
        assert_eq!(h.selected, 21);
        assert_eq!(h.scroll_row, 1, "row 11 of 10 visible scrolls the list");
        assert!(h.cell_origin(21).is_some(), "the selection stays on screen");
        h.toggle_view();
        assert_eq!(h.view, View::Grid);
        assert!(h.cell_origin(h.selected).is_some());
    }

    #[test]
    fn list_pages_hold_twenty_entries() {
        let mut h = home(50);
        h.toggle_view();
        assert_eq!(h.pages(), 3, "50 items = 25 rows of 2, 10 rows per page");
        h.handle_key(Keycode::Num2);
        assert_eq!((h.selected, h.scroll_row), (20, 10));
    }

    #[test]
    fn the_pointer_hits_icons_and_list_rows() {
        let mut h = home(40);
        // Grid: the first icon's cell is at (4, 22).
        assert_eq!(h.hit(30, 30), Some(0));
        assert_eq!(h.hit(4 + 78 + 30, 30), Some(1), "second column");
        assert_eq!(h.hit(30, 22 + 50 + 10), Some(4), "second row");
        assert_eq!(h.hit(2, 5), None, "the menu bar is not an entry");
        assert_eq!(h.hit(318, 100), None, "the scroll bar column is not an entry");
        // Entries below the visible rows are not hit.
        assert_eq!(h.hit(30, 169), None);
        h.toggle_view();
        assert_eq!(h.hit(20, 25), Some(0));
        assert_eq!(h.hit(5 + 155 + 20, 25), Some(1), "second column of the list");
        assert_eq!(h.hit(20, 21 + 15 * 3 + 2), Some(6), "fourth row");
        // After scrolling, rows map to later entries.
        for _ in 0..12 {
            h.handle_key(Keycode::Down);
        }
        assert!(h.scroll_row > 0);
        assert_eq!(h.hit(20, 25), Some(h.scroll_row * 2));
    }

    #[test]
    fn clicking_selects_and_scrolls_into_view() {
        let mut h = home(40);
        h.select(30);
        assert_eq!(h.selected, 30);
        assert!(h.hit(30, 30).is_some() || h.scroll_row > 0);
        h.select(999);
        assert_eq!(h.selected, 30, "out of range is ignored");
    }

    #[test]
    fn scroll_bar_press_drag_and_wheel() {
        let mut h = home(40);
        let track = h.sb_track().expect("40 entries scroll");
        assert!(h.sb_track().is_some() && home(4).sb_track().is_none());
        let max = h.rows() - h.vrows();
        // Track below the thumb: a page down (never past the end); the selection comes along.
        assert!(h.sb_press(track.x() + 1, track.y() + track.height() as i32 - 6));
        assert_eq!(h.scroll_row, h.vrows().min(max));
        assert!(h.selected / h.cols() >= h.scroll_row);
        // A press next to the bar (a little wider than drawn) counts, one far left does not.
        assert!(h.sb_press(track.x() - 3, track.y() + 5));
        assert_eq!(h.scroll_row, 0);
        assert!(!h.sb_press(track.x() - 30, track.y() + 1));
        // The arrow boxes step one row.
        assert!(h.sb_press(track.x() + 1, track.y() + track.height() as i32 - 1));
        assert_eq!(h.scroll_row, 1);
        assert!(h.sb_press(track.x() + 1, track.y() + 1));
        assert_eq!(h.scroll_row, 0);
        // Thumb: starts a drag, moving follows the pointer, release ends it without a drop.
        let thumb = ScrollBar::new(track, h.rows(), h.vrows(), h.scroll_row).thumb();
        assert!(h.sb_press(track.x() + 1, thumb.y() + 1));
        assert!(h.sb_dragging());
        assert!(!h.drag_move(track.x(), track.y() + track.height() as i32 + 50));
        assert_eq!(h.scroll_row, max);
        h.drag_move(track.x(), track.y() - 50);
        assert_eq!(h.scroll_row, 0);
        assert_eq!(h.release(0, 0), DragEnd::NotDragging);
        assert!(!h.sb_dragging());
        // The wheel scrolls rows and keeps the selection visible.
        h.wheel_scroll(-1);
        h.wheel_scroll(-1);
        assert_eq!(h.scroll_row, 2);
        assert!(h.selected / h.cols() >= 2);
        h.wheel_scroll(1);
        assert_eq!(h.scroll_row, 1);
    }

    #[test]
    fn enter_opens_selection() {
        let mut h = home(5);
        h.handle_key(Keycode::Right);
        assert_eq!(h.handle_key(Keycode::Return), HomeAction::Open(1));
    }

    #[test]
    fn fzxc_are_arrows_without_fn() {
        let mut h = home(30);
        h.handle_key(Keycode::C); // right
        h.handle_key(Keycode::C);
        assert_eq!(h.selected, 2);
        h.handle_key(Keycode::X); // down
        assert_eq!(h.selected, 6);
        h.handle_key(Keycode::Z); // left
        assert_eq!(h.selected, 5);
        h.handle_key(Keycode::F); // up
        assert_eq!(h.selected, 1);
    }

    #[test]
    fn set_items_keeps_selection_on_same_target() {
        let mut h = home(10);
        h.selected = 6;
        let mut items = h.items.clone();
        items.remove(0);
        h.set_items(items);
        assert_eq!(h.selected, 5);
    }

    // ---- folders ----

    fn with_folders(n: usize) -> Home {
        let mut h = home(n);
        let a = h.create_folder("Games");
        h.create_folder("Music");
        h.move_key("2.desktop", Dest::Folder(a));
        h.move_key("0.desktop", Dest::Folder(a));
        h.select(0);
        h
    }

    fn names(h: &Home) -> Vec<String> {
        h.items.iter().map(|i| i.name.clone()).collect()
    }

    #[test]
    fn the_top_level_lists_folders_first_then_loose_apps_in_order() {
        let h = with_folders(6);
        assert_eq!(names(&h), ["Games", "Music", "BApp1", "DApp3", "EApp4", "FApp5"]);
        assert_eq!(h.items[0].kind, Kind::Folder(0));
        assert_eq!(h.items[0].minis, ["x", "x"], "icons of its apps, in folder order");
        assert_eq!(h.items[2].kind, Kind::App);
        assert_eq!(h.context(), "");
    }

    #[test]
    fn opening_a_folder_shows_a_back_tile_then_its_apps_in_the_users_order() {
        let mut h = with_folders(6);
        assert!(matches!(h.handle_key(Keycode::Return), HomeAction::Zoom { .. }));
        assert!(h.in_folder());
        assert_eq!(names(&h), ["..", "CApp2", "AApp0"], "folder order, not the global order");
        assert_eq!(h.items[0].kind, Kind::Back);
        assert_eq!(h.context(), "Games");
        assert_eq!(h.selected, 1, "the first app is selected, not Back");
        assert_eq!(h.handle_key(Keycode::Return), HomeAction::Open(1), "apps open as usual");
        h.handle_key(Keycode::Left);
        assert!(matches!(h.handle_key(Keycode::Return), HomeAction::Zoom { .. }), "the Back tile closes it");
        assert!(!h.in_folder());
        assert_eq!(h.selected, 0, "the folder tile is selected again");
    }

    #[test]
    fn esc_and_backspace_leave_a_folder_but_do_nothing_on_the_top_level() {
        let mut h = with_folders(6);
        assert_eq!(h.handle_key(Keycode::Escape), HomeAction::None);
        assert_eq!(h.handle_key(Keycode::Backspace), HomeAction::None);
        h.handle_key(Keycode::Right);
        h.handle_key(Keycode::Return); // open Music (empty)
        assert_eq!(names(&h), [".."]);
        assert!(matches!(h.handle_key(Keycode::Escape), HomeAction::Zoom { .. }));
        assert_eq!(h.selected, 1, "back on the Music tile");
        h.handle_key(Keycode::Return);
        h.handle_key(Keycode::Backspace);
        assert!(!h.in_folder());
    }

    #[test]
    fn folders_in_a_set_items_refresh_keep_the_open_folder_and_selection() {
        let mut h = with_folders(6);
        h.handle_key(Keycode::Return);
        h.handle_key(Keycode::Right);
        assert_eq!(h.items[h.selected].name, "AApp0");
        let items = h.all.clone();
        h.set_items(items); // e.g. after an app ran
        assert!(h.in_folder());
        assert_eq!(h.items[h.selected].name, "AApp0");
        // An app that was hidden / uninstalled simply is not shown.
        let rest: Vec<Item> = h.all.iter().filter(|i| i.key() != "2.desktop").cloned().collect();
        h.set_items(rest);
        assert_eq!(names(&h), ["..", "AApp0"]);
        assert!(h.folders.contains("2.desktop"), "hidden apps keep their folder");
    }

    #[test]
    fn deleting_a_folder_returns_its_apps_to_the_top_level() {
        let mut h = with_folders(6);
        h.delete_folder(0);
        assert_eq!(names(&h), ["Music", "AApp0", "BApp1", "CApp2", "DApp3", "EApp4", "FApp5"]);
        assert_eq!(h.items[0].kind, Kind::Folder(0));
        // Deleting the open folder closes it.
        let mut h = with_folders(6);
        h.handle_key(Keycode::Return);
        h.delete_folder(0);
        assert!(!h.in_folder());
        assert_eq!(h.items.len(), 7);
    }

    #[test]
    fn rename_keeps_the_selection_on_the_folder() {
        let mut h = with_folders(4);
        h.rename_folder(0, "Fun");
        assert_eq!(h.items[0].name, "Fun");
        assert_eq!(h.selected, 0);
        h.handle_key(Keycode::Return);
        h.rename_folder(0, "Fun and games");
        assert_eq!(h.context(), "Fun and games");
    }

    #[test]
    fn move_options_and_menu_entries_follow_the_selection() {
        let mut h = with_folders(6);
        let ids = |h: &Home| h.menu_entries().iter().map(|e| e.0).collect::<Vec<_>>();
        assert_eq!(ids(&h), ["hf_new", "hf_rename", "hf_delete", "hf_auto"], "a folder is selected");
        assert!(h.move_options().is_empty());
        h.select(2);
        assert_eq!(ids(&h), ["hf_new", "hf_move", "hf_auto"], "a loose app");
        let opts: Vec<_> = h.move_options().into_iter().map(|(n, _)| n).collect();
        assert_eq!(opts, ["Games", "Music"]);
        h.handle_key(Keycode::Return); // opens an app (no change)
        h.select(0);
        h.handle_key(Keycode::Return); // open Games
        h.handle_key(Keycode::Right); // selects AApp0
        let opts: Vec<_> = h.move_options().into_iter().map(|(n, _)| n).collect();
        assert_eq!(opts, ["Music", "Top level"], "not the folder it is in; Top level to leave");
        assert_eq!(ids(&h), ["hf_new", "hf_move", "hf_rename", "hf_delete", "hf_auto"], "inside a folder the folder can be renamed");
        h.move_selected(Dest::Top);
        assert_eq!(names(&h), ["..", "CApp2"]);
        assert_eq!(h.selected, 1, "the selection stays near where the app left");
    }

    #[test]
    fn a_folder_is_laid_out_in_the_grid_and_the_list_like_any_item() {
        let mut h = with_folders(20);
        assert_eq!(h.items.len(), 2 + 18);
        let r = h.tile_rect(1).unwrap();
        assert_eq!((r.x(), r.y()), (4 + 76, 18), "second tile of the first row");
        assert_eq!(h.hit(r.x() + 5, r.y() + 5), Some(1));
        h.toggle_view();
        assert_eq!(h.hit(5 + 155 + 5, 25), Some(1), "list: second column");
        let r = h.tile_rect(1).unwrap();
        assert_eq!((r.x(), r.y(), r.width(), r.height()), (160, 18, 152, 14));
        h.select(0);
        assert!(matches!(h.handle_key(Keycode::Return), HomeAction::Zoom { .. }));
        assert_eq!(names(&h), ["..", "CApp2", "AApp0"], "the list shows the folder's rows after a Back row");
    }

    #[test]
    fn folders_survive_a_restart_through_the_json_file() {
        let dir = std::env::temp_dir().join(format!("harbour-home-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("folders.json");
        let mut h = home(5);
        h.set_folders_path(path.clone());
        let g = h.create_folder("Games");
        h.move_key("1.desktop", Dest::Folder(g));
        let mut h2 = home(5);
        h2.set_folders_path(path.clone());
        assert_eq!(names(&h2), ["Games", "AApp0", "CApp2", "DApp3", "EApp4"]);
        // An uninstalled app is dropped on the next refresh, and the file follows.
        h2.prune_missing(&["0.desktop".to_string(), "2.desktop".to_string()]);
        assert!(!h2.folders.contains("1.desktop"));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("1.desktop"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_organize_groups_known_apps() {
        let mut h = home(0);
        let mk = |n: &str| Item::app(n.into(), Target::External(format!("{n}.desktop")), "x".into(), None);
        h.set_items(vec![mk("Snake"), mk("Piano"), mk("Weather"), mk("Calculator")]);
        assert_eq!(h.auto_organize(), 3);
        assert_eq!(names(&h), ["Games", "Music", "Tools", "Weather"]);
    }

    // ---- dragging ----

    #[test]
    fn a_small_movement_is_a_click_not_a_drag() {
        let mut h = with_folders(6);
        let r = h.tile_rect(2).unwrap();
        h.press(2, r.x() + 5, r.y() + 5);
        assert!(!h.drag_move(r.x() + 8, r.y() + 8));
        assert_eq!(h.release(r.x() + 8, r.y() + 8), DragEnd::NotDragging);
        assert_eq!(h.items.len(), 6);
    }

    #[test]
    fn dropping_an_app_on_a_folder_tile_moves_it_in() {
        let mut h = with_folders(6);
        let from = h.tile_rect(3).unwrap(); // DApp3
        h.press(3, from.x() + 10, from.y() + 10);
        assert!(h.drag_move(40, 30), "moved far enough");
        assert!(h.is_dragging());
        assert_eq!(h.drop_target(40, 30), Some(Dest::Folder(0)));
        assert_eq!(h.drop_target(40 + 76, 30), Some(Dest::Folder(1)));
        assert_eq!(h.drop_target(200, 100), None, "empty space cancels");
        assert_eq!(h.release(40, 30), DragEnd::Moved);
        assert_eq!(h.folders.list[0].items, ["2.desktop", "0.desktop", "3.desktop"], "appended at the end");
        assert!(!h.items.iter().any(|i| i.name == "DApp3"));
        assert!(h.drag.is_none());
    }

    #[test]
    fn dropping_outside_cancels_and_folders_cannot_be_dragged() {
        let mut h = with_folders(6);
        h.press(2, 100, 80);
        h.drag_move(250, 130);
        assert_eq!(h.release(250, 130), DragEnd::Cancelled);
        assert_eq!(h.items.len(), 6);
        h.press(0, 10, 30); // a folder tile
        assert!(!h.drag_move(100, 100), "only apps follow the pointer");
        assert_eq!(h.release(100, 100), DragEnd::NotDragging);
        // Dropping on the tile it started from, or on another app, does nothing.
        h.press(2, 100, 30);
        h.drag_move(180, 30);
        assert_eq!(h.drop_target(180, 30), None);
    }

    #[test]
    fn dragging_out_of_an_open_folder_to_the_strip_or_back_tile_returns_it() {
        let mut h = with_folders(6);
        h.handle_key(Keycode::Return);
        h.press(1, 100, 40);
        h.drag_move(100, 10);
        assert_eq!(h.drop_target(100, 10), Some(Dest::Top), "the strip above the grid");
        assert_eq!(h.drop_target(30, 40), Some(Dest::Top), "the back tile");
        assert_eq!(h.release(100, 10), DragEnd::Moved);
        assert_eq!(names(&h), ["..", "AApp0"]);
        assert_eq!(h.folders.list[0].items, ["0.desktop"]);
        h.press(1, 100, 40);
        h.drag_move(250, 100);
        assert_eq!(h.release(250, 100), DragEnd::Cancelled, "inside a folder only the way out is a target");
    }

    #[test]
    fn the_strip_is_not_a_target_on_the_top_level() {
        let mut h = with_folders(6);
        h.press(2, 100, 40);
        h.drag_move(100, 10);
        assert_eq!(h.drop_target(100, 10), None);
    }

    // ---- Recent (virtual folder) ----

    #[test]
    fn recent_is_hidden_until_something_was_launched_and_lists_newest_first() {
        let mut h = home(12);
        assert_eq!(h.items[0].name, "AApp0", "no Recent tile when empty");
        h.record_launch("3.desktop");
        h.record_launch("5.desktop");
        h.record_launch("3.desktop"); // again: moves to the front, no duplicate
        assert_eq!(h.recent, ["3.desktop", "5.desktop"]);
        assert_eq!(h.items[0].kind, Kind::Recent);
        assert_eq!(h.items[0].minis.len(), 2);
        assert_eq!(h.items.len(), 13, "apps stay where they are; Recent only adds a tile");
        h.select(0);
        assert!(matches!(h.handle_key(Keycode::Return), HomeAction::Zoom { .. }));
        assert_eq!(h.context(), "Recent");
        assert_eq!(names(&h), ["..", "DApp3", "FApp5"]);
        assert_eq!(h.target_folder(), None, "Recent cannot be renamed or deleted");
        assert!(!h.menu_entries().iter().any(|e| e.0 == "hf_delete"));
        h.handle_key(Keycode::Escape);
        assert_eq!(h.selected, 0, "back on the Recent tile");
    }

    #[test]
    fn recent_keeps_eight_ignores_missing_apps_and_does_not_touch_folders() {
        let mut h = home(20);
        for i in 0..12 {
            h.record_launch(&format!("{i}.desktop"));
        }
        assert_eq!(h.recent.len(), RECENT_MAX);
        assert_eq!(h.recent[0], "11.desktop");
        h.recent.insert(0, "gone.desktop".into());
        h.relayout(None);
        h.handle_key(Keycode::Return);
        assert_eq!(h.items.len(), 1 + 8, "an uninstalled app is not shown");
        assert!(h.folders.is_empty(), "Recent is not a stored folder");
        // Dragging is off inside Recent, and apps in folders still show up there.
        h.press(1, 100, 40);
        h.drag_move(100, 10);
        assert_eq!(h.drop_target(100, 10), None);
    }

    #[test]
    fn recent_persists_in_recent_txt() {
        let dir = std::env::temp_dir().join(format!("harbour-home-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("recent.txt");
        let mut h = home(6);
        h.set_recent_path(path.clone());
        h.record_launch("1.desktop");
        h.record_launch("4.desktop");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "4.desktop\n1.desktop\n");
        let mut h2 = home(6);
        h2.set_recent_path(path.clone());
        assert_eq!(h2.recent, ["4.desktop", "1.desktop"]);
        assert_eq!(h2.items[0].kind, Kind::Recent);
        std::fs::write(&path, "\n\n").unwrap();
        let mut h3 = home(6);
        h3.set_recent_path(path);
        assert!(h3.items[0].kind == Kind::App, "an empty file hides Recent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn digits_still_jump_to_pages_inside_a_folder_too() {
        let mut h = home(30);
        let g = h.create_folder("Big");
        for i in 0..20 {
            h.move_key(&format!("{i}.desktop"), Dest::Folder(g));
        }
        h.select(0);
        h.handle_key(Keycode::Return);
        assert_eq!(h.pages(), 2, "21 tiles = 6 rows of 4 = 2 pages");
        h.handle_key(Keycode::Num2);
        assert_eq!(h.selected, 12);
    }

    #[test]
    fn context_entries_depend_on_what_is_under_the_pointer() {
        let h = home(5);
        fn ids(v: Vec<(&'static str, &'static str)>) -> Vec<&'static str> {
            v.into_iter().map(|(i, _)| i).collect()
        }
        assert_eq!(ids(h.context_entries(false)), ["hf_new", "hf_auto"], "empty space");
        let on_app = ids(h.context_entries(true));
        assert_eq!(on_app[0], "hf_open");
        assert!(on_app.contains(&"hf_new") && on_app.contains(&"hf_auto"));
        assert!(!on_app.contains(&"hf_out"), "an app at the top level is not in a folder");
    }
}
