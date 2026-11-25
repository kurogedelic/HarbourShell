//! UI parts in the 1-bit style, with the mouse in mind: buttons, check
//! boxes, radio buttons, draggable sliders, drop-down menus, scroll bars and the
//! standard app menu. Drawing and hit-testing are plain functions on rectangles,
//! so screens keep their own state and stay easy to test.
//!
//! Colour roles (see docs/DESIGN.md): `widget_face_active` = surface,
//! `surface_alt` = alternate / hover, `surface_sunken` = wells, `line` = soft
//! rules, `text_faint` = disabled text only (secondary text uses `secondary_ink`), `accent_soft` = hover background.

use std::time::{Duration, Instant};

use crate::keys::Keycode;
use crate::gfx::Color;
use crate::gfx::Rect;
use crate::gfx::Canvas;

use crate::dither::{self, Pattern};
use crate::font::FontSystem;
use crate::grid;
use crate::launcher::i18n;
use crate::theme::ThemeManager;
use crate::ui;

// ----------------------------------------------------------------- pointer

/// What a pointer did, in screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerEvent {
    Move { x: i32, y: i32 },
    /// `clicks` is 1 for a single click, 2 for a double click.
    Down { x: i32, y: i32, button: u8, clicks: u8 },
    Up { x: i32, y: i32, button: u8 },
    /// Movement while the left button is held; `dx`/`dy` since the last event.
    Drag { x: i32, y: i32, dx: i32, dy: i32 },
    /// Positive = wheel up.
    Wheel { x: i32, y: i32, dy: i32 },
}

/// Tracks the pointer between events: position, button state, double clicks.
#[derive(Default)]
pub struct Pointer {
    pub x: i32,
    pub y: i32,
    pub left: bool,
    pub right: bool,
    last_click: Option<(i32, i32, Instant)>,
    /// Where the left button went down (for drag threshold decisions).
    pub press_origin: Option<(i32, i32)>,
}

pub const DOUBLE_CLICK: Duration = Duration::from_millis(450);
/// A click that moves further than this is not a double click.
pub const DOUBLE_CLICK_SLOP: i32 = 6;

impl Pointer {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y, ..Default::default() }
    }

    /// Relative motion: returns a `Drag` while the left button is held, else a `Move`.
    pub fn moved(&mut self, dx: i32, dy: i32) -> PointerEvent {
        self.x = (self.x + dx).clamp(0, grid::sw() - 1);
        self.y = (self.y + dy).clamp(0, grid::sh() - 1);
        if self.left {
            PointerEvent::Drag { x: self.x, y: self.y, dx, dy }
        } else {
            PointerEvent::Move { x: self.x, y: self.y }
        }
    }

    pub fn button(&mut self, button: u8, down: bool, now: Instant) -> PointerEvent {
        let (x, y) = (self.x, self.y);
        match (button, down) {
            (0, true) => {
                self.left = true;
                self.press_origin = Some((x, y));
                let double = self
                    .last_click
                    .map(|(lx, ly, t)| now.duration_since(t) < DOUBLE_CLICK && (lx - x).abs() <= DOUBLE_CLICK_SLOP && (ly - y).abs() <= DOUBLE_CLICK_SLOP)
                    .unwrap_or(false);
                // A double click is consumed: the next press starts a new sequence.
                self.last_click = if double { None } else { Some((x, y, now)) };
                PointerEvent::Down { x, y, button, clicks: if double { 2 } else { 1 } }
            }
            (0, false) => {
                self.left = false;
                self.press_origin = None;
                PointerEvent::Up { x, y, button }
            }
            (b, true) => {
                if b == 1 {
                    self.right = true;
                }
                PointerEvent::Down { x, y, button: b, clicks: 1 }
            }
            (b, false) => {
                if b == 1 {
                    self.right = false;
                }
                PointerEvent::Up { x, y, button: b }
            }
        }
    }
}

pub fn contains(r: Rect, x: i32, y: i32) -> bool {
    r.contains_point((x, y))
}

// ------------------------------------------------------------------ tones, panel, progress

/// The two extremes of a theme: `light` is the lighter of ink / paper (the "white"),
/// `dark` the darker (the "black"). Dark themes draw white-on-black text, but bars, seas and
/// tracks that should read as "white with a black outline" use these, so they look the same on
/// every theme.
#[derive(Clone, Copy)]
pub struct Tones {
    pub light: Color,
    pub dark: Color,
}

pub fn tones(tm: &ThemeManager) -> Tones {
    let ink = tm.get_color("foreground");
    let paper = tm.get_color("widget_face_active");
    let lum = |c: Color| c.r as u32 * 3 + c.g as u32 * 6 + c.b as u32;
    if lum(paper) >= lum(ink) {
        Tones { light: paper, dark: ink }
    } else {
        Tones { light: ink, dark: paper }
    }
}

// ------------------------------------------------------- selection and rows

/// Rows of ink above the text origin / the baseline row, for the body face (Shinonome 12) and
/// the display face (ChiKareGo2): capitals fill origin + 2 ..= origin + 10 (9 rows).
pub const CAP_TOP: i32 = 2;
pub const BASELINE: i32 = 10;

/// Where to draw text so its capitals sit in the middle of a row of `row_h` pixels: the same
/// number of blank rows above the capitals and below the baseline (an odd leftover goes below).
pub fn text_origin(row_y: i32, row_h: i32) -> i32 {
    let cap_h = BASELINE - CAP_TOP + 1;
    row_y + (row_h - cap_h) / 2 - CAP_TOP
}

/// How a selected row looks on this theme: the brightest tone as a plate with dark text on the
/// light themes, the usual inverted plate on the dark ones (see `select_bg` / `select_fg`).
#[derive(Clone, Copy)]
pub struct Selection {
    pub bg: Color,
    pub fg: Color,
}

pub fn selection(tm: &ThemeManager) -> Selection {
    Selection { bg: tm.get_color("select_bg"), fg: tm.get_color("select_fg") }
}

/// Secondary text (addresses, units, hints): the ink pulled 30% toward the zebra tone. The themes'
/// own `muted` / `text_faint` are too close to the stripes to read on them; this keeps a contrast
/// of at least 3:1 on both the page and the stripes on every theme.
pub fn secondary_ink(tm: &ThemeManager) -> Color {
    let (ink, alt) = (tm.get_color("foreground"), tm.get_color("surface_alt"));
    let mix = |a: u8, b: u8| (a as f32 * 0.7 + b as f32 * 0.3).round() as u8;
    Color::RGB(mix(ink.r, alt.r), mix(ink.g, alt.g), mix(ink.b, alt.b))
}

/// Text colour for a row: the selection's text when `selected`, otherwise `ink`.
pub fn row_ink(tm: &ThemeManager, selected: bool, ink: Color) -> Color {
    if selected { selection(tm).fg } else { ink }
}

/// The selection on a page of colour `page` that is not `home_bg` (Infoscope's face is the
/// theme's white, which on the light themes is `select_bg` itself): the theme's selection, unless
/// its plate would melt into the page; then a plate in [`rule_ink`] with page-coloured text.
pub fn selection_on(tm: &ThemeManager, page: Color) -> Selection {
    let s = selection(tm);
    if stands_out(tm, s.bg, page) {
        s
    } else {
        Selection { bg: rule_ink(tm, page), fg: page }
    }
}

/// True when `a` is at least a third of the theme's full tonal range away from `b`.
fn stands_out(tm: &ThemeManager, a: Color, b: Color) -> bool {
    let lum = |c: Color| c.r as i32 * 3 + c.g as i32 * 6 + c.b as i32;
    let t = tones(tm);
    (lum(a) - lum(b)).abs() * 3 >= (lum(t.light) - lum(t.dark)).abs()
}

/// Full-width selection plate for a row: one pixel shorter than the row so neighbouring plates
/// never touch. Pair it with [`draw_row_text`] (same row rectangle).
pub fn draw_row_plate(c: &mut Canvas, tm: &ThemeManager, row: Rect) {
    plate(c, row, selection(tm).bg);
}

/// [`draw_row_plate`] on a page of colour `page` (see [`selection_on`]); returns the selection, whose
/// `fg` is the row's text colour.
pub fn draw_row_plate_on(c: &mut Canvas, tm: &ThemeManager, row: Rect, page: Color) -> Selection {
    let s = selection_on(tm, page);
    plate(c, row, s.bg);
    s
}

fn plate(c: &mut Canvas, row: Rect, bg: Color) {
    ui::fill_rect(c, Rect::new(row.x(), row.y(), row.width(), row.height().saturating_sub(1)), bg);
}

/// The 1 px `line` rule on the last pixel row of a 16 px list row, across the row's full width.
/// Draw it after the plate (the plate is one pixel shorter, so the rule is never covered).
pub fn draw_row_rule(c: &mut Canvas, tm: &ThemeManager, row: Rect) {
    ui::fill_rect(c, row_rule_rect(row), tm.get_color("line"));
}

/// Where [`draw_row_rule`] draws: the bottom pixel row of `row`.
pub fn row_rule_rect(row: Rect) -> Rect {
    Rect::new(row.x(), row.bottom() - 1, row.width(), 1)
}

/// One line of body text in a row, capitals vertically centred (the plate above is symmetric
/// around them). Returns the text width.
pub fn draw_row_text(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, row: Rect, x: i32, text: &str, selected: bool, ink: Color) -> i32 {
    let y = text_origin(row.y(), row.height() as i32);
    fs.draw_text(c, x, y, text, row_ink(tm, selected, ink));
    fs.measure_text(text)
}

/// Like [`draw_row_text`] but the plate only hugs the text (3 px either side), as the label of a
/// control-panel row does.
pub fn draw_label_plate(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, row: Rect, x: i32, text: &str, selected: bool, ink: Color) {
    if selected {
        let w = fs.measure_text(text);
        draw_row_plate(c, tm, Rect::new(x - 3, row.y(), (w + 6) as u32, row.height()));
    }
    draw_row_text(c, fs, tm, row, x, text, selected, ink);
}

/// The scroll offset that keeps `cursor` inside a window of `visible` rows over `len` rows
/// (Files, Music and every other list scroll the same way): the offset only moves when the
/// cursor leaves the window, and never past the last full page.
pub fn follow_scroll(cursor: usize, scroll: usize, visible: usize, len: usize) -> usize {
    let mut s = scroll;
    if cursor < s {
        s = cursor;
    } else if cursor >= s + visible {
        s = cursor + 1 - visible;
    }
    s.min(len.saturating_sub(visible))
}

/// The most tabs a strip with per-tab widths (`Tabs::widths`) can hold.
pub const MAX_TABS: usize = 8;

/// A strip of equally wide tabs (or per-tab `widths`). The active tab wears the theme's selection; the others sit on
/// `idle_fill` with `idle_ink` text. `outlined` draws a 1 px ink frame around every tab (Timer);
/// without it the tabs are flat (Interceptor). `display` picks the display face for the labels.
pub struct Tabs<'a> {
    pub origin: (i32, i32),
    pub tab_w: i32,
    pub h: i32,
    pub labels: &'a [&'a str],
    pub active: usize,
    pub idle_fill: Color,
    pub idle_ink: Color,
    pub outlined: bool,
    pub display: bool,
    /// Folder tabs: chamfered top corners (2 px stair-step), all the same size with a rule under
    /// the whole strip; the active one wears the selection colours. The strip stays one `h` px
    /// row; the rule is its last pixel row. Wins over `outlined`.
    pub folder: bool,
    /// Per-tab widths instead of `tab_w` (the first `labels.len()` entries count), for a folder
    /// strip whose longest label would not fit in equal tabs (see `Tabs::folder_fit`).
    pub widths: Option<[i32; MAX_TABS]>,
    /// The colour of the page right under a folder strip, when it is not `home_bg` (Infoscope's
    /// face). The strip's rule then takes a tone that stands out from it (see `rule_ink`).
    pub page: Option<Color>,
}

/// The colour of a folder strip's rule over a page of colour `page`: the ink, unless the ink is
/// (almost) the page's own tone (a light face on an Invert theme); then the theme tone furthest
/// from the page, so the rule always reads as a line of its own.
pub fn rule_ink(tm: &ThemeManager, page: Color) -> Color {
    let lum = |c: Color| c.r as i32 * 3 + c.g as i32 * 6 + c.b as i32;
    let ink = tm.get_color("foreground");
    if stands_out(tm, ink, page) {
        return ink;
    }
    ["foreground", "widget_face_active", "widget_highlight", "widget_shadow", "muted", "home_bg"]
        .iter()
        .map(|k| tm.get_color(k))
        .max_by_key(|c| (lum(*c) - lum(page)).abs())
        .unwrap_or(ink)
}

/// A tab body with its top corners cut by a 2 px stair-step: border `ink`, inside `fill`; rows
/// `top..=bottom` (the bottom row is drawn as part of the sides).
fn chamfered(c: &mut Canvas, x: i32, top: i32, w: i32, bottom: i32, ink: Color, fill: Color) {
    let px = |c: &mut Canvas, x0: i32, y: i32, n: i32, col: Color| ui::fill_rect(c, Rect::new(x0, y, n.max(1) as u32, 1), col);
    px(c, x + 2, top, w - 4, ink);
    px(c, x + 1, top + 1, 1, ink);
    px(c, x + w - 2, top + 1, 1, ink);
    px(c, x + 2, top + 1, w - 4, fill);
    for y in top + 2..=bottom {
        px(c, x, y, 1, ink);
        px(c, x + w - 1, y, 1, ink);
        px(c, x + 1, y, w - 2, fill);
    }
}

/// The one tab design every tabbed screen uses (Interceptor's): folder tabs in the display face on
/// a `home_bg` strip right under the status bar, 5 units high. The page starts at `FOLDER_BODY_Y`.
pub const FOLDER_TABS_H: i32 = 20;
pub const FOLDER_BODY_Y: i32 = grid::BAR_H + FOLDER_TABS_H;
const FOLDER_X: i32 = grid::MARGIN;
/// The narrowest folder tab (Interceptor's seven tabs), so screens with short labels still match.
pub const FOLDER_MIN_TAB_W: i32 = 44;

/// A folder tab width that fits the widest of `labels` (in the face the strip draws them with)
/// with room on both sides of the chamfered corners, at least `FOLDER_MIN_TAB_W`, and never wider
/// than `room` px shared by all tabs (the strip's last pixel column included).
pub fn folder_tab_w(fs: &FontSystem, labels: &[&str], room: i32) -> i32 {
    let n = labels.len().max(1) as i32;
    folder_need(fs, labels).max().unwrap_or(0).max(FOLDER_MIN_TAB_W).min((room - 1) / n)
}

/// Each label's width plus 5 px on both sides (clear of the chamfered corners and the border).
fn folder_need<'b>(fs: &'b FontSystem, labels: &'b [&str]) -> impl Iterator<Item = i32> + 'b {
    labels.iter().map(|l| fs.measure_display_text(l) + 10)
}

/// Widths for `labels` in `room` px: equal tabs when the widest label fits in them (at least
/// `FOLDER_MIN_TAB_W`), otherwise each tab as wide as its own label needs with the spare room
/// shared out evenly (so no label is ever cut). `None` = equal tabs of `folder_tab_w`.
pub fn folder_widths(needs: &[i32], room: i32) -> Option<[i32; MAX_TABS]> {
    let n = needs.len().clamp(1, MAX_TABS) as i32;
    let widest = needs.iter().copied().max().unwrap_or(0);
    if widest <= (room - 1) / n {
        return None;
    }
    let spare = (room - 1 - needs.iter().take(MAX_TABS).sum::<i32>()).max(0);
    let mut w = [0; MAX_TABS];
    for (i, need) in needs.iter().take(MAX_TABS).enumerate() {
        w[i] = need + spare / n + if (i as i32) < spare % n { 1 } else { 0 };
    }
    Some(w)
}

/// Where a line of text on the folder strip (a status right of the tabs) goes: level with the
/// idle tabs' labels.
pub fn folder_strip_text_y() -> i32 {
    folder_idle_text_y(grid::BAR_H, grid::BAR_H + FOLDER_TABS_H - 1)
}

/// Text origin of an idle folder tab's label in a strip from `top` whose rule is the row `last`.
fn folder_idle_text_y(top: i32, last: i32) -> i32 {
    text_origin(top + 5, last - (top + 5))
}

/// The room a full-width folder strip offers (margins on both sides).
pub fn folder_room() -> i32 {
    grid::sw() - 2 * grid::MARGIN
}
impl<'a> Tabs<'a> {
    /// The shared folder strip (see `FOLDER_TABS_H`); `tab_w` usually from `folder_tab_w`.
    pub fn folder_strip(labels: &'a [&'a str], active: usize, tab_w: i32, tm: &ThemeManager) -> Self {
        Tabs {
            origin: (FOLDER_X, grid::BAR_H),
            tab_w,
            h: FOLDER_TABS_H,
            labels,
            active,
            idle_fill: tm.get_color("surface_alt"),
            idle_ink: tm.get_color("foreground"),
            outlined: true,
            display: true,
            folder: true,
            widths: None,
            page: None,
        }
    }

    /// The shared folder strip across the full width, its tabs sized for `labels` in the current
    /// language (see `folder_widths`).
    pub fn folder_fit(fs: &FontSystem, labels: &'a [&'a str], active: usize, tm: &ThemeManager) -> Self {
        let needs: Vec<i32> = folder_need(fs, labels).collect();
        let mut t = Self::folder_strip(labels, active, folder_tab_w(fs, labels, folder_room()), tm);
        t.widths = folder_widths(&needs, folder_room());
        t
    }

    /// The strip's full-width `home_bg` band, then the tabs on it.
    pub fn draw_strip(&self, c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager) {
        ui::fill_rect(c, Rect::new(0, self.origin.1, grid::sw() as u32, self.h as u32), tm.get_color("home_bg"));
        self.draw(c, fs, tm);
    }

    /// The x just right of the last tab (where a status line on the strip may start).
    pub fn end_x(&self) -> i32 {
        self.x_of(self.labels.len()) + 1
    }
}

/// The narrow tab at each end of a [`FolderScroll`] strip, holding a 1-bit arrow.
pub const FOLDER_ARROW_W: i32 = 15;

/// Where a press on a [`FolderScroll`] strip landed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderHit {
    Tab(usize),
    Prev,
    Next,
}

/// A folder strip with more tabs than fit: a window of whole tabs between two narrow arrow tabs
/// (the "feed" tabs). The window follows the active tab (see [`FolderScroll::follow`]); when every
/// tab fits, it is the plain strip without arrows.
pub struct FolderScroll<'a> {
    pub labels: &'a [&'a str],
    pub active: usize,
    /// Index of the first tab shown.
    pub first: usize,
    /// Room for the strip from `FOLDER_X` (normally `folder_room()`; less leaves a gap at the right).
    pub room: i32,
    pub min_tab_w: i32,
}

impl<'a> FolderScroll<'a> {
    pub fn new(labels: &'a [&'a str], active: usize, first: usize, room: i32) -> Self {
        Self { labels, active, first, room, min_tab_w: FOLDER_MIN_TAB_W }
    }

    fn overflow(&self) -> bool {
        self.labels.len() as i32 * self.min_tab_w > self.room - 1
    }

    /// How many tabs are shown at once.
    pub fn shown(&self) -> usize {
        let n = self.labels.len();
        if !self.overflow() {
            return n;
        }
        (((self.room - 1 - 2 * FOLDER_ARROW_W) / self.min_tab_w).max(1) as usize).min(n)
    }

    /// The tab width that fills the room between the arrows.
    fn tab_w(&self) -> i32 {
        let k = self.shown().max(1) as i32;
        if self.overflow() {
            (self.room - 1 - 2 * FOLDER_ARROW_W) / k
        } else {
            self.min_tab_w
        }
    }

    /// The first tab to show so that `active` is in the window, moving it as little as possible.
    pub fn follow(labels: &[&str], active: usize, first: usize, room: i32) -> usize {
        let s = FolderScroll::new(labels, active, first, room);
        let (n, k) = (labels.len(), s.shown());
        follow_scroll(active.min(n.saturating_sub(1)), first, k, n)
    }

    fn first(&self) -> usize {
        self.first.min(self.labels.len().saturating_sub(self.shown()))
    }

    fn strip_x(&self) -> i32 {
        FOLDER_X + if self.overflow() { FOLDER_ARROW_W } else { 0 }
    }

    fn strip(&self, tm: &ThemeManager) -> Tabs<'a> {
        let f = self.first();
        let end = (f + self.shown()).min(self.labels.len());
        let active = if (f..end).contains(&self.active) { self.active - f } else { usize::MAX };
        let mut t = Tabs::folder_strip(&self.labels[f..end], active, self.tab_w(), tm);
        t.origin.0 = self.strip_x();
        t
    }

    fn arrow_rects(&self) -> (Rect, Rect) {
        let y = grid::BAR_H;
        let right_x = self.strip_x() + self.shown() as i32 * self.tab_w() - 1;
        let right_w = FOLDER_X + self.room - 1 - right_x;
        (Rect::new(FOLDER_X, y, (FOLDER_ARROW_W + 1) as u32, FOLDER_TABS_H as u32), Rect::new(right_x, y, right_w.max(1) as u32, FOLDER_TABS_H as u32))
    }

    pub fn hit(&self, x: i32, y: i32, tm: &ThemeManager) -> Option<FolderHit> {
        if let Some(i) = self.strip(tm).hit(x, y) {
            return Some(FolderHit::Tab(self.first() + i));
        }
        if !self.overflow() {
            return None;
        }
        let (l, r) = self.arrow_rects();
        if contains(l, x, y) {
            Some(FolderHit::Prev)
        } else if contains(r, x, y) {
            Some(FolderHit::Next)
        } else {
            None
        }
    }

    /// The `home_bg` band, the window of tabs, the rule and the two arrow tabs.
    pub fn draw_strip(&self, c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager) {
        let y0 = grid::BAR_H;
        ui::fill_rect(c, Rect::new(0, y0, grid::sw() as u32, FOLDER_TABS_H as u32), tm.get_color("home_bg"));
        let strip = self.strip(tm);
        strip.draw(c, fs, tm);
        if !self.overflow() {
            return;
        }
        let ink = tm.get_color("foreground");
        let last = y0 + FOLDER_TABS_H - 1;
        let (l, r) = self.arrow_rects();
        // The rule under the arrows (the strip draws its own under the tabs).
        ui::fill_rect(c, Rect::new(FOLDER_X, last, (strip.origin.0 - FOLDER_X) as u32, 1), ink);
        ui::fill_rect(c, Rect::new(r.x(), last, r.width(), 1), ink);
        let f = self.first();
        let more_left = f > 0;
        let more_right = f + self.shown() < self.labels.len();
        let fill = tm.get_color("surface_alt");
        let dim = secondary_ink(tm);
        for (rect, left, more) in [(l, true, more_left), (r, false, more_right)] {
            chamfered(c, rect.x(), y0 + 4, rect.width() as i32, last - 1, ink, fill);
            // A 4 px wide, 7 px tall solid triangle, centred in the lowered tab.
            let cx = rect.x() + rect.width() as i32 / 2;
            let cy = y0 + 4 + (last - 1 - (y0 + 4)) / 2 + 1;
            let col = if more { ink } else { dim };
            for i in 0..4 {
                let x = if left { cx - 2 + i } else { cx + 1 - i };
                ui::fill_rect(c, Rect::new(x, cy - i, 1, (2 * i + 1) as u32), col);
            }
        }
    }
}

impl Tabs<'_> {
    fn w_of(&self, i: usize) -> i32 {
        match &self.widths {
            Some(w) => w[i.min(MAX_TABS - 1)],
            None => self.tab_w,
        }
    }

    /// The left edge of tab `i` (`i == len` is the right end of the strip).
    fn x_of(&self, i: usize) -> i32 {
        self.origin.0 + (0..i).map(|k| self.w_of(k)).sum::<i32>()
    }

    pub fn tab_rect(&self, i: usize) -> Rect {
        Rect::new(self.x_of(i), self.origin.1, self.w_of(i) as u32, self.h as u32)
    }

    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.labels.len()).find(|&i| contains(self.tab_rect(i), x, y))
    }

    fn label(&self, c: &mut Canvas, fs: &FontSystem, x: i32, w: i32, y: i32, text: &str, col: Color) {
        if self.display {
            let tw = fs.measure_display_text(text);
            fs.draw_display_text(c, x + (w - tw) / 2, y, text, col);
        } else {
            let tw = fs.measure_text(text);
            fs.draw_text(c, x + (w - tw) / 2, y, text, col);
        }
    }

    fn draw_folder(&self, c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        let sel = selection(tm);
        let (x0, y0) = self.origin;
        let n = self.labels.len() as i32;
        let last = y0 + self.h - 1;
        // Every tab is the same size; the active one differs only in colour (the theme's
        // selection). Neighbours share their edge (each is drawn one px wider).
        for (i, label) in self.labels.iter().enumerate() {
            let r = self.tab_rect(i);
            let w = r.width() as i32 + if (i as i32) < n - 1 { 1 } else { 0 };
            let active = i == self.active;
            let (fill, ink_t) = if active { (sel.bg, sel.fg) } else { (self.idle_fill, self.idle_ink) };
            chamfered(c, r.x(), y0 + 4, w, last - 1, ink, fill);
            // Under the stair-stepped corners, 1 px clear of the rule: a 12 px Japanese glyph ends on
            // y0 + 17 (the rule is y0 + 19); the display face sits 1 px higher so that its
            // descenders (g, p, y) end there too.
            let lift = if self.display && fs.has_display_face(label) { 1 } else { 0 };
            self.label(c, fs, r.x(), r.width() as i32, folder_idle_text_y(y0, last) - lift, label, ink_t);
        }
        // The rule under the whole strip: full width (the same margin on the right as on the left,
        // or to the last tab if that reaches further).
        let right = (grid::sw() - x0).max(self.x_of(n as usize) + 1);
        let rule = self.page.map(|pg| rule_ink(tm, pg)).unwrap_or(ink);
        ui::fill_rect(c, Rect::new(x0, last, (right - x0) as u32, 1), rule);
    }

    pub fn draw(&self, c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager) {
        if self.folder {
            return self.draw_folder(c, fs, tm);
        }
        let ink = tm.get_color("foreground");
        let sel = selection(tm);
        for (i, label) in self.labels.iter().enumerate() {
            let r = self.tab_rect(i);
            let active = i == self.active;
            ui::fill_rect(c, r, if active { sel.bg } else { self.idle_fill });
            if self.outlined {
                ui::draw_rect(c, r, ink);
            }
            let col = if active { sel.fg } else { self.idle_ink };
            let y = text_origin(r.y(), r.height() as i32);
            if self.display {
                let w = fs.measure_display_text(label);
                fs.draw_display_text(c, r.x() + (r.width() as i32 - w) / 2, y, label, col);
            } else {
                let w = fs.measure_text(label);
                fs.draw_text(c, r.x() + (r.width() as i32 - w) / 2, y, label, col);
            }
        }
    }
}

/// A framed card: solid 1 px drop shadow, a surface face and a
/// 2 px outline. Dialogs, alerts, the launch screen and toasts all use it.
pub fn draw_panel(c: &mut Canvas, tm: &ThemeManager, r: Rect) {
    let ink = tm.get_color("foreground");
    hard_shadow(c, r, ink);
    ui::fill_rect(c, r, tm.get_color("widget_face_active"));
    ui::draw_rect_thick(c, r, ink);
}

/// The progress bar: a white track with a black outline and a solid black gauge (the Classic
/// Mac bar). `fraction` 0..=1 fills it; `None` is "no idea how long": diagonal black stripes
/// slide along (`tick` advances them, about every other frame).
pub struct ProgressBar {
    pub rect: Rect,
    pub fraction: Option<f32>,
    pub tick: u32,
}

impl ProgressBar {
    pub fn new(rect: Rect, fraction: Option<f32>, tick: u32) -> Self {
        Self { rect, fraction, tick }
    }

    pub fn draw(&self, c: &mut Canvas, tm: &ThemeManager) {
        let t = tones(tm);
        let r = self.rect;
        // Outer ring in the light tone (it shows on dark panels and melts into light ones), then the
        // black outline, then the white track.
        ui::fill_rect(c, r, t.light);
        let outline = Rect::new(r.x() + 1, r.y() + 1, r.width().saturating_sub(2), r.height().saturating_sub(2));
        ui::draw_rect(c, outline, t.dark);
        let inner = Rect::new(r.x() + 2, r.y() + 2, r.width().saturating_sub(4), r.height().saturating_sub(4));
        match self.fraction {
            Some(f) => {
                let w = (inner.width() as f32 * f.clamp(0.0, 1.0)).round() as u32;
                if w > 0 {
                    ui::fill_rect(c, Rect::new(inner.x(), inner.y(), w, inner.height()), t.dark);
                }
            }
            None => {
                c.set_draw_color(t.dark);
                let shift = (self.tick / 2) as i32;
                for y in 0..inner.height() as i32 {
                    let mut x = -(y + shift).rem_euclid(8) - 8;
                    while x < inner.width() as i32 {
                        for k in 0..4 {
                            let px = x + k;
                            if px >= 0 && px < inner.width() as i32 {
                                let _ = c.draw_point((inner.x() + px, inner.y() + y));
                            }
                        }
                        x += 8;
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ button

#[derive(Clone, Copy, Default)]
pub struct BtnState {
    pub hover: bool,
    pub pressed: bool,
    /// The default button gets the extra ring.
    pub default: bool,
    pub disabled: bool,
}

/// A rounded push button. `pressed` inverts it; `hover` tints the face;
/// `disabled` greys the label with a dither.
pub fn draw_button(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, r: Rect, label: &str, st: BtnState) {
    let ink = tm.get_color("foreground");
    // Buttons are white (the theme's icon white) like the calculator keys.
    let surface = tm.get_color("icon_white");
    let hover = tm.get_color("accent_soft");
    let faint = tm.get_color("text_faint");
    if st.default && !st.disabled {
        // The default button wears a heavy ring: two rounded outlines.
        rounded_frame(c, Rect::new(r.x() - 3, r.y() - 3, r.width() + 6, r.height() + 6), ink);
        rounded_frame(c, Rect::new(r.x() - 2, r.y() - 2, r.width() + 4, r.height() + 4), ink);
    }
    let face = if st.pressed && !st.disabled {
        ink
    } else if st.hover && !st.disabled {
        hover
    } else {
        surface
    };
    // Fill without the four corner pixels: the rounded frame leaves them out, and a filled corner would stick out
    // of the notch by 1 px.
    ui::fill_rect(c, Rect::new(r.x() + 1, r.y(), r.width().saturating_sub(2), r.height()), face);
    ui::fill_rect(c, Rect::new(r.x(), r.y() + 1, r.width(), r.height().saturating_sub(2)), face);
    rounded_frame(c, r, if st.disabled { faint } else { ink });
    let fg = if st.pressed && !st.disabled {
        surface
    } else if st.disabled {
        faint
    } else {
        ink
    };
    let label = &i18n::tr_s(label);
    let w = fs.measure_display_text(label);
    let (tx, ty) = (r.x() + (r.width() as i32 - w) / 2, r.y() + (r.height() as i32 - 13) / 2);
    fs.draw_display_text(c, tx, ty, label, fg);
    if st.disabled {
        dither::gray_out(c, Rect::new(r.x() + 1, r.y() + 1, r.width() - 2, r.height() - 2), surface);
    }
}

/// A rounded push button with a small picture instead of a label: `glyph` rows of '#' (ink) and '.'
/// (nothing), centred. Same states as [`draw_button`]: pressed inverts, hover tints, disabled greys
/// the frame and dithers the picture away.
pub fn draw_icon_button(c: &mut Canvas, tm: &ThemeManager, r: Rect, glyph: &[&str], st: BtnState) {
    let ink = tm.get_color("foreground");
    let surface = tm.get_color("icon_white");
    let faint = tm.get_color("text_faint");
    let face = if st.pressed && !st.disabled {
        ink
    } else if st.hover && !st.disabled {
        tm.get_color("accent_soft")
    } else {
        surface
    };
    ui::fill_rect(c, Rect::new(r.x() + 1, r.y(), r.width().saturating_sub(2), r.height()), face);
    ui::fill_rect(c, Rect::new(r.x(), r.y() + 1, r.width(), r.height().saturating_sub(2)), face);
    rounded_frame(c, r, if st.disabled { faint } else { ink });
    let fg = if st.pressed && !st.disabled {
        surface
    } else if st.disabled {
        faint
    } else {
        ink
    };
    let (gw, gh) = (glyph.first().map(|l| l.len()).unwrap_or(0) as i32, glyph.len() as i32);
    let (gx, gy) = (r.x() + (r.width() as i32 - gw) / 2, r.y() + (r.height() as i32 - gh) / 2);
    c.set_draw_color(fg);
    for (dy, row) in glyph.iter().enumerate() {
        for (dx, b) in row.bytes().enumerate() {
            if b == b'#' {
                let _ = c.draw_point((gx + dx as i32, gy + dy as i32));
            }
        }
    }
    if st.disabled {
        dither::gray_out(c, Rect::new(r.x() + 1, r.y() + 1, r.width() - 2, r.height() - 2), surface);
    }
}

/// 1px frame with the corners notched (a Mac "rounded rectangle" at this size).
pub fn rounded_frame(c: &mut Canvas, r: Rect, color: Color) {
    c.set_draw_color(color);
    let (x0, y0, x1, y1) = (r.x(), r.y(), r.x() + r.width() as i32 - 1, r.y() + r.height() as i32 - 1);
    let _ = c.draw_line((x0 + 2, y0), (x1 - 2, y0));
    let _ = c.draw_line((x0 + 2, y1), (x1 - 2, y1));
    let _ = c.draw_line((x0, y0 + 2), (x0, y1 - 2));
    let _ = c.draw_line((x1, y0 + 2), (x1, y1 - 2));
    for (x, y) in [(x0 + 1, y0 + 1), (x1 - 1, y0 + 1), (x0 + 1, y1 - 1), (x1 - 1, y1 - 1)] {
        let _ = c.draw_point((x, y));
    }
}

// -------------------------------------------------------------- text field

/// Height of every one-line text field: one grid row.
pub const FIELD_H: i32 = 16;

/// What a one-line text field shows. Every text entry in Harbour draws through [`draw_field`]:
/// a 16 px well (sunken fill, 1 px ink frame), body-font text 4 px in, a 1 px caret at the
/// capitals' height, the text scrolled so the caret stays in view.
#[derive(Clone, Copy, Default)]
pub struct Field<'a> {
    pub text: &'a str,
    /// Caret column in characters; `None` = the field is not being typed in (no caret).
    pub caret: Option<usize>,
    /// The whole text is selected (the first key replaces it): drawn inverted.
    pub select_all: bool,
    /// Show one `*` per character (passwords).
    pub masked: bool,
    /// Faint text shown while the field is empty.
    pub placeholder: &'a str,
    /// Fixed faint text at the start (a prompt `>`, a cell name), `prefix_w` px wide (0 = its width).
    pub prefix: &'a str,
    pub prefix_w: i32,
    /// Colours for a field on a page that is not the usual one (Infoscope's face).
    pub fill: Option<Color>,
    pub ink: Option<Color>,
}

impl<'a> Field<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text, ..Default::default() }
    }
    /// Being typed in, caret at the end.
    pub fn editing(text: &'a str) -> Self {
        Self { text, caret: Some(text.chars().count()), ..Default::default() }
    }
}

pub fn draw_field(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, r: Rect, f: &Field) {
    let ink = f.ink.unwrap_or_else(|| tm.get_color("foreground"));
    let fill = f.fill.unwrap_or_else(|| tm.get_color("surface_sunken"));
    let faint = if f.ink.is_some() { ink } else { secondary_ink(tm) };
    ui::fill_rect(c, r, fill);
    ui::draw_rect(c, r, ink);
    let ty = text_origin(r.y(), r.height() as i32);
    let mut x0 = r.x() + 4;
    if !f.prefix.is_empty() {
        fs.draw_text(c, x0, ty, f.prefix, faint);
        x0 += if f.prefix_w > 0 { f.prefix_w } else { fs.measure_text(f.prefix) + 4 };
    }
    let room = r.right() - 4 - x0;
    if room <= 0 {
        return;
    }
    let shown: String = if f.masked { "*".repeat(f.text.chars().count()) } else { f.text.to_string() };
    let col = f.caret.map(|k| k.min(shown.chars().count()));
    let before_w = col.map_or(0, |k| fs.measure_text(&shown.chars().take(k).collect::<String>()));
    let shift = (before_w - (room - 2)).max(0);
    // Clip the text to the field, inside whatever clip the caller has (a scrolled page).
    let prev = c.clip_rect();
    let inner = Rect::new(x0 - 1, r.y() + 1, (room + 2) as u32, r.height().saturating_sub(2));
    match prev {
        crate::gfx::ClipRect::Some(p) => match p.intersection(inner) {
            Some(q) => c.set_clip_rect(q),
            None => return,
        },
        crate::gfx::ClipRect::Zero => return,
        crate::gfx::ClipRect::None => c.set_clip_rect(inner),
    }
    if shown.is_empty() {
        if !f.placeholder.is_empty() {
            fs.draw_text(c, x0, ty, &i18n::tr_s(f.placeholder), faint);
        }
    } else if f.select_all {
        let tw = fs.measure_text(&shown).min(room);
        ui::fill_rect(c, Rect::new(x0 - 1, r.y() + 2, (tw + 2) as u32, r.height().saturating_sub(4)), ink);
        fs.draw_text(c, x0, ty, &shown, fill);
    } else {
        fs.draw_text(c, x0 - shift, ty, &shown, ink);
    }
    if let (Some(_), false) = (col, f.select_all && !shown.is_empty()) {
        ui::fill_rect(c, Rect::new(x0 - shift + before_w, ty + CAP_TOP - 1, 1, 11), ink);
    }
    c.set_clip_rect(prev);
}

// -------------------------------------------------------- check box / radio

/// 12x12 check box. `mixed` shows a dash (some of a group selected).
pub fn draw_checkbox(c: &mut Canvas, tm: &ThemeManager, x: i32, y: i32, checked: bool, mixed: bool, st: BtnState) {
    let ink = tm.get_color("foreground");
    let well = tm.get_color("surface_sunken");
    let faint = tm.get_color("text_faint");
    let r = Rect::new(x, y, 12, 12);
    ui::fill_rect(c, r, if st.hover { tm.get_color("accent_soft") } else { well });
    ui::draw_rect(c, r, if st.disabled { faint } else { ink });
    c.set_draw_color(if st.disabled { faint } else { ink });
    if checked {
        // A bold check mark.
        for (dx, dy) in [(2, 6), (3, 7), (4, 8), (5, 7), (6, 6), (7, 5), (8, 4), (9, 3)] {
            let _ = c.draw_point((x + dx, y + dy));
            let _ = c.draw_point((x + dx, y + dy + 1));
        }
    } else if mixed {
        let _ = c.fill_rect(Rect::new(x + 3, y + 5, 6, 2));
    }
    if st.pressed {
        dither::fill(c, Rect::new(x + 1, y + 1, 10, 10), ink, Pattern::Quarter);
    }
}

pub const CHECKBOX_SIZE: i32 = 12;

/// 12x12 round radio button.
pub fn draw_radio(c: &mut Canvas, tm: &ThemeManager, x: i32, y: i32, selected: bool, st: BtnState) {
    let ink = tm.get_color("foreground");
    let well = tm.get_color("surface_sunken");
    let fg = if st.disabled { tm.get_color("text_faint") } else { ink };
    ui::fill_rect(c, Rect::new(x + 2, y + 2, 8, 8), if st.hover { tm.get_color("accent_soft") } else { well });
    c.set_draw_color(fg);
    for (dx, dy, len) in [(4, 0, 4), (2, 1, 2), (8, 1, 2), (1, 2, 1), (10, 2, 1), (0, 4, 1), (11, 4, 1), (0, 5, 2), (11, 5, 2), (0, 7, 1), (11, 7, 1), (1, 9, 1), (10, 9, 1), (2, 10, 2), (8, 10, 2), (4, 11, 4)] {
        let _ = c.fill_rect(Rect::new(x + dx, y + dy, len as u32, 1));
    }
    // vertical sides
    let _ = c.fill_rect(Rect::new(x, y + 4, 1, 4));
    let _ = c.fill_rect(Rect::new(x + 11, y + 4, 1, 4));
    if selected {
        let _ = c.fill_rect(Rect::new(x + 4, y + 4, 4, 4));
        let _ = c.fill_rect(Rect::new(x + 3, y + 5, 6, 2));
        let _ = c.fill_rect(Rect::new(x + 5, y + 3, 2, 6));
    }
}

// ------------------------------------------------------------------ slider

/// Horizontal slider with ticks and a draggable thumb. `value` is 0..=100.
pub struct Slider {
    pub track: Rect,
}

pub const THUMB_W: i32 = 8;

impl Slider {
    pub fn new(track: Rect) -> Self {
        Self { track }
    }

    fn span(&self) -> i32 {
        self.track.width() as i32 - THUMB_W
    }

    pub fn thumb(&self, value: u32) -> Rect {
        let x = self.track.x() + self.span() * value.min(100) as i32 / 100;
        Rect::new(x, self.track.y() - 2, THUMB_W as u32, self.track.height() + 4)
    }

    /// Value under screen x (the thumb centre follows the pointer).
    pub fn value_at(&self, x: i32) -> u32 {
        let rel = x - self.track.x() - THUMB_W / 2;
        (rel * 100 / self.span().max(1)).clamp(0, 100) as u32
    }

    pub fn hit(&self, x: i32, y: i32) -> bool {
        contains(Rect::new(self.track.x(), self.track.y() - 4, self.track.width(), self.track.height() + 8), x, y)
    }

    pub fn draw(&self, c: &mut Canvas, tm: &ThemeManager, value: u32, dragging: bool, hover: bool) {
        let ink = tm.get_color("foreground");
        let line = tm.get_color("line");
        let well = tm.get_color("surface_sunken");
        let surface = tm.get_color("widget_face_active");
        // Groove: sunken well with a dithered filled part.
        ui::fill_rect(c, self.track, well);
        ui::draw_rect(c, self.track, line);
        let t = self.thumb(value);
        let filled = Rect::new(self.track.x() + 1, self.track.y() + 1, (t.x() - self.track.x()).max(0) as u32, self.track.height().saturating_sub(2));
        dither::fill(c, filled, ink, Pattern::Half);
        // Ticks under the track.
        c.set_draw_color(line);
        for i in 0..=4 {
            let tx = self.track.x() + THUMB_W / 2 + (self.track.width() as i32 - THUMB_W) * i / 4;
            let _ = c.draw_line((tx, self.track.y() + self.track.height() as i32 + 1), (tx, self.track.y() + self.track.height() as i32 + 3));
        }
        // Thumb.
        let face = if dragging { ink } else if hover { tm.get_color("accent_soft") } else { surface };
        ui::fill_rect(c, t, face);
        ui::draw_rect(c, t, ink);
        if !dragging {
            let _ = c.draw_line((t.x() + t.width() as i32 / 2, t.y() + 2), (t.x() + t.width() as i32 / 2, t.y() + t.height() as i32 - 3));
        }
    }
}

// --------------------------------------------------------------- scroll bar

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SbPart {
    Up,
    Down,
    /// The track above the thumb (page up) / below it (page down).
    PageUp,
    PageDown,
    Thumb,
}

/// A vertical scroll bar: arrow boxes at both ends, a dithered track, a thumb.
pub struct ScrollBar {
    pub rect: Rect,
    pub total: usize,
    pub visible: usize,
    pub offset: usize,
}

impl ScrollBar {
    pub fn new(rect: Rect, total: usize, visible: usize, offset: usize) -> Self {
        Self { rect, total, visible, offset }
    }

    pub fn needed(&self) -> bool {
        self.total > self.visible
    }

    fn arrow_h(&self) -> i32 {
        self.rect.width() as i32
    }

    fn track(&self) -> Rect {
        let a = self.arrow_h();
        Rect::new(self.rect.x(), self.rect.y() + a, self.rect.width(), (self.rect.height() as i32 - 2 * a).max(1) as u32)
    }

    pub fn thumb(&self) -> Rect {
        let t = self.track();
        let max_off = self.total.saturating_sub(self.visible).max(1);
        let len = ((t.height() as usize * self.visible) / self.total.max(1)).clamp(8, t.height() as usize) as i32;
        let span = t.height() as i32 - len;
        let y = t.y() + (span as usize * self.offset.min(max_off) / max_off) as i32;
        Rect::new(t.x(), y, t.width(), len as u32)
    }

    pub fn hit(&self, x: i32, y: i32) -> Option<SbPart> {
        if !contains(self.rect, x, y) || !self.needed() {
            return None;
        }
        let a = self.arrow_h();
        if y < self.rect.y() + a {
            return Some(SbPart::Up);
        }
        if y >= self.rect.y() + self.rect.height() as i32 - a {
            return Some(SbPart::Down);
        }
        let th = self.thumb();
        Some(if contains(th, x, y) {
            SbPart::Thumb
        } else if y < th.y() {
            SbPart::PageUp
        } else {
            SbPart::PageDown
        })
    }

    /// What a press at (x, y) does: the new offset (arrows step one row, the track a page) and
    /// whether a thumb drag starts (then follow with `offset_for_y`). `None` = not on the bar.
    pub fn press(&self, x: i32, y: i32) -> Option<(usize, bool)> {
        let max_off = self.total.saturating_sub(self.visible);
        let page = self.visible.max(1);
        Some(match self.hit(x, y)? {
            SbPart::Up => (self.offset.saturating_sub(1), false),
            SbPart::Down => ((self.offset + 1).min(max_off), false),
            SbPart::PageUp => (self.offset.saturating_sub(page), false),
            SbPart::PageDown => ((self.offset + page).min(max_off), false),
            SbPart::Thumb => (self.offset, true),
        })
    }

    /// Offset that puts the thumb's centre at screen y (for dragging).
    pub fn offset_for_y(&self, y: i32) -> usize {
        let t = self.track();
        let th = self.thumb();
        let span = (t.height() as i32 - th.height() as i32).max(1);
        let pos = (y - t.y() - th.height() as i32 / 2).clamp(0, span);
        let max_off = self.total.saturating_sub(self.visible);
        (pos as usize * max_off + span as usize / 2) / span as usize
    }

    /// Always drawn (the thumb fills the track when everything fits), so a screen keeps the same
    /// layout whether or not it scrolls; only `hit` ignores a bar that is not needed.
    pub fn draw(&self, c: &mut Canvas, tm: &ThemeManager, active: Option<SbPart>) {
        self.paint(c, tm.get_color("foreground"), tm.get_color("widget_face_active"), tm.get_color("surface_sunken"), tm.get_color("line"), active);
    }

    /// [`ScrollBar::draw`] on a page of colour `page` that is not `home_bg` (Infoscope's face):
    /// boxes and track in the page's colour, outlined in [`rule_ink`], so the bar reads on every
    /// theme (the plain ink is the face's own tone on the Invert themes).
    pub fn draw_on(&self, c: &mut Canvas, tm: &ThemeManager, active: Option<SbPart>, page: Color) {
        let ink = rule_ink(tm, page);
        let line = tm.get_color("line");
        let line = if stands_out(tm, line, page) { line } else { ink };
        self.paint(c, ink, page, page, line, active);
    }

    fn paint(&self, c: &mut Canvas, ink: Color, surface: Color, well: Color, line: Color, active: Option<SbPart>) {
        let t = self.track();
        dither::fill_two_tone(c, t, well, line, Pattern::Half);
        let a = self.arrow_h();
        for (i, (y, up)) in [(self.rect.y(), true), (self.rect.y() + self.rect.height() as i32 - a, false)].into_iter().enumerate() {
            let r = Rect::new(self.rect.x(), y, a as u32, a as u32);
            let pressed = active == Some(if i == 0 { SbPart::Up } else { SbPart::Down });
            ui::fill_rect(c, r, if pressed { ink } else { surface });
            ui::draw_rect(c, r, ink);
            c.set_draw_color(if pressed { surface } else { ink });
            let (cx, cy) = (r.x() + a / 2, r.y() + a / 2);
            for k in 0..2 {
                let (ax, ay) = if up { (cx - 1 + k, cy - k) } else { (cx - 1 + k, cy + k) };
                let _ = c.draw_line((ax - k, ay), (ax + k + 1 - k, ay));
            }
            let _ = c.draw_point((cx, if up { cy - 1 } else { cy + 1 }));
        }
        let th = self.thumb();
        ui::fill_rect(c, th, if active == Some(SbPart::Thumb) { ink } else { surface });
        ui::draw_rect(c, th, ink);
    }
}

// ----------------------------------------------------------------- dropdown

/// A pop-up menu button: shows the current choice; opens a list of choices.
pub struct Dropdown {
    pub rect: Rect,
    pub items: Vec<String>,
}

impl Dropdown {
    pub fn new(rect: Rect, items: Vec<String>) -> Self {
        Self { rect, items }
    }

    /// The list drops below the button (or above it when it would leave the screen).
    pub fn popup_rect(&self) -> Rect {
        let h = self.items.len() as i32 * grid::ROW_H + 2;
        let below = self.rect.y() + self.rect.height() as i32;
        let y = if below + h <= grid::sh() - grid::WINDOW_INSET { below } else { (self.rect.y() - h).max(grid::BAR_H) };
        Rect::new(self.rect.x(), y, self.rect.width().max(60), h as u32)
    }

    pub fn item_rect(&self, i: usize) -> Rect {
        let p = self.popup_rect();
        Rect::new(p.x() + 1, p.y() + 1 + i as i32 * grid::ROW_H, p.width() - 2, grid::ROW_H as u32)
    }

    pub fn hit_item(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.items.len()).find(|&i| contains(self.item_rect(i), x, y))
    }

    pub fn draw_closed(&self, c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, selected: usize, st: BtnState) {
        let ink = tm.get_color("foreground");
        let surface = tm.get_color("icon_white");
        let r = self.rect;
        // Pop-up button: white face, square frame with a solid 1px shadow right and below.
        hard_shadow(c, r, ink);
        ui::fill_rect(c, r, if st.pressed { ink } else if st.hover { tm.get_color("accent_soft") } else { surface });
        ui::draw_rect(c, r, ink);
        let fg = if st.pressed { surface } else { ink };
        let label = self.items.get(selected).map(String::as_str).unwrap_or("");
        fs.draw_text(c, r.x() + 5, r.y() + (r.height() as i32 - 13) / 2, label, fg);
        // Solid down-pointing triangle, 7 wide.
        let (ax, ay) = (r.x() + r.width() as i32 - 12, r.y() + r.height() as i32 / 2 - 2);
        c.set_draw_color(fg);
        for k in 0..4 {
            let _ = c.draw_line((ax + k, ay + k), (ax + 6 - k, ay + k));
        }
    }

    pub fn draw_popup(&self, c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, selected: usize, hover: Option<usize>) {
        draw_menu_list(c, fs, tm, self.popup_rect(), &self.items.iter().map(String::as_str).collect::<Vec<_>>(), hover.unwrap_or(selected), Some(selected));
    }
}

/// An indeterminate "barber pole" bar (the same look as the launch screen): a label centred
/// above an outlined bar 12 px high with diagonal stripes, 4 px on / 4 px off, sliding right as
/// `tick` grows. `r` is 28 px high (label, then the bar from `r.y() + 16`).
pub fn draw_barber(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, r: Rect, label: &str, tick: u32) {
    let ink = tm.get_color("foreground");
    let surface = tm.get_color("widget_face_active");
    let lw = fs.measure_text(label);
    fs.draw_text(c, r.x() + (r.width() as i32 - lw) / 2, r.y(), label, ink);
    let bar = Rect::new(r.x(), r.y() + 16, r.width(), 12);
    ui::draw_rect(c, bar, ink);
    let inner = Rect::new(bar.x() + 1, bar.y() + 1, bar.width() - 2, bar.height() - 2);
    ui::fill_rect(c, inner, surface);
    c.set_draw_color(ink);
    let shift = (tick / 2) as i32;
    for y in 0..inner.height() as i32 {
        let mut x = -(y + shift).rem_euclid(8) - 8;
        while x < inner.width() as i32 {
            for k in 0..4 {
                let px = x + k;
                if px >= 0 && px < inner.width() as i32 {
                    let _ = c.draw_point((inner.x() + px, inner.y() + y));
                }
            }
            x += 8;
        }
    }
}

/// A small horizontal meter (signal strength, levels): surface track, ink outline
/// and an ink gauge inset by 2 px. Works from 6 px high; `frac` is 0.0..=1.0.
pub fn draw_meter(c: &mut Canvas, tm: &ThemeManager, r: Rect, frac: f32) {
    let ink = tm.get_color("foreground");
    ui::fill_rect(c, r, tm.get_color("widget_face_active"));
    ui::draw_rect(c, r, ink);
    let inner_w = r.width() as i32 - 4;
    let w = (inner_w as f32 * frac.clamp(0.0, 1.0)).round() as i32;
    if w > 0 && r.height() > 4 {
        ui::fill_rect(c, Rect::new(r.x() + 2, r.y() + 2, w as u32, r.height() - 4), ink);
    }
}

/// Drop shadow: a solid 1px line down the right and along the bottom, offset by 1.
pub fn hard_shadow(c: &mut Canvas, r: Rect, color: Color) {
    c.set_draw_color(color);
    let (x1, y1) = (r.x() + r.width() as i32, r.y() + r.height() as i32);
    let _ = c.draw_line((x1, r.y() + 2), (x1, y1));
    let _ = c.draw_line((r.x() + 2, y1), (x1, y1));
}

/// A framed list of choices with a solid shadow. `current` gets a check mark.
fn draw_menu_list(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, r: Rect, items: &[&str], highlight: usize, current: Option<usize>) {
    let ink = tm.get_color("foreground");
    let surface = tm.get_color("widget_face_active");
    hard_shadow(c, r, ink);
    ui::fill_rect(c, r, surface);
    ui::draw_rect(c, r, ink);
    for (i, label) in items.iter().enumerate() {
        let row = Rect::new(r.x() + 1, r.y() + 1 + i as i32 * grid::ROW_H, r.width() - 2, grid::ROW_H as u32);
        let hi = i == highlight;
        if hi {
            ui::fill_rect(c, row, ink);
        }
        let fg = if hi { surface } else { ink };
        if current == Some(i) {
            c.set_draw_color(fg);
            for (dx, dy) in [(0, 0), (1, 1), (2, 2), (3, 1), (4, 0), (5, -1), (6, -2)] {
                let _ = c.draw_point((row.x() + 4 + dx, row.y() + 8 + dy));
            }
        }
        fs.draw_text(c, row.x() + 14, row.y() + 1, label, fg);
    }
}

// ----------------------------------------------------------------- app menu

/// One entry of the menu every app has (opened with Tab, or Ctrl+Tab in text apps).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    pub id: &'static str,
    pub label: String,
}

impl MenuItem {
    pub fn new(id: &'static str, label: impl Into<String>) -> Self {
        Self { id, label: label.into() }
    }
}

/// The standard pull-down menu: app specific entries, a rule, then "Quit".
pub struct AppMenu {
    pub items: Vec<MenuItem>,
    pub cursor: usize,
    /// A context menu opens at the pointer (top-left corner, flipped up / left to stay on screen);
    /// `None` = the pull-down under the menu strip.
    pub origin: Option<(i32, i32)>,
}

pub const QUIT_ID: &str = "quit";

impl AppMenu {
    /// `app_items` followed by the universal Quit.
    pub fn new(mut app_items: Vec<MenuItem>) -> Self {
        app_items.push(MenuItem::new(QUIT_ID, "Quit"));
        Self { items: app_items, cursor: 0, origin: None }
    }

    /// A menu without the universal Quit (Home).
    pub fn plain(items: Vec<MenuItem>) -> Self {
        Self { items, cursor: 0, origin: None }
    }

    /// A context menu at the pointer: no Quit, no rule.
    pub fn context(items: Vec<MenuItem>, x: i32, y: i32) -> Self {
        Self { items, cursor: 0, origin: Some((x, y)) }
    }

    /// True when the last entry is the universal Quit (a rule is drawn above it).
    fn has_rule(&self) -> bool {
        self.items.len() > 1 && self.items.last().map_or(false, |i| i.id == QUIT_ID)
    }

    /// Rows that fit under the menu strip.
    fn max_rows() -> usize {
        ((grid::sh() - grid::BAR_H - grid::WINDOW_INSET - 6) / grid::ROW_H) as usize
    }
    /// One column normally; a long menu (more rows than fit on the screen) is folded into two
    /// columns, filled top to bottom, the left column first. Quit stays last.
    pub fn columns(&self) -> usize {
        if self.items.len() > Self::max_rows() { 2 } else { 1 }
    }

    fn rows_per_column(&self) -> usize {
        self.items.len().div_ceil(self.columns())
    }

    fn column_width(&self, fs: &FontSystem) -> i32 {
        (self.items.iter().map(|i| fs.measure_text(&i18n::tr_s(&i.label))).max().unwrap_or(40) + 28).max(88)
    }

    /// Anchored under the menu strip at the left, like a Mac pull-down.
    pub fn rect(&self, fs: &FontSystem) -> Rect {
        let w = self.column_width(fs) * self.columns() as i32 + 1;
        let h = self.rows_per_column() as i32 * grid::ROW_H + 2 + if self.has_rule() { 4 } else { 0 };
        match self.origin {
            None => Rect::new(grid::MARGIN, grid::BAR_H, w as u32, h as u32),
            Some((px, py)) => {
                let (right, bottom) = (grid::sw() - grid::WINDOW_INSET, grid::sh() - grid::WINDOW_INSET);
                let x = if px + w <= right { px } else { px - w };
                let y = if py + h <= bottom { py } else { py - h };
                Rect::new(x.max(0), y.max(grid::BAR_H), w as u32, h as u32)
            }
        }
    }

    fn row_rect(&self, fs: &FontSystem, i: usize) -> Rect {
        let r = self.rect(fs);
        let rows = self.rows_per_column();
        let (col, row) = (i / rows, i % rows);
        let rule = if i + 1 == self.items.len() && self.has_rule() { 4 } else { 0 };
        let cw = self.column_width(fs);
        Rect::new(r.x() + 1 + col as i32 * cw, r.y() + 1 + row as i32 * grid::ROW_H + rule, (cw - 2) as u32, grid::ROW_H as u32)
    }

    pub fn hit(&self, fs: &FontSystem, x: i32, y: i32) -> Option<usize> {
        (0..self.items.len()).find(|&i| contains(self.row_rect(fs, i), x, y))
    }

    pub fn up(&mut self) {
        self.cursor = if self.cursor == 0 { self.items.len() - 1 } else { self.cursor - 1 };
    }

    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1) % self.items.len();
    }

    /// Left / Right: the same row in the other column (the last item when that row is empty).
    pub fn sideways(&mut self, dir: i32) {
        let cols = self.columns();
        if cols < 2 {
            return;
        }
        let rows = self.rows_per_column();
        let (col, row) = (self.cursor / rows, self.cursor % rows);
        let ncol = (col as i32 + dir).clamp(0, cols as i32 - 1) as usize;
        self.cursor = (ncol * rows + row).min(self.items.len() - 1);
    }

    pub fn selected(&self) -> &MenuItem {
        &self.items[self.cursor.min(self.items.len() - 1)]
    }

    pub fn draw(&self, c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        let surface = tm.get_color("widget_face_active");
        let line = tm.get_color("line");
        let r = self.rect(fs);
        dither::fill(c, Rect::new(r.x() + 2, r.y() + 2, r.width(), r.height()), tm.get_color("widget_shadow"), Pattern::Half);
        ui::fill_rect(c, r, surface);
        ui::draw_rect(c, r, ink);
        if self.columns() == 2 {
            let x = r.x() + self.column_width(fs);
            ui::line(c, x, r.y() + 2, x, r.bottom() - 3, line);
        }
        for (i, item) in self.items.iter().enumerate() {
            let row = self.row_rect(fs, i);
            if i + 1 == self.items.len() && self.has_rule() {
                ui::line(c, row.x() + 2, row.y() - 3, row.right() - 3, row.y() - 3, line);
            }
            let hi = i == self.cursor;
            if hi {
                ui::fill_rect(c, row, ink);
            }
            fs.draw_text(c, row.x() + 8, row.y() + 1, &i18n::tr_s(&item.label), if hi { surface } else { ink });
        }
    }
}

// ------------------------------------------------------------------ tree list

/// One row of a [`TreeList`]. `key` is the caller's own id for the node (the loader maps it to
/// the node's children); everything else is presentation.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TreeNode {
    pub key: String,
    pub label: String,
    /// Right-aligned secondary text (a speed, a count).
    pub extra: String,
    pub depth: usize,
    /// May have children, fetched the first time the node is opened. `false` = a leaf. A node
    /// that turns out to have none becomes a leaf when it is opened.
    pub expandable: bool,
    pub expanded: bool,
    /// Faint text (disabled, gone).
    pub dim: bool,
    /// Highlight band behind the row (just plugged in).
    pub hot: bool,
    /// A property row: a leaf drawn as `name ... value` (no marker, name in the faint tone, value
    /// right-aligned in full ink). The value is `extra`.
    pub prop: bool,
    /// A property whose value is an address: `value` keeps the real text, the caller puts the
    /// shown form into `extra`.
    pub secret: bool,
    pub value: String,
}

impl TreeNode {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self { key: key.into(), label: label.into(), ..Default::default() }
    }
    pub fn expandable(mut self, yes: bool) -> Self {
        self.expandable = yes;
        self
    }
    pub fn dim(mut self, yes: bool) -> Self {
        self.dim = yes;
        self
    }
    pub fn hot(mut self, yes: bool) -> Self {
        self.hot = yes;
        self
    }
    pub fn extra(mut self, text: impl Into<String>) -> Self {
        self.extra = text.into();
        self
    }
    /// A property row `name = value` (a leaf).
    pub fn property(key: impl Into<String>, name: impl Into<String>, value: impl Into<String>) -> Self {
        Self { key: key.into(), label: name.into(), extra: value.into(), prop: true, ..Default::default() }
    }
    pub fn secret(mut self, yes: bool) -> Self {
        self.secret = yes;
        if yes {
            self.value = self.extra.clone();
        }
        self
    }
}

/// What a key did to a [`TreeList`]: `Ignored` lets the caller use it (Left on a collapsed
/// top-level node, Right on a leaf: a screen with tabs switches page).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeKey {
    Used,
    Ignored,
}

/// A collapsible tree in the style of a device manager: 16 px rows on the row grid, 4 px of
/// indent per level, a `+` / `-` marker for nodes that can open, the theme selection on the
/// cursor row, a 1 px rule under every row. Children come from a loader the caller passes in
/// (`FnMut(&TreeNode) -> Vec<TreeNode>`, depth is set here), called when a node is opened.
pub struct TreeList {
    pub rows: Vec<TreeNode>,
    pub cursor: usize,
    pub scroll: usize,
    pub visible: usize,
    /// Characters the value of the cursor row is scrolled by (a value too long for its row runs
    /// through the row so that all of it can be read); see [`TreeList::marquee`].
    pub shift: usize,
    shift_for: usize,
    /// Width a property row has for marker + name + value; [`PROP_ROW_W`] for the 302 px list. A
    /// screen whose list is wider (a big window) sets it to follow the list.
    pub prop_w: i32,
}

pub const TREE_ROW_H: i32 = grid::ROW_H;
pub const TREE_INDENT: i32 = 4;
const TREE_MARK_W: i32 = 10;
/// Width of a property row (302 px list) left for marker + name + value.
pub const PROP_ROW_W: i32 = 295;

/// Widths for a property row `row_w` wide (see [`TreeList::prop_w`]): (name, value), the name at
/// most 45% of the row.
fn prop_split(fs: &FontSystem, n: &TreeNode, depth_px: i32, row_w: i32) -> (i32, i32) {
    let avail = row_w - depth_px - 10;
    let name_w = fs.measure_text(&n.label).min(avail * 45 / 100).max(8);
    (name_w, avail - name_w - 8)
}

impl TreeList {
    pub fn new(roots: Vec<TreeNode>, visible: usize) -> Self {
        Self { rows: roots, cursor: 0, scroll: 0, visible, shift: 0, shift_for: usize::MAX, prop_w: PROP_ROW_W }
    }

    pub fn selected(&self) -> Option<&TreeNode> {
        self.rows.get(self.cursor)
    }

    /// Clamp the cursor (the list may have shrunk) and keep it inside the window.
    pub fn sync(&mut self) {
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        self.scroll = follow_scroll(self.cursor, self.scroll, self.visible, self.rows.len());
    }

    /// Call about every 300 ms: scrolls the value of the cursor row when it does not fit. It
    /// starts over (after a pause at the end) and resets when the cursor moves.
    pub fn marquee(&mut self, fs: &FontSystem) {
        if self.shift_for != self.cursor {
            self.shift_for = self.cursor;
            self.shift = 0;
            return;
        }
        let Some(n) = self.selected().filter(|n| n.prop) else { return };
        let len = n.extra.chars().count();
        let (_, vw) = prop_split(fs, n, 0, self.prop_w);
        if fs.measure_text(&n.extra) <= vw {
            self.shift = 0;
            return;
        }
        // Past the end by a few ticks, then back to the start.
        self.shift = if self.shift >= len + 3 { 0 } else { self.shift + 1 };
    }

    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.rows.iter().position(|r| r.key == key)
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
        self.sync();
    }

    /// Open row `i`. False when it is a leaf, already open, or has no children after all.
    pub fn expand(&mut self, i: usize, load: &mut dyn FnMut(&TreeNode) -> Vec<TreeNode>) -> bool {
        let Some(r) = self.rows.get(i) else { return false };
        if !r.expandable || r.expanded {
            return false;
        }
        let depth = r.depth + 1;
        let mut kids = load(&self.rows[i]);
        if kids.is_empty() {
            self.rows[i].expandable = false;
            return false;
        }
        kids.iter_mut().for_each(|k| k.depth = depth);
        self.rows[i].expanded = true;
        self.rows.splice(i + 1..i + 1, kids);
        true
    }

    pub fn collapse(&mut self, i: usize) -> bool {
        let Some(r) = self.rows.get(i) else { return false };
        if !r.expanded {
            return false;
        }
        let depth = r.depth;
        let end = self.rows[i + 1..].iter().position(|x| x.depth <= depth).map_or(self.rows.len(), |p| i + 1 + p);
        self.rows.drain(i + 1..end);
        self.rows[i].expanded = false;
        self.sync();
        true
    }

    pub fn toggle(&mut self, i: usize, load: &mut dyn FnMut(&TreeNode) -> Vec<TreeNode>) -> bool {
        self.collapse(i) || self.expand(i, load)
    }

    pub fn parent_of(&self, i: usize) -> Option<usize> {
        let d = self.rows.get(i)?.depth;
        if d == 0 {
            return None;
        }
        (0..i).rev().find(|&j| self.rows[j].depth < d)
    }

    pub fn collapse_all(&mut self) {
        let mut i = 0;
        while i < self.rows.len() {
            self.collapse(i);
            i += 1;
        }
        self.cursor = 0;
        self.sync();
    }

    /// Re-read the children of node `key` (data changed underneath), keeping the nodes below it
    /// that were open, open, and the cursor on the same node where it still exists.
    pub fn reload(&mut self, key: &str, load: &mut dyn FnMut(&TreeNode) -> Vec<TreeNode>) {
        let Some(i) = self.index_of(key) else { return };
        if !self.rows[i].expanded {
            return;
        }
        let depth = self.rows[i].depth;
        let end = self.rows[i + 1..].iter().position(|x| x.depth <= depth).map_or(self.rows.len(), |p| i + 1 + p);
        let open: Vec<String> = self.rows[i + 1..end].iter().filter(|r| r.expanded).map(|r| r.key.clone()).collect();
        let cursor_key = self.selected().map(|r| r.key.clone());
        self.collapse(i);
        self.expand(i, load);
        for k in open {
            if let Some(j) = self.index_of(&k) {
                self.expand(j, load);
            }
        }
        if let Some(j) = cursor_key.and_then(|k| self.index_of(&k)) {
            self.cursor = j;
        }
        self.sync();
    }

    /// Up/Down/PageUp/PageDown/Home/End move the cursor, Return opens / closes the node. Left and
    /// Right are not the tree's (a screen with tabs uses them to switch tab). `Ignored` = not for the tree.
    pub fn key(&mut self, key: Keycode, load: &mut dyn FnMut(&TreeNode) -> Vec<TreeNode>) -> TreeKey {
        let i = self.cursor;
        let page = self.visible.max(1) as isize;
        match key {
            Keycode::Up => self.move_cursor(-1),
            Keycode::Down => self.move_cursor(1),
            Keycode::PageUp => self.move_cursor(-page),
            Keycode::PageDown => self.move_cursor(page),
            Keycode::Home => self.move_cursor(isize::MIN / 2),
            Keycode::End => self.move_cursor(isize::MAX / 2),
            Keycode::Return => {
                if !self.rows.get(i).map_or(false, |r| r.expandable || r.expanded) {
                    return TreeKey::Ignored;
                }
                self.toggle(i, load);
            }
            _ => return TreeKey::Ignored,
        }
        TreeKey::Used
    }

    fn marker_right(&self, row: &TreeNode, rect: Rect) -> i32 {
        rect.x() + 4 + row.depth as i32 * TREE_INDENT + TREE_MARK_W
    }

    /// Mouse: click selects, a double click or a click on the marker toggles, the wheel moves the
    /// cursor. `rect` is where [`TreeList::draw`] draws. True when the event was used.
    pub fn pointer(&mut self, ev: PointerEvent, rect: Rect, load: &mut dyn FnMut(&TreeNode) -> Vec<TreeNode>) -> bool {
        match ev {
            PointerEvent::Wheel { dy, .. } => {
                self.move_cursor(if dy > 0 { -1 } else { 1 });
                true
            }
            PointerEvent::Down { x, y, button: 0, clicks } if contains(rect, x, y) => {
                let idx = self.scroll + ((y - rect.y()) / TREE_ROW_H) as usize;
                if idx >= self.rows.len() {
                    return true;
                }
                self.cursor = idx;
                self.sync();
                let on_marker = x < self.marker_right(&self.rows[idx], rect);
                if clicks >= 2 || (on_marker && self.rows[idx].expandable) {
                    self.toggle(idx, load);
                }
                true
            }
            _ => false,
        }
    }

    /// Scroll so that row `offset` is the first one shown (clamped); the cursor is pulled into the
    /// window so that it does not scroll the list back.
    pub fn set_scroll(&mut self, offset: usize) {
        let max = self.rows.len().saturating_sub(self.visible);
        self.scroll = offset.min(max);
        if self.cursor < self.scroll {
            self.cursor = self.scroll;
        } else if self.cursor >= self.scroll + self.visible {
            self.cursor = self.scroll + self.visible - 1;
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    /// The scroll bar for a column of `sb` (always drawn by the caller).
    pub fn scrollbar(&self, sb: Rect) -> ScrollBar {
        ScrollBar::new(sb, self.rows.len(), self.visible, self.scroll)
    }

    /// Draw `visible` rows into `rect` (rows are 16 px; the area below the last row stays empty).
    pub fn draw(&self, c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, rect: Rect) {
        let ink = tm.get_color("foreground");
        let faint = secondary_ink(tm);
        let rule = tm.get_color("line");
        let alt = tm.get_color("surface_alt");
        for (i, node) in self.rows.iter().enumerate().skip(self.scroll).take(self.visible) {
            let row = Rect::new(rect.x(), rect.y() + (i - self.scroll) as i32 * TREE_ROW_H, rect.width(), TREE_ROW_H as u32);
            if i % 2 == 1 {
                ui::fill_rect(c, row, alt);
            }
            if node.hot {
                ui::fill_rect(c, Rect::new(row.x(), row.y(), row.width(), row.height() - 1), rule);
            }
            ui::line(c, row.x(), row.bottom() - 1, row.right() - 1, row.bottom() - 1, rule);
            let sel = i == self.cursor;
            if sel {
                draw_row_plate(c, tm, row);
            }
            let fg = if sel { selection(tm).fg } else if node.dim { faint } else { ink };
            let x = row.x() + 4 + node.depth as i32 * TREE_INDENT;
            if node.prop {
                // name left (faint), value right-aligned (ink); a long value is cut, or scrolled on the cursor row
                let lx = x + TREE_MARK_W - 1;
                let (nw, vw) = prop_split(fs, node, node.depth as i32 * TREE_INDENT, self.prop_w);
                let name = crate::chrome::fit_text(fs, &node.label, nw);
                draw_row_text(c, fs, tm, row, lx, &name, sel, if sel { fg } else { faint });
                let full = fs.measure_text(&node.extra);
                let (text, right) = if full <= vw {
                    (node.extra.clone(), true)
                } else if sel && self.shift > 0 {
                    let tail: String = node.extra.chars().skip(self.shift).collect();
                    (crate::chrome::fit_text(fs, &tail, vw), false)
                } else {
                    (crate::chrome::fit_text(fs, &node.extra, vw), false)
                };
                let w = fs.measure_text(&text);
                let vx = if right { row.right() - 4 - w } else { row.right() - 4 - vw };
                draw_row_text(c, fs, tm, row, vx, &text, sel, fg);
                continue;
            }
            let mark = if node.expanded { "-" } else if node.expandable { "+" } else { "" };
            draw_row_text(c, fs, tm, row, x, mark, sel, fg);
            let extra_w = if node.extra.is_empty() { 0 } else { fs.measure_text(&node.extra) + 8 };
            let lx = x + TREE_MARK_W - 1;
            let label = crate::chrome::fit_text(fs, &node.label, (row.right() - 4 - extra_w) - lx);
            draw_row_text(c, fs, tm, row, lx, &label, sel, fg);
            if !node.extra.is_empty() {
                let w = fs.measure_text(&node.extra);
                draw_row_text(c, fs, tm, row, row.right() - 4 - w, &node.extra, sel, if sel { fg } else { faint });
            }
        }
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    /// root -> a (-> a1, a2 (-> a2x)), b, c (leaf)
    fn kids(n: &TreeNode) -> Vec<TreeNode> {
        let mk = |k: &str, exp: bool| TreeNode::new(k, k).expandable(exp);
        match n.key.as_str() {
            "a" => vec![mk("a1", false), mk("a2", true)],
            "a2" => vec![mk("a2x", false)],
            "b" => vec![],
            _ => vec![],
        }
    }

    fn list() -> TreeList {
        TreeList::new(vec![TreeNode::new("a", "a").expandable(true), TreeNode::new("b", "b").expandable(true), TreeNode::new("c", "c")], 3)
    }

    #[test]
    fn children_load_when_first_opened_and_depth_is_set() {
        let calls = std::cell::RefCell::new(Vec::new());
        let mut load = |n: &TreeNode| {
            calls.borrow_mut().push(n.key.clone());
            kids(n)
        };
        let mut t = list();
        assert!(calls.borrow().is_empty());
        assert!(t.expand(0, &mut load));
        assert!(!t.expand(0, &mut load)); // already open
        assert_eq!(t.rows.iter().map(|r| (r.key.as_str(), r.depth)).collect::<Vec<_>>(), vec![("a", 0), ("a1", 1), ("a2", 1), ("b", 0), ("c", 0)]);
        assert!(!t.expand(1, &mut load)); // a leaf
        assert_eq!(*calls.borrow(), vec!["a".to_string()]);
        // An expandable node without children becomes a leaf.
        let b = t.index_of("b").unwrap();
        assert!(!t.expand(b, &mut load));
        assert!(!t.rows[b].expandable);
    }

    #[test]
    fn collapse_removes_the_whole_subtree_and_keeps_the_cursor_valid() {
        let mut load = |n: &TreeNode| kids(n);
        let mut t = list();
        t.expand(0, &mut load);
        t.expand(2, &mut load);
        assert_eq!(t.rows.len(), 6);
        t.cursor = 3; // a2x
        assert!(t.collapse(0));
        assert_eq!(t.rows.len(), 3);
        assert!(t.cursor < t.rows.len());
        assert!(!t.collapse(0));
    }

    #[test]
    fn keys_move_and_return_toggles_left_right_are_not_the_trees() {
        let mut load = |n: &TreeNode| kids(n);
        let mut t = list();
        // Left / Right never touch the tree (a tabbed screen switches tab with them).
        assert_eq!(t.key(Keycode::Left, &mut load), TreeKey::Ignored);
        assert_eq!(t.key(Keycode::Right, &mut load), TreeKey::Ignored);
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.key(Keycode::Return, &mut load), TreeKey::Used); // opens a
        assert_eq!(t.rows.len(), 5);
        assert_eq!(t.key(Keycode::Down, &mut load), TreeKey::Used);
        assert_eq!(t.cursor, 1);
        assert_eq!(t.key(Keycode::Right, &mut load), TreeKey::Ignored); // still not
        assert_eq!(t.key(Keycode::Left, &mut load), TreeKey::Ignored);
        assert_eq!(t.cursor, 1);
        t.key(Keycode::Up, &mut load);
        assert_eq!(t.key(Keycode::Return, &mut load), TreeKey::Used); // toggles closed
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.key(Keycode::Return, &mut load), TreeKey::Used);
        assert_eq!(t.rows.len(), 5);
        t.key(Keycode::Return, &mut load);
        t.key(Keycode::End, &mut load);
        assert_eq!(t.cursor, 2);
        assert_eq!(t.key(Keycode::Return, &mut load), TreeKey::Ignored); // a leaf
        t.key(Keycode::Down, &mut load);
        assert_eq!(t.cursor, 2); // clamped
        t.key(Keycode::Home, &mut load);
        assert_eq!(t.cursor, 0);
        t.key(Keycode::Up, &mut load);
        assert_eq!(t.cursor, 0);
        assert_eq!(t.key(Keycode::A, &mut load), TreeKey::Ignored);
    }

    #[test]
    fn the_window_follows_the_cursor_and_pages() {
        let mut load = |_: &TreeNode| Vec::new();
        let rows = (0..10).map(|i| TreeNode::new(format!("n{i}"), format!("n{i}"))).collect();
        let mut t = TreeList::new(rows, 4);
        t.key(Keycode::PageDown, &mut load);
        assert_eq!((t.cursor, t.scroll), (4, 1));
        t.key(Keycode::End, &mut load);
        assert_eq!((t.cursor, t.scroll), (9, 6));
        t.key(Keycode::PageUp, &mut load);
        assert_eq!((t.cursor, t.scroll), (5, 5));
        t.rows.truncate(3); // the list shrinks
        t.sync();
        assert_eq!((t.cursor, t.scroll), (2, 0));
    }

    #[test]
    fn reload_keeps_open_nodes_and_the_cursor() {
        let mut load = |n: &TreeNode| kids(n);
        let mut t = list();
        t.expand(0, &mut load);
        t.expand(2, &mut load); // a2 open
        t.cursor = t.index_of("a2x").unwrap();
        let mut load2 = |n: &TreeNode| {
            let mut v = kids(n);
            if n.key == "a" {
                v.insert(0, TreeNode::new("a0", "a0"));
            }
            v
        };
        t.reload("a", &mut load2);
        assert!(t.index_of("a0").is_some());
        assert!(t.rows[t.index_of("a2").unwrap()].expanded);
        assert_eq!(t.selected().unwrap().key, "a2x");
    }

    #[test]
    fn mouse_click_double_click_marker_and_wheel() {
        let mut load = |n: &TreeNode| kids(n);
        let mut t = list();
        let rect = Rect::new(4, 32, 120, 48);
        let down = |x, y, clicks| PointerEvent::Down { x, y, button: 0, clicks };
        // click on the label of row 2 (c)
        assert!(t.pointer(down(60, 32 + 2 * 16 + 4, 1), rect, &mut load));
        assert_eq!(t.cursor, 2);
        // click on the marker of row 0 opens it
        assert!(t.pointer(down(6, 36, 1), rect, &mut load));
        assert_eq!((t.cursor, t.rows.len()), (0, 5));
        // double click on the label closes it again
        assert!(t.pointer(down(60, 36, 2), rect, &mut load));
        assert_eq!(t.rows.len(), 3);
        // outside the rect / below the last row
        assert!(!t.pointer(down(200, 36, 1), rect, &mut load));
        // wheel moves the cursor
        t.pointer(PointerEvent::Wheel { x: 10, y: 40, dy: -1 }, rect, &mut load);
        assert_eq!(t.cursor, 1);
        t.pointer(PointerEvent::Wheel { x: 10, y: 40, dy: 1 }, rect, &mut load);
        assert_eq!(t.cursor, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::{folder_widths, rule_ink, tones, folder_room};

    #[test]
    fn a_folder_rule_always_stands_out_from_the_page_under_it() {
        use crate::theme::{Theme, ThemeManager};
        let lum = |c: crate::gfx::Color| c.r as i32 * 3 + c.g as i32 * 6 + c.b as i32;
        for name in ["black", "paper", "mobile", "terminal"] {
            for inv in ["", "-invert"] {
                let tm = ThemeManager::new(Theme::load_from_yaml(&format!("assets/themes/theme-{name}{inv}.yaml")).unwrap());
                let t = tones(&tm);
                let range = (lum(t.light) - lum(t.dark)).abs();
                // On the usual page (home_bg) the rule is the ink, as before.
                assert_eq!(rule_ink(&tm, tm.get_color("home_bg")), tm.get_color("foreground"), "{name}{inv}");
                // On a light and on a dark face it is at least half the tone range away.
                for page in [t.light, t.dark] {
                    let r = rule_ink(&tm, page);
                    assert!((lum(r) - lum(page)).abs() * 2 >= range, "{name}{inv}: rule {r:?} on page {page:?}");
                }
            }
        }
    }

    #[test]
    fn folder_tabs_are_equal_when_the_widest_label_fits() {
        assert_eq!(folder_widths(&[30, 58, 40], folder_room()), None);
    }

    #[test]
    fn folder_tabs_grow_per_label_when_one_would_be_cut() {
        // Monitor in Japanese: the 6 tabs get 51 px each at most, ディスク needs 58.
        let needs = [34, 32, 46, 34, 34, 58];
        let w = folder_widths(&needs, folder_room()).expect("per-tab widths");
        for (k, need) in needs.iter().enumerate() {
            assert!(w[k] >= *need, "tab {k} is {} px, needs {need}", w[k]);
        }
        let total: i32 = w[..6].iter().sum();
        assert_eq!(total, folder_room() - 1, "the spare room is shared out exactly");
    }

    #[test]
    fn row_rule_is_the_last_pixel_row_of_a_row() {
        let row = Rect::new(4, 32, 280, 16);
        let r = row_rule_rect(row);
        assert_eq!((r.x(), r.y(), r.width(), r.height()), (4, 47, 280, 1));
        // The selection plate is one pixel shorter, so the rule is never covered by it.
        let plate_bottom = row.y() + row.height() as i32 - 1;
        assert_eq!(r.y(), plate_bottom);
    }

    #[test]
    fn a_long_menu_folds_into_two_columns_with_quit_last() {
        use super::*;
        let fs = FontSystem::new();
        let few = AppMenu::new((0..3).map(|i| MenuItem::new("x", &format!("Item {i}"))).collect());
        assert_eq!(few.columns(), 1);
        let mut many = AppMenu::new((0..14).map(|i| MenuItem::new("x", &format!("Item {i}"))).collect());
        assert_eq!(many.columns(), 2);
        let r = many.rect(&fs);
        assert!(r.bottom() <= grid::sh(), "the menu fits on the screen");
        // 15 items: column 0 holds 8, column 1 holds 7 (the last is Quit).
        assert_eq!(many.hit(&fs, many.row_rect(&fs, 0).x() + 3, many.row_rect(&fs, 0).y() + 3), Some(0));
        let last = many.row_rect(&fs, 14);
        assert!(last.x() > many.row_rect(&fs, 0).x(), "Quit is in the second column");
        assert_eq!(many.hit(&fs, last.x() + 3, last.y() + 3), Some(14));
        many.cursor = 2;
        many.sideways(1);
        assert_eq!(many.cursor, 10);
        many.sideways(-1);
        assert_eq!(many.cursor, 2);
        many.cursor = 7;
        many.sideways(1);
        assert_eq!(many.cursor, 14, "the last row of column 1 exists");
    }

    #[test]
    fn the_scroll_offset_follows_the_cursor_without_jumping() {
        use super::follow_scroll;
        assert_eq!(follow_scroll(0, 0, 8, 20), 0);
        assert_eq!(follow_scroll(7, 0, 8, 20), 0, "still inside the window");
        assert_eq!(follow_scroll(8, 0, 8, 20), 1, "one row past the bottom scrolls by one");
        assert_eq!(follow_scroll(2, 5, 8, 20), 2, "above the window: the cursor is the top row");
        assert_eq!(follow_scroll(19, 0, 8, 20), 12, "never past the last full page");
        assert_eq!(follow_scroll(3, 9, 8, 5), 0, "a short list does not scroll");
    }

    #[test]
    fn text_sits_centred_in_a_row_and_the_plate_is_symmetric_around_the_capitals() {
        use super::{text_origin, BASELINE, CAP_TOP};
        // A 16 px row: capitals are 9 rows, so 3 blank rows above and 4 below.
        let y = text_origin(100, 16);
        let (top, bottom) = (y + CAP_TOP, y + BASELINE);
        assert_eq!(top - 100, 3);
        assert_eq!((100 + 16 - 1) - bottom, 4);
        // The plate is one pixel shorter than the row: 3 above the capitals, 3 below the baseline.
        let plate_bottom = 100 + 15 - 1;
        assert_eq!(plate_bottom - bottom, 3);
        // An 18 px row splits 4 / 5 with the odd pixel below.
        let y18 = text_origin(0, 18);
        assert_eq!(y18 + CAP_TOP, 4);
    }
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_second_press_close_by_is_a_double_click() {
        let mut p = Pointer::new(100, 50);
        let now = t0();
        assert_eq!(p.button(0, true, now), PointerEvent::Down { x: 100, y: 50, button: 0, clicks: 1 });
        p.button(0, false, now);
        let e = p.button(0, true, now + Duration::from_millis(200));
        assert_eq!(e, PointerEvent::Down { x: 100, y: 50, button: 0, clicks: 2 });
        p.button(0, false, now);
        // The double click is consumed: a third press is a fresh single click.
        let e = p.button(0, true, now + Duration::from_millis(300));
        assert_eq!(e, PointerEvent::Down { x: 100, y: 50, button: 0, clicks: 1 });
    }

    #[test]
    fn slow_or_distant_second_clicks_are_single_clicks() {
        let mut p = Pointer::new(100, 50);
        let now = t0();
        p.button(0, true, now);
        p.button(0, false, now);
        let e = p.button(0, true, now + Duration::from_millis(700));
        assert_eq!(e, PointerEvent::Down { x: 100, y: 50, button: 0, clicks: 1 }, "too slow");
        p.button(0, false, now);
        p.moved(30, 0);
        let e = p.button(0, true, now + Duration::from_millis(750));
        assert!(matches!(e, PointerEvent::Down { clicks: 1, .. }), "moved too far");
    }

    #[test]
    fn motion_is_a_drag_only_while_the_left_button_is_down() {
        let mut p = Pointer::new(10, 10);
        assert_eq!(p.moved(5, 0), PointerEvent::Move { x: 15, y: 10 });
        p.button(0, true, t0());
        assert_eq!(p.moved(5, 3), PointerEvent::Drag { x: 20, y: 13, dx: 5, dy: 3 });
        p.button(0, false, t0());
        assert_eq!(p.moved(1, 1), PointerEvent::Move { x: 21, y: 14 });
        // Clamped to the screen.
        assert_eq!(p.moved(-999, -999), PointerEvent::Move { x: 0, y: 0 });
        assert_eq!(p.moved(999, 999), PointerEvent::Move { x: 319, y: 169 });
    }

    #[test]
    fn right_button_does_not_start_a_drag() {
        let mut p = Pointer::new(10, 10);
        assert!(matches!(p.button(1, true, t0()), PointerEvent::Down { button: 1, clicks: 1, .. }));
        assert!(p.right && !p.left);
        assert!(matches!(p.moved(2, 2), PointerEvent::Move { .. }));
    }

    #[test]
    fn slider_maps_pixels_to_values_both_ways() {
        let s = Slider::new(Rect::new(100, 40, 108, 8));
        assert_eq!(s.value_at(0), 0);
        assert_eq!(s.value_at(1000), 100);
        assert_eq!(s.value_at(100 + THUMB_W / 2 + 50), 50, "the thumb centre follows the pointer");
        for v in [0u32, 25, 50, 75, 100] {
            let t = s.thumb(v);
            assert_eq!(s.value_at(t.x() + THUMB_W / 2), v, "round trip {v}");
        }
        assert!(s.hit(120, 44));
        assert!(!s.hit(120, 70));
        assert!(!s.hit(50, 44));
    }

    fn sb(offset: usize) -> ScrollBar {
        ScrollBar::new(Rect::new(312, 20, 4, 140), 40, 10, offset)
    }

    #[test]
    fn scroll_bar_parts_are_hit_in_order() {
        let b = sb(0);
        assert_eq!(b.hit(313, 21), Some(SbPart::Up));
        assert_eq!(b.hit(313, 158), Some(SbPart::Down));
        let th = b.thumb();
        assert_eq!(b.hit(313, th.y() + 2), Some(SbPart::Thumb));
        assert_eq!(b.hit(313, th.y() + th.height() as i32 + 5), Some(SbPart::PageDown));
        assert_eq!(b.hit(100, 100), None, "outside the bar");
        let b = sb(30);
        let th = b.thumb();
        assert_eq!(b.hit(313, th.y() - 3), Some(SbPart::PageUp));
        assert_eq!(ScrollBar::new(Rect::new(312, 20, 4, 140), 5, 10, 0).hit(313, 80), None, "nothing to scroll");
    }

    #[test]
    fn dragging_the_thumb_maps_back_to_offsets() {
        for off in [0usize, 7, 15, 30] {
            let b = sb(off);
            let th = b.thumb();
            let got = b.offset_for_y(th.y() + th.height() as i32 / 2);
            assert!((got as i32 - off as i32).abs() <= 1, "offset {off} -> {got}");
        }
        let b = sb(0);
        assert_eq!(b.offset_for_y(-100), 0);
        assert_eq!(b.offset_for_y(1000), 30, "the last page");
        // The thumb never leaves the track.
        let th = sb(30).thumb();
        assert!(th.y() + th.height() as i32 <= 20 + 140 - 4);
    }

    #[test]
    fn dropdown_opens_downward_or_flips_up_near_the_bottom() {
        let items: Vec<String> = ["Never", "10 sec", "30 sec", "1 min", "5 min"].iter().map(|s| s.to_string()).collect();
        let d = Dropdown::new(Rect::new(180, 40, 80, 16), items.clone());
        let p = d.popup_rect();
        assert_eq!(p.y(), 56, "drops below the button");
        assert_eq!(d.hit_item(200, 56 + 1 + 2 * 16 + 3), Some(2));
        assert_eq!(d.hit_item(200, 5), None);
        let low = Dropdown::new(Rect::new(180, 140, 80, 16), items);
        let p = low.popup_rect();
        assert!(p.y() + p.height() as i32 <= 140, "flips above the button: {p:?}");
    }

    #[test]
    fn the_app_menu_always_ends_with_quit_and_wraps() {
        let mut m = AppMenu::new(vec![MenuItem::new("settings", "Settings..."), MenuItem::new("clear", "Clear")]);
        assert_eq!(m.items.len(), 3);
        assert_eq!(m.items.last().unwrap().id, QUIT_ID);
        assert_eq!(m.selected().id, "settings");
        m.up();
        assert_eq!(m.selected().id, QUIT_ID, "Up from the top wraps to Quit");
        m.down();
        assert_eq!(m.selected().id, "settings");
        let only_quit = AppMenu::new(vec![]);
        assert_eq!(only_quit.items.len(), 1);
        assert_eq!(only_quit.selected().id, QUIT_ID);
    }

    #[test]
    fn tones_are_the_lighter_and_darker_of_ink_and_paper_on_every_theme() {
        for name in ["black", "paper", "mobile", "terminal", "black-invert", "paper-invert", "mobile-invert", "terminal-invert"] {
            let path = format!("{}/assets/themes/theme-{name}.yaml", env!("CARGO_MANIFEST_DIR"));
            let tm = ThemeManager::new(crate::theme::Theme::load_from_yaml(&path).unwrap());
            let t = tones(&tm);
            let lum = |c: Color| c.r as u32 * 3 + c.g as u32 * 6 + c.b as u32;
            assert!(lum(t.light) > lum(t.dark), "{name}: light must be lighter than dark");
        }
    }
}

#[cfg(test)]
mod scrollbar_press_tests {
    use super::*;

    fn bar(offset: usize) -> ScrollBar {
        ScrollBar::new(Rect::new(312, 20, 4, 140), 40, 10, offset)
    }

    #[test]
    fn press_steps_pages_and_starts_a_drag() {
        let b = bar(12);
        assert_eq!(b.press(313, 21), Some((11, false)), "up arrow: one row");
        assert_eq!(b.press(313, 158), Some((13, false)), "down arrow: one row");
        let th = b.thumb();
        assert_eq!(b.press(313, th.y() - 2), Some((2, false)), "track above: a page up");
        assert_eq!(b.press(313, th.y() + th.height() as i32 + 2), Some((22, false)), "track below: a page down");
        assert_eq!(b.press(313, th.y() + 1), Some((12, true)), "thumb: a drag starts");
        assert_eq!(b.press(100, 100), None, "elsewhere");
        assert_eq!(bar(0).press(313, 21), Some((0, false)), "clamped at the top");
        assert_eq!(bar(30).press(313, 158), Some((30, false)), "clamped at the bottom");
        assert_eq!(bar(28).press(313, 150), Some((30, false)), "a page down stops at the end");
    }

    #[test]
    fn drag_maps_the_pointer_to_offsets_monotonically() {
        let b = bar(0);
        assert_eq!(b.offset_for_y(0), 0);
        assert_eq!(b.offset_for_y(1000), 30);
        let mut last = 0;
        for y in (20..160).step_by(5) {
            let o = b.offset_for_y(y);
            assert!(o >= last && o <= 30);
            last = o;
        }
    }
}

#[cfg(test)]
mod context_menu_tests {
    use super::*;
    use crate::font::FontSystem;

    fn items(n: usize) -> Vec<MenuItem> {
        (0..n).map(|i| MenuItem::new("x", format!("Item {i}"))).collect()
    }

    #[test]
    fn a_context_menu_opens_at_the_pointer_and_stays_on_screen() {
        let fs = FontSystem::new();
        let m = AppMenu::context(items(3), 100, 60);
        let r = m.rect(&fs);
        assert_eq!((r.x(), r.y()), (100, 60));
        assert_eq!(m.items.len(), 3, "no Quit added");
        assert_eq!(m.hit(&fs, r.x() + 5, r.y() + 1 + grid::ROW_H + 2), Some(1));
        // Near the right / bottom edge it flips up and left.
        let m = AppMenu::context(items(4), 315, 160);
        let r = m.rect(&fs);
        assert!(r.right() <= grid::sw() && r.bottom() <= grid::sh(), "{r:?}");
        assert!(r.x() < 315 && r.y() < 160);
        // Never above the menu strip or left of the screen, even for a tall menu.
        let m = AppMenu::context(items(20), 2, 150);
        let r = m.rect(&fs);
        assert!(r.x() >= 0 && r.y() >= grid::BAR_H);
        assert_eq!(m.columns(), 2, "long menus fold into two columns");
    }

    #[test]
    fn only_a_real_quit_gets_a_rule() {
        let fs = FontSystem::new();
        let plain = AppMenu::context(items(3), 10, 20).rect(&fs);
        let with_quit = AppMenu::new(items(2)).rect(&fs);
        assert_eq!(plain.height(), 3 * grid::ROW_H as u32 + 2);
        assert_eq!(with_quit.height(), 3 * grid::ROW_H as u32 + 2 + 4);
    }
}
