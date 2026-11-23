//! Harbour Radio: a small physical-feeling, one-bit radio/cassette deck.
//!
//! Tab switches Radio / Podcasts. Radio: Up/Down selects a band, Left/Right
//! tunes stations, Enter plays, L opens the station list. Ctrl+F searches
//! Apple's public catalogue, Enter opens a show's episode list; Enter there
//! plays and shows the cassette deck, L toggles list / deck. The station list
//! sits under a genre grid (a lamp per genre, the chosen one lit). B shows
//! saved stations/shows, P pins the current selection. Space pauses; Alt
//! opens the app menu. Audio continues when the app is closed.
//!
//! Network operations always run off the SDL thread.

mod audio;
mod net;

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use sdl2::keyboard::{Keycode, Mod};
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome;
use crate::font::FontSystem;
use crate::grid::{APP_FRAME_BOTTOM, CONTENT_Y, ROW_H, SCROLL_X};
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::i18n::{tr, tr_s};
use crate::launcher::lineedit::LineEdit;
use crate::launcher::widgets::{self, MenuItem, PointerEvent};
use crate::platform::keys::arrow_alias;
use crate::theme::ThemeManager;
use crate::ui;

use net::{Episode, Library, Podcast, Station};

const BANDS: [(&str, &str); 7] = [
    ("AMBIENT", "ambient"),
    ("ELECTRONIC", "electronic"),
    ("JAZZ", "jazz"),
    ("CLASSICAL", "classical"),
    ("WORLD", "world"),
    ("TALK", "talk"),
    ("FAVORITES", ""),
];
const FAV_BAND: usize = BANDS.len() - 1;
/// The screen is used down to y 168 (`grid::APP_FRAME_BOTTOM`): `BOTTOM` is one past it.
const BOTTOM: i32 = APP_FRAME_BOTTOM + 1;
/// Height of the dial / deck panel; the readout sits below it (y 136..169, its plate ending on y 168).
const PANEL_H: i32 = 120;
/// The genre grid above the station list: three rows of 16 px cells, three columns.
const GENRE_ROW: i32 = 16;
/// The podcast lists' header band (the list's name and lamps) above the rows.
const HEADER_H: i32 = 24;
const GENRE_COL: i32 = 104;
/// Width of the cassette on the podcast deck.
const CASSETTE_W: i32 = 184;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Radio, Podcasts }

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Dial,
    Stations,
    Shows,
    /// A show's episodes as a list (opens first; Enter plays and goes to the deck).
    EpisodeList,
    /// The cassette deck for the chosen episode (L toggles back to the list).
    Episodes,
}

/// One list row: the name, a detail on the right, saved / subscribed, on air.
struct Row {
    name: String,
    detail: String,
    saved: bool,
    on_air: bool,
}

enum Job {
    Stations { band: String, query: Option<String> },
    Shows(String),
    Episodes(String),
}
enum Fetched {
    Stations(Vec<Station>),
    Shows(Vec<Podcast>),
    Episodes(Vec<Episode>),
}
type Reply = (u64, Result<Fetched, String>);

fn config_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".config/harbour/radio.json"))
}

fn read_library() -> Library {
    config_path().and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn save_library(lib: &Library) -> Result<(), String> {
    let path = config_path().ok_or("HOME is not set")?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(lib).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}

pub struct Radio {
    mode: Mode,
    screen: Screen,
    band: usize,
    stations: Vec<Station>,
    shows: Vec<Podcast>,
    episodes: Vec<Episode>,
    chosen_show: Option<Podcast>,
    library: Library,
    cursor: usize,
    scroll: usize,
    station_search: Option<String>,
    podcast_query: String,
    saved_shows: bool,
    editing: bool,
    search: LineEdit,
    loading: bool,
    error: Option<String>,
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
    serial: u64,
    last_audio: Option<(String, bool)>,
    /// When the deck's reels / the Loading lamp last moved.
    last_anim: std::time::Instant,
    mock: bool,
}

pub fn make() -> Box<dyn LauncherApp> {
    let mut app = Radio::new();
    app.refresh_stations();
    Box::new(app)
}

impl Radio {
    fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            mode: Mode::Radio, screen: Screen::Dial, band: 0,
            stations: Vec::new(), shows: Vec::new(), episodes: Vec::new(),
            chosen_show: None, library: read_library(), cursor: 0, scroll: 0,
            station_search: None, podcast_query: "music".into(), saved_shows: false,
            editing: false, search: LineEdit::default(), loading: false,
            error: None, tx, rx, serial: 0, last_audio: None, last_anim: std::time::Instant::now(),
            mock: std::env::var("HARBOUR_MOCK").is_ok_and(|x| x == "1" || x == "true"),
        }
    }

    fn start(&mut self, job: Job) {
        self.serial += 1;
        let serial = self.serial;
        self.loading = true;
        self.error = None;
        if self.mock {
            let data = match job {
                Job::Stations { .. } => Fetched::Stations(vec![
                    Station { id: "sample-a".into(), name: "SAMPLE RADIO 01".into(),
                        url: "https://example.org/live".into(), tags: "ambient".into(), codec: "MP3".into(), bitrate: 128, geo: Some((52.5, 13.4)) },
                    Station { id: "sample-b".into(), name: "SAMPLE RADIO 02".into(),
                        url: "https://example.org/stream".into(), tags: "jazz".into(), codec: "AAC".into(), bitrate: 96, geo: Some((35.7, 139.7)) },
                ]),
                Job::Shows(_) => Fetched::Shows(vec![
                    Podcast { title: "DEMO PODCAST".into(), author: "Preview only".into(),
                        feed: "https://example.org/rss".into() },
                ]),
                Job::Episodes(_) => Fetched::Episodes(vec![
                    Episode { title: "EPISODE 01".into(), audio: "https://example.org/01.mp3".into() },
                ]),
            };
            let _ = self.tx.send((serial, Ok(data)));
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = match job {
                Job::Stations { band, query } => net::stations(Some(&band), query.as_deref()).map(Fetched::Stations),
                Job::Shows(term) => net::podcasts(&term).map(Fetched::Shows),
                Job::Episodes(feed) => net::episodes(&feed).map(Fetched::Episodes),
            };
            let _ = tx.send((serial, result));
        });
    }

    fn reset_selection(&mut self) {
        self.cursor = 0;
        self.scroll = 0;
    }

    fn refresh_stations(&mut self) {
        self.reset_selection();
        if self.band == FAV_BAND {
            self.serial += 1; // invalidate an earlier network response
            self.stations = self.library.stations.clone();
            self.loading = false;
            self.error = None;
        } else {
            self.stations.clear();
            self.start(Job::Stations { band: BANDS[self.band].1.into(), query: self.station_search.clone() });
        }
    }

    fn refresh_shows(&mut self) {
        self.reset_selection();
        if self.saved_shows {
            self.serial += 1;
            self.shows = self.library.podcasts.clone();
            self.loading = false;
            self.error = None;
        } else {
            self.shows.clear();
            self.start(Job::Shows(self.podcast_query.clone()));
        }
    }

    fn band_step(&mut self, dir: isize) {
        self.band = (self.band as isize + dir).rem_euclid(BANDS.len() as isize) as usize;
        self.station_search = None;
        self.refresh_stations();
    }

    fn count(&self) -> usize {
        match self.screen {
            Screen::Dial | Screen::Stations => self.stations.len(),
            Screen::Shows => self.shows.len(),
            Screen::Episodes | Screen::EpisodeList => self.episodes.len(),
        }
    }

    /// Rows a list screen shows (the station list sits under the genre grid).
    fn visible(&self) -> usize {
        match self.screen {
            Screen::Dial | Screen::Stations => 6,
            _ => 8,
        }
    }

    /// Where a list screen's first row starts.
    fn list_top(&self) -> i32 {
        match self.screen {
            // Under the genre grid (y 18..67) with 4 px free: 6 rows, the last rule on y 167.
            Screen::Dial | Screen::Stations => genre_rect().bottom() + 4,
            _ => CONTENT_Y + HEADER_H,
        }
    }

    fn set_band(&mut self, band: usize) {
        self.band = band.min(FAV_BAND);
        self.station_search = None;
        self.refresh_stations();
    }

    fn select(&mut self, dir: isize) {
        let n = self.count();
        if n == 0 { return; }
        self.cursor = (self.cursor as isize + dir).rem_euclid(n as isize) as usize;
        self.follow();
    }

    fn follow(&mut self) {
        if self.cursor < self.scroll { self.scroll = self.cursor; }
        let vis = self.visible();
        if self.cursor >= self.scroll + vis {
            self.scroll = self.cursor + 1 - vis;
        }
    }

    fn switch_mode(&mut self) {
        self.editing = false;
        self.mode = if self.mode == Mode::Radio { Mode::Podcasts } else { Mode::Radio };
        self.screen = if self.mode == Mode::Radio { Screen::Dial } else { Screen::Shows };
        self.reset_selection();
        if self.mode == Mode::Radio {
            if self.stations.is_empty() { self.refresh_stations(); }
        } else if self.shows.is_empty() { self.refresh_shows(); }
    }

    fn start_search(&mut self) {
        self.search = LineEdit::new(if self.mode == Mode::Radio {
            self.station_search.as_deref().unwrap_or("")
        } else { &self.podcast_query });
        self.editing = true;
    }

    fn submit_search(&mut self) {
        self.editing = false;
        let term = self.search.text.trim().to_string();
        if term.is_empty() { return; }
        match self.mode {
            Mode::Radio => {
                self.station_search = Some(term);
                if self.band == FAV_BAND { self.band = 0; }
                self.screen = Screen::Stations;
                self.refresh_stations();
            }
            Mode::Podcasts => {
                self.podcast_query = term;
                self.saved_shows = false;
                self.screen = Screen::Shows;
                self.refresh_shows();
            }
        }
    }

    fn open_show(&mut self) {
        let Some(show) = self.shows.get(self.cursor).cloned() else { return };
        self.chosen_show = Some(show.clone());
        self.screen = Screen::EpisodeList;
        self.episodes.clear();
        self.reset_selection();
        self.start(Job::Episodes(show.feed));
    }

    fn play_selected(&mut self) -> AppAction {
        let target = match self.screen {
            Screen::Dial | Screen::Stations => self.stations.get(self.cursor).map(|s| (s.url.clone(), s.name.clone(), false)),
            Screen::Episodes | Screen::EpisodeList => self.episodes.get(self.cursor).map(|e| (e.audio.clone(), e.title.clone(), true)),
            Screen::Shows => { self.open_show(); return AppAction::None; }
        };
        let Some((url, title, podcast)) = target else { return AppAction::None };
        // Playing goes to the dial (radio) or the deck (podcasts).
        let playing_screen = if podcast { Screen::Episodes } else { Screen::Dial };
        if self.mock {
            self.screen = playing_screen;
            return AppAction::Toast(tr("Preview mode: sound disabled").into());
        }
        match audio::play(&url, &title, podcast) {
            Ok(()) => {
                self.screen = playing_screen;
                self.error = None;
                AppAction::Toast(tr_s(&format!("Playing: {}", title)))
            }
            Err(err) => {
                self.error = Some(err.clone());
                AppAction::Toast(tr_s(&err))
            }
        }
    }

    fn toggle_saved(&mut self) -> AppAction {
        let label = match self.screen {
            Screen::Dial | Screen::Stations => {
                let Some(station) = self.stations.get(self.cursor).cloned() else { return AppAction::None };
                if let Some(i) = self.library.stations.iter().position(|s| s.id == station.id) {
                    self.library.stations.remove(i);
                    "Station removed"
                } else {
                    self.library.stations.push(station);
                    "Station saved"
                }
            }
            Screen::Shows => {
                let Some(show) = self.shows.get(self.cursor).cloned() else { return AppAction::None };
                if let Some(i) = self.library.podcasts.iter().position(|s| s.feed == show.feed) {
                    self.library.podcasts.remove(i);
                    "Podcast removed"
                } else {
                    self.library.podcasts.push(show);
                    "Podcast subscribed"
                }
            }
            Screen::Episodes | Screen::EpisodeList => {
                let Some(show) = self.chosen_show.clone() else { return AppAction::None };
                if let Some(i) = self.library.podcasts.iter().position(|s| s.feed == show.feed) {
                    self.library.podcasts.remove(i);
                    "Podcast removed"
                } else {
                    self.library.podcasts.push(show);
                    "Podcast subscribed"
                }
            }
        };
        let result = save_library(&self.library);
        if self.mode == Mode::Radio && self.band == FAV_BAND {
            self.refresh_stations();
        }
        if self.mode == Mode::Podcasts && self.saved_shows && self.screen == Screen::Shows {
            self.refresh_shows();
        }
        AppAction::Toast(result.map(|_| tr(label).to_string()).unwrap_or_else(|e| tr_s(&format!("Save failed: {e}"))))
    }

    fn show_saved(&mut self) {
        self.editing = false;
        match self.mode {
            Mode::Radio => {
                self.band = FAV_BAND;
                self.station_search = None;
                self.screen = Screen::Stations;
                self.refresh_stations();
            }
            Mode::Podcasts => {
                self.saved_shows = !self.saved_shows;
                self.screen = Screen::Shows;
                self.refresh_shows();
            }
        }
    }

    fn handle_reply(&mut self, reply: Reply) -> bool {
        if reply.0 != self.serial { return false; }
        self.loading = false;
        match reply.1 {
            Ok(Fetched::Stations(v)) => { self.stations = v; self.reset_selection(); }
            Ok(Fetched::Shows(v)) => { self.shows = v; self.reset_selection(); }
            Ok(Fetched::Episodes(v)) => { self.episodes = v; self.reset_selection(); }
            Err(err) => { self.error = Some(err); }
        }
        true
    }

    fn list_name(&self) -> String {
        match self.screen {
            Screen::Stations => {
                self.station_search.as_ref().map(|s| format!("{} {s}", tr("SEARCH:")))
                    .unwrap_or_else(|| tr(BANDS[self.band].0).into())
            }
            Screen::Shows => if self.saved_shows { tr("SUBSCRIPTIONS").into() } else { format!("{} {}", tr("PODCAST:"), self.podcast_query) },
            Screen::Episodes | Screen::EpisodeList => self.chosen_show.as_ref().map(|s| s.title.clone()).unwrap_or_else(|| tr("EPISODES").into()),
            Screen::Dial => tr("RADIO").into(),
        }
    }

    fn rows(&self) -> Vec<Row> {
        let now = audio::snapshot();
        let on_air = |url: &str, podcast: bool| now.as_ref().is_some_and(|p| p.podcast == podcast && p.url == url);
        match self.screen {
            Screen::Dial | Screen::Stations => self.stations.iter().map(|s| Row {
                name: s.name.clone(),
                detail: if s.codec.is_empty() { String::new() } else { format!("{} {}k", s.codec, s.bitrate) },
                saved: self.library.stations.iter().any(|f| f.id == s.id),
                on_air: on_air(&s.url, false),
            }).collect(),
            Screen::Shows => self.shows.iter().map(|s| Row {
                name: s.title.clone(),
                detail: s.author.clone(),
                saved: self.library.podcasts.iter().any(|p| p.feed == s.feed),
                on_air: false,
            }).collect(),
            Screen::Episodes | Screen::EpisodeList => self.episodes.iter().enumerate().map(|(i, e)| Row {
                name: e.title.clone(),
                detail: format!("{:02}", i + 1),
                saved: false,
                on_air: on_air(&e.audio, true),
            }).collect(),
        }
    }
}

fn fit(fs: &FontSystem, text: &str, max: i32) -> String {
    if fs.measure_text(text) <= max { return text.to_string(); }
    let mut s = String::new();
    for ch in text.chars() {
        let next = format!("{s}{ch}...");
        if fs.measure_text(&next) > max { break; }
        s.push(ch);
    }
    format!("{s}...")
}

thread_local! {
    /// Where the tuning needle is drawn (it eases toward the tuned station).
    static NEEDLE: std::cell::Cell<f32> = const { std::cell::Cell::new(-1.0) };
}

/// A small filled lamp: a ring, with the centre filled when lit.
fn lamp(c: &mut Canvas<Window>, cx: i32, cy: i32, lit: bool, on: sdl2::pixels::Color, off: sdl2::pixels::Color) {
    ui::fill_rect(c, Rect::new(cx - 5, cy - 5, 11, 11), off);
    ui::draw_circle(c, (cx, cy), 5, on);
    if lit {
        for r in 0..=3 {
            ui::draw_circle(c, (cx, cy), r, on);
        }
    }
}

fn draw_dial(app: &Radio, c: &mut Canvas<Window>, assets: &mut AssetStore, ctx: &Ctx) {
    let (tm, fs) = (ctx.tm, ctx.fs);
    let land = widgets::tones(tm).dark;
    let (mw, my) = (312i32, CONTENT_Y);
    let mx = 4;
    let station = app.stations.get(app.cursor);
    let blink = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_millis() < 500).unwrap_or(true);
    let snap = audio::snapshot().filter(|p| !p.podcast);

    // The panel: the band on an ink plate, On air (playing) and Loading (blinks while the station
    // list loads) lamps, the keys at the bottom.
    let lamps = [(tr("On air"), snap.as_ref().is_some_and(|p| !p.paused)), (tr("Loading"), app.loading && blink)];
    draw_panel(c, ctx, tr(BANDS[app.band].0), &lamps, "");

    // Tuning scale: ticks along a baseline, a dot per station, a needle on the tuned one.
    let n = app.stations.len();
    let (x0, width) = (mx + 10, mw - 20);
    let base = my + 54;
    let pos = |i: usize| -> f32 { x0 as f32 + if n > 1 { i as f32 * width as f32 / (n - 1) as f32 } else { width as f32 / 2.0 } };
    ui::fill_rect(c, Rect::new(x0, base, width as u32 + 1, 1), land);
    for k in (0..=width).step_by(4) {
        let major = k % 20 == 0;
        ui::fill_rect(c, Rect::new(x0 + k, base + 1, 1, if major { 8 } else { 4 }), land);
    }
    // Numbers: under each station when there are few, else every 40 px (never the same number twice).
    if n > 0 {
        let marks: Vec<(i32, usize)> = if n <= 10 {
            (0..n).map(|i| (pos(i).round() as i32, i + 1)).collect()
        } else {
            let mut v: Vec<(i32, usize)> = Vec::new();
            for k in (0..=width).step_by(40) {
                let idx = ((k as f32 / width as f32) * (n - 1) as f32).round() as usize + 1;
                if v.last().map(|m| m.1) != Some(idx) { v.push((x0 + k, idx)); }
            }
            v
        };
        for (x, idx) in marks {
            let label = idx.to_string();
            let lw = fs.measure_text(&label);
            fs.draw_text(c, x - lw / 2, base + 10, &label, widgets::secondary_ink(tm));
        }
    }
    if n > 0 && n <= 78 {
        for i in 0..n {
            if i != app.cursor {
                let x = pos(i).round() as i32;
                ui::fill_rect(c, Rect::new(x - 1, base - 6, 3, 3), land);
            }
        }
    }
    // The needle eases toward the tuned station (it starts at the target the first time).
    let target = pos(app.cursor);
    let cur = NEEDLE.with(|nd| {
        let v = nd.get();
        let v = if v < 0.0 { target } else { v + (target - v) * 0.35 };
        nd.set(if (v - target).abs() < 0.5 { target } else { v });
        nd.get()
    });
    let nx = cur.round() as i32;
    ui::fill_rect(c, Rect::new(nx, my + 29, 2, (base - my - 29 + 12) as u32), land);
    ui::fill_rect(c, Rect::new(nx - 3, my + 27, 8, 3), land);
    ui::fill_rect(c, Rect::new(nx - 2, my + 24, 6, 3), land);

    // The station number in 7-segment digits, then name and tags on plates.
    let name = station.map(|s| s.name.clone()).unwrap_or_else(|| if app.loading { tr("Scanning...").into() } else { tr("No station").into() });
    let detail = station.map(|s| {
        let tag = s.tags.split(',').next().unwrap_or("").trim();
        let mut d = if tag.is_empty() { tr(BANDS[app.band].0).to_string() } else { tag.to_string() };
        if !s.codec.is_empty() { d.push_str(&format!("  {}", s.codec)); }
        if s.bitrate > 0 { d.push_str(&format!(" {}k", s.bitrate)); }
        d
    }).unwrap_or_else(|| app.list_name());
    draw_readout(c, assets, ctx, app.cursor + 1, app.stations.len(), &name, &detail);
}

fn fit_display(fs: &FontSystem, text: &str, max: i32) -> String {
    if fs.measure_display_text(text) <= max { return text.to_string(); }
    let mut s = String::new();
    for ch in text.chars() {
        let mut t = s.clone();
        t.push(ch);
        if fs.measure_display_text(&format!("{t}...")) > max { break; }
        s = t;
    }
    format!("{s}...")
}

/// The panel face of the dial and the deck: the theme's "white" (`icon_white`); Invert themes lift
/// "icon_white" to a dark tone, and there the plain light tone is used.
fn panel_face(tm: &ThemeManager) -> Color {
    let t = widgets::tones(tm);
    let lum = |c: Color| c.r as u32 * 3 + c.g as u32 * 6 + c.b as u32;
    let white = tm.get_color("icon_white");
    if lum(white) * 2 >= lum(t.light) + lum(t.dark) { white } else { t.light }
}

/// Lamps with labels, right-aligned so the last one ends at `right` (text origin `y`).
fn draw_lamps(c: &mut Canvas<Window>, fs: &FontSystem, right: i32, y: i32, lamps: &[(&str, bool)], ink: Color, under: Color) -> i32 {
    let mut rx = right;
    for (label, lit) in lamps.iter().rev() {
        let lw = fs.measure_text(label);
        fs.draw_text(c, rx - lw, y, label, ink);
        lamp(c, rx - lw - 9, y + 7, *lit, ink, under);
        rx -= lw + 9 + 6 + 8;
    }
    rx + 8
}

/// A small heart: a saved station, a subscribed show (7 x 6 px, top-left at x, y).
fn heart(c: &mut Canvas<Window>, x: i32, y: i32, ink: Color) {
    const ROWS: [&str; 6] = [".XX.XX.", "XXXXXXX", "XXXXXXX", ".XXXXX.", "..XXX..", "...X..."];
    for (dy, row) in ROWS.iter().enumerate() {
        for (dx, ch) in row.chars().enumerate() {
            if ch == 'X' { ui::fill_rect(c, Rect::new(x + dx as i32, y + dy as i32, 1, 1), ink); }
        }
    }
}

/// The readout under the dial and the deck: a number in 7-segment digits (of `total`) on an
/// outlined plate, the name on an ink plate and the detail on an outlined plate beside it.
fn draw_readout(c: &mut Canvas<Window>, assets: &mut AssetStore, ctx: &Ctx, n: usize, total: usize, name: &str, detail: &str) {
    use crate::launcher::sevenseg as seg;
    let (tm, fs) = (ctx.tm, ctx.fs);
    let sea = tm.get_color("icon_white"); // the theme's own "white" (a dark tone on Invert, which keeps the text readable)
    let ink = tm.get_color("foreground");
    let muted = tm.get_color("muted");
    let top = CONTENT_Y + PANEL_H + 2;
    ui::fill_rect(c, Rect::new(0, CONTENT_Y + PANEL_H, 320, 1), ink);
    let plate = Rect::new(4, top, 98, (BOTTOM - top) as u32);
    ui::draw_rect(c, plate, tm.get_color("line"));
    let y0 = top + (plate.height() as i32 - 20) / 2;
    let mut x = plate.x() + 5;
    for d in [(n / 10) % 10, n % 10] {
        seg::draw_atlas_digit(c, assets, seg::BIG_KEY, 14, 20, d, x, y0, ink);
        x += 17;
    }
    x += 3;
    for d in [(total / 10) % 10, total % 10] {
        seg::draw_atlas_digit(c, assets, seg::SMALL_KEY, 9, 15, d, x, y0 + 5, muted);
        x += 12;
    }
    let tx = plate.right() + 8;
    let top = top + (plate.height() as i32 - 29) / 2;
    let name = fit_display(fs, name, 300 - tx);
    let nw = fs.measure_display_text(&name);
    ui::fill_rect(c, Rect::new(tx - 3, top, (nw + 6) as u32, 14), ink);
    fs.draw_display_text(c, tx, top, &name, sea);
    let detail = fit(fs, detail, 300 - tx);
    let lw = fs.measure_text(&detail);
    // Outlined plates are 15 px: a free pixel row above and below the text, Japanese included.
    let dr = Rect::new(tx - 3, top + 15, (lw + 6) as u32, 15);
    ui::fill_rect(c, dr, sea);
    ui::draw_rect(c, dr, ink);
    fs.draw_text(c, tx, dr.y() + 1, &detail, ink);
}

/// The panel frame shared by the dial and the deck: inset 2 px from the title bar and 4 px from
/// the separator, a label on an ink plate at the top left, lamps at the top right and the keys
/// on an outlined plate at the bottom left.
fn draw_panel(c: &mut Canvas<Window>, ctx: &Ctx, label: &str, lamps: &[(&str, bool)], keys: &str) {
    let fs = ctx.fs;
    let (face, ink) = (panel_face(ctx.tm), widgets::tones(ctx.tm).dark);
    let (mx, my, mw) = (4, CONTENT_Y, 312);
    let frame = Rect::new(mx, my + 2, mw as u32, (PANEL_H - 6) as u32);
    ui::fill_rect(c, frame, face);
    ui::draw_rect(c, frame, ink);
    let lw = fs.measure_display_text(label);
    ui::fill_rect(c, Rect::new(mx + 6, my + 6, (lw + 6) as u32, 14), ink);
    fs.draw_display_text(c, mx + 9, my + 6, label, face);
    draw_lamps(c, fs, mx + mw - 6, my + 6, lamps, ink, face);
    if keys.is_empty() {
        return;
    }
    let kw = fs.measure_text(keys);
    let kr = Rect::new(mx + 6, my + PANEL_H - 22, (kw + 6) as u32, 15);
    ui::draw_rect(c, kr, ink);
    fs.draw_text(c, kr.x() + 3, kr.y() + 1, keys, ink);
}

/// The genre grid above the station list: three columns of round lamps, the chosen genre lit
/// (and the search lamp while a search is active), on the panel face.
fn draw_genres(app: &Radio, c: &mut Canvas<Window>, ctx: &Ctx) {
    let fs = ctx.fs;
    let (face, ink) = (panel_face(ctx.tm), widgets::tones(ctx.tm).dark);
    let r = genre_rect();
    ui::fill_rect(c, r, face);
    ui::draw_rect(c, r, ink);
    for i in 0..=BANDS.len() {
        let cell = genre_cell(i);
        let (label, lit) = if i < BANDS.len() {
            (tr(BANDS[i].0).to_string(), i == app.band)
        } else {
            match &app.station_search {
                Some(q) => (format!("{} {q}", tr("SEARCH:")), true),
                None => (tr("Search").to_string(), false),
            }
        };
        lamp(c, cell.x() + 10, cell.y() + 7, lit, ink, face);
        let label = fit(fs, &label, cell.width() as i32 - 26);
        fs.draw_text(c, cell.x() + 20, widgets::text_origin(cell.y(), cell.height() as i32), &label, ink);
    }
}

fn genre_rect() -> Rect {
    Rect::new(4, CONTENT_Y + 2, 312, 3 * GENRE_ROW as u32 + 2)
}

/// Cell `i` of the genre grid (row by row, three to a row; the eighth is the search).
fn genre_cell(i: usize) -> Rect {
    let r = genre_rect();
    Rect::new(r.x() + (i % 3) as i32 * GENRE_COL, r.y() + 1 + (i / 3) as i32 * GENRE_ROW, GENRE_COL as u32, GENRE_ROW as u32)
}

/// The header row of the podcast lists: the list's name on an ink plate, lamps on the right.
fn draw_header(c: &mut Canvas<Window>, ctx: &Ctx, title: &str, lamps: &[(&str, bool)], saved: bool) {
    let (tm, fs) = (ctx.tm, ctx.fs);
    let (ink, bg) = (tm.get_color("foreground"), tm.get_color("background"));
    // Centred in the header band.
    let y = CONTENT_Y + (HEADER_H - 13) / 2;
    let left = draw_lamps(c, fs, 314, y, lamps, ink, bg);
    let max = left - 16 - if saved { 12 } else { 0 };
    let title = fit_display(fs, title, max);
    let w = fs.measure_display_text(&title);
    ui::fill_rect(c, Rect::new(4, y, (w + 6) as u32, 13), ink);
    fs.draw_display_text(c, 7, y, &title, bg);
    if saved { heart(c, 4 + w + 6 + 5, y + 4, ink); }
}

/// The list rows (stations, shows, episodes): a lamp column (lit for what is on air; a heart for
/// a subscribed show), the name, the detail right-aligned, row rules and the scroll bar.
fn draw_rows(app: &Radio, c: &mut Canvas<Window>, ctx: &Ctx) {
    let (tm, fs) = (ctx.tm, ctx.fs);
    let ink = tm.get_color("foreground");
    let bg = tm.get_color("background");
    let sel = widgets::selection(tm);
    let faint = widgets::secondary_ink(tm);
    let rows = app.rows();
    let (top, vis) = (app.list_top(), app.visible());
    let lamps = app.screen != Screen::Shows;
    for i in 0..vis {
        let idx = app.scroll + i;
        let row = Rect::new(4, top + i as i32 * ROW_H, 304, ROW_H as u32);
        let active = idx == app.cursor && idx < rows.len();
        if active { widgets::draw_row_plate(c, tm, row); }
        let (fg, under) = if active { (sel.fg, sel.bg) } else { (ink, bg) };
        let cy = row.y() + 7;
        if let Some(r) = rows.get(idx) {
            if lamps { lamp(c, row.x() + 8, cy, r.on_air, fg, under); } else if r.saved { heart(c, row.x() + 5, cy - 3, fg); }
            let mut right = row.right() - 4;
            if !r.detail.is_empty() {
                let d = fit(fs, &r.detail, 96);
                right -= fs.measure_text(&d);
                widgets::draw_row_text(c, fs, tm, row, right, &d, active, faint);
                right -= 8;
            }
            if lamps && r.saved {
                heart(c, right - 7, cy - 3, fg);
                right -= 13;
            }
            let name = fit(fs, &r.name, right - (row.x() + 18));
            widgets::draw_row_text(c, fs, tm, row, row.x() + 18, &name, active, ink);
        } else if idx == 0 {
            let msg = if app.loading {
                if app.screen == Screen::Stations { tr("Scanning...") } else { tr("Loading...") }
            } else if app.error.is_some() {
                tr("Network error")
            } else {
                match app.screen {
                    Screen::Stations => tr("No stations"),
                    Screen::Shows => tr("No podcasts"),
                    _ => tr("No episodes"),
                }
            };
            widgets::draw_row_text(c, fs, tm, row, row.x() + 18, msg, false, faint);
        }
        widgets::draw_row_rule(c, tm, row);
    }
    let sb = widgets::ScrollBar::new(Rect::new(SCROLL_X, top, 4, (BOTTOM - top) as u32), rows.len(), vis, app.scroll);
    sb.draw(c, tm, None);
}

/// A 1-bit cassette (shell, label, window with two reels, head opening) with its top-left at x, y.
/// The reel hubs turn with `angle`.
fn draw_cassette(c: &mut Canvas<Window>, x: i32, y: i32, angle: f32, ink: Color, face: Color) {
    let shell = Rect::new(x, y, CASSETTE_W as u32, 60);
    ui::draw_rect(c, shell, ink);
    for (cx, cy) in [(shell.x(), shell.y()), (shell.right() - 1, shell.y()), (shell.x(), shell.bottom() - 1), (shell.right() - 1, shell.bottom() - 1)] {
        ui::fill_rect(c, Rect::new(cx, cy, 1, 1), face); // rounded corners
    }
    for (sx, sy) in [(x + 4, y + 4), (x + CASSETTE_W - 6, y + 4), (x + 4, y + 54), (x + CASSETTE_W - 6, y + 54)] {
        ui::fill_rect(c, Rect::new(sx, sy, 2, 2), ink); // screws
    }
    // The label: side "A" on an ink square and a dotted writing line.
    let label = Rect::new(x + 10, y + 5, CASSETTE_W as u32 - 20, 14);
    ui::draw_rect(c, label, ink);
    ui::fill_rect(c, Rect::new(label.x() + 2, label.y() + 2, 10, 10), ink);
    // An "A" drawn in pixels (the face's A sits too low in a 10 px box).
    for (dx, dy, w, h) in [(4, 2, 2, 1), (3, 3, 1, 2), (6, 3, 1, 2), (2, 5, 1, 4), (7, 5, 1, 4), (3, 6, 4, 1)] {
        ui::fill_rect(c, Rect::new(label.x() + 2 + dx, label.y() + 1 + dy, w, h), face);
    }
    for k in (label.x() + 18..label.right() - 4).step_by(2) {
        ui::fill_rect(c, Rect::new(k, label.y() + 10, 1, 1), ink);
    }
    // The window with the reels and the tape running between them.
    let win = Rect::new(x + 44, y + 23, CASSETTE_W as u32 - 88, 26);
    ui::draw_rect(c, win, ink);
    let cy = win.y() + 13;
    let reels = [win.x() + 20, win.right() - 21];
    for (k, &cx) in reels.iter().enumerate() {
        // Tape pack: a checker ring between the hub and the rim.
        for dy in -10i32..=10 {
            for dx in -10i32..=10 {
                let d2 = dx * dx + dy * dy;
                if d2 > 30 && d2 <= 100 && (dx + dy).rem_euclid(2) == 0 {
                    ui::fill_rect(c, Rect::new(cx + dx, cy + dy, 1, 1), ink);
                }
            }
        }
        ui::draw_circle(c, (cx, cy), 10, ink);
        ui::draw_circle(c, (cx, cy), 5, ink);
        let a0 = angle + k as f32 * 0.5;
        for t in 0..3 {
            let a = a0 + t as f32 * std::f32::consts::TAU / 3.0;
            let (s, co) = a.sin_cos();
            ui::line(c, cx + (co * 2.0).round() as i32, cy + (s * 2.0).round() as i32, cx + (co * 4.0).round() as i32, cy + (s * 4.0).round() as i32, ink);
        }
    }
    ui::fill_rect(c, Rect::new(reels[0], cy + 10, (reels[1] - reels[0]) as u32, 1), ink);
    // The head opening at the bottom edge.
    let (by, ty) = (y + 59, y + 52);
    ui::line(c, x + 40, by, x + 47, ty, ink);
    ui::line(c, x + 47, ty, x + CASSETTE_W - 48, ty, ink);
    ui::line(c, x + CASSETTE_W - 48, ty, x + CASSETTE_W - 41, by, ink);
    for hx in [x + 60, x + CASSETTE_W / 2 - 2, x + CASSETTE_W - 64] {
        ui::draw_rect(c, Rect::new(hx, y + 54, 4, 3), ink);
    }
}

/// The podcast deck: a cassette, the tape counter (play time) in 7-segment digits, On air /
/// Paused lamps, and the episode under the separator (number, title, show).
fn draw_deck(app: &Radio, c: &mut Canvas<Window>, assets: &mut AssetStore, ctx: &Ctx) {
    use crate::launcher::sevenseg as seg;
    let (tm, fs) = (ctx.tm, ctx.fs);
    let (face, ink) = (panel_face(tm), widgets::tones(tm).dark);
    let muted = tm.get_color("muted");
    let episode = app.episodes.get(app.cursor);
    let this = audio::snapshot().filter(|p| p.podcast && episode.is_some_and(|e| e.audio == p.url));
    let played = this.as_ref().and_then(|_| audio::elapsed()).unwrap_or_default();
    let (on_air, paused) = (this.as_ref().is_some_and(|p| !p.paused), this.as_ref().is_some_and(|p| p.paused));
    draw_panel(c, ctx, tr("PODCAST"), &[(tr("On air"), on_air), (tr("Paused"), paused)], "");

    let (x, y) = (14, CONTENT_Y + 30);
    draw_cassette(c, x, y, played.as_secs_f32() * 3.0, ink, face);
    // Tape counter: minutes and seconds played.
    let counter = Rect::new(x + CASSETTE_W + 8, y, (304 - (x + CASSETTE_W + 8)) as u32, 60);
    ui::draw_rect(c, counter, ink);
    let label = tr("Counter");
    fs.draw_text(c, counter.x() + (counter.width() as i32 - fs.measure_text(label)) / 2, counter.y() + 4, label, ink);
    let secs = played.as_secs().min(99 * 60 + 59);
    let digits = [(secs / 600) as usize, (secs / 60 % 10) as usize, (secs % 60 / 10) as usize, (secs % 10) as usize];
    let col = if this.is_some() { ink } else { muted };
    let mut dx = counter.x() + (counter.width() as i32 - (4 * 17 + 6)) / 2;
    let dy = counter.y() + 26;
    for (k, d) in digits.into_iter().enumerate() {
        seg::draw_atlas_digit(c, assets, seg::BIG_KEY, 14, 20, d, dx, dy, col);
        dx += 17;
        if k == 1 {
            ui::fill_rect(c, Rect::new(dx, dy + 5, 2, 2), col);
            ui::fill_rect(c, Rect::new(dx, dy + 13, 2, 2), col);
            dx += 6;
        }
    }

    let name = episode.map(|e| e.title.clone())
        .unwrap_or_else(|| if app.loading { tr("Loading...").into() } else { tr("No episodes").into() });
    let detail = app.chosen_show.as_ref().map(|s| if s.author.is_empty() { s.title.clone() } else { format!("{}  {}", s.title, s.author) })
        .unwrap_or_default();
    draw_readout(c, assets, ctx, app.cursor + 1, app.episodes.len(), &name, &detail);
}

/// The search field over the top of the screen: "Search:" on an ink plate, the text, a caret.
fn draw_search(app: &Radio, c: &mut Canvas<Window>, ctx: &Ctx) {
    let (tm, fs) = (ctx.tm, ctx.fs);
    let (face, ink) = (panel_face(tm), widgets::tones(tm).dark);
    let r = match app.screen {
        Screen::Stations => genre_rect(),
        Screen::Shows | Screen::EpisodeList => {
            // Hide the header and the first row whole, so no rule is left half covered.
            // (The scroll bar column stays.)
            // The header band whole (its lamps reach x 314), then the first row up to the scroll bar.
            ui::fill_rect(c, Rect::new(0, CONTENT_Y, 320, (app.list_top() - CONTENT_Y) as u32), tm.get_color("background"));
            ui::fill_rect(c, Rect::new(0, app.list_top(), SCROLL_X as u32 - 4, ROW_H as u32), tm.get_color("background"));
            Rect::new(4, CONTENT_Y + 2, 304, 28)
        }
        _ => Rect::new(4, CONTENT_Y + 2, 312, 28),
    };
    ui::fill_rect(c, r, face);
    ui::draw_rect(c, r, ink);
    let fy = r.y() + (r.height() as i32 - widgets::FIELD_H) / 2;
    let label = tr("SEARCH:");
    let lw = fs.measure_display_text(label);
    ui::fill_rect(c, Rect::new(r.x() + 6, fy, (lw + 6) as u32, widgets::FIELD_H as u32), ink);
    fs.draw_display_text(c, r.x() + 9, fy + 1, label, face);
    // The shared text field beside the label, as tall as the label plate area allows (one row).
    let fx = r.x() + 6 + lw + 6 + 6;
    let field = Rect::new(fx, r.y() + (r.height() as i32 - widgets::FIELD_H) / 2, (r.right() - 6 - fx) as u32, widgets::FIELD_H as u32);
    widgets::draw_field(c, fs, tm, field, &widgets::Field { text: &app.search.text, caret: Some(app.search.caret), fill: Some(face), ink: Some(ink), ..Default::default() });
}

/// A message (a network error) on an outlined plate in the last row.
fn draw_error(c: &mut Canvas<Window>, ctx: &Ctx, err: &str) {
    let (tm, fs) = (ctx.tm, ctx.fs);
    let (face, ink) = (panel_face(tm), widgets::tones(tm).dark);
    let r = Rect::new(4, BOTTOM - 15, 304, 15);
    ui::fill_rect(c, r, face);
    ui::draw_rect(c, r, ink);
    fs.draw_text(c, r.x() + 4, r.y() + 1, &fit(fs, &tr_s(err), 294), ink);
}

impl LauncherApp for Radio {
    fn title(&self) -> String { crate::launcher::i18n::tr_s("Radio") }

    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Tab", "Radio / Podcast"),
            k("Left / Right", "Tune (Radio)"),
            k("Up / Down", "Band (Radio), episode (Podcast)"),
            k("Return", "Play"),
            k("Space", "Pause"),
        ]
    }

    fn describe(&self) -> String {
        let now = audio::snapshot().map(|n| format!("playing {}", n.label)).unwrap_or_else(|| "stopped".into());
        format!("Radio: {}; {} entries; {now}", self.list_name(), self.count())
    }

    fn wants_tab(&self) -> bool { true }

    fn menu(&self) -> Vec<MenuItem> {
        vec![
            MenuItem::new("radio", "Radio tuner"),
            MenuItem::new("podcasts", "Podcasts"),
            MenuItem::new("search", "Search..."),
            MenuItem::new("browse", "Browse / stations"),
            MenuItem::new("saved", "Favorites / subscriptions"),
            MenuItem::new("favorite", "Save / Remove"),
            MenuItem::new("refresh", "Refresh"),
            MenuItem::new("pause", "Play / pause"),
            MenuItem::new("stop", "Stop audio"),
        ]
    }

    fn menu_action(&mut self, id: &str, _ctx: &Ctx) -> AppAction {
        match id {
            "radio" => {
                self.mode = Mode::Radio;
                self.screen = Screen::Dial;
                self.reset_selection();
                if self.stations.is_empty() { self.refresh_stations(); }
            }
            "podcasts" => {
                self.mode = Mode::Podcasts;
                self.screen = Screen::Shows;
                self.reset_selection();
                if self.shows.is_empty() { self.refresh_shows(); }
            }
            "search" => self.start_search(),
            "browse" => {
                if self.mode == Mode::Radio {
                    self.screen = Screen::Stations;
                } else { self.screen = Screen::Shows; }
                self.reset_selection();
            }
            "saved" => self.show_saved(),
            "favorite" => return self.toggle_saved(),
            "refresh" => if self.mode == Mode::Radio { self.refresh_stations(); } else { self.refresh_shows(); },
            "pause" => audio::toggle_pause(),
            "stop" => audio::stop(),
            _ => {}
        }
        AppAction::None
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, _ctx: &Ctx) -> AppAction {
        if self.editing {
            match key {
                Keycode::Escape => self.editing = false,
                Keycode::Return | Keycode::KpEnter => self.submit_search(),
                _ => { self.search.key(key, mods); }
            }
            return AppAction::None;
        }
        // F is also the physical Up arrow alias; Ctrl+F must be checked
        // before translating Cardputer's F/X/Z/C directional keys.
        if key == Keycode::F && mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD) {
            self.start_search();
            return AppAction::None;
        }
        let k = arrow_alias(key);
        match k {
            Keycode::Tab => self.switch_mode(),
            Keycode::P => return self.toggle_saved(),
            Keycode::B => self.show_saved(),
            Keycode::R => {
                if self.mode == Mode::Radio { self.refresh_stations(); }
                else if matches!(self.screen, Screen::Episodes | Screen::EpisodeList) {
                    if let Some(feed) = self.chosen_show.as_ref().map(|s| s.feed.clone()) {
                        self.start(Job::Episodes(feed));
                    }
                } else { self.refresh_shows(); }
            }
            Keycode::Space => audio::toggle_pause(),
            Keycode::Escape | Keycode::Backspace => {
                self.editing = false;
                if self.screen == Screen::Episodes {
                    // From the deck back to the episode list, on the same episode.
                    self.screen = Screen::EpisodeList;
                    self.follow();
                } else {
                    self.screen = match self.screen {
                        Screen::EpisodeList => Screen::Shows,
                        Screen::Stations => Screen::Dial,
                        other => other,
                    };
                    self.reset_selection();
                }
            }
            Keycode::L if self.mode == Mode::Radio => {
                self.screen = if self.screen == Screen::Dial { Screen::Stations } else { Screen::Dial };
                self.reset_selection();
            }
            Keycode::L if matches!(self.screen, Screen::Episodes | Screen::EpisodeList) => {
                self.screen = if self.screen == Screen::Episodes { Screen::EpisodeList } else { Screen::Episodes };
                self.follow();
            }
            Keycode::Up if self.screen == Screen::Dial => self.band_step(-1),
            Keycode::Down if self.screen == Screen::Dial => self.band_step(1),
            Keycode::Up => self.select(-1),
            Keycode::Down => self.select(1),
            Keycode::Left if self.screen == Screen::Dial => self.select(-1),
            Keycode::Right if self.screen == Screen::Dial => self.select(1),
            Keycode::Return | Keycode::KpEnter | Keycode::Right => return self.play_selected(),
            _ => {}
        }
        AppAction::None
    }

    fn text(&mut self, text: &str, _ctx: &Ctx) -> AppAction {
        if self.editing && self.search.len() < 80 {
            self.search.insert(text);
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, _ctx: &Ctx) -> AppAction {
        match ev {
            PointerEvent::Wheel { dy, .. } => {
                if self.screen == Screen::Dial { self.select(if dy > 0 { -1 } else { 1 }); }
                else { self.select(if dy > 0 { -1 } else { 1 }); }
            }
            PointerEvent::Down { x, y, button: 0, clicks } => {
                let header = match self.screen {
                    Screen::Dial => y < 42,
                    Screen::Shows | Screen::EpisodeList => y < self.list_top(),
                    _ => false,
                };
                if header && x > 230 {
                    self.switch_mode();
                } else if self.editing {
                    // Editing uses keyboard input; tapping the display does not dismiss it.
                } else if self.screen == Screen::Dial {
                    if (96..CONTENT_Y + PANEL_H).contains(&y) && x < 243 {
                        let n = self.stations.len();
                        if n > 0 {
                            self.cursor = ((x - 20).clamp(0, 210) as usize * (n - 1) / 210).min(n - 1);
                        }
                    } else if x > 246 && y > 96 {
                        return self.play_selected();
                    } else if y >= CONTENT_Y + PANEL_H && x < 245 {
                        self.band_step(1);
                    } else if clicks >= 2 {
                        return self.play_selected();
                    }
                } else if self.screen == Screen::Episodes {
                    if clicks >= 2 { return self.play_selected(); }
                } else if self.screen == Screen::Stations && widgets::contains(genre_rect(), x, y) {
                    let i = (0..=BANDS.len()).find(|&i| widgets::contains(genre_cell(i), x, y));
                    match i {
                        Some(i) if i < BANDS.len() => self.set_band(i),
                        Some(_) => self.start_search(),
                        None => {}
                    }
                } else if (self.list_top()..self.list_top() + self.visible() as i32 * ROW_H).contains(&y) && x < SCROLL_X {
                    let idx = self.scroll + ((y - self.list_top()) / ROW_H) as usize;
                    if idx < self.count() {
                        self.cursor = idx;
                        if clicks >= 2 { return self.play_selected(); }
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, _ctx: &Ctx) -> bool {
        let mut changed = false;
        while let Ok(reply) = self.rx.try_recv() {
            changed |= self.handle_reply(reply);
        }
        let now = audio::snapshot();
        let reels = self.screen == Screen::Episodes && now.as_ref().is_some_and(|p| p.podcast && !p.paused);
        let playing = now.map(|n| (n.label, n.paused));
        if playing != self.last_audio {
            self.last_audio = playing;
            changed = true;
        }
        // Turning reels, the tape counter and the blinking Loading lamp: about eight frames a second.
        if (reels || self.loading) && self.last_anim.elapsed() >= std::time::Duration::from_millis(125) {
            self.last_anim = std::time::Instant::now();
            changed = true;
        }
        changed
    }

    fn draw(&mut self, c: &mut Canvas<Window>, assets: &mut AssetStore, ctx: &Ctx) {
        let blink = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_millis() < 500).unwrap_or(true);
        if !matches!(self.screen, Screen::Dial | Screen::Episodes) {
            ui::fill_rect(c, chrome::app_rect(), ctx.tm.get_color("background"));
        }
        match self.screen {
            Screen::Dial => draw_dial(self, c, assets, ctx),
            Screen::Episodes => draw_deck(self, c, assets, ctx),
            Screen::Stations => {
                draw_genres(self, c, ctx);
                draw_rows(self, c, ctx);
            }
            Screen::Shows => {
                draw_header(c, ctx, &self.list_name(), &[(tr("Loading"), self.loading && blink)], false);
                draw_rows(self, c, ctx);
            }
            Screen::EpisodeList => {
                let show = self.chosen_show.as_ref();
                let saved = show.is_some_and(|s| self.library.podcasts.iter().any(|p| p.feed == s.feed));
                let on_air = audio::snapshot().is_some_and(|p| p.podcast && !p.paused);
                let title = show.map(|s| s.title.clone()).unwrap_or_else(|| tr("EPISODES").into());
                draw_header(c, ctx, &title, &[(tr("On air"), on_air), (tr("Loading"), self.loading && blink)], saved);
                draw_rows(self, c, ctx);
            }
        }
        if self.editing {
            draw_search(self, c, ctx);
        } else if let Some(err) = &self.error {
            draw_error(c, ctx, err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_wrap_and_saved_is_offline() {
        let mut app = Radio::new();
        app.mock = true;
        app.band = 0;
        app.band_step(-1);
        assert_eq!(app.band, FAV_BAND);
        assert!(!app.loading);
        app.band_step(1);
        assert_eq!(app.band, 0);
    }

    #[test]
    fn stale_network_results_do_not_replace_new_selection() {
        let mut app = Radio::new();
        app.serial = 3;
        assert!(!app.handle_reply((2, Ok(Fetched::Stations(vec![])))));
        assert_eq!(app.serial, 3);
    }

    fn podcast_app() -> Radio {
        let mut app = Radio::new();
        app.mock = true;
        app.mode = Mode::Podcasts;
        app.screen = Screen::Shows;
        app.shows = vec![Podcast { title: "Show".into(), author: "A".into(), feed: "https://example.org/rss".into() }];
        app
    }

    #[test]
    fn a_show_opens_as_a_list_and_enter_shows_the_deck() {
        let mut app = podcast_app();
        app.open_show();
        assert!(app.screen == Screen::EpisodeList);
        app.episodes = (0..12).map(|i| Episode { title: format!("E{i}"), audio: format!("https://example.org/{i}.mp3") }).collect();
        app.cursor = 9;
        app.follow();
        assert_eq!(app.scroll, 2); // eight rows under the header
        // Enter plays and shows the deck (mock: a toast, no sound).
        app.play_selected();
        assert!(app.screen == Screen::Episodes);
        assert_eq!(app.cursor, 9, "the deck shows the episode chosen in the list");
    }

    #[test]
    fn genre_cells_tile_the_grid_without_overlap() {
        let r = genre_rect();
        for i in 0..=BANDS.len() {
            let c = genre_cell(i);
            assert!(c.x() >= r.x() && c.right() <= r.right() && c.y() > r.y() && c.bottom() < r.bottom(), "cell {i}");
        }
        // The station list starts below the grid with free pixel rows between, and its six rows
        // end 1 px above the screen's bottom line (y 168).
        let app = Radio::new();
        assert!(r.bottom() < app.list_top());
        assert_eq!(app.list_top() + app.visible() as i32 * ROW_H, APP_FRAME_BOTTOM);
    }
}
