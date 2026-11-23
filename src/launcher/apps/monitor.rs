//! Monitor: live system numbers on six pages (Summary, CPU, Memory, Power, Net, Disk).
//!
//! One snapshot per second from [`Backend::telemetry`] (a few small procfs / sysfs reads) and
//! only while the app is open. Counters are turned into rates here, history is a ring of
//! [`HIST`] samples drawn two pixels wide. No window frame: the pages fill the area under the
//! menu strip. Left / Right (or Tab, 1..6) change page, Up / Down scroll the interface list
//! on Net, P pauses, Esc returns to Overview. Tabs are clickable.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use sdl2::keyboard::{Keycode, Mod};
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome;
use crate::dither::{self, Pattern};
use crate::grid;
use crate::launcher::i18n;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::widgets::{self, draw_meter, MenuItem, PointerEvent, ScrollBar, Tabs};
use crate::platform::keys::arrow_alias;
use crate::system::telemetry::{self, CpuTimes, Telemetry};
use crate::system::{Backend, SysInfo};
use crate::ui;

/// Samples kept per series: a 144 px graph (142 px inside the frame) shows 71 columns of 2 px.
pub const HIST: usize = 71;
const SAMPLE_EVERY: Duration = Duration::from_secs(1);

const ROW: i32 = grid::ROW_H;
/// The page starts under the shared folder tab strip and has 8 whole rows (y 36..164); graphs,
/// cards and dividers run on to [`BOTTOM`], so the screen is used down to its bottom.
const BODY_Y: i32 = widgets::FOLDER_BODY_Y;
const BODY_ROWS: i32 = 8;
/// The last row a page draws on: 1 px above the screen's last row (`grid::APP_FRAME_BOTTOM`).
const BOTTOM: i32 = grid::APP_FRAME_BOTTOM;
/// What is left under the 8 body rows (y 164..168): pages whose rows would stop there give it to
/// a graph or a meter, so their last rule sits on y 168.
const SLACK: i32 = BOTTOM + 1 - (BODY_Y + BODY_ROWS * ROW);
const X0: i32 = grid::MARGIN;
/// Right edge of the text columns (the scroll bar column starts at 312).
const RIGHT: i32 = grid::SCROLL_X - grid::MARGIN;
const GRAPH_W: i32 = 144;
const TABS: [&str; 6] = ["Summary", "CPU", "Memory", "Power", "Net", "Disk"];
const TABS_JA: [&str; 6] = ["概要", "CPU", "メモリ", "電源", "通信", "ディスク"];
const P_OVERVIEW: usize = 0;
const P_CPU: usize = 1;
const P_MEM: usize = 2;
const P_POWER: usize = 3;
const P_NET: usize = 4;
const P_DISK: usize = 5;
/// One interface: a header band and three rows (graph on the left, totals on the right).
const NET_BLOCK: i32 = 4 * ROW;
const NET_VISIBLE: usize = 2;

// ------------------------------------------------------------------ data

/// A fixed-size history of samples, newest last.
#[derive(Clone, Debug)]
pub struct Ring {
    q: VecDeque<f32>,
    cap: usize,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self { q: VecDeque::with_capacity(cap), cap }
    }
    pub fn push(&mut self, v: f32) {
        if self.q.len() == self.cap {
            self.q.pop_front();
        }
        self.q.push_back(v);
    }
    pub fn clear(&mut self) {
        self.q.clear();
    }
    pub fn len(&self) -> usize {
        self.q.len()
    }
    pub fn is_empty(&self) -> bool {
        self.q.is_empty()
    }
    pub fn last(&self) -> Option<f32> {
        self.q.back().copied()
    }
    pub fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        self.q.iter().copied()
    }
    pub fn max(&self) -> Option<f32> {
        self.q.iter().copied().fold(None, |m, v| Some(m.map_or(v, |m: f32| m.max(v))))
    }
    pub fn min(&self) -> Option<f32> {
        self.q.iter().copied().fold(None, |m, v| Some(m.map_or(v, |m: f32| m.min(v))))
    }
    pub fn avg(&self) -> Option<f32> {
        (!self.q.is_empty()).then(|| self.q.iter().sum::<f32>() / self.q.len() as f32)
    }
}

/// Per-core busy fraction between two snapshots (aggregate first). Missing / stale cores give `None`.
pub fn cpu_percents(prev: &[CpuTimes], cur: &[CpuTimes]) -> Vec<Option<f32>> {
    cur.iter().enumerate().map(|(i, c)| prev.get(i).and_then(|p| telemetry::cpu_usage(*p, *c))).collect()
}

/// Bytes per second between two counter readings; a counter that went backwards (interface
/// reset, 32-bit wrap) counts as no traffic rather than a huge spike.
pub fn net_rate(prev: u64, cur: u64, dt_s: f64) -> f64 {
    if dt_s <= 0.0 || cur < prev {
        return 0.0;
    }
    (cur - prev) as f64 / dt_s
}

/// A scale for a graph that shows `v`: a little headroom, never narrower than `min_span`.
pub fn auto_range(lo: f32, hi: f32, min_span: f32) -> (f32, f32) {
    let (mut l, mut h) = ((lo - 2.0).floor(), (hi + 2.0).ceil());
    if h - l < min_span {
        let mid = (l + h) / 2.0;
        l = (mid - min_span / 2.0).floor();
        h = l + min_span;
    }
    (l, h)
}

/// The next 1 / 2 / 5 x 10^n at or above `v` (graph scale for rates).
pub fn nice_ceil(v: f32) -> f32 {
    if v <= 0.0 {
        return 1.0;
    }
    let mag = 10f32.powf(v.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if v <= m * mag {
            return m * mag;
        }
    }
    10.0 * mag
}

pub fn fmt_bytes(b: u64) -> String {
    let f = b as f64;
    if b < 1024 {
        format!("{b} B")
    } else if f < 1024.0 * 1024.0 {
        format!("{:.1} KB", f / 1024.0)
    } else if f < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", f / 1048576.0)
    } else {
        format!("{:.2} GB", f / 1073741824.0)
    }
}

pub fn fmt_rate(bps: f64) -> String {
    if bps < 1024.0 {
        format!("{:.0} B/s", bps)
    } else if bps < 1024.0 * 1024.0 {
        format!("{:.1} KB/s", bps / 1024.0)
    } else {
        format!("{:.1} MB/s", bps / 1048576.0)
    }
}

pub fn fmt_kb(kb: u64) -> String {
    if kb < 1024 * 1024 {
        format!("{} MB", kb / 1024)
    } else {
        format!("{:.2} GB", kb as f64 / 1048576.0)
    }
}

pub fn fmt_mb(mb: u64) -> String {
    if mb < 1024 {
        format!("{mb} MB")
    } else {
        format!("{:.1} GB", mb as f64 / 1024.0)
    }
}

pub fn fmt_pct(f: f32) -> String {
    format!("{:.0}%", f.clamp(0.0, 1.0) * 100.0)
}

pub fn fmt_minutes(m: u32) -> String {
    if m >= 60 {
        format!("{} h {:02} m", m / 60, m % 60)
    } else {
        format!("{m} m")
    }
}

pub fn fmt_uptime(s: u64) -> String {
    let (d, h, m) = (s / 86400, s / 3600 % 24, s / 60 % 60);
    if d > 0 {
        format!("{d} d {h} h")
    } else if h > 0 {
        format!("{h} h {m:02} m")
    } else {
        format!("{m} m {:02} s", s % 60)
    }
}

fn fmt_temp(t: Option<f32>) -> String {
    t.map(|t| format!("{t:.1} C")).unwrap_or_else(|| "-".into())
}

#[derive(Clone, Debug)]
struct NetSeries {
    name: String,
    up: bool,
    rx: Ring,
    tx: Ring,
    rx_rate: f64,
    tx_rate: f64,
    rx_total: u64,
    tx_total: u64,
}

pub struct Monitor {
    page: usize,
    paused: bool,
    last: Option<Instant>,
    prev: Option<(Instant, Telemetry)>,
    cur: Telemetry,
    info: Option<SysInfo>,
    cpu: Ring,
    /// Latest per-core busy fractions (cores only, no aggregate).
    cores: Vec<f32>,
    cpu_now: Option<f32>,
    mem: Ring,
    temp: Ring,
    net: Vec<NetSeries>,
    net_scroll: usize,
    dirty: bool,
}

pub fn make() -> Box<dyn LauncherApp> {
    Box::new(Monitor::new())
}

impl Monitor {
    pub fn new() -> Self {
        Self {
            page: P_OVERVIEW,
            paused: false,
            last: None,
            prev: None,
            cur: Telemetry::default(),
            info: None,
            cpu: Ring::new(HIST),
            cores: Vec::new(),
            cpu_now: None,
            mem: Ring::new(HIST),
            temp: Ring::new(HIST),
            net: Vec::new(),
            net_scroll: 0,
            dirty: true,
        }
    }

    /// Take one snapshot and fold it into the histories.
    pub fn sample(&mut self, sys: &dyn Backend, now: Instant) {
        let t = sys.telemetry();
        if let Some((pt, p)) = &self.prev {
            let dt = now.saturating_duration_since(*pt).as_secs_f64();
            let pct = cpu_percents(&p.cpu, &t.cpu);
            self.cpu_now = pct.first().copied().flatten();
            if let Some(v) = self.cpu_now {
                self.cpu.push(v);
            }
            self.cores = pct.iter().skip(1).map(|v| v.unwrap_or(0.0)).collect();
            let mut next = Vec::with_capacity(t.net.len());
            for n in &t.net {
                let old = self.net.iter().find(|s| s.name == n.name);
                let before = p.net.iter().find(|q| q.name == n.name);
                let mut s = old.cloned().unwrap_or_else(|| NetSeries {
                    name: n.name.clone(),
                    up: n.up,
                    rx: Ring::new(HIST),
                    tx: Ring::new(HIST),
                    rx_rate: 0.0,
                    tx_rate: 0.0,
                    rx_total: 0,
                    tx_total: 0,
                });
                if let Some(b) = before {
                    s.rx_rate = net_rate(b.rx_bytes, n.rx_bytes, dt);
                    s.tx_rate = net_rate(b.tx_bytes, n.tx_bytes, dt);
                    s.rx.push(s.rx_rate as f32);
                    s.tx.push(s.tx_rate as f32);
                }
                s.up = n.up;
                s.rx_total = n.rx_bytes;
                s.tx_total = n.tx_bytes;
                next.push(s);
            }
            self.net = next;
        }
        if t.mem.total_kb > 0 {
            self.mem.push(t.mem.used_kb() as f32 / t.mem.total_kb as f32);
        }
        if let Some(c) = t.temp_c {
            self.temp.push(c);
        }
        let max_scroll = self.net.len().saturating_sub(NET_VISIBLE);
        self.net_scroll = self.net_scroll.min(max_scroll);
        self.prev = Some((now, t.clone()));
        self.cur = t;
        self.last = Some(now);
    }

    fn clear(&mut self) {
        for r in [&mut self.cpu, &mut self.mem, &mut self.temp] {
            r.clear();
        }
        for n in &mut self.net {
            n.rx.clear();
            n.tx.clear();
        }
    }

    fn set_page(&mut self, p: usize) {
        self.page = p % TABS.len();
        self.dirty = true;
    }

    fn step_page(&mut self, d: i32) {
        self.set_page((self.page as i32 + d).rem_euclid(TABS.len() as i32) as usize);
    }

    fn scroll_net(&mut self, d: i32) {
        let max = self.net.len().saturating_sub(NET_VISIBLE);
        self.net_scroll = (self.net_scroll as i32 + d).clamp(0, max as i32) as usize;
        self.dirty = true;
    }

    fn tabs(&self, ctx: &Ctx) -> Tabs<'static> {
        Tabs::folder_fit(ctx.fs, if i18n::get() == i18n::Lang::Ja { &TABS_JA } else { &TABS }, self.page, ctx.tm)
    }
}

// ------------------------------------------------------------------ drawing helpers

struct Pal {
    ink: Color,
    paper: Color,
    faint: Color,
    alt: Color,
    line: Color,
    bg: Color,
}

fn text_l(c: &mut Canvas<Window>, ctx: &Ctx, x: i32, row_y: i32, s: &str, col: Color) {
    ctx.fs.draw_text(c, x, widgets::text_origin(row_y, ROW), &i18n::tr_s(s), col);
}

fn text_r(c: &mut Canvas<Window>, ctx: &Ctx, right: i32, row_y: i32, s: &str, col: Color) {
    let w = ctx.fs.measure_text(&i18n::tr_s(s));
    text_l(c, ctx, right - w, row_y, s, col);
}

/// A 1 px rule on the last pixel row of the row starting at `row_y`.
fn rule(c: &mut Canvas<Window>, x: i32, row_y: i32, w: i32, col: Color) {
    ui::line(c, x, row_y + ROW - 1, x + w - 1, row_y + ROW - 1, col);
}

/// Label on the left (secondary ink), value on the right, rule underneath.
fn kv(c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal, x: i32, w: i32, row_y: i32, key: &str, val: &str) {
    text_l(c, ctx, x, row_y, key, p.faint);
    let kw = ctx.fs.measure_text(&i18n::tr_s(key));
    let v = chrome::fit_text(ctx.fs, &i18n::tr_s(val), w - kw - 8);
    text_r(c, ctx, x + w, row_y, &v, p.ink);
    rule(c, x, row_y, w, p.line);
}

/// A header band: zebra tone, a rule, left and right text.
fn band(c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal, y: i32, left: &str, right: &str) {
    let r = Rect::new(0, y, grid::SCREEN_W as u32, ROW as u32);
    ui::fill_rect(c, r, p.alt);
    ui::line(c, 0, y + ROW - 1, grid::SCREEN_W - 1, y + ROW - 1, p.ink);
    text_l(c, ctx, X0, y, left, p.ink);
    text_r(c, ctx, RIGHT, y, right, p.ink);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    /// Dithered area with a solid cap: the default look of a history.
    Area,
    Solid,
    Half,
}

/// An outlined graph: columns of 2 px, newest at the right edge, values scaled `lo..hi`.
fn graph(c: &mut Canvas<Window>, p: &Pal, r: Rect, series: &[(&Ring, Style)], lo: f32, hi: f32) {
    ui::fill_rect(c, r, p.paper);
    let inner = Rect::new(r.x() + 1, r.y() + 1, r.width() - 2, r.height() - 2);
    let h = inner.height() as i32;
    let span = (hi - lo).max(1e-6);
    let cols = inner.width() as usize / 2;
    for (ring, style) in series {
        let n = ring.len().min(cols);
        let skip = ring.len() - n;
        for (i, v) in ring.iter().skip(skip).enumerate() {
            let x = inner.right() - (n - i) as i32 * 2;
            let hh = (((v - lo) / span).clamp(0.0, 1.0) * h as f32).round() as i32;
            if hh == 0 {
                continue;
            }
            let col = Rect::new(x, inner.bottom() - hh, 2, hh as u32);
            match style {
                Style::Solid => ui::fill_rect(c, col, p.ink),
                Style::Half => dither::fill(c, col, p.ink, Pattern::Half),
                Style::Area => {
                    dither::fill(c, col, p.ink, Pattern::Quarter);
                    ui::fill_rect(c, Rect::new(x, col.y(), 2, 1), p.ink);
                }
            }
        }
    }
    ui::draw_rect(c, r, p.ink);
}

/// A bar split into parts (fraction of the whole, dithered or solid), on the meter's frame.
fn stacked_bar(c: &mut Canvas<Window>, p: &Pal, r: Rect, parts: &[(f32, bool)]) {
    ui::fill_rect(c, r, p.paper);
    ui::draw_rect(c, r, p.ink);
    let inner = Rect::new(r.x() + 2, r.y() + 2, r.width() - 4, r.height() - 4);
    let (mut acc, mut x0) = (0.0f32, inner.x());
    for (f, dith) in parts {
        acc = (acc + f).clamp(0.0, 1.0);
        let x1 = inner.x() + (inner.width() as f32 * acc).round() as i32;
        if x1 > x0 {
            let seg = Rect::new(x0, inner.y(), (x1 - x0) as u32, inner.height());
            if *dith {
                dither::fill(c, seg, p.ink, Pattern::Half);
            } else {
                ui::fill_rect(c, seg, p.ink);
            }
        }
        x0 = x1;
    }
}

/// A framed card of the Overview: title row (label left, value right), a rule, then the body.
fn card(c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal, r: Rect, title: &str, value: &str) {
    ui::fill_rect(c, r, p.paper);
    ui::draw_rect(c, r, p.ink);
    text_l(c, ctx, r.x() + 4, r.y(), title, p.faint);
    text_r(c, ctx, r.right() - 4, r.y(), value, p.ink);
    ui::line(c, r.x() + 1, r.y() + ROW, r.right() - 2, r.y() + ROW, p.line);
}

// ------------------------------------------------------------------ pages

impl Monitor {
    fn draw_overview(&self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let t = &self.cur;
        let cw = 152;
        // Two rows of cards, 1 px clear of the tab strip's rule, the lower one ending on the bottom
        // line (y 37..100 and 105..168).
        let ch = (BOTTOM + 1 - (BODY_Y + 1) - grid::GUTTER) / 2;
        let cell = |i: i32| Rect::new(X0 + (i % 2) * (cw + grid::GUTTER), BODY_Y + 1 + (i / 2) * (ch + grid::GUTTER), cw as u32, ch as u32);

        let r = cell(0);
        card(c, ctx, p, r, "CPU", &self.cpu_now.map(fmt_pct).unwrap_or_else(|| "-".into()));
        graph(c, p, Rect::new(r.x() + 4, r.y() + 20, GRAPH_W as u32, (ch - 24) as u32), &[(&self.cpu, Style::Area)], 0.0, 1.0);

        let r = cell(1);
        let m = &t.mem;
        let total = m.total_kb.max(1) as f32;
        card(c, ctx, p, r, "Memory", &fmt_pct(m.used_kb() as f32 / total));
        stacked_bar(c, p, Rect::new(r.x() + 4, r.y() + 22, GRAPH_W as u32, 12), &[(m.used_kb() as f32 / total, false), (m.cache_kb() as f32 / total, true)]);
        let line = format!("{} {}  {} {}", fmt_kb(m.used_kb()), i18n::tr("used"), fmt_kb(m.cache_kb()), i18n::tr("cache"));
        text_l(c, ctx, r.x() + 4, r.y() + 34, &chrome::fit_text(ctx.fs, &line, cw - 8), p.faint);
        let line = format!("{} {}  {} {}", fmt_kb(m.free_only_kb()), i18n::tr("free"), fmt_kb(m.total_kb), i18n::tr("total"));
        text_l(c, ctx, r.x() + 4, r.y() + 47, &chrome::fit_text(ctx.fs, &line, cw - 8), p.faint);

        let r = cell(2);
        card(c, ctx, p, r, "Temp", &fmt_temp(t.temp_c));
        let (lo, hi) = match (self.temp.min(), self.temp.max()) {
            (Some(a), Some(b)) => auto_range(a, b, 10.0),
            _ => (30.0, 80.0),
        };
        graph(c, p, Rect::new(r.x() + 4, r.y() + 20, GRAPH_W as u32, (ch - 24) as u32), &[(&self.temp, Style::Area)], lo, hi);

        let r = cell(3);
        let b = &t.power;
        card(c, ctx, p, r, "Battery", &b.percent.map(|v| format!("{v}%")).unwrap_or_else(|| "-".into()));
        draw_meter(c, ctx.tm, Rect::new(r.x() + 4, r.y() + 22, GRAPH_W as u32, 12), b.percent.map(|v| v as f32 / 100.0).unwrap_or(0.0));
        let line = match b.voltage_mv {
            Some(mv) => format!("{:.2} V  {}", mv as f32 / 1000.0, b.status),
            None => "no battery".into(),
        };
        text_l(c, ctx, r.x() + 4, r.y() + 34, &chrome::fit_text(ctx.fs, &line, cw - 8), p.faint);
        let line = match (b.time_to_empty_min, b.time_to_full_min) {
            (_, Some(m)) if b.status == "Charging" => format!("{} {}", i18n::tr("To full"), fmt_minutes(m)),
            (Some(m), _) => format!("{} {}", i18n::tr("Left"), fmt_minutes(m)),
            _ => String::new(),
        };
        text_l(c, ctx, r.x() + 4, r.y() + 47, &chrome::fit_text(ctx.fs, &line, cw - 8), p.faint);
    }

    fn draw_cpu(&self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let t = &self.cur;
        let y0 = BODY_Y;
        ui::fill_rect(c, Rect::new(0, y0, grid::SCREEN_W as u32, ROW as u32), p.alt);
        let load = format!("{:.2}  {:.2}  {:.2}", t.load[0], t.load[1], t.load[2]);
        text_l(c, ctx, X0, y0, "Load", p.faint);
        text_l(c, ctx, X0 + 36, y0, &load, p.ink);
        let freq = match (t.freq_mhz, t.freq_max_mhz) {
            (Some(f), Some(m)) => format!("{f} / {m} MHz"),
            (Some(f), None) => format!("{f} MHz"),
            _ => "- MHz".into(),
        };
        text_r(c, ctx, RIGHT, y0, &freq, p.ink);
        rule(c, 0, y0, grid::SCREEN_W, p.ink);

        // The four cores two to a row: name, meter, percentage.
        let cw = (RIGHT - X0 - grid::GUTTER * 2) / 2;
        for i in 0..4usize {
            let y = y0 + ROW * (1 + i as i32 / 2);
            let x = X0 + (i as i32 % 2) * (cw + grid::GUTTER * 2);
            let name = format!("CPU{i}");
            let v = self.cores.get(i).copied();
            text_l(c, ctx, x, y, &name, if v.is_some() { p.ink } else { p.faint });
            draw_meter(c, ctx.tm, Rect::new(x + 36, y + 4, (cw - 36 - 34) as u32, 8), v.unwrap_or(0.0));
            text_r(c, ctx, x + cw, y, &v.map(fmt_pct).unwrap_or_else(|| "-".into()), p.ink);
            rule(c, x, y, cw, p.line);
        }
        let gy = y0 + ROW * 3;
        // The graph runs down to the bottom line; the divider too.
        graph(c, p, Rect::new(X0, gy + 2, GRAPH_W as u32, (BOTTOM + 1 - gy - 2) as u32), &[(&self.cpu, Style::Area)], 0.0, 1.0);
        ui::line(c, 152, gy, 152, BOTTOM, p.line);
        let (kx, kw) = (156, RIGHT - 156);
        let f = |v: Option<f32>| v.map(fmt_pct).unwrap_or_else(|| "-".into());
        kv(c, ctx, p, kx, kw, gy, "Now", &f(self.cpu_now));
        kv(c, ctx, p, kx, kw, gy + ROW, "Peak", &f(self.cpu.max()));
        kv(c, ctx, p, kx, kw, gy + 2 * ROW, "Average", &f(self.cpu.avg()));
        kv(c, ctx, p, kx, kw, gy + 3 * ROW, "Low", &f(self.cpu.min()));
        let cores = if self.cores.is_empty() { "-".to_string() } else { self.cores.len().to_string() };
        kv(c, ctx, p, kx, kw, gy + 4 * ROW, "Cores", &cores);
    }

    fn draw_memory(&self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let m = &self.cur.mem;
        let total = m.total_kb.max(1) as f32;
        let rows: [(&str, u64, u64); 4] = [("Used", m.used_kb(), m.total_kb), ("Cache", m.cache_kb(), m.total_kb), ("Free", m.free_only_kb(), m.total_kb), ("Swap", m.swap_used_kb(), m.swap_total_kb)];
        // The meters start after the widest label (Japanese labels are longer).
        let mx = (X0 + rows.iter().map(|r| ctx.fs.measure_text(&i18n::tr_s(r.0))).max().unwrap_or(0) + 6).max(48);
        for (i, (name, v, of)) in rows.iter().enumerate() {
            let y = BODY_Y + ROW * i as i32;
            text_l(c, ctx, X0, y, name, p.ink);
            let frac = if *of == 0 { 0.0 } else { *v as f32 / *of as f32 };
            draw_meter(c, ctx.tm, Rect::new(mx, y + 4, (200 - mx) as u32, 8), frac);
            // Swap shows its size too (the rows below have no room for a "Swap size" line).
            let val = match (*of, *name) {
                (0, _) => "-".to_string(),
                (_, "Swap") => format!("{} / {}", fmt_kb(*v), fmt_kb(*of)),
                _ => format!("{}  {}", fmt_kb(*v), fmt_pct(frac)),
            };
            text_r(c, ctx, RIGHT, y, &val, p.ink);
            rule(c, X0, y, RIGHT - X0, p.line);
        }
        let gy = BODY_Y + ROW * 4;
        graph(c, p, Rect::new(X0, gy + 2, GRAPH_W as u32, (BOTTOM + 1 - gy - 2) as u32), &[(&self.mem, Style::Area)], 0.0, 1.0);
        ui::line(c, 152, gy, 152, BOTTOM, p.line);
        let (kx, kw) = (156, RIGHT - 156);
        let peak = self.mem.max().map(|f| fmt_kb((f * total) as u64)).unwrap_or_else(|| "-".into());
        kv(c, ctx, p, kx, kw, gy, "Total", &fmt_kb(m.total_kb));
        kv(c, ctx, p, kx, kw, gy + ROW, "Available", &fmt_kb(m.avail_kb));
        kv(c, ctx, p, kx, kw, gy + 2 * ROW, "Peak used", &peak);
        kv(c, ctx, p, kx, kw, gy + 3 * ROW, "Buffers", &fmt_kb(m.buffers_kb));
    }

    fn draw_power(&self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let t = &self.cur;
        let b = &t.power;
        let lw = 144;
        // Left: SoC temperature history.
        kv(c, ctx, p, X0, lw, BODY_Y, "SoC", &fmt_temp(t.temp_c));
        let (lo, hi) = match (self.temp.min(), self.temp.max()) {
            (Some(a), Some(b)) => auto_range(a, b, 10.0),
            _ => (30.0, 80.0),
        };
        // Rows 1..5 (y 52..132): the graph, 2 px clear of the rules around it.
        let gh = 5 * ROW - 4 + SLACK;
        graph(c, p, Rect::new(X0, BODY_Y + ROW + 2, GRAPH_W as u32, gh as u32), &[(&self.temp, Style::Area)], lo, hi);
        text_l(c, ctx, X0 + 4, BODY_Y + ROW + 2, &format!("{hi:.0}"), p.faint);
        text_l(c, ctx, X0 + 4, BODY_Y + ROW + 2 + gh - ROW, &format!("{lo:.0}"), p.faint);
        kv(c, ctx, p, X0, lw, BODY_Y + 6 * ROW + SLACK, "Peak", &fmt_temp(self.temp.max()));
        let thr = t.throttled.map(telemetry::throttle_text).unwrap_or_else(|| "n/a".into());
        let thr = thr.split(", ").map(i18n::tr_s).collect::<Vec<_>>().join(", ");
        kv(c, ctx, p, X0, lw, BODY_Y + 7 * ROW + SLACK, "Throttle", &thr);
        ui::line(c, 152, BODY_Y, 152, BOTTOM, p.line);

        // Right: battery level with its gauge in the same row, between the label and the value.
        let (kx, kw) = (156, RIGHT - 156);
        let pct = b.percent.map(|v| format!("{v}%")).unwrap_or_else(|| "-".into());
        kv(c, ctx, p, kx, kw, BODY_Y, "Battery", &pct);
        let mx = kx + ctx.fs.measure_text(i18n::tr("Battery")) + 6;
        let mr = kx + kw - ctx.fs.measure_text("100%") - 6;
        draw_meter(c, ctx.tm, Rect::new(mx, BODY_Y + 4, (mr - mx).max(8) as u32, 8), b.percent.map(|v| v as f32 / 100.0).unwrap_or(0.0));
        let volts = b.voltage_mv.map(|mv| mv as f32 / 1000.0);
        kv(c, ctx, p, kx, kw, BODY_Y + ROW, "Voltage", &volts.map(|v| format!("{v:.2} V")).unwrap_or_else(|| "-".into()));
        kv(c, ctx, p, kx, kw, BODY_Y + 2 * ROW, "Current", &b.current_ma.map(|a| format!("{a:+} mA")).unwrap_or_else(|| "-".into()));
        kv(c, ctx, p, kx, kw, BODY_Y + 3 * ROW, "State", if b.status.is_empty() { "-" } else { &b.status });
        let mins = |v: Option<u32>| v.map(fmt_minutes).unwrap_or_else(|| "-".into());
        let charging = b.status == "Charging";
        kv(c, ctx, p, kx, kw, BODY_Y + 4 * ROW, "Left", &if charging { "-".into() } else { mins(b.time_to_empty_min) });
        kv(c, ctx, p, kx, kw, BODY_Y + 5 * ROW, "To full", &if charging { mins(b.time_to_full_min) } else { "-".into() });
        let watts = match (volts, b.current_ma) {
            (Some(v), Some(a)) => format!("{:.2} W", v * a.unsigned_abs() as f32 / 1000.0),
            _ => "-".into(),
        };
        kv(c, ctx, p, kx, kw, BODY_Y + 6 * ROW, "Power", &watts);
        kv(c, ctx, p, kx, kw, BODY_Y + 7 * ROW, "Cell temp", &fmt_temp(b.temp_c));
    }

    fn draw_net(&self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        if self.net.is_empty() {
            text_l(c, ctx, X0, BODY_Y, "No network interfaces.", p.faint);
            return;
        }
        for (k, n) in self.net.iter().skip(self.net_scroll).take(NET_VISIBLE).enumerate() {
            let y0 = BODY_Y + k as i32 * NET_BLOCK;
            let left = format!("{}  {}", n.name, if n.up { i18n::tr("up") } else { i18n::tr("down") });
            let right = format!("RX {}  TX {}", fmt_rate(n.rx_rate), fmt_rate(n.tx_rate));
            band(c, ctx, p, y0, &left, &right);
            let max = nice_ceil(n.rx.iter().chain(n.tx.iter()).fold(1024.0f32, f32::max));
            // The block at the foot of the page gives its graph the rows left under it (as the CPU
            // and Memory graphs do), so the graph's bottom line is on y 168.
            let foot = y0 + NET_BLOCK == BODY_Y + BODY_ROWS * ROW;
            let gh = if foot { BOTTOM + 1 - (y0 + ROW + 2) } else { NET_BLOCK - ROW - 4 };
            let g = Rect::new(X0, y0 + ROW + 2, GRAPH_W as u32, gh as u32);
            graph(c, p, g, &[(&n.tx, Style::Half), (&n.rx, Style::Solid)], 0.0, max);
            // The scale (full height of the graph) in its top-left corner, as on the Power page.
            text_l(c, ctx, X0 + 4, g.y(), &fmt_rate(max as f64), p.faint);
            // The last block's divider runs on to the bottom line.
            let last = k + 1 == NET_VISIBLE.min(self.net.len() - self.net_scroll);
            ui::line(c, 152, y0 + ROW, 152, if last { BOTTOM } else { y0 + NET_BLOCK - 1 }, p.line);
            let (kx, kw) = (156, RIGHT - 156);
            kv(c, ctx, p, kx, kw, y0 + ROW, "RX total", &fmt_bytes(n.rx_total));
            kv(c, ctx, p, kx, kw, y0 + 2 * ROW, "TX total", &fmt_bytes(n.tx_total));
            let peak = n.rx.max().unwrap_or(0.0).max(n.tx.max().unwrap_or(0.0));
            kv(c, ctx, p, kx, kw, y0 + 3 * ROW, "Peak", &fmt_rate(peak as f64));
        }
        // 1 px under the tab strip's rule, down to the bottom line.
        let sb = ScrollBar::new(Rect::new(grid::SCROLL_X, BODY_Y + 1, grid::SCROLL_W as u32, (BOTTOM - BODY_Y) as u32), self.net.len(), NET_VISIBLE, self.net_scroll);
        if sb.needed() {
            sb.draw(c, ctx.tm, None);
        }
    }

    fn draw_disk(&self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let t = &self.cur;
        let used = t.disk_total_mb.saturating_sub(t.disk_free_mb);
        let frac = if t.disk_total_mb == 0 { 0.0 } else { used as f32 / t.disk_total_mb as f32 };
        band(c, ctx, p, BODY_Y, &format!("Root  /  {}", fmt_mb(t.disk_total_mb)), &format!("{} {}", fmt_pct(frac), i18n::tr("used")));
        // The meter row is SLACK taller (and its meter with it), so the last rule is on y 168.
        draw_meter(c, ctx.tm, Rect::new(X0, BODY_Y + ROW + 3, (RIGHT - X0) as u32, (10 + SLACK) as u32), frac);
        let w = RIGHT - X0;
        let y = BODY_Y + SLACK;
        kv(c, ctx, p, X0, w, y + 2 * ROW, "Used", &fmt_mb(used));
        kv(c, ctx, p, X0, w, y + 3 * ROW, "Free", &fmt_mb(t.disk_free_mb));
        kv(c, ctx, p, X0, w, y + 4 * ROW, "Uptime", &fmt_uptime(t.uptime_s));
        let (kernel, model) = self.info.as_ref().map(|i| (i.kernel.clone(), i.model.clone())).unwrap_or_default();
        kv(c, ctx, p, X0, w, y + 5 * ROW, "Kernel", &kernel);
        let os = self.info.as_ref().map(|i| i.os.clone()).unwrap_or_default();
        kv(c, ctx, p, X0, w, y + 6 * ROW, "OS", &os);
        kv(c, ctx, p, X0, w, y + 7 * ROW, "Board", &model);
    }
}

impl LauncherApp for Monitor {
    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        let mut v = vec![
            k("Tab / Shift+Tab", "Next / previous tab"),
            k("Left / Right", "Next / previous tab"),
            k("1 - 6", "Jump to a tab"),
            k("Space / P", "Pause / resume"),
        ];
        if self.page == P_NET {
            v.push(k("Up / Down", "Scroll"));
        }
        v
    }

    fn title(&self) -> String {
        crate::launcher::i18n::app_name("monitor")
    }

    fn menu(&self) -> Vec<MenuItem> {
        vec![
            MenuItem::new("pause", if self.paused { "Resume" } else { "Pause" }),
            MenuItem::new("clear", "Clear history"),
            MenuItem::new("next", "Next page"),
            MenuItem::new("prev", "Previous page"),
        ]
    }

    fn menu_action(&mut self, id: &str, _ctx: &Ctx) -> AppAction {
        match id {
            "pause" => self.paused = !self.paused,
            "clear" => self.clear(),
            "next" => self.step_page(1),
            "prev" => self.step_page(-1),
            _ => {}
        }
        self.dirty = true;
        AppAction::None
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, _ctx: &Ctx) -> AppAction {
        let back = mods.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
        match arrow_alias(key) {
            Keycode::Left => self.step_page(-1),
            Keycode::Right => self.step_page(1),
            Keycode::Tab => self.step_page(if back { -1 } else { 1 }),
            Keycode::Up if self.page == P_NET => self.scroll_net(-1),
            Keycode::Down if self.page == P_NET => self.scroll_net(1),
            Keycode::P | Keycode::Space => {
                self.paused = !self.paused;
                self.dirty = true;
            }
            // Short Esc steps back to the first page; the host never quits on it.
            Keycode::Escape => self.set_page(P_OVERVIEW),
            Keycode::Num1 => self.set_page(0),
            Keycode::Num2 => self.set_page(1),
            Keycode::Num3 => self.set_page(2),
            Keycode::Num4 => self.set_page(3),
            Keycode::Num5 => self.set_page(4),
            Keycode::Num6 => self.set_page(5),
            _ => {}
        }
        AppAction::None
    }

    fn wants_tab(&self) -> bool {
        true
    }

    fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> AppAction {
        let tabs = self.tabs(ctx);
        match ev {
            PointerEvent::Down { x, y, button: 0, .. } => {
                if let Some(i) = tabs.hit(x, y) {
                    self.set_page(i);
                }
            }
            PointerEvent::Wheel { dy, .. } if self.page == P_NET => self.scroll_net(if dy > 0 { -1 } else { 1 }),
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, ctx: &Ctx) -> bool {
        if self.info.is_none() {
            self.info = Some(ctx.sys.sysinfo());
        }
        let now = Instant::now();
        let due = self.last.map(|t| now.saturating_duration_since(t) >= SAMPLE_EVERY).unwrap_or(true);
        let mut redraw = std::mem::take(&mut self.dirty);
        if due && !self.paused {
            self.sample(ctx.sys, now);
            redraw = true;
        }
        redraw
    }

    fn draw(&mut self, c: &mut Canvas<Window>, _assets: &mut AssetStore, ctx: &Ctx) {
        let tm = ctx.tm;
        let p = Pal {
            ink: tm.get_color("foreground"),
            paper: tm.get_color("widget_face_active"),
            faint: widgets::secondary_ink(tm),
            alt: tm.get_color("surface_alt"),
            line: tm.get_color("line"),
            bg: tm.get_color("home_bg"),
        };
        ui::fill_rect(c, chrome::app_rect(), p.bg);
        self.tabs(ctx).draw_strip(c, ctx.fs, tm);
        match self.page {
            P_OVERVIEW => self.draw_overview(c, ctx, &p),
            P_CPU => self.draw_cpu(c, ctx, &p),
            P_MEM => self.draw_memory(c, ctx, &p),
            P_POWER => self.draw_power(c, ctx, &p),
            P_NET => self.draw_net(c, ctx, &p),
            P_DISK => self.draw_disk(c, ctx, &p),
            _ => {}
        }
        if self.paused {
            // Over the right end of the last row, ending on y 168.
            let r = Rect::new(grid::SCREEN_W - 56 - grid::MARGIN, BOTTOM + 1 - ROW, 56, ROW as u32);
            ui::fill_rect(c, r, p.ink);
            let t = i18n::tr("Paused");
            let w = ctx.fs.measure_text(t);
            ctx.fs.draw_text(c, r.x() + (56 - w) / 2, widgets::text_origin(r.y(), ROW), t, p.paper);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::mock::MockBackend;

    #[test]
    fn ring_keeps_the_newest_samples() {
        let mut r = Ring::new(3);
        assert!(r.is_empty() && r.max().is_none() && r.avg().is_none());
        for v in [1.0, 2.0, 3.0, 4.0] {
            r.push(v);
        }
        assert_eq!(r.iter().collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
        assert_eq!((r.last(), r.max(), r.min()), (Some(4.0), Some(4.0), Some(2.0)));
        assert!((r.avg().unwrap() - 3.0).abs() < 1e-6);
        r.clear();
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn cpu_percent_per_core_from_deltas() {
        let prev = [CpuTimes { busy: 100, total: 1000 }, CpuTimes { busy: 50, total: 500 }, CpuTimes { busy: 50, total: 500 }];
        let cur = [CpuTimes { busy: 150, total: 1100 }, CpuTimes { busy: 100, total: 600 }, CpuTimes { busy: 50, total: 600 }];
        let p = cpu_percents(&prev, &cur);
        assert!((p[0].unwrap() - 0.5).abs() < 1e-6);
        assert!((p[1].unwrap() - 0.5).abs() < 1e-6);
        assert_eq!(p[2], Some(0.0));
        // a core that appeared since the last snapshot has no delta yet
        assert_eq!(cpu_percents(&prev[..1], &cur)[2], None);
    }

    #[test]
    fn net_rate_survives_counter_resets() {
        assert_eq!(net_rate(1000, 3048, 2.0), 1024.0);
        assert_eq!(net_rate(5000, 10, 1.0), 0.0);
        assert_eq!(net_rate(0, 10, 0.0), 0.0);
    }

    #[test]
    fn graph_scales() {
        assert_eq!(auto_range(45.0, 47.0, 10.0), (41.0, 51.0));
        let (l, h) = auto_range(30.0, 70.0, 10.0);
        assert_eq!((l, h), (28.0, 72.0));
        assert_eq!(nice_ceil(0.0), 1.0);
        assert_eq!(nice_ceil(1024.0), 2000.0);
        assert_eq!(nice_ceil(3000.0), 5000.0);
        assert_eq!(nice_ceil(5000.0), 5000.0);
        assert_eq!(nice_ceil(7e6), 1e7);
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1536), "1.5 KB");
        assert_eq!(fmt_bytes(5 * 1048576), "5.0 MB");
        assert_eq!(fmt_bytes(3 * 1073741824), "3.00 GB");
        assert_eq!(fmt_rate(0.0), "0 B/s");
        assert_eq!(fmt_rate(20.0 * 1024.0), "20.0 KB/s");
        assert_eq!(fmt_rate(3.0 * 1048576.0), "3.0 MB/s");
        assert_eq!(fmt_kb(143_000), "139 MB");
        assert_eq!(fmt_mb(14_800), "14.5 GB");
        assert_eq!(fmt_mb(900), "900 MB");
        assert_eq!(fmt_pct(0.376), "38%");
        assert_eq!(fmt_pct(1.7), "100%");
        assert_eq!(fmt_minutes(45), "45 m");
        assert_eq!(fmt_minutes(192), "3 h 12 m");
        assert_eq!(fmt_uptime(59), "0 m 59 s");
        assert_eq!(fmt_uptime(3725), "1 h 02 m");
        assert_eq!(fmt_uptime(2 * 86400 + 3 * 3600), "2 d 3 h");
        assert_eq!(fmt_temp(Some(46.24)), "46.2 C");
        assert_eq!(fmt_temp(None), "-");
    }

    #[test]
    fn sampling_builds_histories_from_the_second_snapshot() {
        let sys = MockBackend::new();
        let mut m = Monitor::new();
        let t0 = Instant::now();
        m.sample(&sys, t0);
        assert_eq!(m.cpu.len(), 0, "no rate from a single snapshot");
        assert_eq!((m.mem.len(), m.temp.len()), (1, 1));
        std::thread::sleep(Duration::from_millis(5));
        m.sample(&sys, t0 + Duration::from_secs(1));
        assert_eq!(m.cpu.len(), 1);
        assert_eq!(m.cores.len(), 4);
        assert_eq!(m.net.len(), 2);
        assert_eq!(m.net[0].rx.len(), 1);
        for i in 0..100 {
            std::thread::sleep(Duration::from_millis(2));
            m.sample(&sys, t0 + Duration::from_secs(2 + i));
        }
        assert_eq!(m.cpu.len(), HIST, "history is bounded");
        m.clear();
        assert_eq!((m.cpu.len(), m.mem.len(), m.net[0].rx.len()), (0, 0, 0));
    }

    #[test]
    fn pages_wrap_and_net_scroll_is_clamped() {
        let mut m = Monitor::new();
        m.step_page(-1);
        assert_eq!(m.page, TABS.len() - 1);
        m.step_page(1);
        assert_eq!(m.page, 0);
        m.scroll_net(5);
        assert_eq!(m.net_scroll, 0, "no interfaces, nothing to scroll");
    }

    #[test]
    fn help_rows_are_filled_in() {
        let mut m = Monitor::new();
        crate::launcher::help::check_app_rows("Monitor", &m.help());
        m.set_page(P_NET);
        let net = m.help();
        crate::launcher::help::check_app_rows("Monitor Net", &net);
        assert!(net.iter().any(|(k, _)| k == "Up / Down"), "the Net page scrolls");
    }
}
