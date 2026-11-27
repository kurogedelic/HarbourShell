//! Interceptor's LAN page: who is on the network the device is connected to, and which common
//! TCP ports one of them has open.
//!
//! This is the only part of Harbour that sends traffic to other machines, so it is fenced in:
//! * Only the connected interface's own private subnet (RFC 1918 / link-local) is ever probed.
//!   Every probe goes through [`Scope`], which asks `LanInfo::allows` first; there is no other
//!   way in this file to open a socket to a peer.
//! * At most 50 probes a second (`Scope` paces them), short timeouts, cancel with Esc.
//! * A first-use notice ("Scan only networks you own or manage") has to be accepted with Return.
//! * No exploits, no credentials, nothing is read from a connection: a port counts as open when
//!   the TCP handshake completes and the connection is closed again.
//!
//! Discovery: a NetBIOS name query to every host of the subnet (which also makes the kernel ARP
//! them), then the neighbour table from the system backend, then a unicast mDNS / reverse DNS
//! lookup for the devices found. Who was seen first and last is kept in
//! `~/.local/share/harbour/lan-known.tsv`; a device that was never seen before is flagged NEW
//! (not on the very first scan, which only sets the baseline).

use super::*;
use crate::system::lan::{self, LanInfo, Neighbor};
use std::cell::Cell;
use std::io::Read as _;
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::path::PathBuf;

/// Minimum gap between two probes: 20 ms = at most 50 a second.
pub(super) const PROBE_GAP: Duration = Duration::from_millis(20);
/// A host of the local network answers a handshake within a few ms.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
/// What the first-use notice says.
pub(super) const NOTICE: &str = "Scan only networks you own or manage";

// ---------------------------------------------------------------------- the fence

/// The only way to send a probe: refuses every address `info` does not allow, spaces the probes
/// out and gives up when the user cancelled.
pub(super) struct Scope {
    info: LanInfo,
    stop: Arc<AtomicBool>,
    last: Cell<Instant>,
}

impl Scope {
    pub fn new(info: LanInfo, stop: Arc<AtomicBool>) -> Self {
        Self { info, stop, last: Cell::new(Instant::now() - PROBE_GAP) }
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Wait until the next probe may go; false when cancelled.
    fn pace(&self) -> bool {
        let el = self.last.get().elapsed();
        if el < PROBE_GAP {
            sleep_unless(&self.stop, ((PROBE_GAP - el).as_millis() as u64).max(1));
        }
        self.last.set(Instant::now());
        !self.stopped()
    }

    /// One datagram to `ip:port`. false = refused by the fence, cancelled or not sent.
    pub fn udp_send(&self, sock: &UdpSocket, ip: Ipv4Addr, port: u16, data: &[u8]) -> bool {
        self.info.allows(ip).is_ok() && self.pace() && sock.send_to(data, SocketAddr::V4(SocketAddrV4::new(ip, port))).is_ok()
    }

    /// Does `ip:port` complete a TCP handshake? None = refused or cancelled (nothing was sent).
    /// The connection is closed at once; nothing is written or read.
    pub fn tcp_open(&self, ip: Ipv4Addr, port: u16) -> Option<bool> {
        if self.info.allows(ip).is_err() || !self.pace() {
            return None;
        }
        Some(TcpStream::connect_timeout(&SocketAddr::V4(SocketAddrV4::new(ip, port)), CONNECT_TIMEOUT).is_ok())
    }

    /// May `ip` be looked up / probed at all (for lookups that do not send to the peer).
    pub fn permits(&self, ip: Ipv4Addr) -> bool {
        self.info.allows(ip).is_ok() && self.pace()
    }
}

// ------------------------------------------------------------------------ data

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Device {
    pub ip: Option<Ipv4Addr>,
    pub mac: String,
    pub name: Option<String>,
    pub is_self: bool,
    pub is_router: bool,
    pub first: u64,
    pub last: u64,
    pub new: bool,
    pub online: bool,
    /// Open ports of the last check of this device (None = not checked yet).
    pub open: Option<Vec<u16>>,
}

impl Device {
    pub fn vendor(&self) -> String {
        if self.mac.is_empty() {
            "-".into()
        } else {
            lan::oui_vendor(&self.mac)
        }
    }

    /// What the list shows: the name, else the maker, else "Unknown".
    pub fn title(&self) -> String {
        match &self.name {
            Some(n) => n.clone(),
            None => {
                let v = self.vendor();
                // A raw prefix (maker not in the table) tells nobody anything.
                i18n::tr_s(if v.contains(':') || v == "-" { "Unknown" } else { &v })
            }
        }
    }

    fn sort_key(&self) -> (bool, u32, String) {
        (!self.online, self.ip.map_or(u32::MAX, u32::from), self.mac.clone())
    }
}

/// Devices seen on earlier scans: MAC -> (first seen, last seen, name), kept in a small tab
/// separated file.
pub(super) struct Known {
    path: Option<PathBuf>,
    pub map: BTreeMap<String, (u64, u64, String)>,
    /// The file did not exist (or was empty): the first scan only fills it, nothing is "new".
    pub baseline: bool,
}

impl Known {
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut map = BTreeMap::new();
        if let Some(p) = &path {
            for l in std::fs::read_to_string(p).unwrap_or_default().lines() {
                let f: Vec<&str> = l.split('\t').collect();
                if let (Some(mac), Some(a), Some(b)) = (f.first().and_then(|m| lan::norm_mac(m)), f.get(1).and_then(|s| s.parse().ok()), f.get(2).and_then(|s| s.parse().ok())) {
                    map.insert(mac, (a, b, f.get(3).unwrap_or(&"").to_string()));
                }
            }
        }
        let baseline = map.is_empty();
        Self { path, map, baseline }
    }

    pub fn save(&self) {
        let Some(p) = &self.path else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let text: String = self.map.iter().map(|(m, (a, b, n))| format!("{m}\t{a}\t{b}\t{}\n", n.replace(['\t', '\n'], " "))).collect();
        let _ = std::fs::write(p, text);
    }

    /// Mock devices as if the scan had run a few times before: all known except the last one.
    pub fn mock(now: u64) -> Self {
        let mut map = BTreeMap::new();
        for (i, (_, mac, name, _)) in crate::system::lan::MOCK_DEVICES.iter().enumerate() {
            if i + 1 < crate::system::lan::MOCK_DEVICES.len() {
                map.insert((*mac).to_string(), (now - (40 - i as u64 * 5) * 86_400, now - 3600 * i as u64, (*name).to_string()));
            }
        }
        // One that was around two days ago and is not now.
        map.insert("f0:18:98:aa:bb:01".into(), (now - 90 * 86_400, now - 2 * 86_400 - 600, "iPad".into()));
        Self { path: None, map, baseline: false }
    }
}

/// The ports to check: the short list, 1-1024, or a typed range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PortSet {
    Common,
    Wide,
    Custom,
}

pub(super) struct PortScan {
    pub set: PortSet,
    pub custom: Vec<u16>,
    /// The range being typed (Some while editing).
    pub edit: Option<String>,
    pub run: u32,
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub open: Vec<u16>,
    pub target: Option<Ipv4Addr>,
    pub err: Option<String>,
    pub last_mock: Instant,
}

impl PortScan {
    fn new() -> Self {
        Self { set: PortSet::Common, custom: Vec::new(), edit: None, run: 0, running: false, done: 0, total: 0, open: Vec::new(), target: None, err: None, last_mock: Instant::now() }
    }

    pub fn ports(&self) -> Vec<u16> {
        match self.set {
            PortSet::Common => lan::COMMON_PORTS.to_vec(),
            PortSet::Wide => (1..=1024).collect(),
            PortSet::Custom => self.custom.clone(),
        }
    }
}

pub(super) enum LanEv {
    /// Sweep progress: run, hosts done, hosts in all.
    Sweep(u32, usize, usize),
    /// The sweep is over: the names NetBIOS gave.
    Swept(u32, HashMap<Ipv4Addr, String>),
    Name(u32, Ipv4Addr, String),
    /// Name lookups progress.
    Names(u32, usize, usize),
    Done(u32),
    Err(u32, String),
    PortOpen(u32, u16),
    PortProgress(u32, usize, usize),
    PortDone(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Phase {
    Sweep,
    Names,
}

pub(super) struct LanView {
    pub init: bool,
    pub accepted: bool,
    pub info: Option<LanInfo>,
    pub own_mac: Option<String>,
    pub devices: Vec<Device>,
    pub known: Known,
    pub run: u32,
    pub scanning: bool,
    pub phase: Phase,
    pub done: usize,
    pub total: usize,
    pub err: Option<String>,
    /// The device view (ports) of the device under the cursor instead of the list.
    pub detail: bool,
    pub pscan: PortScan,
    /// Names found by the sweep, waiting for the neighbour table.
    pub swept: Option<HashMap<Ipv4Addr, String>>,
    pub names: HashMap<Ipv4Addr, String>,
    pub last_mock: Instant,
}

impl LanView {
    pub fn new() -> Self {
        Self {
            init: false,
            accepted: false,
            info: None,
            own_mac: None,
            devices: Vec::new(),
            known: Known { path: None, map: BTreeMap::new(), baseline: true },
            run: 0,
            scanning: false,
            phase: Phase::Sweep,
            done: 0,
            total: 0,
            err: None,
            detail: false,
            pscan: PortScan::new(),
            swept: None,
            names: HashMap::new(),
            last_mock: Instant::now(),
        }
    }

    pub fn new_count(&self) -> usize {
        self.devices.iter().filter(|d| d.new).count()
    }

    pub fn online_count(&self) -> usize {
        self.devices.iter().filter(|d| d.online).count()
    }
}

fn data_file(name: &str) -> PathBuf {
    sensor::sensor_dir().join(name)
}

/// Fold what a scan found into the list: neighbours the fence allows, the device itself, names,
/// first / last seen and the NEW flag. Devices only known from earlier scans stay as offline rows.
pub(super) fn merge(devs: &mut Vec<Device>, nbs: &[Neighbor], info: &LanInfo, own_mac: Option<&str>, names: &HashMap<Ipv4Addr, String>, known: &mut Known, now: u64) {
    for d in devs.iter_mut() {
        d.online = false;
    }
    let mut seen: Vec<(Ipv4Addr, String)> = nbs.iter().filter(|n| info.allows(n.ip).is_ok()).map(|n| (n.ip, n.mac.clone())).collect();
    if let Some(m) = own_mac.and_then(lan::norm_mac) {
        seen.retain(|(ip, mac)| *ip != info.ip && *mac != m);
        seen.push((info.ip, m));
    }
    for (ip, mac) in seen {
        let is_self = ip == info.ip;
        let idx = match devs.iter().position(|d| d.mac == mac) {
            Some(i) => i,
            None => {
                let was_known = known.map.contains_key(&mac);
                devs.push(Device { ip: Some(ip), mac: mac.clone(), name: None, is_self, is_router: false, first: known.map.get(&mac).map_or(now, |k| k.0), last: now, new: !was_known && !known.baseline && !is_self, online: true, open: None });
                devs.len() - 1
            }
        };
        let d = &mut devs[idx];
        d.ip = Some(ip);
        d.online = true;
        d.is_self = is_self;
        d.is_router = info.gateway == Some(ip);
        d.last = now;
        if let Some(n) = names.get(&ip) {
            d.name = Some(n.clone());
        } else if d.name.is_none() {
            d.name = known.map.get(&mac).map(|k| k.2.clone()).filter(|n| !n.is_empty());
        }
        let e = known.map.entry(mac).or_insert((now, now, String::new()));
        e.1 = now;
        if let Some(n) = &d.name {
            e.2 = n.clone();
        }
    }
    // Devices of earlier scans that did not answer now.
    for (mac, (first, last, name)) in &known.map {
        if !devs.iter().any(|d| &d.mac == mac) {
            devs.push(Device { ip: None, mac: mac.clone(), name: Some(name.clone()).filter(|n| !n.is_empty()), is_self: false, is_router: false, first: *first, last: *last, new: false, online: false, open: None });
        }
    }
    devs.sort_by_key(Device::sort_key);
}

// ---------------------------------------------------------------------- workers

/// Read the replies waiting on the (non-blocking) socket.
fn drain(sock: &UdpSocket, names: &mut HashMap<Ipv4Addr, String>) {
    let mut buf = [0u8; 1500];
    while let Ok((n, from)) = sock.recv_from(&mut buf) {
        if let SocketAddr::V4(a) = from {
            if let Some(name) = lan::parse_nbstat(&buf[..n]) {
                names.insert(*a.ip(), name);
            }
        }
    }
}

/// The sweep: one NetBIOS name query to each host of the subnet, 50 a second at most. It makes the
/// kernel ARP every host (so the neighbour table is full afterwards) and collects the names the
/// Windows / Samba machines give. Ends with `LanEv::Swept`.
pub(super) fn start_sweep(tx: Sender<Ev>, run: u32, info: LanInfo) -> Worker {
    let w = Worker::new();
    let stop = w.stop.clone();
    std::thread::spawn(move || {
        let hosts = info.sweep_hosts();
        let total = hosts.len();
        let scope = Scope::new(info, stop.clone());
        let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
            let _ = tx.send(Ev::Lan(LanEv::Err(run, "Could not open a socket".into())));
            return;
        };
        let _ = sock.set_nonblocking(true);
        let mut names = HashMap::new();
        let q = lan::nbstat_query();
        for (i, h) in hosts.iter().enumerate() {
            if scope.stopped() {
                return;
            }
            scope.udp_send(&sock, *h, 137, &q);
            drain(&sock, &mut names);
            if i % 8 == 7 {
                let _ = tx.send(Ev::Lan(LanEv::Sweep(run, i + 1, total)));
            }
        }
        // Late answers and the last ARP replies.
        for _ in 0..12 {
            sleep_unless(&stop, 100);
            drain(&sock, &mut names);
        }
        if !stop.load(Ordering::SeqCst) {
            let _ = tx.send(Ev::Lan(LanEv::Swept(run, names)));
        }
    });
    w
}

/// Names for the devices that have none yet: a unicast mDNS pointer question, then reverse DNS
/// (`getent hosts`, which asks the configured resolver, not the device).
pub(super) fn start_names(tx: Sender<Ev>, run: u32, info: LanInfo, ips: Vec<Ipv4Addr>) -> Worker {
    let w = Worker::new();
    let stop = w.stop.clone();
    std::thread::spawn(move || {
        let scope = Scope::new(info, stop.clone());
        let total = ips.len();
        let sock = UdpSocket::bind("0.0.0.0:0").ok();
        if let Some(s) = &sock {
            let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
        }
        for (i, ip) in ips.iter().enumerate() {
            if scope.stopped() {
                return;
            }
            let mut name = None;
            if let Some(s) = &sock {
                if scope.udp_send(s, *ip, 5353, &lan::mdns_ptr_query(*ip)) {
                    let mut buf = [0u8; 1500];
                    if let Ok((n, SocketAddr::V4(from))) = s.recv_from(&mut buf) {
                        if from.ip() == ip {
                            name = lan::parse_dns_ptr(&buf[..n]);
                        }
                    }
                }
            }
            if name.is_none() && scope.permits(*ip) {
                name = reverse_dns(*ip, &stop);
            }
            if let Some(n) = name {
                let _ = tx.send(Ev::Lan(LanEv::Name(run, *ip, n)));
            }
            let _ = tx.send(Ev::Lan(LanEv::Names(run, i + 1, total)));
        }
        if !stop.load(Ordering::SeqCst) {
            let _ = tx.send(Ev::Lan(LanEv::Done(run)));
        }
    });
    w
}

/// `getent hosts <ip>` with a time limit (a resolver that does not answer must not hang the scan).
fn reverse_dns(ip: Ipv4Addr, stop: &AtomicBool) -> Option<String> {
    let mut child = Command::new("getent").args(["hosts", &ip.to_string()]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() < Duration::from_millis(1500) && !stop.load(Ordering::SeqCst) => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    lan::parse_getent_hosts(&text)
}

/// The port check of one device: TCP connects only, one at a time, 50 a second at most.
pub(super) fn start_ports(tx: Sender<Ev>, run: u32, info: LanInfo, ip: Ipv4Addr, ports: Vec<u16>) -> Worker {
    let w = Worker::new();
    let stop = w.stop.clone();
    std::thread::spawn(move || {
        let scope = Scope::new(info, stop.clone());
        let total = ports.len();
        for (i, p) in ports.iter().enumerate() {
            match scope.tcp_open(ip, *p) {
                None => {
                    if !scope.stopped() {
                        let _ = tx.send(Ev::Lan(LanEv::Err(run, lan::Refusal::Public.text().into())));
                    }
                    return;
                }
                Some(true) => {
                    let _ = tx.send(Ev::Lan(LanEv::PortOpen(run, *p)));
                }
                Some(false) => {}
            }
            if i % 4 == 3 {
                let _ = tx.send(Ev::Lan(LanEv::PortProgress(run, i + 1, total)));
            }
        }
        if !stop.load(Ordering::SeqCst) {
            let _ = tx.send(Ev::Lan(LanEv::PortDone(run)));
        }
    });
    w
}

// ============================================================ page behaviour

impl Interceptor {
    /// First visit: load what earlier scans saw and read the neighbour table (passive).
    pub(super) fn lan_ensure(&mut self, ctx: &Ctx) {
        if self.page != PAGE_LAN || self.lan.init {
            return;
        }
        self.lan.init = true;
        let now = ctx.now.timestamp().max(0) as u64;
        if self.mock {
            self.lan.known = Known::mock(now);
            self.lan.own_mac = Some("d8:3a:dd:00:00:94".into());
        } else {
            self.lan.known = Known::load(Some(data_file("lan-known.tsv")));
            self.lan.accepted = data_file("lan-notice-ok").exists();
        }
        self.lan.info = ctx.sys.lan_info();
        if let Some(info) = self.lan.info.clone() {
            if !self.mock {
                self.lan.own_mac = std::fs::read_to_string(format!("/sys/class/net/{}/address", info.iface)).ok().and_then(|s| lan::norm_mac(&s));
            }
            let nbs = ctx.sys.neighbors();
            let names = if self.mock { self.lan_mock_names(&nbs) } else { HashMap::new() };
            merge(&mut self.lan.devices, &nbs, &info, self.lan.own_mac.as_deref(), &names, &mut self.lan.known, now);
            // Passive only: nothing was sent, so nothing is written either.
        } else {
            merge(&mut self.lan.devices, &[], &mock_off_info(), None, &HashMap::new(), &mut self.lan.known, now);
        }
    }

    fn lan_mock_names(&self, nbs: &[Neighbor]) -> HashMap<Ipv4Addr, String> {
        nbs.iter().filter_map(|n| lan::mock_name(n.ip).map(|s| (n.ip, s))).collect()
    }

    pub(super) fn lan_accept(&mut self, ctx: &Ctx) {
        self.lan.accepted = true;
        if !self.mock {
            let p = data_file("lan-notice-ok");
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, "ok\n");
        }
        self.lan_start(ctx);
    }

    /// Sweep the subnet again.
    pub(super) fn lan_start(&mut self, ctx: &Ctx) {
        if !self.lan.accepted || self.lan.scanning {
            return;
        }
        self.lan.err = None;
        let Some(info) = ctx.sys.lan_info() else {
            self.lan.err = Some("Not connected to a network".into());
            return;
        };
        if let Err(r) = info.allows(info.ip) {
            self.lan.err = Some(r.text().into());
            return;
        }
        self.lan.info = Some(info.clone());
        self.lan.run = self.lan.run.wrapping_add(1);
        self.lan.scanning = true;
        self.lan.phase = Phase::Sweep;
        self.lan.done = 0;
        self.lan.total = info.sweep_hosts().len();
        self.lan.names.clear();
        self.lan.swept = None;
        if self.mock {
            self.lan.last_mock = Instant::now();
            if crate::rt::frozen() {
                self.lan.done = self.lan.total;
                self.lan_after_sweep(ctx);
            }
        } else {
            self.lan_worker = Some(start_sweep(self.tx.clone(), self.lan.run, info));
        }
    }

    /// Esc: stop whatever scan is going. true when there was one.
    pub(super) fn lan_cancel(&mut self) -> bool {
        let mut was = false;
        if self.lan.scanning {
            self.lan.scanning = false;
            self.lan_worker = None;
            was = true;
        }
        if self.lan.pscan.running {
            self.lan.pscan.running = false;
            self.lan_port_worker = None;
            was = true;
        }
        was
    }

    /// The sweep is over: read the neighbour table, fold it into the list, then look the names of
    /// the devices without one up.
    pub(super) fn lan_after_sweep(&mut self, ctx: &Ctx) {
        let names = self.lan.swept.take().unwrap_or_default();
        let Some(info) = self.lan.info.clone() else { return };
        let now = ctx.now.timestamp().max(0) as u64;
        let nbs = ctx.sys.neighbors();
        let names = if self.mock { self.lan_mock_names(&nbs) } else { names };
        merge(&mut self.lan.devices, &nbs, &info, self.lan.own_mac.as_deref(), &names, &mut self.lan.known, now);
        self.lan.known.save();
        let todo: Vec<Ipv4Addr> = self.lan.devices.iter().filter(|d| d.online && !d.is_self && d.name.is_none()).filter_map(|d| d.ip).collect();
        if todo.is_empty() || self.mock {
            self.lan.scanning = false;
            self.lan_worker = None;
        } else {
            self.lan.phase = Phase::Names;
            self.lan.done = 0;
            self.lan.total = todo.len();
            self.lan_worker = Some(start_names(self.tx.clone(), self.lan.run, info, todo));
        }
    }

    pub(super) fn apply_lan(&mut self, ev: LanEv) {
        match ev {
            LanEv::Sweep(run, done, total) if run == self.lan.run && self.lan.scanning => {
                self.lan.done = done;
                self.lan.total = total;
            }
            LanEv::Swept(run, names) if run == self.lan.run && self.lan.scanning => self.lan.swept = Some(names),
            LanEv::Name(run, ip, name) if run == self.lan.run => {
                self.lan.names.insert(ip, name.clone());
                if let Some(d) = self.lan.devices.iter_mut().find(|d| d.ip == Some(ip)) {
                    d.name = Some(name.clone());
                    if let Some(k) = self.lan.known.map.get_mut(&d.mac) {
                        k.2 = name;
                    }
                }
            }
            LanEv::Names(run, done, total) if run == self.lan.run && self.lan.scanning => {
                self.lan.done = done;
                self.lan.total = total;
            }
            LanEv::Done(run) if run == self.lan.run => {
                self.lan.scanning = false;
                self.lan_worker = None;
                self.lan.known.save();
            }
            LanEv::Err(run, e) if run == self.lan.run => {
                self.lan.scanning = false;
                self.lan.err = Some(e);
                self.lan_worker = None;
            }
            LanEv::PortOpen(run, p) if run == self.lan.pscan.run && self.lan.pscan.running => self.lan.pscan.open.push(p),
            LanEv::PortProgress(run, done, total) if run == self.lan.pscan.run && self.lan.pscan.running => {
                self.lan.pscan.done = done;
                self.lan.pscan.total = total;
            }
            LanEv::PortDone(run) if run == self.lan.pscan.run => self.lan_ports_finished(),
            LanEv::Err(run, e) if run == self.lan.pscan.run => {
                self.lan.pscan.running = false;
                self.lan.pscan.err = Some(e);
                self.lan_port_worker = None;
            }
            _ => {}
        }
    }

    fn lan_ports_finished(&mut self) {
        let ps = &mut self.lan.pscan;
        ps.running = false;
        ps.done = ps.total;
        ps.open.sort_unstable();
        let (target, open) = (ps.target, ps.open.clone());
        self.lan_port_worker = None;
        if let Some(d) = self.lan.devices.iter_mut().find(|d| d.ip == target && target.is_some()) {
            d.open = Some(open);
        }
    }

    /// The device under the cursor.
    pub(super) fn lan_selected(&self) -> Option<&Device> {
        self.lan.devices.get(self.cur[PAGE_LAN])
    }

    /// Return in the device view: start the port check, or stop it.
    pub(super) fn lan_ports_toggle(&mut self, ctx: &Ctx) {
        if self.lan.pscan.running {
            self.lan_cancel();
            return;
        }
        let Some(ip) = self.lan_selected().filter(|d| d.online).and_then(|d| d.ip) else {
            self.lan.pscan.err = Some("The device is offline".into());
            return;
        };
        let ports = self.lan.pscan.ports();
        if ports.is_empty() {
            self.lan.pscan.err = Some("Type a port range first (E)".into());
            return;
        }
        let Some(info) = ctx.sys.lan_info() else {
            self.lan.pscan.err = Some("Not connected to a network".into());
            return;
        };
        if let Err(r) = info.allows(ip) {
            self.lan.pscan.err = Some(r.text().into());
            return;
        }
        let ps = &mut self.lan.pscan;
        ps.err = None;
        ps.run = ps.run.wrapping_add(1);
        ps.running = true;
        ps.done = 0;
        ps.total = ports.len();
        ps.open.clear();
        ps.target = Some(ip);
        ps.last_mock = Instant::now();
        if self.mock {
            if crate::rt::frozen() {
                ps.open = lan::mock_open_ports(ip).into_iter().filter(|p| ports.contains(p)).collect();
                self.lan_ports_finished();
            }
        } else {
            self.lan_port_worker = Some(start_ports(self.tx.clone(), self.lan.pscan.run, info, ip, ports));
        }
    }

    /// E in the device view: next port set; Custom opens the range editor.
    pub(super) fn lan_next_set(&mut self) {
        if self.lan.pscan.running {
            return;
        }
        let ps = &mut self.lan.pscan;
        ps.set = match ps.set {
            PortSet::Common => PortSet::Wide,
            PortSet::Wide => PortSet::Custom,
            PortSet::Custom => PortSet::Common,
        };
        if ps.set == PortSet::Custom {
            ps.edit = Some(ps.custom_text());
        }
    }

    /// P: ping the device (the Ping page, started at once).
    pub(super) fn lan_ping(&mut self) {
        let Some(ip) = self.lan_selected().filter(|d| d.online).and_then(|d| d.ip) else { return };
        self.ping.target = Target::Host(ip.to_string());
        self.set_page(PAGE_PING);
        self.ping.running = false;
        self.ping_worker = None;
        self.ping.reset();
        self.ping_toggle();
    }

    /// Keys of the LAN page that are not the shared ones. Some(action) = handled.
    pub(super) fn lan_key(&mut self, key: Keycode, ctx: &Ctx) -> Option<AppAction> {
        let k = arrow_alias(key);
        // The range editor swallows everything but its three commands.
        if self.lan.pscan.edit.is_some() {
            match k {
                Keycode::Return | Keycode::KpEnter => {
                    let text = self.lan.pscan.edit.clone().unwrap_or_default();
                    match lan::parse_ports(&text) {
                        Ok(v) => {
                            self.lan.pscan.custom = v;
                            self.lan.pscan.edit = None;
                            self.lan.pscan.err = None;
                        }
                        Err(e) => self.lan.pscan.err = Some(e.into()),
                    }
                }
                Keycode::Backspace => {
                    if let Some(t) = self.lan.pscan.edit.as_mut() {
                        t.pop();
                    }
                }
                Keycode::Escape => {
                    self.lan.pscan.edit = None;
                    self.lan.pscan.err = None;
                    if self.lan.pscan.custom.is_empty() {
                        self.lan.pscan.set = PortSet::Common;
                    }
                }
                _ => {}
            }
            return Some(AppAction::None);
        }
        if !self.lan.accepted {
            return match k {
                Keycode::Return | Keycode::KpEnter => {
                    self.lan_accept(ctx);
                    Some(AppAction::None)
                }
                _ => None,
            };
        }
        match k {
            Keycode::Escape => {
                if self.lan_cancel() {
                    return Some(AppAction::None);
                }
                if self.lan.detail {
                    self.lan.detail = false;
                    return Some(AppAction::None);
                }
                None
            }
            Keycode::Return | Keycode::KpEnter if !self.lan.detail => {
                if self.lan_selected().is_some() {
                    self.lan.detail = true;
                    self.lan.pscan.err = None;
                    // Show the last result of this device, not the previous device's.
                    let (open, ip) = self.lan_selected().map(|d| (d.open.clone(), d.ip)).unwrap_or((None, None));
                    self.lan.pscan.open = open.unwrap_or_default();
                    self.lan.pscan.target = ip;
                    self.lan.pscan.done = 0;
                    self.lan.pscan.total = 0;
                }
                Some(AppAction::None)
            }
            Keycode::Return | Keycode::KpEnter => {
                self.lan_ports_toggle(ctx);
                Some(AppAction::None)
            }
            Keycode::R if !self.lan.detail => {
                self.lan_start(ctx);
                Some(AppAction::None)
            }
            Keycode::E if self.lan.detail => {
                self.lan_next_set();
                Some(AppAction::None)
            }
            Keycode::P => {
                self.lan_ping();
                Some(AppAction::None)
            }
            // In the device view Up / Down would move the (hidden) list cursor: leave them out.
            Keycode::Up | Keycode::Down | Keycode::PageUp | Keycode::PageDown | Keycode::Home | Keycode::End if self.lan.detail => Some(AppAction::None),
            _ => None,
        }
    }

    /// Typed characters of the range editor.
    pub(super) fn lan_text(&mut self, text: &str) {
        if let Some(e) = self.lan.pscan.edit.as_mut() {
            for c in text.chars().filter(|c| c.is_ascii_digit() || *c == ',' || *c == '-') {
                if e.len() < 40 {
                    e.push(c);
                }
            }
        }
    }

    /// Mock scan: the progress moves on a timer, then the mock neighbours show up.
    pub(super) fn lan_mock_tick(&mut self, now: Instant, ctx: &Ctx) -> bool {
        let mut dirty = false;
        if self.lan.scanning && now.duration_since(self.lan.last_mock) >= Duration::from_millis(100) {
            self.lan.last_mock = now;
            self.lan.done = (self.lan.done + 40).min(self.lan.total);
            dirty = true;
            if self.lan.done >= self.lan.total {
                self.lan_after_sweep(ctx);
            }
        }
        let ps = &mut self.lan.pscan;
        if ps.running && now.duration_since(ps.last_mock) >= Duration::from_millis(100) {
            ps.last_mock = now;
            ps.done = (ps.done + 6).min(ps.total);
            dirty = true;
            if ps.done >= ps.total {
                if let Some(ip) = ps.target {
                    let ports = ps.ports();
                    ps.open = lan::mock_open_ports(ip).into_iter().filter(|p| ports.contains(p)).collect();
                }
                self.lan_ports_finished();
            }
        }
        dirty
    }
}

impl PortScan {
    fn custom_text(&self) -> String {
        let mut out = String::new();
        let v = &self.custom;
        let mut i = 0;
        while i < v.len() {
            let mut j = i;
            while j + 1 < v.len() && v[j + 1] == v[j] + 1 {
                j += 1;
            }
            if !out.is_empty() {
                out.push(',');
            }
            if j > i {
                out.push_str(&format!("{}-{}", v[i], v[j]));
            } else {
                out.push_str(&v[i].to_string());
            }
            i = j + 1;
        }
        out
    }
}

/// Stand-in when there is no connection (only to give `merge` something to fence against).
fn mock_off_info() -> LanInfo {
    LanInfo { iface: String::new(), ip: Ipv4Addr::new(192, 168, 0, 2), prefix: 24, gateway: None }
}

// ====================================================================== drawing

/// A short "5m ago" text.
fn ago(now: u64, t: u64) -> String {
    let secs = now.saturating_sub(t);
    if secs < 60 {
        return i18n::tr_s("now");
    }
    let (n, en, ja) = match secs {
        60..=3599 => (secs / 60, "m", "分"),
        3600..=86_399 => (secs / 3600, "h", "時間"),
        _ => (secs / 86_400, "d", "日"),
    };
    if i18n::get() == i18n::Lang::Ja {
        format!("{n}{ja}前")
    } else {
        format!("{n}{en} ago")
    }
}

impl Interceptor {
    pub(super) fn draw_lan(&mut self, c: &mut Canvas, ctx: &Ctx, p: &Pal) {
        if !self.lan.accepted {
            return self.draw_lan_notice(c, ctx, p);
        }
        if self.lan.detail && self.lan_selected().is_some() {
            return self.draw_lan_detail(c, ctx, p);
        }
        let l = &self.lan;
        let left = match (&l.info, &l.err) {
            (_, Some(e)) => e.clone(),
            (Some(i), _) => format!("{}  {}/{}", i.iface, Ipv4Addr::from(u32::from(i.ip) & (u32::MAX << (32 - i.prefix.min(32) as u32).min(31))), i.prefix),
            (None, _) => "Not connected to a network".to_string(),
        };
        let right = if l.scanning {
            let what = if l.phase == Phase::Sweep { "scanning" } else { "names" };
            format!("{} {}/{}", i18n::tr_s(what), l.done, l.total)
        } else if l.new_count() > 0 {
            format!("{}  {}", i18n::tr_s(&format!("{} devices", l.online_count())), i18n::tr_s(&format!("{} new", l.new_count())))
        } else {
            i18n::tr_s(&format!("{} devices", l.online_count()))
        };
        header_band(c, ctx, p, 0, &left, &right);
        let vis = (body_rows() - 1) as usize;
        let total = l.devices.len();
        if total == 0 {
            text_row(c, ctx, LIST_X + 4, BODY_Y + ROW + 2, list_w() - 8, if l.scanning { "Looking around..." } else { "R: look for devices" }, p.faint);
            return;
        }
        self.scroll[PAGE_LAN] = self.scroll[PAGE_LAN].min(total.saturating_sub(vis));
        let now = ctx.now.timestamp().max(0) as u64;
        let _ = now;
        for (i, d) in self.lan.devices.iter().enumerate().skip(self.scroll[PAGE_LAN]).take(vis) {
            let row = row_rect(1 + (i - self.scroll[PAGE_LAN]) as i32);
            let (fg, sub) = row_style(c, ctx, p, row, i, i == self.cur[PAGE_LAN]);
            let col = if d.online { fg } else { sub };
            let ip = match (d.online, d.ip) {
                (true, Some(ip)) => ip.to_string(),
                _ => i18n::tr_s("offline"),
            };
            let ipw = ctx.fs.measure_text("255.255.255.255") + 2;
            right_text_raw(c, ctx, row.right() - 4, row.y(), &ip, col);
            let tag = if d.new {
                "NEW"
            } else if d.is_router {
                "router"
            } else if d.is_self {
                "you"
            } else {
                ""
            };
            let tw = if tag.is_empty() { 0 } else { ctx.fs.measure_text(&i18n::tr_s(tag)) + 8 };
            if !tag.is_empty() {
                right_text(c, ctx, row.right() - 4 - ipw - 4, row.y(), tag, if d.new { fg } else { sub });
            }
            text_data(c, ctx, row.x() + 4, row.y(), list_w() - 8 - ipw - 4 - tw, &d.title(), col);
        }
        scrollbar(c, ctx, total, vis, self.scroll[PAGE_LAN]);
    }

    fn draw_lan_notice(&mut self, c: &mut Canvas, ctx: &Ctx, p: &Pal) {
        header_band(c, ctx, p, 0, "Network scan", "");
        let lines = [
            (NOTICE, p.ink),
            ("Only this network's own addresses are probed:", p.faint),
            ("50 probes a second at most, nothing is read.", p.faint),
            ("Esc stops a scan.", p.faint),
            ("", p.faint),
            ("Return: continue", p.ink),
        ];
        for (i, (s, col)) in lines.iter().enumerate() {
            if !s.is_empty() {
                text_row(c, ctx, LIST_X + 4, BODY_Y + (2 + i as i32) * ROW, list_w() - 8, s, *col);
            }
        }
    }

    fn draw_lan_detail(&mut self, c: &mut Canvas, ctx: &Ctx, p: &Pal) {
        let Some(d) = self.lan_selected().cloned() else { return };
        let ps = &self.lan.pscan;
        let now = ctx.now.timestamp().max(0) as u64;
        let state = if let Some(e) = &ps.err {
            e.clone()
        } else if ps.running {
            format!("{} {}/{}", i18n::tr_s("checking"), ps.done, ps.total)
        } else if d.open.is_some() {
            "done".to_string()
        } else if d.online {
            "Return: check ports".to_string()
        } else {
            "offline".to_string()
        };
        header_band(c, ctx, p, 0, &d.title(), &state);
        let set = match ps.set {
            PortSet::Common => format!("{} ({})", i18n::tr_s("common"), lan::COMMON_PORTS.len()),
            PortSet::Wide => "1-1024".to_string(),
            PortSet::Custom => match &ps.edit {
                Some(t) => format!("{t}_"),
                None if ps.custom.is_empty() => "-".to_string(),
                None => ps.custom_text(),
            },
        };
        let seen = format!("{} {}, {} {}", i18n::tr_s("first"), ago(now, d.first), i18n::tr_s("last"), ago(now, d.last));
        let tags: Vec<&str> = [(d.is_router, "router"), (d.is_self, "you"), (d.new, "NEW")].iter().filter(|t| t.0).map(|t| t.1).collect();
        let addr = match d.ip {
            Some(ip) if tags.is_empty() => ip.to_string(),
            Some(ip) => format!("{ip}  {}", tags.iter().map(|t| i18n::tr_s(t)).collect::<Vec<_>>().join(" ")),
            None => "-".to_string(),
        };
        let rows: Vec<(&str, String)> = vec![("Address", addr), ("MAC", mask_addr(&d.mac, self.mask)), ("Maker", i18n::tr_s(&d.vendor())), ("Seen", seen), ("Check", set)];
        for (i, (k, v)) in rows.iter().enumerate() {
            let row = row_rect(1 + i as i32);
            zebra(c, p, row, i);
            ctx.fs.draw_text(c, row.x() + 4, row.y() + 1, &i18n::tr_s(k), p.faint);
            let v = chrome::fit_text(ctx.fs, v, list_w() - 60);
            right_text_raw(c, ctx, row.right() - 4, row.y(), &v, p.ink);
        }
        // The open ports, over the last two rows.
        let open = if ps.running || d.open.is_none() { ps.open.clone() } else { d.open.clone().unwrap_or_default() };
        let r6 = row_rect(6);
        let r7 = row_rect(7);
        zebra(c, p, r6, 5);
        zebra(c, p, r7, 6);
        if open.is_empty() {
            let msg = if d.open.is_some() && !ps.running { "No open port found" } else { "Open ports show here" };
            text_row(c, ctx, r6.x() + 4, r6.y(), list_w() - 8, msg, p.faint);
            return;
        }
        let items: Vec<String> = open
            .iter()
            .map(|port| {
                let s = lan::service_name(*port);
                if s.is_empty() {
                    port.to_string()
                } else {
                    format!("{port} {s}")
                }
            })
            .collect();
        let room = list_w() - 8;
        let mut lines: [String; 2] = [String::new(), String::new()];
        let mut li = 0;
        let mut shown = 0;
        for it in &items {
            let cand = if lines[li].is_empty() { it.clone() } else { format!("{}   {}", lines[li], it) };
            if ctx.fs.measure_text(&cand) > room - 30 {
                if li == 1 {
                    break;
                }
                li = 1;
                lines[1] = it.clone();
            } else {
                lines[li] = cand;
            }
            shown += 1;
        }
        if shown < items.len() {
            lines[1] = format!("{}  +{}", lines[1], items.len() - shown);
        }
        text_data(c, ctx, r6.x() + 4, r6.y(), room, &lines[0], p.ink);
        text_data(c, ctx, r7.x() + 4, r7.y(), room, &lines[1], p.ink);
    }
}

/// Like `right_text` for data that is never translated.
fn right_text_raw(c: &mut Canvas, ctx: &Ctx, right: i32, y: i32, s: &str, col: Color) {
    let w = ctx.fs.measure_text(s);
    ctx.fs.draw_text(c, right - w, y + 1, s, col);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::lan::*;

    fn info() -> LanInfo {
        mock_info()
    }

    #[test]
    fn nothing_leaves_the_fence() {
        let stop = Arc::new(AtomicBool::new(false));
        let scope = Scope::new(info(), stop);
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        // Public, other subnet, loopback, broadcast: refused before anything is sent.
        for ip in ["8.8.8.8", "192.168.4.1", "127.0.0.1", "192.168.3.255", "10.0.0.1"] {
            let ip: Ipv4Addr = ip.parse().unwrap();
            assert!(!scope.udp_send(&sock, ip, 137, b"x"), "{ip}");
            assert_eq!(scope.tcp_open(ip, 80), None, "{ip}");
            assert!(!scope.permits(ip), "{ip}");
        }
    }

    #[test]
    fn probes_are_paced_to_fifty_a_second() {
        let stop = Arc::new(AtomicBool::new(false));
        let scope = Scope::new(info(), stop);
        let sock = UdpSocket::bind("0.0.0.0:0").unwrap();
        let t0 = Instant::now();
        for _ in 0..11 {
            // Allowed address of the mock subnet; UDP send to a non-existent host just fails to deliver.
            scope.udp_send(&sock, "192.168.3.7".parse().unwrap(), 9, b"x");
        }
        assert!(t0.elapsed() >= Duration::from_millis(10 * 20 - 5), "{:?}", t0.elapsed());
    }

    #[test]
    fn a_cancelled_scope_sends_nothing() {
        let stop = Arc::new(AtomicBool::new(true));
        let scope = Scope::new(info(), stop);
        let sock = UdpSocket::bind("0.0.0.0:0").unwrap();
        assert!(!scope.udp_send(&sock, "192.168.3.7".parse().unwrap(), 9, b"x"));
        assert_eq!(scope.tcp_open("192.168.3.7".parse().unwrap(), 80), None);
    }

    #[test]
    fn a_port_check_finds_a_listener_on_the_machine() {
        // Allow loopback only for this test: a fence around 127.0.0.0/8 would refuse it, so
        // build the connect by hand the way tcp_open does and check the pacing helper separately.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), CONNECT_TIMEOUT).is_ok());
        // And the fence itself says loopback is not a scan target.
        let s = Scope::new(info(), Arc::new(AtomicBool::new(false)));
        assert_eq!(s.tcp_open(Ipv4Addr::LOCALHOST, port), None);
    }

    #[test]
    fn a_scan_merges_names_first_seen_and_new_devices() {
        let now = 1_800_000_000u64;
        let mut known = Known::mock(now);
        let mut devs = Vec::new();
        let nbs = mock_neighbors();
        let names: HashMap<Ipv4Addr, String> = nbs.iter().filter_map(|n| mock_name(n.ip).map(|s| (n.ip, s))).collect();
        merge(&mut devs, &nbs, &info(), Some("d8:3a:dd:00:00:94"), &names, &mut known, now);
        // 7 neighbours + the device itself + the iPad of an earlier scan.
        assert_eq!(devs.len(), 9);
        assert_eq!(devs.iter().filter(|d| d.online).count(), 8);
        let me = devs.iter().find(|d| d.is_self).unwrap();
        assert_eq!(me.ip, Some(info().ip));
        let router = devs.iter().find(|d| d.is_router).unwrap();
        assert_eq!(router.name.as_deref(), Some("router.lan"));
        let new: Vec<_> = devs.iter().filter(|d| d.new).collect();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].mac, "be:12:34:56:78:9a");
        let ipad = devs.iter().find(|d| d.name.as_deref() == Some("iPad")).unwrap();
        assert!(!ipad.online && ipad.ip.is_none());
        assert_eq!(ipad.last, now - 2 * 86_400 - 600);
        // Sorted: online by address, offline last.
        let ips: Vec<_> = devs.iter().filter_map(|d| d.ip).collect();
        assert!(ips.windows(2).all(|w| w[0] <= w[1]));
        assert!(!devs.last().unwrap().online);
        // A second scan keeps first-seen and the NEW flag of the session, and learns the device.
        merge(&mut devs, &nbs, &info(), Some("d8:3a:dd:00:00:94"), &names, &mut known, now + 60);
        assert_eq!(devs.len(), 9);
        assert!(devs.iter().find(|d| d.mac == "be:12:34:56:78:9a").unwrap().new);
        assert!(known.map.contains_key("be:12:34:56:78:9a"));
    }

    #[test]
    fn the_first_ever_scan_flags_nothing_new() {
        let mut known = Known::load(None);
        assert!(known.baseline);
        let mut devs = Vec::new();
        merge(&mut devs, &mock_neighbors(), &info(), None, &HashMap::new(), &mut known, 1000);
        assert!(devs.iter().all(|d| !d.new));
        assert_eq!(known.map.len(), 7);
    }

    #[test]
    fn neighbours_outside_the_subnet_never_make_the_list() {
        let mut known = Known::load(None);
        let mut devs = Vec::new();
        let mut nbs = mock_neighbors();
        nbs.push(Neighbor { ip: "8.8.8.8".parse().unwrap(), mac: "aa:bb:cc:dd:ee:ff".into(), reachable: true });
        nbs.push(Neighbor { ip: "10.1.1.1".parse().unwrap(), mac: "aa:bb:cc:dd:ee:fe".into(), reachable: true });
        merge(&mut devs, &nbs, &info(), None, &HashMap::new(), &mut known, 1000);
        assert_eq!(devs.len(), 7);
    }

    #[test]
    fn the_known_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("harbour-lan-{}", std::process::id()));
        let path = dir.join("lan-known.tsv");
        let mut k = Known::load(Some(path.clone()));
        assert!(k.baseline);
        k.map.insert("b8:27:eb:12:34:56".into(), (100, 200, "kitchen\tpi".into()));
        k.save();
        let k2 = Known::load(Some(path));
        assert!(!k2.baseline);
        assert_eq!(k2.map["b8:27:eb:12:34:56"], (100, 200, "kitchen pi".to_string()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_range_is_shown_as_it_was_typed() {
        let mut ps = PortScan::new();
        ps.custom = vec![22, 80, 81, 82, 443];
        assert_eq!(ps.custom_text(), "22,80-82,443");
        assert_eq!(parse_ports(&ps.custom_text()).unwrap(), ps.custom);
    }

    fn setup() -> (crate::font::FontSystem, crate::theme::ThemeManager, crate::system::mock::MockBackend) {
        let fs = crate::font::FontSystem::new();
        let tm = crate::theme::ThemeManager::new(crate::theme::Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        (fs, tm, crate::system::mock::MockBackend::new())
    }

    #[test]
    fn the_page_asks_first_then_scans_then_checks_ports() {
        let (fs, tm, sys) = setup();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut a = Interceptor::with_side(true, Side::Wireless);
        a.set_page(PAGE_LAN);
        a.tick(&ctx);
        assert!(!a.lan.accepted, "the notice comes first");
        assert!(a.lan.devices.iter().any(|d| d.online), "the neighbour table is read passively");
        assert!(!a.lan.scanning);
        // R before accepting does nothing.
        a.key(Keycode::R, Mod::NOMOD, false, &ctx);
        assert!(!a.lan.scanning);
        a.key(Keycode::Return, Mod::NOMOD, false, &ctx);
        assert!(a.lan.accepted && a.lan.scanning);
        // Esc stops it; Esc again leaves nothing to stop and would quit.
        assert!(matches!(a.key(Keycode::Escape, Mod::NOMOD, false, &ctx), AppAction::None));
        assert!(!a.lan.scanning);
        a.key(Keycode::R, Mod::NOMOD, false, &ctx);
        let mut t = Instant::now();
        for _ in 0..20 {
            t += Duration::from_millis(150);
            a.lan_mock_tick(t, &ctx);
        }
        assert!(!a.lan.scanning);
        assert_eq!(a.lan.new_count(), 1);
        // Open the printer and check its ports.
        let i = a.lan.devices.iter().position(|d| d.name.as_deref() == Some("BRW-office")).unwrap();
        a.cur[PAGE_LAN] = i;
        a.key(Keycode::Return, Mod::NOMOD, false, &ctx);
        assert!(a.lan.detail);
        a.key(Keycode::Return, Mod::NOMOD, false, &ctx);
        assert!(a.lan.pscan.running);
        for _ in 0..20 {
            t += Duration::from_millis(150);
            a.lan_mock_tick(t, &ctx);
        }
        assert!(!a.lan.pscan.running);
        assert_eq!(a.lan.devices[i].open, Some(vec![80, 443, 631, 9100]), "only the ports of the common list");
        // Esc: back to the list, then it would quit.
        assert!(matches!(a.key(Keycode::Escape, Mod::NOMOD, false, &ctx), AppAction::None));
        assert!(!a.lan.detail);
        assert!(matches!(a.key(Keycode::Escape, Mod::NOMOD, false, &ctx), AppAction::Quit));
    }

    #[test]
    fn a_custom_range_is_typed_and_checked() {
        let (fs, tm, sys) = setup();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut a = Interceptor::with_side(true, Side::Wireless);
        a.set_page(PAGE_LAN);
        a.tick(&ctx);
        a.lan.accepted = true;
        let i = a.lan.devices.iter().position(|d| d.name.as_deref() == Some("kitchen-pi")).unwrap();
        a.cur[PAGE_LAN] = i;
        a.key(Keycode::Return, Mod::NOMOD, false, &ctx);
        a.key(Keycode::E, Mod::NOMOD, false, &ctx); // 1-1024
        a.key(Keycode::E, Mod::NOMOD, false, &ctx); // custom: editor opens
        assert_eq!(a.lan.pscan.set, PortSet::Custom);
        assert!(a.lan.pscan.edit.is_some());
        a.text("20-30,x,8o", &ctx);
        assert_eq!(a.lan.pscan.edit.as_deref(), Some("20-30,,8"), "letters never get in");
        a.key(Keycode::Backspace, Mod::NOMOD, false, &ctx);
        a.key(Keycode::Backspace, Mod::NOMOD, false, &ctx);
        a.text("80", &ctx);
        a.key(Keycode::Return, Mod::NOMOD, false, &ctx);
        assert_eq!(a.lan.pscan.edit, None);
        assert_eq!(a.lan.pscan.ports().len(), 12);
        // A bad range stays in the editor with a message.
        a.key(Keycode::E, Mod::NOMOD, false, &ctx);
        a.key(Keycode::E, Mod::NOMOD, false, &ctx);
        a.key(Keycode::E, Mod::NOMOD, false, &ctx);
        assert_eq!(a.lan.pscan.set, PortSet::Custom);
        a.lan.pscan.edit = Some("70000".into());
        a.key(Keycode::Return, Mod::NOMOD, false, &ctx);
        assert!(a.lan.pscan.edit.is_some() && a.lan.pscan.err.is_some());
    }

    #[test]
    fn p_pings_the_selected_device() {
        let (fs, tm, sys) = setup();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut a = Interceptor::with_side(true, Side::Wireless);
        a.set_page(PAGE_LAN);
        a.tick(&ctx);
        a.lan.accepted = true;
        a.cur[PAGE_LAN] = a.lan.devices.iter().position(|d| d.name.as_deref() == Some("kitchen-pi")).unwrap();
        a.key(Keycode::P, Mod::NOMOD, false, &ctx);
        assert_eq!(a.page, PAGE_PING);
        assert_eq!(a.ping.host().as_deref(), Some("192.168.3.20"));
        assert!(a.ping.running);
    }
}
