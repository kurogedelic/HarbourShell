//! Timer: a stopwatch with laps and a countdown timer, drawn with the big
//! seven-segment digits.
//!
//! Left / Right switch between Stopwatch and Timer, Enter or Space start and
//! pause, L takes a lap, R or Backspace resets. In Timer mode Up / Down set the
//! time (1 min, Shift = 10 s) while it is stopped. The clock keeps running when
//! you look at another mode.

use std::time::{Duration, Instant};

use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome;
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::sevenseg;
use crate::launcher::widgets::{self, contains, draw_button, BtnState, MenuItem, PointerEvent};
use crate::platform::keys::arrow_alias;
use crate::ui;

/// A clock that can be started, paused and reset. Time is passed in, so it is testable.
#[derive(Clone, Copy, Debug, Default)]
pub struct Clock {
    acc: Duration,
    since: Option<Instant>,
}

impl Clock {
    pub fn running(&self) -> bool {
        self.since.is_some()
    }
    pub fn elapsed(&self, now: Instant) -> Duration {
        self.acc + self.since.map(|s| now.saturating_duration_since(s)).unwrap_or_default()
    }
    pub fn start(&mut self, now: Instant) {
        if self.since.is_none() {
            self.since = Some(now);
        }
    }
    pub fn pause(&mut self, now: Instant) {
        if let Some(s) = self.since.take() {
            self.acc += now.saturating_duration_since(s);
        }
    }
    pub fn reset(&mut self) {
        *self = Clock::default();
    }
    pub fn untouched(&self) -> bool {
        self.acc.is_zero() && self.since.is_none()
    }
}

/// Split a duration into (hours, minutes, seconds, tenths).
pub fn split(d: Duration) -> (u64, u64, u64, u64) {
    let t = d.as_millis() as u64 / 100;
    (t / 36000, t / 600 % 60, t / 10 % 60, t % 10)
}

pub fn format_lap(d: Duration) -> String {
    let (h, m, s, t) = split(d);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{t}")
    } else {
        format!("{m:02}:{s:02}.{t}")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Stopwatch,
    Timer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Toggle,
    Lap,
    Reset,
}

pub struct TimerApp {
    mode: Mode,
    sw: Clock,
    laps: Vec<(Duration, Duration)>, // (total, split)
    cd: Clock,
    cd_total: Duration,
    hover: Option<Act>,
}

pub fn make() -> Box<dyn LauncherApp> {
    Box::new(TimerApp::new())
}

const MIN_SET: Duration = Duration::from_secs(10);
const MAX_SET: Duration = Duration::from_secs(99 * 60 + 59);

const X0: i32 = grid::MARGIN;
/// The page starts under the shared folder tab strip (y 36).
const DIGIT_Y: i32 = widgets::FOLDER_BODY_Y + grid::GUTTER * 2;
/// The page uses the screen down to y 168 (`grid::APP_FRAME_BOTTOM`): the buttons sit so that
/// the default button's 3 px ring ends on y 168.
const BTN_Y: i32 = grid::APP_FRAME_BOTTOM + 1 - 3 - grid::BTN_H;
const BTN_W: i32 = 56;
/// The well holds 8 rows of 16 px inside its 1 px frame (y 39..168), its bottom line on y 168.
const WELL_ROWS: i32 = 8;
const WELL_Y: i32 = grid::APP_FRAME_BOTTOM + 1 - (WELL_ROWS * grid::ROW_H + 2);
fn well_rect() -> Rect {
    Rect::new(196, WELL_Y, 116, (WELL_ROWS * grid::ROW_H + 2) as u32)
}

impl TimerApp {
    pub fn new() -> Self {
        TimerApp { mode: Mode::Stopwatch, sw: Clock::default(), laps: vec![], cd: Clock::default(), cd_total: Duration::from_secs(5 * 60), hover: None }
    }

    fn remaining(&self, now: Instant) -> Duration {
        self.cd_total.saturating_sub(self.cd.elapsed(now))
    }

    fn finished(&self, now: Instant) -> bool {
        !self.cd.untouched() && self.remaining(now).is_zero()
    }

    fn toggle(&mut self, now: Instant) {
        match self.mode {
            Mode::Stopwatch => {
                if self.sw.running() {
                    self.sw.pause(now)
                } else {
                    self.sw.start(now)
                }
            }
            Mode::Timer => {
                if self.finished(now) {
                    self.cd.reset();
                } else if self.cd.running() {
                    self.cd.pause(now)
                } else {
                    self.cd.start(now)
                }
            }
        }
    }

    fn lap(&mut self, now: Instant) {
        if self.mode == Mode::Stopwatch && self.sw.running() {
            let total = self.sw.elapsed(now);
            let prev = self.laps.last().map(|l| l.0).unwrap_or_default();
            self.laps.push((total, total.saturating_sub(prev)));
        }
    }

    fn reset(&mut self) {
        match self.mode {
            Mode::Stopwatch => {
                self.sw.reset();
                self.laps.clear();
            }
            Mode::Timer => self.cd.reset(),
        }
    }

    fn adjust(&mut self, up: bool, fine: bool) {
        self.adjust_by(up, if fine { 10 } else { 60 });
    }

    fn adjust_by(&mut self, up: bool, secs: u64) {
        if self.mode != Mode::Timer || !self.cd.untouched() {
            return;
        }
        let step = Duration::from_secs(secs);
        self.cd_total = if up { (self.cd_total + step).min(MAX_SET) } else { self.cd_total.saturating_sub(step).max(MIN_SET) };
    }

    /// The row starts under the progress bar's left end (X0 + 3): the default button's ring then
    /// stands on the margin instead of left of everything above it.
    fn buttons(&self, now: Instant) -> Vec<(Rect, Act, String)> {
        let (running, done) = match self.mode {
            Mode::Stopwatch => (self.sw.running(), false),
            Mode::Timer => (self.cd.running(), self.finished(now)),
        };
        let first = if done { "Clear" } else if running { if self.mode == Mode::Timer { "Pause" } else { "Stop" } } else { "Start" };
        let mut v = vec![(Act::Toggle, first)];
        if self.mode == Mode::Stopwatch {
            v.push((Act::Lap, "Lap"));
        }
        v.push((Act::Reset, "Reset"));
        v.into_iter().enumerate().map(|(i, (a, l))| (Rect::new(X0 + 3 + i as i32 * (BTN_W + grid::GUTTER), BTN_Y, BTN_W as u32, grid::BTN_H as u32), a, l.to_string())).collect()
    }

    fn act(&mut self, a: Act, now: Instant) {
        match a {
            Act::Toggle => self.toggle(now),
            Act::Lap => self.lap(now),
            Act::Reset => self.reset(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Minutes,
    Seconds,
}

fn mode_labels() -> [&'static str; 2] {
    [crate::launcher::i18n::tr("Stopwatch"), crate::launcher::i18n::tr("Timer")]
}

fn mode_tabs<'a>(ctx: &Ctx, labels: &'a [&'a str], mode: Mode) -> widgets::Tabs<'a> {
    widgets::Tabs::folder_fit(ctx.fs, labels, if mode == Mode::Stopwatch { 0 } else { 1 }, ctx.tm)
}

const DIGIT_W: i32 = 28;
const DIGIT_H: i32 = 56;

/// Which pair of digits a point is over (and whether it is in the upper half).
fn digit_zone(x: i32, y: i32) -> Option<(Unit, bool)> {
    if y < DIGIT_Y || y >= DIGIT_Y + DIGIT_H {
        return None;
    }
    let up = y < DIGIT_Y + DIGIT_H / 2;
    let m0 = X0 + 4;
    let m1 = m0 + 2 * DIGIT_W + 4; // end of the minutes pair (the gap between the digits is 4)
    let s0 = m1 + 6 + 8; // the colon: its thickness (6) and 8 px of space
    let s1 = s0 + 2 * DIGIT_W + 4;
    if (m0..m1).contains(&x) {
        Some((Unit::Minutes, up))
    } else if (s0..s1).contains(&x) {
        Some((Unit::Seconds, up))
    } else {
        None
    }
}

impl Default for TimerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl LauncherApp for TimerApp {
    fn takes_text(&self) -> bool {
        false
    }

    /// Tab switches Stopwatch / Timer, so a double Tab here is not the Agent (Alt+Tab is).
    fn wants_tab(&self) -> bool {
        true
    }

    fn title(&self) -> String {
        crate::launcher::i18n::tr_s("Timer")
    }

    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Tab", "Stopwatch / Timer"),
            k("Return", "Start / pause"),
            k("L", "Lap (Stopwatch)"),
            k("R", "Reset"),
            k("Up / Down", "+/- 1 min (Timer, stopped)"),
            k("Shift + Up / Down", "+/- 10 s"),
            k("Click the digits", "+/- (upper / lower half)"),
        ]
    }

    fn describe(&self) -> String {
        let now = Instant::now();
        match self.mode {
            Mode::Stopwatch => format!(
                "Timer: stopwatch {} {}, {} laps",
                format_lap(self.sw.elapsed(now)),
                if self.sw.running() { "running" } else { "stopped" },
                self.laps.len()
            ),
            Mode::Timer => format!(
                "Timer: countdown {} left of {} ({})",
                format_lap(self.remaining(now)),
                format_lap(self.cd_total),
                if self.finished(now) { "finished" } else if self.cd.running() { "running" } else { "stopped" }
            ),
        }
    }

    fn menu(&self) -> Vec<MenuItem> {
        vec![MenuItem::new("start", "Start / Pause"), MenuItem::new("lap", "Lap"), MenuItem::new("reset", "Reset")]
    }

    fn menu_action(&mut self, id: &str, _ctx: &Ctx) -> AppAction {
        let now = Instant::now();
        match id {
            "start" => self.toggle(now),
            "lap" => self.lap(now),
            "reset" => self.reset(),
            _ => {}
        }
        AppAction::None
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, _ctx: &Ctx) -> AppAction {
        let now = Instant::now();
        let fine = mods.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
        match arrow_alias(key) {
            Keycode::Escape => return AppAction::Quit,
            // Tab / Shift+Tab (or Left / Right): the other tab, as in every tabbed app.
            Keycode::Left | Keycode::Right | Keycode::Tab => self.mode = if self.mode == Mode::Stopwatch { Mode::Timer } else { Mode::Stopwatch },
            Keycode::Up => self.adjust(true, fine),
            Keycode::Down => self.adjust(false, fine),
            Keycode::Return | Keycode::KpEnter | Keycode::Space => self.toggle(now),
            Keycode::L => self.lap(now),
            Keycode::R | Keycode::Backspace => self.reset(),
            _ => {}
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> AppAction {
        let now = Instant::now();
        match ev {
            PointerEvent::Move { x, y } | PointerEvent::Drag { x, y, .. } => {
                self.hover = self.buttons(now).into_iter().find(|(r, _, _)| contains(*r, x, y)).map(|b| b.1);
            }
            PointerEvent::Down { x, y, button: 0, .. } => {
                let labels = mode_labels();
                match mode_tabs(ctx, &labels, self.mode).hit(x, y) {
                    Some(0) => self.mode = Mode::Stopwatch,
                    Some(_) => self.mode = Mode::Timer,
                    None => {}
                }
                // Timer set-up: the top half of the minutes / seconds digits adds, the bottom half subtracts.
                if let Some((unit, up)) = digit_zone(x, y) {
                    self.adjust_by(up, if unit == Unit::Minutes { 60 } else { 10 });
                }
                if let Some((_, a, _)) = self.buttons(now).into_iter().find(|(r, _, _)| contains(*r, x, y)) {
                    self.act(a, now);
                }
            }
            PointerEvent::Wheel { x, y, dy } => {
                // Over the seconds the wheel steps 10 s, elsewhere a minute.
                let secs = if matches!(digit_zone(x, y), Some((Unit::Seconds, _))) { 10 } else { 60 };
                self.adjust_by(dy > 0, secs);
            }
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, _ctx: &Ctx) -> bool {
        let now = Instant::now();
        if self.cd.running() && self.remaining(now).is_zero() {
            // Freeze at zero; the display blinks until cleared.
            self.cd.pause(now);
        }
        self.sw.running() || self.cd.running() || self.finished(now)
    }

    fn draw(&mut self, c: &mut Canvas<Window>, _assets: &mut AssetStore, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let now = Instant::now();
        let ink = tm.get_color("foreground");
        let faint = crate::launcher::widgets::secondary_ink(tm);
        let sunk = tm.get_color("surface_sunken");
        let alt = tm.get_color("surface_alt");
        ui::fill_rect(c, chrome::app_rect(), tm.get_color("home_bg"));

        // Mode tabs: the shared folder strip.
        let labels = mode_labels();
        mode_tabs(ctx, &labels, self.mode).draw_strip(c, fs, tm);

        // Digits.
        let (shown, blink_off) = match self.mode {
            Mode::Stopwatch => (self.sw.elapsed(now), false),
            Mode::Timer => (self.remaining(now), self.finished(now) && ctx.now.timestamp_subsec_millis() % 800 > 400),
        };
        let (h, m, s, t) = split(shown);
        let col = if blink_off { faint } else { ink };
        let (dw, dh, th) = (DIGIT_W, DIGIT_H, 6);
        let mut x = X0 + 4;
        for (i, d) in [m / 10, m % 10, s / 10, s % 10].into_iter().enumerate() {
            sevenseg::draw_digit(c, x, DIGIT_Y, dw, dh, th, d as usize, col);
            x += dw + 4;
            if i == 1 {
                sevenseg::draw_colon(c, x + 2, DIGIT_Y, dh, th, col);
                x += th + 8;
            }
        }
        let tx = x + 12;
        ui::fill_rect(c, Rect::new(tx - 6, DIGIT_Y + dh - 3, 3, 3), col);
        sevenseg::draw_digit(c, tx, DIGIT_Y + dh - 20, 12, 20, 3, t as usize, col);
        if h > 0 {
            fs.draw_text(c, tx, DIGIT_Y, &format!("{h}h"), faint);
        }

        // The shared progress bar: the time left (timer), empty for the stopwatch. Its outline sits
        // 1 px inside the rectangle (a light ring around it), at X0 + 4, 172 x 8.
        let frac = if self.mode == Mode::Timer && !self.cd_total.is_zero() { self.remaining(now).as_secs_f32() / self.cd_total.as_secs_f32() } else { 0.0 };
        widgets::ProgressBar::new(Rect::new(X0 + 3, DIGIT_Y + dh + 11, 174, 10), Some(frac), 0).draw(c, tm);

        // Buttons.
        for (r, a, label) in self.buttons(now) {
            draw_button(c, fs, tm, r, &label, BtnState { hover: self.hover == Some(a), default: a == Act::Toggle, ..Default::default() });
        }

        // Side well: laps or hints.
        let well = well_rect();
        ui::fill_rect(c, well, sunk);
        ui::draw_rect(c, well, ink);
        let rows = ((well.height() as i32 - 2) / grid::ROW_H) as usize;
        if self.mode == Mode::Stopwatch {
            // Header band ("Laps") on the zebra tone.
            ui::fill_rect(c, Rect::new(well.x() + 1, well.y() + 1, well.width() - 2, grid::ROW_H as u32 - 1), alt);
        }
        // A rule under every 16 px row of the well.
        for k in 0..rows as i32 {
            widgets::draw_row_rule(c, tm, Rect::new(well.x() + 1, well.y() + 1 + k * grid::ROW_H, well.width() - 2, grid::ROW_H as u32));
        }
        match self.mode {
            Mode::Stopwatch => {
                fs.draw_text(c, well.x() + 6, well.y() + 1, crate::launcher::i18n::tr("Laps"), faint);
                for (k, (i, (tot, sp))) in self.laps.iter().enumerate().rev().take(rows - 1).enumerate() {
                    let y = well.y() + 1 + (k as i32 + 1) * grid::ROW_H;
                    fs.draw_text(c, well.x() + 6, y + 1, &format!("{}", i + 1), faint);
                    fs.draw_text(c, well.x() + 22, y + 1, &format_lap(*sp), ink);
                    let t = format_lap(*tot);
                    fs.draw_text(c, well.right() - 6 - fs.measure_text(&t), y + 1, &t, faint);
                }
            }
            Mode::Timer => {
                // When it rings (wall clock), while it counts down or waits paused.
                let left = self.remaining(now);
                if !left.is_zero() && !self.cd.untouched() {
                    let at = ctx.now + chrono::Duration::from_std(left).unwrap_or_default();
                    let label = if self.cd.running() { "Rings at" } else { "Would ring at" };
                    fs.draw_text(c, well.x() + 6, well.y() + 1, crate::launcher::i18n::tr(label), faint);
                    let t = at.format("%H:%M:%S").to_string();
                    fs.draw_text(c, well.x() + 6, well.y() + 1 + grid::ROW_H, &t, ink);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_into_clock_parts() {
        assert_eq!(split(Duration::from_millis(0)), (0, 0, 0, 0));
        assert_eq!(split(Duration::from_millis(61_250)), (0, 1, 1, 2));
        assert_eq!(split(Duration::from_secs(3 * 3600 + 5 * 60 + 9)), (3, 5, 9, 0));
        assert_eq!(format_lap(Duration::from_millis(61_250)), "01:01.2");
        assert_eq!(format_lap(Duration::from_secs(3661)), "1:01:01.0");
    }

    #[test]
    fn the_clock_starts_pauses_and_resets() {
        let t0 = Instant::now();
        let mut c = Clock::default();
        assert!(c.untouched());
        c.start(t0);
        assert_eq!(c.elapsed(t0 + Duration::from_secs(5)), Duration::from_secs(5));
        c.pause(t0 + Duration::from_secs(5));
        assert_eq!(c.elapsed(t0 + Duration::from_secs(60)), Duration::from_secs(5), "paused time stands still");
        c.start(t0 + Duration::from_secs(60));
        assert_eq!(c.elapsed(t0 + Duration::from_secs(62)), Duration::from_secs(7));
        c.reset();
        assert!(c.untouched());
    }

    #[test]
    fn stopwatch_laps_record_totals_and_splits() {
        let t0 = Instant::now();
        let mut a = TimerApp::new();
        a.toggle(t0);
        a.lap(t0 + Duration::from_secs(10));
        a.lap(t0 + Duration::from_secs(25));
        assert_eq!(a.laps, vec![(Duration::from_secs(10), Duration::from_secs(10)), (Duration::from_secs(25), Duration::from_secs(15))]);
        a.reset();
        assert!(a.laps.is_empty() && a.sw.untouched());
    }

    #[test]
    fn countdown_counts_down_and_finishes() {
        let t0 = Instant::now();
        let mut a = TimerApp::new();
        a.mode = Mode::Timer;
        a.cd_total = Duration::from_secs(30);
        assert!(!a.finished(t0));
        a.toggle(t0);
        assert_eq!(a.remaining(t0 + Duration::from_secs(10)), Duration::from_secs(20));
        assert!(!a.finished(t0 + Duration::from_secs(10)));
        assert!(a.finished(t0 + Duration::from_secs(31)));
        assert_eq!(a.remaining(t0 + Duration::from_secs(31)), Duration::ZERO);
    }

    #[test]
    fn setting_the_time_is_clamped_and_locked_while_running() {
        let mut a = TimerApp::new();
        a.mode = Mode::Timer;
        a.adjust(true, false);
        assert_eq!(a.cd_total, Duration::from_secs(6 * 60));
        for _ in 0..200 {
            a.adjust(true, false);
        }
        assert_eq!(a.cd_total, MAX_SET);
        for _ in 0..700 {
            a.adjust(false, true);
        }
        assert_eq!(a.cd_total, MIN_SET);
        a.toggle(Instant::now());
        a.adjust(true, false);
        assert_eq!(a.cd_total, MIN_SET, "no changes once started");
    }

    #[test]
    fn digit_zones_split_minutes_and_seconds_into_upper_and_lower_halves() {
        let m = X0 + 4 + 10;
        let s = X0 + 82 + 10;
        assert_eq!(digit_zone(m, DIGIT_Y + 3), Some((Unit::Minutes, true)));
        assert_eq!(digit_zone(m, DIGIT_Y + DIGIT_H - 3), Some((Unit::Minutes, false)));
        assert_eq!(digit_zone(s, DIGIT_Y + 3), Some((Unit::Seconds, true)));
        assert_eq!(digit_zone(X0 + 70, DIGIT_Y + 3), None, "the colon");
        assert_eq!(digit_zone(m, DIGIT_Y - 1), None);
        assert_eq!(digit_zone(m, DIGIT_Y + DIGIT_H), None);
    }

    #[test]
    fn clicks_set_the_timer_and_press_buttons() {
        use crate::font::FontSystem;
        use crate::system::mock::MockBackend;
        use crate::theme::{Theme, ThemeManager};
        let fs = FontSystem::new();
        let tm = ThemeManager::new(Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = MockBackend::new();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut t = TimerApp::new();
        let down = |x, y| PointerEvent::Down { x, y, button: 0, clicks: 1 };
        // Tab "Timer".
        t.pointer(down(X0 + widgets::FOLDER_MIN_TAB_W * 2 - 3, grid::BAR_H + 10), &ctx);
        assert_eq!(t.mode, Mode::Timer);
        let base = t.cd_total;
        t.pointer(down(X0 + 4 + 10, DIGIT_Y + 3), &ctx);
        assert_eq!(t.cd_total, base + Duration::from_secs(60));
        t.pointer(down(X0 + 82 + 10, DIGIT_Y + DIGIT_H - 3), &ctx);
        assert_eq!(t.cd_total, base + Duration::from_secs(50));
        t.pointer(PointerEvent::Wheel { x: X0 + 82 + 10, y: DIGIT_Y + 5, dy: 1 }, &ctx);
        assert_eq!(t.cd_total, base + Duration::from_secs(60), "wheel over the seconds steps 10 s");
        // Start button, then the digits no longer change the set time while it runs.
        let (r, _, _) = t.buttons(Instant::now()).remove(0);
        t.pointer(down(r.x() + 2, r.y() + 2), &ctx);
        assert!(t.cd.running());
        let set = t.cd_total;
        t.pointer(down(X0 + 4 + 10, DIGIT_Y + 3), &ctx);
        assert_eq!(t.cd_total, set);
        // Back to the stopwatch tab.
        t.pointer(down(X0 + 3, grid::BAR_H + 10), &ctx);
        assert_eq!(t.mode, Mode::Stopwatch);
    }
}
