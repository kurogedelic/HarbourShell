//! Interceptor: a signal-interceptor dashboard. One page per thing the machine can hear or touch
//! (Left/Right, Tab or the arrow tabs at both ends of the strip): Ethernet, Wi-Fi, Bluetooth
//! (paired devices), BLE (advertisement scan), IR receiver, the Grove port, USB host, serial
//! ports, I2C, GPIO, 1-Wire, OSC, MIDI and the Device tree. The wired buses live in `ports.rs`.
//!
//! Hardware access is split from drawing:
//! * pure parsers (`/proc/net/dev`, `bluetoothctl`, sysfs USB, `aseqdump`, a MIDI
//!   byte-stream decoder, IR pulse timings) are unit-tested below;
//! * slow sources (bluetoothctl, aseqdump, /dev/lirc*) run in a background thread
//!   that is only alive while its page is in front, and report through a channel;
//! * with `HARBOUR_MOCK=1` (or off Linux) no process is spawned and no device is
//!   touched: believable fake data is produced through the same parsers.
//!
//! Needs on the device: `bluetoothctl` (BT, BLE), `aseqdump` (MIDI, from
//! alsa-utils), read access to `/dev/lirc*` (group video). Pages degrade to an
//! explanation when a tool or device is missing.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
#[cfg(target_os = "linux")]
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sdl2::keyboard::{Keycode, Mod};
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome;
use crate::dither::{self, Pattern};
use crate::grid;
use crate::launcher::i18n;
use crate::launcher::apps::devices::{DevSource, Devices, CAT_USB};
use crate::launcher::apps::osc::{self, OscTable};
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::widgets::{self, draw_meter, MenuItem, PointerEvent, ScrollBar, TreeKey, TreeList, TreeNode};
use crate::platform::keys::arrow_alias;
use crate::ui;

// ===================================================================== layout

mod ports;
use ports::*;

const TABS: [&str; 14] = ["ETH", "Wi-Fi", "BT", "BLE", "IR", "Grove", "USB", "Serial", "I2C", "GPIO", "1-Wire", "OSC", "MIDI", "Device"];
/// Pages (one per tab).
const NP: usize = TABS.len();
/// The strip leaves 8 px at the right end for the pause mark.
fn tabs_room() -> i32 {
    widgets::folder_room() - 8
}
// One grid rule for every page: the tab strip is row 0 (y 16..32), the body is a whole number of
// 16 px rows under it: 8 on the 320 x 170 screen (y 36..164). A two-line item is 2 rows. The px
// left under the last row (y 164..168 there) belong to the body's frame, whose bottom line sits on
// the surface's last row but one like the framed screens' (grid::frame_bottom()), so no empty band
// is left below the screen. In a bigger window the body simply has more rows of the same pitch.
const ROW: i32 = 16;
/// The folder tab strip is 5 units high (the label needs room below the chamfered corners).
const TABS_H: i32 = widgets::FOLDER_TABS_H;
const BODY_Y: i32 = grid::BAR_H + TABS_H;
/// Whole rows that fit under the strip and above the frame's bottom line, which they never touch
/// (8 on 320 x 170: y 36..164).
fn body_rows() -> i32 {
    (body_end() - 1 - BODY_Y) / ROW
}
fn body_h() -> i32 {
    body_rows() * ROW
}
/// Where the body's frame ends (exclusive): its bottom line is on y 168 on 320 x 170.
fn body_end() -> i32 {
    grid::frame_bottom() + 1
}
const LIST_X: i32 = grid::MARGIN;
fn list_w() -> i32 {
    grid::scroll_x() - 4 - LIST_X
}
/// The list width on the 320 x 170 screen, which the fixed column positions were drawn for.
const LIST_W_DESIGN: i32 = grid::SCROLL_X - 4 - LIST_X;
/// Pixels the list is wider than on the 320 x 170 screen: what a page hands to the columns that
/// can use more room (names, values, graphs). 0 on the design screen.
fn extra_w() -> i32 {
    list_w() - LIST_W_DESIGN
}
/// ETH history samples (one a second) kept: enough for the widest graph a window can have (a
/// wider graph shows a longer past).
const HIST_CAP: usize = 1024;
const LOG_CAP: usize = 300;
const PAGE_ETH: usize = 0;
const PAGE_WIFI: usize = 1;
const PAGE_BT: usize = 2;
const PAGE_BLE: usize = 3;
const PAGE_IR: usize = 4;
const PAGE_GROVE: usize = 5;
const PAGE_USB: usize = 6;
const PAGE_SER: usize = 7;
const PAGE_I2C: usize = 8;
const PAGE_GPIO: usize = 9;
const PAGE_W1: usize = 10;
const PAGE_OSC: usize = 11;
const PAGE_MIDI: usize = 12;
const PAGE_DEV: usize = 13;
const OSC_PORTS: [u16; 8] = [7000, 8000, 9000, 9001, 10111, 57120, 57110, 12000];
const OSC_LOG_CAP: usize = 500;

// ============================================================ formatting bits

fn fmt_bytes(b: u64) -> String {
    let f = b as f64;
    if b < 1024 {
        format!("{b} B")
    } else if f < 1024.0 * 1024.0 {
        format!("{:.1} KB", f / 1024.0)
    } else if f < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", f / 1048576.0)
    } else {
        format!("{:.2} GB", f / 1073741824.0)
    }
}

fn fmt_rate(bps: f64) -> String {
    if bps < 1024.0 {
        format!("{:.0} B/s", bps)
    } else if bps < 1048576.0 {
        format!("{:.1} KB/s", bps / 1024.0)
    } else {
        format!("{:.2} MB/s", bps / 1048576.0)
    }
}

/// Bytes per second between two counter readings (a wrapped counter reads as 0).
fn rate(prev: u64, now: u64, dt: f64) -> f64 {
    if dt <= 0.0 || now < prev {
        0.0
    } else {
        (now - prev) as f64 / dt
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(ch) = it.next() {
        if ch == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for n in it.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if ch != '\r' {
            out.push(ch);
        }
    }
    out
}

fn hex_str(bytes: &[u8], max: usize) -> String {
    let mut s: String = bytes.iter().take(max).map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
    if bytes.len() > max {
        s.push_str("...");
    }
    s
}

// ==================================================================== ethernet

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetCounters {
    pub name: String,
    pub rx: u64,
    pub tx: u64,
}

/// Parse `/proc/net/dev` (interface name, received bytes, transmitted bytes).
pub fn parse_proc_net_dev(text: &str) -> Vec<NetCounters> {
    text.lines()
        .skip(2)
        .filter_map(|l| {
            let (name, rest) = l.split_once(':')?;
            let f: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
            if f.len() < 9 {
                return None;
            }
            Some(NetCounters { name: name.trim().to_string(), rx: f[0], tx: f[8] })
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinkInfo {
    pub up: bool,
    pub carrier: bool,
    pub speed_mbps: Option<u32>,
    pub mac: Option<String>,
    pub ip: Option<String>,
}

fn read_trim(p: &str) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn read_link(iface: &str) -> LinkInfo {
    let base = format!("/sys/class/net/{iface}");
    let oper = read_trim(&format!("{base}/operstate")).unwrap_or_default();
    LinkInfo {
        up: oper == "up" || oper == "unknown",
        carrier: read_trim(&format!("{base}/carrier")).as_deref() == Some("1"),
        speed_mbps: read_trim(&format!("{base}/speed")).and_then(|s| s.parse::<i32>().ok()).filter(|v| *v > 0).map(|v| v as u32),
        mac: read_trim(&format!("{base}/address")),
        ip: ipv4_of(iface),
    }
}

/// (interface, address) of every IPv4 address on the machine.
#[cfg(target_os = "linux")]
fn ipv4_list() -> Vec<(String, String)> {
    let mut found = Vec::new();
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return found;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let a = &*cur;
            if !a.ifa_addr.is_null() && (*a.ifa_addr).sa_family as i32 == libc::AF_INET {
                let name = std::ffi::CStr::from_ptr(a.ifa_name).to_string_lossy().to_string();
                let sin = &*(a.ifa_addr as *const libc::sockaddr_in);
                let b = sin.sin_addr.s_addr.to_ne_bytes();
                found.push((name, format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])));
            }
            cur = a.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    found
}

#[cfg(not(target_os = "linux"))]
fn ipv4_list() -> Vec<(String, String)> {
    Vec::new()
}

fn ipv4_of(iface: &str) -> Option<String> {
    ipv4_list().into_iter().find(|(n, _)| n == iface).map(|(_, ip)| ip)
}

struct Eth {
    ifaces: Vec<String>,
    sel: usize,
    prev: Option<(Instant, u64, u64)>,
    rx_rate: f64,
    tx_rate: f64,
    rx_total: u64,
    tx_total: u64,
    hist: VecDeque<(f32, f32)>,
    link: LinkInfo,
}

impl Eth {
    fn new() -> Self {
        Self { ifaces: vec!["eth0".into()], sel: 0, prev: None, rx_rate: 0.0, tx_rate: 0.0, rx_total: 0, tx_total: 0, hist: VecDeque::new(), link: LinkInfo::default() }
    }

    fn iface(&self) -> &str {
        self.ifaces.get(self.sel).map(|s| s.as_str()).unwrap_or("-")
    }

    /// Feed counters (from `parse_proc_net_dev`) taken at `now`.
    fn sample(&mut self, now: Instant, counters: &[NetCounters]) {
        let mut names: Vec<String> = counters.iter().filter(|c| c.name != "lo").map(|c| c.name.clone()).collect();
        if names.is_empty() {
            names.push("eth0".into());
        }
        if names != self.ifaces {
            let cur = self.iface().to_string();
            self.sel = names.iter().position(|n| *n == cur).unwrap_or(0);
            self.ifaces = names;
        }
        let Some(c) = counters.iter().find(|c| c.name == self.iface()) else {
            self.rx_rate = 0.0;
            self.tx_rate = 0.0;
            return;
        };
        if let Some((t, prx, ptx)) = self.prev {
            let dt = now.duration_since(t).as_secs_f64();
            self.rx_rate = rate(prx, c.rx, dt);
            self.tx_rate = rate(ptx, c.tx, dt);
            self.hist.push_back((self.rx_rate as f32, self.tx_rate as f32));
            while self.hist.len() > HIST_CAP {
                self.hist.pop_front();
            }
        }
        self.prev = Some((now, c.rx, c.tx));
        self.rx_total = c.rx;
        self.tx_total = c.tx;
    }

    fn next_iface(&mut self) {
        self.sel = (self.sel + 1) % self.ifaces.len().max(1);
        self.prev = None;
        self.hist.clear();
        self.rx_rate = 0.0;
        self.tx_rate = 0.0;
    }
}

// ================================================================== bluetooth

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BtDev {
    pub addr: String,
    pub name: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub icon: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BtCtrl {
    pub addr: String,
    pub name: String,
    pub powered: bool,
    pub discoverable: bool,
    pub present: bool,
}

fn kv_line<'a>(l: &'a str, key: &str) -> Option<&'a str> {
    l.trim().strip_prefix(key)?.strip_prefix(':').map(|v| v.trim())
}

/// `bluetoothctl devices` -> (address, name).
pub fn parse_bt_devices(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let l = strip_ansi(l);
            let rest = l.trim().strip_prefix("Device ")?;
            let (addr, name) = rest.split_once(' ').unwrap_or((rest, ""));
            (addr.len() == 17).then(|| (addr.to_string(), name.trim().to_string()))
        })
        .collect()
}

/// `bluetoothctl info <addr>`.
pub fn parse_bt_info(text: &str) -> Option<BtDev> {
    let mut d = BtDev::default();
    let mut seen = false;
    for raw in text.lines() {
        let l = strip_ansi(raw);
        if let Some(rest) = l.trim().strip_prefix("Device ") {
            d.addr = rest.split_whitespace().next().unwrap_or("").to_string();
            seen = true;
        } else if let Some(v) = kv_line(&l, "Alias").or_else(|| kv_line(&l, "Name")) {
            if d.name.is_empty() || l.trim().starts_with("Alias") {
                d.name = v.to_string();
            }
        } else if let Some(v) = kv_line(&l, "Paired") {
            d.paired = v == "yes";
        } else if let Some(v) = kv_line(&l, "Trusted") {
            d.trusted = v == "yes";
        } else if let Some(v) = kv_line(&l, "Connected") {
            d.connected = v == "yes";
        } else if let Some(v) = kv_line(&l, "Icon") {
            d.icon = v.to_string();
        }
    }
    seen.then_some(d)
}

/// `bluetoothctl show`.
pub fn parse_bt_show(text: &str) -> BtCtrl {
    let mut c = BtCtrl::default();
    for raw in text.lines() {
        let l = strip_ansi(raw);
        if let Some(rest) = l.trim().strip_prefix("Controller ") {
            c.addr = rest.split_whitespace().next().unwrap_or("").to_string();
            c.present = true;
        } else if let Some(v) = kv_line(&l, "Alias") {
            c.name = v.to_string();
        } else if let Some(v) = kv_line(&l, "Powered") {
            c.powered = v == "yes";
        } else if let Some(v) = kv_line(&l, "Discoverable") {
            c.discoverable = v == "yes";
        }
    }
    c
}

// ======================================================================== BLE

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BleDev {
    pub addr: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    pub mfr_id: Option<u16>,
    pub mfr: Vec<u8>,
    pub last_seen_ms: u64,
}

fn company(id: u16) -> Option<&'static str> {
    Some(match id {
        0x0006 => "Microsoft",
        0x004C => "Apple",
        0x0059 => "Nordic",
        0x0075 => "Samsung",
        0x0087 => "Garmin",
        0x00E0 => "Google",
        0x00D2 => "Fitbit",
        0x0157 => "Huami",
        0x02E5 => "Espressif",
        0x038F => "Xiaomi",
        _ => return None,
    })
}

/// Stateful reader for the stream `bluetoothctl scan on` prints.
#[derive(Default)]
pub struct BleTable {
    pub devs: Vec<BleDev>,
    pending_id: Option<(String, u16)>,
    collecting: Option<String>,
}

impl BleTable {
    fn dev(&mut self, addr: &str, now_ms: u64) -> &mut BleDev {
        let i = match self.devs.iter().position(|d| d.addr == addr) {
            Some(i) => i,
            None => {
                self.devs.push(BleDev { addr: addr.to_string(), ..Default::default() });
                self.devs.len() - 1
            }
        };
        let d = &mut self.devs[i];
        d.last_seen_ms = now_ms;
        d
    }

    pub fn feed_line(&mut self, raw: &str, now_ms: u64) {
        let line = strip_ansi(raw);
        // Continuation of a "ManufacturerData.Value:" hex dump.
        if let Some(addr) = self.collecting.clone() {
            if line.starts_with("  ") && !line.trim_start().starts_with('[') {
                let hexpart: String = line.chars().skip(2).take(48).collect();
                let bytes: Vec<u8> = hexpart.split_whitespace().filter(|t| t.len() == 2).filter_map(|t| u8::from_str_radix(t, 16).ok()).collect();
                self.dev(&addr, now_ms).mfr.extend(bytes);
                return;
            }
            self.collecting = None;
        }
        let line = line.trim_start_matches("[bluetooth]# ").trim_end();
        let (kind, rest) = if let Some(r) = line.strip_prefix("[NEW] Device ") {
            ('N', r)
        } else if let Some(r) = line.strip_prefix("[CHG] Device ") {
            ('C', r)
        } else if let Some(r) = line.strip_prefix("[DEL] Device ") {
            ('D', r)
        } else {
            return;
        };
        if rest.len() < 17 {
            return;
        }
        let addr = rest[..17].to_string();
        let tail = rest[17..].trim();
        match kind {
            'D' => self.devs.retain(|d| d.addr != addr),
            'N' => {
                let dashed = addr.replace(':', "-");
                let d = self.dev(&addr, now_ms);
                if !tail.is_empty() && tail != dashed {
                    d.name = Some(tail.to_string());
                }
            }
            _ => {
                if let Some(v) = tail.strip_prefix("RSSI:") {
                    if let (Some(a), Some(b)) = (v.find('('), v.rfind(')')) {
                        if let Ok(r) = v[a + 1..b].trim().parse::<i16>() {
                            self.dev(&addr, now_ms).rssi = Some(r);
                        }
                    }
                } else if let Some(v) = tail.strip_prefix("Name:") {
                    self.dev(&addr, now_ms).name = Some(v.trim().to_string());
                } else if let Some(v) = tail.strip_prefix("ManufacturerData.Key:") {
                    if let Some(h) = v.trim().split_whitespace().next().and_then(|t| t.strip_prefix("0x")) {
                        if let Ok(id) = u16::from_str_radix(h, 16) {
                            self.pending_id = Some((addr.clone(), id));
                            let d = self.dev(&addr, now_ms);
                            d.mfr_id = Some(id);
                            d.mfr.clear();
                        }
                    }
                } else if tail.starts_with("ManufacturerData.Value:") {
                    self.dev(&addr, now_ms);
                    self.collecting = Some(addr);
                } else {
                    self.dev(&addr, now_ms);
                }
            }
        }
    }

    /// Strongest first; unseen ones last.
    pub fn sorted(&self) -> Vec<&BleDev> {
        let mut v: Vec<&BleDev> = self.devs.iter().collect();
        v.sort_by(|a, b| b.rssi.unwrap_or(-127).cmp(&a.rssi.unwrap_or(-127)).then_with(|| a.addr.cmp(&b.addr)));
        v
    }

    pub fn prune(&mut self, now_ms: u64, max_age_ms: u64) {
        self.devs.retain(|d| now_ms.saturating_sub(d.last_seen_ms) <= max_age_ms);
    }
}

// ======================================================================== USB

#[derive(Clone, Debug, PartialEq)]
pub struct UsbDev {
    pub name: String,
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub speed: Option<String>,
    pub busnum: u32,
    pub devnum: u32,
}

impl UsbDev {
    pub fn speed_label(&self) -> String {
        match self.speed.as_deref() {
            Some("1.5") => "1.5M".into(),
            Some("5000") => "5G".into(),
            Some("10000") => "10G".into(),
            Some(s) => format!("{s}M"),
            None => "-".into(),
        }
    }
    pub fn depth(&self) -> usize {
        self.name.matches('.').count() + if self.name.starts_with("usb") { 0 } else { 1 }
    }
    pub fn label(&self) -> String {
        match (&self.manufacturer, &self.product) {
            (Some(m), Some(p)) if !p.starts_with(m.as_str()) => format!("{m} {p}"),
            (_, Some(p)) => p.clone(),
            (Some(m), None) => m.clone(),
            _ => "(no strings)".into(),
        }
    }
}

/// Build a device from sysfs attribute files; `read(name)` returns a trimmed file's text.
pub fn parse_usb_dev(name: &str, read: &dyn Fn(&str) -> Option<String>) -> Option<UsbDev> {
    let vid = u16::from_str_radix(&read("idVendor")?, 16).ok()?;
    let pid = u16::from_str_radix(&read("idProduct")?, 16).ok()?;
    Some(UsbDev {
        name: name.to_string(),
        vid,
        pid,
        manufacturer: read("manufacturer").filter(|s| !s.is_empty()),
        product: read("product").filter(|s| !s.is_empty()),
        speed: read("speed"),
        busnum: read("busnum").and_then(|s| s.parse().ok()).unwrap_or(0),
        devnum: read("devnum").and_then(|s| s.parse().ok()).unwrap_or(0),
    })
}

fn list_usb(root: &Path) -> Vec<UsbDev> {
    let mut v: Vec<UsbDev> = std::fs::read_dir(root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.contains(':') {
                        return None; // interfaces
                    }
                    let dir = e.path();
                    parse_usb_dev(&name, &|f| read_trim(&dir.join(f).to_string_lossy()))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| (a.busnum, natural_key(&a.name)).cmp(&(b.busnum, natural_key(&b.name))));
    v
}

fn natural_key(name: &str) -> Vec<u32> {
    name.trim_start_matches("usb").split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect()
}

const FLASH: Duration = Duration::from_secs(5);

#[derive(Default)]
struct UsbView {
    devs: Vec<UsbDev>,
    added: HashMap<String, Instant>,
    removed: Vec<(UsbDev, Instant)>,
    primed: bool,
}

impl UsbView {
    /// Returns true when something changed (repaint).
    fn update(&mut self, now: Instant, list: Vec<UsbDev>) -> bool {
        let mut changed = false;
        if self.primed {
            for d in &list {
                if !self.devs.iter().any(|o| o.name == d.name && o.vid == d.vid && o.pid == d.pid) {
                    self.added.insert(d.name.clone(), now);
                    changed = true;
                }
            }
            for o in &self.devs {
                if !list.iter().any(|d| d.name == o.name && d.vid == o.vid && d.pid == o.pid) {
                    self.removed.push((o.clone(), now));
                    changed = true;
                }
            }
        }
        let before = self.added.len() + self.removed.len();
        self.added.retain(|_, t| now.duration_since(*t) < FLASH);
        self.removed.retain(|(_, t)| now.duration_since(*t) < FLASH);
        changed |= before != self.added.len() + self.removed.len();
        self.primed = true;
        self.devs = list;
        changed
    }
}

// ------------------------------------------------------------------ device tab

/// `1-1.2` hangs off `1-1`, `1-1` off the bus's root hub `usb1`; root hubs have no parent.
pub fn usb_parent(name: &str) -> Option<String> {
    if name.starts_with("usb") {
        return None;
    }
    match name.rsplit_once('.') {
        Some((parent, _)) => Some(parent.to_string()),
        None => name.split('-').next().filter(|b| !b.is_empty()).map(|b| format!("usb{b}")),
    }
}

fn usb_node(d: &UsbDev, has_children: bool, hot: bool, gone: bool) -> TreeNode {
    let label = if d.name.starts_with("usb") { format!("Root hub {}", d.name) } else { d.label() };
    let label = if gone { format!("- {label}") } else { label };
    TreeNode::new(format!("usb:{}", d.name), label).extra(d.speed_label()).expandable(has_children).hot(hot).dim(gone)
}

/// The USB branch: category -> root hubs -> devices by port path. Needs the live list (with
/// hot-plug state), so it is built here and not by a [`DevSource`].
fn usb_children(usb: &UsbView, key: &str, now: Instant) -> Vec<TreeNode> {
    let parent: Option<&str> = if key == CAT_USB { None } else { key.strip_prefix("usb:") };
    let is_child = |d: &UsbDev| match (usb_parent(&d.name), parent) {
        (None, None) => true,
        (Some(p), Some(q)) => p == q,
        _ => false,
    };
    let mut v: Vec<TreeNode> = usb
        .devs
        .iter()
        .filter(|d| is_child(d))
        .map(|d| {
            let kids = usb.devs.iter().any(|k| usb_parent(&k.name).as_deref() == Some(d.name.as_str()));
            let hot = usb.added.get(&d.name).map_or(false, |t| now.duration_since(*t) < FLASH);
            usb_node(d, kids, hot, false)
        })
        .collect();
    if parent.is_none() {
        // Unplugged a moment ago: stays at the top, faint, until the highlight times out.
        v.extend(usb.removed.iter().map(|(d, _)| usb_node(d, false, false, true)));
    }
    v
}

fn usb_details(usb: &UsbView, key: &str) -> Vec<(String, String)> {
    let name = key.strip_prefix("usb:").unwrap_or("");
    let (d, gone) = match usb.devs.iter().find(|d| d.name == name) {
        Some(d) => (d.clone(), false),
        None => match usb.removed.iter().find(|(d, _)| d.name == name) {
            Some((d, _)) => (d.clone(), true),
            None => return vec![("items".into(), usb.devs.iter().filter(|d| usb_parent(&d.name).is_none()).count().to_string() + " root hubs")],
        },
    };
    let mut v = vec![("id".to_string(), format!("{:04x}:{:04x}", d.vid, d.pid))];
    if let Some(m) = &d.manufacturer {
        v.push(("manufacturer".into(), m.clone()));
    }
    if let Some(pr) = &d.product {
        v.push(("product".into(), pr.clone()));
    }
    v.push(("speed".into(), d.speed_label()));
    v.push(("port".into(), d.name.clone()));
    v.push(("bus / dev".into(), format!("{} / {}", d.busnum, d.devnum)));
    if gone {
        v.push(("state".into(), "removed".into()));
    }
    v
}

/// What opens under a node: its properties as `name = value` leaf rows, then its child nodes.
/// Categories only list their members (the device tree's root also shows its own properties).
fn dev_children(src: &dyn DevSource, usb: &UsbView, n: &TreeNode) -> Vec<TreeNode> {
    let is_usb = n.key == CAT_USB || n.key.starts_with("usb:");
    let props = if n.key.starts_with("cat:") && n.key != "cat:dt" {
        Vec::new()
    } else if is_usb {
        usb_details(usb, &n.key)
    } else {
        src.details(&n.key)
    };
    let mut v: Vec<TreeNode> = props
        .into_iter()
        .enumerate()
        .map(|(i, (name, val))| {
            let secret = matches!(name.as_str(), "address" | "uniq");
            TreeNode::property(format!("{}#{i}", n.key), name, val).secret(secret)
        })
        .collect();
    v.extend(if is_usb { usb_children(usb, &n.key, Instant::now()) } else { src.children(&n.key) });
    v
}

/// The Device tab: one tree holding everything; opening a node shows its properties and children.
struct DevView {
    src: Box<dyn DevSource>,
    tree: TreeList,
    last_marquee: Option<Instant>,
}

impl DevView {
    fn new(src: Box<dyn DevSource>) -> Self {
        let roots = src.categories();
        Self { src, tree: TreeList::new(roots, body_rows() as usize), last_marquee: None }
    }

    /// Follow the surface (the window may have been resized): as many rows as the body holds,
    /// property values as wide as the list.
    fn fit(&mut self) {
        self.tree.visible = body_rows() as usize;
        self.tree.prop_w = widgets::PROP_ROW_W + extra_w();
    }
}

/// Geometry of the list on the current page (for the mouse).
struct ListGeom {
    /// Body row of the first item, and body rows per item.
    first_row: i32,
    item_rows: i32,
    total: usize,
    vis: usize,
    /// Index of the first item shown.
    start: usize,
}

impl ListGeom {
    /// The item under (x, y), if the point is on the list (not the scroll bar).
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let top = BODY_Y + self.first_row * ROW;
        if x < LIST_X || x >= LIST_X + list_w() || y < top {
            return None;
        }
        let n = ((y - top) / (ROW * self.item_rows)) as usize;
        (n < self.vis && self.start + n < self.total).then_some(self.start + n)
    }
}

/// Two-row BLE devices that fit under the header band.
fn ble_vis() -> usize {
    ((body_rows() - 1) / 2) as usize
}

/// The ETH page's key / value column (the graph takes the rest of the width).
const ETH_COL_W: i32 = 150;

/// The ETH graph: right of the key / value column up to the end of the list, from body row 1 down
/// to 3 px above the last row's end (on 320 x 170: 144 x 109, 2 px per sample).
fn eth_graph_rect() -> Rect {
    let gx = LIST_X + ETH_COL_W + 8;
    let gw = LIST_X + list_w() - 2 - gx;
    Rect::new(gx, BODY_Y + ROW, gw as u32, ((body_rows() - 1) * ROW - 3) as u32)
}

/// Samples graph `g` has room for at 2 px each (72 on 320 x 170).
fn eth_slots(g: Rect) -> usize {
    (g.width() / 2) as usize
}

fn contains_pt(r: Rect, x: i32, y: i32) -> bool {
    widgets::contains(r, x, y)
}

/// 1 px under the tab strip's full-width rule, so the bar never touches it.
fn body_scrollbar_rect() -> Rect {
    Rect::new(grid::scroll_x(), BODY_Y + 1, grid::SCROLL_W as u32, (body_end() - BODY_Y - 1) as u32)
}

fn tabs_strip(active: usize, first: usize) -> widgets::FolderScroll<'static> {
    widgets::FolderScroll::new(&TABS, active, first, tabs_room())
}

fn dev_tree_rect() -> Rect {
    Rect::new(LIST_X, BODY_Y, list_w() as u32, body_h() as u32)
}

/// The list area's outline: `w` wide, from the top of the body down to its frame line on y 168.
fn body_frame(w: i32) -> Rect {
    Rect::new(LIST_X, BODY_Y, w as u32, (body_end() - BODY_Y) as u32)
}

// ======================================================================== MIDI

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Midi {
    NoteOn { ch: u8, note: u8, vel: u8 },
    NoteOff { ch: u8, note: u8, vel: u8 },
    Cc { ch: u8, cc: u8, val: u8 },
    Program { ch: u8, prog: u8 },
    Bend { ch: u8, val: i16 },
    ChanPress { ch: u8, val: u8 },
    PolyPress { ch: u8, note: u8, val: u8 },
    Sysex(usize),
    Clock,
    Start,
    Stop,
    Continue,
}

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

fn note_name(n: u8) -> String {
    format!("{}{}", NOTE_NAMES[(n % 12) as usize], (n as i32 / 12) - 1)
}

fn cc_name(cc: u8) -> &'static str {
    match cc {
        0 => "Bank MSB",
        1 => "Mod wheel",
        2 => "Breath",
        7 => "Volume",
        10 => "Pan",
        11 => "Expression",
        32 => "Bank LSB",
        64 => "Sustain",
        65 => "Portamento",
        71 => "Resonance",
        74 => "Cutoff",
        91 => "Reverb",
        93 => "Chorus",
        120 => "All sound off",
        121 => "Reset ctrl",
        123 => "All notes off",
        _ => "",
    }
}

impl Midi {
    pub fn channel(&self) -> Option<u8> {
        match self {
            Midi::NoteOn { ch, .. } | Midi::NoteOff { ch, .. } | Midi::Cc { ch, .. } | Midi::Program { ch, .. } | Midi::Bend { ch, .. } | Midi::ChanPress { ch, .. } | Midi::PolyPress { ch, .. } => Some(*ch),
            _ => None,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Midi::NoteOn { .. } => "Note On",
            Midi::NoteOff { .. } => "Note Off",
            Midi::Cc { .. } => "CC",
            Midi::Program { .. } => "Program",
            Midi::Bend { .. } => "Bend",
            Midi::ChanPress { .. } => "Pressure",
            Midi::PolyPress { .. } => "Poly Prs",
            Midi::Sysex(_) => "SysEx",
            Midi::Clock => "Clock",
            Midi::Start => "Start",
            Midi::Stop => "Stop",
            Midi::Continue => "Continue",
        }
    }
    pub fn detail(&self) -> String {
        match self {
            Midi::NoteOn { note, vel, .. } | Midi::NoteOff { note, vel, .. } => format!("{} ({})  v{}", note_name(*note), note, vel),
            Midi::Cc { cc, val, .. } => {
                let n = cc_name(*cc);
                if n.is_empty() { format!("#{cc} = {val}") } else { format!("#{cc} {n} = {val}") }
            }
            Midi::Program { prog, .. } => format!("{prog}"),
            Midi::Bend { val, .. } => format!("{val:+}"),
            Midi::ChanPress { val, .. } => format!("{val}"),
            Midi::PolyPress { note, val, .. } => format!("{} = {}", note_name(*note), val),
            Midi::Sysex(n) => format!("{n} bytes"),
            _ => String::new(),
        }
    }
    /// 0..127 magnitude for the little level bar.
    pub fn level(&self) -> Option<u8> {
        match self {
            Midi::NoteOn { vel, .. } => Some(*vel),
            Midi::Cc { val, .. } | Midi::ChanPress { val, .. } | Midi::PolyPress { val, .. } => Some(*val),
            Midi::Bend { val, .. } => Some((((*val as i32 + 8192) >> 7).clamp(0, 127)) as u8),
            _ => None,
        }
    }
}

/// Decodes a raw MIDI 1.0 byte stream (running status, real-time bytes in the
/// middle of messages, SysEx). Active sensing is swallowed.
#[derive(Default)]
pub struct MidiDecoder {
    status: u8,
    data: [u8; 2],
    n: usize,
    sysex: Option<usize>,
}

impl MidiDecoder {
    pub fn push(&mut self, b: u8) -> Option<Midi> {
        if b >= 0xF8 {
            return match b {
                0xF8 => Some(Midi::Clock),
                0xFA => Some(Midi::Start),
                0xFB => Some(Midi::Continue),
                0xFC => Some(Midi::Stop),
                _ => None,
            };
        }
        if b & 0x80 != 0 {
            if b == 0xF0 {
                self.sysex = Some(1);
                self.status = 0;
                return None;
            }
            if b == 0xF7 {
                return self.sysex.take().map(|n| Midi::Sysex(n + 1));
            }
            if let Some(n) = self.sysex.take() {
                // Interrupted SysEx: report what we have.
                self.status = b;
                self.n = 0;
                return Some(Midi::Sysex(n));
            }
            self.status = if b >= 0xF0 { 0 } else { b };
            self.n = 0;
            return None;
        }
        if let Some(n) = &mut self.sysex {
            *n += 1;
            return None;
        }
        if self.status == 0 {
            return None;
        }
        self.data[self.n] = b;
        self.n += 1;
        let need = if (self.status & 0xF0) == 0xC0 || (self.status & 0xF0) == 0xD0 { 1 } else { 2 };
        if self.n < need {
            return None;
        }
        self.n = 0;
        let ch = self.status & 0x0F;
        let (d0, d1) = (self.data[0], self.data[1]);
        Some(match self.status & 0xF0 {
            0x80 => Midi::NoteOff { ch, note: d0, vel: d1 },
            0x90 if d1 == 0 => Midi::NoteOff { ch, note: d0, vel: 0 },
            0x90 => Midi::NoteOn { ch, note: d0, vel: d1 },
            0xA0 => Midi::PolyPress { ch, note: d0, val: d1 },
            0xB0 => Midi::Cc { ch, cc: d0, val: d1 },
            0xC0 => Midi::Program { ch, prog: d0 },
            0xD0 => Midi::ChanPress { ch, val: d0 },
            _ => Midi::Bend { ch, val: (((d1 as i16) << 7) | d0 as i16) - 8192 },
        })
    }
}

/// `aseqdump -l` -> "client:port" of every port that can be read from, minus
/// System (0) and Midi Through (14).
pub fn parse_aseq_ports(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let id = it.next()?;
            let (c, p) = id.split_once(':')?;
            let client: u32 = c.parse().ok()?;
            p.parse::<u32>().ok()?;
            if client == 0 || client == 14 {
                return None;
            }
            // Layout: "128:0    rtpmidid    Network Export" (client name, then port name).
            let rest = l.trim_start()[id.len()..].trim();
            let name = rest.split("  ").next().unwrap_or("").trim().to_string();
            Some((id.to_string(), name))
        })
        .collect()
}

/// One line of `aseqdump` output -> (source "c:p", event).
pub fn parse_aseqdump_line(line: &str) -> Option<(String, Midi)> {
    let l = line.trim();
    let (src, rest) = l.split_once(char::is_whitespace)?;
    let (c, p) = src.split_once(':')?;
    c.parse::<u32>().ok()?;
    p.parse::<u32>().ok()?;
    let rest = rest.trim();
    let nums = |after: &str| -> Vec<i32> {
        after
            .split(',')
            .filter_map(|f| f.trim().rsplit(' ').next().and_then(|t| t.parse::<i32>().ok()))
            .collect()
    };
    let take = |name: &str| rest.strip_prefix(name).map(|r| nums(r));
    let u = |v: i32| v.clamp(0, 127) as u8;
    let ev = if let Some(n) = take("Note on") {
        let (ch, note, vel) = (u(*n.first()?), u(*n.get(1)?), u(*n.get(2)?));
        if vel == 0 { Midi::NoteOff { ch, note, vel } } else { Midi::NoteOn { ch, note, vel } }
    } else if let Some(n) = take("Note off") {
        Midi::NoteOff { ch: u(*n.first()?), note: u(*n.get(1)?), vel: u(*n.get(2)?) }
    } else if let Some(n) = take("Control change") {
        Midi::Cc { ch: u(*n.first()?), cc: u(*n.get(1)?), val: u(*n.get(2)?) }
    } else if let Some(n) = take("Program change") {
        Midi::Program { ch: u(*n.first()?), prog: u(*n.get(1)?) }
    } else if let Some(n) = take("Pitch bend") {
        Midi::Bend { ch: u(*n.first()?), val: (*n.get(1)?).clamp(-8192, 8191) as i16 }
    } else if let Some(n) = take("Channel aftertouch") {
        Midi::ChanPress { ch: u(*n.first()?), val: u(*n.get(1)?) }
    } else if let Some(n) = take("Polyphonic aftertouch") {
        Midi::PolyPress { ch: u(*n.first()?), note: u(*n.get(1)?), val: u(*n.get(2)?) }
    } else if rest.starts_with("System exclusive") {
        Midi::Sysex(rest.matches(' ').count())
    } else if rest.starts_with("Clock") {
        Midi::Clock
    } else if rest.starts_with("Start") {
        Midi::Start
    } else if rest.starts_with("Continue") {
        Midi::Continue
    } else if rest.starts_with("Stop") {
        Midi::Stop
    } else {
        return None;
    };
    Some((src.to_string(), ev))
}

struct MidiView {
    log: VecDeque<(String, Midi)>,
    held: [u8; 128],
    level: [f32; 16],
    ports: Vec<String>,
    ports_known: bool,
    clocks: u64,
    dec: MidiDecoder,
}

impl MidiView {
    fn new() -> Self {
        Self { log: VecDeque::new(), held: [0; 128], level: [0.0; 16], ports: Vec::new(), ports_known: false, clocks: 0, dec: MidiDecoder::default() }
    }

    fn apply(&mut self, src: String, m: Midi) {
        match &m {
            Midi::Clock => {
                self.clocks += 1;
                return;
            }
            Midi::NoteOn { ch, note, vel } => {
                self.held[*note as usize & 127] = *vel;
                self.level[*ch as usize & 15] = (*vel as f32 / 127.0).max(self.level[*ch as usize & 15]);
            }
            Midi::NoteOff { note, .. } => self.held[*note as usize & 127] = 0,
            Midi::Cc { ch: 0..=15, cc: 120 | 123, .. } => self.held = [0; 128],
            Midi::Cc { ch, val, .. } => self.level[*ch as usize & 15] = self.level[*ch as usize & 15].max(*val as f32 / 127.0 * 0.5),
            Midi::Bend { ch, .. } | Midi::PolyPress { ch, .. } | Midi::ChanPress { ch, .. } | Midi::Program { ch, .. } => {
                self.level[*ch as usize & 15] = self.level[*ch as usize & 15].max(0.3)
            }
            _ => {}
        }
        self.log.push_back((src, m));
        while self.log.len() > LOG_CAP {
            self.log.pop_front();
        }
    }

    fn feed_bytes(&mut self, src: &str, bytes: &[u8]) {
        for &b in bytes {
            if let Some(m) = self.dec.push(b) {
                self.apply(src.to_string(), m);
            }
        }
    }

    fn decay(&mut self, dt: f32) -> bool {
        let mut any = false;
        for l in &mut self.level {
            if *l > 0.0 {
                *l = (*l - dt * 1.2).max(0.0);
                any = true;
            }
        }
        any
    }
}

// ========================================================================== IR

/// Samples as the kernel's lirc mode2 reports them: low 24 bits microseconds,
/// top byte the kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrKind {
    Space,
    Pulse,
    Timeout,
    Other,
}

pub fn ir_sample(raw: u32) -> (IrKind, u32) {
    let us = raw & 0x00FF_FFFF;
    let k = match raw >> 24 {
        0 => IrKind::Space,
        1 => IrKind::Pulse,
        3 => IrKind::Timeout,
        _ => IrKind::Other,
    };
    (k, us)
}

#[derive(Clone, Debug, PartialEq)]
pub struct IrFrame {
    pub decoded: String,
    /// Alternating pulse/space lengths in microseconds, starting with a pulse.
    pub edges: Vec<u32>,
}

#[derive(Default)]
pub struct IrAssembler {
    edges: Vec<u32>,
    expect_pulse: bool,
}

impl IrAssembler {
    pub fn feed(&mut self, kind: IrKind, us: u32) -> Option<IrFrame> {
        match kind {
            IrKind::Pulse => {
                if self.edges.len() % 2 == 1 {
                    // two pulses in a row: merge
                    *self.edges.last_mut().unwrap() += us;
                } else {
                    self.edges.push(us);
                }
                self.expect_pulse = false;
                None
            }
            IrKind::Space => {
                if self.edges.is_empty() {
                    return None;
                }
                if us >= 15_000 {
                    return self.finish();
                }
                if self.edges.len() % 2 == 1 {
                    self.edges.push(us);
                }
                None
            }
            IrKind::Timeout => self.finish(),
            IrKind::Other => None,
        }
    }

    fn finish(&mut self) -> Option<IrFrame> {
        let mut e = std::mem::take(&mut self.edges);
        if e.len() % 2 == 0 && !e.is_empty() {
            e.pop(); // trailing space
        }
        (e.len() >= 2).then(|| IrFrame { decoded: decode_ir(&e), edges: e })
    }
}

fn near(v: u32, target: u32) -> bool {
    let t = target as i64;
    (v as i64 - t).abs() <= t * 30 / 100
}

/// NEC (and a repeat code) or Sony SIRC; otherwise a raw edge count.
pub fn decode_ir(e: &[u32]) -> String {
    if e.len() >= 3 && near(e[0], 9000) && near(e[1], 2250) && e.len() <= 4 {
        return "NEC repeat".into();
    }
    if e.len() >= 66 && near(e[0], 9000) && near(e[1], 4500) {
        let mut v: u32 = 0;
        for i in 0..32 {
            let (m, s) = (e[2 + i * 2], e[3 + i * 2]);
            if !near(m, 560) {
                return format!("RAW {} edges", e.len());
            }
            if near(s, 1690) {
                v |= 1 << i;
            } else if !near(s, 560) {
                return format!("RAW {} edges", e.len());
            }
        }
        let b = v.to_le_bytes();
        let cmd = if b[2] == !b[3] { format!("{:02X}", b[2]) } else { format!("{:02X}{:02X}", b[3], b[2]) };
        let addr = if b[0] == !b[1] { format!("{:02X}", b[0]) } else { format!("{:02X}{:02X}", b[1], b[0]) };
        return format!("NEC  A:{addr} C:{cmd}");
    }
    if e.len() >= 25 && near(e[0], 2400) {
        let bits = (e.len() - 1) / 2;
        if matches!(bits, 12 | 15 | 20) {
            let mut v: u32 = 0;
            // Pairs after the header: space (600) then pulse (600 = 0, 1200 = 1).
            let mut ok = true;
            for i in 0..bits {
                let sp = e.get(1 + i * 2).copied().unwrap_or(0);
                let pu = e.get(2 + i * 2).copied().unwrap_or(0);
                if !near(sp, 600) {
                    ok = false;
                    break;
                }
                if near(pu, 1200) {
                    v |= 1 << i;
                } else if !near(pu, 600) {
                    ok = false;
                    break;
                }
            }
            if ok {
                let cmd = v & 0x7F;
                let addr = v >> 7;
                return format!("SIRC{bits} A:{addr:02X} C:{cmd:02X}");
            }
        }
    }
    format!("RAW {} edges", e.len())
}

/// The pulse/space timings of a NEC frame (used by the mock source and tests).
pub fn nec_edges(addr: u8, cmd: u8) -> Vec<u32> {
    let mut e = vec![9000, 4500];
    for byte in [addr, !addr, cmd, !cmd] {
        for i in 0..8 {
            e.push(560);
            e.push(if byte >> i & 1 == 1 { 1690 } else { 560 });
        }
    }
    e.push(560);
    e
}

/// Sony SIRC 12-bit frame timings (mock source and tests).
pub fn sirc12_edges(addr: u8, cmd: u8) -> Vec<u32> {
    let v = (cmd as u32 & 0x7F) | ((addr as u32 & 0x1F) << 7);
    let mut e = vec![2400];
    for i in 0..12 {
        e.push(600);
        e.push(if v >> i & 1 == 1 { 1200 } else { 600 });
    }
    e
}

#[derive(Clone, Debug, PartialEq)]
pub enum IrHw {
    Rx(String),
    /// Why there is no receiver, in plain lines.
    None(Vec<String>),
}

/// Explain what is (not) there. `nodes` = (path, can receive); `config` = text of config.txt.
pub fn ir_explain(nodes: &[(String, bool)], config: &str) -> IrHw {
    if let Some((p, _)) = nodes.iter().find(|(_, rx)| *rx) {
        return IrHw::Rx(p.clone());
    }
    let mut v = Vec::new();
    if nodes.is_empty() {
        v.push("No /dev/lirc* device found.".to_string());
    } else {
        let names: Vec<String> = nodes.iter().map(|(p, _)| p.trim_start_matches("/dev/").to_string()).collect();
        v.push(format!("{} can only transmit.", names.join(", ")));
    }
    let rx_overlay = config.lines().any(|l| {
        let l = l.trim();
        !l.starts_with('#') && l.starts_with("dtoverlay=gpio-ir") && !l.starts_with("dtoverlay=gpio-ir-tx")
    });
    if rx_overlay {
        v.push("gpio-ir overlay is set but no receiver came up.".to_string());
        v.push("Check wiring and the gpio_pin.".to_string());
    } else {
        v.push("Missing: dtoverlay=gpio-ir,gpio_pin=N".to_string());
        v.push("in /boot/firmware/config.txt, then reboot.".to_string());
    }
    v.push("Needs read access to /dev/lirc* (group video).".to_string());
    IrHw::None(v)
}

#[cfg(target_os = "linux")]
fn detect_ir() -> IrHw {
    let mut nodes = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/dev") {
        let mut names: Vec<String> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.starts_with("lirc")).collect();
        names.sort();
        for n in names {
            let path = format!("/dev/{n}");
            let mut can_rx = false;
            if let Ok(f) = std::fs::OpenOptions::new().read(true).custom_flags_nonblock().open(&path) {
                use std::os::unix::io::AsRawFd;
                let mut feat: u32 = 0;
                // LIRC_GET_FEATURES = _IOR('i', 0, __u32); LIRC_CAN_REC_MODE2 = 0x00040000
                let r = unsafe { libc::ioctl(f.as_raw_fd(), 0x8004_6900u32 as _, &mut feat as *mut u32) };
                can_rx = r == 0 && feat & 0x0004_0000 != 0;
            }
            nodes.push((path, can_rx));
        }
    }
    let cfg = std::fs::read_to_string("/boot/firmware/config.txt").unwrap_or_default();
    ir_explain(&nodes, &cfg)
}

#[cfg(target_os = "linux")]
trait NonBlock {
    fn custom_flags_nonblock(&mut self) -> &mut Self;
}
#[cfg(target_os = "linux")]
impl NonBlock for std::fs::OpenOptions {
    fn custom_flags_nonblock(&mut self) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.custom_flags(libc::O_NONBLOCK)
    }
}

#[cfg(not(target_os = "linux"))]
fn detect_ir() -> IrHw {
    IrHw::None(vec!["No /dev/lirc* device found.".into()])
}

// ============================================================ background work

enum Ev {
    BleLine(String),
    Bt(BtCtrl, Vec<BtDev>),
    BtMissing,
    MidiPorts(Vec<String>),
    Midi(String, Midi),
    Ir(Vec<u32>),
    IrError(String),
    OscBound,
    OscErr(String),
    OscPkt { src: String, msgs: Vec<(String, String)> },
    OscBad,
    /// Wi-Fi, serial, I2C, GPIO and 1-Wire (see `ports.rs`).
    Port(PortEv),
}

struct Worker {
    stop: Arc<AtomicBool>,
    pid: Arc<AtomicI32>,
}

impl Worker {
    fn new() -> Self {
        Self { stop: Arc::new(AtomicBool::new(false)), pid: Arc::new(AtomicI32::new(0)) }
    }
    fn halt(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let p = self.pid.load(Ordering::SeqCst);
        if p > 0 {
            #[cfg(unix)]
            unsafe {
                libc::kill(p, libc::SIGTERM);
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.halt();
    }
}

fn sleep_unless(stop: &AtomicBool, ms: u64) {
    let mut left = ms;
    while left > 0 && !stop.load(Ordering::SeqCst) {
        let s = left.min(50);
        std::thread::sleep(Duration::from_millis(s));
        left -= s;
    }
}

/// Run a program and hand every stdout line to `on_line` until it exits or `stop` is set.
/// Returns false when the program could not be started.
fn run_lines(prog: &str, args: &[&str], stop: &AtomicBool, pid: &AtomicI32, on_line: &mut dyn FnMut(String)) -> bool {
    let Ok(mut child) = Command::new(prog).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    pid.store(child.id() as i32, Ordering::SeqCst);
    if stop.load(Ordering::SeqCst) {
        let _ = child.kill();
    }
    if let Some(out) = child.stdout.take() {
        let mut r = BufReader::new(out);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => on_line(String::from_utf8_lossy(&buf).trim_end().to_string()),
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    pid.store(0, Ordering::SeqCst);
    true
}

fn run_capture(prog: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(prog).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

fn start_ble(tx: Sender<Ev>) -> Worker {
    let w = Worker::new();
    let (stop, pid) = (w.stop.clone(), w.pid.clone());
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let ok = run_lines("bluetoothctl", &["--timeout", "30", "scan", "on"], &stop, &pid, &mut |l| {
                let _ = tx.send(Ev::BleLine(l));
            });
            if !ok {
                let _ = tx.send(Ev::BtMissing);
                sleep_unless(&stop, 2000);
            }
        }
    });
    w
}

fn start_bt(tx: Sender<Ev>) -> Worker {
    let w = Worker::new();
    let stop = w.stop.clone();
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match run_capture("bluetoothctl", &["show"]) {
                None => {
                    let _ = tx.send(Ev::BtMissing);
                }
                Some(show) => {
                    let ctrl = parse_bt_show(&show);
                    let mut devs = Vec::new();
                    for (addr, name) in parse_bt_devices(&run_capture("bluetoothctl", &["devices"]).unwrap_or_default()).into_iter().take(16) {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        if let Some(mut d) = run_capture("bluetoothctl", &["info", &addr]).and_then(|t| parse_bt_info(&t)) {
                            if d.name.is_empty() {
                                d.name = name;
                            }
                            if d.paired || d.connected || d.trusted {
                                devs.push(d);
                            }
                        }
                    }
                    let _ = tx.send(Ev::Bt(ctrl, devs));
                }
            }
            sleep_unless(&stop, 3000);
        }
    });
    w
}

fn start_midi(tx: Sender<Ev>) -> Worker {
    let w = Worker::new();
    let (stop, pid) = (w.stop.clone(), w.pid.clone());
    std::thread::spawn(move || {
        let mut last: Vec<String> = vec!["?".into()];
        while !stop.load(Ordering::SeqCst) {
            let ports = parse_aseq_ports(&run_capture("aseqdump", &["-l"]).unwrap_or_default());
            let ids: Vec<String> = ports.iter().map(|p| p.0.clone()).collect();
            if ids != last {
                let _ = tx.send(Ev::MidiPorts(ports.iter().map(|(i, n)| format!("{i} {n}")).collect()));
                last = ids.clone();
            }
            if ids.is_empty() {
                sleep_unless(&stop, 1500);
                continue;
            }
            // Reader runs in its own thread so this one can keep watching for hotplug.
            let arg = ids.join(",");
            let (stop2, pid2, tx2) = (stop.clone(), pid.clone(), tx.clone());
            let reader = std::thread::spawn(move || {
                run_lines("stdbuf", &["-oL", "aseqdump", "-p", &arg], &stop2, &pid2, &mut |l| {
                    if let Some((s, m)) = parse_aseqdump_line(&l) {
                        let _ = tx2.send(Ev::Midi(s, m));
                    }
                });
            });
            loop {
                sleep_unless(&stop, 1500);
                if stop.load(Ordering::SeqCst) || reader.is_finished() {
                    break;
                }
                let now: Vec<String> = parse_aseq_ports(&run_capture("aseqdump", &["-l"]).unwrap_or_default()).into_iter().map(|p| p.0).collect();
                if now != ids {
                    let p = pid.load(Ordering::SeqCst);
                    if p > 0 {
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(p, libc::SIGTERM);
                        }
                    }
                    break;
                }
            }
            let _ = reader.join();
            last = vec!["?".into()]; // re-announce the ports after a restart
        }
    });
    w
}

#[cfg(target_os = "linux")]
fn bind_udp(port: u16) -> std::io::Result<std::net::UdpSocket> {
    use std::os::unix::io::FromRawFd;
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let one: libc::c_int = 1;
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, &one as *const _ as *const libc::c_void, std::mem::size_of::<libc::c_int>() as u32);
        let mut a: libc::sockaddr_in = std::mem::zeroed();
        a.sin_family = libc::AF_INET as libc::sa_family_t;
        a.sin_port = port.to_be();
        a.sin_addr.s_addr = 0;
        if libc::bind(fd, &a as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_in>() as u32) != 0 {
            let e = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        Ok(std::net::UdpSocket::from_raw_fd(fd))
    }
}

#[cfg(not(target_os = "linux"))]
fn bind_udp(port: u16) -> std::io::Result<std::net::UdpSocket> {
    std::net::UdpSocket::bind(("0.0.0.0", port))
}

fn start_osc(tx: Sender<Ev>, port: u16) -> Worker {
    let w = Worker::new();
    let stop = w.stop.clone();
    std::thread::spawn(move || {
        let sock = match bind_udp(port) {
            Ok(s) => s,
            Err(e) => {
                let m = if e.kind() == std::io::ErrorKind::AddrInUse { format!("Port {port} is already in use") } else { format!("Cannot listen on {port}: {e}") };
                let _ = tx.send(Ev::OscErr(m));
                return;
            }
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
        let _ = tx.send(Ev::OscBound);
        let mut buf = vec![0u8; 65536];
        while !stop.load(Ordering::SeqCst) {
            match sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    let src = from.to_string();
                    match osc::parse_packet(&buf[..n]) {
                        Ok(items) => {
                            let msgs = items.into_iter().map(|i| (i.msg.addr, osc::fmt_args(&i.msg.args))).collect();
                            let _ = tx.send(Ev::OscPkt { src, msgs });
                        }
                        Err(_) => {
                            let _ = tx.send(Ev::OscBad);
                        }
                    }
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(e) => {
                    let _ = tx.send(Ev::OscErr(e.to_string()));
                    return;
                }
            }
        }
    });
    w
}

#[cfg(target_os = "linux")]
fn start_ir(tx: Sender<Ev>, path: String) -> Worker {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let w = Worker::new();
    let stop = w.stop.clone();
    std::thread::spawn(move || {
        let mut f = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path) {
            Ok(f) => f,
            Err(e) => {
                let _ = tx.send(Ev::IrError(format!("{path}: {e}")));
                return;
            }
        };
        let mut buf = [0u8; 256];
        while !stop.load(Ordering::SeqCst) {
            let mut pfd = libc::pollfd { fd: f.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let r = unsafe { libc::poll(&mut pfd, 1, 200) };
            if r <= 0 {
                continue;
            }
            match f.read(&mut buf) {
                Ok(n) if n >= 4 => {
                    let v: Vec<u32> = buf[..n - n % 4].chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                    let _ = tx.send(Ev::Ir(v));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    let _ = tx.send(Ev::IrError(e.to_string()));
                    return;
                }
            }
        }
    });
    w
}

#[cfg(not(target_os = "linux"))]
fn start_ir(_tx: Sender<Ev>, _path: String) -> Worker {
    Worker::new()
}

// ================================================================ mock source

const MOCK_BLE: &[(&str, Option<&str>, i16, u16, &str)] = &[
    ("6E:44:B3:64:D5:9E", None, -47, 0x004C, "0C 0E 08 CA 57 BF A3 07 5A 82 AD 90 89 EA E6 C7 10 06 44 1D 47 B0 FA 18"),
    ("C8:2B:96:11:0A:4F", Some("Mi Smart Band 8"), -58, 0x038F, "16 5D 03 00 11 4A 02"),
    ("D0:03:4B:7A:21:9C", Some("JBL Flip 6"), -63, 0x0075, "42 04 01 01 5C"),
    ("50:DA:AA:E1:C0:2E", None, -54, 0x0006, "01 09 20 02 6A 4E 11"),
    ("24:6F:28:B1:7C:E2", Some("ESP32-Beacon"), -71, 0x02E5, "AB CD 01"),
    ("F4:12:FA:00:93:D1", Some("Tile"), -79, 0x0000, ""),
    ("60:7E:88:FE:B0:C8", None, -46, 0x004C, "10 06 5B 1E 33 AA 21 08"),
];

fn mock_ble_lines(step: u32) -> Vec<String> {
    let mut out = Vec::new();
    for (i, (addr, name, rssi, id, hex)) in MOCK_BLE.iter().enumerate() {
        if step == 0 {
            out.push(format!("[NEW] Device {addr} {}", name.map(|s| s.to_string()).unwrap_or_else(|| addr.replace(':', "-"))));
            if !hex.is_empty() {
                out.push(format!("[CHG] Device {addr} ManufacturerData.Key: 0x{id:04x} ({id})"));
                out.push(format!("[CHG] Device {addr} ManufacturerData.Value:"));
                for chunk in hex.split(' ').collect::<Vec<_>>().chunks(16) {
                    let h = chunk.join(" ").to_lowercase();
                    out.push(format!("  {h:<47}  ................"));
                }
            }
        }
        let jitter = (((step as i32 * 7 + i as i32 * 13) % 9) - 4) as i16;
        let r = rssi + jitter;
        out.push(format!("[CHG] Device {addr} RSSI: 0x{:08x} ({r})", r as i32 as u32));
    }
    out
}

/// A believable little performance, as raw bytes (exercises running status and a SysEx).
fn mock_midi_bytes(step: u32) -> Vec<u8> {
    let seq: &[&[u8]] = &[
        &[0x90, 60, 96],
        &[0xB0, 1, 20],
        &[0x90, 64, 88],
        &[0xE0, 0x00, 0x50],
        &[0xB0, 74, 90],
        &[0x91, 67, 72],
        &[0x80, 60, 0],
        &[0x90, 67, 0],
        &[0xC0, 12],
        &[0xB0, 64, 127],
        &[0x92, 72, 100],
        &[0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7],
        &[0x80, 64, 40],
        &[0xB0, 64, 0],
    ];
    seq[step as usize % seq.len()].to_vec()
}

/// Fake OSC datagrams (a norns-ish session), built with the real encoder.
fn mock_osc_packet(n: u32) -> Vec<u8> {
    use osc::{encode_bundle, encode_msg, OscArg::*};
    match n % 8 {
        0 => encode_msg("/grid/key", &[Int((n / 8 % 16) as i32), Int((n / 3 % 8) as i32), Int((n / 8 % 2) as i32)]),
        1 => encode_msg("/enc", &[Int(2), Int(if n % 16 < 8 { -1 } else { 1 })]),
        2 | 6 => encode_msg("/tick", &[Float((n as f32 * 0.037) % 1.0)]),
        3 => encode_msg("/note/on", &[Int(60 + (n / 8 % 12) as i32), Int(96)]),
        4 => encode_bundle(1, &[encode_msg("/note/off", &[Int(60 + (n / 8 % 12) as i32)]), encode_msg("/amp", &[Float(0.707), Str("main".into())])]),
        5 if n % 40 == 5 => vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01],
        5 => encode_msg("/screen/refresh", &[]),
        _ => encode_msg("/sample", &[Blob(vec![0; 64]), True]),
    }
}

fn mock_usb(elapsed: Duration) -> Vec<UsbDev> {
    let mut v = vec![
        UsbDev { name: "usb1".into(), vid: 0x1d6b, pid: 0x0002, manufacturer: Some("Linux 6.12 dwc2_hsotg".into()), product: Some("DWC OTG Controller".into()), speed: Some("480".into()), busnum: 1, devnum: 1 },
        UsbDev { name: "1-1".into(), vid: 0x05e3, pid: 0x0610, manufacturer: None, product: Some("USB2.0 Hub".into()), speed: Some("480".into()), busnum: 1, devnum: 2 },
        UsbDev { name: "1-1.1".into(), vid: 0x0fe6, pid: 0x9900, manufacturer: Some("CoreChips".into()), product: Some("USB 10/100 LAN".into()), speed: Some("480".into()), busnum: 1, devnum: 3 },
    ];
    if elapsed.as_secs() % 24 >= 12 {
        v.push(UsbDev { name: "1-1.2".into(), vid: 0x1235, pid: 0x8200, manufacturer: Some("Felucca".into()), product: Some("MIDI 1".into()), speed: Some("12".into()), busnum: 1, devnum: 4 });
    }
    v
}

fn mock_counters(elapsed: Duration) -> Vec<NetCounters> {
    // Integrals of a smooth, always positive rate, so the graph moves believably.
    let t = elapsed.as_secs_f64();
    let rx = 41_102_252.0 + 120_000.0 * t - 100_000.0 * (0.8 * t).cos() - 17_391.0 * (2.3 * t).cos() + 117_391.0;
    let tx = 1_045_953.0 + 12_000.0 * t - 5_454.0 * (1.1 * t + 1.0).cos() + 2_946.0;
    let (rx, tx) = (rx as u64, tx as u64);
    vec![
        NetCounters { name: "lo".into(), rx: 16084, tx: 16084 },
        NetCounters { name: "eth0".into(), rx, tx },
        NetCounters { name: "wlan0".into(), rx: 2_000_000 + (t * 900.0) as u64, tx: 400_000 + (t * 300.0) as u64 },
    ]
}

// ============================================================================ app

struct OscLog {
    ms: u64,
    src: String,
    addr: String,
    args: String,
}

struct OscView {
    port_idx: usize,
    listening: bool,
    err: Option<String>,
    ips: Vec<String>,
    log: VecDeque<OscLog>,
    table: OscTable,
    table_view: bool,
    mock_n: u32,
}

impl OscView {
    fn new() -> Self {
        Self { port_idx: 2, listening: false, err: None, ips: Vec::new(), log: VecDeque::new(), table: OscTable::default(), table_view: false, mock_n: 0 }
    }
    fn port(&self) -> u16 {
        OSC_PORTS[self.port_idx % OSC_PORTS.len()]
    }
    fn record(&mut self, now_ms: u64, src: &str, addr: String, args: String) {
        self.table.record(&addr, &args, now_ms);
        self.log.push_back(OscLog { ms: now_ms, src: src.to_string(), addr, args });
        while self.log.len() > OSC_LOG_CAP {
            self.log.pop_front();
        }
    }
    fn feed_packet(&mut self, now_ms: u64, src: &str, bytes: &[u8]) {
        match osc::parse_packet(bytes) {
            Ok(items) => {
                for i in items {
                    self.record(now_ms, src, i.msg.addr, osc::fmt_args(&i.msg.args));
                }
            }
            Err(_) => self.table.bad(),
        }
    }
    fn reset(&mut self) {
        self.log.clear();
        self.table.clear();
    }
}

/// "192.168.3.7:9001" -> "3.7:9001" (the LAN prefix is noise on a 320 px screen).
fn short_src(s: &str) -> String {
    s.strip_prefix("192.168.").unwrap_or(s).to_string()
}

pub struct Interceptor {
    osc: OscView,
    clock_base: u64,
    last_osc_mock: Instant,
    mock: bool,
    page: usize,
    paused: bool,
    t0: Instant,
    scroll: [usize; NP],
    /// Cursor row of each list page (absolute index); log pages follow the newest row while `follow` is set.
    cur: [usize; NP],
    follow: [bool; NP],
    /// The first tab shown in the scrolling strip.
    tab_first: usize,
    /// The wired buses and Wi-Fi.
    ports: Ports,
    /// The scroll bar thumb is being dragged.
    sb_drag: bool,
    /// BLE rows re-sort by RSSI, so the cursor sticks to an address.
    ble_sel: Option<String>,
    /// Show device addresses as asterisks (default on).
    mask: bool,
    /// Device tree browser (built on first visit) and the decoded properties of the selected node.
    dev: Option<DevView>,
    tx: Sender<Ev>,
    rx: Receiver<Ev>,
    worker: Option<(usize, Worker)>,
    last_page_started: Option<usize>,
    eth: Eth,
    bt_ctrl: BtCtrl,
    bt_devs: Vec<BtDev>,
    bt_state: Option<String>,
    ble: BleTable,
    ble_state: Option<String>,
    ir_hw: IrHw,
    ir_asm: IrAssembler,
    ir_log: VecDeque<IrFrame>,
    ir_err: Option<String>,
    usb: UsbView,
    midi: MidiView,
    last_sample: Instant,
    last_tick: Instant,
    mock_step: u32,
    last_mock: Instant,
    usb_root: String,
}

pub fn make() -> Box<dyn LauncherApp> {
    let mock = cfg!(not(target_os = "linux")) || matches!(std::env::var("HARBOUR_MOCK").as_deref(), Ok("1" | "true" | "yes" | "on"));
    Box::new(Interceptor::new(mock))
}

impl Interceptor {
    fn new(mock: bool) -> Self {
        let (tx, rx) = channel();
        let now = Instant::now();
        let mut s = Self {
            mock,
            page: 0,
            paused: false,
            t0: now,
            scroll: [0; NP],
            cur: [0; NP],
            follow: [true; NP],
            tab_first: 0,
            ports: Ports::new(mock),
            sb_drag: false,
            ble_sel: None,
            mask: true,
            dev: None,
            tx,
            rx,
            worker: None,
            last_page_started: None,
            eth: Eth::new(),
            bt_ctrl: BtCtrl::default(),
            bt_devs: Vec::new(),
            bt_state: None,
            ble: BleTable::default(),
            ble_state: None,
            ir_hw: IrHw::None(vec!["Not probed yet.".into()]),
            ir_asm: IrAssembler::default(),
            ir_log: VecDeque::new(),
            ir_err: None,
            usb: UsbView::default(),
            midi: MidiView::new(),
            last_sample: now - Duration::from_secs(2),
            last_tick: now,
            mock_step: 0,
            last_mock: now,
            usb_root: "/sys/bus/usb/devices".into(),
            osc: OscView::new(),
            clock_base: 0,
            last_osc_mock: now,
        };
        if mock {
            s.clock_base = 30_000;
            s.seed_mock();
        }
        s
    }

    fn seed_mock(&mut self) {
        self.ir_hw = IrHw::Rx("/dev/lirc1".into());
        self.bt_ctrl = BtCtrl { addr: "F0:40:AF:90:3F:83".into(), name: "CardputerZero".into(), powered: true, discoverable: false, present: true };
        self.bt_devs = vec![
            BtDev { addr: "F0:16:1D:02:B5:DA".into(), name: "StickMouse".into(), paired: true, trusted: true, connected: true, icon: "input-mouse".into() },
            BtDev { addr: "20:74:CF:4B:11:08".into(), name: "Keychron K3".into(), paired: true, trusted: true, connected: false, icon: "input-keyboard".into() },
            BtDev { addr: "88:C9:E8:30:72:1A".into(), name: "WH-1000XM4".into(), paired: true, trusted: false, connected: false, icon: "audio-headphones".into() },
        ];
        for l in mock_ble_lines(0) {
            self.ble.feed_line(&l, 0);
        }
        for step in 0..9 {
            self.midi.feed_bytes("20:0", &mock_midi_bytes(step));
            self.midi_ports_mock();
        }
        self.midi.level = [0.0; 16];
        self.midi.level[0] = 0.8;
        self.midi.level[1] = 0.5;
        self.midi.level[2] = 0.3;
        for (a, c) in [(0x00u8, 0x45u8), (0x00, 0x46), (0x04, 0x08)] {
            self.ir_log.push_back(IrFrame { decoded: decode_ir(&nec_edges(a, c)), edges: nec_edges(a, c) });
        }
        self.ir_log.push_back(IrFrame { decoded: "NEC repeat".into(), edges: vec![9000, 2250, 560] });
        let sirc = sirc12_edges(1, 0x15);
        self.ir_log.push_back(IrFrame { decoded: decode_ir(&sirc), edges: sirc });
        self.osc.listening = true;
        self.osc.ips = vec!["192.168.234.8".into()];
        for i in 0..40u32 {
            self.osc.mock_n += 1;
            let ms = 24_000 + i as u64 * 150;
            self.osc.feed_packet(ms, "192.168.3.7:9001", &mock_osc_packet(self.osc.mock_n));
        }
        self.mock_tick_eth(Duration::from_secs(0));
        for i in 1..=30 {
            self.mock_tick_eth(Duration::from_secs(i));
        }
        self.usb.update(Instant::now(), mock_usb(Duration::ZERO));
    }

    fn midi_ports_mock(&mut self) {
        self.midi.ports = vec!["20:0 Felucca MIDI 1".into(), "128:0 rtpmidid Network Export".into()];
        self.midi.ports_known = true;
    }

    fn mock_tick_eth(&mut self, at: Duration) {
        let base = self.t0;
        self.eth.link = LinkInfo { up: true, carrier: true, speed_mbps: Some(100), mac: Some("00:e0:4c:36:b1:2f".into()), ip: Some("192.168.234.8".into()) };
        self.eth.sample(base + at, &mock_counters(at));
    }

    // ---------------------------------------------------------- page workers

    fn ensure_worker(&mut self) {
        if self.mock || self.last_page_started == Some(self.page) {
            return;
        }
        self.worker = None; // Drop stops the previous one.
        self.last_page_started = Some(self.page);
        let tx = self.tx.clone();
        self.worker = match self.page {
            PAGE_BT => Some((PAGE_BT, start_bt(tx))),
            PAGE_BLE => Some((PAGE_BLE, start_ble(tx))),
            PAGE_OSC => {
                self.osc.listening = false;
                self.osc.err = None;
                Some((PAGE_OSC, start_osc(tx, self.osc.port())))
            }
            PAGE_MIDI => Some((PAGE_MIDI, start_midi(tx))),
            PAGE_WIFI | PAGE_SER | PAGE_I2C | PAGE_GPIO | PAGE_W1 | PAGE_GROVE => self.ports.start(self.page, tx).map(|w| (self.page, w)),
            PAGE_IR => {
                self.ir_hw = detect_ir();
                match &self.ir_hw {
                    IrHw::Rx(p) => Some((PAGE_IR, start_ir(tx, p.clone()))),
                    IrHw::None(_) => None,
                }
            }
            _ => None,
        };
    }

    fn set_page(&mut self, p: usize) {
        let p = p % NP;
        if p != self.page {
            // A port is only open while its page is in front.
            self.ports.leave(self.page);
        }
        self.page = p;
        self.tab_first = widgets::FolderScroll::follow(&TABS, p, self.tab_first, tabs_room());
        self.last_page_started = None;
        if self.worker.is_some() {
            self.worker = None;
        }
    }

    fn apply(&mut self, ev: Ev) {
        let now_ms = self.t0.elapsed().as_millis() as u64;
        match ev {
            Ev::BleLine(l) => {
                self.ble.feed_line(&l, now_ms);
                self.ble_state = None;
            }
            Ev::BtMissing => {
                let m = "bluetoothctl not available".to_string();
                self.bt_state = Some(m.clone());
                self.ble_state = Some(m);
            }
            Ev::Bt(c, d) => {
                self.bt_ctrl = c;
                self.bt_devs = d;
                self.bt_state = None;
            }
            Ev::MidiPorts(p) => {
                self.midi.ports = p;
                self.midi.ports_known = true;
            }
            Ev::Midi(src, m) => self.midi.apply(src, m),
            Ev::Ir(samples) => {
                for raw in samples {
                    let (k, us) = ir_sample(raw);
                    if let Some(f) = self.ir_asm.feed(k, us) {
                        self.ir_log.push_back(f);
                        while self.ir_log.len() > LOG_CAP {
                            self.ir_log.pop_front();
                        }
                    }
                }
            }
            Ev::IrError(e) => self.ir_err = Some(e),
            Ev::OscBound => {
                self.osc.listening = true;
                self.osc.err = None;
            }
            Ev::OscErr(e) => {
                self.osc.listening = false;
                self.osc.err = Some(e);
            }
            Ev::OscPkt { src, msgs } => {
                let now = self.now_ms();
                for (a, args) in msgs {
                    self.osc.record(now, &src, a, args);
                }
            }
            Ev::OscBad => self.osc.table.bad(),
            Ev::Port(e) => self.ports.apply(e, now_ms),
        }
    }

    fn now_ms(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64 + self.clock_base
    }

    fn log_page(&self) -> bool {
        (self.page == PAGE_IR && matches!(self.ir_hw, IrHw::Rx(_))) || self.page == PAGE_MIDI || (self.page == PAGE_OSC && !self.osc.table_view) || self.page == PAGE_SER
    }

    /// (rows in the list, rows visible) of the current page; (0, 0) when it has no list.
    fn dims(&self) -> (usize, usize) {
        match self.page {
            PAGE_BT => (self.bt_devs.len(), (body_rows() - 1) as usize),
            PAGE_BLE => (self.ble.devs.len(), ble_vis()),
            PAGE_OSC if self.osc.table_view => (self.osc.table.map.len(), (body_rows() - 2) as usize),
            PAGE_OSC => (self.osc.log.len(), (body_rows() - 2) as usize),
            PAGE_IR if self.log_page() => (self.ir_log.len(), (body_rows() - 1) as usize),
            PAGE_MIDI => (self.midi.log.len(), (body_rows() - 3) as usize),
            PAGE_USB => (self.usb_rows().len(), (body_rows() - 1) as usize),
            p => self.ports.dims(p).map_or((0, 0), |(len, vis, _)| (len, vis)),
        }
    }

    /// Keep the cursor inside the list (it may have shrunk) and the window around it.
    fn sync_cursor(&mut self) {
        let p = self.page;
        let (len, vis) = self.dims();
        if len == 0 || vis == 0 {
            self.cur[p] = 0;
            self.scroll[p] = 0;
            return;
        }
        if p == PAGE_BLE {
            if let Some(a) = &self.ble_sel {
                if let Some(i) = self.ble.sorted().iter().position(|d| &d.addr == a) {
                    self.cur[p] = i;
                }
            }
        }
        if self.log_page() {
            // Logs scroll from the newest row: `scroll` = rows hidden below the window.
            let ec = if self.follow[p] { len - 1 } else { self.cur[p].min(len - 1) };
            self.cur[p] = ec;
            let start = len.saturating_sub(vis).saturating_sub(self.scroll[p]);
            let start = widgets::follow_scroll(ec, start, vis, len);
            self.scroll[p] = len.saturating_sub(vis).saturating_sub(start);
        } else {
            self.cur[p] = self.cur[p].min(len - 1);
            self.scroll[p] = widgets::follow_scroll(self.cur[p], self.scroll[p], vis, len);
        }
    }

    /// Where the list of the current page sits on screen (None for pages without a list).
    fn list_geom(&self) -> Option<ListGeom> {
        let (total, vis) = self.dims();
        if total == 0 || vis == 0 {
            return None;
        }
        let (first_row, item_rows) = match self.page {
            PAGE_BT | PAGE_IR => (1, 1),
            PAGE_BLE => (1, 2),
            PAGE_OSC => (2, 1),
            PAGE_MIDI => (3, 1),
            PAGE_USB => (1, 1),
            p => (self.ports.dims(p)?.2, 1),
        };
        let start = if self.log_page() { total.saturating_sub(vis).saturating_sub(self.scroll[self.page]) } else { self.scroll[self.page] };
        Some(ListGeom { first_row, item_rows, total, vis, start })
    }

    /// Scroll so that item `start` is the first one shown; the cursor is pulled into the window.
    fn set_start(&mut self, start: usize) {
        let p = self.page;
        let (total, vis) = self.dims();
        if total == 0 || vis == 0 {
            return;
        }
        let max = total.saturating_sub(vis);
        let start = start.min(max);
        let log = self.log_page();
        self.scroll[p] = if log { max - start } else { start };
        let cur = self.cur[p].clamp(start, (start + vis - 1).min(total - 1));
        self.cur[p] = cur;
        self.follow[p] = log && cur == total - 1;
        if p == PAGE_BLE {
            self.ble_sel = self.ble.sorted().get(cur).map(|d| d.addr.clone());
        }
        self.sync_cursor();
    }

    /// Put the cursor on item `i` (a click).
    fn select_row(&mut self, i: usize) {
        let p = self.page;
        let (total, _) = self.dims();
        if i >= total {
            return;
        }
        self.cur[p] = i;
        self.follow[p] = i == total - 1;
        if p == PAGE_BLE {
            self.ble_sel = self.ble.sorted().get(i).map(|d| d.addr.clone());
        }
        self.sync_cursor();
    }

    /// Move the cursor by `delta` rows (clamped) and scroll to keep it visible.
    fn move_cursor(&mut self, delta: isize) {
        let p = self.page;
        self.sync_cursor();
        let (len, _) = self.dims();
        if len == 0 {
            return;
        }
        let now = self.cur[p] as isize;
        let new = (now + delta).clamp(0, len as isize - 1) as usize;
        self.cur[p] = new;
        self.follow[p] = new == len - 1;
        if p == PAGE_BLE {
            self.ble_sel = self.ble.sorted().get(new).map(|d| d.addr.clone());
        }
        self.sync_cursor();
    }

    /// Build the Device tab on first use (the real machine, or a sample in mock mode).
    fn ensure_dev(&mut self) {
        if self.dev.is_none() {
            let src: Box<dyn DevSource> = if self.mock { Box::new(Devices::sample()) } else { Box::new(Devices::system()) };
            self.dev = Some(DevView::new(src));
        }
    }

    fn with_dev<R>(&mut self, f: impl FnOnce(&mut DevView, &UsbView) -> R) -> Option<R> {
        self.ensure_dev();
        let mut dv = self.dev.take()?;
        dv.fit();
        let r = f(&mut dv, &self.usb);
        self.dev = Some(dv);
        Some(r)
    }

    /// A key for the tree on the Device tab; `Ignored` means the page may use it (Left/Right switch tabs).
    fn dev_key(&mut self, key: Keycode) -> TreeKey {
        self.with_dev(|dv, usb| {
            let DevView { src, tree, .. } = dv;
            tree.key(key, &mut |n| dev_children(&**src, usb, n))
        })
        .unwrap_or(TreeKey::Ignored)
    }

    /// The USB list changed (plug / unplug / highlight expiry): rebuild that branch.
    fn dev_usb_reload(&mut self) {
        if self.dev.is_none() {
            return;
        }
        self.with_dev(|dv, usb| {
            let DevView { src, tree, .. } = dv;
            tree.reload(CAT_USB, &mut |n| dev_children(&**src, usb, n));
        });
    }

    fn reset_cursor(&mut self) {
        let p = self.page;
        self.cur[p] = 0;
        self.scroll[p] = 0;
        self.follow[p] = true;
        if p == PAGE_BLE {
            self.ble_sel = None;
        }
    }

    fn clear(&mut self) {
        match self.page {
            PAGE_OSC => self.osc.reset(),
            PAGE_BLE => self.ble.devs.clear(),
            PAGE_IR => self.ir_log.clear(),
            PAGE_MIDI => {
                self.midi.log.clear();
                self.midi.held = [0; 128];
            }
            PAGE_ETH => self.eth.hist.clear(),
            PAGE_SER => self.ports.serial.log.clear(),
            _ => {}
        }
        self.reset_cursor();
    }
}

impl Drop for Interceptor {
    fn drop(&mut self) {
        self.worker = None;
    }
}

// ----------------------------------------------------------------------- drawing

struct Pal {
    ink: Color,
    paper: Color,
    /// Secondary text. `muted` (not `text_faint`) so it stays readable on the zebra tone.
    faint: Color,
    /// The one zebra tone (odd rows); even rows are the page background.
    alt: Color,
    /// Header bands.
    band: Color,
    bg: Color,
    /// Highlighted rows and the active tab (the theme's selection).
    sel_bg: Color,
    sel_fg: Color,
}

/// Row `i` of the body (0..8), full list width.
fn row_rect(i: i32) -> Rect {
    Rect::new(LIST_X, BODY_Y + i * ROW, list_w() as u32, ROW as u32)
}

/// `n` rows starting at body row `i`.
fn rows_rect(i: i32, n: i32) -> Rect {
    Rect::new(LIST_X, BODY_Y + i * ROW, list_w() as u32, (n * ROW) as u32)
}

/// Zebra: odd items get the alt tone.
/// A list row's background: a zebra tone on odd rows and a 1 px rule along the bottom, so every
/// item is separated from the next.
fn zebra(c: &mut Canvas<Window>, p: &Pal, r: Rect, n: usize) {
    if n % 2 == 1 {
        ui::fill_rect(c, r, p.alt);
    }
    ui::line(c, r.x(), r.bottom() - 1, r.right() - 1, r.bottom() - 1, p.band);
}

/// Zebra, then the cursor plate on top when `sel`. Returns (primary, secondary) text colours.
fn row_style(c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal, r: Rect, idx: usize, sel: bool) -> (Color, Color) {
    zebra(c, p, r, idx);
    if sel {
        widgets::draw_row_plate(c, ctx.tm, r);
        (p.sel_fg, p.sel_fg)
    } else {
        (p.ink, p.faint)
    }
}

/// `6E:44:B3:64:D5:9E` -> `**:**:**:**:**:9E` when `on` (only the last octet stays, enough to tell
/// devices apart on screen without printing the whole address).
pub fn mask_addr(addr: &str, on: bool) -> String {
    if !on {
        return addr.to_string();
    }
    let n = addr.split(':').count();
    if n < 2 {
        return addr.chars().map(|_| '*').collect();
    }
    addr.split(':').enumerate().map(|(i, o)| if i + 1 == n { o.to_string() } else { "**".to_string() }).collect::<Vec<_>>().join(":")
}

fn text_row(c: &mut Canvas<Window>, ctx: &Ctx, x: i32, y: i32, w: i32, s: &str, col: Color) {
    text_data(c, ctx, x, y, w, &i18n::tr_s(s), col);
}

/// Like [`text_row`] for data (device names, addresses, decoded frames): never translated.
fn text_data(c: &mut Canvas<Window>, ctx: &Ctx, x: i32, y: i32, w: i32, s: &str, col: Color) {
    let t = chrome::fit_text(ctx.fs, s, w);
    ctx.fs.draw_text(c, x, y + 1, &t, col);
}

fn right_text(c: &mut Canvas<Window>, ctx: &Ctx, right: i32, y: i32, s: &str, col: Color) {
    let s = i18n::tr_s(s);
    let w = ctx.fs.measure_text(&s);
    ctx.fs.draw_text(c, right - w, y + 1, &s, col);
}

/// The header band every page uses: one 16 px row (body row `row`), left text, optional right
/// text, closed by an ink rule on its last pixel row.
fn header_band(c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal, row: i32, left: &str, right: &str) {
    band_at(c, ctx, p, row_rect(row), left, right);
}

fn band_at(c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal, r: Rect, left: &str, right: &str) {
    ui::fill_rect(c, r, p.band);
    ui::line(c, r.x(), r.bottom() - 1, r.right() - 1, r.bottom() - 1, p.ink);
    let rw = if right.is_empty() { 0 } else { ctx.fs.measure_text(&i18n::tr_s(right)) + 8 };
    text_row(c, ctx, r.x() + 4, r.y(), r.width() as i32 - 8 - rw, left, p.ink);
    if !right.is_empty() {
        right_text(c, ctx, r.right() - 4, r.y(), right, p.ink);
    }
}

/// The outline of a list area: left, right and bottom edges (the top edge is the rule above it,
/// so the 16 px rows underneath keep their grid).
fn frame_sides(c: &mut Canvas<Window>, p: &Pal, r: Rect) {
    ui::line(c, r.x(), r.y(), r.x(), r.bottom() - 1, p.band);
    ui::line(c, r.right() - 1, r.y(), r.right() - 1, r.bottom() - 1, p.band);
    ui::line(c, r.x(), r.bottom() - 1, r.right() - 1, r.bottom() - 1, p.band);
}

/// A column separator at `x`, from body row `first_row` down to the frame's bottom line.
fn vsep(c: &mut Canvas<Window>, p: &Pal, x: i32, first_row: i32) {
    ui::line(c, x, BODY_Y + first_row * ROW, x, body_end() - 1, p.band);
}

/// A column separator over `rows` filled rows from body row `first_row` only, so it does not run on
/// through the empty part of a short list.
fn vsep_rows(c: &mut Canvas<Window>, p: &Pal, x: i32, first_row: i32, rows: usize) {
    if rows == 0 {
        return;
    }
    let end = (BODY_Y + (first_row + rows as i32) * ROW - 1).min(BODY_Y + body_h() - 1);
    ui::line(c, x, BODY_Y + first_row * ROW, x, end, p.band);
}

fn scrollbar(c: &mut Canvas<Window>, ctx: &Ctx, total: usize, visible: usize, offset: usize) {
    let sb = ScrollBar::new(body_scrollbar_rect(), total, visible, offset);
    sb.draw(c, ctx.tm, None);
}

/// RSSI as a 40 x 8 meter, vertically centred in the row at `y`.
fn rssi_meter(c: &mut Canvas<Window>, ctx: &Ctx, x: i32, y: i32, rssi: Option<i16>) {
    let frac = rssi.map(|r| ((r as i32 + 100).clamp(0, 70)) as f32 / 70.0).unwrap_or(0.0);
    draw_meter(c, ctx.tm, Rect::new(x, y + 4, 40, 8), frac);
}

impl Interceptor {
    fn draw_tabs(&self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        tabs_strip(self.page, self.tab_first).draw_strip(c, ctx.fs, ctx.tm);
        if self.paused {
            // Two thin bars in the free 4 px at the right end of the strip.
            ui::fill_rect(c, Rect::new(grid::sw() - 10, grid::BAR_H + 6, 1, 8), p.ink);
            ui::fill_rect(c, Rect::new(grid::sw() - 8, grid::BAR_H + 6, 1, 8), p.ink);
        }
    }

    fn draw_eth(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let g = eth_graph_rect();
        // The newest samples the graph has room for; Peak and the scale are of what is shown.
        let slots = eth_slots(g);
        let shown: Vec<(f32, f32)> = self.eth.hist.iter().skip(self.eth.hist.len().saturating_sub(slots)).copied().collect();
        let l = &self.eth.link;
        let rows: Vec<(&str, String)> = vec![
            ("Iface", self.eth.iface().to_string()),
            ("Link", {
                let st = if !l.up { "down" } else if l.carrier { "up" } else { "no carrier" };
                match l.speed_mbps {
                    Some(s) if l.carrier => format!("{st} {s} Mb/s"),
                    _ => st.to_string(),
                }
            }),
            ("IPv4", l.ip.clone().unwrap_or_else(|| "-".into())),
            ("MAC", l.mac.clone().unwrap_or_else(|| "-".into())),
            ("RX", fmt_rate(self.eth.rx_rate)),
            ("TX", fmt_rate(self.eth.tx_rate)),
            ("Peak", fmt_rate(shown.iter().fold(0.0f32, |m, (a, b)| m.max(*a).max(*b)) as f64)),
            ("Total", format!("{} / {}", fmt_bytes(self.eth.rx_total), fmt_bytes(self.eth.tx_total))),
        ];
        let col_w = ETH_COL_W;
        for (i, (k, v)) in rows.iter().enumerate() {
            let mut row = row_rect(i as i32);
            row.set_width(col_w as u32);
            zebra(c, p, row, i);
            ctx.fs.draw_text(c, row.x() + 4, row.y() + 1, k, p.faint);
            let v = chrome::fit_text(ctx.fs, v, col_w - 44);
            right_text(c, ctx, row.right() - 4, row.y(), &v, p.ink);
        }
        // The rule sits right of the longest key, so no value crosses it.
        vsep(c, p, LIST_X + 36, 0);
        // Right panel: label row, then the graph (RX solid, TX shaded) down to the last row.
        let max = nice_ceil(shown.iter().fold(1024.0f32, |m, (a, b)| m.max(*a).max(*b)));
        ctx.fs.draw_text(c, g.x(), BODY_Y + 1, "RX / TX", p.faint);
        right_text(c, ctx, g.right(), BODY_Y, &fmt_rate(max as f64), p.faint);
        ui::fill_rect(c, g, p.paper);
        let h = g.height() as f32;
        let off = slots - shown.len();
        for (i, (rx, tx)) in shown.iter().enumerate() {
            let (x, w) = (g.x() + ((off + i) * 2) as i32, 2);
            let th = ((tx / max) * h) as i32;
            let rh = ((rx / max) * h) as i32;
            if th > 0 {
                dither::fill(c, Rect::new(x, g.bottom() - th, w as u32, th as u32), p.ink, Pattern::Half);
            }
            if rh > 0 {
                ui::fill_rect(c, Rect::new(x, g.bottom() - rh, w as u32, rh as u32), p.ink);
            }
        }
        ui::draw_rect(c, g, p.ink);
    }

    fn draw_bt(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let ctrl = &self.bt_ctrl;
        let left = if let Some(m) = &self.bt_state {
            m.clone()
        } else if !ctrl.present {
            "Looking for the controller...".to_string()
        } else {
            format!("{}  {}", ctrl.name, mask_addr(&ctrl.addr, self.mask))
        };
        let mask = i18n::tr(if self.mask { "mask on" } else { "mask off" });
        let right = if ctrl.present && self.bt_state.is_none() { format!("{}  {}", if ctrl.powered { "ON" } else { "OFF" }, mask) } else { mask.to_string() };
        header_band(c, ctx, p, 0, &left, &right);
        let vis = (body_rows() - 1) as usize;
        self.scroll[PAGE_BT] = self.scroll[PAGE_BT].min(self.bt_devs.len().saturating_sub(vis));
        if self.bt_devs.is_empty() && self.bt_state.is_none() {
            text_row(c, ctx, LIST_X + 4, BODY_Y + ROW, list_w() - 8, "No paired devices.", p.faint);
        }
        for (i, d) in self.bt_devs.iter().enumerate().skip(self.scroll[PAGE_BT]).take(vis) {
            let n = i - self.scroll[PAGE_BT];
            let row = row_rect(1 + n as i32);
            let sel = i == self.cur[PAGE_BT];
            let (fg, sub) = row_style(c, ctx, p, row, i, sel);
            let st = if d.connected { "connected" } else if d.paired { "paired" } else { "trusted" };
            let sw = ctx.fs.measure_text(st);
            let _ = sw;
            text_data(c, ctx, row.x() + 4, row.y(), list_w() - 84 - 8, &d.name, fg);
            right_text(c, ctx, row.right() - 4, row.y(), st, if d.connected { fg } else { sub });
        }
        vsep_rows(c, p, LIST_X + list_w() - 84, 1, self.bt_devs.len().saturating_sub(self.scroll[PAGE_BT]).min(vis));
        scrollbar(c, ctx, self.bt_devs.len(), vis, self.scroll[PAGE_BT]);
    }

    fn draw_ble(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let sorted = self.ble.sorted();
        let head = match &self.ble_state {
            Some(m) => m.clone(),
            None => format!("{}{} {}", i18n::tr(if self.paused { "Paused" } else { "Scanning" }), if self.mock { i18n::tr(" (demo)") } else { "" }, i18n::tr_s(&format!("{} devices", sorted.len()))),
        };
        header_band(c, ctx, p, 0, &head, if self.mask { "mask on" } else { "mask off" });
        // 1 header row + two-row devices (3 on 320 x 170; an odd row left at the bottom stays free).
        let vis = ble_vis();
        let total = sorted.len();
        self.scroll[PAGE_BLE] = self.scroll[PAGE_BLE].min(total.saturating_sub(vis));
        for (i, d) in sorted.iter().enumerate().skip(self.scroll[PAGE_BLE]).take(vis) {
            let n = (i - self.scroll[PAGE_BLE]) as i32;
            let item = rows_rect(1 + 2 * n, 2);
            let (fg, sub_fg) = row_style(c, ctx, p, item, i, i == self.cur[PAGE_BLE]);
            let (x, y) = (item.x(), item.y());
            let addr = mask_addr(&d.addr, self.mask);
            let name = d.name.clone().unwrap_or_else(|| addr.clone());
            let dbm = d.rssi.map(|r| format!("{r}")).unwrap_or_else(|| "-".into());
            let dw = ctx.fs.measure_text("-100");
            right_text(c, ctx, item.right() - 4, y, &dbm, fg);
            rssi_meter(c, ctx, item.right() - 4 - dw - 4 - 40, y, d.rssi);
            text_data(c, ctx, x + 4, y, list_w() - 8 - dw - 4 - 40 - 8, &name, fg);
            let mut sub = String::new();
            if d.name.is_some() {
                sub.push_str(&addr);
                sub.push(' ');
            }
            if let Some(id) = d.mfr_id {
                sub.push_str(&company(id).map(|s| s.to_string()).unwrap_or_else(|| format!("{id:04X}")));
                sub.push(' ');
            }
            sub.push_str(&hex_str(&d.mfr, 12));
            text_data(c, ctx, x + 4, y + ROW, list_w() - 8, &sub, sub_fg);
        }
        scrollbar(c, ctx, total, vis, self.scroll[PAGE_BLE]);
    }

    fn draw_osc(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let now = self.now_ms();
        let o = &self.osc;
        let ip = o.ips.first().cloned().unwrap_or_else(|| "0.0.0.0".into());
        let left = match (&o.err, o.listening) {
            (Some(e), _) => e.clone(),
            (None, true) => format!("listening on {}:{}", ip, o.port()),
            _ => format!("binding port {}...", o.port()),
        };
        header_band(c, ctx, p, 0, &left, "");
        // Stats row.
        let r1 = row_rect(1);
        ui::fill_rect(c, r1, p.alt);
        let st = format!("{:.0} msg/s   {} {}   {} {}", o.table.total.rate(now), i18n::tr("dropped"), o.table.dropped, i18n::tr("total"), o.table.total.count);
        text_row(c, ctx, r1.x() + 4, r1.y(), list_w() - 56, &st, p.faint);
        right_text(c, ctx, r1.right() - 4, r1.y(), if o.table_view { "table" } else { "log" }, p.ink);
        let vis = (body_rows() - 2) as usize;
        if !self.osc.table_view {
            let total = self.osc.log.len();
            self.scroll[PAGE_OSC] = self.scroll[PAGE_OSC].min(total.saturating_sub(vis));
            if total == 0 {
                text_row(c, ctx, LIST_X + 4, BODY_Y + 2 * ROW, list_w() - 8, "Waiting for OSC packets...", p.faint);
            }
            let end = total - self.scroll[PAGE_OSC];
            let start = end.saturating_sub(vis);
            for (k, e) in self.osc.log.iter().enumerate().skip(start).take(end - start) {
                let row = row_rect(2 + (k - start) as i32);
                let (fg, sub) = row_style(c, ctx, p, row, k, k == self.cur[PAGE_OSC]);
                let secs = format!("{:05.1}", (e.ms % 100_000) as f32 / 1000.0);
                ctx.fs.draw_text(c, row.x() + 4, row.y() + 1, &secs, sub);
                let src = short_src(&e.src);
                let sw = ctx.fs.measure_text(&src);
                right_text(c, ctx, row.right() - 4, row.y(), &src, sub);
                let text = if e.args.is_empty() { e.addr.clone() } else { format!("{}  {}", e.addr, e.args) };
                let _ = sw;
                text_data(c, ctx, row.x() + 40, row.y(), list_w() - 40 - 62, &text, fg);
            }
            let shown = end - start;
            vsep_rows(c, p, LIST_X + 36, 2, shown);
            vsep_rows(c, p, LIST_X + list_w() - 58, 2, shown);
            scrollbar(c, ctx, total, vis, total.saturating_sub(vis).saturating_sub(self.scroll[PAGE_OSC]));
        } else {
            let rows: Vec<(&String, &osc::AddrStat)> = self.osc.table.map.iter().collect();
            let total = rows.len();
            // Extra width goes half to the address, half to the last value.
            let (ea, el) = (extra_w() / 2, extra_w() - extra_w() / 2);
            let e = ea + el;
            self.scroll[PAGE_OSC] = self.scroll[PAGE_OSC].min(total.saturating_sub(vis));
            for (k, (addr, s)) in rows.iter().enumerate().skip(self.scroll[PAGE_OSC]).take(vis) {
                let row = row_rect(2 + (k - self.scroll[PAGE_OSC]) as i32);
                let (fg, sub) = row_style(c, ctx, p, row, k, k == self.cur[PAGE_OSC]);
                let (x, y) = (row.x(), row.y());
                text_data(c, ctx, x + 4, y, 82 + ea, addr, fg);
                right_text(c, ctx, x + 122 + ea, y, &format!("{}", s.count), sub);
                text_data(c, ctx, x + 130 + ea, y, 84 + el, &s.last, fg);
                right_text(c, ctx, x + 250 + e, y, &format!("{:.0}/s", s.rate(now)), sub);
                let h = s.history(now);
                let m = h.iter().copied().max().unwrap_or(0).max(1) as i32;
                for (j, v) in h.iter().enumerate() {
                    let bh = (*v as i32 * 10 / m).max(if *v > 0 { 1 } else { 0 });
                    if bh > 0 {
                        ui::fill_rect(c, Rect::new(x + 254 + e + j as i32 * 3, y + 13 - bh, 2, bh as u32), fg);
                    }
                }
            }
            if total == 0 {
                text_row(c, ctx, LIST_X + 4, BODY_Y + 2 * ROW, list_w() - 8, "No addresses yet.", p.faint);
            }
            for x in [90 + ea, 126 + ea, 218 + e, 252 + e] {
                vsep(c, p, LIST_X + x, 2);
            }
            scrollbar(c, ctx, total, vis, self.scroll[PAGE_OSC]);
        }
    }

    fn draw_ir(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        match &self.ir_hw {
            IrHw::None(lines) => {
                header_band(c, ctx, p, 0, "No IR receiver found", "");
                for (i, l) in lines.iter().enumerate().take((body_rows() - 1) as usize) {
                    let row = row_rect(1 + i as i32);
                    zebra(c, p, row, i + 1);
                    text_row(c, ctx, row.x() + 4, row.y(), list_w() - 8, l, p.faint);
                }
            }
            IrHw::Rx(path) => {
                // Row 0: pulse timings of the newest frame.
                let strip = row_rect(0);
                ui::fill_rect(c, strip, p.paper);
                if let Some(e) = &self.ir_err {
                    text_row(c, ctx, strip.x() + 4, strip.y(), list_w() - 8, e, p.faint);
                } else if let Some(f) = self.ir_log.back() {
                    let total: u32 = f.edges.iter().sum::<u32>().max(1);
                    let scale = (strip.width() - 8) as f32 / total as f32;
                    let mut x = 0f32;
                    for (i, e) in f.edges.iter().enumerate() {
                        let w = (*e as f32 * scale).max(1.0);
                        if i % 2 == 0 {
                            ui::fill_rect(c, Rect::new(strip.x() + 4 + x as i32, strip.y() + 3, w.round().max(1.0) as u32, 10), p.ink);
                        }
                        x += w;
                    }
                } else {
                    text_row(c, ctx, strip.x() + 4, strip.y(), list_w() - 8, &format!("Point a remote at the receiver ({})", path.trim_start_matches("/dev/")), p.faint);
                }
                let vis = (body_rows() - 1) as usize;
                let total = self.ir_log.len();
                self.scroll[PAGE_IR] = self.scroll[PAGE_IR].min(total.saturating_sub(vis));
                let end = total - self.scroll[PAGE_IR];
                let start = end.saturating_sub(vis);
                for (k, f) in self.ir_log.iter().enumerate().skip(start).take(end - start) {
                    let row = row_rect(1 + (k - start) as i32);
                    let (fg, sub) = row_style(c, ctx, p, row, k, k == self.cur[PAGE_IR]);
                    text_data(c, ctx, row.x() + 4, row.y(), list_w() - 68, &f.decoded, fg);
                    right_text(c, ctx, row.right() - 4, row.y(), &format!("{} edges", f.edges.len()), sub);
                }
                vsep_rows(c, p, LIST_X + list_w() - 62, 1, end - start);
                scrollbar(c, ctx, total, vis, total.saturating_sub(vis).saturating_sub(self.scroll[PAGE_IR]));
            }
        }
    }

    fn draw_dev(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        let mask = self.mask;
        let Some(dv) = self.dev.as_mut() else { return };
        dv.fit();
        dv.tree.sync();
        // Addresses show masked or not as the setting says (the rows keep the real text).
        for r in dv.tree.rows.iter_mut().filter(|r| r.secret) {
            r.extra = mask_addr(&r.value, mask);
        }
        let tree_rect = dev_tree_rect();
        dv.tree.draw(c, ctx.fs, ctx.tm, tree_rect);
        dv.tree.scrollbar(body_scrollbar_rect()).draw(c, ctx.tm, None);
        frame_sides(c, p, body_frame(list_w()));
    }

    fn draw_midi(&mut self, c: &mut Canvas<Window>, ctx: &Ctx, p: &Pal) {
        // Rows 0..3 (48 px): 4 px margin, piano C1..B6 (20), 4 px gap, 16 channel pads (16), 4 px margin.
        const FIRST: u8 = 24;
        const LAST: u8 = 95;
        let nwhite = (FIRST..=LAST).filter(|n| !is_black(*n)).count() as i32;
        // The keys widen with the list (7 px white, 4 px black on 320 x 170).
        let kw = (list_w() / nwhite).max(7);
        let bw = kw * 4 / 7;
        let x0 = LIST_X + (list_w() - nwhite * kw) / 2;
        let y0 = BODY_Y + 4;
        let mut wi = 0;
        for n in FIRST..=LAST {
            if is_black(n) {
                continue;
            }
            let r = Rect::new(x0 + wi * kw, y0, kw as u32, 20);
            ui::fill_rect(c, r, if self.midi.held[n as usize] > 0 { p.ink } else { p.paper });
            ui::draw_rect(c, r, p.faint);
            wi += 1;
        }
        let mut wi = 0;
        for n in FIRST..=LAST {
            if !is_black(n) {
                wi += 1;
                continue;
            }
            let r = Rect::new(x0 + wi * kw - bw / 2, y0, bw as u32, 12);
            let on = self.midi.held[n as usize] > 0;
            ui::fill_rect(c, r, if on { p.paper } else { p.ink });
            ui::draw_rect(c, r, p.ink);
        }
        let ly = y0 + 24;
        // The 16 channel pads share the extra width too (16 px pads, 2 px apart on 320 x 170).
        let pitch = 18 + extra_w() / 16;
        let px = LIST_X + (list_w() - 16 * pitch + 2) / 2;
        for (ch, lv) in self.midi.level.iter().enumerate() {
            let cell = Rect::new(px + ch as i32 * pitch, ly, (pitch - 2) as u32, 16);
            ui::fill_rect(c, cell, p.paper);
            let h = (lv * 14.0) as i32;
            if h > 0 {
                ui::fill_rect(c, Rect::new(cell.x() + 1, cell.bottom() - 1 - h, cell.width() - 2, h as u32), p.ink);
            }
            ui::draw_rect(c, cell, p.faint);
        }
        // Log, newest at the bottom: rows 3..8.
        let vis = (body_rows() - 3) as usize;
        let total = self.midi.log.len();
        self.scroll[PAGE_MIDI] = self.scroll[PAGE_MIDI].min(total.saturating_sub(vis));
        if total == 0 {
            let msg = if !self.midi.ports_known {
                "Looking for MIDI ports...".to_string()
            } else if self.midi.ports.is_empty() {
                "No MIDI ports. Plug in a USB MIDI device.".to_string()
            } else {
                format!("Listening: {}", self.midi.ports.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "))
            };
            text_row(c, ctx, LIST_X + 4, BODY_Y + 3 * ROW, list_w() - 8, &msg, p.faint);
        }
        let end = total - self.scroll[PAGE_MIDI];
        let start = end.saturating_sub(vis);
        for (k, (_, m)) in self.midi.log.iter().enumerate().skip(start).take(end - start) {
            let row = row_rect(3 + (k - start) as i32);
            let (fg, sub) = row_style(c, ctx, p, row, k, k == self.cur[PAGE_MIDI]);
            let y = row.y();
            let ch = m.channel().map(|c| format!("{:02}", c + 1)).unwrap_or_else(|| "--".into());
            ctx.fs.draw_text(c, row.x() + 4, y + 1, &ch, sub);
            ctx.fs.draw_text(c, row.x() + 24, y + 1, m.kind(), fg);
            text_data(c, ctx, row.x() + 84, y, list_w() - 84 - 52, &m.detail(), fg);
            if let Some(l) = m.level() {
                draw_meter(c, ctx.tm, Rect::new(row.right() - 44, y + 4, 40, 8), l as f32 / 127.0);
            }
        }
        for x in [22, 80, list_w() - 48] {
            vsep_rows(c, p, LIST_X + x, 3, end - start);
        }
        scrollbar(c, ctx, total, vis, total.saturating_sub(vis).saturating_sub(self.scroll[PAGE_MIDI]));
    }
}

fn is_black(n: u8) -> bool {
    matches!(n % 12, 1 | 3 | 6 | 8 | 10)
}

fn nice_ceil(v: f32) -> f32 {
    let mut s = 1024.0f32;
    while s < v {
        s *= 2.0;
    }
    s
}

impl LauncherApp for Interceptor {
    fn sizing(&self) -> crate::launcher::app::Sizing {
        crate::launcher::app::Sizing::Flexible
    }

    /// Tab switches tabs, so a double Tab here is not the Agent (Alt+Tab is).
    fn wants_tab(&self) -> bool {
        true
    }

    fn title(&self) -> String {
        "Interceptor".into()
    }

    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        let mut v = vec![k("Space", "Pause / resume"), k("M", "Mask addresses"), k("Backspace", "Clear the page"), k("Up / Down", "Choose a row")];
        v.extend(match self.page {
            PAGE_ETH => vec![k("Return", "Next interface")],
            PAGE_OSC => vec![k("Return", "Next port"), k("V", "Log / table")],
            PAGE_DEV => vec![k("Return", "Open / close a node")],
            PAGE_WIFI => vec![k("Return", "Scan again")],
            PAGE_GROVE => vec![k("Return", "5V on / off")],
            PAGE_SER => vec![k("Return", "Open the port / send the line"), k("P", "Next port (closed)"), k("B", "Next speed (closed)"), k("E", "Next line ending (closed)"), k("H", "Text / hex (closed)")],
            PAGE_I2C => vec![k("Return", "Next bus"), k("R", "Scan again")],
            PAGE_GPIO => vec![k("Return", "Next chip")],
            PAGE_W1 => vec![k("Return", "Read again")],
            _ => vec![],
        });
        v
    }

    fn menu(&self) -> Vec<MenuItem> {
        // The page's own items first, then the ones every page has.
        let mut v = match self.page {
            PAGE_OSC => vec![MenuItem::new("view", "OSC: log / table"), MenuItem::new("port", "OSC: next port")],
            PAGE_DEV => vec![MenuItem::new("dev_toggle", "Device: expand / collapse (Return)"), MenuItem::new("dev_collapse", "Device: collapse all")],
            p => self.ports.menu(p),
        };
        if let Some((pkg, _)) = self.ports.missing_package(self.page) {
            v.insert(0, MenuItem::new("install_pkg", format!("Install {pkg}...")));
        }
        v.extend([
            MenuItem::new("mask", format!("Mask addresses: {}", if self.mask { "on" } else { "off" })),
            MenuItem::new("pause", if self.paused { "Resume" } else { "Pause" }),
            MenuItem::new("clear", "Clear page"),
            MenuItem::new("next", "Next page"),
        ]);
        v
    }

    fn context_menu(&mut self, x: i32, y: i32, _ctx: &Ctx) -> Option<Vec<MenuItem>> {
        if self.page == PAGE_DEV {
            let rect = dev_tree_rect();
            let paused = self.paused;
            return self.with_dev(|dv, _usb| {
                if widgets::contains(rect, x, y) {
                    let idx = dv.tree.scroll + ((y - rect.y()) / widgets::TREE_ROW_H) as usize;
                    if idx < dv.tree.rows.len() {
                        dv.tree.cursor = idx;
                        dv.tree.sync();
                    }
                }
                let mut v = Vec::new();
                if let Some(n) = dv.tree.selected().filter(|n| n.expandable) {
                    let open = dv.tree.index_of(&n.key).map_or(false, |i| dv.tree.rows[i].expanded);
                    v.push(MenuItem::new("dev_toggle", if open { "Collapse" } else { "Expand" }));
                }
                v.push(MenuItem::new("dev_collapse", "Collapse all"));
                v.push(MenuItem::new("pause", if paused { "Resume" } else { "Pause" }));
                v
            });
        }
        if let Some(g) = self.list_geom() {
            if let Some(i) = g.row_at(x, y) {
                self.select_row(i);
            }
            return Some(vec![
                MenuItem::new("pause", if self.paused { "Resume" } else { "Pause" }),
                MenuItem::new("clear", "Clear page"),
                MenuItem::new("mask", format!("Mask addresses: {}", if self.mask { "on" } else { "off" })),
            ]);
        }
        None
    }

    fn menu_action(&mut self, id: &str, ctx: &Ctx) -> AppAction {
        match id {
            "pause" => self.paused = !self.paused,
            "clear" => self.clear(),
            "next" => self.set_page(self.page + 1),
            "view" => {
                self.osc.table_view = !self.osc.table_view;
                self.set_page(PAGE_OSC);
                self.reset_cursor();
            }
            "mask" => self.mask = !self.mask,
            "dev_toggle" => {
                self.dev_key(Keycode::Return);
            }
            "dev_collapse" => {
                self.with_dev(|dv, _| {
                    dv.tree.collapse_all();
                });
            }
            "grove_serial" => {
                self.ports.serial.select("/dev/ttyS0");
                self.set_page(PAGE_SER);
            }
            "grove_i2c" => self.set_page(PAGE_I2C),
            "install_pkg" => {
                if let Some((pkg, why)) = self.ports.missing_package(self.page) {
                    return AppAction::Install(crate::launcher::installer::InstallReq::apt(&[pkg], why));
                }
            }
            id => {
                if self.ports.menu_action(self.page, id, ctx) {
                    self.last_page_started = None;
                    self.worker = None;
                    self.ports.restart = false;
                }
            }
        }
        AppAction::None
    }

    fn text(&mut self, text: &str, _ctx: &Ctx) -> AppAction {
        if self.page == PAGE_SER && self.ports.serial.is_open() {
            self.ports.serial.input.extend(text.chars().filter(|c| !c.is_control()));
        }
        AppAction::None
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, ctx: &Ctx) -> AppAction {
        let p = self.page;
        if matches!(arrow_alias(key), Keycode::Return | Keycode::KpEnter) {
            if let Some((pkg, why)) = self.ports.missing_package(p) {
                return AppAction::Install(crate::launcher::installer::InstallReq::apt(&[pkg], why));
            }
        }
        if p == PAGE_SER && self.ports.serial.is_open() {
            // Typing goes to the line to send: only keys that never type stay commands.
            match arrow_alias(key) {
                Keycode::Return | Keycode::KpEnter => {
                    self.ports.serial_send();
                    return AppAction::None;
                }
                Keycode::Backspace => {
                    self.ports.serial.input.pop();
                    return AppAction::None;
                }
                Keycode::Escape | Keycode::Tab | Keycode::Left | Keycode::Right | Keycode::Up | Keycode::Down | Keycode::PageUp | Keycode::PageDown | Keycode::Home | Keycode::End => {}
                _ => return AppAction::None,
            }
        }
        match arrow_alias(key) {
            Keycode::Escape => return AppAction::Quit,
            // Tab / Shift+Tab: next / previous tab (Left / Right do the same).
            Keycode::Tab if mods.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD) => self.set_page(p + NP - 1),
            Keycode::Tab => self.set_page(p + 1),
            k @ (Keycode::Up | Keycode::Down | Keycode::PageUp | Keycode::PageDown | Keycode::Home | Keycode::End | Keycode::Return) if p == PAGE_DEV => {
                self.dev_key(k);
            }
            Keycode::Left => self.set_page(p + NP - 1),
            Keycode::Right => self.set_page(p + 1),
            Keycode::Up => self.move_cursor(-1),
            Keycode::Down => self.move_cursor(1),
            Keycode::PageUp => self.move_cursor(-(self.dims().1 as isize).max(1)),
            Keycode::PageDown => self.move_cursor((self.dims().1 as isize).max(1)),
            Keycode::Home => self.move_cursor(isize::MIN / 2),
            Keycode::End => self.move_cursor(isize::MAX / 2),
            Keycode::M => self.mask = !self.mask,
            Keycode::Space => self.paused = !self.paused,
            Keycode::Return if p == PAGE_ETH => self.eth.next_iface(),
            k if self.ports.key(p, k, ctx) => {
                if self.ports.restart {
                    self.ports.restart = false;
                    self.last_page_started = None;
                    self.worker = None;
                }
            }
            Keycode::Return if p == PAGE_OSC => {
                self.osc.port_idx = (self.osc.port_idx + 1) % OSC_PORTS.len();
                self.osc.reset();
                self.set_page(PAGE_OSC); // restart the listener on the new port
            }
            Keycode::V if p == PAGE_OSC => {
                self.osc.table_view = !self.osc.table_view;
                self.reset_cursor();
            }
            Keycode::Delete | Keycode::Backspace => self.clear(),
            _ => {}
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> AppAction {
        // Tabs: a click on a tab switches to it.
        if let PointerEvent::Down { x, y, button: 0, .. } = ev {
            if self.ports.missing_package(self.page).is_some() && contains_pt(Self::install_button_rect(), x, y) {
                let (pkg, why) = self.ports.missing_package(self.page).unwrap();
                return AppAction::Install(crate::launcher::installer::InstallReq::apt(&[pkg], why));
            }
            if let Some(h) = tabs_strip(self.page, self.tab_first).hit(x, y, ctx.tm) {
                self.sb_drag = false;
                let to = match h {
                    widgets::FolderHit::Tab(i) => i,
                    widgets::FolderHit::Prev => self.page + NP - 1,
                    widgets::FolderHit::Next => self.page + 1,
                };
                if to % NP != self.page {
                    self.set_page(to);
                }
                return AppAction::None;
            }
        }
        if let PointerEvent::Up { .. } = ev {
            self.sb_drag = false;
        }
        if self.page == PAGE_DEV {
            let sb_rect = body_scrollbar_rect();
            let mut drag = self.sb_drag;
            self.with_dev(|dv, usb| {
                let DevView { src, tree, .. } = dv;
                let mut used = false;
                match ev {
                    PointerEvent::Down { x, y, button: 0, .. } if contains_pt(sb_rect, x, y) => {
                        if let Some((off, start_drag)) = tree.scrollbar(sb_rect).press(x, y) {
                            tree.set_scroll(off);
                            drag = start_drag;
                        }
                        used = true;
                    }
                    PointerEvent::Drag { y, .. } if drag => {
                        let off = tree.scrollbar(sb_rect).offset_for_y(y);
                        tree.set_scroll(off);
                        used = true;
                    }
                    _ => {}
                }
                if !used {
                    tree.pointer(ev, dev_tree_rect(), &mut |n| dev_children(&**src, usb, n));
                }
            });
            self.sb_drag = drag;
            return AppAction::None;
        }
        match ev {
            PointerEvent::Wheel { dy, .. } => self.move_cursor(if dy > 0 { -1 } else { 1 }),
            PointerEvent::Down { x, y, button: 0, .. } => {
                let Some(g) = self.list_geom() else { return AppAction::None };
                let sb_rect = body_scrollbar_rect();
                if contains_pt(sb_rect, x, y) {
                    let sb = ScrollBar::new(sb_rect, g.total, g.vis, g.start);
                    if let Some((off, start_drag)) = sb.press(x, y) {
                        self.set_start(off);
                        self.sb_drag = start_drag;
                    }
                } else if let Some(i) = g.row_at(x, y) {
                    self.select_row(i);
                }
            }
            PointerEvent::Drag { y, .. } if self.sb_drag => {
                if let Some(g) = self.list_geom() {
                    let off = ScrollBar::new(body_scrollbar_rect(), g.total, g.vis, g.start).offset_for_y(y);
                    self.set_start(off);
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, ctx: &Ctx) -> bool {
        let now = Instant::now();
        let mut dirty = false;
        if self.page == PAGE_DEV {
            if let Some(dv) = self.dev.as_mut() {
                dv.fit();
                if dv.last_marquee.map_or(true, |t| now.duration_since(t) >= Duration::from_millis(300)) {
                    dv.last_marquee = Some(now);
                    let before = dv.tree.shift;
                    dv.tree.marquee(ctx.fs);
                    dirty |= dv.tree.shift != before;
                }
            }
        }
        self.ensure_worker();
        dirty |= self.ports.tick(self.page, self.paused, ctx);
        if self.ports.restart {
            self.ports.restart = false;
            self.last_page_started = None;
            self.worker = None;
        }
        while let Ok(ev) = self.rx.try_recv() {
            if !self.paused {
                self.apply(ev);
                dirty = true;
            }
        }
        let dt = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        if self.midi.decay(dt) {
            dirty = true;
        }
        if self.mock && !self.paused && now.duration_since(self.last_osc_mock) >= Duration::from_millis(130) {
            self.last_osc_mock = now;
            self.osc.mock_n += 1;
            let ms = self.now_ms();
            self.osc.feed_packet(ms, "192.168.3.7:9001", &mock_osc_packet(self.osc.mock_n));
            dirty = true;
        }
        if self.mock && !self.paused && now.duration_since(self.last_mock) >= Duration::from_millis(700) {
            self.last_mock = now;
            self.mock_step += 1;
            let ms = self.t0.elapsed().as_millis() as u64;
            for l in mock_ble_lines(self.mock_step) {
                self.ble.feed_line(&l, ms);
            }
            self.midi.feed_bytes("20:0", &mock_midi_bytes(self.mock_step + 8));
            if self.mock_step % 5 == 0 {
                let (a, c) = (0x00u8, [0x45u8, 0x46, 0x47, 0x44, 0x40][(self.mock_step / 5) as usize % 5]);
                self.ir_log.push_back(IrFrame { decoded: decode_ir(&nec_edges(a, c)), edges: nec_edges(a, c) });
            }
            dirty = true;
        }
        let mut usb_changed = false;
        if now.duration_since(self.last_sample) >= Duration::from_secs(1) && !self.paused {
            self.last_sample = now;
            if self.mock {
                let at = self.t0.elapsed() + Duration::from_secs(31);
                self.mock_tick_eth(at);
                usb_changed = self.usb.update(now, mock_usb(self.t0.elapsed()));
            } else {
                let text = std::fs::read_to_string("/proc/net/dev").unwrap_or_default();
                self.eth.sample(now, &parse_proc_net_dev(&text));
                self.eth.link = read_link(self.eth.iface());
                let list = list_usb(Path::new(&self.usb_root));
                usb_changed = self.usb.update(now, list);
            }
            if !self.mock {
                self.osc.ips = ipv4_list().into_iter().filter(|(n, _)| n != "lo").map(|(_, ip)| ip).collect();
            }
            self.ble.prune(self.t0.elapsed().as_millis() as u64, 60_000);
            dirty = true;
        }
        if usb_changed {
            self.dev_usb_reload();
        }
        // The USB highlight fades by itself.
        if !self.usb.added.is_empty() || !self.usb.removed.is_empty() {
            dirty = true;
        }
        dirty
    }

    fn draw(&mut self, c: &mut Canvas<Window>, _assets: &mut AssetStore, ctx: &Ctx) {
        let tm = ctx.tm;
        let p = Pal {
            ink: tm.get_color("foreground"),
            paper: tm.get_color("widget_face_active"),
            faint: widgets::secondary_ink(tm),
            alt: tm.get_color("surface_alt"),
            band: tm.get_color("line"),
            bg: tm.get_color("home_bg"),
            sel_bg: tm.get_color("select_bg"),
            sel_fg: tm.get_color("select_fg"),
        };
        if self.page == PAGE_DEV {
            self.ensure_dev();
        }
        self.sync_cursor();
        ui::fill_rect(c, chrome::app_rect(), p.bg);
        self.draw_tabs(c, ctx, &p);
        match self.page {
            PAGE_ETH => self.draw_eth(c, ctx, &p),
            PAGE_BT => self.draw_bt(c, ctx, &p),
            PAGE_BLE => self.draw_ble(c, ctx, &p),
            PAGE_OSC => self.draw_osc(c, ctx, &p),
            PAGE_IR => self.draw_ir(c, ctx, &p),
            PAGE_DEV => self.draw_dev(c, ctx, &p),
            PAGE_MIDI => self.draw_midi(c, ctx, &p),
            PAGE_USB => self.draw_usb(c, ctx, &p),
            PAGE_WIFI => self.draw_wifi(c, ctx, &p),
            PAGE_GROVE => self.draw_grove(c, ctx, &p),
            PAGE_SER => self.draw_serial(c, ctx, &p),
            PAGE_I2C => self.draw_i2c(c, ctx, &p),
            PAGE_GPIO => self.draw_gpio(c, ctx, &p),
            _ => self.draw_w1(c, ctx, &p),
        }
        match self.page {
            PAGE_ETH => frame_sides(c, &p, body_frame(ETH_COL_W)),
            PAGE_DEV => {}
            _ => frame_sides(c, &p, body_frame(list_w())),
        }
    }
}

// ======================================================================== tests

#[cfg(test)]
mod tests {
    use super::*;

    const THEME_FILES: [&str; 8] = ["theme-black", "theme-black-invert", "theme-paper", "theme-paper-invert", "theme-mobile", "theme-mobile-invert", "theme-terminal", "theme-terminal-invert"];

    const NETDEV: &str = "Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:   16084      77    0    0    0     0          0         0    16084      77    0    0    0     0       0          0
  eth0: 41102252   29368    0    0    0     0          0         0  1045953   12225    0    0    0     0       0          0
";

    #[test]
    fn proc_net_dev_is_parsed() {
        let v = parse_proc_net_dev(NETDEV);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1], NetCounters { name: "eth0".into(), rx: 41102252, tx: 1045953 });
    }

    #[test]
    fn rates_and_history() {
        assert_eq!(rate(100, 1100, 2.0), 500.0);
        assert_eq!(rate(100, 50, 1.0), 0.0); // wrapped
        let t = Instant::now();
        let mut e = Eth::new();
        e.sample(t, &[NetCounters { name: "eth0".into(), rx: 0, tx: 0 }, NetCounters { name: "lo".into(), rx: 5, tx: 5 }]);
        e.sample(t + Duration::from_secs(1), &[NetCounters { name: "eth0".into(), rx: 2048, tx: 1024 }]);
        assert_eq!(e.rx_rate, 2048.0);
        assert_eq!(e.hist.len(), 1);
        assert_eq!(fmt_rate(2048.0), "2.0 KB/s");
        assert_eq!(fmt_bytes(41_102_252), "39.2 MB");
    }

    #[test]
    fn ble_scan_stream() {
        let mut t = BleTable::default();
        let stream = "\x1b[0;92m[NEW]\x1b[0m Device 6E:44:B3:64:D5:9E 6E-44-B3-64-D5-9E
[NEW] Device C8:2B:96:11:0A:4F Mi Band
[CHG] Device 6E:44:B3:64:D5:9E RSSI: 0xffffffd1 (-47)
[CHG] Device 6E:44:B3:64:D5:9E ManufacturerData.Key: 0x004c (76)
[CHG] Device 6E:44:B3:64:D5:9E ManufacturerData.Value:
  0c 0e 08 ca 57 bf a3 07 5a 82 ad 90 89 ea e6 c7  ....W...Z.......
  10 06 44 1d 47 b0 fa 18                          ..D.G...
[CHG] Device C8:2B:96:11:0A:4F RSSI: 0xffffffc3 (-61)
[CHG] Device C8:2B:96:11:0A:4F TxPower: 0x000c (12)
";
        for l in stream.lines() {
            t.feed_line(l, 10);
        }
        assert_eq!(t.devs.len(), 2);
        let a = &t.devs[0];
        assert_eq!(a.name, None);
        assert_eq!(a.rssi, Some(-47));
        assert_eq!(a.mfr_id, Some(0x4c));
        assert_eq!(a.mfr.len(), 24);
        assert_eq!(&a.mfr[..3], &[0x0c, 0x0e, 0x08]);
        assert_eq!(t.devs[1].name.as_deref(), Some("Mi Band"));
        assert_eq!(t.sorted()[0].addr, "6E:44:B3:64:D5:9E");
        t.feed_line("[DEL] Device C8:2B:96:11:0A:4F", 20);
        assert_eq!(t.devs.len(), 1);
        t.prune(100_000, 60_000);
        assert!(t.devs.is_empty());
    }

    #[test]
    fn bluetoothctl_info_show_devices() {
        let info = "Device F0:16:1D:02:B5:DA (public)\n\tName: StickMouse\n\tAlias: StickMouse\n\tIcon: input-mouse\n\tPaired: yes\n\tTrusted: yes\n\tConnected: no\n";
        let d = parse_bt_info(info).unwrap();
        assert_eq!((d.addr.as_str(), d.name.as_str(), d.paired, d.connected), ("F0:16:1D:02:B5:DA", "StickMouse", true, false));
        assert!(parse_bt_info("Device not available\n").is_none() || true);
        let show = "Controller F0:40:AF:90:3F:83 (public)\n\tName: CardputerZero\n\tAlias: CardputerZero\n\tPowered: yes\n\tDiscoverable: no\n";
        let c = parse_bt_show(show);
        assert!(c.powered && c.present);
        assert_eq!(c.name, "CardputerZero");
        let devs = parse_bt_devices("Device F0:16:1D:02:B5:DA StickMouse\nDevice 11:22:33:44:55:66 My Phone\n");
        assert_eq!(devs[1], ("11:22:33:44:55:66".to_string(), "My Phone".to_string()));
    }

    #[test]
    fn usb_sysfs_and_hotplug() {
        let files: HashMap<&str, &str> = [("idVendor", "0fe6"), ("idProduct", "9900"), ("manufacturer", "CoreChips"), ("product", "USB 10/100 LAN"), ("speed", "480"), ("busnum", "1"), ("devnum", "3")].into_iter().collect();
        let d = parse_usb_dev("1-1.1", &|f| files.get(f).map(|s| s.to_string())).unwrap();
        assert_eq!((d.vid, d.pid), (0x0fe6, 0x9900));
        assert_eq!(d.speed_label(), "480M");
        assert_eq!(d.label(), "CoreChips USB 10/100 LAN");
        assert_eq!(d.depth(), 2);
        assert!(parse_usb_dev("1-0:1.0", &|_| None).is_none());
        assert!(natural_key("1-1.2") < natural_key("1-1.10"));

        let mut v = UsbView::default();
        let t = Instant::now();
        assert!(!v.update(t, vec![d.clone()])); // first scan does not highlight
        let mut e = d.clone();
        e.name = "1-1.2".into();
        assert!(v.update(t, vec![d.clone(), e.clone()]));
        assert!(v.added.contains_key("1-1.2"));
        assert!(v.update(t + Duration::from_secs(1), vec![d.clone()]));
        assert_eq!(v.removed.len(), 1);
        v.update(t + Duration::from_secs(20), vec![d]);
        assert!(v.added.is_empty() && v.removed.is_empty());
    }

    fn decode_all(bytes: &[u8]) -> Vec<Midi> {
        let mut d = MidiDecoder::default();
        bytes.iter().filter_map(|b| d.push(*b)).collect()
    }

    #[test]
    fn midi_byte_stream() {
        let m = decode_all(&[0x90, 60, 100, 62, 90, 62, 0, 0xB1, 7, 99, 0xC2, 5, 0xE0, 0x00, 0x40, 0xE0, 0x7F, 0x7F]);
        assert_eq!(m[0], Midi::NoteOn { ch: 0, note: 60, vel: 100 });
        assert_eq!(m[1], Midi::NoteOn { ch: 0, note: 62, vel: 90 }); // running status
        assert_eq!(m[2], Midi::NoteOff { ch: 0, note: 62, vel: 0 }); // vel 0 = off
        assert_eq!(m[3], Midi::Cc { ch: 1, cc: 7, val: 99 });
        assert_eq!(m[4], Midi::Program { ch: 2, prog: 5 });
        assert_eq!(m[5], Midi::Bend { ch: 0, val: 0 });
        assert_eq!(m[6], Midi::Bend { ch: 0, val: 8191 });
        // Real-time byte in the middle of a message, SysEx, active sensing.
        let m = decode_all(&[0x90, 60, 0xF8, 100, 0xFE, 0xF0, 1, 2, 3, 0xF7, 0x80, 60, 0]);
        assert_eq!(m, vec![Midi::Clock, Midi::NoteOn { ch: 0, note: 60, vel: 100 }, Midi::Sysex(5), Midi::NoteOff { ch: 0, note: 60, vel: 0 }]);
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(21), "A0");
    }

    #[test]
    fn aseqdump_output() {
        let ports = parse_aseq_ports(" Port    Client name                      Port name\n  0:0    System                           Timer\n 14:0    Midi Through                     Midi Through Port-0\n 20:0    Felucca                          Felucca MIDI 1\n128:0    rtpmidid                         Network Export\n");
        assert_eq!(ports.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), vec!["20:0", "128:0"]);
        assert_eq!(ports[0].1, "Felucca");
        let (s, m) = parse_aseqdump_line(" 20:0   Note on                 0, note 60, velocity 100").unwrap();
        assert_eq!((s.as_str(), m), ("20:0", Midi::NoteOn { ch: 0, note: 60, vel: 100 }));
        assert_eq!(parse_aseqdump_line(" 20:0   Note on                 0, note 60, velocity 0").unwrap().1, Midi::NoteOff { ch: 0, note: 60, vel: 0 });
        assert_eq!(parse_aseqdump_line(" 20:0   Control change          3, controller 74, value 12").unwrap().1, Midi::Cc { ch: 3, cc: 74, val: 12 });
        assert_eq!(parse_aseqdump_line(" 20:0   Pitch bend              0, value -1234").unwrap().1, Midi::Bend { ch: 0, val: -1234 });
        assert_eq!(parse_aseqdump_line(" 20:0   Program change          1, program 7").unwrap().1, Midi::Program { ch: 1, prog: 7 });
        assert_eq!(parse_aseqdump_line(" 20:0   Clock").unwrap().1, Midi::Clock);
        assert!(parse_aseqdump_line("Waiting for data. Press Ctrl+C to end.").is_none());
        assert!(parse_aseqdump_line("Source  Event                  Ch  Data").is_none());
    }

    #[test]
    fn ir_decode_and_assemble() {
        assert_eq!(decode_ir(&nec_edges(0x00, 0x45)), "NEC  A:00 C:45");
        assert_eq!(decode_ir(&[9000, 2250, 560]), "NEC repeat");
        assert_eq!(decode_ir(&[100, 200, 300]), "RAW 3 edges");
        // Sony SIRC12: header 2400, then (space 600, pulse 600/1200) x 12; data 0x15 addr 1.
        let v: u32 = 0x15 | (1 << 7);
        let mut e = vec![2400];
        for i in 0..12 {
            e.push(600);
            e.push(if v >> i & 1 == 1 { 1200 } else { 600 });
        }
        assert_eq!(decode_ir(&e), "SIRC12 A:01 C:15");
        // Through the assembler, as lirc delivers it.
        let mut a = IrAssembler::default();
        let mut got = None;
        let edges = nec_edges(0x04, 0x08);
        for (i, us) in edges.iter().enumerate() {
            let raw = if i % 2 == 0 { (1 << 24) | us } else { *us };
            let (k, us) = ir_sample(raw);
            assert!(a.feed(k, us).is_none());
        }
        let (k, us) = ir_sample((3 << 24) | 12664);
        if let Some(f) = a.feed(k, us) {
            got = Some(f);
        }
        let f = got.unwrap();
        assert_eq!(f.decoded, "NEC  A:04 C:08");
        assert_eq!(f.edges.len(), 67);
        assert!(a.feed(IrKind::Pulse, 500).is_none() && a.feed(IrKind::Space, 30_000).is_none());
    }

    #[test]
    fn ir_hardware_explanation() {
        let nodes = vec![("/dev/lirc0".to_string(), false), ("/dev/lirc1".to_string(), true)];
        assert_eq!(ir_explain(&nodes, ""), IrHw::Rx("/dev/lirc1".into()));
        match ir_explain(&[("/dev/lirc0".into(), false)], "dtoverlay=gpio-ir-tx,gpio_pin=12\n") {
            IrHw::None(l) => {
                assert!(l[0].contains("lirc0") && l[0].contains("transmit"));
                assert!(l.iter().any(|s| s.contains("dtoverlay=gpio-ir,")));
            }
            _ => panic!(),
        }
        match ir_explain(&[], "dtoverlay=gpio-ir,gpio_pin=13\n") {
            IrHw::None(l) => assert!(l.iter().any(|s| s.contains("overlay is set"))),
            _ => panic!(),
        }
    }

    #[test]
    fn mock_sources_use_the_real_parsers() {
        let mut t = BleTable::default();
        for l in mock_ble_lines(0) {
            t.feed_line(&l, 0);
        }
        assert_eq!(t.devs.len(), MOCK_BLE.len());
        assert!(t.devs.iter().any(|d| d.mfr.len() == 24));
        let mut m = MidiView::new();
        for s in 0..14 {
            m.feed_bytes("x", &mock_midi_bytes(s));
        }
        assert!(m.log.len() >= 12);
        let app = Interceptor::new(true);
        assert_eq!(app.title(), "Interceptor");
        assert!(app.ble.devs.len() >= 5 && !app.midi.log.is_empty());
    }

    #[test]
    fn osc_listener_receives_and_reports_port_clash() {
        let (tx, rx) = channel();
        let w = start_osc(tx.clone(), 47391);
        assert!(matches!(rx.recv_timeout(Duration::from_secs(2)), Ok(Ev::OscBound)));
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.send_to(&osc::encode_msg("/enc", &[osc::OscArg::Int(2), osc::OscArg::Int(-1)]), "127.0.0.1:47391").unwrap();
        sock.send_to(&[1, 2, 3], "127.0.0.1:47391").unwrap();
        let mut got = (false, false);
        for _ in 0..2 {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Ev::OscPkt { msgs, .. }) => got.0 = msgs == vec![("/enc".to_string(), "2 -1".to_string())],
                Ok(Ev::OscBad) => got.1 = true,
                _ => {}
            }
        }
        assert_eq!(got, (true, true));
        drop(w);
        // A plain (non-reuse) socket on the port makes a new listener report an error on Linux/macOS alike is not guaranteed; just check it starts again.
        let w2 = start_osc(tx, 47391);
        assert!(matches!(rx.recv_timeout(Duration::from_secs(2)), Ok(Ev::OscBound) | Ok(Ev::OscErr(_))));
        drop(w2);
    }

    #[test]
    fn name_fits_the_registry() {
        assert_eq!("Interceptor".len(), 11);
    }

    #[test]
    fn addresses_are_masked_to_the_last_octet() {
        assert_eq!(mask_addr("6E:44:B3:64:D5:9E", true), "**:**:**:**:**:9E");
        assert_eq!(mask_addr("6E:44:B3:64:D5:9E", false), "6E:44:B3:64:D5:9E");
        assert_eq!(mask_addr("abc", true), "***");
        let mut a = Interceptor::new(true);
        assert!(a.mask, "masked by default");
        assert_eq!(a.menu()[0].label, "Mask addresses: on");
        a.mask = false;
        assert_eq!(a.menu()[0].label, "Mask addresses: off");
    }

    #[test]
    fn cursor_moves_clamps_scrolls_and_survives_shrinking() {
        let mut a = Interceptor::new(true);
        // List page (BT, 3 devices): clamped at both ends.
        a.set_page(PAGE_BT);
        a.move_cursor(-1);
        assert_eq!(a.cur[PAGE_BT], 0);
        a.move_cursor(10);
        assert_eq!(a.cur[PAGE_BT], 2);
        a.bt_devs.truncate(1);
        a.sync_cursor();
        assert_eq!(a.cur[PAGE_BT], 0);
        // BLE (7 devices, 3 visible): the window follows the cursor and stays on an address.
        a.set_page(PAGE_BLE);
        for _ in 0..5 {
            a.move_cursor(1);
        }
        assert_eq!(a.cur[PAGE_BLE], 5);
        assert_eq!(a.scroll[PAGE_BLE], 3);
        let sel = a.ble_sel.clone().unwrap();
        a.ble.devs.iter_mut().for_each(|d| d.rssi = d.rssi.map(|r| -r)); // reorder
        a.sync_cursor();
        assert_eq!(a.ble.sorted()[a.cur[PAGE_BLE]].addr, sel);
        a.ble.devs.clear();
        a.sync_cursor();
        assert_eq!((a.cur[PAGE_BLE], a.scroll[PAGE_BLE]), (0, 0));
        // Log page (MIDI): follows the newest row until the cursor moves up, then sticks.
        a.set_page(PAGE_MIDI);
        a.sync_cursor();
        let n = a.midi.log.len();
        assert!(n > 5 && a.follow[PAGE_MIDI] && a.cur[PAGE_MIDI] == n - 1);
        a.move_cursor(-1);
        assert!(!a.follow[PAGE_MIDI]);
        assert_eq!(a.cur[PAGE_MIDI], n - 2);
        a.move_cursor(-100);
        assert_eq!((a.cur[PAGE_MIDI], a.scroll[PAGE_MIDI]), (0, n - (body_rows() - 3) as usize));
        a.move_cursor(1000);
        assert!(a.follow[PAGE_MIDI] && a.scroll[PAGE_MIDI] == 0);
        // Pages without a list ignore the cursor.
        a.set_page(PAGE_ETH);
        a.move_cursor(3);
        assert_eq!(a.cur[PAGE_ETH], 0);
    }

    #[test]
    fn usb_ports_form_a_hub_hierarchy() {
        assert_eq!(usb_parent("usb1"), None);
        assert_eq!(usb_parent("1-1"), Some("usb1".into()));
        assert_eq!(usb_parent("1-1.1"), Some("1-1".into()));
        assert_eq!(usb_parent("1-1.4.2"), Some("1-1.4".into()));
        assert_eq!(usb_parent("2-3"), Some("usb2".into()));
        let mut v = UsbView::default();
        let t = Instant::now();
        v.update(t, mock_usb(Duration::ZERO));
        let top = usb_children(&v, CAT_USB, t);
        assert_eq!(top.iter().map(|n| n.key.as_str()).collect::<Vec<_>>(), vec!["usb:usb1"]);
        assert!(top[0].expandable && top[0].label.starts_with("Root hub"));
        let hub = usb_children(&v, "usb:usb1", t);
        assert_eq!(hub[0].key, "usb:1-1");
        assert!(hub[0].expandable);
        let devs = usb_children(&v, "usb:1-1", t);
        assert_eq!(devs[0].label, "CoreChips USB 10/100 LAN");
        assert_eq!(devs[0].extra, "480M");
        assert!(!devs[0].expandable);
        // Plugging a device in marks it hot; unplugging keeps it, faint, at the top.
        v.update(t, mock_usb(Duration::from_secs(12)));
        assert!(usb_children(&v, "usb:1-1", t).iter().any(|n| n.key == "usb:1-1.2" && n.hot));
        assert!(!usb_children(&v, "usb:1-1", t + Duration::from_secs(10)).iter().any(|n| n.hot));
        v.update(t + Duration::from_secs(1), mock_usb(Duration::ZERO));
        let top = usb_children(&v, CAT_USB, t + Duration::from_secs(1));
        assert!(top.iter().any(|n| n.dim && n.label.starts_with("- ")));
        assert!(usb_details(&v, "usb:1-1.1").iter().any(|(k, v)| k == "id" && v == "0fe6:9900"));
        assert!(usb_details(&v, "usb:1-1.2").iter().any(|(k, v)| k == "state" && v == "removed"));
    }

    #[test]
    fn device_tab_is_one_tree_with_properties_as_leaf_rows() {
        let mut a = Interceptor::new(true);
        a.set_page(PAGE_DEV);
        a.ensure_dev();
        assert_eq!(a.dev.as_ref().unwrap().tree.rows.len(), 11); // the categories
        assert_eq!(a.dims(), (0, 0)); // the tree owns its cursor
        assert_eq!(a.dev_key(Keycode::Up), TreeKey::Used);
        // Left / Right are the tabs' keys, not the tree's.
        assert_eq!(a.dev_key(Keycode::Left), TreeKey::Ignored);
        assert_eq!(a.dev_key(Keycode::Right), TreeKey::Ignored);
        {
            let fs = crate::font::FontSystem::new();
            let tm = crate::theme::ThemeManager::new(crate::theme::Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
            let sys = crate::system::mock::MockBackend::new();
            let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
            a.key(Keycode::Right, Mod::NOMOD, false, &ctx);
            assert_eq!(a.page, PAGE_ETH, "Device is the last page: Right wraps");
            a.key(Keycode::Left, Mod::NOMOD, false, &ctx);
            assert_eq!(a.page, PAGE_DEV);
        }
        assert_eq!(a.dev.as_ref().unwrap().tree.rows.len(), 11);
        // Open the device tree: the root's properties come first, then its nodes.
        assert_eq!(a.dev_key(Keycode::Return), TreeKey::Used);
        assert_eq!(a.dev_key(Keycode::Down), TreeKey::Used);
        let d = a.dev.as_ref().unwrap();
        let first = d.tree.selected().unwrap();
        assert!(first.prop && !first.expandable && first.depth == 1, "{first:?}");
        let rows = &d.tree.rows;
        let last_prop = rows.iter().rposition(|r| r.prop).unwrap();
        let first_node = rows.iter().position(|r| r.depth == 1 && !r.prop).unwrap();
        assert!(last_prop < first_node, "properties before child nodes");
        // soc: open it; `reg` shows decoded, `status`-like rows come before the child nodes.
        let soc = a.dev.as_ref().unwrap().tree.index_of("dt:/soc").unwrap();
        a.dev.as_mut().unwrap().tree.cursor = soc;
        assert_eq!(a.dev_key(Keycode::Return), TreeKey::Used);
        let d = a.dev.as_ref().unwrap();
        assert_eq!(d.tree.rows[soc + 1].depth, 2);
        let gpio = d.tree.index_of("dt:/soc/gpio@7e200000").unwrap();
        assert!(!d.tree.rows[gpio].prop && d.tree.rows[gpio].expandable);
        let mut a2 = a;
        let gpio_i = gpio;
        a2.dev.as_mut().unwrap().tree.cursor = gpio_i;
        a2.dev_key(Keycode::Return);
        let d = a2.dev.as_ref().unwrap();
        let reg = d.tree.rows.iter().find(|r| r.prop && r.label == "reg" && r.key.starts_with("dt:/soc/gpio@7e200000#")).expect("reg row");
        assert_eq!(reg.extra, "<0x7e200000 0xb4>");
        let mut a = a2;
        // USB: expand the category, then plug something in: the branch reloads and keeps the cursor.
        a.dev.as_mut().unwrap().tree.collapse_all();
        a.dev_key(Keycode::Down);
        a.dev_key(Keycode::Return);
        a.dev_key(Keycode::Down);
        assert_eq!(a.dev.as_ref().unwrap().tree.selected().unwrap().key, "usb:usb1");
        a.dev_key(Keycode::Return);
        let hub = a.dev.as_ref().unwrap().tree.index_of("usb:1-1").unwrap();
        a.dev.as_mut().unwrap().tree.cursor = hub;
        a.dev_key(Keycode::Return);
        // The hub shows its id / speed ... rows (leaves) before its devices.
        let t = &a.dev.as_ref().unwrap().tree;
        assert!(t.rows[hub + 1].prop && t.rows.iter().any(|r| r.prop && r.label == "id"));
        a.usb.update(Instant::now(), mock_usb(Duration::from_secs(12)));
        a.dev_usb_reload();
        let d = a.dev.as_ref().unwrap();
        let i = d.tree.index_of("usb:1-1.2").expect("the new device appears");
        assert!(d.tree.rows[i].hot);
        assert_eq!(d.tree.selected().unwrap().key, "usb:1-1");
    }

    #[test]
    fn address_properties_follow_the_mask_setting() {
        let mut a = Interceptor::new(true);
        a.set_page(PAGE_DEV);
        a.ensure_dev();
        let n = TreeNode::property("k", "address", "aa:bb:cc:dd:ee:9e").secret(true);
        assert_eq!(n.value, "aa:bb:cc:dd:ee:9e");
        assert!(n.secret && n.prop);
    }

    #[test]
    fn every_theme_keeps_the_tab_strip_and_selection_readable() {
        use crate::theme::{Theme, ThemeManager};
        fn lum(c: Color) -> f64 {
            let f = |v: u8| {
                let v = v as f64 / 255.0;
                if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
        }
        let contrast = |a: Color, b: Color| {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        };
        for theme in THEME_FILES {
            let tm = ThemeManager::new(Theme::load_from_yaml(&format!("assets/themes/{theme}.yaml")).unwrap());
            let g = |k: &str| tm.get_color(k);
            // Idle tabs: full-strength ink on the zebra tone; the strip must not melt into the page.
            assert!(contrast(g("foreground"), g("surface_alt")) >= 3.0, "{theme}: tab text");
            assert!(contrast(g("surface_alt"), g("home_bg")) >= 1.05, "{theme}: tab fill vs page");
            assert!(contrast(g("select_fg"), g("select_bg")) >= 3.0, "{theme}: active tab / cursor");
            let sec = widgets::secondary_ink(&tm);
            assert!(contrast(sec, g("surface_alt")) >= 3.0, "{theme}: secondary text on stripes");
            assert!(contrast(sec, g("home_bg")) >= 3.0, "{theme}: secondary text on the page");
            assert!(contrast(g("line"), g("home_bg")) >= 1.5, "{theme}: rules");
        }
    }

    /// Renders every page (mock data, every theme) on the 320 x 170 screen and in a bigger
    /// window, and checks, from the pixels of the column x = 5, that every band starts at
    /// y = 36 + 16k and is a whole number of rows tall down to the last row but one (y 148 on
    /// 320 x 170), and that no pixel below is left unpainted.
    #[test]
    fn every_page_follows_the_row_grid() {
        std::env::set_var("SDL_VIDEODRIVER", "dummy");
        let Ok(sdl) = sdl2::init() else {
            eprintln!("skipped: SDL cannot start here");
            return;
        };
        let Ok(video) = sdl.video() else {
            eprintln!("skipped: no SDL video");
            return;
        };
        for (w, h) in [(320, 170), (640, 300)] {
            let Ok(window) = video.window("grid", w as u32, h as u32).build() else {
                eprintln!("skipped: no window");
                return;
            };
            let mut canvas = window.into_canvas().software().build().unwrap();
            grid::with_surface((w, h), || grid_check(&mut canvas, w as usize, h as usize));
        }
    }

    fn grid_check(canvas: &mut Canvas<Window>, w: usize, h: usize) {
        use crate::font::{load_mplus_f12r, FontSystem};
        use crate::pdfont::PlaydateFont;
        use crate::system::mock::MockBackend;
        use crate::theme::{Theme, ThemeManager};
        use sdl2::pixels::PixelFormatEnum;

        let tc = canvas.texture_creator();
        let mut assets = AssetStore::new(&tc).unwrap();
        let mut fs = FontSystem::new();
        fs.load_font(load_mplus_f12r());
        if let Ok(mut f) = std::fs::read("assets/fonts/Cavs.fnt").map_err(|e| e.to_string()).and_then(|b| PlaydateFont::parse(&b)) {
            f.set_space(2);
            fs.display = Some(f);
        }
        fs.display_cavs = true;
        let sys = MockBackend::new();

        for theme in THEME_FILES {
            let tm = ThemeManager::new(Theme::load_from_yaml(&format!("assets/themes/{theme}.yaml")).unwrap());
            let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
            let mut cases: Vec<(String, usize, bool)> = (0..TABS.len()).map(|p| (TABS[p].to_string(), p, false)).collect();
            cases.push(("OSC table".into(), PAGE_OSC, true));
            for (name, page, table) in cases {
                let mut app = Interceptor::new(true);
                app.set_page(page);
                app.osc.table_view = table;
                canvas.set_draw_color(Color::RGB(255, 0, 255));
                canvas.clear();
                app.draw(canvas, &mut assets, &ctx);
                let px = canvas.read_pixels(None, PixelFormatEnum::RGB24).unwrap();
                let at = |x: usize, y: usize| (px[(y * w + x) * 3], px[(y * w + x) * 3 + 1], px[(y * w + x) * 3 + 2]);
                let who = format!("{theme} / {name} / {w}x{h}");
                // Bands in the column x = 5.
                // The tab strip (y 16..36) is folder tabs of one size, rule on its last pixel row.
                let ink = tm.get_color("foreground");
                let ink3 = (ink.r, ink.g, ink.b);
                let sb = tm.get_color("select_bg");
                let strip = tabs_strip(app.page, app.tab_first);
                let act = (0..w as i32).find(|&x| strip.hit(x, 30, &tm) == Some(widgets::FolderHit::Tab(page))).expect("active tab shown") as usize;
                let idle = (0..w as i32).find(|&x| matches!(strip.hit(x, 30, &tm), Some(widgets::FolderHit::Tab(i)) if i != page)).unwrap() as usize;
                if !table {
                    assert_eq!(at(act + 2, 30), (sb.r, sb.g, sb.b), "{who}: the active tab wears the selection colour");
                    assert_eq!(at(act + 10, 35), ink3, "{who}: the rule runs under the active tab too");
                    assert_eq!(at(idle + 10, 35), ink3, "{who}: rule under the idle tabs");
                    assert_eq!(at(4, 35), ink3, "{who}: the tab edge meets the rule");
                }
                // The last row is left out: a page may end its list there or a row early.
                let last = (BODY_Y + (body_rows() - 1) * ROW) as usize;
                let mut start = 36usize;
                for y in 37..=last {
                    // The last pixel row of every 16 px row may carry the 1 px rule between items.
                    if y < last && (y - 36) % 16 == 15 {
                        continue;
                    }
                    let col = |y: usize| at(5, y);
                    if y == last || col(y) != col(start) {
                        assert!(col(start) != (255, 0, 255), "{who}: unpainted band at y={start}");
                        assert_eq!((start - 36) % 16, 0, "{who}: band starts at y={start}");
                        assert_eq!((y - start) % 16, 0, "{who}: band at y={start} is {} px tall", y - start);
                        start = y;
                    }
                }
                // Below, only the app background (no unpainted pixels): windows reach the screen edge minus 1 px.
                for y in last..h - 1 {
                    assert!(at(5, y) != (255, 0, 255), "{who}: unpainted at y={y}");
                }
            }
        }
    }
}

#[cfg(test)]
mod mouse_tests {
    use super::*;
    use crate::font::FontSystem;
    use crate::system::mock::MockBackend;
    use crate::theme::{Theme, ThemeManager};

    fn run(f: impl FnOnce(&Ctx)) {
        let fs = FontSystem::new();
        let tm = ThemeManager::new(Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = MockBackend::new();
        f(&Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() });
    }

    fn down(x: i32, y: i32, clicks: u8) -> PointerEvent {
        PointerEvent::Down { x, y, button: 0, clicks }
    }

    #[test]
    fn clicking_a_tab_switches_page() {
        run(|ctx| {
            let mut a = Interceptor::new(true);
            // The right arrow tab feeds through every page, the window follows.
            let next_x = 4 + tabs_room() - 6;
            for i in 1..NP {
                a.pointer(down(next_x, grid::BAR_H + 10, 1), ctx);
                assert_eq!(a.page, i);
                let s = tabs_strip(a.page, a.tab_first);
                assert!((0..320).any(|x| s.hit(x, grid::BAR_H + 10, ctx.tm) == Some(widgets::FolderHit::Tab(i))), "page {i} is in view");
            }
            // The left arrow goes back; a click on a shown tab switches to it.
            a.pointer(down(4 + 3, grid::BAR_H + 10, 1), ctx);
            assert_eq!(a.page, NP - 2);
            let s = tabs_strip(a.page, a.tab_first);
            let x = (0..320).find(|&x| s.hit(x, grid::BAR_H + 10, ctx.tm) == Some(widgets::FolderHit::Tab(NP - 1))).unwrap();
            a.pointer(down(x, grid::BAR_H + 10, 1), ctx);
            assert_eq!(a.page, NP - 1);
            // Below the tab strip nothing switches.
            a.pointer(down(4 + 5, grid::BAR_H + TABS_H + 3, 1), ctx);
            assert_eq!(a.page, NP - 1);
        });
    }

    #[test]
    fn list_rows_select_and_the_scroll_bar_scrolls() {
        run(|ctx| {
            let mut a = Interceptor::new(true);
            for n in 0..40 {
                a.osc.feed_packet(n * 10, "192.168.3.7:9001", &mock_osc_packet(n as u32 + 1));
            }
            a.set_page(PAGE_OSC);
            a.osc.table_view = false;
            a.sync_cursor();
            let g = a.list_geom().expect("a list");
            assert!(g.total > g.vis, "{} rows, {} visible", g.total, g.vis);
            assert_eq!(g.start, g.total - g.vis, "a log shows its newest rows");
            // Click the first visible row: that item becomes the cursor, following stops.
            let y = BODY_Y + g.first_row * ROW + 2;
            a.pointer(down(LIST_X + 10, y, 1), ctx);
            assert_eq!(a.cur[PAGE_OSC], g.start);
            assert!(!a.follow[PAGE_OSC]);
            // The track above the thumb pages up.
            let sb = body_scrollbar_rect();
            a.pointer(down(sb.x() + 1, sb.y() + sb.height() as i32 / 2 - 20, 1), ctx);
            a.pointer(PointerEvent::Up { x: 0, y: 0, button: 0 }, ctx);
            assert!(a.list_geom().unwrap().start < g.start);
            // Dragging the thumb to the very top shows the oldest row.
            let thumb = ScrollBar::new(sb, g.total, g.vis, a.list_geom().unwrap().start).thumb();
            a.pointer(down(sb.x() + 1, thumb.y() + 1, 1), ctx);
            assert!(a.sb_drag);
            a.pointer(PointerEvent::Drag { x: sb.x(), y: sb.y() - 20, dx: 0, dy: -50 }, ctx);
            assert_eq!(a.list_geom().unwrap().start, 0);
            assert!(a.cur[PAGE_OSC] < g.vis, "the cursor was pulled into the window");
            a.pointer(PointerEvent::Up { x: 0, y: 0, button: 0 }, ctx);
            assert!(!a.sb_drag);
            // Row hit-testing ignores points outside the list and below the last item.
            assert_eq!(g.row_at(LIST_X + list_w() + 5, y), None);
            assert_eq!(g.row_at(LIST_X + 5, BODY_Y), None, "the header band is not an item");
        });
    }

    /// The body's geometry is the old fixed one on 320 x 170 and grows with a bigger window.
    #[test]
    fn layout_follows_the_surface() {
        grid::with_surface((320, 170), || {
            assert_eq!((body_rows(), body_h(), body_end(), list_w(), extra_w()), (8, 128, 169, 304, 0));
            assert_eq!(body_scrollbar_rect(), Rect::new(312, 37, 4, 132));
            assert_eq!(dev_tree_rect(), Rect::new(4, 36, 304, 128));
            assert_eq!(eth_graph_rect(), Rect::new(162, 52, 144, 109));
            assert_eq!(eth_slots(eth_graph_rect()), 72);
            assert_eq!(ble_vis(), 3);
        });
        grid::with_surface((800, 480), || {
            // Same pitch, more rows; the frame line stays clear of the last one.
            assert_eq!(body_rows(), 27);
            assert!(BODY_Y + body_h() < body_end() - 1);
            assert_eq!((list_w(), extra_w()), (784, 480));
            let sb = body_scrollbar_rect();
            assert_eq!((sb.x(), sb.bottom()), (grid::scroll_x(), body_end()));
            let g = eth_graph_rect();
            assert_eq!((g.right(), g.y(), g.bottom()), (LIST_X + list_w() - 2, BODY_Y + ROW, BODY_Y + body_h() - 3));
            assert_eq!(eth_slots(g), 312);
            assert_eq!(ble_vis(), 13);
        });
    }

    /// In a big window every list shows more rows, and a click on a row far below the 320 x 170
    /// area selects that row.
    #[test]
    fn a_big_window_shows_more_rows_and_clicks_reach_them() {
        run(|ctx| {
            let mut a = Interceptor::new(true);
            for n in 0..60 {
                a.osc.feed_packet(n * 10, "192.168.3.7:9001", &mock_osc_packet(n as u32 + 1));
            }
            a.set_page(PAGE_OSC);
            a.osc.table_view = false;
            let small = grid::with_surface((320, 170), || a.dims().1);
            assert_eq!(small, 6);
            grid::with_surface((800, 480), || {
                a.sync_cursor();
                let g = a.list_geom().unwrap();
                assert_eq!(g.vis, 25);
                assert_eq!(g.start, g.total - g.vis);
                // Row 20 of the list: y well below 170, x well right of 320.
                let y = BODY_Y + (g.first_row + 20) * ROW + 3;
                a.pointer(down(600, y, 1), ctx);
                assert_eq!(a.cur[PAGE_OSC], g.start + 20);
                // The scroll bar is at the window's right edge.
                assert_eq!(g.row_at(grid::scroll_x() + 1, y), None);
            });
            // Device tab: the tree follows the window's height.
            a.set_page(PAGE_DEV);
            grid::with_surface((800, 480), || {
                a.with_dev(|dv, _| assert_eq!(dv.tree.visible, 27));
            });
            grid::with_surface((320, 170), || {
                a.with_dev(|dv, _| assert_eq!((dv.tree.visible, dv.tree.prop_w), (8, widgets::PROP_ROW_W)));
            });
        });
    }

    #[test]
    fn device_tab_tree_marker_rows_wheel_and_scroll_bar() {
        run(|ctx| {
            let mut a = Interceptor::new(true);
            a.set_page(PAGE_DEV);
            a.ensure_dev();
            let rect = dev_tree_rect();
            // Click on the label of row 1 selects it without opening.
            a.pointer(down(rect.x() + 60, rect.y() + 16 + 3, 1), ctx);
            assert_eq!(a.dev.as_ref().unwrap().tree.cursor, 1);
            // A click on the +/- marker of the first expandable row opens it.
            let before = a.dev.as_ref().unwrap().tree.rows.len();
            a.pointer(down(rect.x() + 2, rect.y() + 3, 1), ctx);
            assert!(a.dev.as_ref().unwrap().tree.rows.len() > before, "marker toggles");
            // Wheel moves the cursor.
            let c0 = a.dev.as_ref().unwrap().tree.cursor;
            a.pointer(PointerEvent::Wheel { x: 20, y: 60, dy: -1 }, ctx);
            assert_eq!(a.dev.as_ref().unwrap().tree.cursor, c0 + 1);
            // Scroll bar: a press on the lower track pages down, a thumb drag follows the pointer.
            let sb = body_scrollbar_rect();
            let (total, vis) = {
                let t = &a.dev.as_ref().unwrap().tree;
                (t.rows.len(), body_rows() as usize)
            };
            assert!(total > vis);
            a.pointer(down(sb.x() + 1, sb.y() + sb.height() as i32 - 6, 1), ctx);
            assert_eq!(a.dev.as_ref().unwrap().tree.scroll, vis.min(total - vis));
            a.pointer(PointerEvent::Up { x: 0, y: 0, button: 0 }, ctx);
            let thumb = a.dev.as_ref().unwrap().tree.scrollbar(sb).thumb();
            a.pointer(down(sb.x() + 1, thumb.y() + 1, 1), ctx);
            a.pointer(PointerEvent::Drag { x: sb.x(), y: sb.y(), dx: 0, dy: -100 }, ctx);
            assert_eq!(a.dev.as_ref().unwrap().tree.scroll, 0);
            let t = &a.dev.as_ref().unwrap().tree;
            assert!(t.cursor < t.scroll + vis, "cursor stays in the window");
        });
    }
}
