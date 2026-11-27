//! Interceptor's network pages that sample over time: Ping (echo requests to the router, a DNS
//! server or the internet) and Link log (Wi-Fi signal and round trip of the router, a few seconds
//! apart). Passive and polite: one echo request a second at most, to addresses the user picked.
//! The parsers live in `system/parse.rs`; the workers follow the rest of the module (`Worker`,
//! `Ev`); the mock backend (`HARBOUR_MOCK=1`) gets synthetic replies.

use super::*;
use crate::system::parse::ping_reply;

/// Samples kept for the Ping graph / the Link log (the graphs show the newest ones that fit).
pub(super) const PING_CAP: usize = 300;
pub(super) const LINK_CAP: usize = 600;
/// Echo requests per run (0 = until stopped).
pub(super) const PING_COUNTS: [u32; 4] = [10, 30, 100, 0];
/// Seconds between Link log samples.
pub(super) const LINK_EVERY: Duration = Duration::from_secs(3);
/// A target that is neither the router nor a DNS server of the connection.
const INTERNET: &str = "1.1.1.1";

/// What a Ping run pings: the connection's router, one of its DNS servers, or the internet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Router,
    Dns(usize),
    Internet,
    /// A device of the LAN page (an address of the connected subnet).
    Host(String),
}

pub(super) struct PingView {
    pub target: Target,
    /// Settings > Network's Router and DNS rows (read once when the page opens).
    pub router: Option<String>,
    pub dns: Vec<String>,
    pub net_known: bool,
    pub count_idx: usize,
    /// Which run the events belong to (a restart makes late events of the old one stale).
    pub run: u32,
    pub running: bool,
    pub err: Option<String>,
    pub sent: u32,
    pub lost: u32,
    pub sum: f32,
    pub min: f32,
    pub max: f32,
    /// Newest last; `None` = no reply.
    pub samples: VecDeque<Option<f32>>,
    pub last_mock: Instant,
}

impl PingView {
    pub fn new() -> Self {
        Self {
            target: Target::Router,
            router: None,
            dns: Vec::new(),
            net_known: false,
            count_idx: 0,
            run: 0,
            running: false,
            err: None,
            sent: 0,
            lost: 0,
            sum: 0.0,
            min: f32::MAX,
            max: 0.0,
            samples: VecDeque::new(),
            last_mock: Instant::now(),
        }
    }

    pub fn count(&self) -> u32 {
        PING_COUNTS[self.count_idx % PING_COUNTS.len()]
    }

    /// The address the target stands for (None while unknown, e.g. no router).
    pub fn host(&self) -> Option<String> {
        match &self.target {
            Target::Router => self.router.clone(),
            Target::Dns(i) => self.dns.get(*i).cloned(),
            Target::Internet => Some(INTERNET.to_string()),
            Target::Host(h) => Some(h.clone()),
        }
    }

    pub fn target_name(&self) -> &'static str {
        match self.target {
            Target::Router => "router",
            Target::Dns(_) => "DNS",
            Target::Internet => "internet",
            Target::Host(_) => "device",
        }
    }

    /// Forget the run's numbers (the target and count stay).
    pub fn reset(&mut self) {
        self.run = self.run.wrapping_add(1);
        self.sent = 0;
        self.lost = 0;
        self.sum = 0.0;
        self.min = f32::MAX;
        self.max = 0.0;
        self.samples.clear();
        self.err = None;
    }

    pub fn record(&mut self, ms: Option<f32>) {
        self.sent += 1;
        match ms {
            Some(v) => {
                self.sum += v;
                self.min = self.min.min(v);
                self.max = self.max.max(v);
            }
            None => self.lost += 1,
        }
        if self.samples.len() >= PING_CAP {
            self.samples.pop_front();
        }
        self.samples.push_back(ms);
    }

    pub fn received(&self) -> u32 {
        self.sent - self.lost
    }

    pub fn loss_pct(&self) -> f32 {
        if self.sent == 0 {
            0.0
        } else {
            self.lost as f32 * 100.0 / self.sent as f32
        }
    }

    pub fn avg(&self) -> Option<f32> {
        (self.received() > 0).then(|| self.sum / self.received() as f32)
    }

    /// Cycle the target: router, each DNS server, internet.
    pub fn next_target(&mut self) {
        self.target = match &self.target {
            Target::Router => Target::Dns(0),
            Target::Dns(i) if *i + 1 < self.dns.len() => Target::Dns(*i + 1),
            Target::Dns(_) => Target::Internet,
            Target::Internet | Target::Host(_) => Target::Router,
        };
        if matches!(self.target, Target::Dns(_)) && self.dns.is_empty() {
            self.target = Target::Internet;
        }
    }

    pub fn done(&self) -> bool {
        !self.running && self.sent > 0
    }
}

/// One Link log sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct LinkSample {
    /// Wi-Fi signal 0..=100 (None while not connected).
    pub signal: Option<u8>,
    /// Round trip to the router in ms (None = no reply or no router).
    pub rtt: Option<f32>,
}

pub(super) struct LinkLog {
    pub samples: VecDeque<LinkSample>,
    pub ssid: Option<String>,
    pub started: bool,
    pub last_mock: Instant,
}

impl LinkLog {
    pub fn new() -> Self {
        Self { samples: VecDeque::new(), ssid: None, started: false, last_mock: Instant::now() }
    }

    pub fn push(&mut self, s: LinkSample) {
        if self.samples.len() >= LINK_CAP {
            self.samples.pop_front();
        }
        self.samples.push_back(s);
    }
}

/// A round-trip scale: the next of 5, 10, 20, 50, 100, ... ms above `v`.
pub(super) fn nice_ms(v: f32) -> f32 {
    let mut m = 1.0f32;
    loop {
        for k in [1.0, 2.0, 5.0] {
            if m * k >= 5.0 && v <= m * k {
                return m * k;
            }
        }
        m *= 10.0;
    }
}

/// Synthetic round trip of the mock backend: a steady few ms with a slow wobble, a spike now and
/// then and a lost reply every 17th request. The same numbers for the same `n` (golden images).
pub(super) fn mock_ping_ms(n: u32) -> Option<f32> {
    if n % 17 == 16 {
        return None;
    }
    let t = n as f32;
    let base = 3.0 + 1.2 * (t * 0.45).sin() + 0.6 * (t * 1.7).cos();
    Some(if n % 11 == 7 { base + 14.0 } else { base }.max(0.8))
}

/// The mock link: signal wanders around 70 %, the round trip follows [`mock_ping_ms`].
pub(super) fn mock_link(n: u32) -> LinkSample {
    let t = n as f32;
    let signal = (70.0 + 9.0 * (t * 0.21).sin() + 4.0 * (t * 0.9).cos()).round().clamp(0.0, 100.0) as u8;
    LinkSample { signal: Some(signal), rtt: mock_ping_ms(n + 3) }
}

/// Echo requests to `host`, one a second, `count` of them (0 = until stopped). Each result is an
/// `Ev::Ping` for run `run`; `Ev::PingEnd` closes the run.
pub(super) fn start_ping(tx: Sender<Ev>, host: String, count: u32, run: u32) -> Worker {
    let w = Worker::new();
    let (stop, pid) = (w.stop.clone(), w.pid.clone());
    std::thread::spawn(move || {
        let mut n = 0u32;
        while !stop.load(Ordering::SeqCst) && (count == 0 || n < count) {
            let t0 = Instant::now();
            let ms = ping_once(&host, &stop, &pid);
            let Some(ms) = ms else {
                let _ = tx.send(Ev::PingErr(run, "ping is not available".into()));
                return;
            };
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let _ = tx.send(Ev::Ping(run, ms));
            n += 1;
            let spent = t0.elapsed().as_millis() as u64;
            sleep_unless(&stop, 1000u64.saturating_sub(spent));
        }
        if !stop.load(Ordering::SeqCst) {
            let _ = tx.send(Ev::PingEnd(run));
        }
    });
    w
}

/// One echo request: `Some(Some(ms))` replied, `Some(None)` timed out, `None` ping could not run.
fn ping_once(host: &str, stop: &AtomicBool, pid: &AtomicI32) -> Option<Option<f32>> {
    let mut got: Option<f32> = None;
    let ok = run_lines("ping", &["-n", "-c", "1", "-W", "2", host], stop, pid, &mut |l| {
        if got.is_none() {
            got = ping_reply(&l).map(|r| r.ms);
        }
    });
    ok.then_some(got)
}

/// The Link log's sampler: every [`LINK_EVERY`] the signal of the joined network (from the cached
/// scan list, no new scan) and one echo request to the router. Lives until the app closes.
pub(super) fn start_link(tx: Sender<Ev>, router: Option<String>) -> Worker {
    let w = Worker::new();
    let (stop, pid) = (w.stop.clone(), w.pid.clone());
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let t0 = Instant::now();
            let list = run_capture("nmcli", &["-t", "-f", "IN-USE,SSID,BSSID,CHAN,SIGNAL,SECURITY", "dev", "wifi", "list", "--rescan", "no"]).unwrap_or_default();
            let joined = parse_nmcli_wifi(&list).into_iter().find(|n| n.in_use);
            let rtt = router.as_deref().and_then(|h| ping_once(h, &stop, &pid).flatten());
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let _ = tx.send(Ev::Link { ssid: joined.as_ref().map(|n| n.ssid.clone()), sample: LinkSample { signal: joined.map(|n| n.signal), rtt } });
            let spent = t0.elapsed().as_millis() as u64;
            sleep_unless(&stop, (LINK_EVERY.as_millis() as u64).saturating_sub(spent));
        }
    });
    w
}

/// Wi-Fi channel load: how many of the scanned networks sit on each channel, and the strongest
/// signal there. 2.4 GHz channels 1..=13, 5 GHz ones as they come (sorted).
pub(super) fn channel_load(nets: &[WifiNet]) -> Vec<(u32, usize, u8)> {
    let mut v: Vec<(u32, usize, u8)> = Vec::new();
    for n in nets.iter().filter(|n| n.chan > 0) {
        match v.iter_mut().find(|e| e.0 == n.chan) {
            Some(e) => {
                e.1 += 1;
                e.2 = e.2.max(n.signal);
            }
            None => v.push((n.chan, 1, n.signal)),
        }
    }
    v.sort_by_key(|e| e.0);
    v
}

// ====================================================================== drawing

impl Interceptor {
    /// Settings > Network's Router and DNS rows, read once (the Wi-Fi status call may take a
    /// moment on the device). Starts the Link sampler on its first visit.
    pub(super) fn ensure_net(&mut self, ctx: &Ctx) {
        if self.page != PAGE_PING && self.page != PAGE_LINK {
            return;
        }
        if !self.ping.net_known {
            let st = ctx.sys.wifi_status();
            self.ping.router = st.gateway;
            self.ping.dns = st.dns;
            self.ping.net_known = true;
            if matches!(self.ping.target, Target::Router) && self.ping.router.is_none() {
                self.ping.next_target();
            }
        }
        if self.page == PAGE_LINK && !self.link.started {
            self.link.started = true;
            if self.mock {
                for n in 0..140 {
                    self.link.push(mock_link(n));
                }
                self.link.ssid = Some("Harbour Cafe".into());
                self.mock_link_n = 140;
            } else {
                self.link_worker = Some(start_link(self.tx.clone(), self.ping.router.clone()));
            }
        }
    }

    /// Return on the Ping page: start a run, or stop the one in progress.
    pub(super) fn ping_toggle(&mut self) {
        if self.ping.running {
            self.ping.running = false;
            self.ping_worker = None;
            return;
        }
        let Some(host) = self.ping.host() else {
            self.ping.err = Some("No address for this target".into());
            return;
        };
        self.ping.reset();
        self.ping.running = true;
        self.ping.last_mock = Instant::now();
        if self.mock {
            // A first stretch of the run is there at once, then one reply per tick.
            let ahead = if crate::rt::frozen() { 24 } else { 3 };
            for n in 0..ahead.min(self.ping.count().max(1)) {
                self.ping.record(mock_ping_ms(n));
            }
            self.ping_finish_if_done();
        } else {
            self.ping_worker = Some(start_ping(self.tx.clone(), host, self.ping.count(), self.ping.run));
        }
    }

    fn ping_finish_if_done(&mut self) {
        let c = self.ping.count();
        if c > 0 && self.ping.sent >= c {
            self.ping.running = false;
        }
    }

    pub(super) fn ping_set_target(&mut self, t: Target) {
        self.ping.target = t;
        self.ping_restart_if_running();
    }

    pub(super) fn ping_restart_if_running(&mut self) {
        let was = self.ping.running;
        self.ping.running = false;
        self.ping_worker = None;
        self.ping.reset();
        if was {
            self.ping_toggle();
        }
    }

    /// Mock replies while a mock run is going (not under HARBOUR_FROZEN, which pins the scene).
    pub(super) fn mock_net_tick(&mut self, now: Instant) -> bool {
        let mut dirty = false;
        if self.ping.running && now.duration_since(self.ping.last_mock) >= Duration::from_millis(1000) {
            self.ping.last_mock = now;
            self.ping.record(mock_ping_ms(self.ping.sent));
            self.ping_finish_if_done();
            dirty = true;
        }
        if self.link.started && now.duration_since(self.link.last_mock) >= LINK_EVERY {
            self.link.last_mock = now;
            self.link.push(mock_link(self.mock_link_n));
            self.mock_link_n += 1;
            dirty = true;
        }
        dirty
    }

    pub(super) fn apply_net(&mut self, ev: Ev) {
        match ev {
            Ev::Ping(run, ms) if run == self.ping.run && self.ping.running => {
                self.ping.record(ms);
                self.ping_finish_if_done();
                if !self.ping.running {
                    self.ping_worker = None;
                }
            }
            Ev::PingEnd(run) if run == self.ping.run => {
                self.ping.running = false;
                self.ping_worker = None;
            }
            Ev::PingErr(run, e) if run == self.ping.run => {
                self.ping.running = false;
                self.ping.err = Some(e);
                self.ping_worker = None;
            }
            Ev::Link { ssid, sample } => {
                self.link.ssid = ssid;
                self.link.push(sample);
            }
            _ => {}
        }
    }

    pub(super) fn draw_ping(&mut self, c: &mut Canvas, ctx: &Ctx, p: &Pal) {
        let pv = &self.ping;
        let g = eth_graph_rect();
        let slots = eth_slots(g);
        let shown: Vec<Option<f32>> = pv.samples.iter().skip(pv.samples.len().saturating_sub(slots)).copied().collect();
        let host = pv.host().unwrap_or_else(|| "-".into());
        let state = if let Some(e) = &pv.err {
            e.clone()
        } else if pv.running {
            "running".to_string()
        } else if pv.done() {
            "done".to_string()
        } else {
            "Return: start".to_string()
        };
        let count = pv.count();
        let ms = |v: f32| if v >= 100.0 { format!("{v:.0} ms") } else { format!("{v:.1} ms") };
        let last = pv.samples.back().map(|s| s.map_or_else(|| "lost".to_string(), ms)).unwrap_or_else(|| "-".into());
        let rows: Vec<(&str, String)> = vec![
            ("To", format!("{} {}", i18n::tr_s(pv.target_name()), host)),
            ("Count", if count == 0 { "until stopped".into() } else { format!("{count}") }),
            ("Sent", format!("{}", pv.sent)),
            ("Loss", if pv.sent == 0 { "-".into() } else { format!("{:.0}%  ({})", pv.loss_pct(), pv.lost) }),
            ("Last", last),
            ("Min", if pv.received() > 0 { ms(pv.min) } else { "-".into() }),
            ("Avg", pv.avg().map_or_else(|| "-".into(), ms)),
            ("Max", if pv.received() > 0 { ms(pv.max) } else { "-".into() }),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            let mut row = row_rect(i as i32);
            row.set_width(ETH_COL_W as u32);
            zebra(c, p, row, i);
            ctx.fs.draw_text(c, row.x() + 4, row.y() + 1, &i18n::tr_s(k), p.faint);
            let v = chrome::fit_text(ctx.fs, v, ETH_COL_W - 44);
            right_text(c, ctx, row.right() - 4, row.y(), &v, p.ink);
        }
        vsep(c, p, LIST_X + 40, 0);
        // Right panel: label row (state on the right), then the RTT graph, bars from the bottom.
        ctx.fs.draw_text(c, g.x(), BODY_Y + 1, &i18n::tr_s("RTT"), p.faint);
        right_text(c, ctx, g.right(), BODY_Y, &state, p.faint);
        let max = nice_ms(shown.iter().flatten().fold(0.0f32, |m, v| m.max(*v)));
        ui::fill_rect(c, g, p.paper);
        let h = g.height() as f32;
        let off = slots - shown.len();
        for (i, s) in shown.iter().enumerate() {
            let x = g.x() + ((off + i) * 2) as i32;
            match s {
                Some(v) => {
                    let bh = (((v / max) * h) as i32).max(1);
                    ui::fill_rect(c, Rect::new(x, g.bottom() - bh, 2, bh as u32), p.ink);
                }
                // A lost reply: a faint full-height bar.
                None => dither::fill(c, Rect::new(x, g.y() + 1, 2, g.height() - 1), p.ink, Pattern::Half),
            }
        }
        ui::draw_rect(c, g, p.ink);
        // The scale on the frame's top-left, over the bars.
        let top = ms(max);
        let tw = ctx.fs.measure_text(&top);
        ui::fill_rect(c, Rect::new(g.x() + 2, g.y() + 2, (tw + 2) as u32, 12), p.paper);
        ctx.fs.draw_text(c, g.x() + 3, g.y() + 2, &top, p.faint);
    }

    pub(super) fn draw_link(&mut self, c: &mut Canvas, ctx: &Ctx, p: &Pal) {
        let lk = &self.link;
        let last = lk.samples.back().copied();
        let left = match (&lk.ssid, last.and_then(|s| s.signal)) {
            (Some(s), _) => format!("Link  {s}"),
            _ => "Link  not connected".to_string(),
        };
        let mut right = String::new();
        if let Some(s) = last {
            if let Some(sig) = s.signal {
                right.push_str(&format!("{sig}%"));
            }
            match s.rtt {
                Some(v) => right.push_str(&format!("  {v:.1} ms")),
                None if self.ping.router.is_some() || self.mock => right.push_str("  lost"),
                None => {}
            }
        }
        header_band(c, ctx, p, 0, &left, right.trim());
        // The plot: 0..100 % on the left scale, round trips as bars on their own scale.
        let g = Rect::new(LIST_X + 30, BODY_Y + ROW + 4, (list_w() - 30 - 6) as u32, (body_h() - ROW - 8) as u32);
        let slots = (g.width() / 2) as usize;
        let shown: Vec<LinkSample> = lk.samples.iter().skip(lk.samples.len().saturating_sub(slots)).copied().collect();
        ui::fill_rect(c, g, p.paper);
        let max_rtt = nice_ms(shown.iter().filter_map(|s| s.rtt).fold(0.0f32, f32::max));
        let h = g.height() as i32;
        let off = slots - shown.len();
        for (i, s) in shown.iter().enumerate() {
            let x = g.x() + ((off + i) * 2) as i32;
            if let Some(v) = s.rtt {
                let bh = ((v / max_rtt) * h as f32) as i32;
                if bh > 0 {
                    dither::fill(c, Rect::new(x, g.bottom() - bh, 2, bh as u32), p.ink, Pattern::Half);
                }
            }
        }
        let y_of = |sig: u8| g.bottom() - 1 - (sig as i32 * (h - 2)) / 100;
        let mut prev: Option<(i32, i32)> = None;
        for (i, s) in shown.iter().enumerate() {
            let x = g.x() + ((off + i) * 2) as i32 + 1;
            match s.signal {
                Some(sig) => {
                    let pt = (x, y_of(sig));
                    if let Some(q) = prev {
                        ui::line(c, q.0, q.1, pt.0, pt.1, p.ink);
                    }
                    ui::fill_rect(c, Rect::new(pt.0, pt.1, 1, 1), p.ink);
                    prev = Some(pt);
                }
                None => prev = None,
            }
        }
        ui::draw_rect(c, g, p.ink);
        for (v, y) in [("100%", g.y()), ("50%", g.y() + h / 2 - 7), ("0", g.bottom() - 13)] {
            let w = ctx.fs.measure_text(v);
            ctx.fs.draw_text(c, g.x() - 4 - w, y, v, p.faint);
        }
        let key = format!("{} {max_rtt:.0} ms", i18n::tr_s("bars: RTT up to"));
        let kw = ctx.fs.measure_text(&key);
        ui::fill_rect(c, Rect::new(g.right() - kw - 5, g.y() + 2, (kw + 3) as u32, 12), p.paper);
        ctx.fs.draw_text(c, g.right() - kw - 4, g.y() + 2, &key, p.faint);
    }

    /// The Wi-Fi page's channel view: a bar per channel (height = networks on it), 2.4 GHz and 5 GHz.
    pub(super) fn draw_wifi_channels(&mut self, c: &mut Canvas, ctx: &Ctx, p: &Pal) {
        let load = channel_load(&self.ports.wifi);
        let joined = self.ports.wifi.iter().find(|n| n.in_use).map(|n| n.chan);
        let right = match &self.ports.wifi_err {
            Some(e) => e.clone(),
            None => format!("{} networks", self.ports.wifi.len()),
        };
        header_band(c, ctx, p, 0, "Channel load  (V: list)", &right);
        let g = Rect::new(LIST_X + 4, BODY_Y + ROW + 4, (list_w() - 8) as u32, (body_h() - ROW - 8 - 16) as u32);
        ui::fill_rect(c, g, p.paper);
        if load.is_empty() {
            text_row(c, ctx, g.x() + 4, g.y() + 2, g.width() as i32 - 8, "No networks found.", p.faint);
            ui::draw_rect(c, g, p.ink);
            return;
        }
        // Slots: the 2.4 GHz channels 1..=13 first, then every 5 GHz channel seen.
        let mut slots: Vec<u32> = (1..=13).collect();
        slots.extend(load.iter().map(|e| e.0).filter(|&ch| ch > 14));
        let n = slots.len() as i32;
        let slot_w = (g.width() as i32 / n).max(3);
        let top = load.iter().map(|e| e.1).max().unwrap_or(1).max(3);
        let unit = (g.height() as i32 - 4) / top as i32;
        for (i, ch) in slots.iter().enumerate() {
            let x = g.x() + 1 + i as i32 * slot_w;
            if let Some(&(_, count, strongest)) = load.iter().find(|e| e.0 == *ch) {
                let bh = unit * count as i32;
                let w = (slot_w - 2).max(1) as u32;
                let r = Rect::new(x + 1, g.bottom() - 1 - bh, w, bh as u32);
                ui::fill_rect(c, r, p.ink);
                // A strong neighbour (> 60 %) is outlined so the busy channels stand out.
                if strongest > 60 {
                    ui::draw_rect(c, Rect::new(r.x() - 1, r.y() - 1, r.width() + 2, r.height() + 1), p.ink);
                }
            }
            if Some(*ch) == joined {
                ui::fill_rect(c, Rect::new(x + 1, g.bottom() + 1, (slot_w - 2).max(1) as u32, 2), p.ink);
            }
            // Channel numbers under the bars: every channel on a wide slot, else every other.
            if slot_w >= 14 || i % 2 == 0 {
                let s = ch.to_string();
                let w = ctx.fs.measure_text(&s);
                ctx.fs.draw_text(c, x + (slot_w - w) / 2, g.bottom() + 2, &s, p.faint);
            }
        }
        ui::draw_rect(c, g, p.ink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(chan: u32, signal: u8) -> WifiNet {
        WifiNet { in_use: false, ssid: format!("n{chan}"), bssid: String::new(), chan, signal, security: "WPA2".into() }
    }

    #[test]
    fn channel_load_counts_and_keeps_the_strongest() {
        let v = channel_load(&[net(6, 40), net(1, 70), net(6, 62), net(36, 30), net(0, 99)]);
        assert_eq!(v, vec![(1, 1, 70), (6, 2, 62), (36, 1, 30)]);
    }

    #[test]
    fn the_rtt_scale_steps_5_10_20_50() {
        assert_eq!([nice_ms(0.0), nice_ms(5.0), nice_ms(5.1), nice_ms(17.5), nice_ms(30.0), nice_ms(51.0), nice_ms(180.0), nice_ms(1500.0)], [5.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 2000.0]);
    }

    #[test]
    fn ping_numbers_add_up() {
        let mut p = PingView::new();
        for ms in [Some(2.0), None, Some(4.0), Some(6.0)] {
            p.record(ms);
        }
        assert_eq!((p.sent, p.lost, p.received()), (4, 1, 3));
        assert_eq!(p.loss_pct(), 25.0);
        assert_eq!((p.min, p.max, p.avg()), (2.0, 6.0, Some(4.0)));
        p.reset();
        assert_eq!((p.sent, p.avg()), (0, None));
    }

    #[test]
    fn targets_cycle_router_dns_internet() {
        let mut p = PingView::new();
        p.router = Some("192.168.3.1".into());
        p.dns = vec!["192.168.3.1".into(), "8.8.8.8".into()];
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(p.host().unwrap());
            p.next_target();
        }
        assert_eq!(seen, ["192.168.3.1", "192.168.3.1", "8.8.8.8", "1.1.1.1"]);
        p.dns.clear();
        p.target = Target::Router;
        p.next_target();
        assert_eq!(p.target, Target::Internet, "no DNS servers: skip them");
    }

    #[test]
    fn the_mock_is_deterministic_and_loses_some() {
        assert_eq!(mock_ping_ms(5), mock_ping_ms(5));
        assert!((0..40).any(|n| mock_ping_ms(n).is_none()));
        assert!((0..40).filter_map(mock_ping_ms).all(|v| v > 0.0 && v < 30.0));
        assert!((0..100).all(|n| mock_link(n).signal.map_or(false, |s| (40..=100).contains(&s))));
    }

    #[test]
    fn a_mock_run_ends_after_its_count() {
        let mut a = Interceptor::new(true);
        a.set_page(PAGE_PING);
        a.ping.router = Some("192.168.3.1".into());
        a.ping.net_known = true;
        a.ping.count_idx = 0; // 10
        a.ping_toggle();
        assert!(a.ping.running);
        let mut t = Instant::now();
        for _ in 0..20 {
            t += Duration::from_secs(2);
            a.mock_net_tick(t);
        }
        assert_eq!(a.ping.sent, 10);
        assert!(!a.ping.running && a.ping.done());
    }

    #[test]
    fn keys_pick_the_target_and_the_count() {
        let fs = crate::font::FontSystem::new();
        let tm = crate::theme::ThemeManager::new(crate::theme::Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap());
        let sys = crate::system::mock::MockBackend::new();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let mut a = Interceptor::with_side(true, Side::Wireless);
        a.set_page(PAGE_PING);
        a.tick(&ctx); // reads Settings > Network's router and DNS
        assert_eq!(a.ping.router.as_deref(), Some("192.168.3.1"));
        a.key(Keycode::I, Mod::NOMOD, false, &ctx);
        assert_eq!(a.ping.host().as_deref(), Some("1.1.1.1"));
        a.key(Keycode::D, Mod::NOMOD, false, &ctx);
        assert_eq!(a.ping.target, Target::Dns(0));
        a.key(Keycode::R, Mod::NOMOD, false, &ctx);
        assert_eq!(a.ping.target, Target::Router);
        a.key(Keycode::N, Mod::NOMOD, false, &ctx);
        assert_eq!(a.ping.count(), 30);
        a.key(Keycode::Return, Mod::NOMOD, false, &ctx);
        assert!(a.ping.running);
        a.key(Keycode::Return, Mod::NOMOD, false, &ctx);
        assert!(!a.ping.running);
        a.set_page(PAGE_WIFI);
        a.key(Keycode::V, Mod::NOMOD, false, &ctx);
        assert!(a.wifi_chan);
    }

    #[test]
    fn late_events_of_an_old_run_are_ignored() {
        let mut a = Interceptor::new(true);
        a.ping.router = Some("10.0.0.1".into());
        a.ping.running = true;
        let old = a.ping.run;
        a.ping.reset();
        a.ping.running = true;
        a.apply_net(Ev::Ping(old, Some(1.0)));
        assert_eq!(a.ping.sent, 0);
        a.apply_net(Ev::Ping(a.ping.run, Some(1.0)));
        assert_eq!(a.ping.sent, 1);
    }
}
