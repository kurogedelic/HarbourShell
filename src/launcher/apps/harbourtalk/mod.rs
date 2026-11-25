//! HarbourTalk: a small bulletin board of public rooms, each one timeline of short posts, newest
//! first. Everything is public: there are no private messages. The rooms are the server's choice.
//!
//! Posts carry a handle name and this device's short id (`kuro#3fa2`), at most 200 characters,
//! and a device posts at most once every 30 seconds (the server checks both again). The board is
//! a small HTTP + JSON server (see [`net`] and `server/harbourtalk/`, deployed at
//! [`net::DEFAULT_SERVER`]); with the server set to `demo`, and in mock runs, it is a demo board
//! on this device. The last posts are kept for reading offline.
//!
//! Keys: Up / Down pick a room or a post, Enter opens it (Left / Right: the next posts), typing
//! starts a new post in the room (Ctrl+N too), Enter sends it, Esc goes back and keeps the draft.
//! Ctrl+R reloads.
//! Alt: the menu (handle name, server).

pub mod net;

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::keys::{Keycode, Mod};
use crate::gfx::Color;
use crate::gfx::Rect;
use crate::gfx::Canvas;

use crate::assets::AssetStore;
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp, Sizing};
use crate::launcher::apps::browser::layout::{self as page_layout, Metrics};
use crate::launcher::i18n::{self, tr, tr_s, Lang};
use crate::launcher::lineedit::LineEdit;
use crate::launcher::widgets::{self, MenuItem, PointerEvent, ScrollBar};
use crate::platform::keys::arrow_alias;
use crate::rt;
use crate::ui;

use net::{Board, Post, Reply, Room, Settings, Store};

const ROW: i32 = 16;
/// One post in the timeline: the author row and one row of its text.
const ENTRY_H: i32 = 28;
/// The toolbar right under the menu strip: buttons big enough for a finger (the web page on a
/// phone shows the 320 x 170 picture enlarged across its width), then a rule.
const TOOL_H: i32 = 24;
/// Widest toolbar button (a desk window does not stretch them across).
const TOOL_MAX_W: i32 = 110;
/// Lines of a post's full text, and of the draft.
const LINE_H: i32 = 14;
const LEFT: i32 = 6;
/// Reload the timeline this often while it is open (seconds; server boards only).
const AUTO_RELOAD: i64 = 90;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// The list of public rooms (where the app opens).
    Rooms,
    /// One room's posts.
    Timeline,
    Read,
    Write,
    Name,
    Server,
}

/// The row under the menu strip, then the page.
fn head_y() -> i32 {
    grid::BAR_H + TOOL_H
}
fn page_y() -> i32 {
    head_y() + ROW + 1
}
fn page_bottom() -> i32 {
    grid::frame_bottom()
}
/// The toolbar's top (right under the menu strip); its rule is its last row.
fn bar_y() -> i32 {
    grid::BAR_H
}
fn bar_rule_y() -> i32 {
    bar_y() + TOOL_H - 1
}
/// The last row of the page.
fn content_bottom() -> i32 {
    page_bottom()
}

/// What a toolbar button does (the same as its key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Open,
    Refresh,
    Name,
    Server,
    Rooms,
    Write,
    Read,
    Back,
    Previous,
    Next,
    Post,
    Save,
}

impl Act {
    fn label(self) -> &'static str {
        match self {
            Act::Open => "Open",
            Act::Refresh => "Refresh",
            Act::Name => "My name",
            Act::Server => "Server",
            Act::Rooms => "Rooms",
            Act::Write => "Write",
            Act::Read => "Read",
            Act::Back => "Back",
            Act::Previous => "Previous",
            Act::Next => "Next",
            Act::Post => "Post",
            Act::Save => "Save",
        }
    }
}

/// The toolbar's buttons for `n` actions: equal widths across the page, at most [`TOOL_MAX_W`].
pub fn tool_rects(n: usize) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let (gap, left) = (4, 4);
    let room = grid::sw() - 2 * left - gap * (n as i32 - 1);
    let w = (room / n as i32).min(TOOL_MAX_W);
    let y = bar_y() + 2;
    let h = bar_rule_y() - 2 - y;
    (0..n).map(|i| Rect::new(left + i as i32 * (w + gap), y, w as u32, h as u32)).collect()
}
/// Right edge of text: clear of the scroll bar column.
fn text_right() -> i32 {
    grid::scroll_x() - 4
}
/// Whole timeline entries that fit.
pub fn entries_shown() -> usize {
    ((content_bottom() + 1 - page_y()) / ENTRY_H).max(1) as usize
}
/// The scroll bar beside the room and post lists.
fn list_bar_rect() -> Rect {
    Rect::new(grid::scroll_x(), page_y(), grid::SCROLL_W as u32, (entries_shown() as i32 * ENTRY_H) as u32)
}
fn read_lines_shown() -> usize {
    ((content_bottom() - page_y() - 2) / LINE_H).max(1) as usize
}

fn ja() -> bool {
    i18n::get() == Lang::Ja
}

/// "now", "5m", "3h", "2d", then the date.
pub fn ago(now: i64, t: i64) -> String {
    let s = (now - t).max(0);
    let j = ja();
    match s {
        0..=59 => (if j { "今" } else { "now" }).to_string(),
        60..=3599 => format!("{}{}", s / 60, if j { "分" } else { "m" }),
        3600..=86_399 => format!("{}{}", s / 3600, if j { "時間" } else { "h" }),
        86_400..=604_799 => format!("{}{}", s / 86_400, if j { "日" } else { "d" }),
        _ => chrono::DateTime::from_timestamp(t, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%m/%d").to_string())
            .unwrap_or_default(),
    }
}

/// `text` cut to `max` px with an ellipsis.
fn fit(text: &str, max: i32, measure: &dyn Fn(&str) -> i32) -> String {
    if measure(text) <= max {
        return text.to_string();
    }
    let dots = "…";
    let room = max - measure(dots);
    let mut out = String::new();
    for ch in text.chars() {
        out.push(ch);
        if measure(&out) > room {
            out.pop();
            break;
        }
    }
    out + dots
}

/// The draft broken into lines by width, character by character (so the caret maps to a line):
/// (index of the line's first character, the line).
pub fn wrap_chars(text: &str, max: i32, measure: &dyn Fn(&str) -> i32) -> Vec<(usize, String)> {
    let mut lines = vec![(0usize, String::new())];
    for (i, ch) in text.chars().enumerate() {
        let (start, cur) = lines.last_mut().unwrap();
        cur.push(ch);
        if measure(cur) <= max || cur.chars().count() <= 1 {
            continue;
        }
        cur.pop();
        // A word that does not fit goes down whole when there is a space before it on the line.
        let chars: Vec<char> = cur.chars().collect();
        let sp = if ch == ' ' { None } else { chars.iter().rposition(|c| *c == ' ').filter(|k| *k + 1 < chars.len()) };
        match sp {
            Some(k) => {
                let tail: String = chars[k + 1..].iter().collect::<String>() + &ch.to_string();
                let s = *start + k + 1;
                *cur = chars[..=k].iter().collect();
                lines.push((s, tail));
            }
            None => lines.push((i, ch.to_string())),
        }
    }
    lines
}

/// The line holding caret column `caret`, and the caret's column in it.
pub fn caret_line(lines: &[(usize, String)], caret: usize) -> (usize, usize) {
    let k = lines.iter().rposition(|(s, _)| *s <= caret).unwrap_or(0);
    (k, caret - lines[k].0)
}

pub struct HarbourTalk {
    store: Store,
    pub settings: Settings,
    mock: bool,
    board: Board,
    /// The public rooms, and the one picked in their list.
    pub rooms: Vec<Room>,
    pub room_sel: usize,
    room_top: usize,
    /// The room open in the timeline (its id; empty before one is opened).
    pub room: String,
    /// Each room's newest posts as last seen (offline copy), by room id.
    room_posts: BTreeMap<String, Vec<Post>>,
    /// The open room's posts.
    pub posts: Vec<Post>,
    fetched_rooms: Option<i64>,
    fetched_posts: Option<i64>,
    pub view: View,
    /// Where Esc leaves the name / server field for.
    field_from: View,
    pub sel: usize,
    top: usize,
    read_top: usize,
    read_len: usize,
    pub draft: LineEdit,
    field: LineEdit,
    /// The handle name is being asked for before the first post: go on to Write after it.
    then_write: bool,
    /// The line in the head row: what is going on, or what went wrong.
    pub status: Option<String>,
    pub loading_rooms: bool,
    pub loading: bool,
    pub sending: bool,
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
    shown_wait: i64,
    sb_drag: bool,
    /// The toolbar button under a press (it acts when the press ends on it).
    tool_down: Option<Act>,
}

pub fn make() -> Box<dyn LauncherApp> {
    // The web demo runs with pretend hardware (HARBOUR_MOCK) but may still reach a real board
    // through the browser; with no server set it is the demo board there too.
    let mock = !rt::WEB && matches!(std::env::var("HARBOUR_MOCK").as_deref(), Ok("1" | "true" | "yes" | "on"));
    let store = if mock { Store { dir: None } } else { Store::home() };
    let mut app = HarbourTalk::new(store, mock);
    app.reload_rooms(crate::rt::now().timestamp());
    Box::new(app)
}

/// A list position kept in view: (selected, first shown) after moving by `d` in `len` entries.
fn scrolled(sel: usize, top: usize, d: isize, len: usize) -> (usize, usize) {
    if len == 0 {
        return (0, 0);
    }
    let n = entries_shown();
    let sel = (sel as isize + d).clamp(0, len as isize - 1) as usize;
    let mut top = top;
    if sel < top {
        top = sel;
    } else if sel >= top + n {
        top = sel + 1 - n;
    }
    (sel, top.min(len.saturating_sub(n)))
}

impl HarbourTalk {
    pub fn new(store: Store, mock: bool) -> Self {
        let mut settings = store.load_settings();
        if settings.dev.len() != 8 {
            settings.dev = net::new_device_id();
            let _ = store.save_settings(&settings);
        }
        let board = Board::for_settings(&settings, mock);
        let cache = store.load_cache();
        let fresh = cache.server == settings.server() && !mock;
        let (tx, rx) = channel();
        HarbourTalk {
            store,
            settings,
            mock,
            board,
            rooms: if fresh { cache.rooms } else { Vec::new() },
            room_sel: 0,
            room_top: 0,
            room: String::new(),
            room_posts: if fresh { cache.posts } else { BTreeMap::new() },
            posts: Vec::new(),
            fetched_rooms: None,
            fetched_posts: None,
            view: View::Rooms,
            field_from: View::Rooms,
            sel: 0,
            top: 0,
            read_top: 0,
            read_len: 0,
            draft: LineEdit::default(),
            field: LineEdit::default(),
            then_write: false,
            status: None,
            loading_rooms: false,
            loading: false,
            sending: false,
            tx,
            rx,
            shown_wait: 0,
            sb_drag: false,
            tool_down: None,
        }
    }

    pub fn is_demo(&self) -> bool {
        self.board == Board::Demo
    }

    /// A room's name as shown (the demo board's names are translated).
    pub fn room_name(&self, id: &str) -> String {
        let name = self.rooms.iter().find(|r| r.id == id).map_or(id, |r| r.name.as_str());
        if self.is_demo() { tr_s(name) } else { name.to_string() }
    }

    /// Reload what the page shows: the rooms, or the open room's posts.
    pub fn reload(&mut self, now: i64) {
        if self.view == View::Rooms {
            self.reload_rooms(now);
        } else {
            self.reload_posts(now);
        }
    }

    /// Ask the board for its rooms (on a worker).
    pub fn reload_rooms(&mut self, now: i64) {
        if self.loading_rooms {
            return;
        }
        self.loading_rooms = true;
        self.fetched_rooms = Some(now);
        self.status = Some("Loading...".into());
        let (tx, board) = (self.tx.clone(), self.board.clone());
        rt::spawn(move || {
            let _ = tx.send(Reply::Rooms(net::load_rooms(&board, now)));
        });
    }

    /// Ask the board for the open room's newest posts (on a worker).
    pub fn reload_posts(&mut self, now: i64) {
        if self.loading || self.room.is_empty() {
            return;
        }
        self.loading = true;
        self.fetched_posts = Some(now);
        self.status = Some("Loading...".into());
        let (tx, board, room) = (self.tx.clone(), self.board.clone(), self.room.clone());
        rt::spawn(move || {
            let r = net::load(&board, &room, now);
            let _ = tx.send(Reply::Posts(room, r));
        });
    }

    /// Open room `i` of the list: its offline copy at once, then the newest from the board.
    pub fn open_room(&mut self, i: usize, now: i64) {
        let Some(r) = self.rooms.get(i) else { return };
        self.room_sel = i;
        if self.room != r.id {
            self.room = r.id.clone();
            self.posts = self.room_posts.get(&self.room).cloned().unwrap_or_default();
            self.sel = 0;
            self.top = 0;
        }
        self.view = View::Timeline;
        self.status = None;
        self.loading = false;
        self.reload_posts(now);
    }

    /// The demo board keeps what was written on it during this run: its own posts in `room`.
    fn local_posts(&self, room: &str) -> Vec<Post> {
        self.room_posts.get(room).map_or(Vec::new(), |v| v.iter().filter(|p| p.id.starts_with("local-")).cloned().collect())
    }

    /// Count a new post in its room's row.
    fn count_post(&mut self, p: &Post) {
        if let Some(r) = self.rooms.iter_mut().find(|r| r.id == p.room) {
            r.count += 1;
            r.last = r.last.max(p.time);
        }
    }

    /// Take what the worker sent back. True when something changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(r) = self.rx.try_recv() {
            changed = true;
            match r {
                Reply::Rooms(Ok(mut v)) => {
                    self.loading_rooms = false;
                    if self.view == View::Rooms {
                        self.status = None;
                    }
                    if self.is_demo() {
                        for room in v.iter_mut() {
                            for p in self.local_posts(&room.id) {
                                room.count += 1;
                                room.last = room.last.max(p.time);
                            }
                        }
                    }
                    let keep = self.rooms.get(self.room_sel).map(|r| r.id.clone());
                    self.rooms = v;
                    self.room_sel = keep.and_then(|id| self.rooms.iter().position(|r| r.id == id)).unwrap_or(0);
                    (self.room_sel, self.room_top) = scrolled(self.room_sel, self.room_top, 0, self.rooms.len());
                    self.save_cache();
                }
                Reply::Rooms(Err(e)) => {
                    self.loading_rooms = false;
                    if self.view == View::Rooms {
                        self.status = Some(if self.rooms.is_empty() { e } else { "Offline".into() });
                    }
                }
                Reply::Posts(room, Ok(mut v)) => {
                    if self.is_demo() {
                        v.extend(self.local_posts(&room));
                        v.sort_by(|a, b| b.time.cmp(&a.time));
                    }
                    if let Some(r) = self.rooms.iter_mut().find(|r| r.id == room) {
                        r.count = r.count.max(v.len() as u32);
                        r.last = v.first().map_or(r.last, |p| p.time.max(r.last));
                    }
                    self.room_posts.insert(room.clone(), v.clone());
                    if room == self.room {
                        self.loading = false;
                        if self.view != View::Rooms && !self.sending {
                            self.status = None;
                        }
                        let keep = self.posts.get(self.sel).map(|p| p.id.clone());
                        self.posts = v;
                        let sel = keep.and_then(|id| self.posts.iter().position(|p| p.id == id)).unwrap_or(0);
                        (self.sel, self.top) = scrolled(sel, self.top, 0, self.posts.len());
                    }
                    self.save_cache();
                }
                Reply::Posts(room, Err(e)) => {
                    if room == self.room {
                        self.loading = false;
                        if matches!(self.view, View::Timeline | View::Read) {
                            self.status = Some(if self.posts.is_empty() { e } else { "Offline".into() });
                        }
                    }
                }
                Reply::Posted(Ok(p)) => {
                    self.sending = false;
                    self.status = None;
                    self.settings.last_post = p.time.max(crate::rt::now().timestamp());
                    self.save_settings();
                    self.count_post(&p);
                    let list = self.room_posts.entry(p.room.clone()).or_default();
                    list.retain(|q| q.id != p.id);
                    list.insert(0, p.clone());
                    list.truncate(net::KEEP);
                    if p.room == self.room {
                        self.posts = list.clone();
                        self.sel = 0;
                        self.top = 0;
                    }
                    self.draft.clear();
                    self.view = View::Timeline;
                    self.save_cache();
                }
                Reply::Posted(Err(e)) => {
                    self.sending = false;
                    // The server's own count ("Wait 12 s to post again") drives the countdown.
                    if let Some(n) = net::wait_seconds(&e) {
                        self.settings.last_post = crate::rt::now().timestamp() - net::MIN_GAP + n;
                        self.save_settings();
                    }
                    self.status = Some(e);
                }
            }
        }
        changed
    }

    fn save_cache(&self) {
        let c = net::Cache {
            server: self.settings.server().to_string(),
            fetched: self.fetched_rooms,
            rooms: self.rooms.clone(),
            posts: self.room_posts.clone(),
        };
        let _ = self.store.save_cache(&c);
    }

    fn save_settings(&self) {
        let _ = self.store.save_settings(&self.settings);
    }

    fn step(&mut self, d: isize) {
        (self.sel, self.top) = scrolled(self.sel, self.top, d, self.posts.len());
    }

    fn step_room(&mut self, d: isize) {
        (self.room_sel, self.room_top) = scrolled(self.room_sel, self.room_top, d, self.rooms.len());
    }

    /// Open the editor for a new post in the open room; the handle name first if there is none.
    pub fn start_writing(&mut self, typed: &str) {
        if self.sending || self.room.is_empty() {
            return;
        }
        if self.settings.name.is_empty() {
            self.then_write = true;
            self.open_field(View::Name);
            self.field_from = View::Timeline;
            self.field.insert(typed);
            return;
        }
        self.status = None;
        self.view = View::Write;
        if self.draft.len() + typed.chars().count() <= net::MAX_TEXT {
            self.draft.insert(typed);
        }
    }

    fn open_field(&mut self, v: View) {
        self.field = LineEdit::new(match v {
            View::Name => &self.settings.name,
            _ => &self.settings.server,
        });
        self.field_from = if matches!(self.view, View::Rooms) || self.room.is_empty() { View::Rooms } else { View::Timeline };
        self.status = None;
        self.view = v;
    }

    /// Enter in the editor: check the text and the gap, then send.
    pub fn send(&mut self, now: i64) {
        if self.sending {
            return;
        }
        let text = net::clean_text(&self.draft.text);
        if text.is_empty() {
            self.status = Some("Write something first".into());
            return;
        }
        let wait = self.settings.wait(now);
        if wait > 0 {
            self.status = Some(format!("Wait {wait} s to post again"));
            return;
        }
        self.sending = true;
        self.status = Some("Sending...".into());
        let (tx, board, room) = (self.tx.clone(), self.board.clone(), self.room.clone());
        let (name, dev) = (self.settings.name.clone(), self.settings.dev.clone());
        rt::spawn(move || {
            let _ = tx.send(Reply::Posted(net::send(&board, &room, &name, &dev, &text, now)));
        });
    }

    /// Enter in the name or server field.
    fn commit_field(&mut self, now: i64) {
        match self.view {
            View::Name => {
                let name = net::clean_name(&self.field.text);
                if name.is_empty() {
                    self.status = Some("Type a name".into());
                    return;
                }
                self.settings.name = name;
                self.save_settings();
                self.view = self.field_from;
                if std::mem::take(&mut self.then_write) {
                    self.start_writing("");
                }
            }
            View::Server => {
                let url = self.field.text.trim().to_string();
                if !url.is_empty() && url != "demo" && !net::is_web_address(&url) {
                    self.status = Some("Not a web address".into());
                    return;
                }
                self.settings.server = url;
                self.save_settings();
                self.board = Board::for_settings(&self.settings, self.mock);
                self.rooms.clear();
                self.room_posts.clear();
                self.posts.clear();
                self.room.clear();
                (self.sel, self.top, self.room_sel, self.room_top) = (0, 0, 0, 0);
                self.view = View::Rooms;
                self.loading_rooms = false;
                self.loading = false;
                self.reload_rooms(now);
            }
            _ => {}
        }
    }

    fn open_read(&mut self) {
        if self.posts.is_empty() {
            return;
        }
        self.view = View::Read;
        self.read_top = 0;
    }

    /// Esc: the field goes back where it came from, a post or the editor to the room, the room to
    /// the list of rooms.
    fn back(&mut self) {
        self.then_write = false;
        if !self.sending {
            self.status = None;
        }
        self.view = match self.view {
            View::Name | View::Server => self.field_from,
            View::Timeline => View::Rooms,
            _ => View::Timeline,
        };
    }

    /// The toolbar for this page: (action, enabled, the default one).
    pub fn toolbar(&self) -> Vec<(Act, bool, bool)> {
        let has_posts = !self.posts.is_empty();
        match self.view {
            View::Rooms => vec![
                (Act::Open, !self.rooms.is_empty(), true),
                (Act::Refresh, true, false),
                (Act::Name, true, false),
                (Act::Server, true, false),
            ],
            View::Timeline => vec![
                (Act::Rooms, true, false),
                (Act::Write, !self.sending, true),
                (Act::Read, has_posts, false),
                (Act::Refresh, true, false),
            ],
            View::Read => vec![
                (Act::Back, true, false),
                (Act::Previous, self.sel > 0, false),
                (Act::Next, self.sel + 1 < self.posts.len(), false),
                (Act::Write, !self.sending, true),
            ],
            View::Write => vec![(Act::Back, true, false), (Act::Post, !self.sending, true)],
            View::Name | View::Server => vec![(Act::Back, true, false), (Act::Save, true, true)],
        }
    }

    /// Do what a toolbar button says (each is also a key; see `key`).
    pub fn act(&mut self, a: Act, now: i64) {
        match a {
            Act::Open => self.open_room(self.room_sel, now),
            Act::Refresh => self.reload(now),
            Act::Name => {
                self.then_write = false;
                self.open_field(View::Name);
            }
            Act::Server => self.open_field(View::Server),
            Act::Rooms => {
                self.status = None;
                self.view = View::Rooms;
            }
            Act::Write => self.start_writing(""),
            Act::Read => self.open_read(),
            Act::Back => self.back(),
            Act::Previous | Act::Next => {
                self.step(if a == Act::Next { 1 } else { -1 });
                self.read_top = 0;
            }
            Act::Post => self.send(now),
            Act::Save => self.commit_field(now),
        }
    }

    /// The enabled toolbar button at (x, y).
    fn tool_at(&self, x: i32, y: i32) -> Option<Act> {
        let bar = self.toolbar();
        tool_rects(bar.len()).into_iter().zip(bar).find(|(r, (_, on, _))| *on && r.contains_point((x, y))).map(|(_, (a, _, _))| a)
    }

    fn scroll_read(&mut self, d: isize) {
        let max = self.read_len.saturating_sub(read_lines_shown());
        self.read_top = (self.read_top as isize + d).clamp(0, max as isize) as usize;
    }
}

impl LauncherApp for HarbourTalk {
    fn title(&self) -> String {
        "HarbourTalk".to_string()
    }

    fn sizing(&self) -> Sizing {
        Sizing::Flexible
    }

    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Up / Down", "Choose a room or a post"),
            k("Return", "Open / read / send"),
            k("Left / Right", "Previous / next post"),
            k("Type", "Write a new post"),
            k("Ctrl+N", "Write a new post"),
            k("Ctrl+R", "Reload"),
            k("Esc", "Back (the draft is kept)"),
        ]
    }

    fn describe(&self) -> String {
        let board = if self.is_demo() { "demo board" } else { self.settings.server() };
        let mut s = format!("HarbourTalk ({board}), {:?}", self.view);
        if self.view == View::Rooms {
            let names: Vec<String> = self.rooms.iter().map(|r| format!("{} ({} posts)", self.room_name(&r.id), r.count)).collect();
            s += &format!(": rooms {}", names.join(", "));
            if let Some(r) = self.rooms.get(self.room_sel) {
                s += &format!(", selected {}", self.room_name(&r.id));
            }
        } else {
            s += &format!(" in room {}: {} posts", self.room_name(&self.room), self.posts.len());
            if let Some(p) = self.posts.get(self.sel) {
                s += &format!(", selected {}: {}", p.author(), p.text);
            }
        }
        if self.view == View::Write {
            s += &format!(", draft: {}", self.draft.text);
        }
        if let Some(st) = &self.status {
            s += &format!(", status: {st}");
        }
        s
    }

    fn menu(&self) -> Vec<MenuItem> {
        let mut m = Vec::new();
        if !self.room.is_empty() && self.view != View::Rooms {
            m.push(MenuItem::new("write", "New post"));
            m.push(MenuItem::new("rooms", "Rooms"));
        }
        m.push(MenuItem::new("reload", "Reload"));
        m.push(MenuItem::new("name", "Handle name..."));
        m.push(MenuItem::new("server", "Server..."));
        m
    }

    fn menu_action(&mut self, id: &str, ctx: &Ctx) -> AppAction {
        match id {
            "write" => self.start_writing(""),
            "rooms" => {
                self.status = None;
                self.view = View::Rooms;
            }
            "reload" => self.reload(ctx.now.timestamp()),
            "name" => {
                self.then_write = false;
                self.open_field(View::Name);
            }
            "server" => self.open_field(View::Server),
            _ => {}
        }
        AppAction::None
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, ctx: &Ctx) -> AppAction {
        let ctrl = mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
        let now = ctx.now.timestamp();
        if ctrl && key == Keycode::R {
            self.reload(now);
            return AppAction::None;
        }
        match self.view {
            View::Rooms => match arrow_alias(key) {
                Keycode::Escape => return AppAction::Quit,
                Keycode::Up => self.step_room(-1),
                Keycode::Down => self.step_room(1),
                Keycode::Home => self.step_room(-(self.rooms.len() as isize)),
                Keycode::End => self.step_room(self.rooms.len() as isize),
                Keycode::Return | Keycode::KpEnter | Keycode::Right => self.open_room(self.room_sel, now),
                _ => {}
            },
            View::Timeline => match arrow_alias(key) {
                Keycode::Escape | Keycode::Left => self.back(),
                Keycode::N if ctrl => self.start_writing(""),
                Keycode::Up => self.step(-1),
                Keycode::Down => self.step(1),
                Keycode::PageUp => self.step(-(entries_shown() as isize)),
                Keycode::PageDown => self.step(entries_shown() as isize),
                Keycode::Home => self.step(-(self.posts.len() as isize)),
                Keycode::End => self.step(self.posts.len() as isize),
                Keycode::Return | Keycode::KpEnter => {
                    if self.posts.is_empty() {
                        self.start_writing("");
                    } else {
                        self.open_read();
                    }
                }
                _ => {}
            },
            View::Read => match arrow_alias(key) {
                Keycode::Escape | Keycode::Return | Keycode::KpEnter | Keycode::Backspace => self.back(),
                Keycode::N if ctrl => self.start_writing(""),
                Keycode::Up => self.scroll_read(-1),
                Keycode::Down => self.scroll_read(1),
                Keycode::PageUp => self.scroll_read(-(read_lines_shown() as isize)),
                Keycode::PageDown | Keycode::Space => self.scroll_read(read_lines_shown() as isize),
                Keycode::Left => {
                    self.step(-1);
                    self.read_top = 0;
                }
                Keycode::Right => {
                    self.step(1);
                    self.read_top = 0;
                }
                _ => {}
            },
            View::Write => match key {
                Keycode::Escape => self.back(),
                Keycode::Return | Keycode::KpEnter => self.send(now),
                _ => {
                    if !self.sending && self.draft.key(key, mods) && self.status.as_deref() == Some("Write something first") {
                        self.status = None;
                    }
                }
            },
            View::Name | View::Server => match key {
                Keycode::Escape => self.back(),
                Keycode::Return | Keycode::KpEnter => self.commit_field(now),
                _ => {
                    self.field.key(key, mods);
                }
            },
        }
        AppAction::None
    }

    fn text(&mut self, text: &str, ctx: &Ctx) -> AppAction {
        match self.view {
            // Typing in the list of rooms writes in the room picked there.
            View::Rooms if !text.trim().is_empty() && !self.rooms.is_empty() => {
                self.open_room(self.room_sel, ctx.now.timestamp());
                self.start_writing(text);
            }
            View::Timeline => {
                if !text.trim().is_empty() {
                    self.start_writing(text);
                }
            }
            View::Write if !self.sending => {
                let room = net::MAX_TEXT.saturating_sub(self.draft.len());
                let t: String = text.chars().take(room).collect();
                self.draft.insert(&t);
            }
            View::Name if self.field.len() < net::MAX_NAME => self.field.insert(text),
            View::Server if self.field.len() < 300 => self.field.insert(text),
            _ => {}
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> AppAction {
        // The toolbar: a button acts when the press that began on it ends on it.
        match ev {
            PointerEvent::Down { x, y, button: 0, .. } if y >= bar_y() && y <= bar_rule_y() => {
                self.tool_down = self.tool_at(x, y);
                return AppAction::None;
            }
            PointerEvent::Up { x, y, .. } if self.tool_down.is_some() => {
                let a = self.tool_down.take();
                if a.is_some() && self.tool_at(x, y) == a {
                    self.act(a.unwrap(), ctx.now.timestamp());
                }
                return AppAction::None;
            }
            PointerEvent::Drag { x, y, .. } if self.tool_down.is_some() && self.tool_at(x, y) != self.tool_down => {
                self.tool_down = None;
                return AppAction::None;
            }
            _ => {}
        }
        let rooms = self.view == View::Rooms;
        let (len, top) = if rooms { (self.rooms.len(), self.room_top) } else { (self.posts.len(), self.top) };
        let bar = ScrollBar::new(list_bar_rect(), len, entries_shown(), top);
        match (self.view, ev) {
            (View::Rooms, PointerEvent::Wheel { dy, .. }) => self.step_room(if dy > 0 { -1 } else { 1 }),
            (View::Timeline, PointerEvent::Wheel { dy, .. }) => self.step(if dy > 0 { -1 } else { 1 }),
            (View::Read, PointerEvent::Wheel { dy, .. }) => self.scroll_read(if dy > 0 { -2 } else { 2 }),
            (View::Rooms | View::Timeline, PointerEvent::Down { x, y, button: 0, clicks }) => {
                if let Some((off, drag)) = bar.press(x, y) {
                    if rooms { self.room_top = off } else { self.top = off }
                    self.sb_drag = drag;
                } else if y >= page_y() && y < page_y() + entries_shown() as i32 * ENTRY_H {
                    let i = top + ((y - page_y()) / ENTRY_H) as usize;
                    if i < len {
                        let again = i == if rooms { self.room_sel } else { self.sel };
                        if rooms {
                            self.room_sel = i;
                            if again || clicks >= 2 {
                                self.open_room(i, ctx.now.timestamp());
                            }
                        } else {
                            self.sel = i;
                            if again || clicks >= 2 {
                                self.open_read();
                            }
                        }
                    }
                }
            }
            (View::Rooms | View::Timeline, PointerEvent::Drag { y, .. }) if self.sb_drag => {
                let off = bar.offset_for_y(y);
                let last = off + entries_shown() - 1;
                if rooms {
                    self.room_top = off;
                    self.room_sel = self.room_sel.clamp(off, last);
                } else {
                    self.top = off;
                    self.sel = self.sel.clamp(off, last);
                }
            }
            (_, PointerEvent::Up { .. }) => self.sb_drag = false,
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, ctx: &Ctx) -> bool {
        let now = ctx.now.timestamp();
        let mut dirty = self.poll();
        if !self.is_demo() {
            let due = |t: Option<i64>| t.is_none_or(|t| now - t >= AUTO_RELOAD);
            if self.view == View::Rooms && !self.loading_rooms && due(self.fetched_rooms) {
                self.reload_rooms(now);
                dirty = true;
            } else if self.view == View::Timeline && !self.loading && due(self.fetched_posts) {
                self.reload_posts(now);
                dirty = true;
            }
        }
        let wait = if self.view == View::Write { self.settings.wait(now) } else { 0 };
        if wait != self.shown_wait {
            self.shown_wait = wait;
            dirty = true;
        }
        dirty
    }

    fn draw(&mut self, c: &mut Canvas, _assets: &mut AssetStore, ctx: &Ctx) {
        let tm = ctx.tm;
        let p = Paint {
            ink: tm.get_color("foreground"),
            faint: widgets::secondary_ink(tm),
            page: tm.get_color("home_bg"),
            sel: widgets::selection(tm),
        };
        ui::fill_rect(c, Rect::new(0, grid::BAR_H, grid::sw() as u32, (grid::sh() - grid::BAR_H) as u32), p.page);
        let now = ctx.now.timestamp();
        match self.view {
            View::Rooms => self.draw_rooms(c, ctx, &p, now),
            View::Timeline => self.draw_timeline(c, ctx, &p, now),
            View::Read => self.draw_read(c, ctx, &p, now),
            View::Write => self.draw_write(c, ctx, &p, now),
            View::Name | View::Server => self.draw_field(c, ctx, &p),
        }
        // The rule under the head row.
        ui::line(c, 0, page_y() - 1, grid::sw() - 1, page_y() - 1, p.ink);
        self.draw_toolbar(c, ctx, &p);
    }
}

struct Paint {
    ink: Color,
    faint: Color,
    page: Color,
    sel: widgets::Selection,
}

fn text_y(row_y: i32) -> i32 {
    widgets::text_origin(row_y, ROW)
}

impl HarbourTalk {
    /// The head row: `left` in bold, `right` faint at the right edge.
    fn draw_head(&self, c: &mut Canvas, ctx: &Ctx, p: &Paint, left: &str, right: &str) {
        let fs = ctx.fs;
        let y = text_y(head_y());
        let rw = fs.measure_text(right);
        let right_x = grid::sw() - LEFT - rw;
        let left = fit(left, right_x - LEFT - 8, &|t| fs.measure_text(t));
        fs.draw_text_bold(c, LEFT, y, &left, p.ink);
        fs.draw_text(c, right_x, y, right, p.faint);
    }

    fn draw_toolbar(&self, c: &mut Canvas, ctx: &Ctx, p: &Paint) {
        ui::line(c, 0, bar_rule_y(), grid::sw() - 1, bar_rule_y(), p.ink);
        let bar = self.toolbar();
        for (r, (a, on, default)) in tool_rects(bar.len()).into_iter().zip(bar) {
            let st = widgets::BtnState { pressed: self.tool_down == Some(a), default: false, disabled: !on, hover: false };
            widgets::draw_button(c, ctx.fs, ctx.tm, r, a.label(), st);
            // The default button (what Enter does) is underlined: the heavy ring would not fit.
            if default && on {
                ui::line(c, r.x() + 6, r.bottom() - 3, r.right() - 7, r.bottom() - 3, if self.tool_down == Some(a) { ctx.tm.get_color("icon_white") } else { p.ink });
            }
        }
    }

    fn status_text(&self) -> Option<String> {
        self.status.as_deref().map(tr_s)
    }

    /// `name#3fa2`, or `#3fa2` before a name is set.
    fn me(&self) -> String {
        format!("{}#{}", self.settings.name, net::short_dev(&self.settings.dev))
    }

    /// Two-row entries (title in bold with a faint note at the right, then a line of text) with
    /// the selected one on a plate, dotted rules between and a scroll bar when they do not fit.
    fn draw_entries(&self, c: &mut Canvas, ctx: &Ctx, p: &Paint, rows: &[(String, String, String)], sel: usize, top: usize) {
        let fs = ctx.fs;
        let right = text_right();
        let measure = |t: &str| fs.measure_text(t);
        for (k, (title, note, body)) in rows.iter().enumerate().skip(top).take(entries_shown()) {
            let y = page_y() + (k - top) as i32 * ENTRY_H;
            let selected = k == sel;
            let (ink, faint) = if selected { (p.sel.fg, p.sel.fg) } else { (p.ink, p.faint) };
            if selected {
                ui::fill_rect(c, Rect::new(2, y, (right + 2) as u32, (ENTRY_H - 1) as u32), p.sel.bg);
            }
            let ww = fs.measure_text(note);
            let title = fit(title, right - LEFT - ww - 8, &measure);
            let (ty, by) = (widgets::text_origin(y, 14), widgets::text_origin(y + 13, 14));
            fs.draw_text_bold(c, LEFT, ty, &title, ink);
            fs.draw_text(c, right - ww, ty, note, faint);
            let body = fit(body, right - LEFT - 8, &measure);
            fs.draw_text(c, LEFT + 8, by, &body, ink);
            if !selected && k + 1 != sel && k + 1 < top + entries_shown() && k + 1 < rows.len() {
                for x in (LEFT..right).step_by(2) {
                    ui::fill_rect(c, Rect::new(x, y + ENTRY_H - 1, 1, 1), p.faint);
                }
            }
        }
        let bar = ScrollBar::new(list_bar_rect(), rows.len(), entries_shown(), top);
        if bar.needed() {
            bar.draw(c, ctx.tm, None);
        }
    }

    fn draw_rooms(&self, c: &mut Canvas, ctx: &Ctx, p: &Paint, now: i64) {
        let left = match self.status_text() {
            Some(s) => s,
            None if self.is_demo() => tr("Demo board").to_string(),
            None => tr("Public rooms").to_string(),
        };
        self.draw_head(c, ctx, p, &left, &self.me());
        if self.rooms.is_empty() {
            if !self.loading_rooms {
                ctx.fs.draw_text(c, LEFT, text_y(page_y() + 4), tr("No rooms on this board."), p.faint);
            }
            return;
        }
        let rows: Vec<(String, String, String)> = self
            .rooms
            .iter()
            .map(|r| {
                let note = if r.last > 0 { ago(now, r.last) } else { String::new() };
                let count = match r.count {
                    0 => tr("No posts yet.").to_string(),
                    1 => tr("1 post").to_string(),
                    n => tr_s(&format!("{n} posts")),
                };
                (format!("# {}", self.room_name(&r.id)), note, count)
            })
            .collect();
        self.draw_entries(c, ctx, p, &rows, self.room_sel, self.room_top);
    }

    fn draw_timeline(&self, c: &mut Canvas, ctx: &Ctx, p: &Paint, now: i64) {
        let left = match self.status_text() {
            Some(s) => s,
            None => format!("# {}", self.room_name(&self.room)),
        };
        self.draw_head(c, ctx, p, &left, &self.me());
        if self.posts.is_empty() {
            if !self.loading {
                ctx.fs.draw_text(c, LEFT, text_y(page_y() + 4), tr("No posts yet."), p.faint);
                ctx.fs.draw_text(c, LEFT, text_y(page_y() + 4 + ROW), tr("Type to write the first one."), p.faint);
            }
            return;
        }
        let rows: Vec<(String, String, String)> = self.posts.iter().map(|q| (q.author(), ago(now, q.time), q.text.clone())).collect();
        self.draw_entries(c, ctx, p, &rows, self.sel, self.top);
    }

    fn draw_read(&mut self, c: &mut Canvas, ctx: &Ctx, p: &Paint, now: i64) {
        let fs = ctx.fs;
        let Some(post) = self.posts.get(self.sel).cloned() else {
            self.view = View::Timeline;
            return;
        };
        let right = format!("{}  {}/{}", ago(now, post.time), self.sel + 1, self.posts.len());
        self.draw_head(c, ctx, p, &post.author(), &right);
        let measure = |t: &str| fs.measure_text(t);
        let lines = page_layout::wrap_text(&Metrics::fixed(6, 12), &post.text, text_right() - LEFT, 0, &measure);
        self.read_len = lines.len();
        self.scroll_read(0);
        let shown = read_lines_shown();
        for (i, l) in lines.iter().skip(self.read_top).take(shown).enumerate() {
            fs.draw_text(c, LEFT, page_y() + 3 + i as i32 * LINE_H, l, p.ink);
        }
        let bar = ScrollBar::new(
            Rect::new(grid::scroll_x(), page_y() + 2, grid::SCROLL_W as u32, (shown as i32 * LINE_H) as u32),
            lines.len(),
            shown,
            self.read_top,
        );
        if bar.needed() {
            bar.draw(c, ctx.tm, None);
        }
    }

    fn draw_write(&self, c: &mut Canvas, ctx: &Ctx, p: &Paint, now: i64) {
        let fs = ctx.fs;
        let count = format!("{}/{}", self.draft.len(), net::MAX_TEXT);
        let room = format!("# {}", self.room_name(&self.room));
        self.draw_head(c, ctx, p, &tr_s(&format!("New post in {room}")), &count);

        // The box, then the hint row under it.
        let hint_y = content_bottom() + 1 - ROW;
        let bx = Rect::new(2, page_y() + 2, (grid::sw() - 4) as u32, (hint_y - page_y() - 3) as u32);
        ui::fill_rect(c, bx, ctx.tm.get_color("surface_sunken"));
        ui::draw_rect(c, bx, p.ink);
        let measure = |t: &str| fs.measure_text(t);
        let lines = wrap_chars(&self.draft.text, bx.width() as i32 - 10, &measure);
        let room = ((bx.height() as i32 - 4) / LINE_H).max(1) as usize;
        let (cl, cc) = caret_line(&lines, self.draft.caret);
        let first = (cl + 1).saturating_sub(room);
        for (i, (_, l)) in lines.iter().skip(first).take(room).enumerate() {
            fs.draw_text(c, bx.x() + 5, bx.y() + 2 + i as i32 * LINE_H, l, p.ink);
        }
        if !self.sending {
            let before: String = lines[cl].1.chars().take(cc).collect();
            let x = bx.x() + 5 + fs.measure_text(&before);
            let y = bx.y() + 3 + (cl - first) as i32 * LINE_H;
            ui::fill_rect(c, Rect::new(x, y, 1, (LINE_H - 2) as u32), p.ink);
        }
        if self.draft.is_empty() {
            fs.draw_text(c, bx.x() + 8, bx.y() + 2, tr("What's happening in the harbour?"), p.faint);
        }

        let wait = self.settings.wait(now);
        let st = self.status.as_deref();
        let hint = if self.sending {
            tr("Sending...").to_string()
        } else if wait > 0 && st.is_none_or(|s| s.starts_with("Wait")) {
            tr_s(&format!("Wait {wait} s to post again"))
        } else if let Some(s) = st.filter(|s| !s.starts_with("Wait")) {
            tr_s(s)
        } else {
            tr_s(&format!("Posting as {}", self.me()))
        };
        fs.draw_text(c, LEFT, text_y(hint_y), &hint, p.faint);
    }

    fn draw_field(&self, c: &mut Canvas, ctx: &Ctx, p: &Paint) {
        let fs = ctx.fs;
        let (title, label, hint) = if self.view == View::Name {
            ("Handle name", "Your name on the board (up to 16):", "Shown with this device's id, like name#3fa2.")
        } else {
            ("Server", "Board server address:", "Empty: the standard board. demo: this device only.")
        };
        self.draw_head(c, ctx, p, tr(title), "");
        fs.draw_text(c, LEFT, text_y(page_y() + 2), tr(label), p.ink);
        let r = Rect::new(4, page_y() + 2 + ROW, (grid::sw() - 8) as u32, widgets::FIELD_H as u32);
        widgets::draw_field(c, fs, ctx.tm, r, &widgets::Field { text: &self.field.text, caret: Some(self.field.caret), ..Default::default() });
        let line = self.status_text().unwrap_or_else(|| tr(hint).to_string());
        fs.draw_text(c, LEFT, text_y(page_y() + 4 + 2 * ROW), &line, p.faint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_parts() -> (crate::font::FontSystem, crate::theme::ThemeManager, crate::system::mock::MockBackend) {
        use crate::theme::{Theme, ThemeManager};
        (
            crate::font::FontSystem::new(),
            ThemeManager::new(Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap()),
            crate::system::mock::MockBackend::new(),
        )
    }

    /// Run the worker's replies (demo board: they are already there).
    fn settle(a: &mut HarbourTalk) {
        for _ in 0..200 {
            a.poll();
            if !a.loading && !a.sending && !a.loading_rooms {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the worker never answered");
    }

    fn app(tag: &str) -> (HarbourTalk, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("harbour-talk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = HarbourTalk::new(Store { dir: Some(dir.clone()) }, true);
        a.reload_rooms(1_800_000_000);
        settle(&mut a);
        a.open_room(0, 1_800_000_000);
        settle(&mut a);
        (a, dir)
    }

    #[test]
    fn times_read_short() {
        assert_eq!(ago(1000, 990), "now");
        assert_eq!(ago(1000, 1000 - 5 * 60), "5m");
        assert_eq!(ago(100_000, 100_000 - 3 * 3600), "3h");
        assert_eq!(ago(1_000_000, 1_000_000 - 2 * 86_400), "2d");
    }

    #[test]
    fn the_draft_wraps_and_the_caret_finds_its_line() {
        let m = |t: &str| t.chars().count() as i32 * 6;
        let lines = wrap_chars("abcdefghij", 24, &m);
        assert_eq!(lines, [(0, "abcd".to_string()), (4, "efgh".to_string()), (8, "ij".to_string())]);
        assert_eq!(caret_line(&lines, 0), (0, 0));
        assert_eq!(caret_line(&lines, 5), (1, 1));
        assert_eq!(caret_line(&lines, 10), (2, 2));
        assert_eq!(wrap_chars("", 24, &m), [(0, String::new())]);
        let words = wrap_chars("ab cdef gh", 36, &m);
        assert_eq!(words, [(0, "ab ".to_string()), (3, "cdef ".to_string()), (8, "gh".to_string())], "words go down whole");
        assert_eq!(caret_line(&words, 4), (1, 1));
        let joined: String = words.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(joined, "ab cdef gh", "nothing lost");
        assert_eq!(fit("abcdefgh", 30, &m), "abcd…");
        assert_eq!(fit("abc", 30, &m), "abc");
    }

    #[test]
    fn a_mock_run_shows_the_demo_board_and_keeps_a_device_id() {
        let (a, dir) = app("demo");
        assert!(a.is_demo());
        assert_eq!(a.rooms.len(), 4);
        assert_eq!((a.room.as_str(), a.view), ("lobby", View::Timeline));
        assert_eq!(a.posts.len(), 4);
        assert!(a.posts.iter().all(|p| p.room == "lobby"));
        assert_eq!(a.settings.dev.len(), 8);
        let b = HarbourTalk::new(Store { dir: Some(dir.clone()) }, true);
        assert_eq!(b.settings.dev, a.settings.dev, "the id is made once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn typing_asks_for_a_name_first_then_posts_to_the_top() {
        let (fs, tm, sys) = ctx_parts();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let (mut a, dir) = app("post");
        let none = Mod::empty();
        a.text("k", &ctx);
        assert_eq!(a.view, View::Name, "no handle name yet");
        a.text("uro", &ctx);
        a.key(Keycode::Return, none, false, &ctx);
        assert_eq!(a.settings.name, "kuro");
        assert_eq!(a.view, View::Write);
        assert!(a.draft.is_empty(), "the letters went into the name");
        a.key(Keycode::Return, none, false, &ctx);
        assert_eq!(a.status.as_deref(), Some("Write something first"));
        a.text("こんにちは", &ctx);
        a.key(Keycode::Return, none, false, &ctx);
        settle(&mut a);
        assert_eq!(a.view, View::Timeline);
        assert_eq!(a.posts[0].text, "こんにちは");
        assert_eq!(a.posts[0].author(), format!("kuro#{}", &a.settings.dev[..4]));
        assert!(a.draft.is_empty());
        // Too soon for another.
        a.text("again", &ctx);
        assert_eq!(a.view, View::Write);
        a.key(Keycode::Return, none, false, &ctx);
        assert!(a.status.as_deref().unwrap().starts_with("Wait"));
        assert_eq!(a.draft.text, "again", "the draft stays");
        // Esc keeps the draft; typing again goes on from it.
        a.key(Keycode::Escape, none, false, &ctx);
        assert_eq!(a.view, View::Timeline);
        a.text("!", &ctx);
        assert_eq!(a.draft.text, "again!");
        // A reload keeps what was written on the demo board.
        a.key(Keycode::Escape, none, false, &ctx);
        a.reload(ctx.now.timestamp());
        settle(&mut a);
        assert!(a.posts.iter().any(|p| p.text == "こんにちは"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn posts_are_capped_at_two_hundred_characters() {
        let (fs, tm, sys) = ctx_parts();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let (mut a, dir) = app("cap");
        a.settings.name = "kuro".into();
        a.text(&"あ".repeat(150), &ctx);
        a.text(&"い".repeat(150), &ctx);
        assert_eq!(a.draft.len(), net::MAX_TEXT);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reading_moves_through_the_posts_and_back() {
        let (fs, tm, sys) = ctx_parts();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let (mut a, dir) = app("read");
        let none = Mod::empty();
        a.key(Keycode::Down, none, false, &ctx);
        assert_eq!(a.sel, 1);
        a.key(Keycode::Return, none, false, &ctx);
        assert_eq!(a.view, View::Read);
        a.key(Keycode::Right, none, false, &ctx);
        assert_eq!(a.sel, 2);
        a.key(Keycode::Escape, none, false, &ctx);
        assert_eq!(a.view, View::Timeline);
        for _ in 0..20 {
            a.key(Keycode::Down, none, false, &ctx);
        }
        assert_eq!(a.sel, a.posts.len() - 1);
        assert!(a.sel >= a.top && a.sel < a.top + entries_shown(), "the list follows the selection");
        assert_eq!(a.key(Keycode::Escape, none, false, &ctx), AppAction::None);
        assert_eq!(a.view, View::Rooms, "Esc in a room: back to the rooms");
        assert_eq!(a.key(Keycode::Escape, none, false, &ctx), AppAction::Quit);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_server_field_checks_the_address() {
        let (fs, tm, sys) = ctx_parts();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let dir = std::env::temp_dir().join(format!("harbour-talk-server-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = HarbourTalk::new(Store { dir: Some(dir.clone()) }, false);
        assert!(!a.is_demo(), "no server set: the standard board");
        a.menu_action("server", &ctx);
        a.text("not a url", &ctx);
        a.key(Keycode::Return, Mod::empty(), false, &ctx);
        assert_eq!(a.view, View::Server);
        assert_eq!(a.status.as_deref(), Some("Not a web address"));
        a.field = LineEdit::new("http://127.0.0.1:9/");
        a.key(Keycode::Return, Mod::empty(), false, &ctx);
        assert_eq!(a.view, View::Rooms);
        assert!(!a.is_demo());
        assert_eq!(Store { dir: Some(dir.clone()) }.load_settings().server, "http://127.0.0.1:9/");
        settle(&mut a);
        assert!(a.status.is_some(), "nothing listens there: an error, not a crash");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rooms_keep_their_own_posts_and_typing_in_the_list_writes_in_the_picked_room() {
        let (fs, tm, sys) = ctx_parts();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let (mut a, dir) = app("rooms");
        let none = Mod::empty();
        a.settings.name = "kuro".into();
        a.key(Keycode::Escape, none, false, &ctx);
        assert_eq!(a.view, View::Rooms);
        a.key(Keycode::Down, none, false, &ctx);
        a.key(Keycode::Down, none, false, &ctx);
        assert_eq!(a.rooms[a.room_sel].id, "music");
        let before = a.rooms[a.room_sel].count;
        a.text("新曲", &ctx);
        assert_eq!((a.view, a.room.as_str()), (View::Write, "music"));
        settle(&mut a);
        a.key(Keycode::Return, none, false, &ctx);
        settle(&mut a);
        assert_eq!(a.view, View::Timeline);
        assert_eq!((a.posts[0].room.as_str(), a.posts[0].text.as_str()), ("music", "新曲"));
        assert_eq!(a.rooms[a.room_sel].count, before + 1, "the room's row counts it");
        // Another room does not show it; coming back (and reloading) does.
        a.key(Keycode::Escape, none, false, &ctx);
        a.key(Keycode::Up, none, false, &ctx);
        a.key(Keycode::Return, none, false, &ctx);
        settle(&mut a);
        assert_eq!(a.room, "cardputer");
        assert!(a.posts.iter().all(|p| p.text != "新曲"));
        a.key(Keycode::Escape, none, false, &ctx);
        a.key(Keycode::Down, none, false, &ctx);
        a.key(Keycode::Return, none, false, &ctx);
        settle(&mut a);
        assert_eq!(a.posts[0].text, "新曲");
        a.reload_rooms(ctx.now.timestamp());
        settle(&mut a);
        assert_eq!(a.rooms[2].count, before + 1, "a reload of the demo board keeps the count");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_toolbar_fits_the_screen_and_a_desk_window() {
        for (w, h) in [(320, 170), (800, 480)] {
            crate::grid::with_surface((w, h), || {
                for n in 1..=4 {
                    let rs = tool_rects(n);
                    for r in &rs {
                        assert!(r.x() >= 2 && r.right() <= w - 2, "{w}x{h} {n}: {r:?}");
                        assert!(r.y() > bar_y() && r.bottom() < bar_rule_y(), "{w}x{h} {n}: {r:?}");
                        assert!(r.height() >= 19, "big enough for a finger");
                        assert!(r.width() as i32 >= 70 && r.width() as i32 <= TOOL_MAX_W);
                    }
                    assert!(rs.windows(2).all(|p| p[0].right() < p[1].x()), "no overlap");
                }
                assert!(head_y() > bar_rule_y(), "the head row is under the toolbar");
                assert!(page_y() + entries_shown() as i32 * ENTRY_H <= page_bottom() + 1, "the list fits the page");
                if (w, h) == (320, 170) {
                    assert_eq!(entries_shown(), 4);
                }
            });
        }
    }

    #[test]
    fn toolbar_buttons_do_what_the_keys_do() {
        let (fs, tm, sys) = ctx_parts();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let (mut a, dir) = app("toolbar");
        a.settings.name = "kuro".into();
        let click = |a: &mut HarbourTalk, act: Act| {
            let bar = a.toolbar();
            let i = bar.iter().position(|(b, _, _)| *b == act).unwrap_or_else(|| panic!("{act:?} not on {:?}", a.view));
            let r = tool_rects(bar.len())[i];
            let (x, y) = (r.x() + 5, r.y() + 5);
            a.pointer(PointerEvent::Down { x, y, button: 0, clicks: 1 }, &ctx);
            a.pointer(PointerEvent::Up { x, y, button: 0 }, &ctx);
            settle(a);
        };
        click(&mut a, Act::Rooms);
        assert_eq!(a.view, View::Rooms);
        a.step_room(1);
        click(&mut a, Act::Open);
        assert_eq!((a.view, a.room.as_str()), (View::Timeline, "cardputer"));
        click(&mut a, Act::Read);
        assert_eq!(a.view, View::Read);
        assert!(!a.toolbar()[1].1, "Previous is off on the newest post");
        click(&mut a, Act::Next);
        assert_eq!(a.sel, 1);
        click(&mut a, Act::Write);
        assert_eq!(a.view, View::Write);
        a.text("ボタンから", &ctx);
        click(&mut a, Act::Post);
        assert_eq!(a.view, View::Timeline);
        assert_eq!(a.posts[0].text, "ボタンから");
        click(&mut a, Act::Rooms);
        click(&mut a, Act::Name);
        assert_eq!(a.view, View::Name);
        click(&mut a, Act::Back);
        assert_eq!(a.view, View::Rooms);
        // A press that slides off its button does nothing.
        let r = tool_rects(4)[2];
        a.pointer(PointerEvent::Down { x: r.x() + 3, y: r.y() + 3, button: 0, clicks: 1 }, &ctx);
        a.pointer(PointerEvent::Drag { x: r.x() - 40, y: r.y() + 3, dx: -43, dy: 0 }, &ctx);
        a.pointer(PointerEvent::Up { x: r.x() - 40, y: r.y() + 3, button: 0 }, &ctx);
        assert_eq!(a.view, View::Rooms);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
