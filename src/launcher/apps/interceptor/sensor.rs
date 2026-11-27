//! Devices' Sensor log page: sample one source (a 1-Wire temperature sensor, an I2C device's
//! current byte, a GPIO line) at a chosen interval, draw it as a line graph and keep every sample
//! in a CSV under `~/.local/share/harbour/` (`HARBOUR_SENSOR_DIR` overrides) that Graph opens.
//! Device telemetry (temperature, CPU, memory, battery) stays in Monitor.
//!
//! Sources are the ones the other Devices pages found (open the 1-Wire, I2C or GPIO page once
//! and the device is listed here). The mock backend (`HARBOUR_MOCK=1`) offers a demo source.

use super::*;
use std::path::PathBuf;

/// Seconds between samples.
pub(super) const SENSOR_EVERY: [u64; 4] = [1, 5, 10, 60];
/// Samples kept for the graph (the CSV keeps all of them).
pub(super) const SENSOR_CAP: usize = 600;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum SensorSrc {
    /// A 1-Wire device id (`28-0316a2795cff`): its temperature in degrees C.
    W1(String),
    /// gpiochip name and line number: 0 / 1.
    Gpio(String, u32),
    /// i2c bus name (`i2c-1`) and address: the byte the device offers (`i2cget -y bus addr`).
    I2c(String, u8),
    /// Synthetic values (mock backend).
    Demo,
}

impl SensorSrc {
    pub fn unit(&self) -> &'static str {
        match self {
            SensorSrc::W1(_) | SensorSrc::Demo => "C",
            _ => "",
        }
    }
}

pub(super) struct SensorView {
    pub src_idx: usize,
    pub every_idx: usize,
    pub run: u32,
    pub running: bool,
    pub err: Option<String>,
    /// (seconds since the run started, value); newest last. `None` = the read failed.
    pub samples: VecDeque<(f32, Option<f32>)>,
    pub count: u32,
    pub file: Option<PathBuf>,
    pub last_mock: Instant,
}

impl SensorView {
    pub fn new() -> Self {
        Self { src_idx: 0, every_idx: 1, run: 0, running: false, err: None, samples: VecDeque::new(), count: 0, file: None, last_mock: Instant::now() }
    }

    pub fn every(&self) -> u64 {
        SENSOR_EVERY[self.every_idx % SENSOR_EVERY.len()]
    }

    pub fn values(&self) -> impl Iterator<Item = f32> + '_ {
        self.samples.iter().filter_map(|s| s.1)
    }
}

/// Where the CSV files go.
pub(super) fn sensor_dir() -> PathBuf {
    match std::env::var_os("HARBOUR_SENSOR_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".local/share/harbour"),
    }
}

/// `i2cget` prints `0x1f`.
pub(super) fn parse_i2cget(text: &str) -> Option<f32> {
    let t = text.trim().strip_prefix("0x")?;
    u32::from_str_radix(t, 16).ok().map(|v| v as f32)
}

/// `gpioget` prints `0` / `1` (libgpiod 1.x) or `"17"=active` / `"17"=inactive` (2.x).
pub(super) fn parse_gpioget(text: &str) -> Option<f32> {
    let t = text.trim();
    if t.ends_with("=inactive") {
        Some(0.0)
    } else if t.ends_with("=active") {
        Some(1.0)
    } else {
        match t {
            "0" => Some(0.0),
            "1" => Some(1.0),
            _ => None,
        }
    }
}

/// The demo source: a slow temperature swing with a little noise, the same for the same `n`.
pub(super) fn mock_sensor(n: u32) -> f32 {
    let t = n as f32;
    let v = 23.4 + 1.6 * (t * 0.17).sin() + 0.35 * (t * 1.3).cos();
    (v * 100.0).round() / 100.0
}

/// One read of a source (blocks for as long as the tool needs: call from a worker).
fn read_source(src: &SensorSrc, stop: &AtomicBool, pid: &AtomicI32) -> Option<f32> {
    let _ = (stop, pid);
    match src {
        SensorSrc::W1(id) => {
            let dir = Path::new("/sys/bus/w1/devices").join(id);
            let mc = std::fs::read_to_string(dir.join("temperature"))
                .ok()
                .and_then(|t| t.trim().parse::<i32>().ok())
                .or_else(|| std::fs::read_to_string(dir.join("w1_slave")).ok().and_then(|t| parse_w1_slave(&t)))?;
            Some(mc as f32 / 1000.0)
        }
        SensorSrc::Gpio(chip, line) => parse_gpioget(&run_capture("gpioget", &[chip, &line.to_string()])?),
        SensorSrc::I2c(bus, addr) => {
            let n = bus.trim_start_matches("i2c-");
            parse_i2cget(&run_capture("i2cget", &["-y", n, &format!("0x{addr:02x}")])?)
        }
        SensorSrc::Demo => None,
    }
}

pub(super) fn start_sensor(tx: Sender<Ev>, src: SensorSrc, every: u64, run: u32) -> Worker {
    let w = Worker::new();
    let (stop, pid) = (w.stop.clone(), w.pid.clone());
    std::thread::spawn(move || {
        let t0 = Instant::now();
        while !stop.load(Ordering::SeqCst) {
            let at = t0.elapsed().as_secs_f32();
            let v = read_source(&src, &stop, &pid);
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let _ = tx.send(Ev::Sensor(run, at, v));
            sleep_unless(&stop, every * 1000);
        }
    });
    w
}

/// A file name for the label: letters and digits, the rest becomes `-`.
fn slug(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    t.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

/// The CSV text of one sample (`time_s,value`; an empty value for a failed read).
pub(super) fn csv_line(t: f32, v: Option<f32>) -> String {
    match v {
        Some(v) => format!("{t:.1},{v}\n"),
        None => format!("{t:.1},\n"),
    }
}

impl Interceptor {
    /// What the other pages found that can be logged.
    pub(super) fn sensor_sources(&self) -> Vec<(String, SensorSrc)> {
        let mut v = Vec::new();
        if self.mock {
            v.push(("Demo temperature".to_string(), SensorSrc::Demo));
        }
        if let Ok(devs) = &self.ports.w1 {
            for d in devs.iter().filter(|d| d.temp_mc.is_some()) {
                v.push((format!("1-Wire {}", mask_w1(&d.id, self.mask)), SensorSrc::W1(d.id.clone())));
            }
        }
        if let Some(bus) = self.ports.i2c.buses.get(self.ports.i2c.bus) {
            for (addr, _) in &self.ports.i2c.found {
                v.push((format!("I2C {bus} 0x{addr:02x}"), SensorSrc::I2c(bus.clone(), *addr)));
            }
        }
        if let Some(chip) = self.ports.gpio.get(self.ports.gpio_chip) {
            for l in chip.lines.iter().filter(|l| !l.output && l.consumer.is_none()).take(16) {
                v.push((format!("GPIO {}:{}", chip.name, l.num), SensorSrc::Gpio(chip.name.clone(), l.num)));
            }
        }
        v
    }

    fn sensor_current(&self) -> Option<(String, SensorSrc)> {
        let all = self.sensor_sources();
        let i = self.sensor.src_idx.min(all.len().saturating_sub(1));
        all.into_iter().nth(i)
    }

    /// Return: start a run (a new CSV file) or stop it.
    pub(super) fn sensor_toggle(&mut self, ctx: &Ctx) {
        if self.sensor.running {
            self.sensor.running = false;
            self.sensor_worker = None;
            return;
        }
        let Some((label, src)) = self.sensor_current() else {
            self.sensor.err = Some("No sensor listed yet".into());
            return;
        };
        let dir = sensor_dir();
        let path = dir.join(format!("sensor-{}-{}.csv", slug(&label), ctx.now.format("%Y%m%d-%H%M%S")));
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, format!("time_s,{}\n", slug(&label)))) {
            self.sensor.err = Some(format!("Cannot write: {e}"));
            return;
        }
        self.sensor.run = self.sensor.run.wrapping_add(1);
        self.sensor.samples.clear();
        self.sensor.count = 0;
        self.sensor.err = None;
        self.sensor.file = Some(path);
        self.sensor.running = true;
        self.sensor.last_mock = Instant::now();
        let every = self.sensor.every();
        if src == SensorSrc::Demo {
            // A first stretch is there at once (a still scene under HARBOUR_FROZEN), then one a tick.
            let ahead = if crate::rt::frozen() { 40 } else { 3 };
            for n in 0..ahead {
                self.sensor_record(n as f32 * every as f32, Some(mock_sensor(n)));
            }
        } else {
            self.sensor_worker = Some(start_sensor(self.tx.clone(), src, every, self.sensor.run));
        }
    }

    pub(super) fn sensor_stop(&mut self) {
        self.sensor.running = false;
        self.sensor_worker = None;
    }

    pub(super) fn sensor_record(&mut self, t: f32, v: Option<f32>) {
        if self.sensor.samples.len() >= SENSOR_CAP {
            self.sensor.samples.pop_front();
        }
        self.sensor.samples.push_back((t, v));
        self.sensor.count += 1;
        if let Some(path) = &self.sensor.file {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
                let _ = f.write_all(csv_line(t, v).as_bytes());
            }
        }
    }

    pub(super) fn mock_sensor_tick(&mut self, now: Instant) -> bool {
        let demo = self.sensor_current().map_or(false, |(_, s)| s == SensorSrc::Demo);
        if self.sensor.running && demo && now.duration_since(self.sensor.last_mock) >= Duration::from_secs(1) {
            self.sensor.last_mock = now;
            let n = self.sensor.count;
            self.sensor_record(n as f32 * self.sensor.every() as f32, Some(mock_sensor(n)));
            return true;
        }
        false
    }

    pub(super) fn apply_sensor(&mut self, run: u32, t: f32, v: Option<f32>) {
        if run == self.sensor.run && self.sensor.running {
            self.sensor_record(t, v);
        }
    }

    /// G: the CSV in Graph.
    pub(super) fn sensor_open_in_graph(&self) -> AppAction {
        match &self.sensor.file {
            Some(p) if self.sensor.count > 1 => AppAction::Open { app: Some("Graph".into()), path: p.clone() },
            _ => AppAction::Toast(i18n::tr_s("Log at least two samples first")),
        }
    }

    pub(super) fn draw_sensor(&mut self, c: &mut Canvas, ctx: &Ctx, p: &Pal) {
        let g = eth_graph_rect();
        let slots = eth_slots(g);
        let cur = self.sensor_current();
        let sv = &self.sensor;
        let unit = cur.as_ref().map_or("", |(_, s)| s.unit());
        let fmt = |v: f32| if unit.is_empty() { format!("{v:.0}") } else { format!("{v:.1} {unit}") };
        let vals: Vec<f32> = sv.values().collect();
        let shown: Vec<Option<f32>> = sv.samples.iter().skip(sv.samples.len().saturating_sub(slots)).map(|s| s.1).collect();
        let (lo, hi) = shown.iter().flatten().fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        let state = if let Some(e) = &sv.err {
            e.clone()
        } else if sv.running {
            "logging".to_string()
        } else if sv.count > 0 {
            "stopped".to_string()
        } else {
            "Return: start".to_string()
        };
        let name = cur.as_ref().map_or_else(|| "-".to_string(), |(l, _)| l.clone());
        let file = sv.file.as_ref().and_then(|f| f.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "-".into());
        let rows: Vec<(&str, String)> = vec![
            ("From", name),
            ("Every", format!("{} s", sv.every())),
            ("Samples", format!("{}", sv.count)),
            ("Last", sv.samples.back().and_then(|s| s.1).map_or_else(|| "-".into(), fmt)),
            ("Min", vals.iter().copied().reduce(f32::min).map_or_else(|| "-".into(), fmt)),
            ("Avg", if vals.is_empty() { "-".into() } else { fmt(vals.iter().sum::<f32>() / vals.len() as f32) }),
            ("Max", vals.iter().copied().reduce(f32::max).map_or_else(|| "-".into(), fmt)),
            ("File", file),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            let mut row = row_rect(i as i32);
            row.set_width(ETH_COL_W as u32);
            zebra(c, p, row, i);
            ctx.fs.draw_text(c, row.x() + 4, row.y() + 1, &i18n::tr_s(k), p.faint);
            let w = ETH_COL_W - 58;
            let v = chrome::fit_text(ctx.fs, v, w);
            right_text(c, ctx, row.right() - 4, row.y(), &v, p.ink);
        }
        vsep(c, p, LIST_X + 54, 0);
        ctx.fs.draw_text(c, g.x(), BODY_Y + 1, &i18n::tr_s("Sensor"), p.faint);
        right_text(c, ctx, g.right(), BODY_Y, &state, p.faint);
        ui::fill_rect(c, g, p.paper);
        if lo <= hi {
            // The scale hugs the data (at least 1 unit tall) with a little room.
            let (lo, hi) = if hi - lo < 1.0 { (lo - 0.5, hi + 0.5) } else { (lo, hi) };
            let pad = (hi - lo) * 0.1;
            let (lo, hi) = (lo - pad, hi + pad);
            let h = g.height() as i32 - 2;
            let off = slots - shown.len();
            let y_of = |v: f32| g.bottom() - 2 - (((v - lo) / (hi - lo)) * (h - 2) as f32) as i32;
            let mut prev: Option<(i32, i32)> = None;
            for (i, s) in shown.iter().enumerate() {
                let x = g.x() + ((off + i) * 2) as i32 + 1;
                match s {
                    Some(v) => {
                        let pt = (x, y_of(*v));
                        if let Some(q) = prev {
                            ui::line(c, q.0, q.1, pt.0, pt.1, p.ink);
                        }
                        ui::fill_rect(c, Rect::new(pt.0, pt.1, 1, 1), p.ink);
                        prev = Some(pt);
                    }
                    None => prev = None,
                }
            }
            for (v, y) in [(hi, g.y() + 2), (lo, g.bottom() - 14)] {
                let t = fmt(v);
                let tw = ctx.fs.measure_text(&t);
                ui::fill_rect(c, Rect::new(g.x() + 2, y, (tw + 2) as u32, 12), p.paper);
                ctx.fs.draw_text(c, g.x() + 3, y, &t, p.faint);
            }
        } else if cur.is_none() {
            text_row(c, ctx, g.x() + 4, g.y() + 2, g.width() as i32 - 8, "No sensor listed.", p.faint);
            text_row(c, ctx, g.x() + 4, g.y() + 18, g.width() as i32 - 8, "Open 1-Wire, I2C or GPIO once.", p.faint);
        }
        ui::draw_rect(c, g, p.ink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_print_their_values() {
        assert_eq!(parse_i2cget("0x1f\n"), Some(31.0));
        assert_eq!(parse_i2cget("Error: Read failed"), None);
        assert_eq!(parse_gpioget("1\n"), Some(1.0));
        assert_eq!(parse_gpioget("\"17\"=inactive\n"), Some(0.0));
        assert_eq!(parse_gpioget("\"17\"=active"), Some(1.0));
        assert_eq!(parse_gpioget("nope"), None);
    }

    #[test]
    fn csv_lines_and_file_names() {
        assert_eq!(csv_line(5.0, Some(23.4)), "5.0,23.4\n");
        assert_eq!(csv_line(6.0, None), "6.0,\n");
        assert_eq!(slug("1-Wire 28-****5cff"), "1-wire-28-5cff");
        assert_eq!(slug("I2C i2c-1 0x76"), "i2c-i2c-1-0x76");
    }

    #[test]
    fn a_demo_run_writes_a_csv_that_graph_can_open() {
        let dir = std::env::temp_dir().join(format!("harbour-sensor-test-{}", std::process::id()));
        std::env::set_var("HARBOUR_SENSOR_DIR", &dir);
        let fs = crate::font::FontSystem::new();
        let tm = crate::theme::ThemeManager::new(crate::theme::Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = crate::system::mock::MockBackend::new();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut a = Interceptor::with_side(true, Side::Hardware);
        a.set_page(PAGE_SENSOR);
        assert!(a.sensor_sources().iter().any(|(_, s)| *s == SensorSrc::Demo));
        a.sensor_toggle(&ctx);
        assert!(a.sensor.running);
        let mut t = Instant::now();
        for _ in 0..5 {
            t += Duration::from_secs(2);
            a.mock_sensor_tick(t);
        }
        assert!(a.sensor.count >= 8);
        let path = a.sensor.file.clone().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("time_s,demo-temperature\n"));
        assert_eq!(text.lines().count() as u32, a.sensor.count + 1);
        // Graph takes it.
        let g = crate::launcher::apps::graph::csv_to_graph(&text, "Demo").unwrap();
        assert!(g.contains("mode: function"));
        assert!(matches!(a.sensor_open_in_graph(), AppAction::Open { .. }));
        a.sensor_toggle(&ctx);
        assert!(!a.sensor.running);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
