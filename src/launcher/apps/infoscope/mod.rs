//! Infoscope: one window with channels, after Sherlock 2 — a row of channel tabs at the top
//! (News / Search / Nearby / Inbox) and the results below, in whole 16 px rows.
//!
//! Keys: Tab / Shift+Tab (or Left / Right, 1-4 outside the search field) change the channel;
//! Up / Down select; Enter opens; Esc goes back. Ctrl+F jumps to Search. Alt opens the menu.
//!
//! News reads RSS / Atom feeds (fetched by curl on a worker thread, the last list cached for
//! offline use) and opens an article in the Browser. Search finds file names on the device and
//! hands the query to the Browser as a web search. Nearby and Inbox come in later stages.
//!
//! There is no dictionary lookup: the only dictionary on the device is the input method's, which
//! maps readings to words (no meanings), so it does not answer "what does this word mean".

mod news;
mod search;

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use chrono::{Local, TimeZone};
use sdl2::keyboard::{Keycode, Mod};
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome;
use crate::font::FontSystem;
use crate::theme::ThemeManager;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::feed::{self, NewsItem};
use crate::launcher::i18n::{tr, tr_s};
use crate::launcher::lineedit::LineEdit;
use crate::launcher::widgets::{self, MenuItem, PointerEvent, SbPart, ScrollBar};
use crate::platform::keys::arrow_alias;
use crate::ui;

use news::{Article, Articles, Cache, FeedResult, Store, Subscriptions};
use crate::launcher::apps::browser::layout::{self as page_layout, Metrics};

const CHANNELS: [&str; 4] = ["News", "Search", "Nearby", "Inbox"];
const NEWS: usize = 0;
const SEARCH: usize = 1;
const NEARBY: usize = 2;
const INBOX: usize = 3;

const ROW: i32 = 16;
/// The folder tab strip (the shared tab design), then the page of list rows from y 36.
const LIST_Y: i32 = widgets::FOLDER_BODY_Y;
/// Whole 16 px rows from under the tab strip down to the screen's usable bottom (y 168, like the
/// framed screens and Settings): 8 rows, y 36..163. The 5 px left over (y 164..168) are the
/// page's bottom edge; y 169 is never drawn.
fn list_rows() -> usize {
    ((crate::grid::frame_bottom() + 1 - LIST_Y) / ROW) as usize
}
const LEFT: i32 = 6;
const RIGHT: i32 = 312;
/// Right edge of list text: clear of the scroll bar column.
fn list_right() -> i32 {
    crate::grid::scroll_x() - 4
}
/// Refresh on opening when the cached list is older than this (seconds).
const STALE: i64 = 30 * 60;
/// Page slide: as long as Settings' (same ease, same directions).
const SLIDE_MS: u64 = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NewsView {
    Items,
    /// One article's text, read inside Infoscope.
    Article,
    Feeds,
    AddFeed,
}

/// What the area under the tab strip shows: the channel, News' page, and the line shown in place
/// of an empty headline list ("Loading..."). The slide keeps the one it left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PageKey {
    channel: usize,
    view: NewsView,
    note: Option<&'static str>,
}

impl PageKey {
    /// How deep a News page is: going deeper slides in from the right, back from the left.
    fn depth(&self) -> u8 {
        match self.view {
            NewsView::Items => 0,
            NewsView::Article | NewsView::Feeds => 1,
            NewsView::AddFeed => 2,
        }
    }
}

/// A page change being animated: the page left (`None`: nothing yet, the app just opened).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slide {
    from: Option<PageKey>,
    start: Instant,
    /// +1: the new page comes in from the right; -1: from the left.
    dir: i32,
}

/// The slide's direction from one page to another: channels by their order in the tab strip,
/// News' pages by depth; opening the app, or the first headlines arriving, come from the right.
fn slide_dir(from: Option<PageKey>, to: PageKey) -> i32 {
    match from {
        Some(f) if f.channel != to.channel => if to.channel > f.channel { 1 } else { -1 },
        Some(f) if to.depth() < f.depth() => -1,
        _ => 1,
    }
}

/// Offsets (old page, new page) at `t` (0..1 of the slide), with Settings' ease-out cubic.
fn slide_offsets(dir: i32, t: f32, w: i32) -> (i32, i32) {
    let e = 1.0 - (1.0 - t).powi(3);
    let w = w as f32;
    if dir > 0 { (-(w * e) as i32, (w * (1.0 - e)) as i32) } else { ((w * e) as i32, -(w * (1.0 - e)) as i32) }
}

/// A list position: selected row and first visible row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Pos {
    cursor: usize,
    scroll: usize,
}

impl Pos {
    fn step(&mut self, d: isize, len: usize, visible: usize) {
        if len == 0 {
            *self = Pos::default();
            return;
        }
        self.cursor = (self.cursor as isize + d).clamp(0, len as isize - 1) as usize;
        self.follow(len, visible);
    }
    fn follow(&mut self, len: usize, visible: usize) {
        self.cursor = self.cursor.min(len.saturating_sub(1));
        self.scroll = widgets::follow_scroll(self.cursor, self.scroll, visible, len);
    }
}

/// A fetch in progress: which feeds, what has come back so far.
struct Refresh {
    serial: u64,
    urls: Vec<String>,
    got: Vec<FeedResult>,
}

/// The article open in the reader.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reading {
    link: String,
    /// `None` while the page is being read.
    paragraphs: Option<Vec<String>>,
    /// The page could not be read: this is the feed's own text.
    from_feed: bool,
    /// Neither the page nor the feed had any text (`paragraphs` is empty).
    failed: bool,
}

/// One 16 px row of the article as laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    Title(String),
    /// Source and date on an ink plate.
    Meta(String),
    /// Body text, `indent` px in (a paragraph's first line).
    Text(String, i32),
    /// "Loading..." and the like, centred.
    Note(&'static str),
}

/// One row of Search's results.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Hit {
    File(search::Entry),
    Web,
}

pub struct Infoscope {
    channel: usize,
    view: NewsView,
    store: Store,
    subs: Subscriptions,
    cache: Cache,
    items: Pos,
    feeds: Pos,
    add: LineEdit,
    /// Feed waiting for "Enter = remove".
    confirm: Option<usize>,
    refresh: Option<Refresh>,
    serial: u64,
    tx: Sender<(u64, FeedResult)>,
    rx: Receiver<(u64, FeedResult)>,
    /// A short line in the channel row ("Updated 12:30", "Offline", an error).
    status: String,
    mock: bool,
    // Search
    query: LineEdit,
    hits: Vec<Hit>,
    found: Pos,
    index: Option<Vec<search::Entry>>,
    index_rx: Option<Receiver<Vec<search::Entry>>>,
    search_root: Option<PathBuf>,
    dirty: bool,
    // Article reader
    articles: Articles,
    reading: Option<Reading>,
    art_tx: Sender<(String, Result<Vec<String>, String>)>,
    art_rx: Receiver<(String, Result<Vec<String>, String>)>,
    /// The article laid out (rebuilt when its text changes) and the first row shown.
    lines: Option<Vec<Line>>,
    art_scroll: usize,
    /// The page last shown (`None` before the first frame) and the slide away from the one before.
    shown: Option<PageKey>,
    slide: Option<Slide>,
    /// The list's scroll bar thumb is being dragged.
    sb_drag: bool,
}

pub fn make() -> Box<dyn LauncherApp> {
    let mock = matches!(std::env::var("HARBOUR_MOCK").as_deref(), Ok("1" | "true" | "yes" | "on"));
    let store = if mock { Store { dir: None } } else { Store::home() };
    let root = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_dir());
    let mut app = Infoscope::new(store, root, mock);
    let now = Local::now().timestamp();
    if app.cache.items.is_empty() || app.cache.fetched.is_none_or(|t| now - t > STALE) {
        app.start_refresh(None);
    }
    Box::new(app)
}

impl Infoscope {
    fn new(store: Store, search_root: Option<PathBuf>, mock: bool) -> Self {
        let (tx, rx) = channel();
        let (art_tx, art_rx) = channel();
        let subs = store.load_subs();
        let cache = store.load_cache();
        let articles = store.load_articles();
        Infoscope {
            channel: NEWS,
            view: NewsView::Items,
            store,
            subs,
            cache,
            items: Pos::default(),
            feeds: Pos::default(),
            add: LineEdit::default(),
            confirm: None,
            refresh: None,
            serial: 0,
            tx,
            rx,
            status: String::new(),
            mock,
            query: LineEdit::default(),
            hits: Vec::new(),
            found: Pos::default(),
            index: None,
            index_rx: None,
            search_root,
            dirty: true,
            articles,
            reading: None,
            art_tx,
            art_rx,
            lines: None,
            art_scroll: 0,
            shown: None,
            slide: None,
            sb_drag: false,
        }
    }

    // ------------------------------------------------------------- news

    /// Fetch every feed (`None`) or just one address. Results arrive in [`Self::poll`].
    fn start_refresh(&mut self, only: Option<String>) {
        let urls: Vec<String> = match only {
            Some(u) => vec![u],
            None => self.subs.feeds.iter().map(|f| f.url.clone()).collect(),
        };
        if urls.is_empty() {
            self.status = "No feeds".into();
            return;
        }
        self.serial += 1;
        let serial = self.serial;
        self.refresh = Some(Refresh { serial, urls: urls.clone(), got: Vec::new() });
        self.status = "Loading...".into();
        let tx = self.tx.clone();
        let mock = self.mock;
        std::thread::spawn(move || {
            for u in urls {
                let r = if mock { news::mock_feed(&u) } else { news::read_feed(&u) };
                if tx.send((serial, r)).is_err() {
                    break;
                }
            }
        });
    }

    /// Take finished feeds; when the last one is in, merge them into the list and the cache.
    fn poll(&mut self, now: i64) -> bool {
        let mut changed = false;
        while let Ok((serial, r)) = self.rx.try_recv() {
            let Some(job) = self.refresh.as_mut().filter(|j| j.serial == serial) else { continue };
            job.got.push(r);
            changed = true;
            if job.got.len() == job.urls.len() {
                let job = self.refresh.take().unwrap();
                self.finish(job.got, now);
            }
        }
        changed
    }

    fn finish(&mut self, got: Vec<FeedResult>, now: i64) {
        let mut fresh: Vec<Vec<NewsItem>> = Vec::new();
        let mut refreshed: Vec<String> = Vec::new();
        let mut failed = 0;
        let mut subs_changed = false;
        for (url, r) in got {
            let Some(f) = self.subs.feeds.iter_mut().find(|f| f.url == url) else { continue };
            match r {
                Ok(p) => {
                    let old = f.label().to_string();
                    if !p.title.is_empty() && p.title != f.title {
                        f.title = p.title.clone();
                        subs_changed = true;
                    }
                    refreshed.push(old);
                    refreshed.push(f.label().to_string());
                    let src = f.label().to_string();
                    fresh.push(p.items.into_iter().map(|mut i| {
                        i.source = src.clone();
                        i
                    }).collect());
                }
                Err(_) => failed += 1,
            }
        }
        if subs_changed {
            let _ = self.store.save_subs(&self.subs);
        }
        let ok = fresh.len();
        // Keep the cached items of feeds that did not answer (offline), drop removed feeds' items.
        let labels: Vec<String> = self.subs.feeds.iter().map(|f| f.label().to_string()).collect();
        let kept: Vec<NewsItem> = self.cache.items.iter()
            .filter(|i| labels.contains(&i.source) && !refreshed.contains(&i.source))
            .cloned().collect();
        let selected = self.cache.items.get(self.items.cursor).map(|i| i.link.clone());
        fresh.push(kept);
        self.cache.items = feed::merge(&fresh, news::KEEP);
        if ok > 0 {
            self.cache.fetched = Some(now);
            let _ = self.store.save_cache(&self.cache);
        }
        // Stay on the same headline when it is still there.
        self.items.cursor = selected.and_then(|l| self.cache.items.iter().position(|i| i.link == l)).unwrap_or(0);
        self.items.follow(self.cache.items.len(), list_rows());
        self.status = match (ok, failed) {
            (0, _) => "Offline".into(),
            (_, 0) => String::new(),
            (_, n) => format!("{n} feeds failed"),
        };
    }

    fn selected_item(&self) -> Option<&NewsItem> {
        self.cache.items.get(self.items.cursor)
    }

    fn open_link(link: &str) -> AppAction {
        AppAction::Open { app: Some("Browser".into()), path: PathBuf::from(link) }
    }

    fn subscribe(&mut self) {
        let text = self.add.text.clone();
        match self.subs.add(&text) {
            Ok(()) => {
                let _ = self.store.save_subs(&self.subs);
                let url = self.subs.feeds.last().unwrap().url.clone();
                self.add.clear();
                self.view = NewsView::Feeds;
                self.feeds.cursor = self.subs.feeds.len() - 1;
                self.feeds.follow(self.subs.feeds.len() + 1, list_rows());
                self.start_refresh(Some(url));
            }
            Err(e) => self.status = e,
        }
    }

    fn remove_feed(&mut self, i: usize) {
        if let Some(f) = self.subs.remove(i) {
            let _ = self.store.save_subs(&self.subs);
            let label = f.label().to_string();
            self.cache.items.retain(|it| it.source != label);
            let _ = self.store.save_cache(&self.cache);
            self.items.follow(self.cache.items.len(), list_rows());
            self.feeds.follow(self.subs.feeds.len() + 1, list_rows());
            self.status = "Feed removed".into();
        }
        self.confirm = None;
    }

    // ---------------------------------------------------------- article

    /// Open the selected headline in the reader: the saved text at once, else read the page on a
    /// worker thread (the feed's own text when the page has none or the network is down).
    fn open_article(&mut self) {
        let Some(link) = self.selected_item().map(|i| i.link.clone()) else { return };
        self.view = NewsView::Article;
        self.art_scroll = 0;
        self.lines = None;
        if let Some(a) = self.articles.get(&link) {
            self.reading = Some(Reading { link, paragraphs: Some(a.paragraphs.clone()), from_feed: false, failed: false });
            return;
        }
        self.reading = Some(Reading { link: link.clone(), paragraphs: None, from_feed: false, failed: false });
        let tx = self.art_tx.clone();
        let mock = self.mock;
        std::thread::spawn(move || {
            let r = if mock { news::mock_article(&link) } else { news::read_article(&link) };
            let _ = tx.send((link, r));
        });
    }

    /// Take finished article reads: keep the text, show it if that article is still open.
    fn poll_articles(&mut self, now: i64) -> bool {
        let mut changed = false;
        while let Ok((link, r)) = self.art_rx.try_recv() {
            if let Ok(p) = &r {
                self.articles.put(Article { link: link.clone(), fetched: now, paragraphs: p.clone() });
                let _ = self.store.save_articles(&self.articles);
            }
            let fallback = self.cache.items.iter().find(|i| i.link == link).map(news::feed_text).unwrap_or_default();
            let Some(rd) = self.reading.as_mut().filter(|rd| rd.link == link && rd.paragraphs.is_none()) else { continue };
            match r {
                Ok(p) => rd.paragraphs = Some(p),
                Err(_) => {
                    rd.from_feed = !fallback.is_empty();
                    rd.failed = fallback.is_empty();
                    rd.paragraphs = Some(fallback);
                }
            }
            self.lines = None;
            changed = true;
        }
        changed
    }

    /// The open article's headline (the item may have left the list since: then the link).
    fn reading_item(&self) -> Option<&NewsItem> {
        let rd = self.reading.as_ref()?;
        self.cache.items.iter().find(|i| i.link == rd.link)
    }

    /// Lay the open article out in rows `list_right() - LEFT` wide (kept until its text changes).
    fn layout_article(&mut self, fs: &FontSystem) {
        self.layout_article_with(&|t: &str| fs.measure_text(t));
    }

    /// [`Self::layout_article`] with any measure of drawn text (the tests use the fonts' metrics).
    fn layout_article_with(&mut self, measure: &dyn Fn(&str) -> i32) {
        if self.lines.is_some() {
            return;
        }
        let Some(rd) = self.reading.clone() else { return };
        let m = Metrics::fixed(6, 12);
        let w = list_right() - LEFT;
        let mut out = Vec::new();
        let (title, meta) = match self.reading_item() {
            Some(it) => {
                let when = it.date.and_then(|d| Local.timestamp_opt(d, 0).single()).map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                (it.title.clone(), if when.is_empty() { it.source.clone() } else { format!("{}  {}", it.source, when) })
            }
            None => (rd.link.clone(), String::new()),
        };
        out.extend(page_layout::wrap_text(&m, &title, w, 0, measure).into_iter().map(Line::Title));
        if !meta.is_empty() {
            out.push(Line::Meta(meta));
        }
        match &rd.paragraphs {
            None => out.push(Line::Note("Loading...")),
            Some(_) if rd.failed => out.push(Line::Note("Could not read the article.")),
            Some(ps) => {
                // The page's own heading repeats the headline: leave it out.
                let same = |a: &str, b: &str| a.split_whitespace().eq(b.split_whitespace());
                let skip = usize::from(ps.first().is_some_and(|p| same(p, &title)));
                for para in &ps[skip..] {
                    let indent = if para.starts_with("* ") { 0 } else { 12 };
                    for (k, l) in page_layout::wrap_text(&m, para, w, indent, measure).into_iter().enumerate() {
                        out.push(Line::Text(l, if k == 0 { indent } else { 0 }));
                    }
                }
            }
        }
        self.lines = Some(out);
        self.art_scroll = self.art_scroll.min(self.art_max());
    }

    fn art_len(&self) -> usize {
        self.lines.as_ref().map_or(0, |l| l.len())
    }

    fn art_max(&self) -> usize {
        self.art_len().saturating_sub(list_rows())
    }

    fn scroll_article(&mut self, d: isize) {
        self.art_scroll = (self.art_scroll as isize + d).clamp(0, self.art_max() as isize) as usize;
    }

    /// The scroll bar of the list on screen (None when the page has no list or it fits).
    fn scrollbar(&self) -> Option<ScrollBar> {
        let (scroll, len, rows, first) = match (self.channel, self.view) {
            (NEWS, NewsView::Items) => (self.items.scroll, self.cache.items.len(), list_rows(), 0),
            (NEWS, NewsView::Article) => (self.art_scroll, self.art_len(), list_rows(), 0),
            (NEWS, NewsView::Feeds) => (self.feeds.scroll, self.subs.feeds.len() + 1, list_rows(), 0),
            (SEARCH, _) if !self.query.is_empty() => (self.found.scroll, self.hits.len(), list_rows() - 1, 1),
            _ => return None,
        };
        (len > rows).then(|| ScrollBar::new(bar_rect(first, rows), len, rows, scroll))
    }

    /// Scroll the list on screen to `to` (from the scroll bar); the selection is pulled into view.
    fn set_scroll(&mut self, to: usize) {
        let pull = |p: &mut Pos, rows: usize| {
            p.scroll = to;
            p.cursor = p.cursor.clamp(to, to + rows - 1);
        };
        match (self.channel, self.view) {
            (NEWS, NewsView::Items) => pull(&mut self.items, list_rows()),
            (NEWS, NewsView::Article) => self.art_scroll = to.min(self.art_max()),
            (NEWS, NewsView::Feeds) => {
                self.confirm = None;
                pull(&mut self.feeds, list_rows())
            }
            (SEARCH, _) => pull(&mut self.found, list_rows() - 1),
            _ => {}
        }
    }

    // ----------------------------------------------------------- search

    fn enter_search(&mut self) {
        self.channel = SEARCH;
        if self.index.is_none() && self.index_rx.is_none() {
            if let Some(root) = self.search_root.clone() {
                let (tx, rx) = channel();
                self.index_rx = Some(rx);
                std::thread::spawn(move || {
                    let _ = tx.send(search::index(&root));
                });
            }
        }
    }

    fn update_hits(&mut self) {
        let q = self.query.text.trim();
        self.hits.clear();
        if !q.is_empty() {
            if let Some(idx) = &self.index {
                self.hits.extend(search::find(idx, q).into_iter().cloned().map(Hit::File));
            }
            self.hits.push(Hit::Web);
        }
        self.found = Pos::default();
    }

    fn open_hit(&self) -> AppAction {
        match self.hits.get(self.found.cursor) {
            Some(Hit::File(e)) if e.dir => AppAction::Open { app: Some("Files".into()), path: e.path.clone() },
            Some(Hit::File(e)) => AppAction::Open { app: None, path: e.path.clone() },
            Some(Hit::Web) => Self::open_link(&search::web_url(&self.query.text)),
            None => AppAction::None,
        }
    }

    // ---------------------------------------------------------- general

    fn set_channel(&mut self, ch: usize) {
        self.confirm = None;
        if ch == SEARCH {
            self.enter_search();
        } else {
            self.channel = ch;
        }
    }

    fn cycle(&mut self, d: isize) {
        let n = CHANNELS.len() as isize;
        self.set_channel(((self.channel as isize + d).rem_euclid(n)) as usize);
    }

    /// Esc: one step back. True if something changed.
    fn back(&mut self) -> bool {
        if self.confirm.take().is_some() {
            self.status.clear();
            return true;
        }
        match (self.channel, self.view) {
            (NEWS, NewsView::Feeds) => self.view = NewsView::Items,
            (NEWS, NewsView::Article) => {
                self.view = NewsView::Items;
                self.status.clear();
            }
            (NEWS, NewsView::AddFeed) => {
                self.add.clear();
                self.view = NewsView::Feeds;
            }
            (SEARCH, _) if !self.query.is_empty() => {
                self.query.clear();
                self.update_hits();
            }
            // An empty search field: Esc leaves it (the Search channel is always typing, so
            // Left / Right / 1-4 go to the field; without this only Tab got out).
            (SEARCH, _) => self.set_channel(NEWS),
            _ => return false,
        }
        true
    }

    fn activate(&mut self) -> AppAction {
        match (self.channel, self.view) {
            // Enter reads the article here; the Browser is only the menu's "Open in Browser".
            (NEWS, NewsView::Items) => self.open_article(),
            (NEWS, NewsView::Feeds) => {
                if let Some(i) = self.confirm {
                    self.remove_feed(i);
                } else if self.feeds.cursor >= self.subs.feeds.len() {
                    self.view = NewsView::AddFeed;
                    self.status.clear();
                }
            }
            (NEWS, NewsView::AddFeed) => self.subscribe(),
            (SEARCH, _) => return self.open_hit(),
            _ => {}
        }
        AppAction::None
    }

    fn move_sel(&mut self, d: isize) {
        match (self.channel, self.view) {
            (NEWS, NewsView::Items) => self.items.step(d, self.cache.items.len(), list_rows()),
            (NEWS, NewsView::Article) => self.scroll_article(d),
            (NEWS, NewsView::Feeds) => {
                self.confirm = None;
                self.feeds.step(d, self.subs.feeds.len() + 1, list_rows());
            }
            (SEARCH, _) => self.found.step(d, self.hits.len(), list_rows() - 1),
            _ => {}
        }
    }

    /// True while typed text goes into a field.
    fn typing(&self) -> bool {
        self.channel == SEARCH || (self.channel == NEWS && self.view == NewsView::AddFeed)
    }

    /// The page on screen now.
    fn page(&self) -> PageKey {
        let view = if self.channel == NEWS { self.view } else { NewsView::Items };
        let note = (self.channel == NEWS && view == NewsView::Items && self.cache.items.is_empty()).then(|| self.empty_note());
        PageKey { channel: self.channel, view, note }
    }

    /// The line shown when there are no headlines.
    fn empty_note(&self) -> &'static str {
        if self.refresh.is_some() {
            "Loading..."
        } else if self.subs.feeds.is_empty() {
            "No feeds. Alt: Add feed..."
        } else {
            "No news yet. Alt: Refresh"
        }
    }

    /// Start a slide when the page changed since the last frame, end it when it has run.
    /// Only the moment the page changes moves; keys never wait for it (state changes at once,
    /// a newer change restarts the slide from the page then on screen).
    fn sync_slide(&mut self, now: Instant) {
        if self.slide.is_some_and(|s| now.duration_since(s.start) >= Duration::from_millis(SLIDE_MS)) {
            self.slide = None;
        }
        let to = self.page();
        let from = self.shown;
        if from == Some(to) {
            return;
        }
        self.shown = Some(to);
        let same_page = from.is_some_and(|f| f.channel == to.channel && f.view == to.view);
        if same_page {
            // Only the empty-list line changed. The first headlines arriving slide in (unless a
            // slide is still running: then that one carries them); one note to another is instant.
            let arrived = from.is_some_and(|f| f.note.is_some()) && to.note.is_none();
            if !arrived || self.slide.is_some() {
                return;
            }
        }
        self.slide = Some(Slide { from, start: now, dir: slide_dir(from, to) });
    }

    fn ask_remove(&mut self) {
        if self.channel == NEWS && self.view == NewsView::Feeds && self.feeds.cursor < self.subs.feeds.len() {
            self.confirm = Some(self.feeds.cursor);
        }
    }
}

// --------------------------------------------------------------- drawing

struct Paint {
    face: Color,
    ink: Color,
    muted: Color,
    /// Horizontal shift of the page being drawn (the slide between pages).
    ox: i32,
}

impl Paint {
    /// List row `i`, shifted with the page.
    fn row(&self, i: usize) -> Rect {
        let r = row_rect(i);
        Rect::new(r.x() + self.ox, r.y(), r.width(), r.height())
    }
}

fn paint(ctx: &Ctx) -> Paint {
    let tm = ctx.tm;
    let t = widgets::tones(tm);
    // The face is the theme's white (icon_white); Invert themes lift it to a dark tone, and there
    // the plain light tone is used instead (as Radio's meter does).
    let lum = |c: Color| c.r as u32 * 3 + c.g as u32 * 6 + c.b as u32;
    let white = tm.get_color("icon_white");
    let face = if lum(white) * 2 >= lum(t.light) + lum(t.dark) { white } else { t.light };
    let muted = tm.get_color("muted");
    // `muted` must still read on the face; otherwise fall back to the ink.
    let diff = |a: Color, b: Color| (lum(a) as i32 - lum(b) as i32).unsigned_abs();
    let muted = if diff(muted, face) * 3 >= diff(t.dark, face) { muted } else { t.dark };
    Paint { face, ink: t.dark, muted, ox: 0 }
}

fn fit(fs: &FontSystem, text: &str, max: i32) -> String {
    if fs.measure_text(text) <= max {
        return text.to_string();
    }
    let mut s = String::new();
    for ch in text.chars() {
        let next = format!("{s}{ch}…");
        if fs.measure_text(&next) > max {
            break;
        }
        s.push(ch);
    }
    format!("{s}…")
}

/// "12:30" for today, "11/18" for another day (local time).
fn short_date(ts: i64, now: chrono::DateTime<Local>) -> String {
    let Some(d) = Local.timestamp_opt(ts, 0).single() else { return String::new() };
    if d.date_naive() == now.date_naive() { d.format("%H:%M").to_string() } else { d.format("%-m/%-d").to_string() }
}

fn channel_labels() -> [&'static str; 4] {
    CHANNELS.map(tr)
}

fn tabs<'a>(fs: &FontSystem, tm: &ThemeManager, labels: &'a [&'a str], active: usize) -> widgets::Tabs<'a> {
    widgets::Tabs::folder_fit(fs, labels, active, tm)
}

fn row_rect(i: usize) -> Rect {
    Rect::new(1, LIST_Y + i as i32 * ROW, 318, ROW as u32)
}

fn text_y(r: Rect) -> i32 {
    widgets::text_origin(r.y(), r.height() as i32)
}

/// A list row: the selected one on the selection plate (1 px in from the top, so it never touches
/// the tab strip's rule; the plate keeps clear of the scroll bar column), the rest with a rule.
/// Returns the row's text colour.
fn draw_row(c: &mut Canvas<Window>, tm: &ThemeManager, p: &Paint, i: usize, selected: bool) -> Color {
    let r = p.row(i);
    let w = (crate::grid::scroll_x() + p.ox - 2 - (r.x() + 2)) as u32;
    if selected {
        widgets::draw_row_plate_on(c, tm, Rect::new(r.x() + 2, r.y() + 1, w, r.height() - 1), p.face).fg
    } else {
        ui::fill_rect(c, Rect::new(r.x() + 2, r.bottom() - 1, w, 1), p.muted);
        p.ink
    }
}

/// Where the scroll bar of a list of `rows` rows from row `first_row` goes (1 px clear of the
/// tab strip's rule and the bottom row's rule).
fn bar_rect(first_row: usize, rows: usize) -> Rect {
    let top = LIST_Y + first_row as i32 * ROW;
    Rect::new(crate::grid::scroll_x(), top + 1, crate::grid::SCROLL_W as u32, (rows as i32 * ROW - 2) as u32)
}

/// The shared scroll bar at the right edge of the list, on Infoscope's face; only when the list
/// scrolls.
fn draw_bar(c: &mut Canvas<Window>, tm: &ThemeManager, p: &Paint, scroll: usize, len: usize, rows: usize, first_row: usize, dragging: bool) {
    if len <= rows {
        return;
    }
    let r = bar_rect(first_row, rows);
    let sb = ScrollBar::new(Rect::new(r.x() + p.ox, r.y(), r.width(), r.height()), len, rows, scroll);
    sb.draw_on(c, tm, dragging.then_some(SbPart::Thumb), p.face);
}

fn centered(c: &mut Canvas<Window>, fs: &FontSystem, p: &Paint, y: i32, text: &str, col: Color) {
    fs.draw_text(c, 160 + p.ox - fs.measure_text(text) / 2, y, text, col);
}

impl Infoscope {
    fn draw_tabs(&self, c: &mut Canvas<Window>, ctx: &Ctx) {
        let (fs, tm) = (ctx.fs, ctx.tm);
        let labels = channel_labels();
        let mut tabs = tabs(fs, tm, &labels, self.channel);
        // The page here is Infoscope's own face (light even on Invert themes): the rule must stand out from it.
        tabs.page = Some(paint(ctx).face);
        tabs.draw_strip(c, fs, tm);
        // Status on the strip, right of the tabs, level with the idle tab labels.
        let status = self.status_text();
        if !status.is_empty() {
            let s = fit(fs, &status, RIGHT - tabs.end_x() - 4);
            fs.draw_text(c, RIGHT - fs.measure_text(&s), widgets::folder_strip_text_y(), &s, widgets::secondary_ink(tm));
        }
    }

    fn status_text(&self) -> String {
        if self.confirm.is_some() {
            return tr_s("Remove?");
        }
        if !self.status.is_empty() {
            return tr_s(&self.status);
        }
        match self.channel {
            NEWS if self.view == NewsView::Feeds => tr_s(&format!("{} feeds", self.subs.feeds.len())),
            NEWS if self.view == NewsView::Article && self.reading.as_ref().is_some_and(|r| r.from_feed) => tr_s("From the feed"),
            SEARCH if self.index_rx.is_some() => tr_s("Indexing..."),
            SEARCH if !self.query.is_empty() => tr_s(&format!("{} files found", self.hits.iter().filter(|h| matches!(h, Hit::File(_))).count())),
            _ => String::new(),
        }
    }

    fn draw_items(&self, c: &mut Canvas<Window>, p: &Paint, ctx: &Ctx) {
        let (left, right) = (LEFT + p.ox, list_right() + p.ox);
        let fs = ctx.fs;
        if self.cache.items.is_empty() {
            centered(c, fs, p, text_y(p.row(3)), tr(self.empty_note()), p.muted);
            return;
        }
        for (row, i) in (self.items.scroll..self.cache.items.len()).take(list_rows()).enumerate() {
            let it = &self.cache.items[i];
            let sel = i == self.items.cursor;
            let col = draw_row(c, ctx.tm, p, row, sel);
            let r = p.row(row);
            let date = it.date.map(|d| short_date(d, ctx.now)).unwrap_or_default();
            let dw = fs.measure_text("00/00");
            let src = fit(fs, &it.source, 62);
            let sx = right - dw - 6 - fs.measure_text(&src);
            fs.draw_text(c, right - fs.measure_text(&date), text_y(r), &date, if sel { col } else { p.muted });
            fs.draw_text(c, sx, text_y(r), &src, if sel { col } else { p.muted });
            fs.draw_text(c, left, text_y(r), &fit(fs, &it.title, sx - left - 6), col);
        }
        draw_bar(c, ctx.tm, p, self.items.scroll, self.cache.items.len(), list_rows(), 0, self.sb_drag);
    }

    fn draw_article(&self, c: &mut Canvas<Window>, p: &Paint, ctx: &Ctx) {
        let (left, right) = (LEFT + p.ox, list_right() + p.ox);
        let fs = ctx.fs;
        let Some(lines) = &self.lines else { return };
        for (row, line) in lines.iter().skip(self.art_scroll).take(list_rows()).enumerate() {
            let y = text_y(p.row(row));
            match line {
                Line::Title(t) => fs.draw_text(c, left, y, t, p.ink),
                Line::Meta(m) => {
                    // The source and date on an ink plate, like a chip.
                    let m = fit(fs, m, right - left - 6);
                    let r = p.row(row);
                    ui::fill_rect(c, Rect::new(left - 3, r.y() + 1, (fs.measure_text(&m) + 6) as u32, 13), p.ink);
                    fs.draw_text(c, left, y, &m, p.face);
                }
                Line::Text(t, indent) => fs.draw_text(c, left + indent, y, t, p.ink),
                Line::Note(n) => centered(c, fs, p, y, tr(n), p.muted),
            }
        }
        draw_bar(c, ctx.tm, p, self.art_scroll, lines.len(), list_rows(), 0, self.sb_drag);
    }

    fn draw_feeds(&self, c: &mut Canvas<Window>, p: &Paint, ctx: &Ctx) {
        let (left, right) = (LEFT + p.ox, list_right() + p.ox);
        let fs = ctx.fs;
        let n = self.subs.feeds.len() + 1;
        for (row, i) in (self.feeds.scroll..n).take(list_rows()).enumerate() {
            let sel = i == self.feeds.cursor;
            let col = draw_row(c, ctx.tm, p, row, sel);
            let r = p.row(row);
            match self.subs.feeds.get(i) {
                Some(f) => {
                    let host = f.url.split('/').nth(2).unwrap_or("");
                    let hw = fs.measure_text(host).min(120);
                    let host = fit(fs, host, 120);
                    fs.draw_text(c, right - hw, text_y(r), &host, if sel { col } else { p.muted });
                    let mark = if self.confirm == Some(i) { tr("Remove?") } else { "" };
                    let lw = fs.measure_text(mark);
                    fs.draw_text(c, left, text_y(r), &fit(fs, f.label(), right - hw - left - 10 - lw), col);
                    if lw > 0 {
                        fs.draw_text(c, right - hw - 6 - lw, text_y(r), mark, col);
                    }
                }
                None => {
                    fs.draw_text(c, left, text_y(r), tr("+ Add feed..."), col);
                }
            }
        }
        draw_bar(c, ctx.tm, p, self.feeds.scroll, n, list_rows(), 0, self.sb_drag);
    }

    /// A one-line text field with a frame 1 px inside its row.
    fn draw_field(c: &mut Canvas<Window>, p: &Paint, ctx: &Ctx, row: usize, x: i32, e: &LineEdit) {
        let (x, right) = (x + p.ox, RIGHT + p.ox);
        let r = p.row(row);
        // The shared text field, in the face's own colours, one whole row high.
        let frame = Rect::new(x, r.y(), (right - x + 2) as u32, widgets::FIELD_H as u32);
        widgets::draw_field(c, ctx.fs, ctx.tm, frame, &widgets::Field { text: &e.text, caret: Some(e.caret), fill: Some(p.face), ink: Some(p.ink), ..Default::default() });
    }

    fn draw_add(&self, c: &mut Canvas<Window>, p: &Paint, ctx: &Ctx) {
        let fs = ctx.fs;
        fs.draw_text(c, LEFT + p.ox, text_y(p.row(0)), tr("Feed address (RSS or Atom):"), p.ink);
        Self::draw_field(c, p, ctx, 1, 4, &self.add);
    }

    fn draw_search(&self, c: &mut Canvas<Window>, p: &Paint, ctx: &Ctx) {
        let (left, right) = (LEFT + p.ox, list_right() + p.ox);
        let fs = ctx.fs;
        let label = tr("Find");
        let r = p.row(0);
        let lw = fs.measure_display_text(label);
        ui::fill_rect(c, Rect::new(4 + p.ox, r.y(), (lw + 8) as u32, widgets::FIELD_H as u32), p.ink);
        fs.draw_display_text(c, 8 + p.ox, text_y(r), label, p.face);
        Self::draw_field(c, p, ctx, 0, lw + 14, &self.query);
        if self.query.is_empty() {
            let lines: &[&str] = if self.search_root.is_some() {
                &["Type to find files on this device,", "or to search the web in the Browser."]
            } else {
                &["Type to search the web in the Browser."]
            };
            for (k, l) in lines.iter().enumerate() {
                fs.draw_text(c, left, text_y(p.row(2 + k)), tr(l), p.muted);
            }
            return;
        }
        for (row, i) in (self.found.scroll..self.hits.len()).take(list_rows() - 1).enumerate() {
            let sel = i == self.found.cursor;
            let col = draw_row(c, ctx.tm, p, row + 1, sel);
            let r = p.row(row + 1);
            match &self.hits[i] {
                Hit::File(e) => {
                    let parent = e.path.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let pw = fs.measure_text(&parent).min(96);
                    let parent = fit(fs, &parent, 96);
                    fs.draw_text(c, right - pw, text_y(r), &parent, if sel { col } else { p.muted });
                    let name = if e.dir { format!("{}/", e.name) } else { e.name.clone() };
                    fs.draw_text(c, left, text_y(r), &fit(fs, &name, right - pw - left - 8), col);
                }
                Hit::Web => {
                    let q = fit(fs, self.query.text.trim(), 150);
                    let t = tr_s(&format!("Search the web for \"{q}\""));
                    fs.draw_text(c, left, text_y(r), &fit(fs, &t, right - left), col);
                }
            }
        }
        draw_bar(c, ctx.tm, p, self.found.scroll, self.hits.len(), list_rows() - 1, 1, self.sb_drag);
    }

    fn draw_not_yet(c: &mut Canvas<Window>, p: &Paint, ctx: &Ctx, lines: &[&'static str]) {
        let fs = ctx.fs;
        let head = tr("Not yet");
        let hw = fs.measure_display_text(head);
        let r = p.row(2);
        ui::fill_rect(c, Rect::new(160 + p.ox - hw / 2 - 5, r.y() + 1, (hw + 10) as u32, 14), p.ink);
        fs.draw_display_text(c, 160 + p.ox - hw / 2, text_y(r), head, p.face);
        for (k, l) in lines.iter().enumerate() {
            centered(c, fs, p, text_y(p.row(4 + k)), tr(l), p.ink);
        }
    }
}

impl LauncherApp for Infoscope {
    fn sizing(&self) -> crate::launcher::app::Sizing {
        crate::launcher::app::Sizing::Flexible
    }

    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        let mut v = vec![k("Tab / Shift+Tab", "Next / previous tab"), k("Ctrl+F", "Search")];
        if !self.typing() {
            v.push(k("1 - 4", "News, Search, Nearby, Inbox"));
        }
        v.extend(match (self.channel, self.view) {
            (NEWS, NewsView::Items) => vec![k("Up / Down", "Choose a row"), k("Return", "Read the article"), k("R", "Refresh news")],
            (NEWS, NewsView::Article) => vec![k("Up / Down", "Scroll"), k("Space", "Next page")],
            (NEWS, NewsView::Feeds) => vec![k("Up / Down", "Choose a row"), k("Return", "Add feed..."), k("Backspace", "Remove feed")],
            (NEWS, NewsView::AddFeed) => vec![k("Return", "Add feed...")],
            (SEARCH, _) => vec![k("Letters", "Type to search"), k("Up / Down", "Choose a row"), k("Return", "Open")],
            _ => vec![],
        });
        v
    }

    fn title(&self) -> String {
        crate::launcher::i18n::app_name("infoscope")
    }

    fn wants_tab(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        match self.channel {
            NEWS if self.view == NewsView::Article => {
                let text: Vec<&str> = self.lines.iter().flatten().filter_map(|l| match l { Line::Text(t, _) => Some(t.as_str()), _ => None }).collect();
                let title = self.reading_item().map(|i| i.title.clone()).unwrap_or_default();
                format!("Infoscope: reading \"{title}\", {} lines; shown from line {}: {}", self.art_len(), self.art_scroll + 1, text.join(" "))
            }
            NEWS => {
                let sel = self.selected_item().map(|i| format!("; selected: {} ({})", i.title, i.source)).unwrap_or_default();
                format!("Infoscope: News, {} headlines from {} feeds{sel}", self.cache.items.len(), self.subs.feeds.len())
            }
            SEARCH => format!("Infoscope: Search \"{}\", {} results", self.query.text, self.hits.len()),
            _ => format!("Infoscope: {} (not yet available)", CHANNELS[self.channel]),
        }
    }

    fn menu(&self) -> Vec<MenuItem> {
        let mut m = vec![
            MenuItem::new("news", "News"),
            MenuItem::new("search", "Search"),
            MenuItem::new("nearby", "Nearby"),
            MenuItem::new("inbox", "Inbox"),
            MenuItem::new("refresh", "Refresh news"),
            MenuItem::new("feeds", "Feeds..."),
            MenuItem::new("add", "Add feed..."),
        ];
        if self.channel == NEWS && self.view == NewsView::Feeds && self.feeds.cursor < self.subs.feeds.len() {
            m.push(MenuItem::new("remove", "Remove feed"));
        }
        let reading = self.view == NewsView::Article && self.reading.is_some();
        if self.channel == NEWS && (reading || self.view == NewsView::Items && self.selected_item().is_some()) {
            m.push(MenuItem::new("browser", "Open in Browser"));
        }
        m
    }

    fn menu_action(&mut self, id: &str, _ctx: &Ctx) -> AppAction {
        self.menu_id(id)
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, _ctx: &Ctx) -> AppAction {
        self.dirty = true;
        let ctrl = mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
        let shift = mods.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
        if key == Keycode::F && ctrl {
            self.set_channel(SEARCH);
            return AppAction::None;
        }
        if key == Keycode::Tab {
            self.cycle(if shift { -1 } else { 1 });
            return AppAction::None;
        }
        if self.typing() {
            match key {
                Keycode::Escape => {
                    self.back();
                }
                Keycode::Return | Keycode::KpEnter => return self.activate(),
                Keycode::Up => self.move_sel(-1),
                Keycode::Down => self.move_sel(1),
                _ => {
                    let field = if self.channel == SEARCH { &mut self.query } else { &mut self.add };
                    if field.key(key, mods) && self.channel == SEARCH {
                        self.update_hits();
                    }
                }
            }
            return AppAction::None;
        }
        match arrow_alias(key) {
            Keycode::Escape => {
                self.back();
            }
            Keycode::Return | Keycode::KpEnter => return self.activate(),
            Keycode::PageUp => self.move_sel(-(list_rows() as isize - 1)),
            Keycode::PageDown => self.move_sel(list_rows() as isize - 1),
            Keycode::Space if self.view == NewsView::Article && self.channel == NEWS => self.move_sel(list_rows() as isize - 1),
            Keycode::Home if self.view == NewsView::Article && self.channel == NEWS => self.art_scroll = 0,
            Keycode::End if self.view == NewsView::Article && self.channel == NEWS => self.art_scroll = self.art_max(),
            Keycode::Up => self.move_sel(-1),
            Keycode::Down => self.move_sel(1),
            Keycode::Left => self.cycle(-1),
            Keycode::Right => self.cycle(1),
            Keycode::Num1 => self.set_channel(NEWS),
            Keycode::Num2 => self.set_channel(SEARCH),
            Keycode::Num3 => self.set_channel(NEARBY),
            Keycode::Num4 => self.set_channel(INBOX),
            Keycode::R if self.channel == NEWS => self.start_refresh(None),
            Keycode::Backspace | Keycode::Delete => self.ask_remove(),
            _ => self.dirty = false,
        }
        AppAction::None
    }

    fn text(&mut self, text: &str, _ctx: &Ctx) -> AppAction {
        if !self.typing() {
            return AppAction::None;
        }
        self.dirty = true;
        if self.channel == SEARCH {
            if self.query.len() < 80 {
                self.query.insert(text);
                self.update_hits();
            }
        } else if self.add.len() < 400 {
            self.add.insert(text);
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> AppAction {
        match ev {
            PointerEvent::Wheel { dy, .. } => {
                self.move_sel(if dy > 0 { -1 } else { 1 });
                self.dirty = true;
            }
            PointerEvent::Down { x, y, button: 0, clicks } => {
                self.dirty = true;
                if y < LIST_Y {
                    let labels = channel_labels();
                    if let Some(i) = tabs(ctx.fs, ctx.tm, &labels, self.channel).hit(x, y) {
                        self.set_channel(i);
                    }
                    return AppAction::None;
                }
                // The scroll bar (a press a pixel or two left of it counts): arrows step, the
                // track pages, the thumb drags.
                if let Some(sb) = self.scrollbar().filter(|_| x >= crate::grid::scroll_x() - 2) {
                    if let Some((to, thumb)) = sb.press(x.clamp(sb.rect.x(), sb.rect.right() - 1), y) {
                        if thumb {
                            self.sb_drag = true;
                        } else {
                            self.set_scroll(to);
                        }
                        return AppAction::None;
                    }
                }
                let row = ((y - LIST_Y) / ROW) as usize;
                let (pos, len, first) = match (self.channel, self.view) {
                    (NEWS, NewsView::Items) => (&mut self.items, self.cache.items.len(), 0),
                    (NEWS, NewsView::Feeds) => (&mut self.feeds, self.subs.feeds.len() + 1, 0),
                    (NEWS, NewsView::Article) => return AppAction::None,
                    (SEARCH, _) if row >= 1 => (&mut self.found, self.hits.len(), 1),
                    _ => return AppAction::None,
                };
                if row >= list_rows() || row < first {
                    return AppAction::None;
                }
                let i = pos.scroll + row - first;
                if i < len {
                    let again = pos.cursor == i;
                    pos.cursor = i;
                    self.confirm = None;
                    if clicks >= 2 || (again && self.channel == NEWS && self.view == NewsView::Feeds && i == len - 1) {
                        return self.activate();
                    }
                }
            }
            PointerEvent::Drag { y, .. } if self.sb_drag => {
                if let Some(sb) = self.scrollbar() {
                    self.set_scroll(sb.offset_for_y(y));
                }
                self.dirty = true;
            }
            PointerEvent::Up { .. } if self.sb_drag => {
                self.sb_drag = false;
                self.dirty = true;
            }
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, ctx: &Ctx) -> bool {
        let mut changed = std::mem::take(&mut self.dirty);
        changed |= self.poll(ctx.now.timestamp());
        changed |= self.poll_articles(ctx.now.timestamp());
        // The opening slide starts with the first frame drawn (the host's zoom plays before it).
        if self.shown.is_some() {
            self.sync_slide(Instant::now());
        }
        changed |= self.slide.is_some();
        if let Some(rx) = &self.index_rx {
            if let Ok(idx) = rx.try_recv() {
                self.index = Some(idx);
                self.index_rx = None;
                self.update_hits();
                changed = true;
            }
        }
        changed
    }

    fn draw(&mut self, c: &mut Canvas<Window>, _assets: &mut AssetStore, ctx: &Ctx) {
        let p = paint(ctx);
        ui::fill_rect(c, chrome::app_rect(), p.face);
        self.layout_article(ctx.fs);
        self.draw_tabs(c, ctx);
        let now = Instant::now();
        self.sync_slide(now);
        let to = self.page();
        let Some(s) = self.slide else {
            self.draw_page(c, &p, ctx, to);
            return;
        };
        // The tab strip stays; only the page under it slides: the page left and the new one side
        // by side, shifted together (no fade: one-bit pictures).
        let t = (now.duration_since(s.start).as_secs_f32() * 1000.0 / SLIDE_MS as f32).min(1.0);
        let (ox_old, ox_new) = slide_offsets(s.dir, t, crate::grid::sw());
        c.set_clip_rect(Rect::new(0, LIST_Y, crate::grid::sw() as u32, (crate::grid::frame_bottom() + 1 - LIST_Y) as u32));
        if let Some(from) = s.from {
            self.draw_page(c, &Paint { ox: ox_old, ..p }, ctx, from);
        }
        self.draw_page(c, &Paint { ox: ox_new, ..p }, ctx, to);
        c.set_clip_rect(None);
    }
}

impl Infoscope {
    /// One page under the tab strip, drawn with `p.ox` as its shift.
    fn draw_page(&self, c: &mut Canvas<Window>, p: &Paint, ctx: &Ctx, page: PageKey) {
        if let Some(note) = page.note {
            centered(c, ctx.fs, p, text_y(p.row(3)), tr(note), p.muted);
            return;
        }
        match page.channel {
            NEWS => match page.view {
                NewsView::Items => self.draw_items(c, &p, ctx),
                NewsView::Article => self.draw_article(c, &p, ctx),
                NewsView::Feeds => self.draw_feeds(c, &p, ctx),
                NewsView::AddFeed => self.draw_add(c, &p, ctx),
            },
            SEARCH => self.draw_search(c, &p, ctx),
            NEARBY => Self::draw_not_yet(c, &p, ctx, &["Nearby devices (infrared and Bluetooth)", "come in a later version."]),
            _ => Self::draw_not_yet(c, &p, ctx, &["Messages, cards and new headlines", "will collect here in a later version."]),
        }
    }
}

impl Infoscope {
    /// The app menu's actions (shared by the trait's `menu_action` and the tests).
    fn menu_id(&mut self, id: &str) -> AppAction {
        self.dirty = true;
        match id {
            "news" => self.set_channel(NEWS),
            "search" => self.set_channel(SEARCH),
            "nearby" => self.set_channel(NEARBY),
            "inbox" => self.set_channel(INBOX),
            "refresh" => {
                self.set_channel(NEWS);
                self.start_refresh(None);
            }
            "feeds" => {
                self.set_channel(NEWS);
                self.view = NewsView::Feeds;
            }
            "add" => {
                self.set_channel(NEWS);
                self.view = NewsView::AddFeed;
                self.status.clear();
            }
            "remove" => self.ask_remove(),
            // The Browser app itself (its own title and toolbar), with the article's page.
            "browser" => {
                let link = match self.view {
                    NewsView::Article => self.reading.as_ref().map(|r| r.link.clone()),
                    _ => self.selected_item().map(|i| i.link.clone()),
                };
                if let Some(l) = link {
                    return Self::open_link(&l);
                }
            }
            _ => {}
        }
        AppAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(dir: &str) -> Infoscope {
        let d = std::env::temp_dir().join(format!("infoscope-app-{dir}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Infoscope::new(Store { dir: Some(d) }, None, true)
    }

    fn wait(a: &mut Infoscope) {
        for _ in 0..200 {
            a.poll(1_795_060_000);
            if a.refresh.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("refresh did not finish");
    }

    #[test]
    fn refresh_merges_feeds_newest_first_and_caches_them() {
        let mut a = app("merge");
        a.start_refresh(None);
        wait(&mut a);
        assert_eq!(a.cache.items.len(), 8);
        assert_eq!(a.cache.items[0].title, "港の灯台、嵐のあと点灯を再開");
        assert!(a.cache.items.windows(2).all(|w| w[0].date >= w[1].date));
        // Feed titles are learnt from the feeds and kept.
        assert_eq!(a.subs.feeds[1].title, "Sample World");
        assert_eq!(a.cache.items.iter().find(|i| i.link.ends_with("/ferry")).unwrap().source, "Sample World");
        // A new app over the same folder shows the cached list without fetching.
        let b = Infoscope::new(a.store.clone(), None, true);
        assert_eq!(b.cache.items.len(), 8);
        assert_eq!(b.cache.fetched, Some(1_795_060_000));
        let _ = std::fs::remove_dir_all(a.store.dir.unwrap());
    }

    fn wait_article(a: &mut Infoscope) {
        for _ in 0..200 {
            a.poll_articles(1_795_060_000);
            if a.reading.as_ref().is_some_and(|r| r.paragraphs.is_some()) {
                a.layout_article_with(&|t: &str| Metrics::fixed(6, 12).width(t));
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("article did not load");
    }

    fn item(a: &mut Infoscope, slug: &str) {
        a.items.cursor = a.cache.items.iter().position(|i| i.link.ends_with(slug)).unwrap();
    }

    #[test]
    fn enter_reads_the_article_here_and_the_browser_is_in_the_menu() {
        let mut a = app("enter");
        a.start_refresh(None);
        wait(&mut a);
        item(&mut a, "/lights");
        let link = a.selected_item().unwrap().link.clone();
        assert_eq!(a.activate(), AppAction::None, "Enter never leaves for the Browser");
        assert_eq!(a.view, NewsView::Article);
        assert_eq!(a.page().depth(), 1);
        a.layout_article_with(&|t: &str| Metrics::fixed(6, 12).width(t));
        assert!(a.lines.as_ref().unwrap().contains(&Line::Note("Loading...")));
        a.lines = None;
        wait_article(&mut a);
        let lines = a.lines.clone().unwrap();
        assert!(matches!(&lines[0], Line::Title(t) if t.starts_with("Harbour lights")));
        assert!(matches!(&lines[1], Line::Meta(m) if m.starts_with("Sample World  2026-11-19")));
        // The page's own <h1> repeats the headline and is left out; paragraphs start indented.
        assert!(matches!(&lines[2], Line::Text(t, 12) if t.starts_with("Crews worked")), "{:?}", lines[2]);
        assert!(lines.len() > list_rows() + 10, "a long article: {}", lines.len());
        // Scrolling: by a row, by a page, and never past the end.
        a.move_sel(1);
        assert_eq!(a.art_scroll, 1);
        a.move_sel(list_rows() as isize - 1);
        assert_eq!(a.art_scroll, list_rows());
        a.move_sel(1000);
        assert_eq!(a.art_scroll, lines.len() - list_rows());
        a.move_sel(-1000);
        assert_eq!(a.art_scroll, 0);
        // The Browser: only the menu's "Open in Browser", as the Browser app.
        assert!(a.menu().iter().any(|m| m.id == "browser"));
        assert_eq!(a.menu_id("browser"), AppAction::Open { app: Some("Browser".into()), path: PathBuf::from(link.clone()) });
        // Saved: it opens again at once (offline), also from a new app over the same folder.
        assert!(a.back());
        assert_eq!(a.view, NewsView::Items);
        assert!(!a.back());
        let mut b = Infoscope::new(a.store.clone(), None, true);
        b.cache = a.cache.clone();
        item(&mut b, "/lights");
        b.activate();
        assert!(b.reading.as_ref().unwrap().paragraphs.as_ref().is_some_and(|p| p.len() == 7), "{:?}", b.reading);
        let _ = std::fs::remove_dir_all(a.store.dir.unwrap());
    }

    #[test]
    fn without_an_article_page_the_feed_text_is_shown() {
        let mut a = app("fallback");
        a.start_refresh(None);
        wait(&mut a);
        item(&mut a, "/ferry");
        a.activate();
        wait_article(&mut a);
        let r = a.reading.clone().unwrap();
        assert!(r.from_feed && !r.failed);
        assert_eq!(a.status_text(), "From the feed");
        assert_eq!(a.lines.as_ref().unwrap().iter().filter(|l| matches!(l, Line::Text(_, 12))).count(), 2, "two paragraphs");
        assert!(a.articles.list.is_empty(), "the feed's text is not saved as an article");
        // No page and no feed text: says so.
        a.back();
        item(&mut a, "/market");
        a.activate();
        wait_article(&mut a);
        assert!(a.reading.as_ref().unwrap().failed);
        assert!(a.lines.as_ref().unwrap().contains(&Line::Note("Could not read the article.")));
        let _ = std::fs::remove_dir_all(a.store.dir.unwrap());
    }

    #[test]
    fn removing_a_feed_drops_its_headlines_after_asking() {
        let mut a = app("remove");
        a.start_refresh(None);
        wait(&mut a);
        a.view = NewsView::Feeds;
        a.feeds.cursor = 1;
        a.ask_remove();
        assert_eq!(a.confirm, Some(1));
        a.activate();
        assert_eq!(a.subs.feeds.len(), 2);
        assert!(a.cache.items.iter().all(|i| i.source != "Sample World"));
        assert_eq!(a.store.load_subs().feeds.len(), 2);
        let _ = std::fs::remove_dir_all(a.store.dir.unwrap());
    }

    #[test]
    fn adding_a_feed_fetches_only_it_and_keeps_the_rest() {
        let mut a = app("add");
        a.start_refresh(None);
        wait(&mut a);
        a.view = NewsView::AddFeed;
        a.add = LineEdit::new("https://example.net/b");
        a.activate();
        assert_eq!(a.view, NewsView::Feeds);
        assert_eq!(a.subs.feeds.len(), 4);
        wait(&mut a);
        // The added feed answers with the same sample items (same links): no duplicates.
        assert_eq!(a.cache.items.len(), 8);
        a.view = NewsView::AddFeed;
        a.add = LineEdit::new("not an address");
        a.activate();
        assert_eq!(a.status, "Not a web address");
        assert_eq!(a.view, NewsView::AddFeed);
        let _ = std::fs::remove_dir_all(a.store.dir.unwrap());
    }

    #[test]
    fn search_lists_files_then_the_web_row() {
        let mut a = app("search");
        a.index = Some(vec![]);
        a.channel = SEARCH;
        a.query = LineEdit::new("lighthouse");
        a.update_hits();
        assert_eq!(a.hits, [Hit::Web]);
        assert_eq!(a.open_hit(), AppAction::Open { app: Some("Browser".into()), path: PathBuf::from("https://lite.duckduckgo.com/lite/?q=lighthouse") });
        assert!(a.back());
        assert!(a.hits.is_empty());
        assert_eq!(a.channel, SEARCH, "the first Esc only clears the query");
        assert!(a.back(), "Esc on an empty search field leaves it");
        assert_eq!(a.channel, NEWS);
    }

    #[test]
    fn channels_cycle_both_ways() {
        let mut a = app("cycle");
        a.cycle(-1);
        assert_eq!(a.channel, INBOX);
        a.cycle(1);
        a.cycle(1);
        assert_eq!(a.channel, SEARCH);
    }

    #[test]
    fn pages_slide_in_their_direction_and_never_hold_keys() {
        let mut a = app("slide");
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // Opening: the first page comes in from the right, from nothing.
        a.sync_slide(t0);
        let s = a.slide.unwrap();
        assert_eq!((s.from, s.dir), (None, 1));
        assert_eq!(a.page().note, Some("No news yet. Alt: Refresh"));
        a.sync_slide(ms(SLIDE_MS));
        assert!(a.slide.is_none(), "the slide ends after SLIDE_MS");
        // The first headlines arriving slide in from the right.
        a.start_refresh(None);
        a.sync_slide(ms(300));
        assert!(a.slide.is_none(), "one empty-list line to another is instant");
        wait(&mut a);
        a.sync_slide(ms(400));
        assert_eq!(a.slide.map(|s| (s.from.unwrap().note, s.dir)), Some((Some("Loading..."), 1)));
        // Deeper pages from the right, back from the left; keys change the page at once.
        a.sync_slide(ms(700));
        a.menu_id("feeds");
        assert_eq!(a.view, NewsView::Feeds);
        a.sync_slide(ms(710));
        assert_eq!(a.slide.unwrap().dir, 1);
        a.back();
        a.sync_slide(ms(720));
        let s = a.slide.unwrap();
        assert_eq!((s.from.unwrap().view, s.dir, s.start), (NewsView::Feeds, -1, ms(720)), "a newer change restarts the slide");
        a.cache.items = news::mock_feed("https://x/a").1.unwrap().items;
        a.activate();
        assert_eq!(a.view, NewsView::Article);
        a.sync_slide(ms(730));
        assert_eq!(a.slide.unwrap().dir, 1);
        // Channels by their order in the strip (Tab from Inbox to News goes left).
        a.set_channel(NEARBY);
        a.sync_slide(ms(1000));
        assert_eq!(a.slide.unwrap().dir, 1);
        a.cycle(1);
        a.sync_slide(ms(1010));
        a.cycle(1);
        a.sync_slide(ms(1020));
        assert_eq!((a.channel, a.slide.unwrap().dir), (NEWS, -1));
        // Nothing changed: no slide.
        a.sync_slide(ms(1300));
        a.sync_slide(ms(1301));
        assert!(a.slide.is_none());
        let _ = std::fs::remove_dir_all(a.store.dir.unwrap());
    }

    #[test]
    fn slide_offsets_keep_the_two_pages_side_by_side() {
        assert_eq!(slide_offsets(1, 0.0, 320), (0, 320));
        assert_eq!(slide_offsets(1, 1.0, 320), (-320, 0));
        assert_eq!(slide_offsets(-1, 0.0, 320), (0, -320));
        assert_eq!(slide_offsets(-1, 1.0, 320), (320, 0));
        for t in [0.1, 0.33, 0.5, 0.9] {
            let (o, n) = slide_offsets(1, t, 320);
            assert!((n - o - 320).abs() <= 1, "pages touch at t={t}: {o} {n}");
        }
    }

    #[test]
    fn the_scroll_bar_shows_only_when_the_list_scrolls_and_pulls_the_selection() {
        let mut a = app("scrollbar");
        a.start_refresh(None);
        wait(&mut a);
        assert!(a.scrollbar().is_none(), "eight headlines fit");
        let more = a.cache.items.clone();
        a.cache.items.extend(more.iter().cloned());
        a.cache.items.extend(more);
        let sb = a.scrollbar().expect("24 headlines scroll");
        assert_eq!(sb.rect, Rect::new(crate::grid::scroll_x(), LIST_Y + 1, 4, (list_rows() as i32 * ROW - 2) as u32));
        // Below the thumb: a page down; the selection comes along.
        let (to, thumb) = sb.press(sb.rect.x(), sb.rect.bottom() - 8).unwrap();
        assert!(!thumb);
        a.set_scroll(to);
        assert_eq!(a.items.scroll, list_rows());
        assert_eq!(a.items.cursor, list_rows());
        // Dragging the thumb to the end.
        let sb = a.scrollbar().unwrap();
        assert_eq!(sb.press(sb.rect.x(), sb.thumb().y() + 1), Some((list_rows(), true)));
        a.set_scroll(sb.offset_for_y(1000));
        assert_eq!(a.items.scroll, 24 - list_rows());
        assert!(a.items.cursor >= a.items.scroll);
    }

    #[test]
    fn the_page_uses_the_screen_down_to_its_bottom() {
        assert_eq!(list_rows(), 8);
        let last = row_rect(list_rows() - 1);
        assert_eq!(last.bottom(), 164, "rows end at y 163");
        assert!(crate::grid::frame_bottom() + 1 - last.bottom() < ROW, "no whole empty row is left");
        assert_eq!(crate::grid::frame_bottom(), 168, "y 169 stays clear");
    }

    #[test]
    fn help_rows_are_filled_in() {
        let mut a = app("help");
        for view in [NewsView::Items, NewsView::Article, NewsView::Feeds, NewsView::AddFeed] {
            a.view = view;
            crate::launcher::help::check_app_rows("Infoscope News", &a.help());
        }
        for ch in [SEARCH, NEARBY, INBOX] {
            a.set_channel(ch);
            crate::launcher::help::check_app_rows("Infoscope", &a.help());
        }
    }
}
