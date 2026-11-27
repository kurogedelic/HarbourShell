//! In-memory backend for developing the UI on a Mac (and for headless tests).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::telemetry::{CpuTimes, MemInfo, NetCounters, PowerInfo, Telemetry};

use super::{Ap, Backend, Battery, BtDevice, ExtPort, SysInfo, WifiStatus, R};

struct State {
    brightness: u32,
    volume: u8,
    muted: bool,
    wifi_on: bool,
    ssid: Option<String>,
    bt_on: bool,
    grove: bool,
    ext: bool,
    bt_connected: Vec<String>,
    /// (Wi-Fi, Bluetooth) before airplane mode was switched on.
    prev_radios: Option<(bool, bool)>,
}

pub struct MockBackend {
    s: Mutex<State>,
    /// Artificial latency for blocking calls so spinners can be exercised.
    pub delay: Duration,
    start: Instant,
    /// Every value passed to `set_brightness`, in order (for tests).
    brightness_calls: Mutex<Vec<u32>>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            s: Mutex::new(State {
                brightness: 80,
                volume: 30,
                muted: false,
                wifi_on: true,
                ssid: Some("Harbour Cafe".into()),
                bt_on: true,
                grove: false,
                ext: false,
                bt_connected: vec![],
                prev_radios: None,
            }),
            delay: Duration::from_millis(400),
            start: Instant::now(),
            brightness_calls: Mutex::new(Vec::new()),
        }
    }

    /// The values `set_brightness` was called with.
    pub fn brightness_log(&self) -> Vec<u32> {
        self.brightness_calls.lock().unwrap().clone()
    }
}

impl Backend for MockBackend {
    fn battery(&self) -> Battery {
        // HARBOUR_MOCK_CHARGING=1 pretends the USB cable is in (screenshots).
        let charging = matches!(std::env::var("HARBOUR_MOCK_CHARGING").as_deref(), Ok("1" | "true" | "yes" | "on"));
        let status = if charging { "Charging" } else { "Discharging" };
        Battery { percent: Some(76), charging, voltage_mv: Some(3980), status: status.into() }
    }
    fn brightness(&self) -> Option<(u32, u32)> {
        Some((self.s.lock().unwrap().brightness, 100))
    }
    fn set_brightness(&self, v: u32) -> R<()> {
        self.brightness_calls.lock().unwrap().push(v.min(100));
        self.s.lock().unwrap().brightness = v.min(100);
        Ok(())
    }
    fn volume(&self) -> Option<(u8, bool)> {
        let s = self.s.lock().unwrap();
        Some((s.volume, s.muted))
    }
    fn set_volume(&self, p: u8) -> R<()> {
        self.s.lock().unwrap().volume = p.min(100);
        Ok(())
    }
    fn set_mute(&self, m: bool) -> R<()> {
        self.s.lock().unwrap().muted = m;
        Ok(())
    }
    fn wifi_status(&self) -> WifiStatus {
        let s = self.s.lock().unwrap();
        WifiStatus {
            radio: s.wifi_on,
            ssid: if s.wifi_on { s.ssid.clone() } else { None },
            ip: (s.wifi_on && s.ssid.is_some()).then(|| "192.168.3.94".to_string()),
            signal: (s.wifi_on && s.ssid.is_some()).then_some(72),
            dns: if s.wifi_on && s.ssid.is_some() { vec!["192.168.3.1".to_string()] } else { vec![] },
            gateway: (s.wifi_on && s.ssid.is_some()).then(|| "192.168.3.1".to_string()),
        }
    }
    fn wifi_set_radio(&self, on: bool) -> R<()> {
        self.s.lock().unwrap().wifi_on = on;
        Ok(())
    }
    fn airplane(&self) -> bool {
        let s = self.s.lock().unwrap();
        !s.wifi_on && !s.bt_on
    }
    fn set_airplane(&self, on: bool) -> R<()> {
        let mut s = self.s.lock().unwrap();
        if on {
            if s.wifi_on || s.bt_on {
                s.prev_radios = Some((s.wifi_on, s.bt_on));
            }
            s.wifi_on = false;
            s.bt_on = false;
        } else {
            let (w, b) = s.prev_radios.take().unwrap_or((true, true));
            s.wifi_on = w;
            s.bt_on = b;
        }
        Ok(())
    }
    fn wifi_scan(&self) -> R<Vec<Ap>> {
        std::thread::sleep(self.delay);
        // HARBOUR_MOCK_NO_WIFI=1: a scan that finds nothing (to see the empty page).
        if std::env::var_os("HARBOUR_MOCK_NO_WIFI").is_some() {
            return Ok(Vec::new());
        }
        let cur = self.s.lock().unwrap().ssid.clone();
        let mk = |n: &str, sig, sec, saved| Ap {
            ssid: n.into(),
            signal: sig,
            secure: sec,
            in_use: cur.as_deref() == Some(n),
            saved,
        };
        Ok(vec![
            mk("Harbour Cafe", 72, true, true),
            mk("Pier 9 Guest", 55, false, false),
            mk("Lighthouse 2G", 41, true, false),
        ])
    }
    fn wifi_connect(&self, ssid: &str, password: Option<&str>) -> R<()> {
        std::thread::sleep(self.delay);
        if ssid == "Lighthouse 2G" && password != Some("anchor123") {
            return Err("Secrets were required, but not provided".into());
        }
        self.s.lock().unwrap().ssid = Some(ssid.to_string());
        Ok(())
    }
    fn eth_ip(&self) -> Option<String> {
        None
    }
    fn lan_info(&self) -> Option<super::lan::LanInfo> {
        let s = self.s.lock().unwrap();
        (s.wifi_on && s.ssid.is_some()).then(super::lan::mock_info)
    }
    fn neighbors(&self) -> Vec<super::lan::Neighbor> {
        super::lan::mock_neighbors()
    }
    fn bt_powered(&self) -> bool {
        self.s.lock().unwrap().bt_on
    }
    fn bt_set_power(&self, on: bool) -> R<()> {
        self.s.lock().unwrap().bt_on = on;
        Ok(())
    }
    fn bt_scan(&self) -> R<Vec<BtDevice>> {
        std::thread::sleep(self.delay);
        let c = self.s.lock().unwrap().bt_connected.clone();
        let mk = |a: &str, n: &str, paired| BtDevice {
            addr: a.into(),
            name: n.into(),
            paired,
            connected: c.iter().any(|x| x == a),
        };
        Ok(vec![mk("AA:BB:CC:DD:EE:01", "Zero Buds", true), mk("AA:BB:CC:DD:EE:02", "Keyboard K380", false)])
    }
    fn bt_connect(&self, addr: &str) -> R<()> {
        std::thread::sleep(self.delay);
        self.s.lock().unwrap().bt_connected.push(addr.to_string());
        Ok(())
    }
    fn bt_disconnect(&self, addr: &str) -> R<()> {
        self.s.lock().unwrap().bt_connected.retain(|a| a != addr);
        Ok(())
    }
    fn ext_port(&self, p: ExtPort) -> Option<bool> {
        let s = self.s.lock().unwrap();
        Some(if p == ExtPort::Grove5V { s.grove } else { s.ext })
    }
    fn set_ext_port(&self, p: ExtPort, on: bool) -> R<()> {
        let mut s = self.s.lock().unwrap();
        if p == ExtPort::Grove5V {
            s.grove = on
        } else {
            s.ext = on
        }
        Ok(())
    }
    fn clock_text(&self) -> String {
        crate::rt::now().format("%Y-%m-%d %H:%M").to_string()
    }
    fn timezone(&self) -> String {
        "Asia/Tokyo".into()
    }
    fn ntp_synced(&self) -> Option<bool> {
        Some(true)
    }
    fn sysinfo(&self) -> SysInfo {
        SysInfo {
            hostname: "CardputerZero".into(),
            os: "Debian GNU/Linux 13 (trixie)".into(),
            model: "Raspberry Pi Compute Module 0".into(),
            cpu: "4 x ARMv8 Processor rev 4".into(),
            disk_total_mb: 14_800,
            disk_free_mb: 9_200,
            kernel: "6.18.39+rpt-rpi-v8".into(),
            uptime_s: 3725,
            mem_total_kb: 360_000,
            mem_avail_kb: 143_000,
            temp_c: Some(46.2),
            load1: 0.31,
        }
    }
    fn telemetry(&self) -> Telemetry {
        // Everything is a smooth function of the time since start, so counters grow like the
        // real ones and the monitor's deltas give plausible, moving rates.
        // (Frozen runs stand at one point of the curves, so every sample is the same.)
        let t = if crate::rt::frozen() { 40.0 } else { self.start.elapsed().as_secs_f64() + 40.0 };
        let cores = 4;
        let hz = 10_000.0; // a fast fake jiffy clock, so even back-to-back snapshots differ
        let mut cpu = vec![CpuTimes::default()];
        let (mut sb, mut st) = (0.0, 0.0);
        for i in 0..cores {
            let k = i as f64;
            // utilisation u(t) = 0.30 + 0.25 sin(t/7 + k) + 0.10 sin(t/2.3 + 2k); integral in closed form
            let busy = hz * (0.30 * t - 0.25 * 7.0 * ((t / 7.0 + k).cos() - k.cos()) - 0.10 * 2.3 * ((t / 2.3 + 2.0 * k).cos() - (2.0 * k).cos()));
            let total = hz * t;
            sb += busy;
            st += total;
            cpu.push(CpuTimes { busy: busy.max(0.0) as u64, total: total as u64 });
        }
        cpu[0] = CpuTimes { busy: sb as u64, total: st as u64 };
        let kb = |mb: f64| (mb * 1024.0) as u64;
        let used = 140.0 + 18.0 * (t / 19.0).sin();
        let cache = 96.0 + 6.0 * (t / 31.0).sin();
        let mem = MemInfo {
            total_kb: 360_000,
            free_kb: 360_000 - kb(used) - kb(cache),
            avail_kb: 360_000 - kb(used),
            buffers_kb: kb(8.0),
            cached_kb: kb(cache - 8.0),
            swap_total_kb: 102_400,
            swap_free_kb: 102_400 - 2_400,
        };
        // rates: rx 20 KB/s +- 18, tx 6 KB/s +- 5 on wlan0; eth0 idle with bursts
        let rx = 20e3 * t + 18e3 * 5.0 * (1.0 - (t / 5.0).cos());
        let tx = 6e3 * t + 5e3 * 3.0 * (1.0 - (t / 3.0).cos());
        let burst = 1.5e3 * t;
        Telemetry {
            cpu,
            freq_mhz: Some(if (t / 11.0).sin() > 0.3 { 1000 } else { 600 }),
            freq_max_mhz: Some(1000),
            load: [0.31 + 0.2 * (t / 13.0).sin().abs() as f32, 0.25, 0.20],
            mem,
            temp_c: Some((46.0 + 5.0 * (t / 17.0).sin() + 1.5 * (t / 4.0).sin()) as f32),
            throttled: Some(0),
            power: PowerInfo {
                percent: Some(76),
                voltage_mv: Some(3980),
                current_ma: Some(-(310.0 + 60.0 * (t / 6.0).sin()) as i32),
                status: "Discharging".into(),
                temp_c: Some(33.4),
                time_to_empty_min: Some(192),
                time_to_full_min: None,
            },
            net: vec![
                NetCounters { name: "eth0".into(), rx_bytes: burst as u64, tx_bytes: (burst / 4.0) as u64, up: false },
                NetCounters { name: "wlan0".into(), rx_bytes: rx as u64, tx_bytes: tx as u64, up: true },
            ],
            disk_total_mb: 14_800,
            disk_free_mb: 9_200,
            uptime_s: 3725 + t as u64,
        }
    }
    fn upgradable_count(&self) -> R<usize> {
        std::thread::sleep(self.delay);
        Ok(146)
    }
    fn reboot(&self) -> R<()> {
        Err("mock: reboot disabled".into())
    }
    fn poweroff(&self) -> R<()> {
        Err("mock: power off disabled".into())
    }
    fn switch_to_official_launcher(&self) -> R<()> {
        Err("mock: not available".into())
    }
}
