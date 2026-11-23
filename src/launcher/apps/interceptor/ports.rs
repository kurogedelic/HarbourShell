//! Interceptor's pages for Wi-Fi and the wired buses: the Grove port, USB host, serial ports,
//! I2C, GPIO and 1-Wire.
//!
//! As in the rest of Interceptor, the parsers are pure and unit-tested; the slow sources run in a
//! background thread only while their page is in front:
//! * Wi-Fi: `nmcli -t ... dev wifi list` every few seconds (NetworkManager).
//! * Serial: `stty` sets the port up (raw, the chosen speed, 8N1), then the port is read in a
//!   thread and written from the UI. A port is open only while you ask for it and its page is
//!   in front. Needs group `dialout`.
//! * I2C: `i2cdetect -y N` (i2c-tools) once per visit or on request, never in the background
//!   (probing can wake devices). Needs group `i2c`.
//! * GPIO: `gpiodetect` / `gpioinfo` (gpiod) every few seconds: names, direction, who uses a
//!   line. Values are not read (that would claim the line).
//! * 1-Wire: `/sys/bus/w1/devices` (temperature sensors read their `temperature` file).
//! * Grove: the 5 V rail through the backend (`grove_5v_out`); on the Cardputer Zero the port is
//!   the mini UART on GPIO14/15 (`/dev/ttyS0`), switched from I2C by GPIO4, which the board
//!   driver offers as the LED `grove_fun` (0 = UART, 1 = I2C; group `gpio` may write it).

use std::fs::File;
use std::io::Write;

use super::*;
use crate::system::ExtPort;

// ======================================================================= events

pub enum PortEv {
    Wifi(Result<Vec<WifiNet>, String>),
    /// The port is open; this is the handle to write to (the thread reads its own clone).
    SerOpened(File),
    SerRx(Vec<u8>),
    SerErr(String),
    I2c(String, Result<Vec<(u8, bool)>, String>),
    Gpio(Result<Vec<GpioChip>, String>),
    W1(Result<Vec<W1Dev>, Vec<String>>),
}

fn send(tx: &Sender<Ev>, e: PortEv) {
    let _ = tx.send(Ev::Port(e));
}

// ======================================================================== Wi-Fi

#[derive(Clone, Debug, PartialEq)]
pub struct WifiNet {
    pub in_use: bool,
    pub ssid: String,
    pub bssid: String,
    pub chan: u32,
    pub signal: u8,
    pub security: String,
}

/// One line of `nmcli -t` output split at unescaped colons (`\:` and `\\` are escapes).
pub fn nmcli_fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut it = line.chars();
    while let Some(ch) = it.next() {
        match ch {
            '\\' => {
                if let Some(n) = it.next() {
                    out.last_mut().unwrap().push(n);
                }
            }
            ':' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

/// `nmcli -t -f IN-USE,SSID,BSSID,CHAN,SIGNAL,SECURITY dev wifi list`: the network in use first,
/// then by signal.
pub fn parse_nmcli_wifi(text: &str) -> Vec<WifiNet> {
    let mut v: Vec<WifiNet> = text
        .lines()
        .filter_map(|l| {
            let f = nmcli_fields(l);
            if f.len() < 6 {
                return None;
            }
            let sec = f[5].trim();
            Some(WifiNet {
                in_use: f[0].trim() == "*",
                ssid: f[1].clone(),
                bssid: f[2].clone(),
                chan: f[3].trim().parse().ok()?,
                signal: f[4].trim().parse().unwrap_or(0),
                security: if sec.is_empty() || sec == "--" { "open".into() } else { sec.to_string() },
            })
        })
        .collect();
    v.sort_by(|a, b| b.in_use.cmp(&a.in_use).then(b.signal.cmp(&a.signal)));
    v
}

pub fn wifi_band(chan: u32) -> &'static str {
    if chan <= 14 {
        "2.4G"
    } else {
        "5G"
    }
}

fn start_wifi(tx: Sender<Ev>) -> Worker {
    let w = Worker::new();
    let stop = w.stop.clone();
    std::thread::spawn(move || {
        let mut first = true;
        while !stop.load(Ordering::SeqCst) {
            let rescan = if first { "yes" } else { "auto" };
            first = false;
            let args = ["-t", "-f", "IN-USE,SSID,BSSID,CHAN,SIGNAL,SECURITY", "dev", "wifi", "list", "--rescan", rescan];
            match Command::new("nmcli").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output() {
                Ok(o) if o.status.success() => send(&tx, PortEv::Wifi(Ok(parse_nmcli_wifi(&String::from_utf8_lossy(&o.stdout))))),
                Ok(_) => send(&tx, PortEv::Wifi(Err("Wi-Fi is off or busy".into()))),
                Err(_) => send(&tx, PortEv::Wifi(Err("nmcli not available".into()))),
            }
            sleep_unless(&stop, 6000);
        }
    });
    w
}

// ======================================================================= serial

pub const BAUDS: [u32; 8] = [9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600];
pub const EOLS: [(&str, &[u8]); 4] = [("LF", b"\n"), ("CR", b"\r"), ("CRLF", b"\r\n"), ("none", b"")];
const GROVE_TTY: &str = "/dev/ttyS0";
const GROVE_FUN: &str = "/sys/class/leds/grove_fun/brightness";

/// The Grove port's function: Some(true) = I2C, Some(false) = UART, None = unknown.
fn read_grove_fun() -> Option<bool> {
    std::fs::read_to_string(GROVE_FUN).ok().and_then(|t| t.trim().parse::<u32>().ok()).map(|v| v != 0)
}

#[derive(Clone, Debug, PartialEq)]
pub struct SerPort {
    pub path: String,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Rx,
    Tx,
    Info,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SerLine {
    pub dir: Dir,
    pub bytes: Vec<u8>,
}

impl SerLine {
    /// As shown: text with control characters as dots, or hex; what was sent starts with `> `.
    pub fn render(&self, hex: bool) -> String {
        let body = if hex && self.dir != Dir::Info {
            hex_str(&self.bytes, 15)
        } else {
            String::from_utf8_lossy(&self.bytes).chars().map(|c| if c.is_control() { '.' } else { c }).collect()
        };
        if self.dir == Dir::Tx {
            format!("> {body}")
        } else {
            body
        }
    }
}

/// Names under /dev that are serial ports worth listing.
pub fn is_serial_name(n: &str) -> bool {
    if n == "ttyS0" {
        return true;
    }
    ["ttyACM", "ttyUSB", "ttyAMA", "ttyGS"].iter().any(|p| n.strip_prefix(p).map_or(false, |r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit())))
}

/// A readable name: the Grove port, a USB device's own name from `/dev/serial/by-id`
/// (`by_id` = (link name, target basename)), or the kind of port.
pub fn port_label(path: &str, by_id: &[(String, String)]) -> String {
    let base = path.trim_start_matches("/dev/");
    if path == GROVE_TTY {
        return "Grove port (GPIO14 / 15)".into();
    }
    if let Some((link, _)) = by_id.iter().find(|(_, t)| t == base) {
        let mut s = link.strip_prefix("usb-").unwrap_or(link).to_string();
        for suf in ["-port0", "-if00", "-if01", "-if02"] {
            if let Some(t) = s.strip_suffix(suf) {
                s = t.to_string();
            }
        }
        // `..._debug_unit_F4:12:FA:...`, `..._UART_A10K5XYZ`: drop the serial number.
        if let Some((name, tail)) = s.rsplit_once('_') {
            if tail.len() >= 6 && tail.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == ':') && tail.chars().any(|c| c.is_ascii_digit()) {
                s = name.to_string();
            }
        }
        return s.replace('_', " ");
    }
    if base.starts_with("ttyGS") {
        "USB gadget serial".into()
    } else if base.starts_with("ttyAMA") {
        "UART".into()
    } else {
        base.to_string()
    }
}

fn port_order(path: &str) -> (u8, Vec<u32>) {
    let base = path.trim_start_matches("/dev/");
    let kind = ["ttyACM", "ttyUSB", "ttyS", "ttyAMA", "ttyGS"].iter().position(|p| base.starts_with(p)).unwrap_or(9) as u8;
    (kind, natural_key(base))
}

fn list_serial() -> Vec<SerPort> {
    let by_id: Vec<(String, String)> = std::fs::read_dir("/dev/serial/by-id")
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let t = std::fs::read_link(e.path()).ok()?;
                    Some((e.file_name().to_string_lossy().to_string(), t.file_name()?.to_string_lossy().to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut v: Vec<String> = std::fs::read_dir("/dev")
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| is_serial_name(n)).map(|n| format!("/dev/{n}")).collect())
        .unwrap_or_default();
    v.sort_by_key(|p| port_order(p));
    v.into_iter().map(|p| SerPort { label: port_label(&p, &by_id), path: p }).collect()
}

pub struct SerialView {
    pub ports: Vec<SerPort>,
    pub port: usize,
    pub baud: usize,
    pub eol: usize,
    pub hex: bool,
    /// Asked to be open (the worker opens it).
    pub want_open: bool,
    /// Open: `file` is the write handle (None in mock mode).
    opened: bool,
    file: Option<File>,
    pub err: Option<String>,
    pub log: VecDeque<SerLine>,
    partial: Vec<u8>,
    last_rx: Option<Instant>,
    /// The line being typed.
    pub input: String,
    pub rx_n: u64,
    pub tx_n: u64,
    /// A port asked for before the list was read (the Grove page's "open in Serial").
    wanted: Option<String>,
}

impl SerialView {
    fn new() -> Self {
        Self {
            ports: Vec::new(),
            port: 0,
            baud: 4,
            eol: 0,
            hex: false,
            want_open: false,
            opened: false,
            file: None,
            err: None,
            log: VecDeque::new(),
            partial: Vec::new(),
            last_rx: None,
            input: String::new(),
            rx_n: 0,
            tx_n: 0,
            wanted: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.opened
    }

    pub fn current(&self) -> Option<&SerPort> {
        self.ports.get(self.port)
    }

    pub fn select(&mut self, path: &str) {
        self.wanted = Some(path.to_string());
        self.take_wanted();
    }

    fn take_wanted(&mut self) {
        if let Some(w) = &self.wanted {
            if let Some(i) = self.ports.iter().position(|p| &p.path == w) {
                self.port = i;
                self.wanted = None;
            }
        }
    }

    fn set_ports(&mut self, ports: Vec<SerPort>) {
        let cur = self.current().map(|p| p.path.clone());
        self.ports = ports;
        self.port = cur.and_then(|c| self.ports.iter().position(|p| p.path == c)).unwrap_or(0);
        self.take_wanted();
    }

    pub fn push(&mut self, dir: Dir, bytes: Vec<u8>) {
        self.log.push_back(SerLine { dir, bytes });
        while self.log.len() > LOG_CAP {
            self.log.pop_front();
        }
    }

    fn info(&mut self, s: &str) {
        self.push(Dir::Info, s.as_bytes().to_vec());
    }

    /// Received bytes: one log line per line of text (64 bytes at most per line).
    pub fn feed(&mut self, bytes: &[u8], now: Instant) {
        self.rx_n += bytes.len() as u64;
        for &b in bytes {
            if b == b'\n' {
                self.flush();
            } else {
                self.partial.push(b);
                if self.partial.len() >= 64 {
                    self.flush();
                }
            }
        }
        self.last_rx = Some(now);
    }

    /// A line without its newline yet (a prompt) shows once the port goes quiet.
    pub fn flush(&mut self) {
        let mut l = std::mem::take(&mut self.partial);
        while l.last() == Some(&b'\r') {
            l.pop();
        }
        if !l.is_empty() {
            self.push(Dir::Rx, l);
        }
    }

    fn close(&mut self, why: Option<&str>) {
        let was = self.opened;
        self.opened = false;
        self.want_open = false;
        self.file = None;
        self.flush();
        if let Some(w) = why {
            self.info(w);
        } else if was {
            self.info("Closed");
        }
    }
}

#[cfg(target_os = "linux")]
fn start_serial(tx: Sender<Ev>, path: String, baud: u32) -> Worker {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let w = Worker::new();
    let stop = w.stop.clone();
    std::thread::spawn(move || {
        let st = Command::new("stty")
            .args(["-F", &path, &baud.to_string(), "raw", "-echo", "-echoe", "-echok", "-echoctl", "-echoke", "-ixon", "-ixoff", "-crtscts", "cs8", "-cstopb", "-parenb", "clocal", "cread", "-hupcl"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        let denied = |s: &str| s.contains("Permission denied") || s.contains("ermission");
        match st {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                let e = String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("stty failed").trim().to_string();
                let e = if denied(&e) { "Permission denied (needs group dialout)".to_string() } else { e };
                send(&tx, PortEv::SerErr(e));
                return;
            }
            Err(_) => {
                send(&tx, PortEv::SerErr("stty not available".into()));
                return;
            }
        }
        let mut f = match std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK).open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                send(&tx, PortEv::SerErr("Permission denied (needs group dialout)".into()));
                return;
            }
            Err(e) => {
                send(&tx, PortEv::SerErr(e.to_string()));
                return;
            }
        };
        match f.try_clone() {
            Ok(wf) => send(&tx, PortEv::SerOpened(wf)),
            Err(e) => {
                send(&tx, PortEv::SerErr(e.to_string()));
                return;
            }
        }
        let mut buf = [0u8; 512];
        while !stop.load(Ordering::SeqCst) {
            let mut pfd = libc::pollfd { fd: f.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let r = unsafe { libc::poll(&mut pfd, 1, 200) };
            if r <= 0 {
                continue;
            }
            if pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                send(&tx, PortEv::SerErr("The port went away (unplugged?)".into()));
                return;
            }
            match f.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => send(&tx, PortEv::SerRx(buf[..n].to_vec())),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    send(&tx, PortEv::SerErr(e.to_string()));
                    return;
                }
            }
        }
    });
    w
}

#[cfg(not(target_os = "linux"))]
fn start_serial(_tx: Sender<Ev>, _path: String, _baud: u32) -> Worker {
    Worker::new()
}

// ========================================================================== I2C

/// `i2cdetect -y N`: (address, held by a kernel driver "UU").
pub fn parse_i2cdetect(text: &str) -> Vec<(u8, bool)> {
    let mut v = Vec::new();
    for l in text.lines() {
        let Some((head, rest)) = l.split_once(':') else { continue };
        let Ok(base) = u8::from_str_radix(head.trim(), 16) else { continue };
        let cells: Vec<char> = rest.chars().collect();
        for (i, cell) in cells.chunks(3).enumerate().take(16) {
            let c: String = cell.iter().collect::<String>().trim().to_string();
            let addr = base.wrapping_add(i as u8);
            if c == "UU" {
                v.push((addr, true));
            } else if c.len() == 2 && u8::from_str_radix(&c, 16).is_ok() {
                v.push((addr, false));
            }
        }
    }
    v
}

/// What usually answers at an I2C address (a guess: many parts share addresses).
pub fn i2c_hint(addr: u8) -> &'static str {
    match addr {
        0x10 | 0x11 => "ES8388 / ES8389 codec",
        0x1E => "HMC5883L compass",
        0x20..=0x22 | 0x24..=0x26 => "MCP23017 / PCF8574 GPIO",
        0x27 => "PCF8574 LCD backpack",
        0x23 => "BH1750 light",
        0x29 => "VL53L0X / TCS34725",
        0x38 => "AHT20 / AHT10",
        0x39 => "APDS-9960",
        0x3C | 0x3D => "SSD1306 / SH1106 OLED",
        0x40 => "INA219 / PCA9685 / HTU21",
        0x44 | 0x45 => "SHT3x / SHT4x",
        0x48..=0x4B => "ADS1115 / TMP102",
        0x50 | 0x52..=0x54 | 0x56 => "EEPROM",
        0x51 => "PCF8563 RTC / EEPROM",
        0x55 => "BQ27220 fuel gauge",
        0x57 => "EEPROM / MAX30102",
        0x5A => "MLX90614 / MPR121",
        0x5C => "AM2320",
        0x60 => "MCP4725 / Si1145",
        0x62 => "SCD4x CO2",
        0x68 => "DS3231 / MPU6050 / BMI270",
        0x69 => "MPU6050 / BMI270 (alt)",
        0x6A | 0x6B => "LSM6DS3 / QMI8658",
        0x70 => "TCA9548A mux",
        0x76 | 0x77 => "BME280 / BMP280",
        _ => "",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum I2cState {
    Idle,
    Scanning,
    Done,
    Err(String),
}

pub struct I2cView {
    pub buses: Vec<String>,
    pub bus: usize,
    pub found: Vec<(u8, bool)>,
    pub state: I2cState,
}

fn list_i2c() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir("/dev")
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.strip_prefix("i2c-").map_or(false, |r| r.parse::<u32>().is_ok())).collect())
        .unwrap_or_default();
    v.sort_by_key(|n| natural_key(n));
    v
}

fn start_i2c(tx: Sender<Ev>, bus: String) -> Worker {
    let w = Worker::new();
    std::thread::spawn(move || {
        let n = bus.trim_start_matches("i2c-").to_string();
        let r = match Command::new("i2cdetect").args(["-y", &n]).stdin(Stdio::null()).output() {
            Ok(o) if o.status.success() => Ok(parse_i2cdetect(&String::from_utf8_lossy(&o.stdout))),
            Ok(o) => {
                let e = String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("scan failed").trim().to_string();
                Err(if e.contains("ermission") { "Permission denied (needs group i2c)".to_string() } else { e })
            }
            Err(_) => Err("i2cdetect not found (install i2c-tools)".into()),
        };
        send(&tx, PortEv::I2c(bus, r));
    });
    w
}

// ========================================================================= GPIO

#[derive(Clone, Debug, PartialEq, Default)]
pub struct GpioLine {
    pub num: u32,
    pub name: String,
    pub consumer: Option<String>,
    pub output: bool,
    pub active_low: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct GpioChip {
    pub name: String,
    pub label: String,
    pub lines: Vec<GpioLine>,
}

/// Whitespace-separated words, with `"quoted text"` (also `key="..."`) kept as one word.
fn words(s: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                cur.push(ch);
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    v.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        v.push(cur);
    }
    v
}

fn unquote(s: &str) -> String {
    s.trim_matches('"').to_string()
}

/// `gpioinfo`, libgpiod 1.x and 2.x:
/// `line   4:  "GPIO4"  "grove_fun"  output  active-high [used]` (1.x),
/// `line   4:  "GPIO4"  output consumer="grove_fun"` (2.x).
pub fn parse_gpioinfo(text: &str) -> Vec<GpioChip> {
    let mut chips: Vec<GpioChip> = Vec::new();
    for raw in text.lines() {
        let l = raw.trim();
        if let Some(rest) = l.strip_prefix("line") {
            let Some((num, rest)) = rest.split_once(':') else { continue };
            let Ok(num) = num.trim().parse::<u32>() else { continue };
            let w = words(rest);
            let mut g = GpioLine { num, ..Default::default() };
            for (i, t) in w.iter().enumerate() {
                match t.as_str() {
                    _ if i == 0 => g.name = if t == "unnamed" { String::new() } else { unquote(t) },
                    "input" => g.output = false,
                    "output" => g.output = true,
                    "active-low" => g.active_low = true,
                    _ if t.starts_with("consumer=") => g.consumer = Some(unquote(&t["consumer=".len()..])),
                    _ if i == 1 && t.starts_with('"') => g.consumer = Some(unquote(t)),
                    _ => {}
                }
            }
            g.consumer = g.consumer.filter(|c| !c.is_empty() && c != "unused");
            if let Some(c) = chips.last_mut() {
                c.lines.push(g);
            }
        } else if l.starts_with("gpiochip") {
            let name = l.split_whitespace().next().unwrap_or("").trim_end_matches(':').to_string();
            chips.push(GpioChip { name, ..Default::default() });
        }
    }
    chips
}

/// `gpiodetect`: `gpiochip0 [pinctrl-bcm2835] (54 lines)` -> (name, label).
pub fn parse_gpiodetect(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (name, rest) = l.trim().split_once(' ')?;
            let label = rest.split_once('[')?.1.split_once(']')?.0;
            Some((name.to_string(), label.to_string()))
        })
        .collect()
}

fn start_gpio(tx: Sender<Ev>) -> Worker {
    let w = Worker::new();
    let stop = w.stop.clone();
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match run_capture("gpioinfo", &[]) {
                Some(info) => {
                    let mut chips = parse_gpioinfo(&info);
                    let labels = run_capture("gpiodetect", &[]).map(|t| parse_gpiodetect(&t)).unwrap_or_default();
                    for c in chips.iter_mut() {
                        if let Some((_, l)) = labels.iter().find(|(n, _)| n == &c.name) {
                            c.label = l.clone();
                        }
                    }
                    send(&tx, PortEv::Gpio(Ok(chips)));
                }
                None => {
                    send(&tx, PortEv::Gpio(Err("gpioinfo not found (install gpiod)".into())));
                    return;
                }
            }
            sleep_unless(&stop, 3000);
        }
    });
    w
}

// ====================================================================== 1-Wire

#[derive(Clone, Debug, PartialEq)]
pub struct W1Dev {
    pub id: String,
    /// Millidegrees Celsius, for the temperature sensors.
    pub temp_mc: Option<i32>,
}

pub fn w1_family(id: &str) -> &'static str {
    match id.split('-').next().unwrap_or("") {
        "28" => "DS18B20",
        "10" => "DS18S20",
        "22" => "DS1822",
        "3b" => "MAX31850",
        "26" => "DS2438",
        "01" => "DS2401 ID",
        "2d" => "DS2431 EEPROM",
        "29" => "DS2408 GPIO",
        "3a" => "DS2413 GPIO",
        _ => "",
    }
}

/// `w1_slave` of a thermometer: `... : crc=a4 YES` then `... t=23125`.
pub fn parse_w1_slave(text: &str) -> Option<i32> {
    let mut it = text.lines();
    if !it.next()?.trim_end().ends_with("YES") {
        return None;
    }
    it.next()?.split("t=").nth(1)?.trim().parse().ok()
}

/// `28-0316a2795cff` -> `28-********5cff` when `on`.
pub fn mask_w1(id: &str, on: bool) -> String {
    match id.split_once('-') {
        Some((fam, rest)) if on && rest.len() > 4 => format!("{fam}-{}{}", "*".repeat(rest.len() - 4), &rest[rest.len() - 4..]),
        _ => id.to_string(),
    }
}

fn w1_missing() -> Vec<String> {
    vec![
        "No 1-Wire bus found.".into(),
        "Missing: dtoverlay=w1-gpio,gpiopin=N".into(),
        "in /boot/firmware/config.txt, then reboot.".into(),
        "DS18B20 sensors then appear here.".into(),
    ]
}

fn read_w1(root: &Path) -> Result<Vec<W1Dev>, Vec<String>> {
    let Ok(rd) = std::fs::read_dir(root) else { return Err(w1_missing()) };
    let names: Vec<String> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).collect();
    if !names.iter().any(|n| n.starts_with("w1_bus_master")) {
        return Err(w1_missing());
    }
    let mut v: Vec<W1Dev> = names
        .into_iter()
        .filter(|n| !n.starts_with("w1_bus_master"))
        .map(|n| {
            let dir = root.join(&n);
            let temp_mc = std::fs::read_to_string(dir.join("temperature"))
                .ok()
                .and_then(|t| t.trim().parse().ok())
                .or_else(|| std::fs::read_to_string(dir.join("w1_slave")).ok().and_then(|t| parse_w1_slave(&t)));
            W1Dev { id: n, temp_mc }
        })
        .collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(v)
}

fn start_w1(tx: Sender<Ev>) -> Worker {
    let w = Worker::new();
    let stop = w.stop.clone();
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            send(&tx, PortEv::W1(read_w1(Path::new("/sys/bus/w1/devices"))));
            sleep_unless(&stop, 3000);
        }
    });
    w
}

fn fmt_temp(mc: i32) -> String {
    format!("{:.1}℃", mc as f32 / 1000.0)
}

// ======================================================================== state

pub struct Ports {
    mock: bool,
    /// Set when the page's worker must start again (a new port, bus or scan).
    pub restart: bool,
    pub wifi: Vec<WifiNet>,
    pub wifi_err: Option<String>,
    pub wifi_known: bool,
    pub serial: SerialView,
    pub i2c: I2cView,
    pub gpio: Vec<GpioChip>,
    pub gpio_chip: usize,
    pub gpio_err: Option<String>,
    pub w1: Result<Vec<W1Dev>, Vec<String>>,
    pub grove_5v: Option<bool>,
    pub grove_i2c: Option<bool>,
    last_grove: Option<Instant>,
    last_ports: Instant,
    last_mock: Instant,
    mock_n: u32,
}

impl Ports {
    pub fn new(mock: bool) -> Self {
        let now = Instant::now();
        let mut p = Self {
            mock,
            restart: false,
            wifi: Vec::new(),
            wifi_err: None,
            wifi_known: false,
            serial: SerialView::new(),
            i2c: I2cView { buses: Vec::new(), bus: 0, found: Vec::new(), state: I2cState::Idle },
            gpio: Vec::new(),
            gpio_chip: 0,
            gpio_err: None,
            w1: Ok(Vec::new()),
            grove_5v: None,
            grove_i2c: None,
            last_grove: None,
            last_ports: now,
            last_mock: now,
            mock_n: 0,
        };
        if mock {
            p.seed_mock();
        }
        p
    }

    fn seed_mock(&mut self) {
        let net = |u: bool, s: &str, ch: u32, sig: u8, sec: &str| WifiNet { in_use: u, ssid: s.into(), bssid: String::new(), chan: ch, signal: sig, security: sec.into() };
        self.wifi = vec![
            net(true, "harbour-5G", 36, 82, "WPA2"),
            net(false, "aterm-1c2e-g", 1, 71, "WPA2 WPA3"),
            net(false, "harbour", 6, 66, "WPA2"),
            net(false, "DIRECT-7B-Printer", 6, 52, "WPA2"),
            net(false, "eduroam", 44, 40, "WPA2 802.1X"),
            net(false, "", 11, 33, "WPA2"),
            net(false, "cafe-free", 11, 28, "open"),
        ];
        self.wifi_known = true;
        self.serial.ports = vec![
            SerPort { path: "/dev/ttyACM0".into(), label: "Espressif USB JTAG serial debug unit".into() },
            SerPort { path: GROVE_TTY.into(), label: "Grove port (GPIO14 / 15)".into() },
            SerPort { path: "/dev/ttyGS0".into(), label: "USB gadget serial".into() },
        ];
        self.serial.opened = true;
        self.serial.want_open = true;
        self.serial.info("Opened ttyACM0 115200");
        for l in ["ESP-ROM:esp32s3-20210327", "I (312) main: sensor ready", "t=23.4 h=41 p=1012.6", "t=23.4 h=41 p=1012.7"] {
            self.serial.push(Dir::Rx, l.as_bytes().to_vec());
        }
        self.serial.push(Dir::Tx, b"status".to_vec());
        self.serial.push(Dir::Rx, b"ok uptime=128s heap=211k".to_vec());
        self.serial.rx_n = 1843;
        self.serial.tx_n = 7;
        self.serial.input = "led on".into();
        self.i2c.buses = vec!["i2c-1".into(), "i2c-11".into()];
        self.i2c.found = vec![(0x10, true), (0x3C, false), (0x55, true), (0x68, false), (0x76, false)];
        self.i2c.state = I2cState::Done;
        if std::env::var("HARBOUR_MOCK_MISSING").is_ok() {
            // Screenshots of the install card: pretend i2c-tools is not installed.
            self.i2c.found.clear();
            self.i2c.state = I2cState::Err("i2cdetect not found (install i2c-tools)".into());
        }
        let mut lines: Vec<GpioLine> = (0..28)
            .map(|n| GpioLine { num: n, name: match n { 0 => "ID_SDA".into(), 1 => "ID_SCL".into(), _ => format!("GPIO{n}") }, ..Default::default() })
            .collect();
        lines[4].consumer = Some("grove_fun".into());
        lines[4].output = true;
        lines[17].consumer = Some("gpio-ir".into());
        lines[23].consumer = Some("kbd-irq".into());
        lines[24].consumer = Some("lcd-reset".into());
        lines[24].output = true;
        lines[24].active_low = true;
        self.gpio = vec![
            GpioChip { name: "gpiochip0".into(), label: "pinctrl-bcm2835".into(), lines },
            GpioChip {
                name: "gpiochip1".into(),
                label: "raspberrypi-exp-gpio".into(),
                lines: (0..8).map(|n| GpioLine { num: n, name: ["BT_ON", "WL_ON", "PWR_LED_OFF", "GLOBAL_RESET", "VDD_SD_IO_SEL", "CAM_GPIO", "SD_PWR_ON", ""][n as usize].into(), output: n < 7, ..Default::default() }).collect(),
            },
        ];
        self.w1 = Ok(vec![W1Dev { id: "28-0316a2795cff".into(), temp_mc: Some(23_437) }, W1Dev { id: "28-04179b2c11aa".into(), temp_mc: Some(19_812) }, W1Dev { id: "01-000019a3f2c4".into(), temp_mc: None }]);
        self.grove_5v = Some(true);
        self.grove_i2c = Some(false);
    }

    /// The page's background worker (None when the page needs none or this is a mock run).
    pub fn start(&mut self, page: usize, tx: Sender<Ev>) -> Option<Worker> {
        if self.mock {
            if page == PAGE_SER && self.serial.want_open && !self.serial.opened {
                self.serial.opened = true;
                let msg = format!("Opened {} {}", self.serial.current().map_or("?", |p| p.path.trim_start_matches("/dev/")), BAUDS[self.serial.baud]);
                self.serial.info(&msg);
            }
            return None;
        }
        match page {
            PAGE_WIFI => Some(start_wifi(tx)),
            PAGE_SER => {
                self.serial.set_ports(list_serial());
                self.last_ports = Instant::now();
                if !self.serial.want_open {
                    return None;
                }
                match self.serial.current() {
                    Some(p) => Some(start_serial(tx, p.path.clone(), BAUDS[self.serial.baud])),
                    None => {
                        self.serial.want_open = false;
                        None
                    }
                }
            }
            PAGE_I2C => {
                self.i2c.buses = list_i2c();
                self.i2c.bus = self.i2c.bus.min(self.i2c.buses.len().saturating_sub(1));
                let bus = self.i2c.buses.get(self.i2c.bus)?.clone();
                self.i2c.state = I2cState::Scanning;
                Some(start_i2c(tx, bus))
            }
            PAGE_GPIO | PAGE_GROVE => Some(start_gpio(tx)),
            PAGE_W1 => Some(start_w1(tx)),
            _ => None,
        }
    }

    /// The page goes out of view: a serial port closes.
    pub fn leave(&mut self, page: usize) {
        if page == PAGE_SER && (self.serial.opened || self.serial.want_open) {
            self.serial.close(None);
        }
    }

    pub fn apply(&mut self, e: PortEv, _now_ms: u64) {
        match e {
            PortEv::Wifi(r) => {
                self.wifi_known = true;
                match r {
                    Ok(v) => {
                        self.wifi = v;
                        self.wifi_err = None;
                    }
                    Err(e) => self.wifi_err = Some(e),
                }
            }
            PortEv::SerOpened(f) => {
                if self.serial.want_open {
                    self.serial.file = Some(f);
                    self.serial.opened = true;
                    self.serial.err = None;
                    let msg = format!("Opened {} {}", self.serial.current().map_or("?", |p| p.path.trim_start_matches("/dev/")), BAUDS[self.serial.baud]);
                    self.serial.info(&msg);
                }
            }
            PortEv::SerRx(b) => {
                if self.serial.opened {
                    self.serial.feed(&b, Instant::now());
                }
            }
            PortEv::SerErr(e) => {
                self.serial.err = Some(e.clone());
                self.serial.close(Some(&e));
            }
            PortEv::I2c(bus, r) => {
                if self.i2c.buses.get(self.i2c.bus) == Some(&bus) {
                    match r {
                        Ok(v) => {
                            self.i2c.found = v;
                            self.i2c.state = I2cState::Done;
                        }
                        Err(e) => {
                            self.i2c.found.clear();
                            self.i2c.state = I2cState::Err(e);
                        }
                    }
                }
            }
            PortEv::Gpio(r) => match r {
                Ok(v) => {
                    self.gpio = v;
                    self.gpio_chip = self.gpio_chip.min(self.gpio.len().saturating_sub(1));
                    self.gpio_err = None;
                }
                Err(e) => self.gpio_err = Some(e),
            },
            PortEv::W1(r) => self.w1 = r,
        }
    }

    /// Send the typed line with the chosen line ending.
    pub fn serial_send(&mut self) {
        let s = &mut self.serial;
        if !s.opened {
            return;
        }
        let line = std::mem::take(&mut s.input);
        let mut bytes = line.clone().into_bytes();
        bytes.extend_from_slice(EOLS[s.eol].1);
        if let Some(f) = s.file.as_mut() {
            if let Err(e) = f.write_all(&bytes) {
                let e = e.to_string();
                s.err = Some(e.clone());
                s.close(Some(&e));
                return;
            }
        }
        s.tx_n += bytes.len() as u64;
        s.push(Dir::Tx, line.clone().into_bytes());
        if self.mock {
            let reply = if line.is_empty() { "> ".to_string() } else { format!("ok {line}") };
            s.push(Dir::Rx, reply.into_bytes());
        }
    }

    /// (rows, rows visible, first body row) of a list page here.
    pub fn dims(&self, page: usize) -> Option<(usize, usize, i32)> {
        let full = (BODY_ROWS - 1) as usize;
        match page {
            PAGE_WIFI => Some((self.wifi.len(), full, 1)),
            PAGE_SER if !self.serial.ports.is_empty() => Some((self.serial.log.len(), full - 1, 1)),
            PAGE_I2C => Some((self.i2c.found.len(), full, 1)),
            PAGE_GPIO => Some((self.gpio.get(self.gpio_chip).map_or(0, |c| c.lines.len()), full, 1)),
            PAGE_W1 => Some((self.w1.as_ref().map_or(0, |v| v.len()), full, 1)),
            _ => None,
        }
    }

    /// A package this page needs and does not have: (package, why), for the install card.
    pub fn missing_package(&self, page: usize) -> Option<(&'static str, &'static str)> {
        match page {
            PAGE_I2C if matches!(&self.i2c.state, I2cState::Err(e) if e.starts_with("i2cdetect not found")) => Some(("i2c-tools", "Interceptor scans the I2C bus with i2cdetect.")),
            PAGE_GPIO | PAGE_GROVE if self.gpio_err.as_deref().map_or(false, |e| e.starts_with("gpioinfo not found")) => Some(("gpiod", "Interceptor lists the GPIO lines with gpioinfo.")),
            _ => None,
        }
    }

    /// One action of a page (from a key or the menu). Returns true when it was this page's.
    fn act(&mut self, page: usize, id: &str, ctx: &Ctx) -> bool {
        match (page, id) {
            (PAGE_WIFI, "wifi_scan") | (PAGE_W1, "w1_scan") | (PAGE_I2C, "i2c_scan") => self.restart = true,
            (PAGE_I2C, "i2c_bus") => {
                if !self.i2c.buses.is_empty() {
                    self.i2c.bus = (self.i2c.bus + 1) % self.i2c.buses.len();
                    self.i2c.found.clear();
                    self.i2c.state = I2cState::Idle;
                    self.restart = true;
                }
            }
            (PAGE_GPIO, "gpio_chip") => {
                if !self.gpio.is_empty() {
                    self.gpio_chip = (self.gpio_chip + 1) % self.gpio.len();
                }
            }
            (PAGE_GROVE, "grove_5v") => {
                let on = !ctx.sys.ext_port(ExtPort::Grove5V).or(self.grove_5v).unwrap_or(false);
                if self.mock || ctx.sys.set_ext_port(ExtPort::Grove5V, on).is_ok() {
                    self.grove_5v = Some(on);
                }
            }
            (PAGE_GROVE, "grove_fun") => {
                let i2c = !self.grove_i2c.unwrap_or(false);
                if self.mock || std::fs::write(GROVE_FUN, if i2c { "1" } else { "0" }).is_ok() {
                    self.grove_i2c = Some(i2c);
                }
            }
            (PAGE_SER, "ser_open") => {
                if self.serial.opened || self.serial.want_open {
                    self.serial.close(None);
                    self.restart = true;
                } else if !self.serial.ports.is_empty() {
                    self.serial.want_open = true;
                    self.serial.err = None;
                    self.restart = true;
                }
            }
            (PAGE_SER, "ser_port" | "ser_baud") => {
                let reopen = self.serial.opened || self.serial.want_open;
                if reopen {
                    self.serial.close(None);
                }
                if id == "ser_port" && !self.serial.ports.is_empty() {
                    self.serial.port = (self.serial.port + 1) % self.serial.ports.len();
                } else if id == "ser_baud" {
                    self.serial.baud = (self.serial.baud + 1) % BAUDS.len();
                }
                self.serial.want_open = reopen;
                self.restart = true;
            }
            (PAGE_SER, "ser_hex") => self.serial.hex = !self.serial.hex,
            (PAGE_SER, "ser_eol") => self.serial.eol = (self.serial.eol + 1) % EOLS.len(),
            _ => return false,
        }
        true
    }

    /// A key on one of these pages. Returns true when it was used.
    pub fn key(&mut self, page: usize, k: Keycode, ctx: &Ctx) -> bool {
        let id = match (page, k) {
            (PAGE_WIFI, Keycode::Return) => "wifi_scan",
            (PAGE_W1, Keycode::Return) => "w1_scan",
            (PAGE_I2C, Keycode::Return) => "i2c_bus",
            (PAGE_I2C, Keycode::R) => "i2c_scan",
            (PAGE_GPIO, Keycode::Return) => "gpio_chip",
            (PAGE_GROVE, Keycode::Return) => "grove_5v",
            (PAGE_SER, Keycode::Return) => "ser_open",
            (PAGE_SER, Keycode::P) => "ser_port",
            (PAGE_SER, Keycode::B) => "ser_baud",
            (PAGE_SER, Keycode::H) => "ser_hex",
            (PAGE_SER, Keycode::E) => "ser_eol",
            _ => return false,
        };
        self.act(page, id, ctx)
    }

    /// The page's own menu items.
    pub fn menu(&self, page: usize) -> Vec<MenuItem> {
        match page {
            PAGE_WIFI => vec![MenuItem::new("wifi_scan", "Wi-Fi: scan again (Return)")],
            PAGE_GROVE => vec![
                MenuItem::new("grove_5v", "Grove: 5V on / off (Return)"),
                MenuItem::new("grove_fun", "Grove: switch UART / I2C"),
                MenuItem::new("grove_serial", "Grove: open in Serial"),
                MenuItem::new("grove_i2c", "Grove: scan I2C"),
            ],
            PAGE_SER => vec![
                MenuItem::new("ser_open", if self.serial.opened || self.serial.want_open { "Serial: close (Return)" } else { "Serial: open (Return)" }),
                MenuItem::new("ser_port", "Serial: next port (P)"),
                MenuItem::new("ser_baud", "Serial: next speed (B)"),
                MenuItem::new("ser_eol", "Serial: next line ending (E)"),
                MenuItem::new("ser_hex", "Serial: text / hex (H)"),
            ],
            PAGE_I2C => vec![MenuItem::new("i2c_scan", "I2C: scan again (R)"), MenuItem::new("i2c_bus", "I2C: next bus (Return)")],
            PAGE_GPIO => vec![MenuItem::new("gpio_chip", "GPIO: next chip (Return)")],
            PAGE_W1 => vec![MenuItem::new("w1_scan", "1-Wire: read again (Return)")],
            _ => Vec::new(),
        }
    }

    /// A menu choice; true when the page's worker must restart.
    pub fn menu_action(&mut self, page: usize, id: &str, ctx: &Ctx) -> bool {
        self.act(page, id, ctx) && std::mem::take(&mut self.restart)
    }

    /// Per frame: a quiet port shows its unfinished line; the Grove page reads the 5 V rail; the
    /// serial port list follows hot-plug; mock data moves.
    pub fn tick(&mut self, page: usize, paused: bool, ctx: &Ctx) -> bool {
        let now = Instant::now();
        let mut dirty = false;
        let s = &mut self.serial;
        if !s.partial.is_empty() && s.last_rx.map_or(false, |t| now.duration_since(t) >= Duration::from_millis(200)) {
            s.flush();
            dirty = true;
        }
        // A tool that was missing has just been installed (the install card): look again.
        if !self.mock && self.missing_package(page).is_some() && now.duration_since(self.last_ports) >= Duration::from_secs(1) {
            self.last_ports = now;
            let tool = if page == PAGE_I2C { "i2cdetect" } else { "gpioinfo" };
            if ["/usr/sbin", "/usr/bin", "/sbin", "/bin"].iter().any(|d| Path::new(d).join(tool).exists()) {
                self.gpio_err = None;
                self.i2c.state = I2cState::Idle;
                self.restart = true;
                dirty = true;
            }
        }
        if page == PAGE_GROVE && !self.mock && self.last_grove.map_or(true, |t| now.duration_since(t) >= Duration::from_secs(1)) {
            self.last_grove = Some(now);
            let v = ctx.sys.ext_port(ExtPort::Grove5V);
            let f = read_grove_fun();
            dirty |= v != self.grove_5v || f != self.grove_i2c;
            self.grove_5v = v;
            self.grove_i2c = f;
        }
        if page == PAGE_SER && !self.mock && !self.serial.opened && now.duration_since(self.last_ports) >= Duration::from_secs(2) {
            self.last_ports = now;
            let list = list_serial();
            if list != self.serial.ports {
                self.serial.set_ports(list);
                dirty = true;
            }
        }
        if self.mock && !paused && page == PAGE_SER && self.serial.opened && now.duration_since(self.last_mock) >= Duration::from_millis(900) {
            self.last_mock = now;
            self.mock_n += 1;
            let line = format!("t=23.{} h={} p=1012.{}", self.mock_n % 10, 40 + self.mock_n % 3, self.mock_n % 7);
            self.serial.feed(format!("{line}\r\n").as_bytes(), now);
            dirty = true;
        }
        dirty
    }
}

// ====================================================================== drawing

impl Interceptor {
    /// Rows `start..end` of a list page that is not a log, with the window kept in range.
    fn window(&mut self, p: usize, total: usize, vis: usize) -> std::ops::Range<usize> {
        self.scroll[p] = self.scroll[p].min(total.saturating_sub(vis));
        let s = self.scroll[p];
        s..(s + vis).min(total)
    }

    /// The "Install ..." button a page shows when a tool it needs is missing (Return or a click).
    pub(super) fn install_button_rect() -> Rect {
        Rect::new(LIST_X + (LIST_W - 180) / 2, BODY_Y + 4 * ROW + 4, 180, 24)
    }

    fn draw_install_button(&self, c: &mut Canvas<Window>, ctx: &Ctx, page: usize) {
        if let Some((pkg, _)) = self.ports.missing_package(page) {
            let label = i18n::tr_s(&format!("Install {pkg}..."));
            widgets::draw_button(c, ctx.fs, ctx.tm, Self::install_button_rect(), &label, widgets::BtnState { default: true, ..Default::default() });
        }
    }

    /// A page with nothing to show yet: a header and plain lines under it.
    fn explain(&self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal, title: &str, lines: &[String]) {
        header_band(c, ctx, p, 0, title, "");
        for (i, l) in lines.iter().enumerate().take((BODY_ROWS - 1) as usize) {
            let row = row_rect(1 + i as i32);
            zebra(c, p, row, i + 1);
            text_row(c, ctx, row.x() + 4, row.y(), LIST_W - 8, l, p.faint);
        }
    }

    pub(super) fn draw_wifi(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        if !self.ports.wifi_known {
            return self.explain(c, ctx, p, "Scanning...", &[]);
        }
        let joined = self.ports.wifi.iter().find(|n| n.in_use).map(|n| n.ssid.clone());
        let right = match &self.ports.wifi_err {
            Some(e) => e.clone(),
            None => format!("{} networks", self.ports.wifi.len()),
        };
        match joined {
            Some(s) => band_at(c, ctx, p, row_rect(0), &format!("Wi-Fi  {s}"), &right),
            None => header_band(c, ctx, p, 0, "Not connected", &right),
        }
        let vis = (BODY_ROWS - 1) as usize;
        let total = self.ports.wifi.len();
        let r = self.window(PAGE_WIFI, total, vis);
        let n = r.len();
        for (k, i) in r.enumerate() {
            let net = self.ports.wifi[i].clone();
            let row = row_rect(1 + k as i32);
            let (fg, sub) = row_style(c, ctx, p, row, i, i == self.cur[PAGE_WIFI]);
            let mark = if net.in_use { "* " } else { "  " };
            let name = if net.ssid.is_empty() { i18n::tr_s("(hidden)") } else { net.ssid.clone() };
            text_data(c, ctx, row.x() + 2, row.y(), 140, &format!("{mark}{name}"), if net.ssid.is_empty() { sub } else { fg });
            draw_meter(c, ctx.tm, Rect::new(row.x() + 148, row.y() + 4, 40, 8), net.signal as f32 / 100.0);
            text_data(c, ctx, row.x() + 196, row.y(), 52, &format!("{} {}", net.chan, wifi_band(net.chan)), sub);
            let sec = if net.security == "open" { i18n::tr_s("open") } else { net.security.split_whitespace().next().unwrap_or("").to_string() };
            right_text(c, ctx, row.right() - 4, row.y(), &sec, sub);
        }
        for x in [144, 192, 250] {
            vsep_rows(c, p, LIST_X + x, 1, n);
        }
        scrollbar(c, ctx, total, vis, self.scroll[PAGE_WIFI]);
    }

    /// USB devices in port order, with the ones plugged (+1) or pulled (-1) a moment ago.
    pub(super) fn usb_rows(&self) -> Vec<(UsbDev, i8)> {
        let now = Instant::now();
        let mut v: Vec<(UsbDev, i8)> = self.usb.devs.iter().map(|d| (d.clone(), if self.usb.added.get(&d.name).map_or(false, |t| now.duration_since(*t) < FLASH) { 1 } else { 0 })).collect();
        v.extend(self.usb.removed.iter().map(|(d, _)| (d.clone(), -1)));
        v
    }

    pub(super) fn draw_usb(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let rows = self.usb_rows();
        let devices = rows.iter().filter(|(d, s)| *s >= 0 && !d.name.starts_with("usb")).count();
        header_band(c, ctx, p, 0, "USB host", &format!("{devices} devices"));
        let vis = (BODY_ROWS - 1) as usize;
        let r = self.window(PAGE_USB, rows.len(), vis);
        let n = r.len();
        for (k, i) in r.enumerate() {
            let (d, state) = &rows[i];
            let row = row_rect(1 + k as i32);
            let (fg, sub) = row_style(c, ctx, p, row, i, i == self.cur[PAGE_USB]);
            let indent = (d.depth() as i32 * 8).min(40);
            let label = if d.name.starts_with("usb") { i18n::tr_s(&format!("Root hub {}", d.name)) } else { d.label() };
            let label = match state {
                1 => format!("+ {label}"),
                -1 => format!("- {label}"),
                _ => label,
            };
            text_data(c, ctx, row.x() + 4 + indent, row.y(), LIST_W - 100 - indent, &label, if *state < 0 { sub } else { fg });
            text_data(c, ctx, row.right() - 88, row.y(), 56, &format!("{:04x}:{:04x}", d.vid, d.pid), sub);
            right_text(c, ctx, row.right() - 4, row.y(), &d.speed_label(), sub);
        }
        vsep_rows(c, p, LIST_X + LIST_W - 92, 1, n);
        scrollbar(c, ctx, rows.len(), vis, self.scroll[PAGE_USB]);
    }

    pub(super) fn draw_grove(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let on = self.ports.grove_5v;
        let fun = self.ports.grove_i2c;
        let power = match on {
            Some(true) => "5V on",
            Some(false) => "5V off",
            None => "5V unknown",
        };
        header_band(c, ctx, p, 0, "Grove port", power);
        // Rows 1-2: the connector, pin by pin as printed on the case (G 5V RX TX).
        let area = rows_rect(1, 2);
        ui::fill_rect(c, area, p.paper);
        let pins: [(&str, String); 4] = [
            ("GND", "-".into()),
            ("5V", i18n::tr_s(if on == Some(true) { "on" } else { "off" })),
            // Names as printed on the case; in I2C mode the pins are routed to the I2C bus.
            ("RX", if fun == Some(true) { "I2C".into() } else { "GPIO15".into() }),
            ("TX", if fun == Some(true) { "I2C".into() } else { "GPIO14".into() }),
        ];
        let pw = 64;
        let x0 = area.x() + (area.width() as i32 - 4 * pw - 3 * 8) / 2;
        for (i, (name, sub)) in pins.iter().enumerate() {
            let r = Rect::new(x0 + i as i32 * (pw + 8), area.y() + 2, pw as u32, (area.height() - 5) as u32);
            let live = i == 1 && on == Some(true);
            if live {
                widgets::draw_row_plate(c, ctx.tm, r);
            } else {
                ui::fill_rect(c, r, p.bg);
            }
            ui::draw_rect(c, r, p.ink);
            let (fg, fsub) = if live { (p.sel_fg, p.sel_fg) } else { (p.ink, p.faint) };
            let w = ctx.fs.measure_text(name);
            ctx.fs.draw_text(c, r.x() + (pw - w) / 2, r.y() + 1, name, fg);
            let w = ctx.fs.measure_text(sub);
            ctx.fs.draw_text(c, r.x() + (pw - w) / 2, r.y() + 14, sub, fsub);
        }
        ui::line(c, area.x(), area.bottom() - 1, area.right() - 1, area.bottom() - 1, p.band);
        let line_of = |n: u32| -> String {
            self.ports.gpio.first().and_then(|ch| ch.lines.iter().find(|l| l.num == n)).and_then(|l| l.consumer.clone()).unwrap_or_else(|| "-".into())
        };
        let ser = &self.ports.serial;
        let ser_state = if ser.is_open() && ser.current().map_or(false, |p| p.path == GROVE_TTY) { format!("open, {} baud", BAUDS[ser.baud]) } else { i18n::tr_s("closed") };
        let rows: Vec<(&str, String)> = vec![
            ("Function", match fun {
                Some(false) => format!("UART ({})", GROVE_TTY.trim_start_matches("/dev/")),
                Some(true) => "I2C".into(),
                None => i18n::tr_s("unknown"),
            }),
            ("Selected by", format!("GPIO4 ({})", line_of(4))),
            ("GPIO14 (TX)", line_of(14)),
            ("GPIO15 (RX)", line_of(15)),
            ("Serial", ser_state),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            let row = row_rect(3 + i as i32);
            zebra(c, p, row, i);
            text_row(c, ctx, row.x() + 4, row.y(), 92, k, p.faint);
            text_data(c, ctx, row.x() + 104, row.y(), LIST_W - 108, v, p.ink);
        }
        vsep_rows(c, p, LIST_X + 100, 3, rows.len());
    }

    pub(super) fn draw_serial(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let s = &self.ports.serial;
        if s.ports.is_empty() {
            let lines = vec![
                "Plug in a USB serial device (ttyACM / ttyUSB).".to_string(),
                "The Grove port is ttyS0 when the UART is on:".into(),
                "enable_uart=1 in /boot/firmware/config.txt.".into(),
                "Needs read / write access (group dialout).".into(),
            ];
            return self.explain(c, ctx, p, "No serial port found", &lines);
        }
        let port = s.current().cloned().unwrap_or(SerPort { path: "?".into(), label: String::new() });
        let left = format!("{}  {}  {}", port.path.trim_start_matches("/dev/"), BAUDS[s.baud], EOLS[s.eol].0);
        let right = if s.is_open() {
            format!("{}  RX {}", if s.hex { "hex" } else { "text" }, fmt_bytes(s.rx_n))
        } else if s.want_open {
            i18n::tr_s("Opening...")
        } else {
            i18n::tr_s("Port closed")
        };
        band_at(c, ctx, p, row_rect(0), &left, &right);
        let vis = (BODY_ROWS - 2) as usize;
        let total = s.log.len();
        self.scroll[PAGE_SER] = self.scroll[PAGE_SER].min(total.saturating_sub(vis));
        let end = total - self.scroll[PAGE_SER];
        let start = end.saturating_sub(vis);
        let s = &self.ports.serial;
        for (k, i) in (start..end).enumerate() {
            let l = &s.log[i];
            let row = row_rect(1 + k as i32);
            let (fg, sub) = row_style(c, ctx, p, row, i, i == self.cur[PAGE_SER]);
            let col = if l.dir == Dir::Info { sub } else { fg };
            let text = l.render(s.hex);
            if l.dir == Dir::Info {
                text_row(c, ctx, row.x() + 4, row.y(), LIST_W - 8, &text, col);
            } else {
                text_data(c, ctx, row.x() + 4, row.y(), LIST_W - 8, &text, col);
            }
        }
        // The last row: the line to send (the shared text field) while open, the port's name when closed.
        let row = row_rect(BODY_ROWS - 1);
        if s.is_open() {
            widgets::draw_field(c, ctx.fs, ctx.tm, row, &widgets::Field { text: &s.input, caret: Some(s.input.chars().count()), prefix: ">", ..Default::default() });
        } else {
            ui::fill_rect(c, row, p.paper);
            ui::line(c, row.x(), row.y(), row.right() - 1, row.y(), p.band);
            let label = if let Some(e) = &s.err { e.clone() } else { port.label.clone() };
            text_data(c, ctx, row.x() + 4, row.y(), LIST_W - 8, &label, p.faint);
        }
        scrollbar(c, ctx, total, vis, total.saturating_sub(vis).saturating_sub(self.scroll[PAGE_SER]));
    }

    pub(super) fn draw_i2c(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let i2c = &self.ports.i2c;
        if i2c.buses.is_empty() && !self.mock {
            let lines = vec!["No /dev/i2c-* bus found.".to_string(), "Missing: dtparam=i2c_arm=on".into(), "in /boot/firmware/config.txt, then reboot.".into(), "Needs read / write access (group i2c).".into()];
            return self.explain(c, ctx, p, "No I2C bus found", &lines);
        }
        let bus = i2c.buses.get(i2c.bus).cloned().unwrap_or_default();
        let right = match &i2c.state {
            I2cState::Idle | I2cState::Scanning => i18n::tr_s("Scanning..."),
            I2cState::Done => format!("{} found", i2c.found.len()),
            I2cState::Err(_) => i18n::tr_s("Scan failed"),
        };
        band_at(c, ctx, p, row_rect(0), &bus, &right);
        if let I2cState::Err(e) = &i2c.state {
            let row = row_rect(1);
            zebra(c, p, row, 1);
            text_row(c, ctx, row.x() + 4, row.y(), LIST_W - 8, e, p.faint);
            self.draw_install_button(c, ctx, PAGE_I2C);
            return;
        }
        if i2c.state == I2cState::Done && i2c.found.is_empty() {
            let row = row_rect(1);
            zebra(c, p, row, 1);
            text_row(c, ctx, row.x() + 4, row.y(), LIST_W - 8, "No device answered.", p.faint);
            return;
        }
        let vis = (BODY_ROWS - 1) as usize;
        let total = self.ports.i2c.found.len();
        let r = self.window(PAGE_I2C, total, vis);
        let n = r.len();
        for (k, i) in r.enumerate() {
            let (addr, used) = self.ports.i2c.found[i];
            let row = row_rect(1 + k as i32);
            let (fg, sub) = row_style(c, ctx, p, row, i, i == self.cur[PAGE_I2C]);
            text_data(c, ctx, row.x() + 4, row.y(), 36, &format!("0x{addr:02X}"), fg);
            let hint = i2c_hint(addr);
            text_data(c, ctx, row.x() + 48, row.y(), LIST_W - 130, if hint.is_empty() { "?" } else { hint }, if hint.is_empty() { sub } else { fg });
            if used {
                right_text(c, ctx, row.right() - 4, row.y(), "kernel driver", sub);
            }
        }
        vsep_rows(c, p, LIST_X + 44, 1, n);
        scrollbar(c, ctx, total, vis, self.scroll[PAGE_I2C]);
    }

    pub(super) fn draw_gpio(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        if self.ports.gpio.is_empty() {
            let lines = match &self.ports.gpio_err {
                Some(e) => vec![e.clone()],
                None => vec!["Reading the GPIO lines...".to_string()],
            };
            self.explain(c, ctx, p, "GPIO", &lines);
            return self.draw_install_button(c, ctx, PAGE_GPIO);
        }
        let chip = self.ports.gpio[self.ports.gpio_chip.min(self.ports.gpio.len() - 1)].clone();
        let used = chip.lines.iter().filter(|l| l.consumer.is_some()).count();
        band_at(c, ctx, p, row_rect(0), &format!("{}  {}", chip.name, chip.label), &format!("{used} / {} used", chip.lines.len()));
        let vis = (BODY_ROWS - 1) as usize;
        let r = self.window(PAGE_GPIO, chip.lines.len(), vis);
        let n = r.len();
        for (k, i) in r.enumerate() {
            let l = &chip.lines[i];
            let row = row_rect(1 + k as i32);
            let (fg, sub) = row_style(c, ctx, p, row, i, i == self.cur[PAGE_GPIO]);
            right_text(c, ctx, row.x() + 22, row.y(), &l.num.to_string(), sub);
            text_data(c, ctx, row.x() + 30, row.y(), 104, if l.name.is_empty() { "-" } else { &l.name }, fg);
            let dir = format!("{}{}", if l.output { "out" } else { "in" }, if l.active_low { " low" } else { "" });
            text_data(c, ctx, row.x() + 142, row.y(), 50, &dir, sub);
            text_data(c, ctx, row.x() + 198, row.y(), LIST_W - 202, l.consumer.as_deref().unwrap_or("-"), if l.consumer.is_some() { fg } else { sub });
        }
        for x in [26, 138, 194] {
            vsep_rows(c, p, LIST_X + x, 1, n);
        }
        scrollbar(c, ctx, chip.lines.len(), vis, self.scroll[PAGE_GPIO]);
    }

    pub(super) fn draw_w1(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let devs = match &self.ports.w1 {
            Err(lines) => {
                let lines = lines.clone();
                return self.explain(c, ctx, p, "No 1-Wire bus found", &lines);
            }
            Ok(v) => v.clone(),
        };
        header_band(c, ctx, p, 0, "1-Wire", &format!("{} devices", devs.len()));
        if devs.is_empty() {
            let row = row_rect(1);
            zebra(c, p, row, 1);
            text_row(c, ctx, row.x() + 4, row.y(), LIST_W - 8, "No device on the bus.", p.faint);
            return;
        }
        let vis = (BODY_ROWS - 1) as usize;
        let r = self.window(PAGE_W1, devs.len(), vis);
        let n = r.len();
        for (k, i) in r.enumerate() {
            let d = &devs[i];
            let row = row_rect(1 + k as i32);
            let (fg, sub) = row_style(c, ctx, p, row, i, i == self.cur[PAGE_W1]);
            text_data(c, ctx, row.x() + 4, row.y(), 104, &mask_w1(&d.id, self.mask), fg);
            text_data(c, ctx, row.x() + 116, row.y(), 110, w1_family(&d.id), sub);
            if let Some(t) = d.temp_mc {
                right_text(c, ctx, row.right() - 4, row.y(), &fmt_temp(t), fg);
            }
        }
        vsep_rows(c, p, LIST_X + 112, 1, n);
        scrollbar(c, ctx, devs.len(), vis, self.scroll[PAGE_W1]);
    }
}

// ======================================================================== tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nmcli_wifi_list_is_parsed_with_escapes() {
        let t = "*:harbour-5G:F0\\:9F\\:C2\\:11\\:22\\:33:36:82:WPA2\n :cafe\\:free:AA\\:BB\\:CC\\:DD\\:EE\\:FF:11:28:\n :aterm:11\\:22\\:33\\:44\\:55\\:66:1:71:WPA2 WPA3\nbroken line\n";
        let v = parse_nmcli_wifi(t);
        assert_eq!(v.len(), 3);
        assert!(v[0].in_use && v[0].ssid == "harbour-5G" && v[0].bssid == "F0:9F:C2:11:22:33" && v[0].chan == 36);
        assert_eq!(v[1].ssid, "aterm", "then by signal");
        assert_eq!(v[2].ssid, "cafe:free");
        assert_eq!(v[2].security, "open");
        assert_eq!((wifi_band(11), wifi_band(36)), ("2.4G", "5G"));
    }

    #[test]
    fn serial_ports_are_named_and_ordered() {
        for n in ["ttyACM0", "ttyUSB12", "ttyAMA0", "ttyS0", "ttyGS0"] {
            assert!(is_serial_name(n), "{n}");
        }
        for n in ["ttyS1", "tty1", "ttyACM", "ttyUSBx", "console"] {
            assert!(!is_serial_name(n), "{n}");
        }
        let by_id = vec![("usb-Espressif_USB_JTAG_serial_debug_unit_F4:12:FA:00:93:D1-if00".to_string(), "ttyACM0".to_string()), ("usb-FTDI_FT232R_USB_UART_A10K5XYZ-if00-port0".to_string(), "ttyUSB0".to_string())];
        assert_eq!(port_label("/dev/ttyACM0", &by_id), "Espressif USB JTAG serial debug unit");
        assert_eq!(port_label("/dev/ttyUSB0", &by_id), "FTDI FT232R USB UART");
        assert_eq!(port_label("/dev/ttyS0", &by_id), "Grove port (GPIO14 / 15)");
        assert_eq!(port_label("/dev/ttyGS0", &by_id), "USB gadget serial");
        let mut v = vec!["/dev/ttyGS0", "/dev/ttyS0", "/dev/ttyUSB1", "/dev/ttyACM10", "/dev/ttyACM2"];
        v.sort_by_key(|p| port_order(p));
        assert_eq!(v, ["/dev/ttyACM2", "/dev/ttyACM10", "/dev/ttyUSB1", "/dev/ttyS0", "/dev/ttyGS0"]);
    }

    #[test]
    fn serial_bytes_become_lines() {
        let mut s = SerialView::new();
        let t = Instant::now();
        s.feed(b"hello\r\nwor", t);
        assert_eq!(s.log.len(), 1);
        assert_eq!(s.log[0].render(false), "hello");
        s.feed(b"ld\n\n", t);
        assert_eq!(s.log.len(), 2, "empty lines are skipped");
        assert_eq!(s.log[1].render(true), "77 6F 72 6C 64");
        s.feed(b"login: ", t);
        assert_eq!(s.log.len(), 2);
        s.flush();
        assert_eq!(s.log[2].render(false), "login: ");
        s.feed(&[0x41, 0x07, 0x42, b'\n'], t);
        assert_eq!(s.log[3].render(false), "A.B", "control characters show as dots");
        s.feed(&[b'x'; 70], t);
        assert_eq!(s.log[4].bytes.len(), 64, "long lines are cut");
        s.push(Dir::Tx, b"status".to_vec());
        assert_eq!(s.log.back().unwrap().render(false), "> status");
        assert_eq!(s.rx_n, 10 + 4 + 7 + 4 + 70, "every byte counted");
    }

    #[test]
    fn mock_serial_sends_and_closes_when_the_page_goes() {
        let mut p = Ports::new(true);
        assert!(p.serial.is_open());
        p.serial.input = "ping".into();
        p.serial_send();
        let tail: Vec<String> = p.serial.log.iter().rev().take(2).map(|l| l.render(false)).collect();
        assert_eq!(tail, ["ok ping", "> ping"]);
        assert!(p.serial.input.is_empty());
        assert_eq!(p.serial.tx_n, 7 + 5, "the line ending is sent too");
        p.leave(PAGE_SER);
        assert!(!p.serial.is_open() && !p.serial.want_open);
        assert_eq!(p.serial.log.back().unwrap().render(false), "Closed");
    }

    #[test]
    fn i2cdetect_grid_is_read() {
        let t = "     0  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f
00:                         -- -- -- -- -- -- -- --
10: UU -- -- -- -- -- -- -- -- -- -- -- -- -- -- --
30: -- -- -- -- -- -- -- -- -- -- -- -- 3c -- -- --
50: -- -- -- -- -- UU -- -- -- -- -- -- -- -- -- --
70: -- -- -- -- -- -- 76 --
";
        assert_eq!(parse_i2cdetect(t), vec![(0x10, true), (0x3C, false), (0x55, true), (0x76, false)]);
        assert_eq!(i2c_hint(0x3C), "SSD1306 / SH1106 OLED");
        assert_eq!(i2c_hint(0x02), "");
    }

    #[test]
    fn gpioinfo_v1_and_v2_are_read() {
        let v1 = "gpiochip0 - 3 lines:
\tline   0:     \"ID_SDA\"       unused   input  active-high
\tline   4:      \"GPIO4\" \"grove_fun\"  output  active-high [used]
\tline  24:      unnamed   \"lcd reset\"  output   active-low [used]
gpiochip1 - 1 lines:
\tline   0:      \"BT_ON\"       unused  output  active-high
";
        let c = parse_gpioinfo(v1);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].lines.len(), 3);
        assert_eq!(c[0].lines[0], GpioLine { num: 0, name: "ID_SDA".into(), consumer: None, output: false, active_low: false });
        assert_eq!(c[0].lines[1].consumer.as_deref(), Some("grove_fun"));
        assert!(c[0].lines[1].output);
        assert_eq!(c[0].lines[2].name, "");
        assert_eq!(c[0].lines[2].consumer.as_deref(), Some("lcd reset"));
        assert!(c[0].lines[2].active_low);
        let v2 = "gpiochip0 - 54 lines:
\tline   0:\t\"ID_SDA\"        \tinput
\tline   4:\t\"GPIO4\"         \toutput consumer=\"grove_fun\"
\tline  17:\t\"GPIO17\"        \tinput active-low edges=both consumer=\"gpio ir\"
";
        let c = parse_gpioinfo(v2);
        assert_eq!(c[0].lines[1].consumer.as_deref(), Some("grove_fun"));
        assert_eq!(c[0].lines[2].consumer.as_deref(), Some("gpio ir"));
        assert!(c[0].lines[2].active_low && !c[0].lines[2].output);
        assert_eq!(parse_gpiodetect("gpiochip0 [pinctrl-bcm2835] (54 lines)\ngpiochip1 [raspberrypi-exp-gpio] (8 lines)\n")[1], ("gpiochip1".to_string(), "raspberrypi-exp-gpio".to_string()));
    }

    #[test]
    fn one_wire_sensors() {
        assert_eq!(parse_w1_slave("72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=23125\n"), Some(23125));
        assert_eq!(parse_w1_slave("72 01 : crc=00 NO\n72 01 t=85000\n"), None, "a bad CRC is no reading");
        assert_eq!(w1_family("28-0316a2795cff"), "DS18B20");
        assert_eq!(mask_w1("28-0316a2795cff", true), "28-********5cff");
        assert_eq!(mask_w1("28-0316a2795cff", false), "28-0316a2795cff");
        assert_eq!(fmt_temp(23437), "23.4℃");
        let root = std::env::temp_dir().join(format!("harbour-w1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(read_w1(&root).is_err(), "no bus");
        std::fs::create_dir_all(root.join("w1_bus_master1")).unwrap();
        std::fs::create_dir_all(root.join("28-aaa")).unwrap();
        std::fs::write(root.join("28-aaa/temperature"), "21500\n").unwrap();
        std::fs::create_dir_all(root.join("10-bbb")).unwrap();
        std::fs::write(root.join("10-bbb/w1_slave"), "x : crc=1 YES\nx t=-1250\n").unwrap();
        let v = read_w1(&root).unwrap();
        assert_eq!(v, vec![W1Dev { id: "10-bbb".into(), temp_mc: Some(-1250) }, W1Dev { id: "28-aaa".into(), temp_mc: Some(21500) }]);
        let _ = std::fs::remove_dir_all(root);
    }
}
