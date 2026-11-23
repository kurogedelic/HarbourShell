//! Calendar: a month grid (weeks start on Sunday) with today highlighted.
//!
//! Arrows (or F/X/Z/C) move the day, PageUp / PageDown the month, Home jumps to
//! today. Mouse: click a day, wheel changes the month, double click = today.

use chrono::{Datelike, Duration, NaiveDate};
use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::dither::{self, Pattern};
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::widgets::{self, contains, MenuItem, PointerEvent};
use crate::platform::keys::arrow_alias;
use crate::ui;

pub const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const JA_WD: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];
const JA_WEEKDAYS: [&str; 7] = ["日曜日", "月曜日", "火曜日", "水曜日", "木曜日", "金曜日", "土曜日"];
pub const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

pub fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    NaiveDate::from_ymd_opt(ny, nm, 1).unwrap().signed_duration_since(first).num_days() as u32
}

/// Move by whole months, clamping the day (31 Jan + 1 month = 28/29 Feb).
pub fn add_months(d: NaiveDate, n: i32) -> NaiveDate {
    let total = d.year() * 12 + d.month0() as i32 + n;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    NaiveDate::from_ymd_opt(y, m, d.day().min(days_in_month(y, m))).unwrap()
}

/// The 6 x 7 days shown for a month: starts on the Sunday on or before the 1st.
pub fn month_grid(year: i32, month: u32) -> [NaiveDate; 42] {
    let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    let start = first - Duration::days(first.weekday().num_days_from_sunday() as i64);
    std::array::from_fn(|i| start + Duration::days(i as i64))
}

pub struct Calendar {
    sel: Option<NaiveDate>,
    /// The month arrow under the pointer (false = previous).
    hover: Option<bool>,
}

pub fn make() -> Box<dyn LauncherApp> {
    Box::new(Calendar { sel: None, hover: None })
}

const GRID_X: i32 = grid::MARGIN;
const CELL_W: i32 = 28;
const HEAD_Y: i32 = grid::BAR_H + grid::MARGIN;
const WD_Y: i32 = HEAD_Y + grid::ROW_H;
/// The weekday band is 17 px and every week 19 px, so the six weeks end on y 166 and the grid, the
/// side panel and its shadow (y 168) use the screen down to its bottom.
const WD_H: i32 = 17;
const DAYS_Y: i32 = WD_Y + WD_H;
const DAY_H: i32 = 19;
const PANEL_X: i32 = GRID_X + 7 * CELL_W + grid::GUTTER * 2;

impl Calendar {
    fn sel(&self, ctx: &Ctx) -> NaiveDate {
        self.sel.unwrap_or_else(|| ctx.now.date_naive())
    }

    fn cell_at(&self, x: i32, y: i32, ctx: &Ctx) -> Option<NaiveDate> {
        let area = Rect::new(GRID_X, DAYS_Y, (7 * CELL_W) as u32, (6 * DAY_H) as u32);
        if !contains(area, x, y) {
            return None;
        }
        let s = self.sel(ctx);
        let (col, row) = ((x - GRID_X) / CELL_W, (y - DAYS_Y) / DAY_H);
        Some(month_grid(s.year(), s.month())[(row * 7 + col) as usize])
    }
}

/// The month arrows at the right end of the title row.
fn arrow_rect(next: bool) -> Rect {
    let right = GRID_X + 7 * CELL_W;
    Rect::new(right - 18 * (2 - next as i32), HEAD_Y, 16, grid::ROW_H as u32)
}

impl LauncherApp for Calendar {
    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Left / Right", "Previous / next day"),
            k("Up / Down", "Previous / next week"),
            k("PageUp / PageDown", "Previous / next month"),
            k("Wheel", "Previous / next month"),
            k("T", "Go to today"),
            k("Double click", "Go to today"),
        ]
    }

    fn title(&self) -> String {
        crate::launcher::i18n::tr_s("Calendar")
    }

    fn describe(&self) -> String {
        match self.sel {
            Some(d) => format!("Calendar: selected day {d}"),
            None => "Calendar: showing the current month, today is highlighted".to_string(),
        }
    }

    fn menu(&self) -> Vec<MenuItem> {
        vec![MenuItem::new("today", "Go to today"), MenuItem::new("prev", "Previous month"), MenuItem::new("next", "Next month")]
    }

    fn context_menu(&mut self, x: i32, y: i32, ctx: &Ctx) -> Option<Vec<MenuItem>> {
        if let Some(d) = self.cell_at(x, y, ctx) {
            self.sel = Some(d);
        }
        Some(self.menu())
    }

    fn menu_action(&mut self, id: &str, ctx: &Ctx) -> AppAction {
        let s = self.sel(ctx);
        self.sel = match id {
            "today" => Some(ctx.now.date_naive()),
            "prev" => Some(add_months(s, -1)),
            "next" => Some(add_months(s, 1)),
            _ => self.sel,
        };
        AppAction::None
    }

    fn key(&mut self, key: Keycode, _mods: Mod, _repeat: bool, ctx: &Ctx) -> AppAction {
        let s = self.sel(ctx);
        let day = |n: i64| s.checked_add_signed(Duration::days(n)).unwrap_or(s);
        self.sel = Some(match arrow_alias(key) {
            Keycode::Escape => return AppAction::Quit,
            Keycode::Left => day(-1),
            Keycode::Right => day(1),
            Keycode::Up => day(-7),
            Keycode::Down => day(7),
            Keycode::PageUp => add_months(s, -1),
            Keycode::PageDown => add_months(s, 1),
            Keycode::Home | Keycode::T => ctx.now.date_naive(),
            _ => s,
        });
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> AppAction {
        match ev {
            PointerEvent::Wheel { dy, .. } => self.sel = Some(add_months(self.sel(ctx), if dy > 0 { -1 } else { 1 })),
            PointerEvent::Move { x, y } | PointerEvent::Drag { x, y, .. } => {
                self.hover = [false, true].into_iter().find(|n| contains(arrow_rect(*n), x, y));
            }
            PointerEvent::Down { x, y, button: 0, .. } if contains(arrow_rect(false), x, y) || contains(arrow_rect(true), x, y) => {
                self.sel = Some(add_months(self.sel(ctx), if contains(arrow_rect(true), x, y) { 1 } else { -1 }));
            }
            PointerEvent::Down { x, y, button: 0, clicks } => {
                if clicks >= 2 {
                    self.sel = Some(ctx.now.date_naive());
                } else if let Some(d) = self.cell_at(x, y, ctx) {
                    self.sel = Some(d);
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, _ctx: &Ctx) -> bool {
        false
    }

    fn draw(&mut self, c: &mut Canvas<Window>, _assets: &mut AssetStore, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let paper = tm.get_color("widget_face_active");
        let faint = crate::launcher::widgets::secondary_ink(tm);
        let line = tm.get_color("line");
        let alt = tm.get_color("surface_alt");
        let soft = tm.get_color("accent_soft");
        let well = tm.get_color("surface_sunken");
        ui::fill_rect(c, Rect::new(0, grid::BAR_H, grid::SCREEN_W as u32, (grid::SCREEN_H - grid::BAR_H) as u32), tm.get_color("home_bg"));

        let today = ctx.now.date_naive();
        let sel = self.sel(ctx);
        let (y, m) = (sel.year(), sel.month());

        // Month title.
        let ja = crate::launcher::i18n::get() == crate::launcher::i18n::Lang::Ja;
        let title = if ja { format!("{}年{}月", y, m) } else { format!("{} {}", MONTHS[(m - 1) as usize], y) };
        fs.draw_display_text(c, GRID_X + 2, HEAD_Y + 1, &title, ink);
        for next in [false, true] {
            widgets::draw_button(c, fs, tm, arrow_rect(next), if next { ">" } else { "<" }, widgets::BtnState { hover: self.hover == Some(next), ..Default::default() });
        }

        // Weekday header.
        let wd = Rect::new(GRID_X, WD_Y, (7 * CELL_W) as u32, WD_H as u32);
        ui::fill_rect(c, wd, alt);
        for (i, name) in WEEKDAYS.iter().enumerate() {
            let s: String = if ja { JA_WD[i].to_string() } else { name.chars().take(2).collect() };
            let w = fs.measure_text(&s);
            let col = if i == 0 || i == 6 { faint } else { ink };
            fs.draw_text(c, GRID_X + i as i32 * CELL_W + (CELL_W - w) / 2, WD_Y + 1, &s, col);
        }

        // Days.
        let area = Rect::new(GRID_X, DAYS_Y, (7 * CELL_W) as u32, (6 * DAY_H) as u32);
        ui::fill_rect(c, area, paper);
        // Grid lines: a rule under every week and between every day column (the plates sit inside).
        for k in 1..7 {
            ui::fill_rect(c, Rect::new(GRID_X + k * CELL_W, WD_Y, 1, area.height() + WD_H as u32), line);
        }
        for k in 0..6 {
            ui::fill_rect(c, Rect::new(GRID_X, DAYS_Y + (k + 1) * DAY_H - 1, area.width(), 1), line);
        }
        for (i, d) in month_grid(y, m).iter().enumerate() {
            let r = Rect::new(GRID_X + (i as i32 % 7) * CELL_W, DAYS_Y + (i as i32 / 7) * DAY_H, CELL_W as u32, DAY_H as u32);
            let in_month = d.month() == m;
            let cell = Rect::new(r.x() + if i % 7 == 0 { 0 } else { 1 }, r.y(), r.width() - if i % 7 == 0 { 0 } else { 1 }, r.height() - 1);
            let is_sel = *d == sel;
            let is_today = *d == today;
            if is_sel {
                ui::fill_rect(c, cell, widgets::selection(tm).bg);
            } else if is_today {
                ui::fill_rect(c, cell, soft);
            }
            if is_today {
                ui::draw_rect(c, cell, ink);
            }
            let s = d.day().to_string();
            let w = fs.measure_text(&s);
            let col = if is_sel { widgets::selection(tm).fg } else if in_month { ink } else { faint };
            fs.draw_text(c, r.x() + (CELL_W - w) / 2, widgets::text_origin(r.y(), r.height() as i32), &s, col);
            if !in_month && !is_sel {
                dither::fill(c, cell, paper, Pattern::Quarter);
            }
        }
        ui::draw_rect(c, area, line);
        // The weekday band is closed by a heavier (ink) rule.
        ui::fill_rect(c, Rect::new(GRID_X, DAYS_Y - 1, area.width(), 1), ink);

        // Side panel: the selected day.
        let panel = Rect::new(PANEL_X, WD_Y, (grid::SCROLL_X + grid::SCROLL_W - PANEL_X) as u32, (DAYS_Y + 6 * DAY_H - WD_Y) as u32);
        dither::fill(c, Rect::new(panel.x() + 2, panel.y() + 2, panel.width(), panel.height()), line, Pattern::Half);
        ui::fill_rect(c, panel, well);
        ui::draw_rect(c, panel, ink);
        let day = sel.day().to_string();
        let dw = fs.measure_display_text(&day);
        fs.draw_display_text(c, panel.x() + (panel.width() as i32 - dw) / 2, panel.y() + 6, &day, ink);
        let wdn_i = sel.weekday().num_days_from_sunday() as usize;
        let wdn = if ja { JA_WEEKDAYS[wdn_i] } else { WEEKDAYS[wdn_i] };
        let ww = fs.measure_text(wdn);
        fs.draw_text(c, panel.x() + (panel.width() as i32 - ww) / 2, panel.y() + 24, wdn, ink);
        let rel = match (sel - today).num_days() {
            0 => "Today".to_string(),
            1 => "Tomorrow".to_string(),
            -1 => "Yesterday".to_string(),
            n if n > 0 => format!("In {n} days"),
            n => format!("{} days ago", -n),
        };
        let lines = [crate::launcher::i18n::tr_s(&rel), crate::launcher::i18n::tr_s(&format!("Day {}", sel.ordinal())), crate::launcher::i18n::tr_s(&format!("Week {}", sel.iso_week().week()))];
        for (i, l) in lines.iter().enumerate() {
            fs.draw_text(c, panel.x() + 6, panel.y() + 44 + i as i32 * grid::ROW_H, l, if i == 0 { ink } else { faint });
        }
        // Rules: under the date block and under every info row.
        for k in 0..4 {
            ui::fill_rect(c, Rect::new(panel.x() + 1, panel.y() + 40 + k * grid::ROW_H, panel.width() - 2, 1), line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn month_lengths_include_leap_years() {
        assert_eq!(days_in_month(2026, 1), 31);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2028, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2026, 12), 31);
        assert_eq!(days_in_month(2026, 4), 30);
    }

    #[test]
    fn adding_months_clamps_and_wraps_years() {
        assert_eq!(add_months(d(2026, 1, 31), 1), d(2026, 2, 28));
        assert_eq!(add_months(d(2028, 1, 31), 1), d(2028, 2, 29));
        assert_eq!(add_months(d(2026, 12, 15), 1), d(2027, 1, 15));
        assert_eq!(add_months(d(2026, 1, 15), -1), d(2025, 12, 15));
        assert_eq!(add_months(d(2026, 3, 31), -12), d(2025, 3, 31));
    }

    #[test]
    fn the_grid_starts_on_a_sunday_and_covers_the_month() {
        // 1 Nov 2026 is a Sunday.
        let g = month_grid(2026, 11);
        assert_eq!(g[0], d(2026, 11, 1));
        // 1 Oct 2026 is a Thursday: the grid starts on Sun 27 Sep.
        let g = month_grid(2026, 10);
        assert_eq!(g[0], d(2026, 9, 27));
        for date in &g {
            assert_eq!(date.weekday().num_days_from_sunday(), (*date - g[0]).num_days() as u32 % 7);
        }
        let last = *month_grid(2026, 2).iter().find(|x| **x == d(2026, 2, 28)).unwrap();
        assert_eq!(last, d(2026, 2, 28));
        assert_eq!(g[41], g[0] + Duration::days(41));
    }

    #[test]
    fn arrows_step_months_and_day_cells_select() {
        use crate::font::FontSystem;
        use crate::system::mock::MockBackend;
        use crate::theme::{Theme, ThemeManager};
        let fs = FontSystem::new();
        let tm = ThemeManager::new(Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = MockBackend::new();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut c = Calendar { sel: Some(d(2026, 3, 31)), hover: None };
        let down = |x, y, clicks| PointerEvent::Down { x, y, button: 0, clicks };
        let (n, p) = (arrow_rect(true), arrow_rect(false));
        assert!(!p.has_intersection(n));
        c.pointer(down(n.x() + 2, n.y() + 2, 1), &ctx);
        assert_eq!(c.sel, Some(d(2026, 4, 30)));
        c.pointer(down(p.x() + 2, p.y() + 2, 1), &ctx);
        c.pointer(down(p.x() + 2, p.y() + 2, 1), &ctx);
        assert_eq!(c.sel, Some(d(2026, 2, 28)));
        c.pointer(PointerEvent::Move { x: n.x() + 1, y: n.y() + 1 }, &ctx);
        assert_eq!(c.hover, Some(true));
        // A day cell: the first cell of the grid is the Sunday-or-Monday that starts the month view.
        let first = month_grid(2026, 2)[0];
        c.pointer(down(GRID_X + 2, DAYS_Y + 2, 1), &ctx);
        assert_eq!(c.sel, Some(first));
        // Wheel = month; a double click = today.
        c.pointer(PointerEvent::Wheel { x: 50, y: 80, dy: -1 }, &ctx);
        assert_eq!(c.sel, Some(add_months(first, 1)));
        c.pointer(down(GRID_X + 2, DAYS_Y + 2, 2), &ctx);
        assert_eq!(c.sel, Some(ctx.now.date_naive()));
    }

    #[test]
    fn right_click_selects_the_day_and_offers_month_commands() {
        use crate::font::FontSystem;
        use crate::system::mock::MockBackend;
        use crate::theme::{Theme, ThemeManager};
        let fs = FontSystem::new();
        let tm = ThemeManager::new(Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = MockBackend::new();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut c = Calendar { sel: Some(d(2026, 3, 15)), hover: None };
        let items = c.context_menu(GRID_X + 2, DAYS_Y + 2, &ctx).unwrap();
        assert_eq!(items.iter().map(|i| i.id).collect::<Vec<_>>(), ["today", "prev", "next"]);
        assert_eq!(c.sel, Some(month_grid(2026, 3)[0]));
        c.menu_action("next", &ctx);
        assert_eq!(c.sel.unwrap().month(), 4);
    }

    #[test]
    fn help_rows_are_filled_in() {
        crate::launcher::help::check_app_rows("Calendar", &Calendar { sel: None, hover: None }.help());
    }
}
