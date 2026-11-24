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
const HEAD_Y: i32 = grid::BAR_H + grid::MARGIN;
const WD_Y: i32 = HEAD_Y + grid::ROW_H;
/// The weekday band is 17 px. On the 320 x 170 screen every week is 19 px, so the six weeks end on
/// y 166 and the grid, the side panel and its shadow (y 168) use the screen down to its bottom.
const WD_H: i32 = 17;
const DAYS_Y: i32 = WD_Y + WD_H;
/// Rows kept free under the last week: its rule is the week's last row, then the panel's 2 px shadow.
const BOTTOM_GAP: i32 = 3;
/// The side panel never gets narrower than on the 320 x 170 screen (108 px); on a wide window it
/// takes a quarter of the width, so the month grid gets most of it.
const PANEL_MIN_W: i32 = 108;
/// Between the grid and the panel.
const PANEL_GAP: i32 = grid::GUTTER * 2;

/// Where the month grid and the side panel sit on a surface. Cells are whole pixels: the 7 columns
/// and 6 weeks share the room evenly and the pixels left over go to the panel (width) or stay empty
/// under the grid (height). At 320 x 170 this is the original layout: 28 x 19 cells, panel at x 208.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    cell_w: i32,
    day_h: i32,
    panel_x: i32,
    panel_w: i32,
}

impl Layout {
    fn for_size(w: i32, h: i32) -> Layout {
        // From the grid's left edge to the scroll column's right edge, like every other screen.
        let room = w - grid::MARGIN - GRID_X;
        let panel_min = PANEL_MIN_W.max(room / 4);
        let cell_w = ((room - PANEL_GAP - panel_min) / 7).max(1);
        let day_h = ((h - BOTTOM_GAP - DAYS_Y) / 6).max(1);
        let panel_x = GRID_X + 7 * cell_w + PANEL_GAP;
        Layout { cell_w, day_h, panel_x, panel_w: GRID_X + room - panel_x }
    }

    /// The layout for the surface being drawn now.
    fn now() -> Layout {
        Layout::for_size(grid::sw(), grid::sh())
    }

    fn grid_w(&self) -> i32 {
        7 * self.cell_w
    }

    /// The six weeks (below the weekday band).
    fn days(&self) -> Rect {
        Rect::new(GRID_X, DAYS_Y, self.grid_w() as u32, (6 * self.day_h) as u32)
    }
}

impl Calendar {
    fn sel(&self, ctx: &Ctx) -> NaiveDate {
        self.sel.unwrap_or_else(|| ctx.now.date_naive())
    }

    fn cell_at(&self, x: i32, y: i32, ctx: &Ctx) -> Option<NaiveDate> {
        let l = Layout::now();
        if !contains(l.days(), x, y) {
            return None;
        }
        let s = self.sel(ctx);
        let (col, row) = ((x - GRID_X) / l.cell_w, (y - DAYS_Y) / l.day_h);
        Some(month_grid(s.year(), s.month())[(row * 7 + col) as usize])
    }
}

/// The month arrows at the right end of the title row.
fn arrow_rect(next: bool) -> Rect {
    let right = GRID_X + Layout::now().grid_w();
    Rect::new(right - 18 * (2 - next as i32), HEAD_Y, 16, grid::ROW_H as u32)
}

impl LauncherApp for Calendar {
    fn sizing(&self) -> crate::launcher::app::Sizing {
        crate::launcher::app::Sizing::Flexible
    }

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
        ui::fill_rect(c, Rect::new(0, grid::BAR_H, grid::sw() as u32, (grid::sh() - grid::BAR_H) as u32), tm.get_color("home_bg"));

        let l = Layout::now();
        let (cell_w, day_h) = (l.cell_w, l.day_h);
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
        let wd = Rect::new(GRID_X, WD_Y, l.grid_w() as u32, WD_H as u32);
        // A black-and-white panel prints the band's tone as dots, right through the letters: there
        // the weekdays stand on the plain page (the heavy rule below still closes the row).
        let mono = crate::launcher::settings::panel_is_mono();
        if !mono {
            ui::fill_rect(c, wd, alt);
        }
        for (i, name) in WEEKDAYS.iter().enumerate() {
            let s: String = if ja { JA_WD[i].to_string() } else { name.chars().take(2).collect() };
            let w = fs.measure_text(&s);
            let col = if i == 0 || i == 6 { faint } else { ink };
            fs.draw_text(c, GRID_X + i as i32 * cell_w + (cell_w - w) / 2, WD_Y + 1, &s, col);
        }

        // Days.
        let area = l.days();
        ui::fill_rect(c, area, paper);
        // Grid lines: a rule under every week and between every day column (the plates sit inside).
        for k in 1..7 {
            ui::fill_rect(c, Rect::new(GRID_X + k * cell_w, WD_Y, 1, area.height() + WD_H as u32), line);
        }
        for k in 0..6 {
            ui::fill_rect(c, Rect::new(GRID_X, DAYS_Y + (k + 1) * day_h - 1, area.width(), 1), line);
        }
        for (i, d) in month_grid(y, m).iter().enumerate() {
            let r = Rect::new(GRID_X + (i as i32 % 7) * cell_w, DAYS_Y + (i as i32 / 7) * day_h, cell_w as u32, day_h as u32);
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
            // The dates are in the display face (bold), like the big day number in the side panel.
            let w = fs.measure_display_text(&s);
            let col = if is_sel { widgets::selection(tm).fg } else if in_month { ink } else { faint };
            fs.draw_display_text(c, r.x() + (cell_w - w + 1) / 2, widgets::text_origin(r.y(), r.height() as i32) - 1, &s, col);
            // The days of the neighbouring months are in the fainter ink, clean. Only where the
            // theme has no fainter ink (black and white) are they thinned with a dither instead.
            if !in_month && !is_sel && (faint == ink || mono) {
                dither::fill(c, cell, paper, Pattern::Quarter);
            }
        }
        ui::draw_rect(c, area, line);
        // The weekday band is closed by a heavier (ink) rule.
        ui::fill_rect(c, Rect::new(GRID_X, DAYS_Y - 1, area.width(), 1), ink);

        // Side panel: the selected day.
        let panel = Rect::new(l.panel_x, WD_Y, l.panel_w as u32, (DAYS_Y + 6 * day_h - WD_Y) as u32);
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
            // In the middle of its row (between the rule above and the one below), kanji included.
            let y = crate::launcher::widgets::text_origin(panel.y() + 41 + i as i32 * grid::ROW_H, grid::ROW_H - 1);
            fs.draw_text(c, panel.x() + 6, y, l, if i == 0 { ink } else { faint });
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
    fn the_layout_at_320x170_is_the_original_one() {
        let l = Layout::for_size(grid::SCREEN_W, grid::SCREEN_H);
        assert_eq!(l, Layout { cell_w: 28, day_h: 19, panel_x: 208, panel_w: 108 });
        // The panel still ends on the scroll column's right edge, the last week's rule on y 166.
        assert_eq!(l.panel_x + l.panel_w, grid::SCROLL_X + grid::SCROLL_W);
        assert_eq!(l.days().bottom() - 1, 166);
    }

    #[test]
    fn a_bigger_window_gets_bigger_cells_that_fit() {
        for (w, h) in [(640, 400), (800, 480), (321, 171), (1920, 1080)] {
            let l = Layout::for_size(w, h);
            let small = Layout::for_size(grid::SCREEN_W, grid::SCREEN_H);
            assert!(l.cell_w >= small.cell_w && l.day_h >= small.day_h, "{w}x{h}: {l:?}");
            assert!(l.panel_w >= PANEL_MIN_W, "{w}x{h}: the panel keeps its minimum width");
            assert!(l.grid_w() > l.panel_w, "{w}x{h}: the grid gets most of the width");
            assert_eq!(l.panel_x + l.panel_w, w - grid::MARGIN, "{w}x{h}: the panel ends at the right margin");
            assert!(l.days().bottom() + BOTTOM_GAP <= h, "{w}x{h}: the weeks and the panel's shadow fit");
            // Only a few pixels are left over: no room for a taller week.
            assert!(h - BOTTOM_GAP - l.days().bottom() < 6, "{w}x{h}");
        }
        let l = Layout::for_size(640, 400);
        assert!(l.cell_w > 28 && l.day_h > 19, "{l:?}");
    }

    #[test]
    fn clicks_hit_the_cell_drawn_at_that_spot_in_a_big_window() {
        use crate::font::FontSystem;
        use crate::system::mock::MockBackend;
        use crate::theme::{Theme, ThemeManager};
        let fs = FontSystem::new();
        let tm = ThemeManager::new(Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = MockBackend::new();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut c = Calendar { sel: Some(d(2026, 3, 15)), hover: None };
        grid::with_surface((640, 400), || {
            let l = Layout::now();
            let g = month_grid(2026, 3);
            // The last cell's far corner (bottom right, 1 px in) is still that cell.
            let (x, y) = (GRID_X + 7 * l.cell_w - 1, DAYS_Y + 6 * l.day_h - 1);
            assert_eq!(c.cell_at(x, y, &ctx), Some(g[41]));
            assert_eq!(c.cell_at(x + 1, y, &ctx), None);
            // Row 2, column 3.
            assert_eq!(c.cell_at(GRID_X + 3 * l.cell_w + 1, DAYS_Y + 2 * l.day_h + 1, &ctx), Some(g[17]));
            // The arrows sit at the grid's right end.
            let n = arrow_rect(true);
            assert_eq!(n.right(), GRID_X + l.grid_w() - 2);
            c.pointer(PointerEvent::Down { x: n.x() + 2, y: n.y() + 2, button: 0, clicks: 1 }, &ctx);
            assert_eq!(c.sel, Some(d(2026, 4, 15)));
        });
    }

    #[test]
    fn help_rows_are_filled_in() {
        crate::launcher::help::check_app_rows("Calendar", &Calendar { sel: None, hover: None }.help());
    }
}
