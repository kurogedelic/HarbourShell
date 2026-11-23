//! Browser: a small graphical, text-first web browser (no JavaScript, almost no CSS).
//!
//! * Pages are fetched by `curl` in a worker thread, parsed and laid out there too
//!   (see `browser/`), and only the visible lines are drawn. Esc cancels a load.
//! * Keys: Up/Down scroll, Space / Shift+Space (or PageDown/PageUp) page, Home/End, Tab /
//!   Shift+Tab or Left/Right (Z/C) = next / previous link or field, Enter = follow / edit /
//!   press, Backspace = back, Shift+Backspace = forward, `/` or Ctrl+L = address bar,
//!   Ctrl+R or F5 = reload, Esc = stop loading / leave a text field. Alt = menu.
//! * Mouse: click links, buttons and fields, wheel scrolls, drag the scroll bar.
//! * `about:home`, `about:bookmarks`, `about:history`, `about:help` are built in, and
//!   `file://` works, so it is usable offline. `HARBOUR_MOCK=1` serves the fixtures in
//!   `tests/fixtures/` instead of the network.
//! * Menu: Reload, Back, Forward, Bookmarks (add / remove / list), History, Home,
//!   Address bar, Open in Lynx (the old behaviour).

pub mod bookmarks;
pub mod charset;
pub mod fetch;
pub mod html;
pub mod layout;
pub mod load;
pub mod url;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use sdl2::image::{InitFlag, Sdl2ImageContext};
use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome;
use crate::grid;
use crate::launcher::i18n;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::apps::imageviewer;
use crate::launcher::apps::tone;
use crate::launcher::lineedit::LineEdit;
use crate::launcher::widgets::{self, contains, BtnState, Dropdown, MenuItem, PointerEvent, ProgressBar, SbPart, ScrollBar};
use crate::platform::keys::arrow_alias;
use crate::ui;

use bookmarks::{Entry, NamedList};
use fetch::{Fetcher, Req};
use layout::{FieldKind, Focus, ItemKind, Metrics, Page, Request, LINE_H};
use load::Loaded;

const HISTORY_MAX: usize = 100;
const NAV_MAX: usize = 60;
const LYNX_DIRS: [&str; 4] = ["/usr/bin", "/usr/local/bin", "/bin", "/opt/homebrew/bin"];
/// Characters that must not reach the launcher's `Exec` validation raw.
const UNSAFE: &[char] = &[';', '|', '$', '`', '\\', '"', '\'', '(', ')', '{', '}', '[', ']', '*', '!', '~', '<', '>', '%'];

// ---- geometry (all on the 4 px grid; the content area is y 16..160)
const TOOL_Y: i32 = grid::BAR_H;
/// The page sits in the app frame (`grid::draw_app_frame`, square corners, down to y 168). It
/// starts 1 px below the toolbar (y 31) when the toolbar is shown, else 1 px below the menu strip
/// (y 17, like every framed app).
const FRAME_TOP_BAR: i32 = TOOL_Y + grid::ROW_H + 1;
const FRAME_TOP: i32 = grid::BAR_H + 1;
/// The page fills the frame's inside left of the scroll bar (which keeps 1 px to the page and the line).
const VIEW_X: i32 = 3;
const VIEW_W: i32 = grid::SCROLL_X - 1 - VIEW_X;
const PAGE_X: i32 = VIEW_X + 2;
/// Space above the first line.
const PAGE_Y: i32 = 2;
const SCROLL_LINE: i32 = LINE_H;
/// Toolbar buttons: square-ish icon buttons (back, forward, go / stop), then the address field.
const BTN_W: i32 = 20;
fn back_r() -> Rect { Rect::new(grid::MARGIN, TOOL_Y, BTN_W as u32, grid::BTN_H_SM as u32) }
fn fwd_r() -> Rect { Rect::new(grid::MARGIN + BTN_W + 2, TOOL_Y, BTN_W as u32, grid::BTN_H_SM as u32) }
fn reload_r() -> Rect { Rect::new(grid::MARGIN + 2 * (BTN_W + 2), TOOL_Y, BTN_W as u32, grid::BTN_H_SM as u32) }
const ADDR_X: i32 = grid::MARGIN + 3 * (BTN_W + 2) + 2;
fn addr_r() -> Rect { Rect::new(ADDR_X, TOOL_Y, (grid::SCROLL_X + grid::SCROLL_W - ADDR_X) as u32, grid::BTN_H_SM as u32) }

/// 9 x 9 toolbar glyphs ('#' = ink), drawn centred on their buttons.
const ICON_BACK: [&str; 9] = [
    "....#....",
    "...##....",
    "..###....",
    ".########",
    "#########",
    ".########",
    "..###....",
    "...##....",
    "....#....",
];
const ICON_FORWARD: [&str; 9] = [
    "....#....",
    "....##...",
    "....###..",
    "########.",
    "#########",
    "########.",
    "....###..",
    "....##...",
    "....#....",
];
/// Go / reload: a circular arrow.
const ICON_GO: [&str; 9] = [
    "..####.#.",
    ".##..####",
    "##....###",
    "##...####",
    "##.......",
    "##.....##",
    "##.....##",
    ".##...##.",
    "..#####..",
];
/// Stop: a cross.
const ICON_STOP: [&str; 9] = [
    "##.....##",
    "###...###",
    ".###.###.",
    "..#####..",
    "...###...",
    "..#####..",
    ".###.###.",
    "###...###",
    "##.....##",
];

// ------------------------------------------------------------------------------ pure helpers

/// Find `lynx` as an absolute path (the host only runs absolute programs).
pub fn find_lynx(exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    LYNX_DIRS.iter().map(|d| Path::new(d).join("lynx")).find(|p| exists(p))
}

/// The command for `AppAction::Launch`: unsafe characters are percent-encoded.
pub fn launch_command(url: &str, lynx: &Path) -> String {
    let mut out = String::new();
    for ch in url.chars() {
        if (UNSAFE.contains(&ch) && ch != '%') || ch.is_control() || ch == ' ' {
            out.push_str(&format!("%{:02X}", ch as u32));
        } else {
            out.push(ch);
        }
    }
    format!("{} {}", lynx.display(), out)
}

pub fn normalize(input: &str) -> Option<String> {
    url::normalize(input)
}

fn link_list(items: &[Entry]) -> String {
    let mut s = String::from("<ul>");
    for e in items {
        let label = if e.title.trim().is_empty() { url::display(&e.url) } else { e.title.clone() };
        s.push_str(&format!("<li><a href=\"{}\">{}</a></li>", load::escape(&e.url), load::escape(&label)));
    }
    s.push_str("</ul>");
    s
}

pub fn home_html(bookmarks: &NamedList) -> String {
    let mut s = String::from("<title>Harbour Browser</title><h2>Harbour Browser</h2><form action=\"https://lite.duckduckgo.com/lite/\" method=\"get\"><input type=\"text\" name=\"q\" size=\"28\"> <input type=\"submit\" value=\"Search\"></form><h3>Bookmarks</h3>");
    if bookmarks.entries.is_empty() {
        s.push_str("<p>No bookmarks yet. Menu (Alt) &gt; Add bookmark.</p>");
    } else {
        s.push_str(&link_list(&bookmarks.entries[..bookmarks.len().min(12)]));
    }
    s.push_str("<p><a href=\"about:history\">History</a> | <a href=\"about:bookmarks\">All bookmarks</a> | <a href=\"about:help\">Help</a></p>");
    s
}

pub fn bookmarks_html(bookmarks: &NamedList) -> String {
    let mut s = String::from("<title>Bookmarks</title><h2>Bookmarks</h2>");
    if bookmarks.entries.is_empty() {
        s.push_str("<p>None yet.</p>");
    } else {
        s.push_str(&link_list(&bookmarks.entries));
    }
    s
}

pub fn history_html(history: &NamedList) -> String {
    let mut s = String::from("<title>History</title><h2>History</h2>");
    if history.entries.is_empty() {
        s.push_str("<p>Nothing visited yet.</p>");
    } else {
        s.push_str(&link_list(&history.entries));
    }
    s
}

pub fn help_html() -> String {
    "<title>Help</title><h2>Keys</h2><dl>\
     <dt>Up / Down</dt><dd>scroll a line</dd>\
     <dt>Space, Shift+Space</dt><dd>page down / up</dd>\
     <dt>Tab, Shift+Tab</dt><dd>next / previous link (Left / Right too)</dd>\
     <dt>Enter</dt><dd>open the link, edit the field, press the button</dd>\
     <dt>Backspace</dt><dd>back (Shift+Backspace: forward)</dd>\
     <dt>/ or Ctrl+L</dt><dd>address bar: a URL or search words (the toolbar shows until Enter or Esc; Menu &gt; Show Toolbar keeps it)</dd>\
     <dt>Ctrl+R, F5</dt><dd>reload</dd>\
     <dt>R</dt><dd>reader view: just the article (kept per site)</dd>\
     <dt>Esc</dt><dd>stop loading, leave a field</dd>\
     <dt>Alt</dt><dd>menu: bookmarks, history, Lynx</dd></dl>\
     <p>Mouse: click, wheel, drag the scroll bar; click the top row to type an address.</p><p><a href=\"about:home\">Home</a></p>"
        .to_string()
}

fn about_html(url: &str, bookmarks: &NamedList, history: &NamedList) -> String {
    match url::strip_fragment(url) {
        "about:bookmarks" => bookmarks_html(bookmarks),
        "about:history" => history_html(history),
        "about:help" => help_html(),
        "about:home" | "about:" | "about:blank" => home_html(bookmarks),
        other => load::error_html(other, "Unknown about: page"),
    }
}

fn mem_available_mb() -> Option<u64> {
    let t = std::fs::read_to_string("/proc/meminfo").ok()?;
    let l = t.lines().find(|l| l.starts_with("MemAvailable:"))?;
    Some(l.split_whitespace().nth(1)?.parse::<u64>().ok()? / 1024)
}

fn config_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".config/harbour"))
}

// ------------------------------------------------------------------------------ state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    None,
    Address,
    Field(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Push,
    Replace,
    Goto(usize),
    Reload,
    /// Same page, other layout (reader mode): no history change.
    View,
}

enum Msg {
    Partial(Box<Page>),
    Done(Result<Loaded, String>),
}

struct Nav {
    url: String,
    scroll: i32,
}

struct Loading {
    url: String,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU32>,
    rx: Receiver<Msg>,
    mode: Mode,
    reader: bool,
    redirects: u8,
}

enum Img {
    Pending,
    Failed,
    Ready { w: i32, h: i32, runs: tone::ToneRuns },
}

struct ImgJob {
    url: String,
    max: (i32, i32),
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<fetch::Resp, String>>,
}

pub struct Browser {
    fetcher: Arc<dyn Fetcher>,
    persist: bool,
    _img: Option<Sdl2ImageContext>,
    started: bool,
    /// The first page to load: the home page, or a web address another app handed over
    /// (Infoscope's news and web search, via `AppAction::Open { app: Some("Browser"), path: url }`).
    first: String,
    page: Option<Page>,
    scroll: i32,
    sel: Option<usize>,
    edit: Edit,
    addr: String,
    caret: usize,
    fresh: bool,
    swallow: Option<char>,
    hist_pos: Option<usize>,
    nav: Vec<Nav>,
    pos: usize,
    loading: Option<Loading>,
    status: String,
    /// "Page info" card open over the view.
    info: bool,
    frame: u32,
    bookmarks: NamedList,
    history: NamedList,
    images: HashMap<String, Img>,
    img_job: Option<ImgJob>,
    viewer: Option<(String, Option<Result<(i32, i32, tone::ToneRuns), String>>)>,
    hover: Option<Focus>,
    pressed: u8,
    /// The toolbar stays shown (the user's choice in the menu, remembered); else it only shows while
    /// the address is being typed.
    show_bar: bool,
    sb_drag: bool,
    sb_active: Option<SbPart>,
    lynx: Option<PathBuf>,
    refreshes: u8,
    dirty: bool,
    reader_sites: std::collections::HashSet<String>,
    partial_shown: bool,
}

fn history_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".cache/harbour-shell/browser_history"))
}

fn bookmarks_path() -> Option<PathBuf> {
    Some(config_dir()?.join("bookmarks.txt"))
}

/// `toolbar=1` keeps the toolbar shown (Menu > Show Toolbar).
fn toolbar_pref_path() -> Option<PathBuf> {
    Some(config_dir()?.join("browser.conf"))
}

pub fn make() -> Box<dyn LauncherApp> {
    let mock = matches!(std::env::var("HARBOUR_MOCK").as_deref(), Ok("1" | "true" | "yes" | "on"));
    let fetcher: Arc<dyn Fetcher> = if mock { Arc::new(fetch::Mock) } else { Arc::new(fetch::Real) };
    let mut b = Browser::new(fetcher, mock);
    if b.persist {
        if let Some(t) = history_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            b.history = NamedList::parse("History", &t);
            b.history.entries.truncate(HISTORY_MAX);
        }
        if let Some(t) = bookmarks_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            b.bookmarks = NamedList::parse("Bookmarks", &t);
        }
        if let Some(t) = toolbar_pref_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            b.show_bar = t.lines().any(|l| l.trim() == "toolbar=1");
        }
    }
    Box::new(b)
}

impl Browser {
    pub fn new(fetcher: Arc<dyn Fetcher>, mock: bool) -> Self {
        let img = sdl2::image::init(InitFlag::PNG | InitFlag::JPG).or_else(|_| sdl2::image::init(InitFlag::PNG)).ok();
        Browser {
            fetcher,
            persist: !mock,
            _img: img,
            started: false,
            first: "about:home".into(),
            page: None,
            scroll: 0,
            sel: None,
            edit: Edit::None,
            addr: String::new(),
            caret: 0,
            fresh: false,
            swallow: None,
            hist_pos: None,
            nav: Vec::new(),
            pos: 0,
            loading: None,
            status: String::new(),
            info: false,
            frame: 0,
            bookmarks: NamedList::new("Bookmarks"),
            history: NamedList::new("History"),
            images: HashMap::new(),
            img_job: None,
            viewer: None,
            hover: None,
            pressed: 0,
            show_bar: false,
            sb_drag: false,
            sb_active: None,
            lynx: find_lynx(&|p| p.is_file()),
            refreshes: 0,
            dirty: true,
            reader_sites: Default::default(),
            partial_shown: false,
        }
    }

    /// Whether the toolbar is on screen: always when chosen, else while the address is typed.
    fn bar(&self) -> bool {
        self.show_bar || self.edit == Edit::Address
    }

    fn frame_r(&self) -> Rect {
        grid::app_frame_from(if self.bar() { FRAME_TOP_BAR } else { FRAME_TOP })
    }

    fn vy(&self) -> i32 {
        self.frame_r().y() + 1
    }

    /// The page's height: down to the frame's content rectangle (y 161):
    /// 128 px under the toolbar, 144 without it.
    fn vh(&self) -> i32 {
        grid::app_content_from(self.frame_r().y()).bottom() - self.vy()
    }

    fn view_r(&self) -> Rect {
        Rect::new(VIEW_X, self.vy(), VIEW_W as u32, self.vh() as u32)
    }

    fn sb_r(&self) -> Rect {
        Rect::new(grid::SCROLL_X, self.vy() + 1, grid::SCROLL_W as u32, (self.vh() - 2) as u32)
    }

    fn set_show_bar(&mut self, on: bool) {
        self.show_bar = on;
        if self.persist {
            if let Some(p) = toolbar_pref_path() {
                if let Some(d) = p.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                let _ = std::fs::write(p, if on { "toolbar=1\n" } else { "toolbar=0\n" });
            }
        }
        self.set_scroll(self.scroll);
        self.dirty = true;
    }

    fn metrics(ctx: &Ctx) -> Metrics {
        Metrics { latin: ctx.fs.font.as_ref().map(|f| f.width as i32).unwrap_or(6), wide: ctx.fs.jis_width as i32, jis: ctx.fs.jis_font.is_some() }
    }

    fn cur_url(&self) -> String {
        self.nav.get(self.pos).map(|n| n.url.clone()).or_else(|| self.page.as_ref().map(|p| p.url.clone())).unwrap_or_default()
    }

    fn max_scroll(&self) -> i32 {
        self.page.as_ref().map(|p| (p.height + 4 - self.vh()).max(0)).unwrap_or(0)
    }

    fn set_scroll(&mut self, v: i32) {
        let v = v.clamp(0, self.max_scroll());
        if v != self.scroll {
            self.scroll = v;
            self.dirty = true;
        }
    }

    fn ensure_visible(&mut self, y: i32, h: i32) {
        if y < self.scroll {
            self.set_scroll(y - LINE_H);
        } else if y + h > self.scroll + self.vh() {
            self.set_scroll(y + h - self.vh() + LINE_H);
        }
    }

    fn set_addr_from_page(&mut self) {
        if self.edit != Edit::Address {
            self.addr = self.cur_url();
            self.caret = self.addr.chars().count();
        }
    }

    // ---- loading

    fn site_of(u: &str) -> String {
        url::parse(u).map(|p| p.host).unwrap_or_default()
    }

    fn navigate(&mut self, target: String, post: Option<String>, mode: Mode, ctx: &Ctx) {
        let inline = target.starts_with("about:").then(|| about_html(&target, &self.bookmarks, &self.history));
        let reader = inline.is_none() && self.reader_sites.contains(&Self::site_of(&target));
        self.navigate_with(target, post, mode, inline, reader, ctx)
    }

    /// Switch the current page between normal and reader layout (the choice is kept per site).
    fn toggle_reader(&mut self, ctx: &Ctx) {
        let Some(p) = &self.page else { return };
        let Some(src) = p.source.clone() else {
            self.status = "No reader view here".into();
            return;
        };
        let (u, site, want) = (p.url.clone(), Self::site_of(&p.url), !p.reader);
        if want {
            self.reader_sites.insert(site);
        } else {
            self.reader_sites.remove(&site);
        }
        self.navigate_with(u, None, Mode::View, Some((*src).clone()), want, ctx);
    }

    fn navigate_with(&mut self, target: String, post: Option<String>, mode: Mode, inline: Option<String>, reader: bool, ctx: &Ctx) {
        let target = if post.is_none() { url::unwrap_redirect(&target) } else { target };
        self.started = true;
        self.partial_shown = false;
        // a jump inside the page needs no fetch
        if post.is_none() && matches!(mode, Mode::Push) {
            if let Some(p) = &self.page {
                if url::strip_fragment(&p.url) == url::strip_fragment(&target) && url::fragment(&target).is_some() {
                    let y = url::fragment(&target).and_then(|f| p.anchors.get(f).copied()).unwrap_or(0);
                    self.push_nav(target);
                    self.set_scroll(y);
                    self.sel = None;
                    self.set_addr_from_page();
                    self.dirty = true;
                    return;
                }
            }
        }
        if let Some(l) = self.loading.take() {
            l.cancel.store(true, Ordering::Relaxed);
        }
        if mode != Mode::View {
            if let Some(n) = self.nav.get_mut(self.pos) {
                n.scroll = self.scroll;
            }
        }
        let (cancel, progress) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU32::new(fetch::UNKNOWN)));
        let (tx, rx) = channel();
        let req = Req { url: target.clone(), post, max_bytes: fetch::PAGE_MAX };
        let (f, m, c, p) = (self.fetcher.clone(), Self::metrics(ctx), cancel.clone(), progress.clone());
        let spawned = std::thread::Builder::new().name("browser-load".into()).stack_size(8 << 20).spawn(move || {
            let tx2 = tx.clone();
            let r = load::load(f.as_ref(), req, load::Job { inline, m, reader, cancel: &c, progress: &p }, &mut |page| {
                let _ = tx2.send(Msg::Partial(Box::new(page)));
            });
            let _ = tx.send(Msg::Done(r));
        });
        if spawned.is_err() {
            self.status = "Cannot start a thread".into();
            return;
        }
        self.status = format!("Connecting to {}", url::display(&target));
        self.loading = Some(Loading { url: target, cancel, progress, rx, mode, reader, redirects: if mode == Mode::Replace { self.refreshes } else { 0 } });
        self.dirty = true;
    }

    /// An early page while the download goes on: shown at once, history untouched.
    fn show_partial(&mut self, p: Page) {
        let Some(l) = &self.loading else { return };
        if l.mode == Mode::View {
            return;
        }
        let keep = self.partial_shown;
        self.page = Some(p);
        self.sel = None;
        self.hover = None;
        if !keep {
            self.scroll = 0;
        }
        self.partial_shown = true;
        let m = self.max_scroll();
        self.scroll = self.scroll.min(m);
        self.dirty = true;
    }

    fn push_nav(&mut self, url: String) {
        if !self.nav.is_empty() {
            self.nav.truncate(self.pos + 1);
        }
        self.nav.push(Nav { url, scroll: 0 });
        if self.nav.len() > NAV_MAX {
            self.nav.remove(0);
        }
        self.pos = self.nav.len() - 1;
    }

    fn finish(&mut self, res: Result<Loaded, String>, l: Loading, ctx: &Ctx) {
        match res {
            Err(e) => {
                self.status = e;
            }
            Ok(Loaded::Image(u, bytes)) => {
                self.open_viewer(u, bytes);
            }
            Ok(Loaded::Page(p)) if l.mode == Mode::View => {
                let p = *p;
                if l.reader && !p.reader {
                    self.reader_sites.remove(&Self::site_of(&p.url));
                    self.status = "No article text found".into();
                } else {
                    self.status = if p.reader { "Reader view".into() } else { "Normal view".into() };
                    self.page = Some(p);
                    self.sel = None;
                    self.hover = None;
                    self.scroll = 0;
                }
            }
            Ok(Loaded::Page(p)) => {
                let p = *p;
                let final_url = p.url.clone();
                let mut restore = 0;
                match l.mode {
                    Mode::Push => self.push_nav(final_url.clone()),
                    Mode::Replace => {
                        if let Some(n) = self.nav.get_mut(self.pos) {
                            n.url = final_url.clone();
                        } else {
                            self.push_nav(final_url.clone());
                        }
                    }
                    Mode::Goto(i) => {
                        self.pos = i.min(self.nav.len().saturating_sub(1));
                        restore = self.nav.get(self.pos).map(|n| n.scroll).unwrap_or(0);
                        if let Some(n) = self.nav.get_mut(self.pos) {
                            n.url = final_url.clone();
                        }
                    }
                    Mode::Reload => restore = self.scroll,
                    Mode::View => {}
                }
                if !final_url.starts_with("about:") && p.url != "about:home" {
                    self.history.push_front(&final_url, &p.title, HISTORY_MAX);
                    self.save_history();
                }
                self.status = if p.truncated { "Page cut (too large)".into() } else { String::new() };
                let mut p = p;
                if matches!(l.mode, Mode::View) {
                    if let Some(old) = &self.page {
                        (p.charset, p.frames, p.bytes) = (old.charset.clone(), old.frames, old.bytes);
                    }
                }
                self.info = false;
                self.page = Some(p);
                self.sel = None;
                self.hover = None;
                self.edit = if self.edit == Edit::Address { Edit::None } else { Edit::None };
                let keep = self.partial_shown && matches!(l.mode, Mode::Push | Mode::Replace);
                self.partial_shown = false;
                let kept = self.scroll;
                self.scroll = 0;
                let target_scroll = match url::fragment(&l.url).and_then(|f| self.page.as_ref().and_then(|p| p.anchors.get(f).copied())) {
                    Some(y) => y,
                    None if keep => kept,
                    None => restore,
                };
                self.set_scroll(target_scroll);
                self.set_addr_from_page();
                // meta refresh
                if let Some(r) = self.page.as_ref().and_then(|p| p.refresh.clone()) {
                    if l.redirects < 3 && r != self.cur_url() {
                        self.refreshes = l.redirects + 1;
                        self.navigate(r, None, Mode::Replace, ctx);
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn save_history(&self) {
        if !self.persist {
            return;
        }
        if let Some(p) = history_path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, self.history.to_text());
        }
    }

    fn save_bookmarks(&self) {
        if !self.persist {
            return;
        }
        if let Some(p) = bookmarks_path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, self.bookmarks.to_text());
        }
    }

    fn stop(&mut self) -> bool {
        if let Some(l) = self.loading.take() {
            l.cancel.store(true, Ordering::Relaxed);
            self.status = "Cancelled".into();
            self.dirty = true;
            return true;
        }
        false
    }

    fn go_back(&mut self, ctx: &Ctx) {
        if self.pos > 0 && !self.nav.is_empty() {
            let i = self.pos - 1;
            let u = self.nav[i].url.clone();
            self.navigate(u, None, Mode::Goto(i), ctx);
        }
    }

    fn go_forward(&mut self, ctx: &Ctx) {
        if self.pos + 1 < self.nav.len() {
            let i = self.pos + 1;
            let u = self.nav[i].url.clone();
            self.navigate(u, None, Mode::Goto(i), ctx);
        }
    }

    fn reload(&mut self, ctx: &Ctx) {
        let u = self.cur_url();
        if !u.is_empty() {
            self.navigate(u, None, Mode::Reload, ctx);
        }
    }

    fn open_typed(&mut self, text: &str, ctx: &Ctx) -> AppAction {
        match url::normalize(text) {
            Some(u) => {
                self.edit = Edit::None;
                self.navigate(u, None, Mode::Push, ctx);
                AppAction::None
            }
            None => AppAction::Toast(i18n::tr("Not a valid address").into()),
        }
    }

    // ---- images

    fn open_viewer(&mut self, u: String, bytes: Vec<u8>) {
        let r = imageviewer::decode_bytes(&bytes, VIEW_W as u32, self.vh() as u32).map(|p| {
            let runs = tone::build(&p, tone::DEFAULT, 0, 0);
            (p.w as i32, p.h as i32, runs)
        });
        self.viewer = Some((u, Some(r)));
        self.dirty = true;
    }

    fn start_image(&mut self, url: String, max: (i32, i32)) {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (f, c) = (self.fetcher.clone(), cancel.clone());
        let u = url.clone();
        let ok = std::thread::Builder::new()
            .name("browser-img".into())
            .spawn(move || {
                let r = f.fetch(&Req { url: u, post: None, max_bytes: fetch::IMAGE_MAX }, &c, &AtomicU32::new(0));
                let _ = tx.send(r);
            })
            .is_ok();
        if ok {
            self.images.insert(url.clone(), Img::Pending);
            self.img_job = Some(ImgJob { url, max, cancel, rx });
        } else {
            self.images.insert(url, Img::Failed);
        }
    }

    fn poll_images(&mut self) -> bool {
        let mut changed = false;
        if let Some(job) = &self.img_job {
            match job.rx.try_recv() {
                Ok(r) => {
                    let job = self.img_job.take().unwrap();
                    let entry = match r {
                        Ok(resp) if resp.body.len() as u64 <= fetch::IMAGE_MAX && !resp.body.is_empty() => match imageviewer::decode_bytes(&resp.body, job.max.0 as u32, job.max.1 as u32) {
                            Ok(p) => {
                                let runs = tone::build(&p, tone::DEFAULT, 0, 0);
                                Img::Ready { w: p.w as i32, h: p.h as i32, runs }
                            }
                            Err(_) => Img::Failed,
                        },
                        _ => Img::Failed,
                    };
                    self.images.insert(job.url, entry);
                    changed = true;
                }
                Err(TryRecvError::Disconnected) => {
                    let job = self.img_job.take().unwrap();
                    self.images.insert(job.url, Img::Failed);
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if self.img_job.is_none() && self.loading.is_none() && self.viewer.is_none() {
            if let Some(p) = &self.page {
                if mem_available_mb().map(|m| m > 50).unwrap_or(true) {
                    let r = p.visible(self.scroll - self.vh(), self.scroll + 2 * self.vh());
                    let mut want = None;
                    for i in r {
                        if let ItemKind::Img(k) = p.items[i].kind {
                            let im = &p.images[k as usize];
                            if !im.placeholder && !self.images.contains_key(&im.url) {
                                want = Some((im.url.clone(), (im.box_w, im.box_h)));
                                break;
                            }
                        }
                    }
                    if let Some((u, max)) = want {
                        if self.images.len() >= 8 {
                            // keep the cache small: drop everything that is not pending
                            self.images.retain(|_, v| matches!(v, Img::Pending));
                        }
                        self.start_image(u, max);
                    }
                }
            }
        }
        changed
    }

    // ---- focus / activation

    fn focus_at(&self, i: usize) -> Option<(Focus, i32, i32)> {
        self.page.as_ref().and_then(|p| p.focus.get(i).copied())
    }

    fn step_focus(&mut self, dir: i32) {
        let Some(p) = &self.page else { return };
        let n = p.focus.len() as i32;
        if n == 0 {
            return;
        }
        let (scroll, view) = (self.scroll, self.vh());
        let visible = |i: usize| {
            let (_, y, h) = p.focus[i];
            y + h > scroll && y < scroll + view
        };
        let next = match self.sel {
            Some(s) if s < p.focus.len() && visible(s) => s as i32 + dir,
            _ => {
                if dir > 0 {
                    p.focus.iter().position(|f| f.1 >= scroll).map(|i| i as i32).unwrap_or(0)
                } else {
                    p.focus.iter().rposition(|f| f.1 < scroll + view).map(|i| i as i32).unwrap_or(n - 1)
                }
            }
        };
        let next = next.rem_euclid(n) as usize;
        let (_, y, h) = p.focus[next];
        self.sel = Some(next);
        self.ensure_visible(y, h);
        self.dirty = true;
    }

    fn selected(&self) -> Option<Focus> {
        self.sel.and_then(|i| self.focus_at(i)).map(|f| f.0)
    }

    fn start_edit_field(&mut self, f: u32) {
        self.edit = Edit::Field(f);
        if let Some(p) = &self.page {
            self.caret = p.fields[f as usize].value.chars().count();
        }
        if let Some(p) = &self.page {
            if let Some((_, y, _, h)) = p.field_rect(f) {
                self.ensure_visible(y, h);
            }
        }
        self.dirty = true;
    }

    fn submit(&mut self, form: Option<usize>, submit_field: Option<usize>, ctx: &Ctx) {
        let Some(fi) = form else { return };
        let req = self.page.as_ref().and_then(|p| p.form_request(fi, submit_field));
        match req {
            Some(Request::Get(u)) => self.navigate(u, None, Mode::Push, ctx),
            Some(Request::Post { url, body }) => self.navigate(url, Some(body), Mode::Push, ctx),
            None => self.status = "Bad form action".into(),
        }
    }

    fn activate(&mut self, f: Focus, ctx: &Ctx) {
        match f {
            Focus::Link(l) => {
                if let Some(u) = self.page.as_ref().and_then(|p| p.links.get(l as usize).cloned()) {
                    self.navigate(u, None, Mode::Push, ctx);
                }
            }
            Focus::Image(i) => {
                if let Some(u) = self.page.as_ref().and_then(|p| p.images.get(i as usize).map(|im| im.url.clone())) {
                    self.viewer = Some((u.clone(), None));
                    let cancel = Arc::new(AtomicBool::new(false));
                    let (tx, rx) = channel();
                    let (fe, c) = (self.fetcher.clone(), cancel.clone());
                    let _ = std::thread::Builder::new().spawn(move || {
                        let r = fe.fetch(&Req { url: u, post: None, max_bytes: fetch::IMAGE_MAX }, &c, &AtomicU32::new(0));
                        let _ = tx.send(r);
                    });
                    if let Some(old) = self.img_job.take() {
                        old.cancel.store(true, Ordering::Relaxed);
                    }
                    self.img_job = Some(ImgJob { url: String::from("#viewer"), max: (VIEW_W, self.vh()), cancel, rx });
                }
            }
            Focus::Field(fi) => {
                let (kind, form) = match &self.page {
                    Some(p) => (p.fields[fi as usize].kind.clone(), p.fields[fi as usize].form),
                    None => return,
                };
                match kind {
                    FieldKind::Text | FieldKind::Password => self.start_edit_field(fi),
                    FieldKind::Check | FieldKind::Radio | FieldKind::Select => {
                        if let Some(p) = &mut self.page {
                            p.toggle(fi as usize);
                        }
                    }
                    FieldKind::Submit => self.submit(form, Some(fi as usize), ctx),
                    FieldKind::Hidden => {}
                }
            }
        }
        self.dirty = true;
    }

    fn insert_text(&mut self, t: &str) {
        let t: String = t.chars().filter(|c| !c.is_control()).collect();
        if t.is_empty() {
            return;
        }
        match self.edit {
            Edit::Address => {
                if self.fresh {
                    self.addr.clear();
                    self.caret = 0;
                    self.fresh = false;
                }
                let b = byte_at(&self.addr, self.caret);
                self.addr.insert_str(b, &t);
                self.caret += t.chars().count();
                self.hist_pos = None;
            }
            Edit::Field(f) => {
                let caret = self.caret;
                if let Some(p) = &mut self.page {
                    let v = &mut p.fields[f as usize].value;
                    let b = byte_at(v, caret);
                    v.insert_str(b, &t);
                    self.caret += t.chars().count();
                }
            }
            Edit::None => {}
        }
        self.dirty = true;
    }

    /// Edit keys shared by the address bar and text fields; true if handled.
    fn edit_key(&mut self, key: Keycode, mods: Mod) -> bool {
        let ctrl = mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
        let (text, field) = match self.edit {
            Edit::Address => (std::mem::take(&mut self.addr), None),
            Edit::Field(f) => match &mut self.page {
                Some(p) => (std::mem::take(&mut p.fields[f as usize].value), Some(f)),
                None => return false,
            },
            Edit::None => return false,
        };
        let mut ed = LineEdit { text, caret: self.caret };
        let mut handled = true;
        match key {
            Keycode::Left | Keycode::Right => {
                ed.key(key, mods);
                self.fresh = false;
            }
            Keycode::Home | Keycode::End | Keycode::Delete => {
                ed.key(key, mods);
            }
            Keycode::Backspace => {
                // A freshly focused address bar is selected: Backspace clears it.
                if self.fresh && self.edit == Edit::Address {
                    ed.clear();
                } else {
                    ed.key(key, mods);
                }
                self.fresh = false;
            }
            Keycode::U if ctrl => {
                ed.clear();
                self.fresh = false;
            }
            _ => handled = false,
        }
        self.caret = ed.caret;
        let text = ed.text;
        match field {
            None => self.addr = text,
            Some(f) => {
                if let Some(p) = &mut self.page {
                    p.fields[f as usize].value = text;
                }
            }
        }
        self.dirty = true;
        handled
    }

    fn focus_address(&mut self) {
        self.edit = Edit::Address;
        self.addr = self.cur_url();
        self.caret = self.addr.chars().count();
        self.fresh = true;
        self.hist_pos = None;
        self.dirty = true;
    }

    fn add_or_remove_bookmark(&mut self) -> AppAction {
        let u = self.cur_url();
        if u.is_empty() || u.starts_with("about:") {
            return AppAction::Toast(i18n::tr("Nothing to bookmark").into());
        }
        let title = self.page.as_ref().map(|p| p.title.clone()).unwrap_or_default();
        let added = self.bookmarks.toggle(&u, &title);
        self.save_bookmarks();
        AppAction::Toast(i18n::tr(if added { "Bookmark added" } else { "Bookmark removed" }).into())
    }
}

fn byte_at(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(b, _)| b).unwrap_or(s.len())
}

// ------------------------------------------------------------------------------ the app

impl LauncherApp for Browser {
    fn title(&self) -> String {
        crate::launcher::i18n::app_name("browser")
    }

    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Tab / Shift+Tab", "Next / previous link or field"),
            k("Return", "Follow a link; type in a field, Return again sends"),
            k("Up / Down", "Scroll"),
            k("R", "Reader view"),
            k("Ctrl+R / F5", "Reload"),
            k("Alt", "Address, bookmarks, back, reload"),
        ]
    }

    fn wants_tab(&self) -> bool {
        true
    }

    /// Another app hands over a web address (as the "path"): it becomes the first page.
    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        let u = path.to_str().ok_or("Not a web address")?;
        if !(u.starts_with("https://") || u.starts_with("http://")) || u.chars().any(char::is_control) {
            return Err("Not a web address".into());
        }
        self.first = u.to_string();
        Ok(())
    }

    fn menu(&self) -> Vec<MenuItem> {
        let marked = self.bookmarks.contains(&self.cur_url());
        vec![
            MenuItem::new("reload", "Reload"),
            MenuItem::new("back", "Back"),
            MenuItem::new("forward", "Forward"),
            MenuItem::new("home", "Home"),
            MenuItem::new("addr", "Open URL..."),
            MenuItem::new("toolbar", if self.show_bar { "Hide Toolbar" } else { "Show Toolbar" }),
            MenuItem::new("bookmarks", "Bookmarks"),
            MenuItem::new("bookmark", if marked { "Remove bookmark" } else { "Add bookmark" }),
            MenuItem::new("history", "History"),
            MenuItem::new("info", "Page info"),
            MenuItem::new("reader", if self.page.as_ref().map(|p| p.reader).unwrap_or(false) { "Normal view" } else { "Reader" }),
            MenuItem::new("lynx", "Open in Lynx"),
            MenuItem::new("help", "Help"),
        ]
    }

    fn context_menu(&mut self, x: i32, y: i32, _ctx: &Ctx) -> Option<Vec<MenuItem>> {
        let in_view = contains(self.view_r(), x, y);
        let hit = if in_view { self.page.as_ref().and_then(|p| p.hit(x - PAGE_X, y - self.vy() - PAGE_Y + self.scroll)) } else { None };
        if let Some(f @ Focus::Link(_)) = hit {
            self.sel = self.page.as_ref().and_then(|p| p.focus.iter().position(|g| g.0 == f));
            let marked = self.bookmarks.contains(&self.cur_url());
            return Some(vec![
                MenuItem::new("openlink", "Open link"),
                MenuItem::new("bookmark", if marked { "Remove bookmark" } else { "Add bookmark" }),
            ]);
        }
        Some(vec![MenuItem::new("back", "Back"), MenuItem::new("forward", "Forward"), MenuItem::new("reload", "Reload"), MenuItem::new("info", "Page info")])
    }

    fn menu_action(&mut self, id: &str, ctx: &Ctx) -> AppAction {
        match id {
            "openlink" => {
                if let Some(f) = self.selected() {
                    self.activate(f, ctx);
                }
            }
            "reload" => self.reload(ctx),
            "back" => self.go_back(ctx),
            "forward" => self.go_forward(ctx),
            "home" => self.navigate("about:home".into(), None, Mode::Push, ctx),
            "addr" => self.focus_address(),
            "toolbar" => self.set_show_bar(!self.show_bar),
            "bookmarks" => self.navigate("about:bookmarks".into(), None, Mode::Push, ctx),
            "history" => self.navigate("about:history".into(), None, Mode::Push, ctx),
            "reader" => self.toggle_reader(ctx),
            "info" => self.info = self.page.is_some(),
            "help" => self.navigate("about:help".into(), None, Mode::Push, ctx),
            "bookmark" => return self.add_or_remove_bookmark(),
            "lynx" => {
                let u = self.cur_url();
                if u.starts_with("about:") || u.is_empty() {
                    return AppAction::Toast(i18n::tr("Open a web page first").into());
                }
                return match &self.lynx {
                    Some(l) => AppAction::Launch(launch_command(&u, l)),
                    None => AppAction::Toast(i18n::tr("lynx is not installed").into()),
                };
            }
            _ => {}
        }
        AppAction::None
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, ctx: &Ctx) -> AppAction {
        self.dirty = true;
        let ctrl = mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
        let shift = mods.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
        // Page info card: any key closes it.
        if self.info {
            self.info = false;
            return AppAction::None;
        }
        // Full-screen picture: any "back" key closes it.
        if self.viewer.is_some() {
            if matches!(key, Keycode::Escape | Keycode::Backspace | Keycode::Return | Keycode::KpEnter | Keycode::Space) {
                self.viewer = None;
                if let Some(j) = self.img_job.take() {
                    if j.url == "#viewer" {
                        j.cancel.store(true, Ordering::Relaxed);
                    } else {
                        self.img_job = Some(j);
                    }
                }
            }
            return AppAction::None;
        }
        // Escape: leave a text box, else stop loading.
        if key == Keycode::Escape {
            if self.edit != Edit::None {
                self.edit = Edit::None;
                self.set_addr_from_page();
            } else if !self.stop() {
                self.sel = None;
            }
            return AppAction::None;
        }
        if key == Keycode::L && ctrl {
            self.focus_address();
            return AppAction::None;
        }
        if (key == Keycode::R && ctrl) || key == Keycode::F5 {
            self.reload(ctx);
            return AppAction::None;
        }
        match self.edit {
            Edit::Address => {
                match key {
                    Keycode::Return | Keycode::KpEnter => {
                        let t = self.addr.clone();
                        return self.open_typed(&t, ctx);
                    }
                    Keycode::Up | Keycode::Down => {
                        let n = self.history.entries.len();
                        if n > 0 {
                            let i = match (self.hist_pos, key) {
                                (None, Keycode::Down) => 0,
                                (None, _) => return AppAction::None,
                                (Some(i), Keycode::Down) => (i + 1).min(n - 1),
                                (Some(0), _) => {
                                    self.hist_pos = None;
                                    self.addr = self.cur_url();
                                    self.caret = self.addr.chars().count();
                                    return AppAction::None;
                                }
                                (Some(i), _) => i - 1,
                            };
                            self.hist_pos = Some(i);
                            self.addr = self.history.entries[i].url.clone();
                            self.caret = self.addr.chars().count();
                            self.fresh = false;
                        }
                    }
                    Keycode::Tab => {
                        self.edit = Edit::None;
                        self.step_focus(if shift { -1 } else { 1 });
                    }
                    _ => {
                        self.edit_key(key, mods);
                    }
                }
                return AppAction::None;
            }
            Edit::Field(f) => {
                match key {
                    Keycode::Return | Keycode::KpEnter => {
                        let form = self.page.as_ref().and_then(|p| p.fields[f as usize].form);
                        self.edit = Edit::None;
                        if form.is_some() {
                            // Enter in a text box submits its form (through its first submit button)
                            let sub = self.page.as_ref().and_then(|p| p.forms[form.unwrap()].fields.iter().copied().find(|&i| p.fields[i].kind == FieldKind::Submit && !p.fields[i].name.is_empty()));
                            self.submit(form, sub, ctx);
                        }
                    }
                    Keycode::Tab => {
                        self.edit = Edit::None;
                        self.step_focus(if shift { -1 } else { 1 });
                    }
                    Keycode::Up | Keycode::Down => {
                        self.edit = Edit::None;
                        self.step_focus(if key == Keycode::Up { -1 } else { 1 });
                    }
                    _ => {
                        self.edit_key(key, mods);
                    }
                }
                return AppAction::None;
            }
            Edit::None => {}
        }
        let k = arrow_alias(key);
        match k {
            Keycode::Up => self.set_scroll(self.scroll - SCROLL_LINE),
            Keycode::Down => self.set_scroll(self.scroll + SCROLL_LINE),
            Keycode::PageUp => self.set_scroll(self.scroll - self.vh() + LINE_H),
            Keycode::PageDown => self.set_scroll(self.scroll + self.vh() - LINE_H),
            Keycode::Space => self.set_scroll(self.scroll + if shift { -(self.vh() - LINE_H) } else { self.vh() - LINE_H }),
            Keycode::Home => self.set_scroll(0),
            Keycode::End => self.set_scroll(self.max_scroll()),
            Keycode::Tab => self.step_focus(if shift { -1 } else { 1 }),
            Keycode::Left => self.step_focus(-1),
            Keycode::Right => self.step_focus(1),
            Keycode::Backspace => {
                if shift {
                    self.go_forward(ctx)
                } else {
                    self.go_back(ctx)
                }
            }
            Keycode::R => self.toggle_reader(ctx),
            Keycode::Slash => {
                self.focus_address();
                self.swallow = Some('/');
            }
            Keycode::Return | Keycode::KpEnter => {
                if let Some(f) = self.selected() {
                    self.activate(f, ctx);
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn text(&mut self, text: &str, _ctx: &Ctx) -> AppAction {
        if let Some(c) = self.swallow.take() {
            if text == c.to_string() {
                return AppAction::None;
            }
        }
        self.insert_text(text);
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> AppAction {
        self.dirty = true;
        match ev {
            PointerEvent::Wheel { dy, .. } => {
                if self.viewer.is_none() {
                    self.set_scroll(self.scroll - dy.signum() * 2 * SCROLL_LINE);
                }
            }
            PointerEvent::Move { x, y } => {
                let h = if contains(self.view_r(), x, y) { self.page.as_ref().and_then(|p| p.hit(x - PAGE_X, y - self.vy() - PAGE_Y + self.scroll)) } else { None };
                self.hover = h;
            }
            PointerEvent::Down { x, y, button: 0, .. } => {
                if self.info {
                    self.info = false;
                    return AppAction::None;
                }
                if self.viewer.is_some() {
                    self.viewer = None;
                    return AppAction::None;
                }
                let bar = self.bar();
                if !bar && y < self.vy() {
                    // The row above the page (where the toolbar would be): bring it up to type an address.
                    self.focus_address();
                } else if bar && contains(back_r(), x, y) {
                    self.pressed = 1;
                    self.go_back(ctx);
                } else if bar && contains(fwd_r(), x, y) {
                    self.pressed = 2;
                    self.go_forward(ctx);
                } else if bar && contains(reload_r(), x, y) {
                    self.pressed = 3;
                    if !self.stop() {
                        self.reload(ctx);
                    }
                } else if bar && contains(addr_r(), x, y) {
                    if self.edit != Edit::Address {
                        self.focus_address();
                    }
                } else if contains(self.sb_r(), x, y) {
                    let sb = ScrollBar::new(self.sb_r(), self.page.as_ref().map(|p| p.height as usize + 4).unwrap_or(0), self.vh() as usize, self.scroll as usize);
                    match sb.hit(x, y) {
                        Some(SbPart::Up) => self.set_scroll(self.scroll - SCROLL_LINE),
                        Some(SbPart::Down) => self.set_scroll(self.scroll + SCROLL_LINE),
                        Some(SbPart::PageUp) => self.set_scroll(self.scroll - self.vh() + LINE_H),
                        Some(SbPart::PageDown) => self.set_scroll(self.scroll + self.vh() - LINE_H),
                        Some(SbPart::Thumb) => {
                            self.sb_drag = true;
                            self.sb_active = Some(SbPart::Thumb);
                        }
                        None => {}
                    }
                } else if contains(self.view_r(), x, y) {
                    if self.edit == Edit::Address {
                        self.edit = Edit::None;
                        self.set_addr_from_page();
                    }
                    let hit = self.page.as_ref().and_then(|p| p.hit(x - PAGE_X, y - self.vy() - PAGE_Y + self.scroll));
                    self.edit = Edit::None;
                    match hit {
                        Some(f) => {
                            self.sel = self.page.as_ref().and_then(|p| p.focus.iter().position(|g| g.0 == f));
                            self.activate(f, ctx);
                        }
                        None => self.sel = None,
                    }
                }
            }
            PointerEvent::Drag { y, .. } if self.sb_drag => {
                let sb = ScrollBar::new(self.sb_r(), self.page.as_ref().map(|p| p.height as usize + 4).unwrap_or(0), self.vh() as usize, self.scroll as usize);
                self.set_scroll(sb.offset_for_y(y) as i32);
            }
            PointerEvent::Up { button: 0, .. } => {
                self.sb_drag = false;
                self.sb_active = None;
                self.pressed = 0;
            }
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, ctx: &Ctx) -> bool {
        if !self.started {
            self.started = true;
            self.navigate(self.first.clone(), None, Mode::Push, ctx);
        }
        let mut changed = std::mem::take(&mut self.dirty);
        if let Some(l) = &self.loading {
            self.frame = self.frame.wrapping_add(1);
            changed = true;
            let mut got = None;
            let mut gone = false;
            // keep only the newest partial page; stop at the final result
            loop {
                match l.rx.try_recv() {
                    Ok(Msg::Partial(p)) => got = Some(Msg::Partial(p)),
                    Ok(m) => {
                        got = Some(m);
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        gone = got.is_none();
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
            match got {
                Some(Msg::Done(res)) => {
                    let l = self.loading.take().unwrap();
                    self.finish(res, l, ctx);
                }
                Some(Msg::Partial(p)) => self.show_partial(*p),
                None if gone => {
                    self.loading = None;
                    self.status = "Load failed".into();
                }
                None => {}
            }
        }
        // the full-screen picture arrives through the image job slot
        if matches!(&self.img_job, Some(j) if j.url == "#viewer") {
            let got = self.img_job.as_ref().and_then(|j| j.rx.try_recv().ok());
            if let Some(r) = got {
                self.img_job = None;
                match r {
                    Ok(resp) => {
                        let u = self.viewer.as_ref().map(|v| v.0.clone()).unwrap_or_default();
                        self.open_viewer(u, resp.body);
                    }
                    Err(e) => self.viewer = Some((String::new(), Some(Err(e)))),
                }
                changed = true;
            }
        } else if self.poll_images() {
            changed = true;
        }
        changed
    }

    fn draw(&mut self, c: &mut Canvas<Window>, _assets: &mut AssetStore, ctx: &Ctx) {
        if !self.started {
            self.started = true;
            self.navigate(self.first.clone(), None, Mode::Push, ctx);
        }
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let paper = tm.get_color("widget_face_active");
        let faint = crate::launcher::widgets::secondary_ink(tm);
        let line = tm.get_color("line");
        let sunken = tm.get_color("surface_sunken");
        ui::fill_rect(c, chrome::app_rect(), tm.get_color("home_bg"));

        // ---- toolbar: only while it is wanted (Menu > Show Toolbar) or an address is being typed
        let loading = self.loading.is_some();
        if self.bar() {
        let back_st = BtnState { pressed: self.pressed == 1, disabled: self.pos == 0, ..Default::default() };
        widgets::draw_icon_button(c, tm, back_r(), &ICON_BACK, back_st);
        let fwd_st = BtnState { pressed: self.pressed == 2, disabled: self.pos + 1 >= self.nav.len(), ..Default::default() };
        widgets::draw_icon_button(c, tm, fwd_r(), &ICON_FORWARD, fwd_st);
        widgets::draw_icon_button(c, tm, reload_r(), if loading { &ICON_STOP } else { &ICON_GO }, BtnState { pressed: self.pressed == 3, ..Default::default() });
            // The shared text field: the address being typed, or the page's address.
            if self.edit == Edit::Address {
                widgets::draw_field(c, fs, tm, addr_r(), &widgets::Field { text: &self.addr, caret: Some(self.caret), select_all: self.fresh, ..Default::default() });
            } else {
                let t = if let Some(l) = &self.loading { l.url.clone() } else { self.cur_url() };
                let shown = if t.is_empty() { String::new() } else { url::display(&t) };
                widgets::draw_field(c, fs, tm, addr_r(), &widgets::Field { text: &shown, placeholder: "Address or search words", ..Default::default() });
            }
        }

        // ---- viewport
        let view = self.view_r();
        grid::draw_app_frame(c, self.frame_r().y(), ink, Some(paper));
        if let Some((_, state)) = &self.viewer {
            match state {
                Some(Ok((w, h, runs))) => {
                    let (ox, oy) = (view.x() + (VIEW_W - w) / 2, view.y() + (self.vh() - h) / 2);
                    draw_runs(c, runs, ox, oy, &tone::theme_ramp(tm));
                }
                Some(Err(e)) => fs.draw_text(c, view.x() + 6, view.y() + 6, &chrome::fit_text(fs, &i18n::tr_s(&format!("Cannot show picture: {e}")), VIEW_W - 12), faint),
                None => fs.draw_text(c, view.x() + 6, view.y() + 6, i18n::tr("Loading picture..."), faint),
            }
        } else if let Some(p) = &self.page {
            c.set_clip_rect(view);
            let sel = self.selected();
            let sel_link = if let Some(Focus::Link(l)) = sel { Some(l) } else { None };
            let ox = PAGE_X;
            let oy = self.vy() + PAGE_Y - self.scroll;
            for i in p.visible(self.scroll, self.scroll + self.vh()) {
                let it = &p.items[i];
                let (x, y) = (ox + it.x, oy + it.y);
                if y + it.h < self.vy() || y > self.vy() + self.vh() {
                    continue;
                }
                match &it.kind {
                    ItemKind::Text { text, style } => {
                        let hot = it.link.is_some() && it.link == sel_link;
                        if hot {
                            ui::fill_rect(c, Rect::new(x - 1, y, it.w as u32 + 2, it.h as u32), ink);
                        }
                        let col = if hot { paper } else if style.mono { faint } else { ink };
                        let col = if style.mono && !hot { ink } else { col };
                        if style.bold {
                            fs.draw_text_bold(c, x, y, text, col);
                        } else {
                            fs.draw_text(c, x, y, text, col);
                        }
                        if (style.underline || style.italic) && !hot {
                            c.set_draw_color(if it.link.is_some() { ink } else { faint });
                            let uy = y + it.h - 1;
                            if style.italic && !style.underline {
                                let mut xx = x;
                                while xx < x + it.w {
                                    let _ = c.draw_point((xx, uy));
                                    xx += 2;
                                }
                            } else {
                                let _ = c.draw_line((x, uy), (x + it.w - 1, uy));
                            }
                        }
                    }
                    ItemKind::Rule => {
                        ui::fill_rect(c, Rect::new(x, y, it.w as u32, it.h as u32), if it.h > 1 { line } else { ink });
                    }
                    ItemKind::Bar => ui::fill_rect(c, Rect::new(x, y, 2, it.h as u32), line),
                    ItemKind::Bg => ui::fill_rect(c, Rect::new(x, y, it.w as u32, it.h as u32), sunken),
                    ItemKind::Img(k) => {
                        let im = &p.images[*k as usize];
                        let hot = it.link.is_some() && it.link == sel_link || sel == Some(Focus::Image(*k));
                        let r = Rect::new(x, y, it.w as u32, it.h as u32);
                        match self.images.get(&im.url) {
                            Some(Img::Ready { w, h, runs }) if !im.placeholder => {
                                draw_runs(c, runs, x + (it.w - w) / 2, y + (it.h - h) / 2, &tone::theme_ramp(tm));
                                if hot {
                                    ui::draw_rect_thick(c, r, ink);
                                }
                            }
                            _ => {
                                ui::fill_rect(c, r, if hot { ink } else { sunken });
                                ui::draw_rect(c, r, if hot { ink } else { line });
                                if im.placeholder {
                                    fs.draw_text(c, x + 2, y + 1, &im.alt, if hot { paper } else { ink });
                                } else if let Some(Img::Failed) = self.images.get(&im.url) {
                                    fs.draw_text(c, x + 3, y + 1, "x", faint);
                                }
                            }
                        }
                    }
                    ItemKind::Field(k) => {
                        let f = &p.fields[*k as usize];
                        let hot = sel == Some(Focus::Field(*k));
                        let r = Rect::new(x, y, it.w as u32, it.h as u32);
                        match f.kind {
                            FieldKind::Text | FieldKind::Password => {
                                // The shared text field; a field the cursor is on gets the thick frame.
                                let editing = self.edit == Edit::Field(*k);
                                let field = widgets::Field { text: &f.value, caret: editing.then_some(self.caret), masked: f.kind == FieldKind::Password, ..Default::default() };
                                widgets::draw_field(c, fs, tm, r, &field);
                                if hot || editing {
                                    ui::draw_rect_thick(c, r, ink);
                                }
                            }
                            FieldKind::Submit => widgets::draw_button(c, fs, tm, r, &f.label, BtnState { pressed: hot, ..Default::default() }),
                            FieldKind::Check => widgets::draw_checkbox(c, tm, x, y, f.checked, false, BtnState { hover: hot, pressed: hot, ..Default::default() }),
                            FieldKind::Radio => widgets::draw_radio(c, tm, x, y, f.checked, BtnState { hover: hot, pressed: hot, ..Default::default() }),
                            FieldKind::Select => {
                                let dd = Dropdown::new(r, f.options.iter().map(|o| o.0.clone()).collect());
                                dd.draw_closed(c, fs, tm, f.sel, BtnState { pressed: hot, ..Default::default() });
                            }
                            FieldKind::Hidden => {}
                        }
                    }
                }
            }
            c.set_clip_rect(None);
            if p.items.is_empty() {
                fs.draw_text(c, view.x() + 6, view.y() + 6, i18n::tr("(empty page)"), faint);
            }
            ScrollBar::new(self.sb_r(), p.height as usize + 4, self.vh() as usize, self.scroll as usize).draw(c, tm, self.sb_active);
        } else {
            fs.draw_text(c, view.x() + 6, view.y() + 6, i18n::tr("Loading..."), faint);
        }

        // ---- loading gauge over the page's top edge while the toolbar (and its Stop button) is hidden:
        // 2 px in the blank space above the first line, filled as far as the download got.
        if let (false, Some(l)) = (self.bar(), &self.loading) {
            let g = Rect::new(VIEW_X + 1, self.vy() + 1, (VIEW_W - 2) as u32, 2);
            ui::fill_rect(c, g, paper);
            match l.progress.load(Ordering::Relaxed) {
                fetch::UNKNOWN => {
                    let seg = 24;
                    let x = g.x() + ((self.frame as i32 * 4) % (g.width() as i32 + seg)) - seg;
                    let (x0, x1) = (x.max(g.x()), (x + seg).min(g.right()));
                    if x1 > x0 {
                        ui::fill_rect(c, Rect::new(x0, g.y(), (x1 - x0) as u32, 2), ink);
                    }
                }
                v => ui::fill_rect(c, Rect::new(g.x(), g.y(), ((g.width() as u64 * v.min(1000) as u64) / 1000).max(1) as u32, 2), ink),
            }
        }

        // ---- transient status: a line over the bottom of the page, only while there is something to say
        // 1 px in from the frame and the scroll bar, so its rule touches neither.
        let strip = Rect::new(VIEW_X + 1, self.vy() + self.vh() - grid::ROW_H, (VIEW_W - 2) as u32, grid::ROW_H as u32);
        let target = self.hover.or_else(|| self.selected()).and_then(|f| self.page.as_ref().map(|p| match f {
            Focus::Link(l) => url::display(&p.links[l as usize]),
            Focus::Field(k) => match p.fields[k as usize].kind {
                FieldKind::Submit => i18n::tr("Form button").to_string(),
                FieldKind::Text | FieldKind::Password => i18n::tr("Text field").to_string(),
                _ => i18n::tr("Form choice").to_string(),
            },
            Focus::Image(i) => format!("{}: {}", i18n::tr("Picture"), url::display(&p.images[i as usize].url)),
        }));
        let msg = if let Some(l) = &self.loading {
            Some(format!("{} {}", i18n::tr(if l.progress.load(Ordering::Relaxed) == fetch::UNKNOWN { "Connecting" } else { "Loading" }), url::display(&l.url)))
        } else if let Some(t) = target {
            Some(t)
        } else if !self.status.is_empty() {
            Some(i18n::tr_s(&self.status))
        } else {
            None
        };
        if self.viewer.is_none() && !self.info {
            if let Some(msg) = msg {
                ui::fill_rect(c, strip, paper);
                ui::fill_rect(c, Rect::new(strip.x(), strip.y(), strip.width(), 1), ink);
                let mut room = strip.width() as i32 - 8;
                if let Some(l) = &self.loading {
                    let bar_w = 64;
                    let frac = match l.progress.load(Ordering::Relaxed) {
                        fetch::UNKNOWN => None,
                        v => Some(v as f32 / 1000.0),
                    };
                    ProgressBar::new(Rect::new(strip.x() + strip.width() as i32 - bar_w - 4, strip.y() + 5, bar_w as u32, 8), frac, self.frame).draw(c, tm);
                    room -= bar_w + 4;
                }
                fs.draw_text(c, strip.x() + 4, strip.y() + 2, &chrome::fit_text(fs, &msg, room), ink);
            }
        }

        // ---- page info card (opened from the menu)
        if self.info {
            if let Some(p) = &self.page {
                let card = Rect::new(VIEW_X + 8, self.vy() + 8, (VIEW_W - 16) as u32, (self.vh() - 16) as u32);
                ui::fill_rect(c, card, paper);
                ui::draw_rect(c, card, ink);
                let room = card.width() as i32 - 12;
                let title = if p.title.is_empty() { i18n::tr("(no title)").to_string() } else { p.title.clone() };
                fs.draw_text_bold(c, card.x() + 6, card.y() + 4, &chrome::fit_text(fs, &title, room), ink);
                let kb = (p.bytes.max(p.source.as_ref().map(|s| s.len()).unwrap_or(0)) + 1023) / 1024;
                let mut rows: Vec<String> = Vec::new();
                for chunk in wrap_chars(&url::display(&p.url), room / 6, 2) {
                    rows.push(chunk);
                }
                if !p.charset.is_empty() {
                    rows.push(format!("{}: {}", i18n::tr("Charset"), p.charset));
                }
                rows.push(format!("{}: {} KB", i18n::tr("Size"), kb));
                rows.push(format!("{}: {}  {}: {}  {}: {}", i18n::tr("Links"), p.links.len(), i18n::tr("Pictures"), p.images.len(), i18n::tr("Forms"), p.forms.len()));
                if p.frames > 0 {
                    rows.push(format!("{}: {}", i18n::tr("Frames"), p.frames));
                }
                if p.reader {
                    rows.push(i18n::tr("Reader view").to_string());
                }
                let mut y = card.y() + 4 + LINE_H;
                for r in rows {
                    if y + LINE_H > card.y() + card.height() as i32 {
                        break;
                    }
                    fs.draw_text(c, card.x() + 6, y, &chrome::fit_text(fs, &r, room), faint);
                    y += LINE_H;
                }
            }
        }
    }
}

/// Split `s` into at most `rows` chunks of `n` characters (the last one ends with an ellipsis if cut).
fn wrap_chars(s: &str, n: i32, rows: usize) -> Vec<String> {
    let n = n.max(4) as usize;
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<String> = chars.chunks(n).map(|c| c.iter().collect()).collect();
    if out.len() > rows {
        out.truncate(rows);
        if let Some(l) = out.last_mut() {
            l.pop();
            l.push('~');
        }
    }
    out
}

fn draw_runs(c: &mut Canvas<Window>, t: &tone::ToneRuns, ox: i32, oy: i32, ramp: &[sdl2::pixels::Color; tone::TONES]) {
    c.set_draw_color(ramp[0]);
    let _ = c.fill_rect(Rect::new(ox, oy, t.bounds.width(), t.bounds.height()));
    for i in 1..tone::TONES {
        if t.runs[i].is_empty() {
            continue;
        }
        c.set_draw_color(ramp[i]);
        let moved: Vec<Rect> = t.runs[i].iter().map(|r| Rect::new(r.x() + ox, r.y() + oy, r.width(), r.height())).collect();
        let _ = c.fill_rects(&moved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::apps::browser::fetch::Mock;

    fn ctx_parts() -> (crate::font::FontSystem, crate::system::mock::MockBackend, crate::theme::ThemeManager) {
        let theme = crate::theme::Theme::load_from_yaml(&format!("{}/assets/themes/theme-black.yaml", env!("CARGO_MANIFEST_DIR"))).unwrap();
        (crate::font::FontSystem::new(), crate::system::mock::MockBackend::new(), crate::theme::ThemeManager::new(theme))
    }

    macro_rules! ctx {
        ($f:ident, $b:ident, $t:ident) => {
            Ctx { fs: &$f, tm: &$t, sys: &$b, now: chrono::Local::now() }
        };
    }

    fn settle(b: &mut Browser, ctx: &Ctx) {
        for _ in 0..200 {
            b.tick(ctx);
            if b.loading.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("load did not finish");
    }

    fn new_browser() -> Browser {
        Browser::new(Arc::new(Mock), true)
    }

    fn press(b: &mut Browser, k: Keycode, ctx: &Ctx) {
        b.key(k, Mod::empty(), false, ctx);
    }

    fn typed(b: &mut Browser, s: &str, ctx: &Ctx) {
        b.text(s, ctx);
    }

    #[test]
    fn old_lynx_helpers_still_work() {
        assert_eq!(find_lynx(&|p| p == Path::new("/usr/local/bin/lynx")), Some(PathBuf::from("/usr/local/bin/lynx")));
        assert_eq!(find_lynx(&|_| false), None);
        assert_eq!(launch_command("https://a.b/x?q=1&r=2", Path::new("/usr/bin/lynx")), "/usr/bin/lynx https://a.b/x?q=1&r=2");
        let u = launch_command("https://x.org/a;b|c$d`e(f)~g'h\"i", Path::new("/usr/bin/lynx"));
        for ch in [';', '|', '$', '`', '(', ')', '~', '\'', '"'] {
            assert!(!u.contains(ch), "{ch} left in {u}");
        }
        assert_eq!(normalize("example.com").unwrap(), "https://example.com/");
    }

    #[test]
    fn page_info_opens_from_the_menu_and_any_key_closes_it() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.tick(&ctx);
        settle(&mut b, &ctx);
        assert!(b.menu().iter().any(|m| m.id == "info"));
        b.navigate("http://frames.example/".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        b.menu_action("info", &ctx);
        assert!(b.info);
        // the card is closed by a key without side effects
        let pos = b.pos;
        press(&mut b, Keycode::Backspace, &ctx);
        assert!(!b.info);
        assert_eq!(b.pos, pos);
        let p = b.page.as_ref().unwrap();
        assert_eq!(p.charset, "Shift_JIS");
        assert_eq!(p.frames, 2);
        assert!(p.bytes > 0);
        // reader toggling keeps the info
        b.toggle_reader(&ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.page.as_ref().unwrap().charset, "Shift_JIS");
    }

    #[test]
    fn the_page_owns_the_footer_row_inside_its_frame() {
        let mut b = new_browser();
        for bar in [false, true] {
            b.show_bar = bar;
            let f = b.frame_r();
            assert_eq!(f.bottom(), grid::APP_FRAME_BOTTOM + 1, "the frame runs down to y 168");
            if bar {
                assert_eq!(f.y(), back_r().bottom() + 1, "1 px between the toolbar and the frame");
            } else {
                assert_eq!(f.y(), grid::app_frame().y(), "without the toolbar the frame starts where every app's does");
            }
            let ct = grid::app_content_from(f.y());
            assert_eq!((VIEW_X, b.vy(), VIEW_X + VIEW_W, b.vy() + b.vh()), (ct.x(), f.y() + 1, b.sb_r().x() - 1, ct.bottom()));
            assert!(b.sb_r().right() <= ct.right() && b.sb_r().bottom() < ct.bottom(), "the scroll bar keeps clear of the line and its corners");
        }
        assert_eq!(VIEW_W - 4, load::WIDTH, "the layout width is unchanged");
        assert!(addr_r().right() <= grid::SCROLL_X + grid::SCROLL_W && reload_r().right() + 2 < addr_r().x());
    }

    #[test]
    fn the_toolbar_is_hidden_until_the_address_is_typed() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        assert!(!b.bar(), "hidden by default");
        let page_h = b.vh();
        b.key(Keycode::L, Mod::LCTRLMOD, false, &ctx);
        assert!(b.bar() && b.vh() == page_h - 16, "Ctrl+L shows it; the page gives up one row");
        b.key(Keycode::Escape, Mod::empty(), false, &ctx);
        assert!(!b.bar(), "Esc hides it again");
        // A click on the top row (above the page) shows it too.
        b.pointer(PointerEvent::Down { x: 200, y: grid::BAR_H, button: 0, clicks: 1 }, &ctx);
        assert_eq!(b.edit, Edit::Address);
        b.pointer(PointerEvent::Up { x: 200, y: grid::BAR_H, button: 0 }, &ctx);
        b.key(Keycode::Escape, Mod::empty(), false, &ctx);
        // The menu switch keeps it.
        b.menu_action("toolbar", &ctx);
        assert!(b.show_bar && b.bar());
        b.menu_action("toolbar", &ctx);
        assert!(!b.bar());
    }

    #[test]
    fn about_pages() {
        let mut b = NamedList::new("Bookmarks");
        b.push_front("https://a.b/", "A <b>", 9);
        let h = NamedList::new("History");
        assert!(home_html(&b).contains("A &lt;b&gt;"));
        assert!(about_html("about:bookmarks", &b, &h).contains("https://a.b/"));
        assert!(about_html("about:history", &b, &h).contains("Nothing visited"));
        assert!(about_html("about:nope", &b, &h).contains("Unknown"));
        assert!(help_html().contains("Backspace"));
    }

    #[test]
    fn starts_on_home_and_navigates_with_history() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.tick(&ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.page.as_ref().unwrap().title, "Harbour Browser");
        b.navigate("https://example.org/".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.page.as_ref().unwrap().title, "Example Domain");
        b.navigate("https://example.org/news".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        assert_eq!((b.nav.len(), b.pos), (3, 2));
        press(&mut b, Keycode::Backspace, &ctx);
        settle(&mut b, &ctx);
        assert_eq!((b.pos, b.page.as_ref().unwrap().title.as_str()), (1, "Example Domain"));
        b.key(Keycode::Backspace, Mod::LSHIFTMOD, false, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.pos, 2);
        // a new page after going back drops the forward entries
        press(&mut b, Keycode::Backspace, &ctx);
        settle(&mut b, &ctx);
        b.navigate("https://example.org/nothing".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        assert_eq!((b.nav.len(), b.pos), (3, 2));
        assert!(b.history.contains("https://example.org/"));
    }

    #[test]
    fn tab_enter_follows_links_and_scrolls() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.navigate("https://example.org/".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        press(&mut b, Keycode::Tab, &ctx);
        assert!(matches!(b.selected(), Some(Focus::Link(0))));
        press(&mut b, Keycode::Return, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.page.as_ref().unwrap().url, "https://example.org/news");
        // scrolling is clamped
        press(&mut b, Keycode::End, &ctx);
        assert_eq!(b.scroll, b.max_scroll());
        for _ in 0..50 {
            press(&mut b, Keycode::Down, &ctx);
        }
        assert_eq!(b.scroll, b.max_scroll());
        press(&mut b, Keycode::Home, &ctx);
        press(&mut b, Keycode::Up, &ctx);
        assert_eq!(b.scroll, 0);
    }

    #[test]
    fn address_bar_search_and_slash_is_swallowed() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.tick(&ctx);
        settle(&mut b, &ctx);
        press(&mut b, Keycode::Slash, &ctx);
        typed(&mut b, "/", &ctx);
        assert_eq!(b.edit, Edit::Address);
        assert!(b.addr.starts_with("about:") && !b.addr.contains('/'));
        typed(&mut b, "rust lang", &ctx);
        assert_eq!(b.addr, "rust lang", "first typing replaces the selected address");
        press(&mut b, Keycode::Return, &ctx);
        settle(&mut b, &ctx);
        let p = b.page.as_ref().unwrap();
        assert_eq!(p.title, "Search results");
        assert!(p.url.starts_with(url::SEARCH));
        assert_eq!(b.edit, Edit::None);
    }

    #[test]
    fn form_typing_and_submit() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.navigate("https://example.org/".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        let field = b.page.as_ref().unwrap().focus.iter().position(|f| matches!(f.0, Focus::Field(_))).unwrap();
        b.sel = Some(field);
        press(&mut b, Keycode::Return, &ctx);
        assert!(matches!(b.edit, Edit::Field(_)));
        typed(&mut b, "ab", &ctx);
        press(&mut b, Keycode::Backspace, &ctx);
        typed(&mut b, "c", &ctx);
        press(&mut b, Keycode::Return, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.page.as_ref().unwrap().url, "https://lite.duckduckgo.com/lite/?q=ac");
    }

    #[test]
    fn escape_cancels_a_load() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.tick(&ctx);
        settle(&mut b, &ctx);
        b.navigate("https://example.org/".into(), None, Mode::Push, &ctx);
        assert!(b.loading.is_some());
        press(&mut b, Keycode::Escape, &ctx);
        assert!(b.loading.is_none());
        assert_eq!(b.status, "Cancelled");
        assert_eq!(b.page.as_ref().unwrap().title, "Harbour Browser", "the old page stays");
    }

    #[test]
    fn click_hit_testing_and_scroll_bar() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.navigate("https://example.org/".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        let it = b.page.as_ref().unwrap().items.iter().find(|i| i.link == Some(0)).unwrap().clone();
        let (sx, sy) = (PAGE_X + it.x + 2, b.vy() + PAGE_Y + it.y - b.scroll + 3);
        b.pointer(PointerEvent::Move { x: sx, y: sy }, &ctx);
        assert_eq!(b.hover, Some(Focus::Link(0)));
        b.pointer(PointerEvent::Down { x: sx, y: sy, button: 0, clicks: 1 }, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.page.as_ref().unwrap().url, "https://example.org/news");
        // wheel + scroll bar
        b.pointer(PointerEvent::Wheel { x: 100, y: 80, dy: -1 }, &ctx);
        assert_eq!(b.scroll, 2 * SCROLL_LINE.min(b.max_scroll()));
        b.pointer(PointerEvent::Down { x: b.sb_r().x() + 1, y: b.sb_r().bottom() - 2, button: 0, clicks: 1 }, &ctx);
        assert!(b.scroll > 2 * SCROLL_LINE.min(b.max_scroll()) || b.scroll == b.max_scroll());
        // toolbar back button (with the toolbar kept on)
        b.pointer(PointerEvent::Up { x: 0, y: 0, button: 0 }, &ctx);
        b.set_show_bar(true);
        b.pointer(PointerEvent::Down { x: back_r().x() + 2, y: back_r().y() + 2, button: 0, clicks: 1 }, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.page.as_ref().unwrap().url, "https://example.org/");
    }

    #[test]
    fn bookmarks_add_and_remove() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.navigate("https://example.org/".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.menu_action("bookmark", &ctx), AppAction::Toast("Bookmark added".into()));
        assert_eq!(b.bookmarks.entries[0].title, "Example Domain");
        assert!(b.menu().iter().any(|m| m.label == "Remove bookmark"));
        b.menu_action("bookmarks", &ctx);
        settle(&mut b, &ctx);
        assert!(b.page.as_ref().unwrap().links.contains(&"https://example.org/".to_string()));
        b.navigate("https://example.org/".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.menu_action("bookmark", &ctx), AppAction::Toast("Bookmark removed".into()));
        assert!(b.bookmarks.entries.is_empty());
    }

    #[test]
    fn open_in_lynx_uses_the_current_url() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.lynx = Some(PathBuf::from("/usr/bin/lynx"));
        b.tick(&ctx);
        settle(&mut b, &ctx);
        assert!(matches!(b.menu_action("lynx", &ctx), AppAction::Toast(_)), "about: pages are refused");
        b.navigate("https://example.org/".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.menu_action("lynx", &ctx), AppAction::Launch("/usr/bin/lynx https://example.org/".into()));
    }

    #[test]
    fn fragment_links_jump_without_fetching() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.navigate("https://example.org/".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        let n = b.nav.len();
        b.navigate("https://example.org/#lists".into(), None, Mode::Push, &ctx);
        assert!(b.loading.is_none(), "no fetch for a jump");
        assert_eq!(b.nav.len(), n + 1);
        assert!(b.scroll > 0 || b.max_scroll() == 0);
    }

    #[test]
    fn local_files_load() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = Browser::new(Arc::new(fetch::Real), true);
        b.navigate(format!("file://{}/tests/fixtures/news.html", env!("CARGO_MANIFEST_DIR")), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        assert!(b.page.as_ref().unwrap().title.contains("Harbour"));
        b.navigate("file:///no/such/file.html".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.page.as_ref().unwrap().title, "Error");
    }

    #[test]
    fn reader_mode_toggles_and_is_remembered_per_site() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.navigate("https://example.org/article".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        let normal_links = b.page.as_ref().unwrap().links.len();
        assert!(normal_links >= 5 && !b.page.as_ref().unwrap().reader);
        press(&mut b, Keycode::R, &ctx);
        settle(&mut b, &ctx);
        let p = b.page.as_ref().unwrap();
        assert!(p.reader && p.links.len() < normal_links, "only the article's own link stays");
        assert_eq!(b.nav.len(), 1, "no history entry for a layout switch");
        assert!(b.menu().iter().any(|m| m.label == "Normal view"));
        // another page of the same site opens in reader view; a long article is kept
        b.navigate("https://example.org/article".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        assert!(b.page.as_ref().unwrap().reader);
        // a page without prose falls back and forgets the choice
        b.navigate("https://example.org/".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        assert!(b.reader_sites.contains("example.org"));
        // toggle off
        b.navigate("https://example.org/article".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        press(&mut b, Keycode::R, &ctx);
        settle(&mut b, &ctx);
        assert!(!b.page.as_ref().unwrap().reader && b.reader_sites.is_empty());
        // typing 'r' in the address bar does not toggle
        b.focus_address();
        b.key(Keycode::R, Mod::empty(), false, &ctx);
        assert!(b.edit == Edit::Address && !b.page.as_ref().unwrap().reader);
    }

    #[test]
    fn back_returns_to_the_scroll_position() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.navigate("https://example.org/article".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        for _ in 0..5 {
            press(&mut b, Keycode::Down, &ctx);
        }
        let at = b.scroll;
        assert!(at > 0);
        b.navigate("https://example.org/news".into(), None, Mode::Push, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.scroll, 0);
        press(&mut b, Keycode::Backspace, &ctx);
        settle(&mut b, &ctx);
        assert_eq!(b.scroll, at);
    }

    #[test]
    fn a_page_appears_before_the_download_ends() {
        let (f, bk, t) = ctx_parts();
        let ctx = ctx!(f, bk, t);
        let mut b = new_browser();
        b.navigate("https://example.org/article".into(), None, Mode::Push, &ctx);
        let mut early = None;
        for _ in 0..300 {
            b.tick(&ctx);
            if b.loading.is_some() && b.page.is_some() {
                early = Some(b.page.as_ref().unwrap().height);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let early = early.expect("a partial page");
        assert!(b.nav.is_empty(), "history is untouched until the load ends");
        settle(&mut b, &ctx);
        assert!(b.page.as_ref().unwrap().height > early);
        assert_eq!(b.nav.len(), 1);
    }
}
