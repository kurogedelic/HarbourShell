//! Lock screen, in one of four styles (Settings > Lock Screen > Style, `harbour_lock_style`):
//! a wall clock (an analog dial in a dithered bezel, hanging on the desktop), World Clock (the
//! map with the night side), a full-screen ASCII-art clock and a big 7-segment clock. The date
//! and the battery are optional (`harbour_lock_date`, `harbour_lock_battery`). The menu strip
//! stays as everywhere; scrolling (wheel or Up / Down) fills the unlock chip.
//!
//! The old dot-matrix HH:MM beside the analog clock is gone as a style of its own: the 7-segment
//! clock is the big digital face now, and its 5x7 glyphs live on as the ASCII clock's font.

use chrono::{DateTime, Datelike, Local, Timelike};
use crate::gfx::Color;
use crate::gfx::Rect;
use crate::gfx::Canvas;

use crate::assets::AssetStore;
use crate::font::FontSystem;
use crate::grid;
use crate::launcher::i18n::{self, Lang};
use crate::launcher::sevenseg as seg;
use crate::launcher::widgets;
use crate::launcher::worldclock::WorldClock;
use crate::system::config::Config;
use crate::theme::ThemeManager;
use crate::ui;

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const DAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const DAYS_JA: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// The four lock screen styles, in the order of Settings' Style pop-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Wall,
    World,
    Ascii,
    Seg,
}

impl Style {
    pub fn from_index(i: i64) -> Self {
        match i {
            1 => Style::World,
            2 => Style::Ascii,
            3 => Style::Seg,
            _ => Style::Wall,
        }
    }
}

/// What the lock screen shows, read from the config each frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Opts {
    pub style: Style,
    pub date: bool,
    /// `Some((percent, charging))` when the battery is shown.
    pub battery: Option<(Option<u8>, bool)>,
    /// A newer Harbour Shell version that is already known (never fetched from here); None when there is none
    /// or when `harbour_lock_update` / `harbour_update_check` is off.
    pub update: Option<String>,
}

impl Opts {
    /// `newer` is the already-known newer version (`update::available()`); nothing is fetched here.
    pub fn from_config(cfg: &Config, battery: Option<u8>, charging: bool, newer: Option<String>) -> Self {
        Opts {
            style: Style::from_index(cfg.get_int("harbour_lock_style", 0)),
            date: cfg.get_int("harbour_lock_date", 1) == 1,
            battery: (cfg.get_int("harbour_lock_battery", 1) == 1).then_some((battery, charging)),
            update: if cfg.get_int("harbour_lock_update", 1) == 1 && cfg.get_int("harbour_update_check", 1) != 0 { newer } else { None },
        }
    }
}

/// Seconds of idle time after which the screen locks itself (`harbour_lock_auto`; 0 = never).
pub fn auto_lock_secs(cfg: &Config) -> u64 {
    cfg.get_int("harbour_lock_auto", 0).max(0) as u64
}

/// Angles (radians, clockwise from 12 o'clock) of the hour, minute and second hands.
pub fn hand_angles(h: u32, m: u32, s: u32) -> (f32, f32, f32) {
    let tau = std::f32::consts::TAU;
    let sec = s as f32 / 60.0;
    let min = (m as f32 + sec) / 60.0;
    let hour = ((h % 12) as f32 + min) / 12.0;
    (hour * tau, min * tau, sec * tau)
}

/// End point of a hand of `len` pixels from the centre.
pub fn hand_end(cx: i32, cy: i32, len: f32, angle: f32) -> (i32, i32) {
    (cx + (angle.sin() * len).round() as i32, cy - (angle.cos() * len).round() as i32)
}

/// A line `thick` pixels wide (drawn as parallel lines).
fn thick_line(c: &mut Canvas, a: (i32, i32), b: (i32, i32), thick: i32, color: Color) {
    c.set_draw_color(color);
    let horizontal = (b.0 - a.0).abs() > (b.1 - a.1).abs();
    for o in -(thick / 2)..=((thick - 1) / 2) {
        let (dx, dy) = if horizontal { (0, o) } else { (o, 0) };
        let _ = c.draw_line((a.0 + dx, a.1 + dy), (b.0 + dx, b.1 + dy));
    }
}

pub fn draw_face(c: &mut Canvas, cx: i32, cy: i32, r: i32, hms: (u32, u32, u32), ink: Color, muted: Color, paper: Color) {
    // Dial: double ring, hour ticks (long at 12/3/6/9) and minute dots.
    // Filled disc (not a square) so the desktop shows around the dial.
    c.set_draw_color(paper);
    for dy in -r..=r {
        let half = ((r * r - dy * dy) as f32).sqrt().round() as i32;
        let _ = c.fill_rect(Rect::new(cx - half, cy + dy, (2 * half + 1) as u32, 1));
    }
    ui::draw_circle(c, (cx, cy), r, ink);
    ui::draw_circle(c, (cx, cy), r - 1, ink);
    let small = r < 20;
    if small {
        // Tiny dial: just the four quarter marks.
        for q in 0..4 {
            let a = q as f32 * std::f32::consts::FRAC_PI_2;
            thick_line(c, hand_end(cx, cy, r as f32 - 4.0, a), hand_end(cx, cy, r as f32 - 2.0, a), 2, ink);
        }
    } else {
        for i in 0..60 {
            let a = i as f32 * std::f32::consts::TAU / 60.0;
            if i % 5 == 0 {
                let long = i % 15 == 0;
                let inner = if long { r as f32 - 11.0 } else { r as f32 - 7.0 };
                thick_line(c, hand_end(cx, cy, inner, a), hand_end(cx, cy, r as f32 - 3.0, a), if long { 3 } else { 2 }, ink);
            } else {
                let p = hand_end(cx, cy, r as f32 - 4.0, a);
                c.set_draw_color(muted);
                let _ = c.draw_point(p);
            }
        }
    }
    let (h, m, s) = hand_angles(hms.0, hms.1, hms.2);
    let rf = r as f32;
    if small {
        thick_line(c, (cx, cy), hand_end(cx, cy, rf * 0.50, h), 2, ink);
        thick_line(c, (cx, cy), hand_end(cx, cy, rf * 0.80, m), 1, ink);
        ui::fill_rect(c, Rect::new(cx - 1, cy - 1, 3, 3), ink);
        let _ = s;
        return;
    }
    thick_line(c, (cx, cy), hand_end(cx, cy, rf * 0.50, h), 4, ink);
    thick_line(c, (cx, cy), hand_end(cx, cy, rf * 0.78, m), 3, ink);
    // Second hand: thin, with a short tail.
    c.set_draw_color(ink);
    let tip = hand_end(cx, cy, rf * 0.86, s);
    let tail = hand_end(cx, cy, -rf * 0.16, s);
    let _ = c.draw_line(tail, tip);
    ui::fill_rect(c, Rect::new(cx - 3, cy - 3, 7, 7), ink);
    ui::fill_rect(c, Rect::new(cx - 1, cy - 1, 3, 3), paper);
}

/// Padlock, 9x10.
fn draw_padlock(c: &mut Canvas, x: i32, y: i32, color: Color) {
    const ART: [&str; 10] = [
        "  XXXXX  ",
        " XX   XX ",
        " X     X ",
        " X     X ",
        "XXXXXXXXX",
        "XXXXXXXXX",
        "XXXX XXXX",
        "XXXX XXXX",
        "XXXXXXXXX",
        "XXXXXXXXX",
    ];
    c.set_draw_color(color);
    for (r, line) in ART.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            if ch == b'X' {
                let _ = c.draw_point((x + col as i32, y + r as i32));
            }
        }
    }
}

/// 5x7 dot-matrix digits.
const DOTS: [[&str; 7]; 10] = [
    ["01110", "10001", "10011", "10101", "11001", "10001", "01110"],
    ["00100", "01100", "00100", "00100", "00100", "00100", "01110"],
    ["01110", "10001", "00001", "00010", "00100", "01000", "11111"],
    ["11110", "00001", "00001", "01110", "00001", "00001", "11110"],
    ["00010", "00110", "01010", "10010", "11111", "00010", "00010"],
    ["11111", "10000", "11110", "00001", "00001", "10001", "01110"],
    ["00110", "01000", "10000", "11110", "10001", "10001", "01110"],
    ["11111", "00001", "00010", "00100", "01000", "01000", "01000"],
    ["01110", "10001", "10001", "01110", "10001", "10001", "01110"],
    ["01110", "10001", "10001", "01111", "00001", "00010", "01100"],
];

/// Width of one 5x7 glyph of [`DOTS`], in cells.
const DOT_W: usize = 5;

/// The ASCII clock: `HH:MM` as rows of text, each lit dot of the 5x7 glyphs two `#`, each dark
/// one two `.` (so the dots come out about square in a 6x12 cell), digits two cells apart and
/// the colon (`##` on rows 2 and 4 while `colon`) in a 6-cell slot. 50 columns, 7 rows.
pub fn ascii_clock(h: u32, m: u32, colon: bool) -> Vec<String> {
    let digits = [(h / 10) as usize % 10, (h % 10) as usize, (m / 10) as usize % 10, (m % 10) as usize];
    (0..7)
        .map(|row| {
            let mut line = String::new();
            for (i, d) in digits.iter().enumerate() {
                for ch in DOTS[*d][row].bytes().take(DOT_W) {
                    line.push_str(if ch == b'1' { "##" } else { ".." });
                }
                match i {
                    0 | 2 => line.push_str("  "),
                    1 => line.push_str(if colon && (row == 2 || row == 4) { "  ##  " } else { "      " }),
                    _ => {}
                }
            }
            line
        })
        .collect()
}

/// The lock screen's two tones: the theme's white (`icon_white`; Invert themes lift it to a dark
/// tone, there the plain light tone is used, as Radio does) and the dark ink, plus a faint tone
/// between them for unlit segments and dots.
struct Inks {
    face: Color,
    land: Color,
    faint: Color,
}

fn inks(tm: &ThemeManager) -> Inks {
    let t = widgets::tones(tm);
    let lum = |c: Color| c.r as u32 * 3 + c.g as u32 * 6 + c.b as u32;
    let white = tm.get_color("icon_white");
    let face = if lum(white) * 2 >= lum(t.light) + lum(t.dark) { white } else { t.light };
    let mix = |a: u8, b: u8| ((a as u32 * 4 + b as u32) / 5) as u8;
    Inks { face, land: t.dark, faint: Color::RGB(mix(face.r, t.dark.r), mix(face.g, t.dark.g), mix(face.b, t.dark.b)) }
}

/// Chip height: an ink plate with text in the face tone (no frame, so no line to touch).
const CHIP_H: i32 = 14;

/// An ink chip with `text` at (x, y); the display face when `display`. Returns its width.
fn chip(c: &mut Canvas, fs: &FontSystem, ink: &Inks, x: i32, y: i32, text: &str, display: bool) -> i32 {
    let w = if display { fs.measure_display_text(text) } else { fs.measure_text(text) } + 8;
    ui::fill_rect(c, Rect::new(x, y, w as u32, CHIP_H as u32), ink.land);
    if display {
        fs.draw_display_text(c, x + 4, y, text, ink.face);
    } else {
        fs.draw_text(c, x + 4, widgets::text_origin(y, CHIP_H), text, ink.face);
    }
    w
}

/// Width of the battery chip.
fn battery_w(fs: &FontSystem, pct: Option<u8>) -> i32 {
    4 + 12 + pct.map(|p| 4 + fs.measure_text(&format!("{p}%"))).unwrap_or(0) + 4
}

/// The battery chip: the menu strip's battery icon and the percentage, on an ink plate.
fn battery_chip(c: &mut Canvas, assets: &mut AssetStore, fs: &FontSystem, ink: &Inks, x: i32, y: i32, pct: Option<u8>, charging: bool) {
    let w = battery_w(fs, pct);
    ui::fill_rect(c, Rect::new(x, y, w as u32, CHIP_H as u32), ink.land);
    crate::chrome::draw_battery(c, assets, x + 4, y + 1, pct, charging, ink.face, ink.faint);
    if let Some(p) = pct {
        fs.draw_text(c, x + 20, widgets::text_origin(y, CHIP_H), &format!("{p}%"), ink.face);
    }
}

/// The update notice: "Update 0.3.1 available" (`short`: "Update 0.3.1"), "アップデート 0.3.1 あり" (short: "更新 0.3.1").
fn update_text(version: &str, short: bool) -> String {
    match (i18n::get() == Lang::Ja, short) {
        (false, false) => format!("Update {version} available"),
        (false, true) => format!("Update {version}"),
        (true, false) => format!("アップデート {version} あり"),
        (true, true) => format!("更新 {version}"),
    }
}

/// The update chip at (x, y): the long wording when it fits in `room` px, else the short one (None if neither does).
fn update_chip(c: &mut Canvas, fs: &FontSystem, ink: &Inks, x: i32, y: i32, room: i32, version: &str) {
    for short in [false, true] {
        let t = update_text(version, short);
        if fs.measure_text(&t) + 8 <= room {
            chip(c, fs, ink, x, y, &t, false);
            return;
        }
    }
}

/// The weekday and the date in one line ("Thu 19 Nov 2026", "11月19日(木)").
fn date_line(now: DateTime<Local>) -> String {
    let wd = now.weekday().num_days_from_sunday() as usize;
    if i18n::get() == Lang::Ja {
        format!("{}年{}月{}日({})", now.year(), now.month(), now.day(), DAYS_JA[wd])
    } else {
        format!("{} {} {} {}", DAYS_SHORT[wd], now.day(), MONTHS[now.month0() as usize], now.year())
    }
}

/// The x where the unlock chip starts (it is right-aligned at RIGHT).
fn unlock_left(fs: &FontSystem) -> i32 {
    RIGHT - (4 + 9 + 5 + fs.measure_text(i18n::tr("Scroll up to unlock")) + 5)
}

/// The unlock chip, right-aligned at `right`: a padlock and the hint on an ink plate. As the user
/// scrolls, the plate turns over to the face tone from the left (the chip is its own gauge).
fn unlock_chip(c: &mut Canvas, fs: &FontSystem, ink: &Inks, right: i32, y: i32, unlock: f32) {
    let hint = i18n::tr("Scroll up to unlock");
    let w = 4 + 9 + 5 + fs.measure_text(hint) + 5;
    let r = Rect::new(right - w, y, w as u32, CHIP_H as u32);
    let content = |c: &mut Canvas, fg: Color| {
        draw_padlock(c, r.x() + 4, y + 2, fg);
        fs.draw_text(c, r.x() + 18, widgets::text_origin(y, CHIP_H), hint, fg);
    };
    ui::fill_rect(c, r, ink.land);
    content(c, ink.face);
    let done = (w as f32 * unlock.clamp(0.0, 1.0)).round() as u32;
    if done > 0 {
        let part = Rect::new(r.x(), y, done, CHIP_H as u32);
        c.set_clip_rect(part);
        ui::fill_rect(c, part, ink.face);
        content(c, ink.land);
        c.set_clip_rect(None);
    }
}

/// The bottom chip row (the unlock chip): its last row is y 168, the last one the screens use
/// (docs/DESIGN.md), so no strip of wall is left under it.
const ROW_B: i32 = grid::APP_FRAME_BOTTOM + 1 - CHIP_H;
/// The chip row above it, 1 px clear (the 7-segment screen's date and battery).
const ROW_A: i32 = ROW_B - CHIP_H - 1;
/// The World screen's first strip row, 3 px under the map's rule.
const WORLD_ROW_A: i32 = 131;
const RIGHT: i32 = 316;

/// Draw the lock screen below the menu strip. `world` keeps the lock map's night side (centred on
/// the device's own time zone). `unlock` is the scroll-to-unlock progress, 0.0..=1.0.
#[allow(clippy::too_many_arguments)]
pub fn draw(c: &mut Canvas, assets: &mut AssetStore, fs: &FontSystem, tm: &ThemeManager, world: &mut WorldClock, now: DateTime<Local>, unlock: f32, opts: &Opts) {
    let ink = inks(tm);
    match opts.style {
        Style::Wall => draw_wall(c, assets, fs, tm, &ink, now, unlock, opts),
        Style::World => draw_world(c, assets, fs, tm, &ink, world, now, unlock, opts),
        Style::Ascii => draw_ascii(c, fs, tm, &ink, now, unlock, opts),
        Style::Seg => draw_seg(c, assets, fs, &ink, now, unlock, opts),
    }
}

/// An analog clock hanging on the desktop "wall" from a nail, its bezel dithered, its shadow too;
/// the date and the battery on chips to its right.
#[allow(clippy::too_many_arguments)]
fn draw_wall(c: &mut Canvas, assets: &mut AssetStore, fs: &FontSystem, tm: &ThemeManager, ink: &Inks, now: DateTime<Local>, unlock: f32, opts: &Opts) {
    // With neither the date nor the battery beside it, the clock hangs in the middle of the wall.
    let cx = if opts.date || opts.battery.is_some() { 86 } else { 160 };
    // The nail sits at y 20 and the shadow ends at y 153, 1 px above the unlock chip (which the
    // centred clock reaches over).
    let (cy, r) = (92, 58);
    let disc = |c: &mut Canvas, ox: i32, oy: i32, rr: i32, f: &mut dyn FnMut(&mut Canvas, Rect)| {
        for dy in -rr..=rr {
            let half = ((rr * rr - dy * dy) as f32).sqrt().round() as i32;
            f(c, Rect::new(cx - half + ox, cy + dy + oy, (2 * half + 1) as u32, 1));
        }
    };
    // Shadow on the wall, then the bezel: the face tone with a dithered ink ring, framed.
    disc(c, 3, 3, r, &mut |c, row| crate::dither::fill(c, row, tm.get_color("widget_shadow"), crate::dither::Pattern::Half));
    disc(c, 0, 0, r, &mut |c, row| ui::fill_rect(c, row, ink.face));
    disc(c, 0, 0, r, &mut |c, row| crate::dither::fill(c, row, ink.land, crate::dither::Pattern::Half));
    ui::draw_circle(c, (cx, cy), r, ink.land);
    // The cord from the nail to the bezel, and the nail.
    let nail = (cx, cy - r - 14);
    for a in [-0.5f32, 0.5] {
        let end = hand_end(cx, cy, r as f32, a);
        thick_line(c, nail, end, 1, ink.land);
    }
    ui::fill_rect(c, Rect::new(nail.0 - 2, nail.1 - 2, 5, 5), ink.land);
    ui::fill_rect(c, Rect::new(nail.0 - 1, nail.1 - 1, 2, 2), ink.face);
    draw_face(c, cx, cy, r - 6, (now.hour(), now.minute(), now.second()), ink.land, ink.faint, ink.face);

    let x0 = 162;
    let mut y = 36;
    if opts.date {
        let wd = now.weekday().num_days_from_sunday() as usize;
        let ja = i18n::get() == Lang::Ja;
        let day = if ja { format!("{}曜日", DAYS_JA[wd]) } else { DAYS[wd].to_string() };
        chip(c, fs, ink, x0, y, &day, !ja);
        y += CHIP_H + 2;
        let date = if ja { format!("{}年{}月{}日", now.year(), now.month(), now.day()) } else { format!("{} {} {}", now.day(), MONTHS[now.month0() as usize], now.year()) };
        chip(c, fs, ink, x0, y, &date, false);
        y += CHIP_H + 8;
    }
    if let Some((pct, charging)) = opts.battery {
        battery_chip(c, assets, fs, ink, x0, y, pct, charging);
        y += CHIP_H + 2;
    }
    if let Some(v) = &opts.update {
        update_chip(c, fs, ink, x0, y, RIGHT - x0, v);
    }
    unlock_chip(c, fs, ink, RIGHT, ROW_B, unlock);
}

/// The World Clock map (night side, the device's own time zone in white), a rule, then the
/// local time in 7-segment digits on an outlined plate, the date, the battery and the unlock chip.
#[allow(clippy::too_many_arguments)]
fn draw_world(c: &mut Canvas, assets: &mut AssetStore, fs: &FontSystem, tm: &ThemeManager, ink: &Inks, world: &mut WorldClock, now: DateTime<Local>, unlock: f32, opts: &Opts) {
    let mh = world.draw_map(c, assets, tm, now, false);
    ui::fill_rect(c, Rect::new(0, 16 + mh, crate::chrome::SCREEN_W, 1), ink.land);
    // The clock plate runs from the first strip row down to y 168, the time in its middle.
    let plate = Rect::new(4, WORLD_ROW_A, 83, (grid::APP_FRAME_BOTTOM + 1 - WORLD_ROW_A) as u32);
    ui::fill_rect(c, plate, ink.face);
    ui::draw_rect(c, plate, ink.land);
    let y0 = plate.y() + (plate.height() as i32 - 20) / 2;
    let mut x = plate.x() + 5;
    for d in [now.hour() / 10, now.hour() % 10] {
        seg::draw_atlas_digit(c, assets, seg::BIG_KEY, 14, 20, d as usize, x, y0, ink.land);
        x += 17;
    }
    if now.second() % 2 == 0 {
        seg::draw_colon_upright(c, x + 1, y0, 20, 3, ink.land);
    }
    x += 8;
    for d in [now.minute() / 10, now.minute() % 10] {
        seg::draw_atlas_digit(c, assets, seg::BIG_KEY, 14, 20, d as usize, x, y0, ink.land);
        x += 17;
    }
    let left = plate.x() + plate.width() as i32 + 5;
    if opts.date {
        chip(c, fs, ink, left, WORLD_ROW_A, &date_line(now), false);
    }
    if let Some((pct, charging)) = opts.battery {
        battery_chip(c, assets, fs, ink, RIGHT - battery_w(fs, pct), WORLD_ROW_A, pct, charging);
    }
    if let Some(v) = &opts.update {
        // Left of the unlock chip, right of the clock plate.
        update_chip(c, fs, ink, left, ROW_B, unlock_left(fs) - 4 - left, v);
    }
    unlock_chip(c, fs, ink, RIGHT, ROW_B, unlock);
}

/// Text cell of the ASCII screen (the body face's half-width glyphs).
const CELL: (i32, i32) = (6, 12);
/// 53 x 12 cells fill x 1..319; the 12 rows (144 px) sit in the middle of the dark panel, y 20..164.
const ASCII_COLS: usize = 53;
/// The dark panel: under the menu strip down to y 168 (nothing on the screen's last row).
const ASCII_TOP: i32 = 16;
const ASCII_ROW0: i32 = ASCII_TOP + (grid::APP_FRAME_BOTTOM + 1 - ASCII_TOP - 12 * CELL.1) / 2;

/// The whole screen in text: a `+--+` box, `HH:MM` in `#` and `.` (rows 1-7), the date and the battery on
/// one line, and the unlock gauge `[####......]` with its hint.
fn draw_ascii(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, ink: &Inks, now: DateTime<Local>, unlock: f32, opts: &Opts) {
    let _ = tm;
    ui::fill_rect(c, Rect::new(0, ASCII_TOP, 320, (grid::APP_FRAME_BOTTOM + 1 - ASCII_TOP) as u32), ink.land);
    let at = |col: usize, row: usize| (1 + col as i32 * CELL.0, ASCII_ROW0 + row as i32 * CELL.1);
    // One character per cell (the body face's Latin advance is not a fixed 6 px), so the columns
    // line up; Japanese text (two cells a glyph) goes through as one run.
    let put = |c: &mut Canvas, col: usize, row: usize, t: &str, col_: Color| {
        if !t.is_ascii() {
            let (x, y) = at(col, row);
            fs.draw_text(c, x, y, t, col_);
            return;
        }
        for (i, ch) in t.chars().enumerate() {
            if ch != ' ' {
                let (x, y) = at(col + i, row);
                fs.draw_text(c, x + (CELL.0 - fs.measure_text(&ch.to_string())) / 2, y, &ch.to_string(), col_);
            }
        }
    };
    let cells = |t: &str| t.chars().map(|ch| if ch.is_ascii() { 1 } else { 2 }).sum::<usize>();
    let inner = ASCII_COLS - 2;
    let rule = format!("+{}+", "-".repeat(inner));
    put(c, 0, 0, &rule, ink.faint);
    put(c, 0, 11, &rule, ink.faint);
    for row in 1..11 {
        put(c, 0, row, "|", ink.faint);
        put(c, ASCII_COLS - 1, row, "|", ink.faint);
    }
    // The clock: lit dots in the face tone, dark ones faint (drawn as two passes of the same rows).
    let lines = ascii_clock(now.hour(), now.minute(), now.second() % 2 == 0);
    let col0 = 1 + (inner - lines[0].len()) / 2;
    for (i, l) in lines.iter().enumerate() {
        let lit: String = l.chars().map(|ch| if ch == '#' { '#' } else { ' ' }).collect();
        let dark: String = l.chars().map(|ch| if ch == '.' { '.' } else { ' ' }).collect();
        put(c, col0, 1 + i, &dark, ink.faint);
        put(c, col0, 1 + i, &lit, ink.face);
    }
    // Row 9: the date on the left, the battery on the right, inside the box.
    if opts.date {
        put(c, 3, 9, &date_line(now), ink.face);
    }
    if let Some((pct, charging)) = opts.battery {
        let t = format!("{} {}{}", i18n::tr("Battery"), pct.map(|p| format!("{p}%")).unwrap_or_else(|| "-".into()), if charging { "+" } else { "" });
        put(c, ASCII_COLS - 3 - cells(&t), 9, &t, ink.face);
    }
    // Row 8: the update notice, centred (the clock is rows 1-7).
    if let Some(v) = &opts.update {
        let t = update_text(v, false);
        put(c, 1 + inner.saturating_sub(cells(&t)) / 2, 8, &t, ink.faint);
    }
    // Row 10: the unlock gauge and its hint, centred.
    let n = 16;
    let k = (n as f32 * unlock.clamp(0.0, 1.0)).round() as usize;
    let gauge = format!("[{}{}]", "#".repeat(k), ".".repeat(n - k));
    let hint = i18n::tr("Scroll up to unlock");
    let cols = (n + 2) + 1 + cells(hint);
    let g0 = 1 + inner.saturating_sub(cols) / 2;
    put(c, g0, 10, &gauge, if unlock > 0.0 { ink.face } else { ink.faint });
    put(c, g0 + n + 3, 10, hint, if unlock > 0.0 { ink.face } else { ink.faint });
}

/// A big 7-segment HH:MM on a face-tone panel (unlit segments faintly visible, like an LCD), the
/// date and the battery on chips below it.
fn draw_seg(c: &mut Canvas, assets: &mut AssetStore, fs: &FontSystem, ink: &Inks, now: DateTime<Local>, unlock: f32, opts: &Opts) {
    // The panel ends 2 px above the chip row.
    let panel = Rect::new(4, 18, 312, (ROW_A - 2 - 18) as u32);
    ui::fill_rect(c, panel, ink.face);
    ui::draw_rect(c, panel, ink.land);
    let (w, h, t) = (48, 88, 8);
    // Four digits 60 px apart, a 28 px colon slot, the italic slant (h/4) on top: 266 px.
    let span = 4 * w + 2 * 12 + 28 + seg::shear(h, 0);
    let x0 = panel.x() + (panel.width() as i32 - span) / 2;
    let y0 = panel.y() + (panel.height() as i32 - h) / 2;
    let xs = [x0, x0 + w + 12, x0 + 2 * w + 12 + 28, x0 + 3 * w + 2 * 12 + 28];
    let ds = [now.hour() / 10, now.hour() % 10, now.minute() / 10, now.minute() % 10];
    for (x, d) in xs.iter().zip(ds) {
        seg::draw_digit(c, *x, y0, w, h, t, 8, ink.faint);
        seg::draw_digit(c, *x, y0, w, h, t, d as usize, ink.land);
    }
    let colon_x = x0 + 2 * w + 12 + (28 - t) / 2;
    seg::draw_colon(c, colon_x, y0, h, t, if now.second() % 2 == 0 { ink.land } else { ink.faint });
    if opts.date {
        chip(c, fs, ink, 4, ROW_A, &date_line(now), false);
    }
    if let Some((pct, charging)) = opts.battery {
        battery_chip(c, assets, fs, ink, RIGHT - battery_w(fs, pct), ROW_A, pct, charging);
    }
    if let Some(v) = &opts.update {
        update_chip(c, fs, ink, 4, ROW_B, unlock_left(fs) - 4 - 4, v);
    }
    unlock_chip(c, fs, ink, RIGHT, ROW_B, unlock);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, PI, TAU};

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn hands_point_the_right_way() {
        let (h, m, s) = hand_angles(3, 0, 0);
        assert!(close(h, FRAC_PI_2) && close(m, 0.0) && close(s, 0.0));
        let (h, m, _) = hand_angles(6, 30, 0);
        assert!(close(m, PI), "30 minutes is straight down");
        assert!(close(h, 6.5 / 12.0 * TAU), "the hour hand is half way between 6 and 7");
        let (h, _, _) = hand_angles(15, 0, 0);
        assert!(close(h, FRAC_PI_2), "24-hour input wraps to 12-hour");
        let (_, m, s) = hand_angles(1, 14, 30);
        assert!(close(s, PI) && close(m, 14.5 / 60.0 * TAU), "the minute hand creeps with the seconds");
    }

    #[test]
    fn styles_follow_the_settings_pop_up() {
        assert_eq!(Style::from_index(0), Style::Wall);
        assert_eq!(Style::from_index(1), Style::World);
        assert_eq!(Style::from_index(2), Style::Ascii);
        assert_eq!(Style::from_index(3), Style::Seg);
        assert_eq!(Style::from_index(9), Style::Wall, "an unknown value falls back to the wall clock");
        assert_eq!(crate::launcher::settings::LOCK_STYLES.len(), 4);
    }

    #[test]
    fn options_come_from_the_harbour_lock_keys() {
        let dir = std::env::temp_dir().join(format!("lock-opts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::load(dir.join("config.json"));
        let o = Opts::from_config(&cfg, Some(76), false, None);
        assert_eq!(o, Opts { style: Style::Wall, date: true, battery: Some((Some(76), false)), update: None }, "defaults: wall clock, date and battery shown");
        assert_eq!(auto_lock_secs(&cfg), 0, "auto lock is off by default");
        cfg.set_int("harbour_lock_style", 3);
        cfg.set_int("harbour_lock_date", 0);
        cfg.set_int("harbour_lock_battery", 0);
        cfg.set_int("harbour_lock_auto", 300);
        let o = Opts::from_config(&cfg, Some(76), true, None);
        assert_eq!(o, Opts { style: Style::Seg, date: false, battery: None, update: None });
        assert_eq!(auto_lock_secs(&cfg), 300);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_update_notice_follows_the_switches_and_the_known_version() {
        let dir = std::env::temp_dir().join(format!("lock-upd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::load(dir.join("config.json"));
        let v = || Some("0.3.1".to_string());
        assert_eq!(Opts::from_config(&cfg, None, false, v()).update, v(), "shown by default when a newer version is known");
        assert_eq!(Opts::from_config(&cfg, None, false, None).update, None, "nothing known, nothing shown");
        cfg.set_int("harbour_lock_update", 0);
        assert_eq!(Opts::from_config(&cfg, None, false, v()).update, None, "its own switch hides it");
        cfg.set_int("harbour_lock_update", 1);
        cfg.set_int("harbour_update_check", 0);
        assert_eq!(Opts::from_config(&cfg, None, false, v()).update, None, "\"Check for new versions\" off hides it too");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_update_notice_wording() {
        assert_eq!(update_text("0.3.1", false), "Update 0.3.1 available");
        assert_eq!(update_text("0.3.1", true), "Update 0.3.1");
    }

    #[test]
    fn ascii_clock_fills_the_box_with_square_dots() {
        let l = ascii_clock(12, 34, true);
        assert_eq!(l.len(), 7);
        assert!(l.iter().all(|r| r.len() == 50), "every row is 50 columns: {:?}", l.iter().map(|r| r.len()).collect::<Vec<_>>());
        assert!(50 <= ASCII_COLS - 2, "fits inside the box");
        // "1" is 00100 on its first row: two dark dots, one lit, two dark, each two characters.
        assert_eq!(&l[0][..10], "....##....");
        assert_eq!(&l[2][22..28], "  ##  ", "the colon shows while lit");
        assert_eq!(&ascii_clock(12, 34, false)[2][22..28], "      ");
        assert!(l.iter().all(|r| r.chars().all(|c| matches!(c, '#' | '.' | ' '))));
    }

    #[test]
    fn hand_end_points() {
        assert_eq!(hand_end(100, 100, 50.0, 0.0), (100, 50), "12 o'clock is up");
        assert_eq!(hand_end(100, 100, 50.0, FRAC_PI_2), (150, 100), "3 o'clock is right");
        assert_eq!(hand_end(100, 100, 50.0, PI), (100, 150), "6 o'clock is down");
    }
}
