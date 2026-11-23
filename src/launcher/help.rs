//! Help: the keys of the screen in front, then the keys that work everywhere.
//!
//! Opened from the Harbour menu (Help). Apps give their own rows through
//! [`LauncherApp::help`](crate::launcher::app::LauncherApp::help); the host adds rows for its own
//! screens (Home, Settings, the Text Editor...). This replaces key hints drawn on the screens
//! themselves: a screen stays clean, the keys are one menu away.
//!
//! Drawn over the app area like a window: a title band, 16 px rows (key | what it does), the shared
//! scroll bar. Esc, Return or Help again closes it; Up/Down, PageUp/PageDown and the wheel scroll.

use sdl2::keyboard::Keycode;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::chrome;
use crate::font::FontSystem;
use crate::grid;
use crate::launcher::i18n;
use crate::launcher::widgets::{self, ScrollBar};
use crate::theme::ThemeManager;
use crate::ui;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelpRow {
    Section(String),
    Key(String, String),
}

/// The keys every screen shares (docs/DESIGN.md "Keys").
pub fn global_rows() -> Vec<HelpRow> {
    let k = |a: &str, b: &str| HelpRow::Key(a.into(), b.into());
    vec![
        HelpRow::Section("Everywhere".into()),
        k("Alt", "App menu (Left: Harbour menu)"),
        k("Ctrl+Tab", "App menu"),
        k("Esc", "Close or go back one step"),
        k("Esc (hold)", "Leave the app to Home"),
        k("Tab (hold)", "Lock the screen"),
        k("Tab Tab", "Agent (where Tab is free)"),
        k("Alt+Tab", "Agent"),
        k("Ctrl+Space", "Japanese input on / off"),
        k("Ctrl+Shift+Space", "Kana mode"),
        k("F X Z C", "Arrows without Fn"),
        k("Fn + keys", "Volume and brightness"),
        HelpRow::Section("Tabbed screens".into()),
        k("Tab / Shift+Tab", "Next / previous tab"),
        k("Left / Right", "Next / previous tab"),
        HelpRow::Section("Mouse".into()),
        k("Click", "Select; double click opens"),
        k("Right click", "App menu"),
        k("Wheel", "Scroll"),
    ]
}

pub struct HelpView {
    pub title: String,
    rows: Vec<HelpRow>,
    scroll: usize,
}

const ROW: i32 = 16;
const TOP: i32 = grid::BAR_H;
const KEY_W: i32 = 112;

fn body_rect() -> Rect {
    Rect::new(0, TOP, grid::SCREEN_W as u32, (grid::APP_FRAME_BOTTOM + 1 - TOP) as u32)
}

fn list_top() -> i32 {
    TOP + ROW
}

fn visible() -> usize {
    ((grid::APP_FRAME_BOTTOM + 1 - list_top()) / ROW) as usize
}

fn sb_rect() -> Rect {
    Rect::new(grid::SCROLL_X, list_top() + 1, grid::SCROLL_W as u32, (grid::APP_FRAME_BOTTOM - list_top() - 1) as u32)
}

impl HelpView {
    /// `app` = the screen's name and its own rows (may be empty).
    pub fn new(app: &str, app_rows: Vec<(String, String)>) -> Self {
        let mut rows = Vec::new();
        if !app_rows.is_empty() {
            rows.push(HelpRow::Section(app.to_string()));
            rows.extend(app_rows.into_iter().map(|(a, b)| HelpRow::Key(a, b)));
        }
        rows.extend(global_rows());
        Self { title: app.to_string(), rows, scroll: 0 }
    }

    pub fn rows(&self) -> &[HelpRow] {
        &self.rows
    }

    fn max_scroll(&self) -> usize {
        self.rows.len().saturating_sub(visible())
    }

    fn scroll_by(&mut self, d: isize) {
        self.scroll = (self.scroll as isize + d).clamp(0, self.max_scroll() as isize) as usize;
    }

    /// Returns true when Help should close.
    pub fn key(&mut self, key: Keycode) -> bool {
        match key {
            Keycode::Escape | Keycode::Return | Keycode::KpEnter => return true,
            Keycode::Up => self.scroll_by(-1),
            Keycode::Down => self.scroll_by(1),
            Keycode::PageUp | Keycode::Left => self.scroll_by(-(visible() as isize)),
            Keycode::PageDown | Keycode::Right | Keycode::Space => self.scroll_by(visible() as isize),
            Keycode::Home => self.scroll = 0,
            Keycode::End => self.scroll = self.max_scroll(),
            _ => {}
        }
        false
    }

    pub fn wheel(&mut self, dy: i32) {
        self.scroll_by(if dy > 0 { -1 } else { 1 });
    }

    /// A press: the scroll bar pages / jumps; elsewhere nothing (Esc or Return closes).
    pub fn click(&mut self, x: i32, y: i32) {
        let sb = ScrollBar::new(sb_rect(), self.rows.len(), visible(), self.scroll);
        if let Some((off, _)) = sb.press(x, y) {
            self.scroll = off.min(self.max_scroll());
        }
    }

    pub fn draw(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        let faint = widgets::secondary_ink(tm);
        let band = tm.get_color("line");
        ui::fill_rect(c, body_rect(), tm.get_color("home_bg"));
        // Title band.
        let t = Rect::new(grid::MARGIN, TOP, (grid::SCREEN_W - 2 * grid::MARGIN) as u32, ROW as u32);
        ui::fill_rect(c, t, band);
        ui::line(c, t.x(), t.bottom() - 1, t.right() - 1, t.bottom() - 1, ink);
        let title = format!("{}  {}", i18n::tr("Help"), i18n::tr_s(&self.title));
        fs.draw_text(c, t.x() + 4, t.y() + 2, &chrome::fit_text(fs, &title, t.width() as i32 - 8), ink);
        let w = grid::SCROLL_X - 4 - grid::MARGIN;
        for (k, row) in self.rows.iter().skip(self.scroll).take(visible()).enumerate() {
            let r = Rect::new(grid::MARGIN, list_top() + k as i32 * ROW, w as u32, ROW as u32);
            match row {
                HelpRow::Section(s) => {
                    ui::fill_rect(c, r, tm.get_color("surface_alt"));
                    fs.draw_text(c, r.x() + 4, r.y() + 2, &chrome::fit_text(fs, &i18n::tr_s(s), w - 8), ink);
                }
                HelpRow::Key(key, does) => {
                    fs.draw_text(c, r.x() + 4, r.y() + 2, &chrome::fit_text(fs, &i18n::tr_s(key), KEY_W - 8), ink);
                    ui::line(c, r.x() + KEY_W, r.y(), r.x() + KEY_W, r.bottom() - 1, band);
                    fs.draw_text(c, r.x() + KEY_W + 6, r.y() + 2, &chrome::fit_text(fs, &i18n::tr_s(does), w - KEY_W - 10), faint);
                }
            }
            ui::line(c, r.x(), r.bottom() - 1, r.right() - 1, r.bottom() - 1, band);
        }
        // Frame sides and bottom, like the framed app screens.
        let f = Rect::new(grid::MARGIN, list_top(), w as u32, (grid::APP_FRAME_BOTTOM + 1 - list_top()) as u32);
        ui::line(c, f.x(), f.y(), f.x(), f.bottom() - 1, band);
        ui::line(c, f.right() - 1, f.y(), f.right() - 1, f.bottom() - 1, band);
        ui::line(c, f.x(), f.bottom() - 1, f.right() - 1, f.bottom() - 1, band);
        ScrollBar::new(sb_rect(), self.rows.len(), visible(), self.scroll).draw(c, tm, None);
    }
}

/// Tests: an app's Help rows are present, short, filled in and translated. A key needs a Japanese
/// entry unless it is made only of key names ("Ctrl+C / X / V", "Up / Down", "1 - 6").
#[cfg(test)]
pub fn check_app_rows(app: &str, rows: &[(String, String)]) {
    const KEYS: &[&str] = &[
        "Return", "Space", "Backspace", "Up", "Down", "Left", "Right", "Tab", "Shift", "Ctrl", "Esc", "PageUp", "PageDown", "F2", "F5", "0-9",
    ];
    assert!(!rows.is_empty(), "{app}: no Help rows");
    assert!(rows.len() <= 12, "{app}: {} Help rows", rows.len());
    for (key, does) in rows {
        assert!(!key.trim().is_empty() && !does.trim().is_empty(), "{app}: empty row {key:?} / {does:?}");
        assert!(i18n::has_ja(does), "{app}: {does:?} has no Japanese entry");
        let plain = key.split(|c: char| c == ' ' || c == '/' || c == '+').filter(|t| !t.is_empty()).all(|t| t.chars().count() == 1 || KEYS.contains(&t));
        assert!(plain || i18n::has_ja(key), "{app}: key {key:?} has no Japanese entry");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_rows_come_first_then_the_shared_keys() {
        let h = HelpView::new("Interceptor", vec![("Space".into(), "Pause".into())]);
        assert_eq!(h.rows()[0], HelpRow::Section("Interceptor".into()));
        assert_eq!(h.rows()[1], HelpRow::Key("Space".into(), "Pause".into()));
        assert_eq!(h.rows()[2], HelpRow::Section("Everywhere".into()));
        let plain = HelpView::new("Clock", vec![]);
        assert_eq!(plain.rows()[0], HelpRow::Section("Everywhere".into()), "no empty app section");
    }

    #[test]
    fn scrolling_stays_in_range_and_esc_closes() {
        let mut h = HelpView::new("x", vec![]);
        h.key(Keycode::Up);
        assert_eq!(h.scroll, 0);
        h.key(Keycode::End);
        assert_eq!(h.scroll, h.rows.len() - visible());
        h.key(Keycode::PageDown);
        assert_eq!(h.scroll, h.rows.len() - visible());
        h.wheel(1);
        assert_eq!(h.scroll, h.rows.len() - visible() - 1);
        assert!(!h.key(Keycode::Down));
        assert!(h.key(Keycode::Escape));
    }
}
