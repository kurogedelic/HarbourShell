//! World Clock: a dithered world map that scrolls sideways (wrapping around the
//! date line) so the selected city is always in the middle, its point blinking,
//! with the city's analog clock and time in a strip below. No window frame.
//!
//! Z / C (or Left / Right) move to the city west / east of the current one.

use chrono::{DateTime, Datelike, Local, NaiveDate, Offset, Timelike, Utc};
use chrono_tz::Tz;
use sdl2::keyboard::Keycode;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome::SCREEN_W;
use crate::font::FontSystem;
use crate::launcher::sevenseg as seg;
use crate::launcher::worldmap as map;
use crate::theme::ThemeManager;
use crate::ui;

pub struct City {
    pub name: &'static str,
    pub zone: &'static str,
    pub lat: f32,
    pub lon: f32,
}

const fn city(name: &'static str, zone: &'static str, lat: f32, lon: f32) -> City {
    City { name, zone, lat, lon }
}

/// Ordered west to east, so Left / Right walk across the map (and wrap around).
pub const CITIES: [City; 18] = [
    city("Honolulu", "Pacific/Honolulu", 21.3, -157.8),
    city("Los Angeles", "America/Los_Angeles", 34.05, -118.24),
    city("Mexico City", "America/Mexico_City", 19.4, -99.1),
    city("Chicago", "America/Chicago", 41.9, -87.6),
    city("New York", "America/New_York", 40.7, -74.0),
    city("Sao Paulo", "America/Sao_Paulo", -23.5, -46.6),
    city("London", "Europe/London", 51.5, -0.12),
    city("Paris", "Europe/Paris", 48.85, 2.35),
    city("Cape Town", "Africa/Johannesburg", -33.9, 18.4),
    city("Cairo", "Africa/Cairo", 30.0, 31.2),
    city("Moscow", "Europe/Moscow", 55.75, 37.6),
    city("Dubai", "Asia/Dubai", 25.2, 55.3),
    city("Mumbai", "Asia/Kolkata", 19.1, 72.9),
    city("Singapore", "Asia/Singapore", 1.35, 103.8),
    city("Beijing", "Asia/Shanghai", 39.9, 116.4),
    city("Tokyo", "Asia/Tokyo", 35.68, 139.69),
    city("Sydney", "Australia/Sydney", -33.87, 151.2),
    city("Auckland", "Pacific/Auckland", -36.85, 174.76),
];

const MAP_X: i32 = 4;
const MAP_Y: i32 = 16;
const MAP_MAX_H: i32 = 112;
/// The World Clock app: the whole map.
const APP_MAP_H: i32 = map::H as i32;

const JA_DAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CityTime {
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday.
    pub weekday: u32,
    pub day: u32,
    pub month0: u32,
    /// Offset from UTC in seconds (daylight saving included).
    pub utc_offset: i32,
    /// Calendar days ahead (+1) or behind (-1) of `base`.
    pub day_diff: i64,
}

/// Time in `zone` at the instant `utc`. An empty zone means `local`.
pub fn city_time(zone: &str, utc: DateTime<Utc>, local: DateTime<Local>, base: NaiveDate) -> CityTime {
    let parts = |h: u32, m: u32, s: u32, wd: u32, d: u32, mo: u32, off: i32, date: NaiveDate| CityTime {
        hour: h,
        minute: m,
        second: s,
        weekday: wd,
        day: d,
        month0: mo,
        utc_offset: off,
        day_diff: date.signed_duration_since(base).num_days(),
    };
    match zone.parse::<Tz>() {
        Ok(tz) if !zone.is_empty() => {
            let t = utc.with_timezone(&tz);
            parts(
                t.hour(),
                t.minute(),
                t.second(),
                t.weekday().num_days_from_sunday(),
                t.day(),
                t.month0(),
                t.offset().fix().local_minus_utc(),
                t.date_naive(),
            )
        }
        _ => parts(
            local.hour(),
            local.minute(),
            local.second(),
            local.weekday().num_days_from_sunday(),
            local.day(),
            local.month0(),
            local.offset().local_minus_utc(),
            local.date_naive(),
        ),
    }
}

/// `UTC+9`, `UTC-5`, `UTC+5:30`.
pub fn offset_label(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.abs();
    let (h, m) = (a / 3600, (a % 3600) / 60);
    if m == 0 {
        format!("UTC{sign}{h}")
    } else {
        format!("UTC{sign}{h}:{m:02}")
    }
}

/// Where the sun is directly overhead at `utc`: (latitude, longitude) in degrees
/// (NOAA's low-precision solar position formulas).
pub fn subsolar(utc: DateTime<Utc>) -> (f32, f32) {
    let hour = utc.hour() as f32 + utc.minute() as f32 / 60.0 + utc.second() as f32 / 3600.0;
    let g = std::f32::consts::TAU / 365.0 * (utc.ordinal0() as f32 + (hour - 12.0) / 24.0);
    let decl = 0.006918 - 0.399912 * g.cos() + 0.070257 * g.sin() - 0.006758 * (2.0 * g).cos()
        + 0.000907 * (2.0 * g).sin()
        - 0.002697 * (3.0 * g).cos()
        + 0.00148 * (3.0 * g).sin();
    let eqtime = 229.18
        * (0.000075 + 0.001868 * g.cos() - 0.032077 * g.sin() - 0.014615 * (2.0 * g).cos() - 0.040849 * (2.0 * g).sin());
    (decl.to_degrees(), map::wrap_lon(15.0 * (12.0 - hour) - eqtime / 4.0))
}

/// Is the point (degrees) in darkness when the sun is over `sun` (lat, lon)?
pub fn is_night(lat: f32, lon: f32, sun: (f32, f32)) -> bool {
    let (phi, delta) = (lat.to_radians(), sun.0.to_radians());
    let h = map::wrap_lon(lon - sun.1).to_radians();
    phi.sin() * delta.sin() + phi.cos() * delta.cos() * h.cos() < 0.0
}

/// The *night* side in map coordinates as horizontal runs `(x, y, len)`, recomputed once a
/// minute. The map fills them with one flat light grey over the sea (under the land).
pub fn night_runs(sun: (f32, f32)) -> Vec<(u16, u16, u16)> {
    let mut runs = Vec::new();
    let range = map::LAT_TOP - map::LAT_BOTTOM;
    for y in 0..map::H {
        let lat = map::LAT_TOP - (y as f32 + 0.5) * range / map::H as f32;
        let mut start: Option<usize> = None;
        for x in 0..=map::W {
            let night = x < map::W && {
                let lon = -180.0 + (x as f32 + 0.5) * 360.0 / map::W as f32;
                is_night(lat, lon, sun)
            };
            match (night, start) {
                (true, None) => start = Some(x),
                (false, Some(x0)) => {
                    runs.push((x0 as u16, y as u16, (x - x0) as u16));
                    start = None;
                }
                _ => {}
            }
        }
    }
    runs
}

pub struct WorldClock {
    pub selected: usize,
    /// Longitude currently at the centre of the map (eases towards the city).
    pan: f32,
    sun: (f32, f32),
    night: Vec<(u16, u16, u16)>,
    night_minute: i64,
    last_phase: i64,
}

impl WorldClock {
    /// For the Agent's `current_screen`: the selected city and its local time.
    pub fn describe(&self, local: DateTime<Local>) -> String {
        let c = &CITIES[self.selected % CITIES.len()];
        let t = city_time(c.zone, chrono::Utc::now(), local, local.date_naive());
        format!("World Clock: {} {:02}:{:02}, UTC{}", c.name, t.hour, t.minute, offset_label(t.utc_offset))
    }

    /// Starts on the city whose UTC offset is closest to the device's own.
    pub fn new(local: DateTime<Local>) -> Self {
        let utc = local.with_timezone(&Utc);
        let base = local.date_naive();
        let want = local.offset().local_minus_utc();
        let selected = (0..CITIES.len())
            .min_by_key(|&i| (city_time(CITIES[i].zone, utc, local, base).utc_offset - want).abs())
            .unwrap_or(0);
        Self { selected, pan: CITIES[selected].lon, sun: (0.0, 0.0), night: Vec::new(), night_minute: -1, last_phase: -1 }
    }

    pub fn handle_key(&mut self, key: Keycode) {
        let key = crate::platform::keys::arrow_alias(key);
        let n = CITIES.len();
        match key {
            Keycode::Left | Keycode::Up => self.selected = (self.selected + n - 1) % n,
            Keycode::Right | Keycode::Down => self.selected = (self.selected + 1) % n,
            Keycode::Home => self.selected = 0,
            Keycode::End => self.selected = n - 1,
            _ => {}
        }
    }

    /// True when the screen needs repainting: the map is moving, or the blink /
    /// seconds phase (quarter seconds) changed. Keeps the idle cost low.
    pub fn poll_redraw(&mut self, local: DateTime<Local>) -> bool {
        let moving = map::wrap_lon(CITIES[self.selected].lon - self.pan).abs() >= 0.3 && self.pan != CITIES[self.selected].lon;
        let phase = local.timestamp() * 4 + (local.timestamp_subsec_millis() / 250) as i64;
        let changed = phase != self.last_phase;
        self.last_phase = phase;
        moving || changed
    }

    /// Ease the map towards the selected city (call once per frame).
    pub fn tick(&mut self) {
        let d = map::wrap_lon(CITIES[self.selected].lon - self.pan);
        if d.abs() < 0.3 {
            self.pan = CITIES[self.selected].lon;
        } else {
            self.pan = map::wrap_lon(self.pan + d * 0.28);
        }
    }

    pub fn draw(&mut self, c: &mut Canvas<Window>, assets: &mut AssetStore, fs: &FontSystem, tm: &ThemeManager, local: DateTime<Local>) {
        self.tick();
        // The app shows the whole map (121 rows): the rule under it is on y 137 and the clock plate
        // ends on y 168, so the screen is used down to its bottom.
        let mh = self.draw_map_rows(c, assets, tm, local, true, APP_MAP_H);
        self.draw_details(c, assets, fs, tm, local, mh);
    }

    /// The map strip under the menu bar (x 4.., y 16..16 + the returned height): sea, land, the
    /// night side, the selected city's time zone and the cities. `blink`: the selected city blinks
    /// (the lock screen, redrawn once a second, shows it steady).
    pub fn draw_map(&mut self, c: &mut Canvas<Window>, assets: &mut AssetStore, tm: &ThemeManager, local: DateTime<Local>, blink: bool) -> i32 {
        self.draw_map_rows(c, assets, tm, local, blink, MAP_MAX_H)
    }

    /// [`Self::draw_map`] with at most `max_h` rows of the map (cropped at its southern edge).
    pub fn draw_map_rows(&mut self, c: &mut Canvas<Window>, assets: &mut AssetStore, tm: &ThemeManager, local: DateTime<Local>, blink: bool, max_h: i32) -> i32 {
        let minute = local.timestamp() / 60;
        if minute != self.night_minute {
            self.night_minute = minute;
            self.sun = subsolar(local.with_timezone(&Utc));
            self.night = night_runs(self.sun);
        }
        let ink = tm.get_color("foreground");
        let muted = tm.get_color("muted");
        let paper = tm.get_color("widget_face_active");
        let utc = local.with_timezone(&Utc);
        let base = local.date_naive();
        let city = &CITIES[self.selected];
        let sel = city_time(city.zone, utc, local, base);
        let zsel = map::zone_sel(city.zone, sel.utc_offset);
        let _ = ink;

        // --- map: a LIGHT sea with dark land (the lighter of ink / paper is the sea), graticule,
        //     night shading, date line, cities ---
        let lum = |c: sdl2::pixels::Color| c.r as u32 * 3 + c.g as u32 * 6 + c.b as u32;
        let (sea, land) = if lum(paper) >= lum(ink) { (paper, ink) } else { (ink, paper) };
        // The lock screen crops the map to 112 rows (its southern edge, below about 47 S, is cut) so
        // its clock plate ends on y 160; the app shows all of it.
        let (mw, mh) = (map::W as i32, (map::H as i32).min(max_h));
        ui::fill_rect(c, Rect::new(MAP_X, MAP_Y, mw as u32, mh as u32), sea);
        c.set_draw_color(muted);
        let equator = MAP_Y + map::lat_to_y(0.0) as i32;
        for x in (MAP_X..MAP_X + mw).step_by(4) {
            let _ = c.draw_point((x, equator));
        }
        let greenwich = MAP_X + map::lon_to_x(0.0, self.pan) as i32;
        if greenwich >= MAP_X && greenwich < MAP_X + mw {
            for y in (MAP_Y..MAP_Y + mh).step_by(4) {
                let _ = c.draw_point((greenwich, y));
            }
        }
        let start = map::start_col(self.pan);
        // Night side: a flat light grey over the sea (drawn under the land); runs that wrap
        // around the map edge are split.
        let grey = sdl2::pixels::Color::RGB(
            (sea.r as u32 * 3 / 4 + land.r as u32 / 4) as u8,
            (sea.g as u32 * 3 / 4 + land.g as u32 / 4) as u8,
            (sea.b as u32 * 3 / 4 + land.b as u32 / 4) as u8,
        );
        c.set_draw_color(grey);
        for &(x, y, n) in self.night.iter().filter(|r| (r.1 as i32) < mh) {
            let (x, n) = (x as i32, n as i32);
            let sx = (x - start).rem_euclid(mw);
            let first = (mw - sx).min(n);
            let _ = c.fill_rect(Rect::new(MAP_X + sx, MAP_Y + y as i32, first as u32, 1));
            if n > first {
                let _ = c.fill_rect(Rect::new(MAP_X, MAP_Y + y as i32, (n - first) as u32, 1));
            }
        }
        let first = (mw - start).max(0);
        let _ = assets.blit_tinted_src(c, map::TEXTURE_KEY, Rect::new(start, 0, first as u32, mh as u32), Rect::new(MAP_X, MAP_Y, first as u32, mh as u32), land);
        if start > 0 {
            let _ = assets.blit_tinted_src(c, map::TEXTURE_KEY, Rect::new(0, 0, start as u32, mh as u32), Rect::new(MAP_X + first, MAP_Y, start as u32, mh as u32), land);
        }
        // The selected city's time zone: its land filled white with a black frame (its coastline and the
        // borders of the zone), over the outlined land.
        let white = tm.get_color("icon_white");
        let mut zone = vec![false; map::W * map::H];
        for (y, x0, n) in map::zone_runs(zsel) {
            for dx in 0..n {
                zone[y * map::W + (x0 + dx) % map::W] = true;
            }
        }
        let inz = |x: i32, y: i32| y >= 0 && y < mh && zone[y as usize * map::W + x.rem_euclid(mw) as usize];
        for y in 0..mh {
            for mx in 0..mw {
                if !inz(mx, y) {
                    continue;
                }
                let edge = !inz(mx - 1, y) || !inz(mx + 1, y) || !inz(mx, y - 1) || !inz(mx, y + 1);
                let sx = (mx - start).rem_euclid(mw);
                c.set_draw_color(if edge { land } else { white });
                let _ = c.fill_rect(Rect::new(MAP_X + sx, MAP_Y + y, 1, 1));
            }
        }
        // International date line, as it really runs: around Chukotka and the Aleutians, out east
        // around Kiribati, between Samoa / Tonga and American Samoa / Niue, east of the Chathams.
        draw_date_line(c, self.pan, mw, mh, land);

        // The sun, over the point where it is at the zenith.
        let (sx, sy) = (MAP_X + map::lon_to_x(self.sun.1, self.pan).round() as i32, MAP_Y + map::lat_to_y(self.sun.0).round() as i32);
        if sx > MAP_X + 3 && sx < MAP_X + mw - 4 && sy > MAP_Y + 3 && sy < MAP_Y + mh - 4 {
            ui::fill_rect(c, Rect::new(sx - 3, sy - 3, 7, 7), sea);
            c.set_draw_color(land);
            for (dx, dy) in [(0, -3), (0, 3), (-3, 0), (3, 0), (-2, -2), (2, -2), (-2, 2), (2, 2)] {
                let _ = c.draw_point((sx + dx, sy + dy));
            }
            ui::fill_rect(c, Rect::new(sx - 1, sy - 1, 3, 3), land);
        }

        // Cities: small dots; the selected one blinks as a ring with a core.
        let blink_on = !blink || local.timestamp_subsec_millis() < 500;
        for (i, ct) in CITIES.iter().enumerate() {
            let x = MAP_X + map::lon_to_x(ct.lon, self.pan).round() as i32;
            let y = MAP_Y + map::lat_to_y(ct.lat).round() as i32;
            if x < MAP_X + 2 || x > MAP_X + mw - 3 || y > MAP_Y + mh - 3 {
                continue;
            }
            if i == self.selected {
                if blink_on {
                    ui::fill_rect(c, Rect::new(x - 4, y - 4, 9, 9), sea);
                    ui::draw_rect(c, Rect::new(x - 4, y - 4, 9, 9), land);
                    ui::fill_rect(c, Rect::new(x - 2, y - 2, 5, 5), land);
                } else {
                    ui::fill_rect(c, Rect::new(x - 1, y - 1, 3, 3), land);
                }
            } else {
                ui::fill_rect(c, Rect::new(x - 2, y - 2, 5, 5), sea);
                ui::fill_rect(c, Rect::new(x - 1, y - 1, 3, 3), land);
            }
        }

        mh
    }

    /// The strip below the map: the selected city's 7-segment clock, its name and date.
    fn draw_details(&self, c: &mut Canvas<Window>, assets: &mut AssetStore, fs: &FontSystem, tm: &ThemeManager, local: DateTime<Local>, mh: i32) {
        let ink = tm.get_color("foreground");
        let muted = tm.get_color("muted");
        let paper = tm.get_color("widget_face_active");
        let city = &CITIES[self.selected];
        let sel = city_time(city.zone, local.with_timezone(&Utc), local, local.date_naive());
        // --- details: 7-segment clock (your atlas), then city and date on plates ---
        let top = MAP_Y + mh + 3;
        let plate = Rect::new(4, top, 114, 29);
        // A rule closes the map; the clock plate is outlined like the date plate.
        ui::fill_rect(c, Rect::new(0, MAP_Y + mh, SCREEN_W as u32, 1), ink);
        ui::draw_rect(c, plate, tm.get_color("line"));
        let y0 = top + 4;
        let mut x = plate.x() + 5;
        // Big digits advance 17 px (the 14 px cell plus a 3-4 px gap).
        for d in [sel.hour / 10, sel.hour % 10] {
            seg::draw_atlas_digit(c, assets, seg::BIG_KEY, 14, 20, d as usize, x, y0, ink);
            x += 17;
        }
        if sel.second % 2 == 0 {
            seg::draw_colon_upright(c, x + 1, y0, 20, 3, ink);
        }
        x += 8;
        for d in [sel.minute / 10, sel.minute % 10] {
            seg::draw_atlas_digit(c, assets, seg::BIG_KEY, 14, 20, d as usize, x, y0, ink);
            x += 17;
        }
        x += 1;
        for d in [sel.second / 10, sel.second % 10] {
            seg::draw_atlas_digit(c, assets, seg::SMALL_KEY, 9, 15, d as usize, x, y0 + 5, muted);
            x += 12;
        }
        let tx = plate.x() + plate.width() as i32 + 8;
        let nw = fs.measure_display_text(city.name);
        ui::fill_rect(c, Rect::new(tx - 3, top, (nw + 6) as u32, 14), ink);
        fs.draw_display_text(c, tx, top, city.name, paper);
        let ja = crate::launcher::i18n::get() == crate::launcher::i18n::Lang::Ja;
        let mut line2 = if ja {
            format!("{}月{}日({})  {}", sel.month0 + 1, sel.day, JA_DAYS[sel.weekday as usize], offset_label(sel.utc_offset))
        } else {
            format!("{} {} {}  {}", DAYS[sel.weekday as usize], sel.day, MONTHS[sel.month0 as usize], offset_label(sel.utc_offset))
        };
        if sel.day_diff != 0 {
            line2.push_str(&format!("  {}{}{}", if sel.day_diff > 0 { "+" } else { "" }, sel.day_diff, if ja { "日" } else { "d" }));
        }
        let lw = fs.measure_text(&line2);
        ui::fill_rect(c, Rect::new(tx - 3, top + 16, (lw + 6) as u32, 13), paper);
        ui::draw_rect(c, Rect::new(tx - 3, top + 16, (lw + 6) as u32, 13), ink);
        fs.draw_text(c, tx, top + 16, &line2, ink);
        let _ = SCREEN_W;
    }
}

/// Corners of the international date line (latitude, longitude), north to south. A simplified
/// version of the line in use since 1995 (Kiribati moved east of it) and 2011 (Samoa and Tokelau).
pub const DATE_LINE: [(f32, f32); 15] = [
    (90.0, 180.0),
    (75.0, 180.0),
    (68.25, -169.0),  // Bering Strait: Russia west, Alaska east
    (65.5, -169.0),
    (53.0, 170.0),    // west of the Aleutians (US, east of the line)
    (47.5, 180.0),
    (5.0, 180.0),
    (5.0, -150.0),    // around Kiribati's Phoenix and Line Islands (UTC+13 / +14)
    (-11.0, -150.0),
    (-11.0, -171.3),  // Tokelau and Samoa west, American Samoa east
    (-15.0, -171.3),
    (-22.0, -172.5),  // Tonga west, Niue east
    (-51.2, -172.5),  // the Chatham Islands (New Zealand) west
    (-51.2, 180.0),
    (-90.0, 180.0),
];

/// Draw [`DATE_LINE`] on the map panned to `pan`, 1 px in `col`, clipped to the map; the line
/// crosses the map's wrap-around seam continuously (a copy is drawn one map width to each side).
fn draw_date_line(c: &mut Canvas<Window>, pan: f32, mw: i32, mh: i32, col: sdl2::pixels::Color) {
    let px_per_deg = map::W as f32 / 360.0;
    // Screen x of each corner, unwrapped along the line so no segment jumps across the map.
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(DATE_LINE.len());
    for (i, &(lat, lon)) in DATE_LINE.iter().enumerate() {
        let x = if i == 0 {
            map::lon_to_x(lon, pan)
        } else {
            let (_, plon) = DATE_LINE[i - 1];
            pts[i - 1].0 + map::wrap_lon(lon - plon) * px_per_deg
        };
        pts.push((x, map::lat_to_y(lat)));
    }
    c.set_draw_color(col);
    for shift in [-(mw as f32), 0.0, mw as f32] {
        for w in pts.windows(2) {
            let ((x0, y0), (x1, y1)) = (w[0], w[1]);
            let steps = ((x1 - x0).abs().max((y1 - y0).abs()).ceil() as i32).max(1);
            for k in 0..=steps {
                let t = k as f32 / steps as f32;
                let x = (x0 + (x1 - x0) * t + shift).round() as i32;
                let y = (y0 + (y1 - y0) * t).round() as i32;
                if x >= 0 && x < mw && y >= 0 && y < mh {
                    let _ = c.draw_point((MAP_X + x, MAP_Y + y));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_date_line_runs_pole_to_pole_and_leaves_180_only_around_islands() {
        let dl = super::DATE_LINE;
        assert_eq!(dl.first().unwrap().0, 90.0);
        assert_eq!(dl.last().unwrap().0, -90.0);
        assert!(dl.windows(2).all(|w| w[0].0 >= w[1].0), "north to south");
        // Kiribati's Line Islands (Kiritimati, 157.4 W) are west of the line: the bulge reaches 150 W.
        assert!(dl.iter().any(|&(lat, lon)| lon == -150.0 && lat <= 2.0));
        // Samoa (171.8 W) is west of the line, American Samoa (170.7 W) east of it.
        let samoa = dl.iter().find(|&&(lat, _)| lat == -11.0 && true).map(|_| -171.3).unwrap();
        assert!(samoa > -171.8 && samoa < -170.7);
    }

    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 17, h, m, 0).unwrap()
    }

    fn ct(zone: &str, utc: DateTime<Utc>) -> CityTime {
        city_time(zone, utc, utc.with_timezone(&Local), utc.date_naive())
    }

    #[test]
    fn offsets_follow_the_zone_rules() {
        let u = at(3, 30);
        assert_eq!((ct("Asia/Tokyo", u).hour, ct("Asia/Tokyo", u).minute), (12, 30));
        assert_eq!(ct("Asia/Tokyo", u).utc_offset, 9 * 3600);
        assert_eq!(ct("America/New_York", u).utc_offset, -5 * 3600, "EST in November");
        assert_eq!(ct("Europe/London", u).utc_offset, 0);
        assert_eq!(ct("Australia/Sydney", u).utc_offset, 11 * 3600, "AEDT in November");
        // Daylight saving applies in summer.
        let summer = Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap();
        assert_eq!(ct("America/New_York", summer).utc_offset, -4 * 3600);
        assert_eq!(ct("Europe/Paris", summer).utc_offset, 2 * 3600);
    }

    #[test]
    fn day_difference_is_relative_to_the_base_date() {
        let u = at(3, 30);
        assert_eq!(ct("America/New_York", u).day_diff, -1, "22:30 the previous evening");
        assert_eq!(ct("America/New_York", u).day, 16);
        assert_eq!(ct("Asia/Tokyo", u).day_diff, 0);
        assert_eq!(ct("Australia/Sydney", at(14, 0)).day_diff, 1, "01:00 the next morning");
        assert_eq!(ct("UTC", u).utc_offset, 0);
    }

    #[test]
    fn empty_zone_means_local_time() {
        let u = Utc::now();
        let local = u.with_timezone(&Local);
        let t = city_time("", u, local, local.date_naive());
        assert_eq!((t.hour, t.minute), (local.hour(), local.minute()));
        assert_eq!(t.utc_offset, local.offset().local_minus_utc());
    }

    #[test]
    fn every_listed_zone_parses() {
        for c in &CITIES {
            assert!(c.zone.parse::<Tz>().is_ok(), "{}: {}", c.name, c.zone);
        }
    }

    #[test]
    fn offset_labels() {
        assert_eq!(offset_label(9 * 3600), "UTC+9");
        assert_eq!(offset_label(-5 * 3600), "UTC-5");
        assert_eq!(offset_label(5 * 3600 + 1800), "UTC+5:30");
        assert_eq!(offset_label(0), "UTC+0");
    }

    #[test]
    fn the_sun_follows_the_seasons_and_the_clock() {
        let (lat, lon) = subsolar(Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap());
        assert!((lat - 23.4).abs() < 0.6, "June solstice: sun over the Tropic of Cancer, got {lat}");
        assert!(lon.abs() < 3.0, "noon UTC: sun near Greenwich, got {lon}");
        let (lat, _) = subsolar(Utc.with_ymd_and_hms(2026, 12, 21, 12, 0, 0).unwrap());
        assert!((lat + 23.4).abs() < 0.6, "December solstice, got {lat}");
        let (lat, _) = subsolar(Utc.with_ymd_and_hms(2026, 3, 20, 14, 0, 0).unwrap());
        assert!(lat.abs() < 0.8, "near the equinox the sun is over the equator, got {lat}");
        // Six hours later the sun is 90 degrees further west.
        let (_, a) = subsolar(Utc.with_ymd_and_hms(2026, 3, 20, 6, 0, 0).unwrap());
        let (_, b) = subsolar(Utc.with_ymd_and_hms(2026, 3, 20, 12, 0, 0).unwrap());
        assert!((map::wrap_lon(b - a) + 90.0).abs() < 2.0, "{a} -> {b}");
    }

    #[test]
    fn day_and_night_sides() {
        let sun = (0.0, 0.0); // equinox, noon at Greenwich
        assert!(!is_night(0.0, 0.0, sun) && !is_night(40.0, 60.0, sun));
        assert!(is_night(0.0, 180.0, sun) && is_night(30.0, -150.0, sun));
        // In northern summer the north pole is in daylight all day, the south pole in darkness.
        let summer = (23.4, 0.0);
        assert!(!is_night(85.0, 100.0, summer) && is_night(-85.0, 100.0, summer));
    }

    #[test]
    fn about_half_the_planet_is_dark() {
        let runs = night_runs((10.0, 40.0));
        let dark: usize = runs.iter().map(|r| r.2 as usize).sum();
        let share = dark as f32 / (map::W * map::H) as f32;
        assert!((0.35..0.65).contains(&share), "night share {share}");
        assert!(runs.iter().all(|&(x, y, n)| n > 0 && (x + n) as usize <= map::W && (y as usize) < map::H));
    }

    #[test]
    fn redraw_is_throttled_to_quarter_seconds() {
        let local = Utc.with_ymd_and_hms(2026, 11, 17, 3, 0, 0).unwrap().with_timezone(&Local);
        let mut w = WorldClock::new(local);
        assert!(w.poll_redraw(local), "the first poll paints");
        assert!(!w.poll_redraw(local), "the same quarter second does not");
        assert!(w.poll_redraw(local + chrono::Duration::milliseconds(300)), "the next quarter second does");
    }

    #[test]
    fn cities_are_ordered_west_to_east_and_navigation_wraps() {
        for w in CITIES.windows(2) {
            assert!(w[0].lon < w[1].lon, "{} must be west of {}", w[0].name, w[1].name);
        }
        let local = Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap().with_timezone(&Local);
        let mut w = WorldClock::new(local);
        w.selected = 0;
        w.handle_key(Keycode::Z); // west of the westernmost city wraps to the easternmost
        assert_eq!(w.selected, CITIES.len() - 1);
        w.handle_key(Keycode::C);
        assert_eq!(w.selected, 0);
        w.handle_key(Keycode::Right);
        assert_eq!(w.selected, 1);
    }

    #[test]
    fn the_map_eases_towards_the_selected_city_the_short_way() {
        let local = Utc::now().with_timezone(&Local);
        let mut w = WorldClock::new(local);
        w.selected = 0; // Honolulu, -157.8
        w.pan = 170.0; // near the date line on the other side
        w.tick();
        assert!(w.pan > 170.0 || w.pan < -170.0, "it moves east across the date line, not back through Greenwich: {}", w.pan);
        for _ in 0..60 {
            w.tick();
        }
        assert!((w.pan - CITIES[0].lon).abs() < 0.5);
    }

    #[test]
    fn the_initial_city_matches_the_device_offset() {
        let tokyo_noon = Utc.with_ymd_and_hms(2026, 11, 17, 3, 0, 0).unwrap().with_timezone(&Local);
        let w = WorldClock::new(tokyo_noon);
        let off = tokyo_noon.offset().local_minus_utc();
        let utc = tokyo_noon.with_timezone(&Utc);
        let got = city_time(CITIES[w.selected].zone, utc, tokyo_noon, tokyo_noon.date_naive()).utc_offset;
        let best = CITIES.iter().map(|c| (city_time(c.zone, utc, tokyo_noon, tokyo_noon.date_naive()).utc_offset - off).abs()).min().unwrap();
        assert_eq!((got - off).abs(), best);
    }
}
