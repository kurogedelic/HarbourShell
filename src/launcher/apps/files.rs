//! Files: a browser of the real filesystem, starting at `$HOME`, with the usual file operations.
//!
//! Enter / Right opens a folder, or a file in the app for its type (txt/md/log -> Text Editor,
//! png/jpg/bmp/gif -> Images, csv -> Sheet, wav/mp3/ogg... -> Music; anything else says why not).
//! Backspace / Left goes up, Esc closes a dialog. The menu (Alt, or right click) has Open With,
//! New Folder, Rename, Copy, Cut, Paste, Delete, Info and Go to Trash; shortcuts: Ctrl+C / X / V copy, cut,
//! paste, Ctrl+N new folder, Ctrl+I info, F2 rename, Delete key delete. Delete moves to the Trash
//! (`~/.local/share/Trash`) after a confirmation whose default is Cancel; only things already in the
//! Trash are deleted for good (with their `.trashinfo`). In the Trash the menu adds Restore (back to
//! where it came from, under a free name on a clash) and Empty Trash. Info shows name, kind, size,
//! modified date, permissions and location on a card; any key or click closes it. Nothing ever overwrites: a clash gets a free name (`a copy.txt`).
//! Hidden files are off by default (menu). Mouse: click selects, double click opens, wheel
//! scrolls, the scroll bar works.

use std::path::{Path, PathBuf};

use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome;
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::dialogs::{Dialog, DialogKind, DialogOut, ListPicker};
use crate::launcher::fileops;
use crate::launcher::widgets;
use crate::launcher::widgets::{contains, MenuItem, PointerEvent, SbPart, ScrollBar};
use crate::platform::keys::arrow_alias;
use crate::ui;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// The synthetic ".." row.
    pub parent: bool,
}

/// What a dialog is about.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ask {
    NewFolder,
    Rename(PathBuf),
    /// Move to the Trash (`false`) or delete for good (`true`, for things already in the Trash).
    Delete(PathBuf, bool),
    OpenWith(PathBuf),
    EmptyTrash,
}

/// The Info card: a title (the item's name) and label / value rows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct InfoCard {
    title: String,
    rows: Vec<(&'static str, String)>,
}

/// Info card rows for `info` (labels in English, values ready to show).
fn info_rows(info: &fileops::ItemInfo) -> Vec<(&'static str, String)> {
    use crate::launcher::i18n::{tr, tr_s};
    let size = if info.size_capped { tr_s(&format!("at least {}", format_size(info.size))) } else { format_size(info.size) };
    let size = if info.size >= 1024 { format!("{size} ({} B)", info.size) } else { size };
    let modified = info
        .modified
        .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".into());
    vec![
        ("Kind", tr(info.kind).to_string()),
        ("Size", size),
        ("Modified", modified),
        ("Permissions", format!("{} ({:o})", fileops::mode_string(info.mode), info.mode)),
        ("Location", info.path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())),
    ]
}

pub struct Files {
    modal: Option<Dialog<Ask>>,
    /// Copy (`false`) or cut (`true`) of this path, waiting for Paste.
    clip: Option<(PathBuf, bool)>,
    trash: Option<PathBuf>,
    dir: PathBuf,
    entries: Vec<Entry>,
    cursor: usize,
    scroll: usize,
    hidden: bool,
    error: Option<String>,
    hover: Option<usize>,
    sb_drag: bool,
    info: Option<InfoCard>,
}

pub fn make() -> Box<dyn LauncherApp> {
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(|| PathBuf::from("/"));
    Box::new(Files::new(home))
}

/// 1234 -> "1.2 KB". Whole bytes below 1 KB, one decimal below 10, then none.
pub fn format_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{:.0} {}", v, UNITS[u])
    }
}

/// Folders first, then names, case-insensitively.
pub fn sort_entries(v: &mut [Entry]) {
    v.sort_by(|a, b| {
        b.parent
            .cmp(&a.parent)
            .then(b.is_dir.cmp(&a.is_dir))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn read_dir(dir: &Path, hidden: bool) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| e.kind().to_string())?;
    let mut v: Vec<Entry> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                return None;
            }
            let md = std::fs::metadata(e.path()).or_else(|_| e.metadata()).ok()?;
            Some(Entry { name, is_dir: md.is_dir(), size: md.len(), parent: false })
        })
        .collect();
    if dir.parent().is_some() {
        v.push(Entry { name: "..".into(), is_dir: true, size: 0, parent: true });
    }
    sort_entries(&mut v);
    Ok(v)
}

const LIST_Y: i32 = grid::BAR_H + grid::MARGIN + grid::ROW_H;
const LIST_X: i32 = grid::MARGIN;
const LIST_W: i32 = grid::SCROLL_X - grid::GUTTER - grid::MARGIN;
const VISIBLE: usize = ((grid::SCREEN_H - grid::WINDOW_INSET - LIST_Y) / grid::ROW_H) as usize;
/// The list's box and the scroll bar run down to y 168 (1 px above the screen's last row); the
/// 5 px under the last whole row are the box's paper, not an empty band.
const LIST_BOTTOM: i32 = grid::APP_FRAME_BOTTOM + 1;

impl Files {
    pub fn new(dir: PathBuf) -> Self {
        let mut f = Files { modal: None, clip: None, trash: fileops::trash_dir(), dir, entries: vec![], cursor: 0, scroll: 0, hidden: false, error: None, hover: None, sb_drag: false, info: None };
        f.reload(None);
        f
    }

    fn reload(&mut self, select: Option<&str>) {
        match read_dir(&self.dir, self.hidden) {
            Ok(v) => {
                self.entries = v;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(e);
                if self.dir.parent().is_some() {
                    self.entries.push(Entry { name: "..".into(), is_dir: true, size: 0, parent: true });
                }
            }
        }
        self.cursor = select.and_then(|n| self.entries.iter().position(|e| e.name == n)).unwrap_or(0);
        self.scroll = 0;
        self.follow();
    }

    fn follow(&mut self) {
        self.scroll = widgets::follow_scroll(self.cursor, self.scroll, VISIBLE, self.entries.len());
    }

    fn up(&mut self) {
        if let Some(p) = self.dir.parent().map(Path::to_path_buf) {
            let from = self.dir.file_name().map(|n| n.to_string_lossy().into_owned());
            self.dir = p;
            self.reload(from.as_deref());
        }
    }

    fn open(&mut self) -> AppAction {
        let Some(e) = self.entries.get(self.cursor).cloned() else { return AppAction::None };
        if e.parent {
            self.up();
        } else if e.is_dir {
            self.dir.push(&e.name);
            self.reload(None);
        } else {
            let path = self.dir.join(&e.name);
            return match fileops::open_kind(&path) {
                Some(_) => AppAction::Open { app: None, path },
                None => Self::say(format!("{}. {}", crate::launcher::i18n::tr_s(&fileops::cannot_open_reason(&path)), crate::launcher::i18n::tr("Try Open With in the menu"))),
            };
        }
        AppAction::None
    }

    /// The selected entry and its path (not the ".." row).
    fn selected(&self) -> Option<(Entry, PathBuf)> {
        let e = self.entries.get(self.cursor)?;
        (!e.parent).then(|| (e.clone(), self.dir.join(&e.name)))
    }

    fn say(msg: impl Into<String>) -> AppAction {
        AppAction::Toast(crate::launcher::i18n::tr_s(&msg.into()))
    }

    /// "Pasted x" in English, "x: (verb)" in Japanese: a past-tense verb and a file name.
    fn note(verb: &'static str, name: &str) -> String {
        if crate::launcher::i18n::get() == crate::launcher::i18n::Lang::Ja {
            format!("{name}: {}", crate::launcher::i18n::tr(verb))
        } else {
            format!("{verb} {name}")
        }
    }

    /// Reload and put the cursor on `name`, else near where it was.
    fn refresh(&mut self, name: Option<&str>) {
        let old = self.cursor;
        self.reload(name);
        if name.is_none() || !self.entries.get(self.cursor).map_or(false, |e| Some(e.name.as_str()) == name) {
            self.cursor = old.min(self.entries.len().saturating_sub(1));
            self.follow();
        }
    }

    fn in_trash(&self, p: &Path) -> bool {
        self.trash.as_deref().map_or(false, |t| fileops::in_trash(p, t))
    }

    /// `trash/files`, where trashed items sit (and can be restored from).
    fn trash_files(&self) -> Option<PathBuf> {
        self.trash.as_ref().map(|t| t.join("files"))
    }

    /// The selection, when it sits directly in the Trash and so can be put back.
    fn restorable(&self) -> Option<PathBuf> {
        let (_, p) = self.selected()?;
        (p.parent().map(Path::to_path_buf) == self.trash_files()).then_some(p)
    }

    fn restore(&mut self) -> AppAction {
        let (Some(p), Some(t)) = (self.restorable(), self.trash.clone()) else { return AppAction::None };
        match fileops::restore(&p, &t) {
            Ok(to) => {
                if self.clip.as_ref().map_or(false, |(c, _)| *c == p) {
                    self.clip = None;
                }
                self.refresh(None);
                let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                Self::say(Self::note("Restored", &name))
            }
            Err(e) => Self::say(e),
        }
    }

    fn ask_empty_trash(&mut self) -> AppAction {
        let Some(files) = self.trash_files() else { return Self::say("No Trash folder (HOME is not set)") };
        let empty = std::fs::read_dir(&files).map_or(true, |mut rd| rd.next().is_none());
        if empty {
            return Self::say("The Trash is empty");
        }
        self.modal = Some(Dialog::confirm(vec!["Empty the Trash?".into(), "Everything in it is removed for good.".into()], "Delete", Ask::EmptyTrash));
        AppAction::None
    }

    fn show_info(&mut self) -> AppAction {
        let Some((_, p)) = self.selected() else { return AppAction::None };
        match fileops::item_info(&p) {
            Ok(i) => {
                self.info = Some(InfoCard { title: i.name.clone(), rows: info_rows(&i) });
                AppAction::None
            }
            Err(e) => Self::say(e),
        }
    }

    fn ask_new_folder(&mut self) {
        self.modal = Some(Dialog::name("New folder name:", "untitled folder", Ask::NewFolder));
    }

    fn ask_rename(&mut self) {
        if let Some((e, p)) = self.selected() {
            self.modal = Some(Dialog::name("Rename to:", &e.name, Ask::Rename(p)));
        }
    }

    fn ask_delete(&mut self) {
        let Some((e, p)) = self.selected() else { return };
        let (lines, label, forever) = if self.in_trash(&p) {
            (vec![format!("Delete \"{}\"?", e.name), "It is removed for good.".to_string()], "Delete", true)
        } else {
            (vec![format!("Move \"{}\" to the Trash?", e.name), "Get it back from Go to Trash.".to_string()], "Trash", false)
        };
        self.modal = Some(Dialog::confirm(lines, label, Ask::Delete(p, forever)));
    }

    fn ask_open_with(&mut self) {
        let Some((e, p)) = self.selected() else { return };
        if e.is_dir {
            return;
        }
        let mut rows: Vec<String> = ["Text Editor", "Images", "Sheet", "Music"].iter().map(|s| s.to_string()).collect();
        if paint_installed() {
            rows.push("Paint".into());
        }
        let title = format!("Open \"{}\" with:", e.name);
        self.modal = Some(Dialog::new(DialogKind::Pick(ListPicker::new(&title, rows).with_button("Open")), Ask::OpenWith(p)));
    }

    fn copy_or_cut(&mut self, cut: bool) -> AppAction {
        match self.selected() {
            Some((e, p)) => {
                self.clip = Some((p, cut));
                Self::say(Self::note(if cut { "Cut" } else { "Copied" }, &e.name))
            }
            None => AppAction::None,
        }
    }

    fn paste(&mut self) -> AppAction {
        let Some((src, cut)) = self.clip.clone() else { return Self::say("Nothing to paste") };
        if std::fs::symlink_metadata(&src).is_err() {
            self.clip = None;
            return Self::say("The copied item is gone");
        }
        if cut && src.parent() == Some(self.dir.as_path()) {
            return Self::say("It is already here");
        }
        let r = if cut { fileops::move_to(&src, &self.dir) } else { fileops::copy_to(&src, &self.dir) };
        match r {
            Ok(p) => {
                if cut {
                    self.clip = None;
                }
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.refresh(Some(&name));
                Self::say(Self::note(if cut { "Moved" } else { "Pasted" }, &name))
            }
            Err(e) => Self::say(e),
        }
    }

    /// The answer of the open dialog.
    fn finish(&mut self, out: DialogOut) -> AppAction {
        let Some(d) = self.modal.take() else { return AppAction::None };
        match (d.tag, out) {
            (_, DialogOut::Cancel) => {}
            (Ask::NewFolder, DialogOut::Name(n)) => match fileops::new_folder(&self.dir, &n) {
                Ok(p) => {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.refresh(Some(&name));
                    return Self::say(self.hidden_note("Created", &name));
                }
                Err(e) => {
                    self.modal = Some(Dialog::name("New folder name:", &n, Ask::NewFolder));
                    return Self::say(e);
                }
            },
            (Ask::Rename(path), DialogOut::Name(n)) => match fileops::rename(&path, &n) {
                Ok(p) => {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if self.clip.as_ref().map_or(false, |(c, _)| *c == path) {
                        self.clip = Some((p, self.clip.as_ref().map_or(false, |c| c.1)));
                    }
                    self.refresh(Some(&name));
                    return Self::say(self.hidden_note("Renamed to", &name));
                }
                Err(e) => {
                    self.modal = Some(Dialog::name("Rename to:", &n, Ask::Rename(path)));
                    return Self::say(e);
                }
            },
            (Ask::Delete(path, forever), DialogOut::Ok) => {
                let r = match (&self.trash, forever) {
                    (Some(t), true) => fileops::delete_from_trash(&path, t),
                    (None, true) => fileops::remove_any(&path).map_err(|e| e.to_string()),
                    (Some(t), false) => fileops::trash(&path, t).map(|_| ()),
                    (None, false) => Err("No Trash folder (HOME is not set)".to_string()),
                };
                return match r {
                    Ok(()) => {
                        if self.clip.as_ref().map_or(false, |(c, _)| *c == path) {
                            self.clip = None;
                        }
                        self.refresh(None);
                        Self::say(if forever { "Deleted" } else { "Moved to the Trash" })
                    }
                    Err(e) => Self::say(e),
                };
            }
            (Ask::EmptyTrash, DialogOut::Ok) => {
                let Some(t) = self.trash.clone() else { return AppAction::None };
                let r = fileops::empty_trash(&t);
                if self.clip.as_ref().map_or(false, |(c, _)| fileops::in_trash(c, &t)) {
                    self.clip = None;
                }
                // Inside a trashed folder that is now gone: back to the Trash itself.
                if self.in_trash(&self.dir) && !self.dir.is_dir() {
                    self.dir = t.join("files");
                }
                let _ = std::fs::create_dir_all(t.join("files"));
                self.refresh(None);
                return match r {
                    Ok(_) => Self::say("Emptied the Trash"),
                    Err(e) => Self::say(e),
                };
            }
            (Ask::OpenWith(path), DialogOut::Pick(i)) => {
                let mut names = vec!["Text Editor", "Images", "Sheet", "Music"];
                if paint_installed() {
                    names.push("Paint");
                }
                if let Some(n) = names.get(i) {
                    return AppAction::Open { app: Some((*n).to_string()), path };
                }
            }
            _ => {}
        }
        AppAction::None
    }

    /// "Created x", plus a hint when the new name is hidden by the current setting.
    fn hidden_note(&self, verb: &'static str, name: &str) -> String {
        let hidden = name.starts_with('.') && !self.hidden;
        let note = Self::note(verb, name);
        if hidden { format!("{note} ({})", crate::launcher::i18n::tr("hidden")) } else { note }
    }

    fn modal_key(&mut self, key: Keycode) -> AppAction {
        let out = self.modal.as_mut().and_then(|d| d.key(key));
        match out {
            Some(o) => self.finish(o),
            None => AppAction::None,
        }
    }

    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let r = Rect::new(LIST_X, LIST_Y, LIST_W as u32, (VISIBLE as i32 * grid::ROW_H) as u32);
        if !contains(r, x, y) {
            return None;
        }
        let i = self.scroll + ((y - LIST_Y) / grid::ROW_H) as usize;
        (i < self.entries.len()).then_some(i)
    }

    fn scrollbar(&self) -> ScrollBar {
        let r = Rect::new(grid::SCROLL_X, LIST_Y, grid::SCROLL_W as u32, (LIST_BOTTOM - LIST_Y) as u32);
        ScrollBar::new(r, self.entries.len(), VISIBLE, self.scroll)
    }

    fn move_cursor(&mut self, delta: isize) {
        let max = self.entries.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
        self.follow();
    }
}

impl LauncherApp for Files {
    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Up / Down", "Move the selection"),
            k("Return / Right", "Open"),
            k("Backspace / Left", "Go up"),
            k("F2", "Rename"),
            k("Ctrl+C / X / V", "Copy / cut / paste"),
            k("Ctrl+N", "New Folder..."),
            k("Ctrl+O", "Open With..."),
            k("Ctrl+I", "Info"),
        ]
    }

    fn title(&self) -> String {
        crate::launcher::i18n::app_name("files")
    }

    fn describe(&self) -> String {
        if let Some(e) = &self.error {
            return format!("Files: {} (cannot read: {e})", self.dir.display());
        }
        let mut out = format!("Files: {}\n", self.dir.display());
        for (i, e) in self.entries.iter().take(30).enumerate() {
            let name = if e.parent { "..".to_string() } else if e.is_dir { format!("{}/", e.name) } else { format!("{} ({} bytes)", e.name, e.size) };
            out.push_str(&format!("{} {}\n", if i == self.cursor { ">" } else { " " }, name));
        }
        if self.entries.len() > 30 {
            out.push_str(&format!("[{} more]\n", self.entries.len() - 30));
        }
        out.push_str("(> marks the selected row)");
        if let Some(i) = &self.info {
            out.push_str(&format!("\nInfo card for {}:", i.title));
            for (k, v) in &i.rows {
                out.push_str(&format!("\n  {k}: {v}"));
            }
        }
        if self.modal.is_some() {
            out.push_str("\nA dialog is open (Esc cancels).");
        }
        out
    }

    fn menu(&self) -> Vec<MenuItem> {
        let mut v = Vec::new();
        if let Some((e, _)) = self.selected() {
            v.push(MenuItem::new("open", if e.is_dir { "Open folder" } else { "Open" }));
            if !e.is_dir {
                v.push(MenuItem::new("with", "Open With..."));
            }
            v.push(MenuItem::new("rename", "Rename..."));
            v.push(MenuItem::new("copy", "Copy"));
            v.push(MenuItem::new("cut", "Cut"));
        }
        if self.clip.is_some() {
            v.push(MenuItem::new("paste", "Paste"));
        }
        if self.restorable().is_some() {
            v.push(MenuItem::new("restore", "Restore"));
        }
        if self.selected().is_some() {
            v.push(MenuItem::new("delete", "Delete..."));
            v.push(MenuItem::new("info", "Info..."));
        }
        if self.in_trash(&self.dir) {
            v.push(MenuItem::new("empty", "Empty Trash..."));
        }
        v.push(MenuItem::new("newdir", "New Folder..."));
        v.push(MenuItem::new("hidden", if self.hidden { "Hide hidden files" } else { "Show hidden files" }));
        v.push(MenuItem::new("home", "Go Home"));
        v.push(MenuItem::new("trash", "Go to Trash"));
        v.push(MenuItem::new("root", "Go to /"));
        v
    }

    fn context_menu(&mut self, x: i32, y: i32, _ctx: &Ctx) -> Option<Vec<MenuItem>> {
        if self.modal.is_some() || self.info.is_some() {
            return Some(Vec::new());
        }
        if let Some(i) = self.row_at(x, y) {
            self.cursor = i;
        }
        let mut v = self.menu();
        v.insert(v.len().saturating_sub(4), MenuItem::new("up", "Go up"));
        Some(v)
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        let dir = if path.is_dir() { path } else { path.parent().ok_or("No folder")? };
        self.dir = dir.to_path_buf();
        let name = if path.is_dir() { None } else { path.file_name().map(|n| n.to_string_lossy().into_owned()) };
        self.reload(name.as_deref());
        Ok(())
    }

    fn menu_action(&mut self, id: &str, _ctx: &Ctx) -> AppAction {
        match id {
            "open" => return self.open(),
            "with" => self.ask_open_with(),
            "rename" => self.ask_rename(),
            "delete" => self.ask_delete(),
            "restore" => return self.restore(),
            "empty" => return self.ask_empty_trash(),
            "info" => return self.show_info(),
            "copy" => return self.copy_or_cut(false),
            "cut" => return self.copy_or_cut(true),
            "paste" => return self.paste(),
            "newdir" => self.ask_new_folder(),
            "trash" => match &self.trash {
                Some(t) => {
                    let t = t.join("files");
                    match std::fs::create_dir_all(&t) {
                        Ok(()) => {
                            self.dir = t;
                            self.reload(None);
                        }
                        Err(e) => return Self::say(format!("Trash: {e}")),
                    }
                }
                None => return Self::say("No Trash folder (HOME is not set)"),
            },
            "up" => self.up(),
            "hidden" => {
                self.hidden = !self.hidden;
                self.reload(None);
            }
            "home" => {
                if let Some(h) = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_dir()) {
                    self.dir = h;
                    self.reload(None);
                }
            }
            "root" => {
                self.dir = PathBuf::from("/");
                self.reload(None);
            }
            _ => {}
        }
        AppAction::None
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, _ctx: &Ctx) -> AppAction {
        if self.modal.is_some() {
            return self.modal_key(key);
        }
        if self.info.is_some() {
            // Any key closes the card (Esc closes only the card, not the app).
            self.info = None;
            return AppAction::None;
        }
        if mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD) {
            match key {
                Keycode::I => return self.show_info(),
                Keycode::C => return self.copy_or_cut(false),
                Keycode::X => return self.copy_or_cut(true),
                Keycode::V => return self.paste(),
                Keycode::N => self.ask_new_folder(),
                Keycode::O => self.ask_open_with(),
                _ => {}
            }
            return AppAction::None;
        }
        match arrow_alias(key) {
            Keycode::F2 => self.ask_rename(),
            Keycode::Delete => self.ask_delete(),
            Keycode::Escape => return AppAction::Quit,
            Keycode::Up => self.move_cursor(-1),
            Keycode::Down => self.move_cursor(1),
            Keycode::PageUp => self.move_cursor(-(VISIBLE as isize)),
            Keycode::PageDown => self.move_cursor(VISIBLE as isize),
            Keycode::Home => self.move_cursor(-(self.cursor as isize)),
            Keycode::End => self.move_cursor(self.entries.len() as isize),
            Keycode::Left | Keycode::Backspace => self.up(),
            Keycode::Right | Keycode::Return | Keycode::KpEnter => return self.open(),
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

    fn pointer(&mut self, ev: PointerEvent, _ctx: &Ctx) -> AppAction {
        if let Some(d) = &mut self.modal {
            match ev {
                PointerEvent::Down { x, y, button: 0, .. } => {
                    let out = d.click(x, y);
                    if let Some(o) = out {
                        return self.finish(o);
                    }
                }
                PointerEvent::Wheel { dy, .. } => d.wheel(dy),
                _ => {}
            }
            return AppAction::None;
        }
        if self.info.is_some() {
            if matches!(ev, PointerEvent::Down { .. }) {
                self.info = None;
            }
            return AppAction::None;
        }
        match ev {
            PointerEvent::Move { x, y } => self.hover = self.row_at(x, y),
            PointerEvent::Wheel { dy, .. } => {
                let max = self.entries.len().saturating_sub(VISIBLE);
                self.scroll = if dy > 0 { self.scroll.saturating_sub(1) } else { (self.scroll + 1).min(max) };
            }
            PointerEvent::Down { x, y, button: 0, clicks } => {
                let sb = self.scrollbar();
                match sb.hit(x, y) {
                    Some(SbPart::Up) => self.scroll = self.scroll.saturating_sub(1),
                    Some(SbPart::Down) => self.scroll = (self.scroll + 1).min(self.entries.len().saturating_sub(VISIBLE)),
                    Some(SbPart::PageUp) => self.scroll = self.scroll.saturating_sub(VISIBLE),
                    Some(SbPart::PageDown) => self.scroll = (self.scroll + VISIBLE).min(self.entries.len().saturating_sub(VISIBLE)),
                    Some(SbPart::Thumb) => self.sb_drag = true,
                    None => {
                        if let Some(i) = self.row_at(x, y) {
                            self.cursor = i;
                            if clicks >= 2 {
                                return self.open();
                            }
                        }
                    }
                }
            }
            PointerEvent::Drag { y, .. } if self.sb_drag => self.scroll = self.scrollbar().offset_for_y(y),
            PointerEvent::Up { .. } => self.sb_drag = false,
            _ => {}
        }
        AppAction::None
    }

    fn draw(&mut self, c: &mut Canvas<Window>, _assets: &mut AssetStore, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let paper = tm.get_color("widget_face_active");
        let faint = crate::launcher::widgets::secondary_ink(tm);
        let line = tm.get_color("line");
        let alt = tm.get_color("surface_alt");
        let soft = tm.get_color("accent_soft");
        ui::fill_rect(c, Rect::new(0, grid::BAR_H, grid::SCREEN_W as u32, (grid::SCREEN_H - grid::BAR_H) as u32), tm.get_color("home_bg"));

        // Path bar, boxed, with a rule under it.
        let bar = Rect::new(LIST_X, grid::BAR_H + grid::MARGIN, LIST_W as u32, grid::ROW_H as u32);
        ui::fill_rect(c, bar, alt);
        let mut path_w = LIST_W - 12;
        if let Some((p, cut)) = &self.clip {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let tag = chrome::fit_text(fs, &format!("{}: {name}", crate::launcher::i18n::tr(if *cut { "Cut" } else { "Copy" })), 96);
            let w = fs.measure_text(&tag);
            fs.draw_text(c, bar.right() - w - 6, bar.y() + 1, &tag, faint);
            path_w -= w + 8;
        }
        let shown = chrome::fit_text(fs, &self.dir.to_string_lossy(), path_w);
        fs.draw_text(c, bar.x() + 6, bar.y() + 1, &shown, ink);
        widgets::draw_row_rule(c, tm, bar);
        ui::draw_rect(c, bar, line);

        let area = Rect::new(LIST_X, LIST_Y, LIST_W as u32, (LIST_BOTTOM - LIST_Y) as u32);
        ui::fill_rect(c, area, paper);
        if self.entries.iter().all(|e| e.parent) {
            // Nothing but ".." (or nothing at all): say so on the row below it.
            let msg = self.error.as_deref().map(|e| format!("Cannot read: {e}")).unwrap_or_else(|| "Empty folder".into());
            let msg = crate::launcher::i18n::tr_s(&msg);
            let row = if self.entries.is_empty() { 0 } else { 1 };
            fs.draw_text(c, area.x() + 20, widgets::text_origin(area.y() + row * grid::ROW_H, grid::ROW_H), &msg, faint);
        }
        for (n, e) in self.entries.iter().enumerate().skip(self.scroll).take(VISIBLE) {
            let y = LIST_Y + (n - self.scroll) as i32 * grid::ROW_H;
            let row = Rect::new(LIST_X, y, LIST_W as u32, grid::ROW_H as u32);
            let sel = n == self.cursor;
            if sel {
                widgets::draw_row_plate(c, tm, row);
            } else if Some(n) == self.hover {
                ui::fill_rect(c, row, soft);
            }
            let fg = if sel { widgets::selection(tm).fg } else if e.name.starts_with('.') && !e.parent { faint } else { ink };
            // Folder marker: a filled square for folders, none for files.
            if e.is_dir {
                ui::fill_rect(c, Rect::new(row.x() + 6, row.y() + 5, 8, 6), fg);
            }
            let size = if e.is_dir { String::new() } else { format_size(e.size) };
            let sw = fs.measure_text(&size);
            let name = chrome::fit_text(fs, &e.name, LIST_W - 22 - sw - 12);
            let ty = widgets::text_origin(row.y(), row.height() as i32);
            fs.draw_text(c, row.x() + 20, ty, &name, fg);
            fs.draw_text(c, row.right() - sw - 6, ty, &size, if sel { fg } else { faint });
            widgets::draw_row_rule(c, tm, row);
        }
        ui::draw_rect(c, area, line);
        self.scrollbar().draw(c, tm, if self.sb_drag { Some(SbPart::Thumb) } else { None });
        if let Some(i) = &self.info {
            draw_info(c, ctx, i);
        }
        if let Some(d) = &self.modal {
            d.draw(c, fs, tm);
        }
    }
}

/// The Info card: a panel with the name as its title and label / value rows; the location wraps
/// onto a second row when it is long.
fn draw_info(c: &mut Canvas<Window>, ctx: &Ctx, card: &InfoCard) {
    use crate::launcher::i18n::tr;
    let (tm, fs) = (ctx.tm, ctx.fs);
    let r = Rect::new(12, grid::BAR_H + 8, (grid::SCREEN_W - 24) as u32, 136);
    widgets::draw_panel(c, tm, r);
    let ink = tm.get_color("foreground");
    let faint = widgets::secondary_ink(tm);
    let inner = r.width() as i32 - 20;
    fs.draw_display_text(c, r.x() + 10, r.y() + 7, &chrome::fit_display_text(fs, &card.title, inner), ink);
    let label_w = card.rows.iter().map(|(k, _)| fs.measure_text(tr(k))).max().unwrap_or(0) + 8;
    let vx = r.x() + 10 + label_w;
    let vw = r.right() - 10 - vx;
    let mut y = r.y() + 28;
    for (k, v) in &card.rows {
        fs.draw_text(c, r.x() + 10, y, tr(k), faint);
        let lines = wrap_chars(fs, v, vw, 3);
        for l in &lines {
            fs.draw_text(c, vx, y, l, ink);
            y += 14;
        }
        if lines.is_empty() {
            y += 14;
        }
    }
}

/// Break `s` into at most `max` lines of width `w` (by character, for paths); the last line is
/// shortened with an ellipsis when it still does not fit.
fn wrap_chars(fs: &crate::font::FontSystem, s: &str, w: i32, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        cur.push(chars[i]);
        if fs.measure_text(&cur) > w && cur.chars().count() > 1 {
            cur.pop();
            if out.len() + 1 == max {
                let rest: String = chars[i - cur.chars().count()..].iter().collect();
                out.push(chrome::fit_text(fs, &rest, w));
                return out;
            }
            out.push(std::mem::take(&mut cur));
            continue;
        }
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// "Edit in Paint" / "Open With > Paint" only exist when the Paint app is registered.
pub fn paint_installed() -> bool {
    crate::launcher::apps::REGISTRY.iter().any(|d| d.name == "Paint")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, dir: bool) -> Entry {
        Entry { name: name.into(), is_dir: dir, size: 0, parent: false }
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(10 * 1024), "10 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size(3 * 1024u64.pow(3)), "3.0 GB");
    }

    #[test]
    fn folders_sort_before_files_case_insensitively() {
        let mut v = vec![e("b.txt", false), e("Zed", true), e("a.txt", false), e("apple", true)];
        v.push(Entry { name: "..".into(), is_dir: true, size: 0, parent: true });
        sort_entries(&mut v);
        let names: Vec<_> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["..", "apple", "Zed", "a.txt", "b.txt"]);
    }

    #[test]
    fn browses_a_real_directory() {
        let base = std::env::temp_dir().join(format!("harbour_files_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("a.txt"), b"hello").unwrap();
        std::fs::write(base.join(".hid"), b"x").unwrap();
        let mut f = Files::new(base.clone());
        let names: Vec<_> = f.entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["..", "sub", "a.txt"], "hidden files are not listed");
        assert_eq!(f.entries[2].size, 5);
        f.cursor = 1;
        f.open();
        assert_eq!(f.dir, base.join("sub"));
        f.up();
        assert_eq!(f.dir, base);
        assert_eq!(f.entries[f.cursor].name, "sub", "going up re-selects the folder we left");
        f.hidden = true;
        f.reload(None);
        assert!(f.entries.iter().any(|e| e.name == ".hid"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cursor_scrolls_into_view() {
        let mut f = Files::new(PathBuf::from("/nonexistent_dir_for_test"));
        f.entries = (0..30).map(|i| e(&format!("f{i}"), false)).collect();
        f.cursor = 29;
        f.follow();
        assert_eq!(f.scroll, 30 - VISIBLE);
        f.cursor = 0;
        f.follow();
        assert_eq!(f.scroll, 0);
    }

    fn sandbox(tag: &str) -> (PathBuf, Files) {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("harbour_files_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("work")).unwrap();
        let mut f = Files::new(base.join("work"));
        f.trash = Some(base.join("Trash"));
        (base, f)
    }

    fn select(f: &mut Files, name: &str) {
        f.cursor = f.entries.iter().position(|e| e.name == name).unwrap_or_else(|| panic!("{name} not listed"));
    }

    fn type_name(f: &mut Files, name: &str) {
        f.modal.as_mut().unwrap().text(name);
    }

    #[test]
    fn new_folder_creates_selects_and_refuses_bad_names() {
        let (base, mut f) = sandbox("newdir");
        f.ask_new_folder();
        type_name(&mut f, "Docs");
        let a = f.modal_key(Keycode::Return);
        assert!(matches!(a, AppAction::Toast(t) if t.contains("Created Docs")));
        assert!(base.join("work/Docs").is_dir());
        assert_eq!(f.entries[f.cursor].name, "Docs");
        // A clash keeps the prompt open with the typed name.
        f.ask_new_folder();
        type_name(&mut f, "Docs");
        let a = f.modal_key(Keycode::Return);
        assert!(matches!(a, AppAction::Toast(t) if t.contains("already exists")));
        assert!(f.modal.is_some(), "the prompt comes back so the name can be fixed");
        f.modal_key(Keycode::Escape);
        // A slash is refused.
        f.ask_new_folder();
        type_name(&mut f, "a/b");
        let a = f.modal_key(Keycode::Return);
        assert!(matches!(a, AppAction::Toast(_)));
        assert!(!base.join("work/a").exists());
        // A hidden name says so.
        f.modal = None;
        f.ask_new_folder();
        type_name(&mut f, ".cache");
        let a = f.modal_key(Keycode::Return);
        assert!(matches!(a, AppAction::Toast(t) if t.contains("hidden")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn rename_keeps_the_selection_and_japanese_names_work() {
        let (base, mut f) = sandbox("rename");
        std::fs::write(base.join("work/a.txt"), "A").unwrap();
        std::fs::write(base.join("work/b.txt"), "B").unwrap();
        f.reload(None);
        select(&mut f, "a.txt");
        f.ask_rename();
        type_name(&mut f, "日本語のメモ.txt");
        f.modal_key(Keycode::Return);
        assert!(base.join("work/日本語のメモ.txt").exists() && !base.join("work/a.txt").exists());
        assert_eq!(f.entries[f.cursor].name, "日本語のメモ.txt");
        // Renaming onto an existing name never overwrites.
        f.ask_rename();
        type_name(&mut f, "b.txt");
        let a = f.modal_key(Keycode::Return);
        assert!(matches!(a, AppAction::Toast(t) if t.contains("already exists")));
        assert_eq!(std::fs::read_to_string(base.join("work/b.txt")).unwrap(), "B");
        f.modal_key(Keycode::Escape);
        // The ".." row cannot be renamed or deleted.
        f.cursor = 0;
        assert!(f.entries[0].parent);
        f.ask_rename();
        f.ask_delete();
        assert!(f.modal.is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delete_asks_first_defaults_to_cancel_and_goes_to_the_trash() {
        let (base, mut f) = sandbox("delete");
        std::fs::write(base.join("work/a.txt"), "A").unwrap();
        f.reload(None);
        select(&mut f, "a.txt");
        f.ask_delete();
        // Enter alone cancels.
        f.modal_key(Keycode::Return);
        assert!(base.join("work/a.txt").exists(), "Enter on the default button keeps the file");
        f.ask_delete();
        f.modal_key(Keycode::Escape);
        assert!(base.join("work/a.txt").exists());
        f.ask_delete();
        f.modal_key(Keycode::Right);
        let a = f.modal_key(Keycode::Return);
        assert!(matches!(a, AppAction::Toast(t) if t.contains("Trash")));
        assert!(!base.join("work/a.txt").exists());
        assert_eq!(std::fs::read_to_string(base.join("Trash/files/a.txt")).unwrap(), "A", "recoverable");
        // In the Trash the second delete is for good.
        f.dir = base.join("Trash/files");
        f.reload(None);
        select(&mut f, "a.txt");
        f.ask_delete();
        assert!(matches!(&f.modal.as_ref().unwrap().tag, Ask::Delete(_, true)));
        f.modal_key(Keycode::Right);
        f.modal_key(Keycode::Return);
        assert!(!base.join("Trash/files/a.txt").exists());
        assert!(!base.join("Trash/info/a.txt.trashinfo").exists(), "its .trashinfo goes too");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_brings_a_trashed_item_back() {
        let fs = crate::font::FontSystem::new();
        let tm = crate::theme::ThemeManager::new(crate::theme::Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = crate::system::mock::MockBackend::new();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let (base, mut f) = sandbox("restore");
        std::fs::write(base.join("work/a.txt"), "A").unwrap();
        f.reload(None);
        select(&mut f, "a.txt");
        assert!(f.restorable().is_none(), "nothing to restore outside the Trash");
        assert!(!f.menu().iter().any(|m| m.id == "restore" || m.id == "empty"));
        f.ask_delete();
        f.modal_key(Keycode::Right);
        f.modal_key(Keycode::Return);
        // Something new took the name meanwhile.
        std::fs::write(base.join("work/a.txt"), "NEW").unwrap();
        f.menu_action("trash", &ctx);
        assert_eq!(f.dir, base.join("Trash/files"));
        select(&mut f, "a.txt");
        let ids: Vec<_> = f.menu().iter().map(|m| m.id).collect();
        assert!(ids.contains(&"restore") && ids.contains(&"empty"));
        let a = f.restore();
        assert!(matches!(a, AppAction::Toast(t) if t.contains("a copy.txt")));
        assert_eq!(std::fs::read_to_string(base.join("work/a.txt")).unwrap(), "NEW", "never overwrites");
        assert_eq!(std::fs::read_to_string(base.join("work/a copy.txt")).unwrap(), "A");
        assert!(!base.join("Trash/info/a.txt.trashinfo").exists());
        assert!(f.entries.iter().all(|e| e.parent), "the Trash is empty again");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn empty_trash_asks_then_removes_everything() {
        let (base, mut f) = sandbox("emptytrash");
        let t = base.join("Trash");
        assert!(matches!(f.ask_empty_trash(), AppAction::Toast(m) if m.contains("empty")));
        assert!(f.modal.is_none());
        for n in ["a", "b"] {
            std::fs::write(base.join("work").join(n), n).unwrap();
            fileops::trash(&base.join("work").join(n), &t).unwrap();
        }
        f.dir = t.join("files");
        f.reload(None);
        f.ask_empty_trash();
        f.modal_key(Keycode::Return);
        assert_eq!(std::fs::read_dir(t.join("files")).unwrap().count(), 2, "Enter alone cancels");
        f.ask_empty_trash();
        f.modal_key(Keycode::Right);
        let a = f.modal_key(Keycode::Return);
        assert!(matches!(a, AppAction::Toast(m) if m.contains("Emptied")));
        assert_eq!(std::fs::read_dir(t.join("files")).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(t.join("info")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn info_card_shows_the_facts_and_any_key_closes_it() {
        let fs = crate::font::FontSystem::new();
        let tm = crate::theme::ThemeManager::new(crate::theme::Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = crate::system::mock::MockBackend::new();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let (base, mut f) = sandbox("info");
        std::fs::create_dir_all(base.join("work/d")).unwrap();
        std::fs::write(base.join("work/d/x"), vec![0u8; 2048]).unwrap();
        f.reload(None);
        select(&mut f, "d");
        f.show_info();
        let card = f.info.clone().expect("card");
        assert_eq!(card.title, "d");
        let row = |k: &str| card.rows.iter().find(|r| r.0 == k).map(|r| r.1.clone()).unwrap();
        assert_eq!(row("Kind"), "Folder");
        assert_eq!(row("Size"), "2.0 KB (2048 B)");
        assert_eq!(row("Location"), base.join("work").to_string_lossy());
        assert!(row("Permissions").starts_with("rwx"));
        assert!(f.describe().contains("Info card"));
        f.key(Keycode::Escape, Mod::NOMOD, false, &ctx);
        assert!(f.info.is_none(), "Esc closes the card");
        f.cursor = 0;
        f.show_info();
        assert!(f.info.is_none(), "no card for the .. row");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn copy_cut_paste_never_overwrite() {
        let (base, mut f) = sandbox("paste");
        std::fs::create_dir_all(base.join("work/dst")).unwrap();
        std::fs::write(base.join("work/a.txt"), "A").unwrap();
        f.reload(None);
        select(&mut f, "a.txt");
        f.copy_or_cut(false);
        assert!(matches!(f.paste(), AppAction::Toast(t) if t.contains("a copy.txt")), "pasting next to the original renames");
        assert_eq!(std::fs::read_to_string(base.join("work/a copy.txt")).unwrap(), "A");
        // Cut, go into dst, paste: moved.
        select(&mut f, "a copy.txt");
        f.copy_or_cut(true);
        assert!(matches!(f.paste(), AppAction::Toast(t) if t.contains("already here")));
        select(&mut f, "dst");
        f.open();
        f.paste();
        assert!(base.join("work/dst/a copy.txt").exists() && !base.join("work/a copy.txt").exists());
        assert!(f.clip.is_none(), "a cut is used up by its paste");
        // A copy pasted over a same-named file keeps both.
        std::fs::write(base.join("work/dst/a.txt"), "OTHER").unwrap();
        f.dir = base.join("work");
        f.reload(None);
        select(&mut f, "a.txt");
        f.copy_or_cut(false);
        f.dir = base.join("work/dst");
        f.reload(None);
        f.paste();
        assert_eq!(std::fs::read_to_string(base.join("work/dst/a.txt")).unwrap(), "OTHER");
        assert_eq!(std::fs::read_to_string(base.join("work/dst/a copy 2.txt")).unwrap(), "A");
        // The source vanished: a toast, not a crash.
        std::fs::remove_file(base.join("work/a.txt")).unwrap();
        assert!(matches!(f.paste(), AppAction::Toast(t) if t.contains("gone")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn enter_opens_by_type_and_explains_what_it_cannot_open() {
        let (base, mut f) = sandbox("open");
        for n in ["n.txt", "p.png", "t.csv", "s.mp3", "x.bin", "README"] {
            std::fs::write(base.join("work").join(n), "x").unwrap();
        }
        f.reload(None);
        for n in ["n.txt", "p.png", "t.csv", "s.mp3"] {
            select(&mut f, n);
            assert_eq!(f.open(), AppAction::Open { app: None, path: base.join("work").join(n) }, "{n}");
        }
        select(&mut f, "x.bin");
        assert!(matches!(f.open(), AppAction::Toast(t) if t.contains(".bin") && t.contains("Open With")));
        select(&mut f, "README");
        assert!(matches!(f.open(), AppAction::Toast(t) if t.contains("extension")));
        // Open With offers the apps and returns the chosen one.
        select(&mut f, "x.bin");
        f.ask_open_with();
        f.modal_key(Keycode::Down); // Images
        let a = f.modal_key(Keycode::Return);
        assert_eq!(a, AppAction::Open { app: Some("Images".into()), path: base.join("work/x.bin") });
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_menu_follows_the_selection_and_the_clipboard() {
        let (base, mut f) = sandbox("menu");
        std::fs::write(base.join("work/a.txt"), "A").unwrap();
        f.reload(None);
        let ids = |f: &Files| f.menu().iter().map(|m| m.id).collect::<Vec<_>>();
        f.cursor = 0; // ".."
        assert!(!ids(&f).contains(&"delete") && !ids(&f).contains(&"rename") && ids(&f).contains(&"newdir"));
        select(&mut f, "a.txt");
        assert!(ids(&f).contains(&"with") && ids(&f).contains(&"delete") && !ids(&f).contains(&"paste"));
        f.copy_or_cut(false);
        assert!(ids(&f).contains(&"paste"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_empty_folder_lists_only_the_parent_row_and_a_long_name_is_listed_whole() {
        let (base, mut f) = sandbox("empty");
        assert_eq!(f.entries.len(), 1);
        assert!(f.entries[0].parent);
        assert!(f.selected().is_none());
        // Linux file names are at most 255 bytes; 7 x 10 Japanese characters is 210 bytes plus the extension.
        let long = format!("{}.txt", "とても長いファイル名".repeat(7));
        std::fs::write(base.join("work").join(&long), "x").unwrap();
        f.reload(None);
        assert_eq!(f.entries[1].name, long);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn help_rows_are_filled_in() {
        crate::launcher::help::check_app_rows("Files", &Files::new(std::env::temp_dir()).help());
    }
}
