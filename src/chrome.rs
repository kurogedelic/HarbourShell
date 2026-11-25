//! Shared Cardputer Zero chrome: the 16 px menu strip, titled app windows, the
//! command bar, sliders and the on-screen toast. Everything is drawn at native
//! 320x170 pixels with the amber theme; nothing is scaled.

use crate::gfx::Rect;
use crate::gfx::Canvas;

use crate::assets::AssetStore;
use crate::font::FontSystem;
use crate::theme::ThemeManager;
use crate::ui;

/// The design size (see [`crate::grid::SCREEN_W`]); [`screen_w`] / [`screen_h`] give the surface
/// being drawn now.
pub const SCREEN_W: u32 = 320;
pub const SCREEN_H: u32 = 170;

pub fn screen_w() -> u32 {
    crate::grid::sw() as u32
}

pub fn screen_h() -> u32 {
    crate::grid::sh() as u32
}
pub const BAR_H: i32 = crate::grid::BAR_H;
pub const TITLEBAR_H: i32 = crate::grid::TITLE_H;
pub const COMMANDBAR_H: i32 = 16;
pub const ROW_H: i32 = crate::grid::ROW_H;

/// Live values shown in the menu strip.
#[derive(Clone, Debug, Default)]
pub struct StatusView {
    pub time: String,
    pub battery: Option<u8>,
    pub charging: bool,
    /// Wi-Fi signal 0..=100 while connected.
    pub wifi: Option<u8>,
    pub wifi_radio: bool,
    /// The Ethernet port has an address: it is the connection in use, so it replaces the Wi-Fi bars.
    pub ethernet: bool,
    /// The internet is reachable (a TCP connection to a public address works).
    pub online: bool,
    /// Output volume 0..=100 and mute; `None` when the mixer is not available.
    pub volume: Option<(u8, bool)>,
    /// Menu strip colours: 0 = the theme's, 1 = black fill with white text, 2 = white fill with black text.
    pub bar_style: u8,
    /// Input-method mode shown at the right of the context name (`あ`, `A`...); empty = hidden.
    pub ime: String,
    /// Only the anchor and the context name: a window's title strip on a desk (the desk's own strip
    /// shows the status).
    pub bare: bool,
}

/// Windows fill the screen below the menu bar, inset 2 px on the left, right and
/// bottom: the LCD's outermost pixels can be hidden by the bezel, which used to
/// cut off the frame line.
pub const WINDOW_INSET: i32 = crate::grid::WINDOW_INSET;

pub fn app_rect() -> Rect {
    crate::grid::window()
}

/// 1px dotted outline (every other pixel), as in the zoom rectangles.
fn dotted_rect(c: &mut Canvas, r: Rect) {
    let (x0, y0) = (r.x(), r.y());
    let (x1, y1) = (r.x() + r.width() as i32 - 1, r.y() + r.height() as i32 - 1);
    for x in x0..=x1 {
        if (x - x0) % 2 == 0 {
            let _ = c.draw_point((x, y0));
            let _ = c.draw_point((x, y1));
        }
    }
    for y in y0..=y1 {
        if (y - y0) % 2 == 0 {
            let _ = c.draw_point((x0, y));
            let _ = c.draw_point((x1, y));
        }
    }
}

/// "Zoom rects": a few dotted outlines chase each other from `from`
/// to `to` as `t` goes 0 -> 1 (opening a window: icon -> window; closing: reversed).
pub fn draw_zoom(c: &mut Canvas, tm: &ThemeManager, from: Rect, to: Rect, t: f32) {
    c.set_draw_color(tm.get_color("foreground"));
    let lerp = |a: i32, b: i32, k: f32| (a as f32 + (b - a) as f32 * k).round() as i32;
    for i in 0..4 {
        let k = t - i as f32 * 0.1;
        if k < 0.0 {
            continue;
        }
        let e = 1.0 - (1.0 - k.min(1.0)) * (1.0 - k.min(1.0)); // ease-out
        let x = lerp(from.x(), to.x(), e);
        let y = lerp(from.y(), to.y(), e);
        let w = lerp(from.width() as i32, to.width() as i32, e).max(2) as u32;
        let h = lerp(from.height() as i32, to.height() as i32, e).max(2) as u32;
        dotted_rect(c, Rect::new(x, y, w, h));
    }
}

/// Desktop: orange with faint scanlines (every other row a touch darker).
pub fn draw_desktop(c: &mut Canvas, tm: &ThemeManager) {
    let a = tm.get_color("background");
    let b = tm.get_color("shadow");
    // fill_rect, not clear(): clear() ignores the viewport and would wipe the other windows on a desk.
    c.set_draw_color(a);
    let _ = c.fill_rect(Rect::new(0, 0, screen_w(), screen_h()));
    c.set_draw_color(b);
    let mut y = BAR_H + 1;
    while y < screen_h() as i32 {
        let _ = c.draw_line((0, y), (screen_w() as i32 - 1, y));
        y += 2;
    }
}

/// Round the two top corners of the screen like a CRT / compact Mac (the bottom ones stay square,
/// so the framed screens can run down to y 168). Call last.
pub fn draw_corners(c: &mut Canvas, tm: &ThemeManager) {
    const R: [i32; 6] = crate::grid::CORNER_MASK; // pixels masked per row, from the corner
    // Always pure black, whatever the theme or Invert: the corners are the bezel, not part of the picture.
    let _ = tm;
    c.set_draw_color(crate::gfx::Color::RGB(0, 0, 0));
    let w = screen_w() as i32;
    for (row, n) in R.iter().enumerate() {
        let row = row as i32;
        let _ = c.draw_line((0, row), (n - 1, row));
        let _ = c.draw_line((w - n, row), (w - 1, row));
    }
}

/// Area inside the window frame, below the title bar.
pub fn inner_rect(rect: &Rect) -> Rect {
    crate::grid::window_inner(*rect)
}

/// Content area. Windows have no permanent footer; messages are overlaid
/// at the bottom with [`draw_command_bar`] only while there is something to say.
pub fn content_rect(inner: &Rect) -> Rect {
    *inner
}

pub fn fit_text(fs: &FontSystem, text: &str, max_w: i32) -> String {
    if fs.measure_text(text) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let candidate = format!("{out}{ch}...");
        if fs.measure_text(&candidate) > max_w {
            break;
        }
        out.push(ch);
    }
    if out.is_empty() {
        "...".to_string()
    } else {
        format!("{out}...")
    }
}

pub fn fit_display_text(fs: &FontSystem, text: &str, max_w: i32) -> String {
    if fs.measure_display_text(text) <= max_w {
        return text.to_string();
    }
    let mut prefix = text.to_string();
    while !prefix.is_empty() && fs.measure_display_text(&format!("{prefix}...")) > max_w {
        prefix.pop();
    }
    if prefix.is_empty() {
        "...".to_string()
    } else {
        format!("{prefix}...")
    }
}

/// Classic-Mac titled window: single outer border, striped title bar.
pub fn draw_app_window(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, rect: &Rect, title: &str) {
    let ink = tm.get_color("foreground");
    let stripe = tm.get_color("muted");
    let paper = tm.get_color("widget_face_active");
    // Dithered drop shadow (a 50% checkerboard), then the paper and a 1px frame.
    crate::dither::fill(
        c,
        Rect::new(rect.x() + 2, rect.y() + 2, rect.width(), rect.height()),
        tm.get_color("widget_shadow"),
        crate::dither::Pattern::Half,
    );
    c.set_draw_color(paper);
    let _ = c.fill_rect(*rect);
    ui::draw_rect(c, *rect, ink);

    let max_title_width = (rect.width() as i32 - 68).max(8);
    let title = fit_display_text(fs, title, max_title_width);
    let title_w = fs.measure_display_text(&title);
    let title_x = rect.x() + (rect.width() as i32 - title_w) / 2;
    let left_begin = rect.x() + 21;
    let left_end = title_x - 6;
    let right_begin = title_x + title_w + 6;
    let right_end = rect.x() + rect.width() as i32 - 6;
    for offset in [4, 8, 12] {
        if left_begin <= left_end {
            ui::line(c, left_begin, rect.y() + offset, left_end, rect.y() + offset, stripe);
        }
        if right_begin <= right_end {
            ui::line(c, right_begin, rect.y() + offset, right_end, rect.y() + offset, stripe);
        }
    }
    fs.draw_display_text(c, title_x, rect.y() + 1, &title, ink);
    ui::draw_rect(c, Rect::new(rect.x() + 6, rect.y() + 4, 8, 8), ink);
    ui::line(
        c,
        rect.x() + 1,
        rect.y() + TITLEBAR_H,
        rect.x() + rect.width() as i32 - 2,
        rect.y() + TITLEBAR_H,
        ink,
    );
}

/// Transient message overlaid on the bottom of a window (nothing is drawn when
/// `status` is empty). `_commands` is kept for call-site compatibility and unused.
pub fn draw_command_bar(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, inner: &Rect, status: &str, _commands: &str) {
    if status.trim().is_empty() || inner.height() < COMMANDBAR_H as u32 {
        return;
    }
    let ink = tm.get_color("foreground");
    let paper = tm.get_color("widget_face_active");
    let h = 15;
    let y = inner.y() + inner.height() as i32 - h;
    ui::fill_rect(c, Rect::new(inner.x(), y, inner.width(), h as u32), paper);
    ui::line(c, inner.x(), y, inner.x() + inner.width() as i32 - 1, y, ink);
    let text = fit_text(fs, &crate::launcher::i18n::tr_s(status), inner.width() as i32 - 12);
    fs.draw_text(c, inner.x() + 6, y + 1, &text, ink);
}

mod statusbar {
    include!("generated/statusbar.rs");
}
pub use statusbar::Icon;

/// Draws one frame of a status-bar icon from the atlas (`assets/images/statusbar/atlas_*.png`,
/// built from private drafts (assets/statusbar-src, not published) by `tools/gen_statusbar.py`): the grey layer in
/// `tint`, the dark layer in `fg`. Every icon is 12 x 12.
pub fn status_icon(c: &mut Canvas, assets: &mut AssetStore, icon: Icon, frame: i32, x: i32, y: i32, fg: crate::gfx::Color, tint: crate::gfx::Color) {
    let s = statusbar::SIZE;
    let frame = frame.clamp(0, icon.frames() - 1);
    let src = Rect::new(frame * s, icon as i32 * s, s as u32, s as u32);
    let dst = Rect::new(x, y, s as u32, s as u32);
    let _ = assets.blit_tinted_src(c, "statusbar/atlas_tint", src, dst, tint);
    let _ = assets.blit_tinted_src(c, "statusbar/atlas_fg", src, dst, fg);
}

/// Wi-Fi: frame 0 = radio off, then no signal / 1 / 2 / 3 bars from the signal strength (0..=100).
pub fn draw_wifi(c: &mut Canvas, assets: &mut AssetStore, x: i32, y: i32, signal: Option<u8>, radio: bool, ink: crate::gfx::Color, dim: crate::gfx::Color) {
    use statusbar::wifi_frame as f;
    let frame = if !radio {
        f::DISABLED
    } else {
        match signal {
            None => f::F0,
            Some(s) => f::F0 + 1 + (s as i32 / 34).min(2),
        }
    };
    status_icon(c, assets, Icon::Wifi, frame, x, y, ink, dim);
}

/// Battery: 0 / 1 / 2 / 3 (full) from the percentage, charging, or disabled (no gauge).
pub fn draw_battery(c: &mut Canvas, assets: &mut AssetStore, x: i32, y: i32, percent: Option<u8>, charging: bool, ink: crate::gfx::Color, dim: crate::gfx::Color) {
    use statusbar::battery_frame as f;
    let frame = if charging {
        f::CHARGE
    } else {
        match percent {
            None => f::DISABLED,
            Some(p) => f::F0 + [10u8, 40, 70].iter().filter(|&&t| p > t).count() as i32,
        }
    };
    status_icon(c, assets, Icon::Battery, frame, x, y, ink, dim);
}

/// 16 px menu strip: anchor, the current context title, and on the
/// right Wi-Fi, battery and clock (real values, never invented).
pub fn draw_menu_bar(c: &mut Canvas, assets: &mut AssetStore, fs: &FontSystem, tm: &ThemeManager, context: &str, st: &StatusView) {
    // "Black" and "White" use the theme palette's darkest and brightest colours (not pure black / white), so
    // the bar stays in the palette; the dim tone for inactive marks sits halfway between them.
    // The brightest and the darkest colour anywhere in the theme's palette.
    const KEYS: [&str; 12] = [
        "foreground", "background", "topbar_bg", "widget_face", "widget_face_active", "widget_highlight",
        "icon_white", "select_bg", "select_fg", "surface_alt", "home_bg", "widget_shadow",
    ];
    let lum = |c: crate::gfx::Color| c.r as u32 * 3 + c.g as u32 * 6 + c.b as u32;
    let pal: Vec<crate::gfx::Color> = KEYS.iter().map(|k| tm.get_color(k)).collect();
    let light = pal.iter().copied().max_by_key(|c| lum(*c)).unwrap_or(crate::gfx::Color::RGB(255, 255, 255));
    let dark = pal.iter().copied().min_by_key(|c| lum(*c)).unwrap_or(crate::gfx::Color::RGB(0, 0, 0));
    struct T { light: crate::gfx::Color, dark: crate::gfx::Color }
    let t = T { light, dark };
    let mid = |a: crate::gfx::Color, b: crate::gfx::Color| {
        crate::gfx::Color::RGB(((a.r as u16 + b.r as u16) / 2) as u8, ((a.g as u16 + b.g as u16) / 2) as u8, ((a.b as u16 + b.b as u16) / 2) as u8)
    };
    let (ink, dim, paper) = match st.bar_style {
        1 => (t.light, mid(t.light, t.dark), t.dark),
        2 => (t.dark, mid(t.light, t.dark), t.light),
        _ => (tm.get_color("foreground"), tm.get_color("muted"), tm.get_color("topbar_bg")),
    };
    c.set_draw_color(paper);
    let _ = c.fill_rect(Rect::new(0, 0, screen_w(), BAR_H as u32));
    ui::line(c, 0, BAR_H - 1, screen_w() as i32 - 1, BAR_H - 1, ink);
    status_icon(c, assets, Icon::Anchor, 0, 7, 2, ink, dim);
    let x = 22;

    if st.bare {
        if !context.is_empty() {
            let text = fit_display_text(fs, context, screen_w() as i32 - 7 - x);
            fs.draw_display_text(c, x, 1, &text, ink);
        }
        return;
    }
    // Right side, built from the edge inward.
    let mut right = screen_w() as i32 - 7;
    let clock = &st.time;
    let cw = fs.measure_display_text(clock);
    right -= cw;
    fs.draw_display_text(c, right, 1, clock, ink);
    right -= 9;
    if st.battery.is_some() || st.charging {
        right -= 12;
        draw_battery(c, assets, right, 2, st.battery, st.charging, ink, dim);
        right -= 4;
        if let Some(p) = st.battery {
            let t = format!("{p}%");
            let w = fs.measure_text(&t);
            right -= w;
            fs.draw_text(c, right, 1, &t, ink);
            right -= 9;
        }
    }
    right -= 12;
    if st.ethernet {
        status_icon(c, assets, Icon::Ethernet, 0, right, 2, ink, dim);
    } else {
        draw_wifi(c, assets, right, 2, st.wifi, st.wifi_radio, ink, dim);
    }
    right -= 4;
    right -= 12;
    status_icon(c, assets, if st.online { Icon::Globe } else { Icon::GlobeDisabled }, 0, right, 2, ink, dim);
    if let Some((p, muted)) = st.volume {
        use statusbar::speaker_frame as f;
        let frame = if muted { f::MUTE } else if p == 0 { f::F0 } else if p <= 50 { f::F1 } else { f::F2 };
        right -= 15;
        status_icon(c, assets, Icon::Speaker, frame, right, 2, ink, dim);
    }
    right -= 8;
    if !st.ime.is_empty() {
        // A small reversed cell, like a keyboard-menu icon.
        let w = fs.measure_text(&st.ime).max(6) + 6;
        right -= w;
        ui::fill_rect(c, Rect::new(right, 1, w as u32, (BAR_H - 3) as u32), ink);
        fs.draw_text(c, right + 3, 1, &st.ime, paper);
        right -= 6;
    }

    if !context.is_empty() {
        let max = (right - x).max(8);
        let text = fit_display_text(fs, context, max);
        fs.draw_display_text(c, x, 1, &text, ink);
    }
}

/// The things at the right of the menu strip that open a menu when clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusItem {
    Clock,
    Battery,
    Network,
    Internet,
    Speaker,
    Ime,
}

/// Where each status item sits in the strip (x from, x to exclusive), laid out exactly as
/// [`draw_menu_bar`] draws them, right to left. A couple of pixels wider each side to hit.
pub fn status_spans(fs: &FontSystem, st: &StatusView) -> Vec<(StatusItem, i32, i32)> {
    let mut v = Vec::new();
    if st.bare {
        return v;
    }
    let mut right = screen_w() as i32 - 7;
    let cw = fs.measure_display_text(&st.time);
    v.push((StatusItem::Clock, right - cw - 2, right + 4));
    right -= cw + 9;
    if st.battery.is_some() || st.charging {
        let end = right;
        right -= 16;
        if let Some(p) = st.battery {
            right -= fs.measure_text(&format!("{p}%")) + 9;
        }
        v.push((StatusItem::Battery, right + 7, end + 2));
    }
    right -= 12;
    v.push((StatusItem::Network, right - 2, right + 14));
    right -= 16;
    v.push((StatusItem::Internet, right - 2, right + 14));
    if st.volume.is_some() {
        right -= 15;
        v.push((StatusItem::Speaker, right - 2, right + 15));
    }
    right -= 8;
    if !st.ime.is_empty() {
        let w = fs.measure_text(&st.ime).max(6) + 6;
        right -= w;
        v.push((StatusItem::Ime, right - 2, right + w + 2));
    }
    v
}

/// The status item at strip x `x` (y is the caller's: the strip only).
pub fn status_hit(fs: &FontSystem, st: &StatusView, x: i32) -> Option<(StatusItem, i32)> {
    status_spans(fs, st).into_iter().find(|(_, a, b)| (*a..*b).contains(&x)).map(|(i, a, _)| (i, a))
}

/// Horizontal slider 0..=100 inside `r` (1px frame, filled portion).
pub fn draw_slider(c: &mut Canvas, r: Rect, value: u32, fg: crate::gfx::Color, bg: crate::gfx::Color) {
    ui::fill_rect(c, r, bg);
    ui::draw_rect(c, r, fg);
    let inner_w = r.width().saturating_sub(4) as u32;
    let w = inner_w * value.min(100) / 100;
    if w > 0 {
        ui::fill_rect(c, Rect::new(r.x() + 2, r.y() + 2, w, r.height().saturating_sub(4)), fg);
    }
}

/// Mouse pointer: an arrow with an ink fill and a paper outline (visible on any theme).
pub fn draw_cursor(c: &mut Canvas, tm: &ThemeManager, x: i32, y: i32) {
    for (row, line) in CURSOR_ART.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            let color = match ch {
                b'X' => tm.get_color("widget_face_active"),
                b'O' => tm.get_color("foreground"),
                _ => continue,
            };
            c.set_draw_color(color);
            let _ = c.draw_point((x + col as i32, y + row as i32));
        }
    }
}

/// The pointer arrow: `X` the paper outline, `O` the ink fill; the tip (0, 0) is the hot spot. Also
/// the system cursor's shape in a desktop window.
pub const CURSOR_ART: [&str; 16] = [
        "X           ",
        "XX          ",
        "XOX         ",
        "XOOX        ",
        "XOOOX       ",
        "XOOOOX      ",
        "XOOOOOX     ",
        "XOOOOOOX    ",
        "XOOOOOOOX   ",
        "XOOOOOXXXX  ",
        "XOOXOOX     ",
        "XOX XOOX    ",
        "XX  XOOX    ",
        "X    XOOX   ",
        "     XOOX   ",
        "      XX    ",
];

/// Centered toast near the bottom, e.g. "Volume 45%".
pub fn draw_toast(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, text: &str, fraction: Option<u32>) {
    let ink = tm.get_color("foreground");
    // Long messages (e.g. a failed app start) must never overflow the screen.
    let text = &fit_text(fs, &crate::launcher::i18n::tr_s(text), screen_w() as i32 - 32);
    let w = ((fs.measure_text(text) + 16).max(if fraction.is_some() { 120 } else { 60 }) as u32).min(screen_w() - 8);
    let h = if fraction.is_some() { 30 } else { 20 };
    let x = (screen_w() - w) as i32 / 2;
    // Its 1 px shadow ends at y 166, 1 px above the app frame's bottom line (y 168).
    let y = crate::grid::frame_bottom() - 2 - h as i32;
    let r = Rect::new(x, y, w, h);
    crate::launcher::widgets::draw_panel(c, tm, r);
    let tw = fs.measure_text(text);
    fs.draw_text(c, x + (w as i32 - tw) / 2, y + 4, text, ink);
    if let Some(f) = fraction {
        crate::launcher::widgets::ProgressBar::new(Rect::new(x + 8, y + 17, w - 16, 9), Some(f as f32 / 100.0), 0).draw(c, tm);
    }
}

/// "Hold Esc to quit": a small plate near the bottom with a dithered gauge that
/// fills as the key is held. `fraction` is 0.0..=1.0.
pub fn draw_hold_gauge(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, label: &str, fraction: f32) {
    let ink = tm.get_color("foreground");
    let label = &crate::launcher::i18n::tr_s(label);
    let w = (fs.measure_text(label) + 24).max(132) as u32;
    // Like the toast: its shadow ends at y 166, 1 px above the app frame's bottom line.
    let r = Rect::new((screen_w() as i32 - w as i32) / 2, crate::grid::frame_bottom() - 2 - 32, w, 32);
    crate::launcher::widgets::draw_panel(c, tm, r);
    let tw = fs.measure_text(label);
    fs.draw_text(c, r.x() + (w as i32 - tw) / 2, r.y() + 3, label, ink);
    crate::launcher::widgets::ProgressBar::new(Rect::new(r.x() + 8, r.y() + 17, w - 16, 10), Some(fraction), 0).draw(c, tm);
}

/// "Opening <app>": the launch screen shown while an external app starts. The bar is a
/// A "barber pole" (diagonal stripes sliding along), since nobody knows
/// how long the app will take; `tick` advances the stripes.
pub fn draw_launching(c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, name: &str, tick: u32) {
    draw_desktop(c, tm);
    let ink = tm.get_color("foreground");
    let w = (fs.measure_display_text(name).max(120) + 56).min(screen_w() as i32 - 24) as u32;
    let r = Rect::new((screen_w() as i32 - w as i32) / 2, 52, w, 60);
    crate::launcher::widgets::draw_panel(c, tm, r);
    let label = fit_display_text(fs, &crate::launcher::i18n::tr_s(&format!("Opening {name}")), r.width() as i32 - 16);
    let lw = fs.measure_display_text(&label);
    fs.draw_display_text(c, r.x() + (r.width() as i32 - lw) / 2, r.y() + 10, &label, ink);
    crate::launcher::widgets::ProgressBar::new(Rect::new(r.x() + 12, r.y() + 32, r.width() - 24, 14), None, tick).draw(c, tm);
}
