//! System backend: the settings the official launcher controls through its
//! in-process `cp0_signal_*` API, re-implemented against plain Linux interfaces
//! (sysfs, PipeWire `wpctl`, NetworkManager `nmcli`, BlueZ `bluetoothctl`).
//!
//! The UI only talks to the [`Backend`] trait, so it runs unchanged on a Mac
//! with [`mock::MockBackend`].

pub mod config;
pub mod linux;
pub mod mock;
pub mod parse;
pub mod telemetry;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    pub percent: Option<u8>,
    pub charging: bool,
    pub voltage_mv: Option<u32>,
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiStatus {
    pub radio: bool,
    pub ssid: Option<String>,
    pub ip: Option<String>,
    /// 0..=100
    pub signal: Option<u8>,
    /// DNS servers of the Wi-Fi connection, in order (empty when unknown).
    pub dns: Vec<String>,
    /// Default gateway (the router).
    pub gateway: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ap {
    pub ssid: String,
    pub signal: u8,
    pub secure: bool,
    pub in_use: bool,
    pub saved: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtDevice {
    pub addr: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtPort {
    /// Grove connector 5 V output.
    Grove5V,
    /// Cap / external port 5 V output.
    Ext5V,
}

#[derive(Clone, Debug, Default)]
pub struct SysInfo {
    pub hostname: String,
    /// Distribution name (`PRETTY_NAME`).
    pub os: String,
    pub kernel: String,
    /// Board model (device tree) and CPU summary.
    pub model: String,
    pub cpu: String,
    pub disk_total_mb: u64,
    pub disk_free_mb: u64,
    pub uptime_s: u64,
    pub mem_total_kb: u64,
    pub mem_avail_kb: u64,
    pub temp_c: Option<f32>,
    pub load1: f32,
}

pub type R<T> = Result<T, String>;

pub trait Backend: Send + Sync {
    fn battery(&self) -> Battery;

    /// `(value, max)`
    fn brightness(&self) -> Option<(u32, u32)>;
    fn set_brightness(&self, value: u32) -> R<()>;

    /// `(percent, muted)`
    fn volume(&self) -> Option<(u8, bool)>;
    fn set_volume(&self, percent: u8) -> R<()>;
    fn set_mute(&self, muted: bool) -> R<()>;

    fn wifi_status(&self) -> WifiStatus;
    fn wifi_set_radio(&self, on: bool) -> R<()>;
    /// Airplane mode: on when Wi-Fi and Bluetooth are both off.
    fn airplane(&self) -> bool;
    /// On switches every radio off (remembering Wi-Fi/Bluetooth); off restores what was on before.
    fn set_airplane(&self, on: bool) -> R<()>;
    /// Blocking (several seconds): call from a worker thread.
    fn wifi_scan(&self) -> R<Vec<Ap>>;
    /// Blocking: call from a worker thread.
    fn wifi_connect(&self, ssid: &str, password: Option<&str>) -> R<()>;
    fn eth_ip(&self) -> Option<String>;

    fn bt_powered(&self) -> bool;
    fn bt_set_power(&self, on: bool) -> R<()>;
    /// Blocking (about 6 s): call from a worker thread.
    fn bt_scan(&self) -> R<Vec<BtDevice>>;
    /// Pair, trust and connect. Blocking.
    fn bt_connect(&self, addr: &str) -> R<()>;
    fn bt_disconnect(&self, addr: &str) -> R<()>;

    fn ext_port(&self, port: ExtPort) -> Option<bool>;
    fn set_ext_port(&self, port: ExtPort, on: bool) -> R<()>;

    /// e.g. `2026-11-16 23:05` and the time zone name.
    fn clock_text(&self) -> String;
    fn timezone(&self) -> String;
    fn ntp_synced(&self) -> Option<bool>;

    fn sysinfo(&self) -> SysInfo;
    /// One snapshot of counters for the System Monitor (cheap: a few small reads).
    fn telemetry(&self) -> telemetry::Telemetry {
        telemetry::Telemetry::default()
    }
    fn upgradable_count(&self) -> R<usize>;

    fn reboot(&self) -> R<()>;
    fn poweroff(&self) -> R<()>;
    /// Stop Harbour and start the official launcher service.
    fn switch_to_official_launcher(&self) -> R<()>;
}
