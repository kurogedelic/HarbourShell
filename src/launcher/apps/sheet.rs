//! Sheet: a small spreadsheet, 8 columns (A-H) x 99 rows.
//!
//! Cells hold numbers, text or formulas: `=A1+B2*3`, `=SUM(A1:A5)`, `AVG`,
//! `MIN`, `MAX`, `COUNT`, parentheses and unary minus. A cycle or any other
//! problem shows `#ERR`. The sheet starts from `~/Documents/sheet.csv`.
//!
//! Files (menu): New... (asks for a name, never replaces a file), Open... (csv files), Save, Save As....
//! The menu strip shows the file name, with * while there are unsaved changes; leaving (menu Quit,
//! holding Esc), New and Open ask "Save changes?" first. Ctrl+S saves, Ctrl+N new, Ctrl+O open.
//!
//! Navigation: arrows (F/X/Z/C too), Enter edits the cell, typing a digit or `=`
//! starts an edit, Backspace/Delete clears, Ctrl+S saves, Esc quits. While
//! editing: Enter commits (and moves down), Esc cancels, arrows move the caret.
//! Tab is left to the host: Ctrl+Tab opens the app menu in text-entry mode.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome;
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::dialogs::{CardChoice, Dialog, DialogKind, DialogOut, SavePanel, UnsavedCard};
use crate::launcher::fileops;
use crate::launcher::lineedit::LineEdit;
use crate::launcher::widgets::{self, contains, MenuItem, PointerEvent};
use crate::platform::keys::arrow_alias;
use crate::ui;

pub const COLS: usize = 8;
pub const ROWS: usize = 99;

// ------------------------------------------------------------ references

pub fn col_name(c: usize) -> char {
    (b'A' + c as u8) as char
}

pub fn cell_name(col: usize, row: usize) -> String {
    format!("{}{}", col_name(col), row + 1)
}

/// "B12" -> (col 1, row 11).
pub fn parse_ref(s: &str) -> Option<(usize, usize)> {
    let mut ch = s.chars();
    let c = ch.next()?.to_ascii_uppercase();
    if !('A'..='H').contains(&c) {
        return None;
    }
    let rest: String = ch.collect();
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let r: usize = rest.parse().ok()?;
    (1..=ROWS).contains(&r).then(|| ((c as u8 - b'A') as usize, r - 1))
}

// ------------------------------------------------------------ evaluation

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Empty,
    Num(f64),
    Text(String),
}

type Res<T> = Result<T, ()>;

pub struct Sheet {
    pub cells: Vec<String>,
}

struct Eval<'a> {
    sheet: &'a Sheet,
    memo: HashMap<usize, Res<Val>>,
    stack: Vec<usize>,
}

impl Eval<'_> {
    fn value(&mut self, idx: usize) -> Res<Val> {
        if let Some(v) = self.memo.get(&idx) {
            return v.clone();
        }
        if self.stack.contains(&idx) {
            return Err(()); // cycle
        }
        self.stack.push(idx);
        let raw = self.sheet.cells[idx].trim().to_string();
        let v = if raw.is_empty() {
            Ok(Val::Empty)
        } else if let Some(f) = raw.strip_prefix('=') {
            self.formula(f).map(Val::Num)
        } else if let Ok(n) = raw.parse::<f64>() {
            if n.is_finite() { Ok(Val::Num(n)) } else { Ok(Val::Text(raw)) }
        } else {
            Ok(Val::Text(raw))
        };
        self.stack.pop();
        self.memo.insert(idx, v.clone());
        v
    }

    fn formula(&mut self, f: &str) -> Res<f64> {
        let mut p = Parser { s: f.chars().collect(), i: 0, ev: self };
        let v = p.expr()?;
        p.ws();
        if p.i != p.s.len() || !v.is_finite() {
            return Err(());
        }
        Ok(v)
    }

    fn number(&mut self, idx: usize) -> Res<f64> {
        match self.value(idx)? {
            Val::Empty => Ok(0.0),
            Val::Num(n) => Ok(n),
            Val::Text(_) => Err(()),
        }
    }
}

struct Parser<'a, 'b> {
    s: Vec<char>,
    i: usize,
    ev: &'b mut Eval<'a>,
}

impl Parser<'_, '_> {
    fn ws(&mut self) {
        while self.s.get(self.i).map_or(false, |c| c.is_whitespace()) {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<char> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Res<f64> {
        let mut v = self.term()?;
        loop {
            if self.eat('+') {
                v += self.term()?;
            } else if self.eat('-') {
                v -= self.term()?;
            } else {
                return Ok(v);
            }
        }
    }

    fn term(&mut self) -> Res<f64> {
        let mut v = self.unary()?;
        loop {
            if self.eat('*') {
                v *= self.unary()?;
            } else if self.eat('/') {
                let d = self.unary()?;
                if d == 0.0 {
                    return Err(());
                }
                v /= d;
            } else {
                return Ok(v);
            }
        }
    }

    fn unary(&mut self) -> Res<f64> {
        if self.eat('-') {
            Ok(-self.unary()?)
        } else if self.eat('+') {
            self.unary()
        } else {
            self.primary()
        }
    }

    fn word(&mut self) -> String {
        let st = self.i;
        while self.s.get(self.i).map_or(false, |c| c.is_ascii_alphanumeric()) {
            self.i += 1;
        }
        self.s[st..self.i].iter().collect()
    }

    fn primary(&mut self) -> Res<f64> {
        match self.peek().ok_or(())? {
            '(' => {
                self.i += 1;
                let v = self.expr()?;
                if self.eat(')') { Ok(v) } else { Err(()) }
            }
            c if c.is_ascii_digit() || c == '.' => {
                let st = self.i;
                while self.s.get(self.i).map_or(false, |c| c.is_ascii_digit() || *c == '.') {
                    self.i += 1;
                }
                self.s[st..self.i].iter().collect::<String>().parse().map_err(|_| ())
            }
            c if c.is_ascii_alphabetic() => {
                let w = self.word();
                if self.eat('(') {
                    self.function(&w.to_ascii_uppercase())
                } else {
                    let (c, r) = parse_ref(&w).ok_or(())?;
                    self.ev.number(r * COLS + c)
                }
            }
            _ => Err(()),
        }
    }

    fn function(&mut self, name: &str) -> Res<f64> {
        let mut nums: Vec<f64> = Vec::new();
        if !self.eat(')') {
            loop {
                self.arg(&mut nums)?;
                if self.eat(',') {
                    continue;
                }
                if self.eat(')') {
                    break;
                }
                return Err(());
            }
        }
        match name {
            "SUM" => Ok(nums.iter().sum()),
            "COUNT" => Ok(nums.len() as f64),
            "AVG" | "AVERAGE" if !nums.is_empty() => Ok(nums.iter().sum::<f64>() / nums.len() as f64),
            "MIN" if !nums.is_empty() => Ok(nums.iter().cloned().fold(f64::INFINITY, f64::min)),
            "MAX" if !nums.is_empty() => Ok(nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
            _ => Err(()),
        }
    }

    /// One function argument: a range (A1:B5, empty and text cells skipped) or an expression.
    fn arg(&mut self, out: &mut Vec<f64>) -> Res<()> {
        self.ws();
        let save = self.i;
        let a = self.word();
        if let Some(from) = parse_ref(&a) {
            if self.eat(':') {
                let b = self.word();
                let to = parse_ref(&b).ok_or(())?;
                let (c0, c1) = (from.0.min(to.0), from.0.max(to.0));
                let (r0, r1) = (from.1.min(to.1), from.1.max(to.1));
                for r in r0..=r1 {
                    for c in c0..=c1 {
                        if let Val::Num(n) = self.ev.value(r * COLS + c)? {
                            out.push(n);
                        }
                    }
                }
                return Ok(());
            }
        }
        self.i = save;
        out.push(self.expr()?);
        Ok(())
    }
}

pub fn format_num(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    let s = format!("{n:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

impl Sheet {
    pub fn new() -> Self {
        Sheet { cells: vec![String::new(); COLS * ROWS] }
    }

    pub fn set(&mut self, name: &str, text: &str) {
        if let Some((c, r)) = parse_ref(name) {
            self.cells[r * COLS + c] = text.to_string();
        }
    }

    /// What every cell shows (numbers formatted, `#ERR` for errors).
    pub fn display_all(&self) -> Vec<(String, bool)> {
        let mut ev = Eval { sheet: self, memo: HashMap::new(), stack: Vec::new() };
        (0..COLS * ROWS)
            .map(|i| match ev.value(i) {
                Ok(Val::Empty) => (String::new(), false),
                Ok(Val::Num(n)) => (format_num(n), true),
                Ok(Val::Text(t)) => (t, false),
                Err(()) => ("#ERR".to_string(), false),
            })
            .collect()
    }

    pub fn display(&self, name: &str) -> String {
        parse_ref(name).map(|(c, r)| self.display_all()[r * COLS + c].0.clone()).unwrap_or_default()
    }

    pub fn to_csv(&self) -> String {
        let last = self.cells.iter().rposition(|c| !c.is_empty()).map(|i| i / COLS + 1).unwrap_or(0);
        let mut out = String::new();
        for r in 0..last {
            let row: Vec<String> = (0..COLS)
                .map(|c| {
                    let f = &self.cells[r * COLS + c];
                    if f.contains([',', '"', '\n', '\r']) { format!("\"{}\"", f.replace('"', "\"\"")) } else { f.clone() }
                })
                .collect();
            out.push_str(row.join(",").trim_end_matches(','));
            out.push('\n');
        }
        out
    }

    pub fn from_csv(text: &str) -> Self {
        let mut s = Sheet::new();
        let (mut row, mut col) = (0usize, 0usize);
        let mut field = String::new();
        let (mut quoted, mut was_quoted) = (false, false);
        let mut chars = text.chars().peekable();
        let put = |s: &mut Sheet, row: usize, col: usize, f: &mut String| {
            if row < ROWS && col < COLS {
                s.cells[row * COLS + col] = std::mem::take(f);
            } else {
                f.clear();
            }
        };
        while let Some(ch) = chars.next() {
            match (ch, quoted) {
                ('"', true) if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                ('"', true) => quoted = false,
                ('"', false) if field.is_empty() => {
                    quoted = true;
                    was_quoted = true;
                }
                (',', false) => {
                    put(&mut s, row, col, &mut field);
                    col += 1;
                    was_quoted = false;
                }
                ('\r', false) => {}
                ('\n', false) => {
                    put(&mut s, row, col, &mut field);
                    row += 1;
                    col = 0;
                    was_quoted = false;
                }
                (c, _) => field.push(c),
            }
        }
        if !field.is_empty() || col > 0 || was_quoted {
            put(&mut s, row, col, &mut field);
        }
        s
    }
}

impl Default for Sheet {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------- app

pub struct SheetApp {
    sheet: Sheet,
    shown: Vec<(String, bool)>,
    col: usize,
    row: usize,
    scroll_col: usize,
    scroll_row: usize,
    edit: Option<LineEdit>,
    dirty: bool,
    path: PathBuf,
    modal: Option<Dialog<Ask>>,
    /// What to do once the unsaved changes are dealt with.
    then: Next,
}

/// What a dialog is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    SaveAs,
    NewFile,
    OpenFile,
    Unsaved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    Quit,
    New,
    Open,
}

pub fn default_path() -> PathBuf {
    fileops::user_dir(fileops::UserDir::Documents).join("sheet.csv")
}

pub fn make() -> Box<dyn LauncherApp> {
    Box::new(SheetApp::open(default_path()))
}

const BAR_Y: i32 = grid::BAR_H + grid::MARGIN;
const HEAD_Y: i32 = BAR_Y + grid::ROW_H;
const BODY_Y: i32 = HEAD_Y + grid::ROW_H;
const ROW_HEAD_W: i32 = 24;
const CELL_W: i32 = 56;
const X0: i32 = grid::MARGIN;
const VIS_ROWS: usize = ((grid::SCREEN_H - grid::WINDOW_INSET - BODY_Y) / grid::ROW_H) as usize;
/// The grid's frame runs down to y 168 (1 px above the screen's last row): the 5 px under the last
/// whole row continue the grid as the frame's edge (paper, column lines), never an empty band.
const GRID_BOTTOM: i32 = grid::APP_FRAME_BOTTOM + 1;
const VIS_COLS: usize = ((grid::SCROLL_X + grid::SCROLL_W - grid::MARGIN - X0 - ROW_HEAD_W) / CELL_W) as usize;

impl SheetApp {
    pub fn open(path: PathBuf) -> Self {
        let sheet = std::fs::read_to_string(&path).map(|t| Sheet::from_csv(&t)).unwrap_or_default();
        let mut a = SheetApp { sheet, shown: vec![], col: 0, row: 0, scroll_col: 0, scroll_row: 0, edit: None, dirty: false, path, modal: None, then: Next::Quit };
        a.refresh();
        a
    }

    fn refresh(&mut self) {
        self.shown = self.sheet.display_all();
    }

    fn idx(&self) -> usize {
        self.row * COLS + self.col
    }

    fn follow(&mut self) {
        self.scroll_row = self.scroll_row.min(self.row).max((self.row + 1).saturating_sub(VIS_ROWS));
        self.scroll_col = self.scroll_col.min(self.col).max((self.col + 1).saturating_sub(VIS_COLS));
    }

    fn go(&mut self, dc: isize, dr: isize) {
        self.col = (self.col as isize + dc).clamp(0, COLS as isize - 1) as usize;
        self.row = (self.row as isize + dr).clamp(0, ROWS as isize - 1) as usize;
        self.follow();
    }

    fn begin_edit(&mut self, initial: Option<&str>) {
        let t = initial.map(str::to_string).unwrap_or_else(|| self.sheet.cells[self.idx()].clone());
        self.edit = Some(LineEdit::new(&t));
    }

    fn commit(&mut self) {
        if let Some(LineEdit { text: t, .. }) = self.edit.take() {
            let i = self.idx();
            if self.sheet.cells[i] != t {
                self.sheet.cells[i] = t;
                self.dirty = true;
                self.refresh();
            }
        }
    }

    fn clear(&mut self) {
        let i = self.idx();
        if !self.sheet.cells[i].is_empty() {
            self.sheet.cells[i].clear();
            self.dirty = true;
            self.refresh();
        }
    }

    pub fn save(&mut self) -> AppAction {
        if let Some(d) = self.path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        match std::fs::write(&self.path, self.sheet.to_csv()) {
            Ok(()) => {
                self.dirty = false;
                AppAction::Toast(crate::launcher::i18n::tr_s(&format!("Saved {}", self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())))
            }
            Err(e) => AppAction::Toast(crate::launcher::i18n::tr_s(&format!("Save failed: {e}"))),
        }
    }

    fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "sheet.csv".into())
    }

    fn home() -> PathBuf {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
    }

    fn folder(&self) -> PathBuf {
        self.path.parent().map(PathBuf::from).unwrap_or_else(Self::home)
    }

    /// Replace the sheet with the contents of `path` (an empty sheet for an empty file).
    fn load(&mut self, path: PathBuf) -> Result<(), String> {
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        self.sheet = Sheet::from_csv(&text);
        self.path = path;
        self.col = 0;
        self.row = 0;
        self.scroll_col = 0;
        self.scroll_row = 0;
        self.edit = None;
        self.dirty = false;
        self.refresh();
        Ok(())
    }

    /// Go on with New / Open / Quit once the changes are safe.
    fn proceed(&mut self, next: Next) -> AppAction {
        match next {
            Next::Quit => return AppAction::Quit,
            Next::New => {
                let p = SavePanel::new(&self.folder(), "untitled.csv", &Self::home()).titled("New sheet:", "Create");
                self.modal = Some(Dialog::new(DialogKind::Panel(p), Ask::NewFile));
            }
            Next::Open => {
                let p = SavePanel::open(&self.folder(), &Self::home(), &fileops::SHEET_EXT);
                self.modal = Some(Dialog::new(DialogKind::Panel(p), Ask::OpenFile));
            }
        }
        AppAction::None
    }

    /// Run `next` now, or after asking "Save changes?" when there is something to lose.
    fn guarded(&mut self, next: Next) -> AppAction {
        self.commit();
        if self.dirty {
            self.then = next;
            self.modal = Some(Dialog::new(DialogKind::Card(UnsavedCard::new(&self.file_name())), Ask::Unsaved));
            AppAction::None
        } else {
            self.proceed(next)
        }
    }

    fn ask_save_as(&mut self) {
        self.commit();
        let p = SavePanel::new(&self.folder(), &self.file_name(), &Self::home());
        self.modal = Some(Dialog::new(DialogKind::Panel(p), Ask::SaveAs));
    }

    fn finish(&mut self, out: DialogOut) -> AppAction {
        let Some(d) = self.modal.take() else { return AppAction::None };
        match (d.tag, out) {
            (Ask::Unsaved, DialogOut::Card(CardChoice::Save)) => {
                let a = self.save();
                if self.dirty {
                    return a; // the write failed: stay, with the reason in the toast
                }
                return self.proceed(self.then);
            }
            (Ask::Unsaved, DialogOut::Card(_)) => {
                self.dirty = false;
                return self.proceed(self.then);
            }
            (Ask::SaveAs, DialogOut::Path(p)) => {
                let old = std::mem::replace(&mut self.path, p);
                let a = self.save();
                if self.dirty {
                    self.path = old; // the write failed: keep the old file
                }
                return a;
            }
            (Ask::NewFile, DialogOut::Path(p)) => {
                let made = match p.file_name().and_then(|n| n.to_str()).zip(p.parent()) {
                    Some((n, dir)) => fileops::new_file(dir, n),
                    None => Err("Type a file name".to_string()),
                };
                match made.and_then(|p| self.load(p)) {
                    Ok(()) => return AppAction::Toast(crate::launcher::i18n::tr_s(&format!("New sheet {}", self.file_name()))),
                    Err(e) => {
                        self.reopen(Ask::NewFile, p, &e);
                        return AppAction::Toast(e);
                    }
                }
            }
            (Ask::OpenFile, DialogOut::Path(p)) => match self.load(p.clone()) {
                Ok(()) => return AppAction::Toast(crate::launcher::i18n::tr_s(&format!("Opened {}", self.file_name()))),
                Err(e) => {
                    self.reopen(Ask::OpenFile, p, &e);
                    return AppAction::Toast(e);
                }
            },
            _ => {}
        }
        AppAction::None
    }

    /// Put the file panel back after a failure, with the reason in it.
    fn reopen(&mut self, ask: Ask, picked: PathBuf, why: &str) {
        let dir = picked.parent().map(PathBuf::from).unwrap_or_else(Self::home);
        let panel = if ask == Ask::OpenFile {
            SavePanel::open(&dir, &Self::home(), &fileops::SHEET_EXT)
        } else {
            let name = picked.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            SavePanel::new(&dir, &name, &Self::home()).titled("New sheet:", "Create")
        };
        let mut d = Dialog::new(DialogKind::Panel(panel), ask);
        d.say(why);
        self.modal = Some(d);
    }

    fn cell_at(&self, x: i32, y: i32) -> Option<(usize, usize)> {
        let r = Rect::new(X0 + ROW_HEAD_W, BODY_Y, (VIS_COLS as i32 * CELL_W) as u32, (VIS_ROWS as i32 * grid::ROW_H) as u32);
        contains(r, x, y).then(|| (self.scroll_col + ((x - r.x()) / CELL_W) as usize, self.scroll_row + ((y - BODY_Y) / grid::ROW_H) as usize))
    }
}

impl LauncherApp for SheetApp {
    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Arrows", "Move between cells"),
            k("Return / F2", "Edit cell"),
            k("0-9 = + -", "Start a number or formula"),
            k("Return (editing)", "Enter and go down"),
            k("Backspace", "Clear cell"),
            k("Ctrl+N / O / S", "New, Open, Save"),
        ]
    }

    fn title(&self) -> String {
        let name = self.file_name();
        if self.dirty { format!("{name} *") } else { name }
    }

    fn describe(&self) -> String {
        format!("Sheet {}{} (cell {})", self.file_name(), if self.dirty { " (unsaved changes)" } else { "" }, cell_name(self.col, self.row))
    }

    fn menu(&self) -> Vec<MenuItem> {
        vec![
            MenuItem::new("new", "New..."),
            MenuItem::new("open", "Open..."),
            MenuItem::new("save", "Save"),
            MenuItem::new("saveas", "Save As..."),
            MenuItem::new("clear", "Clear cell"),
            MenuItem::new("home", "Go to A1"),
        ]
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        self.load(path.to_path_buf())
    }

    fn request_close(&mut self) -> bool {
        self.commit();
        if self.dirty && self.modal.is_none() {
            self.then = Next::Quit;
            self.modal = Some(Dialog::new(DialogKind::Card(UnsavedCard::new(&self.file_name())), Ask::Unsaved));
            return false;
        }
        !self.dirty
    }

    fn context_menu(&mut self, x: i32, y: i32, _ctx: &Ctx) -> Option<Vec<MenuItem>> {
        let (c, r) = self.cell_at(x, y)?;
        self.commit();
        self.col = c;
        self.row = r;
        Some(vec![
            MenuItem::new("edit", "Edit cell"),
            MenuItem::new("clear", "Clear cell"),
            MenuItem::new("home", "Go to A1"),
            MenuItem::new("save", "Save"),
            MenuItem::new("saveas", "Save As..."),
        ])
    }

    fn menu_action(&mut self, id: &str, _ctx: &Ctx) -> AppAction {
        match id {
            "edit" => self.begin_edit(None),
            "new" => return self.guarded(Next::New),
            "open" => return self.guarded(Next::Open),
            "saveas" => self.ask_save_as(),
            "save" => {
                self.commit();
                return self.save();
            }
            "clear" => self.clear(),
            "home" => {
                self.col = 0;
                self.row = 0;
                self.follow();
            }
            _ => {}
        }
        AppAction::None
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, _ctx: &Ctx) -> AppAction {
        if let Some(d) = &mut self.modal {
            return match d.key(key) {
                Some(o) => self.finish(o),
                None => AppAction::None,
            };
        }
        let ctrl = mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
        if ctrl && key == Keycode::S {
            self.commit();
            return self.save();
        }
        if ctrl && key == Keycode::N {
            return self.guarded(Next::New);
        }
        if ctrl && key == Keycode::O {
            return self.guarded(Next::Open);
        }
        if let Some(e) = &mut self.edit {
            match key {
                Keycode::Escape => self.edit = None,
                Keycode::Return | Keycode::KpEnter => {
                    self.commit();
                    self.go(0, 1);
                }
                _ => {
                    e.key(key, Mod::NOMOD);
                }
            }
            return AppAction::None;
        }
        match arrow_alias(key) {
            Keycode::Escape => return AppAction::Quit,
            Keycode::Left => self.go(-1, 0),
            Keycode::Right => self.go(1, 0),
            Keycode::Up => self.go(0, -1),
            Keycode::Down => self.go(0, 1),
            Keycode::PageUp => self.go(0, -(VIS_ROWS as isize)),
            Keycode::PageDown => self.go(0, VIS_ROWS as isize),
            Keycode::Home => {
                self.col = 0;
                self.follow();
            }
            Keycode::Return | Keycode::KpEnter | Keycode::F2 => self.begin_edit(None),
            Keycode::Backspace | Keycode::Delete => self.clear(),
            _ => {}
        }
        AppAction::None
    }

    fn text(&mut self, text: &str, _ctx: &Ctx) -> AppAction {
        if let Some(d) = &mut self.modal {
            d.text(text);
            return AppAction::None;
        }
        let printable = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>();
        let t = printable(text);
        if let Some(e) = &mut self.edit {
            e.insert(&t);
        } else if t.chars().next().map_or(false, |c| c.is_ascii_digit() || "=+-.(".contains(c)) {
            // Letters stay navigation (F/X/Z/C are arrows); numbers and formulas start an edit.
            self.begin_edit(Some(&t));
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, _ctx: &Ctx) -> AppAction {
        if let Some(d) = &mut self.modal {
            match ev {
                PointerEvent::Down { x, y, button: 0, .. } => {
                    if let Some(o) = d.click(x, y) {
                        return self.finish(o);
                    }
                }
                PointerEvent::Wheel { dy, .. } => d.wheel(dy),
                _ => {}
            }
            return AppAction::None;
        }
        match ev {
            PointerEvent::Wheel { dy, .. } if self.edit.is_none() => {
                self.scroll_row = if dy > 0 { self.scroll_row.saturating_sub(1) } else { (self.scroll_row + 1).min(ROWS - VIS_ROWS) };
            }
            PointerEvent::Down { x, y, button: 0, clicks } => {
                if let Some((c, r)) = self.cell_at(x, y) {
                    self.commit();
                    self.col = c;
                    self.row = r;
                    if clicks >= 2 {
                        self.begin_edit(None);
                    }
                }
            }
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
        let right = grid::SCROLL_X + grid::SCROLL_W;

        // Input line: reference, then the raw contents (or the edit in progress).
        let bar = Rect::new(X0, BAR_Y, (right - X0 - grid::MARGIN) as u32, widgets::FIELD_H as u32);
        let name = cell_name(self.col, self.row);
        let field = match &self.edit {
            Some(e) => widgets::Field { text: &e.text, caret: Some(e.caret), ..Default::default() },
            None => widgets::Field::new(&self.sheet.cells[self.idx()]),
        };
        widgets::draw_field(c, fs, tm, bar, &widgets::Field { prefix: &name, prefix_w: 32, ..field });

        // Column headers and row numbers.
        let hdr = Rect::new(X0, HEAD_Y, (ROW_HEAD_W + VIS_COLS as i32 * CELL_W) as u32, grid::ROW_H as u32);
        ui::fill_rect(c, hdr, alt);
        for k in 0..VIS_COLS {
            let col = self.scroll_col + k;
            if col >= COLS {
                break;
            }
            let l = col_name(col).to_string();
            let x = X0 + ROW_HEAD_W + k as i32 * CELL_W + (CELL_W - fs.measure_text(&l)) / 2;
            fs.draw_text(c, x, HEAD_Y + 1, &l, if col == self.col { ink } else { faint });
        }
        let body = Rect::new(X0 + ROW_HEAD_W, BODY_Y, (VIS_COLS as i32 * CELL_W) as u32, (GRID_BOTTOM - BODY_Y) as u32);
        ui::fill_rect(c, body, paper);
        ui::fill_rect(c, Rect::new(X0, BODY_Y, ROW_HEAD_W as u32, body.height()), alt);
        for k in 0..VIS_ROWS {
            let row = self.scroll_row + k;
            if row >= ROWS {
                break;
            }
            let y = BODY_Y + k as i32 * grid::ROW_H;
            let l = (row + 1).to_string();
            fs.draw_text(c, X0 + ROW_HEAD_W - 4 - fs.measure_text(&l), y + 1, &l, if row == self.row { ink } else { faint });
            for j in 0..VIS_COLS {
                let col = self.scroll_col + j;
                if col >= COLS {
                    break;
                }
                // The plate sits inside the grid lines (left / bottom pixel row are the rules).
                let cell = Rect::new(body.x() + j as i32 * CELL_W + 1, y, CELL_W as u32 - 1, grid::ROW_H as u32 - 1);
                let (text, num) = &self.shown[row * COLS + col];
                let sel = row == self.row && col == self.col;
                if sel {
                    ui::fill_rect(c, cell, widgets::selection(tm).bg);
                    ui::draw_rect(c, cell, ink);
                } else if row == self.row || col == self.col {
                    ui::fill_rect(c, cell, soft);
                }
                let fg = if sel { widgets::selection(tm).fg } else if text == "#ERR" { faint } else { ink };
                let t = chrome::fit_text(fs, text, CELL_W - 6);
                let w = fs.measure_text(&t);
                let tx = if *num { cell.right() - 3 - w } else { cell.x() + 3 };
                fs.draw_text(c, tx, y + 1, &t, fg);
            }
        }
        // Grid lines between every cell (solid 1 px `line`), also through the row numbers and
        // the column letters; a heavier ink rule closes the header band and the row-number column.
        let full_h = (grid::ROW_H + body.height() as i32) as u32;
        for j in 0..VIS_COLS as i32 {
            ui::fill_rect(c, Rect::new(body.x() + j * CELL_W, HEAD_Y, 1, full_h), line);
        }
        for k in 0..VIS_ROWS as i32 {
            ui::fill_rect(c, Rect::new(X0, BODY_Y + k * grid::ROW_H + grid::ROW_H - 1, hdr.width(), 1), line);
        }
        ui::fill_rect(c, Rect::new(X0, BODY_Y - 1, hdr.width(), 1), ink);
        ui::fill_rect(c, Rect::new(body.x() - 1, HEAD_Y, 1, full_h), ink);
        ui::draw_rect(c, Rect::new(X0, HEAD_Y, hdr.width(), (grid::ROW_H + body.height() as i32) as u32), ink);
        if let Some(d) = &self.modal {
            d.draw(c, fs, tm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(cells: &[(&str, &str)]) -> Sheet {
        let mut s = Sheet::new();
        for (n, t) in cells {
            s.set(n, t);
        }
        s
    }

    #[test]
    fn references_parse() {
        assert_eq!(parse_ref("A1"), Some((0, 0)));
        assert_eq!(parse_ref("h99"), Some((7, 98)));
        assert_eq!(parse_ref("I1"), None);
        assert_eq!(parse_ref("A0"), None);
        assert_eq!(parse_ref("A100"), None);
        assert_eq!(parse_ref("A"), None);
        assert_eq!(cell_name(2, 11), "C12");
    }

    #[test]
    fn arithmetic_and_references() {
        let s = sheet(&[("A1", "2"), ("B2", "5"), ("C1", "=A1+B2*3"), ("C2", "=(A1+B2)*3"), ("C3", "=-A1+10/4"), ("C4", "= a1 * 1.5")]);
        assert_eq!(s.display("C1"), "17");
        assert_eq!(s.display("C2"), "21");
        assert_eq!(s.display("C3"), "0.5");
        assert_eq!(s.display("C4"), "3");
    }

    #[test]
    fn functions_over_ranges() {
        let s = sheet(&[
            ("A1", "1"), ("A2", "2"), ("A3", "text"), ("A4", ""), ("A5", "6"),
            ("B1", "=SUM(A1:A5)"), ("B2", "=AVG(A1:A5)"), ("B3", "=MIN(A1:A5)"), ("B4", "=MAX(A1:A5, 10)"),
            ("B5", "=COUNT(A1:A5)"), ("B6", "=SUM(A1:A2, 100)"), ("B7", "=AVG(A3:A4)"), ("B8", "=SUM(A1:B1)"),
        ]);
        assert_eq!(s.display("B1"), "9", "text and empty cells are skipped");
        assert_eq!(s.display("B2"), "3");
        assert_eq!(s.display("B3"), "1");
        assert_eq!(s.display("B4"), "10");
        assert_eq!(s.display("B5"), "3");
        assert_eq!(s.display("B6"), "103");
        assert_eq!(s.display("B7"), "#ERR", "average of nothing");
        assert_eq!(s.display("B8"), "10", "2-D ranges include the formula cell B1 only if it is a number");
    }

    #[test]
    fn cycles_and_bad_formulas_show_err() {
        let s = sheet(&[("A1", "=B1"), ("B1", "=A1"), ("C1", "=C1+1"), ("D1", "=A1+1"), ("E1", "=1/0"), ("F1", "=1+"), ("G1", "=Z9"), ("H1", "=A9+\"x\""), ("A2", "x"), ("B2", "=A2+1")]);
        for n in ["A1", "B1", "C1", "D1", "E1", "F1", "G1", "H1", "B2"] {
            assert_eq!(s.display(n), "#ERR", "{n}");
        }
        assert_eq!(s.display("A2"), "x");
    }

    #[test]
    fn numbers_format_compactly() {
        assert_eq!(format_num(3.0), "3");
        assert_eq!(format_num(-0.5), "-0.5");
        assert_eq!(format_num(1.0 / 3.0), "0.333333");
        assert_eq!(format_num(-0.0000001), "0");
    }

    #[test]
    fn csv_round_trips_quotes_commas_and_newlines() {
        let s = sheet(&[("A1", "hello, world"), ("B1", "say \"hi\""), ("A2", "=SUM(A3:A4)"), ("C2", "multi\nline"), ("A3", "5")]);
        let csv = s.to_csv();
        assert!(csv.starts_with("\"hello, world\",\"say \"\"hi\"\"\"\n"));
        let t = Sheet::from_csv(&csv);
        assert_eq!(t.cells, s.cells);
        assert_eq!(Sheet::from_csv("1,2\r\n3\r\n").cells[COLS], "3");
        assert_eq!(Sheet::from_csv("").to_csv(), "");
    }

    #[test]
    fn csv_overflow_is_ignored() {
        let t = Sheet::from_csv("1,2,3,4,5,6,7,8,9,10\n");
        assert_eq!(t.cells[7], "8");
        assert_eq!(t.cells[8], "", "column I does not exist");
    }

    #[test]
    fn editing_and_saving_through_the_app() {
        let path = std::env::temp_dir().join(format!("harbour_sheet_{}.csv", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut a = SheetApp::open(path.clone());
        a.begin_edit(Some("4"));
        a.commit();
        a.go(0, 1);
        a.begin_edit(Some("=A1*2"));
        a.commit();
        assert_eq!(a.shown[COLS].0, "8");
        assert!(a.dirty);
        assert!(matches!(a.save(), AppAction::Toast(t) if t.starts_with("Saved")));
        assert!(!a.dirty);
        let b = SheetApp::open(path.clone());
        assert_eq!(b.shown[COLS].0, "8");
        assert_eq!(b.sheet.cells[COLS], "=A1*2");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_cursor_stays_inside_and_scrolls() {
        let mut a = SheetApp::open(std::env::temp_dir().join("harbour_sheet_nonexistent.csv"));
        a.go(-1, -1);
        assert_eq!((a.col, a.row), (0, 0));
        a.go(100, 200);
        assert_eq!((a.col, a.row), (COLS - 1, ROWS - 1));
        assert!(a.scroll_row + VIS_ROWS >= ROWS && a.scroll_col + VIS_COLS >= COLS);
    }

    #[test]
    fn right_click_on_a_cell_selects_it_and_offers_edit_and_clear() {
        use crate::font::FontSystem;
        use crate::system::mock::MockBackend;
        use crate::theme::{Theme, ThemeManager};
        let fs = FontSystem::new();
        let tm = ThemeManager::new(Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = MockBackend::new();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut s = SheetApp::open(std::env::temp_dir().join("harbour_sheet_ctx_nonexistent.csv"));
        let (x, y) = (X0 + ROW_HEAD_W + CELL_W + 2, BODY_Y + 2 * grid::ROW_H + 2);
        let items = s.context_menu(x, y, &ctx).expect("on a cell");
        assert_eq!((s.col, s.row), (1, 2));
        assert_eq!(items[0].id, "edit");
        s.menu_action("edit", &ctx);
        assert!(s.edit.is_some());
        assert!(s.context_menu(0, 0, &ctx).is_none(), "off the grid the app menu is used");
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("harbour_sheetops_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn type_in(a: &mut SheetApp, text: &str) {
        a.modal.as_mut().unwrap().text(text);
    }

    fn press(a: &mut SheetApp, key: Keycode) -> AppAction {
        let d = a.modal.as_mut().unwrap();
        match d.key(key) {
            Some(o) => a.finish(o),
            None => AppAction::None,
        }
    }

    #[test]
    fn leaving_with_unsaved_changes_asks_and_dont_save_drops_them() {
        let d = tmpdir("leave");
        let path = d.join("a.csv");
        std::fs::write(&path, "1\n").unwrap();
        let mut a = SheetApp::open(path.clone());
        assert!(a.request_close(), "nothing to lose: leave at once");
        a.begin_edit(Some("9"));
        assert!(!a.request_close(), "unsaved: stay and ask");
        assert!(matches!(a.modal.as_ref().unwrap().kind, DialogKind::Card(_)));
        assert_eq!(press(&mut a, Keycode::Escape), AppAction::None, "Cancel stays");
        assert!(a.dirty && a.modal.is_none());
        assert!(!a.request_close());
        assert_eq!(press(&mut a, Keycode::D), AppAction::Quit, "Don't Save leaves");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "1\n", "the file is untouched");
        assert!(!a.request_close() || !a.dirty);
        // Save then leave.
        let mut a = SheetApp::open(path.clone());
        a.begin_edit(Some("7"));
        a.commit();
        assert!(!a.request_close());
        assert_eq!(press(&mut a, Keycode::S), AppAction::Quit);
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("7"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn save_as_writes_a_new_file_and_asks_before_replacing() {
        let d = tmpdir("saveas");
        std::fs::write(d.join("other.csv"), "OLD\n").unwrap();
        let mut a = SheetApp::open(d.join("a.csv"));
        a.begin_edit(Some("5"));
        a.commit();
        a.ask_save_as();
        type_in(&mut a, "b.csv");
        press(&mut a, Keycode::Return);
        assert_eq!(a.path, d.join("b.csv"));
        assert!(std::fs::read_to_string(d.join("b.csv")).unwrap().starts_with("5"));
        assert!(!a.dirty && a.title() == "b.csv");
        // An existing name needs a second Enter.
        a.begin_edit(Some("6"));
        a.commit();
        a.ask_save_as();
        type_in(&mut a, "other.csv");
        press(&mut a, Keycode::Return);
        assert_eq!(std::fs::read_to_string(d.join("other.csv")).unwrap(), "OLD\n", "first Enter only warns");
        press(&mut a, Keycode::Return);
        assert!(std::fs::read_to_string(d.join("other.csv")).unwrap().starts_with("6"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn new_and_open_guard_unsaved_work_and_never_replace_a_file() {
        let d = tmpdir("newopen");
        std::fs::write(d.join("a.csv"), "1\n").unwrap();
        std::fs::write(d.join("b.csv"), "2,3\n").unwrap();
        std::fs::write(d.join("note.txt"), "x").unwrap();
        let mut a = SheetApp::open(d.join("a.csv"));
        a.begin_edit(Some("9"));
        a.commit();
        // Open with unsaved changes asks first; Cancel keeps everything.
        a.guarded(Next::Open);
        assert!(matches!(a.modal.as_ref().unwrap().kind, DialogKind::Card(_)));
        press(&mut a, Keycode::Escape);
        assert!(a.dirty && a.path == d.join("a.csv"));
        // Don't Save, then pick b.csv (txt files are not offered).
        a.guarded(Next::Open);
        press(&mut a, Keycode::D);
        let DialogKind::Panel(p) = &a.modal.as_ref().unwrap().kind else { panic!("open panel expected") };
        assert!(p.open_mode && !p.entries.iter().any(|e| e.name == "note.txt"));
        let i = p.entries.iter().position(|e| e.name == "b.csv").unwrap();
        for _ in 0..=i {
            a.modal.as_mut().unwrap().key(Keycode::Down);
        }
        press(&mut a, Keycode::Return);
        assert_eq!(a.path, d.join("b.csv"));
        assert_eq!(a.sheet.cells[1], "3");
        assert!(!a.dirty);
        // New: an existing name is refused, a free one starts an empty sheet.
        a.guarded(Next::New);
        type_in(&mut a, "a.csv");
        press(&mut a, Keycode::Return); // warns (exists)
        let r = press(&mut a, Keycode::Return);
        assert!(matches!(r, AppAction::Toast(t) if t.contains("already exists")));
        assert_eq!(std::fs::read_to_string(d.join("a.csv")).unwrap(), "1\n", "never replaced");
        assert!(a.modal.is_some(), "the panel returns so the name can be changed");
        type_in(&mut a, "fresh.csv");
        let r = press(&mut a, Keycode::Return);
        assert!(matches!(r, AppAction::Toast(t) if t.contains("fresh.csv")));
        assert!(d.join("fresh.csv").exists() && a.sheet.cells.iter().all(|c| c.is_empty()));
        assert_eq!(a.title(), "fresh.csv");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn open_path_loads_a_csv_and_reports_a_missing_file() {
        let d = tmpdir("openpath");
        std::fs::write(d.join("日本語.csv"), "あ,2\n").unwrap();
        let mut a = SheetApp::open(d.join("x.csv"));
        a.open_path(&d.join("日本語.csv")).unwrap();
        assert_eq!(a.sheet.cells[0], "あ");
        assert_eq!(a.title(), "日本語.csv");
        assert!(a.open_path(&d.join("nope.csv")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn help_rows_are_filled_in() {
        crate::launcher::help::check_app_rows("Sheet", &SheetApp::open(std::env::temp_dir().join("sheet-help-test-none.csv")).help());
    }
}
