//! Real backend for Cardputer Zero (Debian trixie on a Raspberry Pi CM0).
//!
//! Interfaces used (all reachable by the `pi` user without sudo, verified on a
//! device unless noted):
//! - brightness: `/sys/class/backlight/backlight/brightness` (group `video`)
//! - 5 V ports: `/sys/class/leds/{grove_5v_out,ext_5v_out}/brightness` (group `gpio`)
//! - battery: `/sys/class/power_supply/bq27220-0/*` (read only)
//! - volume: PipeWire `wpctl`
//! - Wi-Fi: NetworkManager `nmcli` (group `netdev`)
//! - Bluetooth: BlueZ `bluetoothctl` (permissions not yet verified)
//! - reboot / power off: `systemctl` (may need a privileged helper)

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{parse, telemetry, Ap, Backend, Battery, BtDevice, ExtPort, SysInfo, WifiStatus, R};

pub struct LinuxBackend {
    /// Filesystem root; `/` on a device, a temp tree in tests.
    root: PathBuf,
    /// Harbour's data folder (`HARBOUR_DATA`, else `~/.local/share/harbour`): the airplane-mode memory lives here.
    data: PathBuf,
}

impl LinuxBackend {
    /// After the radio comes on, ask NetworkManager to join a known network now instead of
    /// waiting for its own retry. Errors ("no suitable connection", already connected) are ignored.
    fn wifi_autoconnect(&self) {
        let devs = run("nmcli", &["-t", "-f", "DEVICE,TYPE", "dev"], SHORT).unwrap_or_default();
        let dev = devs.lines().map(parse::split_terse).find(|f| f.len() >= 2 && f[1] == "wifi").map(|f| f[0].clone());
        if let Some(d) = dev {
            let _ = run("nmcli", &["dev", "connect", &d], Duration::from_secs(30));
        }
    }

    pub fn new() -> Self {
        let data = std::env::var_os("HARBOUR_DATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/harbour")))
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        Self { root: PathBuf::from("/"), data }
    }

    #[cfg(test)]
    pub fn with_root(root: PathBuf) -> Self {
        let data = root.join("data");
        Self { root, data }
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.root.join(rel.trim_start_matches('/'))
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.p(rel)).ok()
    }

    fn first_dir(&self, rel: &str, must_have: &str) -> Option<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(self.p(rel))
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join(must_have).exists())
            .collect();
        dirs.sort();
        dirs.into_iter().next()
    }

    fn led_path(&self, port: ExtPort) -> PathBuf {
        let name = match port {
            ExtPort::Grove5V => "grove_5v_out",
            ExtPort::Ext5V => "ext_5v_out",
        };
        self.p(&format!("sys/class/leds/{name}/brightness"))
    }
}

/// Drain a child's stdout/stderr concurrently to prevent pipe-buffer deadlocks.
/// Keep at most 256 KiB for diagnostics while consuming all remaining output.
fn drain_pipe<R: Read + Send + 'static>(mut stream: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        const MAX_CAPTURE: usize = 256 * 1024;
        let mut output = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let keep = n.min(MAX_CAPTURE - output.len());
                    output.extend_from_slice(&buf[..keep]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        output
    })
}

/// Run a command with bounded output and a timeout.
fn run(prog: &str, args: &[&str], timeout: Duration) -> R<String> {
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        cmd.env("XDG_RUNTIME_DIR", format!("/run/user/{}", unsafe { libc::getuid() }));
    }
    let mut child = cmd.spawn().map_err(|e| format!("{prog}: {e}"))?;
    let stdout = child.stdout.take().map(drain_pipe);
    let stderr = child.stderr.take().map(drain_pipe);
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{prog}: timed out"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = stdout.and_then(|t| t.join().ok()).unwrap_or_default();
    let err = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
    if status.success() {
        Ok(String::from_utf8_lossy(&out).into_owned())
    } else {
        let message = String::from_utf8_lossy(&err);
        let msg = message.trim();
        Err(if msg.is_empty() { format!("{prog}: exit {:?}", status.code()) } else { msg.to_string() })
    }
}

const SHORT: Duration = Duration::from_secs(4);

/// The Cardputer Zero has only the bq27220 gauge (no charger node, no `online` flag): its `status` is
/// "Charging" / "Discharging" / "Not charging" (also shown when full on USB), derived from the current sign.
/// Be generous: "Charging", or a clearly positive current (> 20 mA, in µA), or external power online while
/// the battery is not yet full.
fn is_charging(status: &str, current_ua: Option<i64>, external_online: bool) -> bool {
    let s = status.trim();
    if s.eq_ignore_ascii_case("charging") {
        return true;
    }
    if s.eq_ignore_ascii_case("discharging") || s.eq_ignore_ascii_case("full") {
        return false;
    }
    current_ua.is_some_and(|c| c > 20_000) || (external_online && s.eq_ignore_ascii_case("not charging") && current_ua.is_none())
}

impl LinuxBackend {
    /// Some power_supply node of type Mains / USB* reports `online` = 1.
    fn external_power(&self) -> bool {
        let Ok(rd) = std::fs::read_dir(self.p("sys/class/power_supply")) else { return false };
        rd.flatten().any(|e| {
            let d = e.path();
            let ty = std::fs::read_to_string(d.join("type")).unwrap_or_default();
            let ty = ty.trim();
            (ty == "Mains" || ty.starts_with("USB")) && std::fs::read_to_string(d.join("online")).is_ok_and(|v| v.trim() == "1")
        })
    }
}

impl Backend for LinuxBackend {
    fn battery(&self) -> Battery {
        let Some(dir) = self.first_dir("sys/class/power_supply", "capacity") else {
            return Battery::default();
        };
        let rd = |f: &str| std::fs::read_to_string(dir.join(f)).ok().map(|s| s.trim().to_string());
        let status = rd("status").unwrap_or_default();
        Battery {
            percent: rd("capacity").and_then(|s| s.parse::<u8>().ok()).map(|p| p.min(100)),
            charging: is_charging(&status, rd("current_now").or_else(|| rd("current_instant")).and_then(|s| s.parse::<i64>().ok()), self.external_power()),
            voltage_mv: rd("voltage_now").and_then(|s| s.parse::<u64>().ok()).map(|uv| (uv / 1000) as u32),
            status,
        }
    }

    fn brightness(&self) -> Option<(u32, u32)> {
        let dir = self.first_dir("sys/class/backlight", "brightness")?;
        let v = std::fs::read_to_string(dir.join("brightness")).ok()?.trim().parse().ok()?;
        let m = std::fs::read_to_string(dir.join("max_brightness")).ok()?.trim().parse().ok()?;
        Some((v, m))
    }

    fn set_brightness(&self, value: u32) -> R<()> {
        let dir = self.first_dir("sys/class/backlight", "brightness").ok_or("no backlight")?;
        let max: u32 = std::fs::read_to_string(dir.join("max_brightness"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(100);
        std::fs::write(dir.join("brightness"), value.min(max).to_string()).map_err(|e| e.to_string())
    }

    fn volume(&self) -> Option<(u8, bool)> {
        parse::wpctl_volume(&run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"], SHORT).ok()?)
    }

    fn set_volume(&self, percent: u8) -> R<()> {
        let v = format!("{:.2}", percent.min(100) as f32 / 100.0);
        run("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &v], SHORT).map(|_| ())
    }

    fn set_mute(&self, muted: bool) -> R<()> {
        run("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", if muted { "1" } else { "0" }], SHORT).map(|_| ())
    }

    fn wifi_status(&self) -> WifiStatus {
        let radio = run("nmcli", &["radio", "wifi"], SHORT)
            .map(|s| s.trim() == "enabled")
            .unwrap_or(false);
        let mut st = WifiStatus { radio, ..Default::default() };
        if !radio {
            return st;
        }
        let devs = run("nmcli", &["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "dev"], SHORT).unwrap_or_default();
        let wifi = devs.lines().map(parse::split_terse).find(|f| f.len() >= 4 && f[1] == "wifi");
        if let Some(f) = wifi {
            if f[2] == "connected" {
                st.ssid = Some(f[3].clone());
                st.ip = run("nmcli", &["-g", "IP4.ADDRESS", "dev", "show", &f[0]], SHORT)
                    .ok()
                    .and_then(|s| s.lines().next().map(|l| l.split('/').next().unwrap_or("").to_string()))
                    .filter(|s| !s.is_empty());
                st.dns = run("nmcli", &["-g", "IP4.DNS", "dev", "show", &f[0]], SHORT).map(|s| parse::dns_list(&s)).unwrap_or_default();
                st.gateway = run("nmcli", &["-g", "IP4.GATEWAY", "dev", "show", &f[0]], SHORT).ok().and_then(|s| parse::gateway(&s));
                st.signal = run("nmcli", &["-t", "-f", "IN-USE,SIGNAL", "dev", "wifi", "list"], SHORT)
                    .ok()
                    .and_then(|s| {
                        s.lines().map(parse::split_terse).find(|f| f.len() >= 2 && f[0] == "*")
                            .and_then(|f| f[1].parse().ok())
                    });
            }
        }
        st
    }

    fn wifi_set_radio(&self, on: bool) -> R<()> {
        let r = run("nmcli", &["radio", "wifi", if on { "on" } else { "off" }], SHORT).map(|_| ());
        if on && r.is_ok() {
            self.wifi_autoconnect();
        }
        r
    }

    fn airplane(&self) -> bool {
        let wifi = run("nmcli", &["radio", "wifi"], SHORT).map(|s| s.trim() == "enabled").unwrap_or(false);
        !wifi && !self.bt_powered()
    }

    fn set_airplane(&self, on: bool) -> R<()> {
        let file = self.data.join("airplane-prev");
        if on {
            // Remember what was on (only when something is: a second "on" must not overwrite it).
            let wifi = run("nmcli", &["radio", "wifi"], SHORT).map(|s| s.trim() == "enabled").unwrap_or(false);
            let bt = self.bt_powered();
            if wifi || bt {
                let _ = std::fs::create_dir_all(&self.data);
                let _ = std::fs::write(&file, format!("wifi={}\nbt={}\n", wifi as u8, bt as u8));
            }
            let r = run("nmcli", &["radio", "all", "off"], SHORT).map(|_| ());
            let b = self.bt_set_power(false);
            r.and(b)
        } else {
            let (wifi, bt) = std::fs::read_to_string(&file).map(|s| parse::airplane_prev(&s)).unwrap_or((true, true));
            let _ = std::fs::remove_file(&file);
            let r = if wifi { self.wifi_set_radio(true) } else { Ok(()) };
            let b = if bt { self.bt_set_power(true) } else { Ok(()) };
            r.and(b)
        }
    }

    fn wifi_scan(&self) -> R<Vec<Ap>> {
        let saved = run("nmcli", &["-t", "-f", "NAME,TYPE", "con", "show"], SHORT)
            .map(|s| parse::saved_wifi_names(&s))
            .unwrap_or_default();
        let out = run(
            "nmcli",
            // `auto` rescans only when the last scan is older than ~30 s. A forced rescan right after
            // another one makes the driver wait about 15 s, which looked like a hung "Scanning".
            &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "dev", "wifi", "list", "--rescan", "auto"],
            Duration::from_secs(30),
        )?;
        let aps = parse::wifi_list(&out, &saved);
        if !aps.is_empty() {
            return Ok(aps);
        }
        // An empty answer (seen right after start-up, before the first scan had finished): ask
        // for a fresh scan and wait for it once.
        let out = run(
            "nmcli",
            &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "dev", "wifi", "list", "--rescan", "yes"],
            Duration::from_secs(30),
        )?;
        Ok(parse::wifi_list(&out, &saved))
    }

    fn wifi_connect(&self, ssid: &str, password: Option<&str>) -> R<()> {
        let t = Duration::from_secs(40);
        let saved = run("nmcli", &["-t", "-f", "NAME,TYPE", "con", "show"], SHORT)
            .map(|s| parse::saved_wifi_names(&s))
            .unwrap_or_default();
        match password {
            None if saved.iter().any(|n| n == ssid) => {
                run("nmcli", &["con", "up", "id", ssid], t).map(|_| ())
            }
            None => run("nmcli", &["dev", "wifi", "connect", ssid], t).map(|_| ()),
            // Note: nmcli takes the password as an argument; on this single-user
            // device that is visible only to the same user's `ps`.
            Some(pw) => run("nmcli", &["dev", "wifi", "connect", ssid, "password", pw], t).map(|_| ()),
        }
    }

    fn eth_ip(&self) -> Option<String> {
        let out = run("nmcli", &["-g", "IP4.ADDRESS", "dev", "show", "eth0"], SHORT).ok()?;
        out.lines().next().map(|l| l.split('/').next().unwrap_or("").to_string()).filter(|s| !s.is_empty())
    }

    fn lan_info(&self) -> Option<super::lan::LanInfo> {
        let route = run("ip", &["-4", "route", "show", "default"], SHORT).unwrap_or_default();
        let addrs = run("ip", &["-4", "-o", "addr", "show"], SHORT).ok()?;
        super::lan::pick_lan(&route, &addrs)
    }

    fn neighbors(&self) -> Vec<super::lan::Neighbor> {
        let v = run("ip", &["-4", "neigh", "show"], SHORT).map(|t| super::lan::parse_ip_neigh(&t)).unwrap_or_default();
        if v.is_empty() {
            return std::fs::read_to_string("/proc/net/arp").map(|t| super::lan::parse_proc_arp(&t)).unwrap_or_default();
        }
        v
    }

    fn bt_powered(&self) -> bool {
        run("bluetoothctl", &["show"], SHORT).map(|s| parse::bt_powered(&s)).unwrap_or(false)
    }

    fn bt_set_power(&self, on: bool) -> R<()> {
        run("bluetoothctl", &["power", if on { "on" } else { "off" }], Duration::from_secs(8)).map(|_| ())
    }

    fn bt_scan(&self) -> R<Vec<BtDevice>> {
        // `--timeout` keeps scanning for N seconds then exits.
        let _ = run("bluetoothctl", &["--timeout", "6", "scan", "on"], Duration::from_secs(12));
        let all = parse::bt_devices(&run("bluetoothctl", &["devices"], SHORT)?);
        let paired = run("bluetoothctl", &["devices", "Paired"], SHORT)
            .or_else(|_| run("bluetoothctl", &["paired-devices"], SHORT))
            .map(|s| parse::bt_devices(&s))
            .unwrap_or_default();
        let connected = run("bluetoothctl", &["devices", "Connected"], SHORT)
            .map(|s| parse::bt_devices(&s))
            .unwrap_or_default();
        Ok(parse::bt_merge(&all, &paired, &connected))
    }

    fn bt_connect(&self, addr: &str) -> R<()> {
        if !parse::valid_bt_addr(addr) {
            return Err("bad address".into());
        }
        let t = Duration::from_secs(25);
        let _ = run("bluetoothctl", &["pair", addr], t);
        let _ = run("bluetoothctl", &["trust", addr], SHORT);
        run("bluetoothctl", &["connect", addr], t).map(|_| ())
    }

    fn bt_disconnect(&self, addr: &str) -> R<()> {
        if !parse::valid_bt_addr(addr) {
            return Err("bad address".into());
        }
        run("bluetoothctl", &["disconnect", addr], Duration::from_secs(10)).map(|_| ())
    }

    fn ext_port(&self, port: ExtPort) -> Option<bool> {
        let v = std::fs::read_to_string(self.led_path(port)).ok()?;
        Some(v.trim() != "0")
    }

    fn set_ext_port(&self, port: ExtPort, on: bool) -> R<()> {
        std::fs::write(self.led_path(port), if on { "1" } else { "0" }).map_err(|e| e.to_string())
    }

    fn clock_text(&self) -> String {
        chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
    }

    fn timezone(&self) -> String {
        run("timedatectl", &["show", "-p", "Timezone", "--value"], SHORT)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    fn ntp_synced(&self) -> Option<bool> {
        run("timedatectl", &["show", "-p", "NTPSynchronized", "--value"], SHORT)
            .ok()
            .map(|s| s.trim() == "yes")
    }

    fn sysinfo(&self) -> SysInfo {
        let (mem_total_kb, mem_avail_kb) = self.read("proc/meminfo").map(|s| parse::meminfo(&s)).unwrap_or((0, 0));
        SysInfo {
            hostname: self.read("etc/hostname").map(|s| s.trim().to_string()).unwrap_or_default(),
            os: self.read("etc/os-release").map(|s| parse::os_pretty_name(&s)).unwrap_or_default(),
            model: self
                .read("proc/device-tree/model")
                .map(|s| s.trim_end_matches('\0').trim().to_string())
                .unwrap_or_default(),
            cpu: self.read("proc/cpuinfo").map(|s| parse::cpu_summary(&s)).unwrap_or_default(),
            disk_total_mb: disk_mb(&self.p("")).0,
            disk_free_mb: disk_mb(&self.p("")).1,
            kernel: self.read("proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default(),
            uptime_s: self
                .read("proc/uptime")
                .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
                .unwrap_or(0.0) as u64,
            mem_total_kb,
            mem_avail_kb,
            temp_c: self
                .read("sys/class/thermal/thermal_zone0/temp")
                .and_then(|s| s.trim().parse::<f32>().ok())
                .map(|m| m / 1000.0),
            load1: self
                .read("proc/loadavg")
                .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
                .unwrap_or(0.0),
        }
    }

    fn telemetry(&self) -> telemetry::Telemetry {
        let num = |rel: &str| self.read(rel).and_then(|s| s.trim().parse::<i64>().ok());
        let (disk_total_mb, disk_free_mb) = disk_mb(&self.p(""));
        let mut power = telemetry::PowerInfo::default();
        if let Some(dir) = self.first_dir("sys/class/power_supply", "capacity") {
            let rd = |f: &str| std::fs::read_to_string(dir.join(f)).ok().map(|s| s.trim().to_string());
            let n = |f: &str| rd(f).and_then(|s| s.parse::<i64>().ok());
            power = telemetry::PowerInfo {
                percent: n("capacity").map(|p| p.clamp(0, 100) as u8),
                voltage_mv: n("voltage_now").map(|uv| (uv / 1000).max(0) as u32),
                current_ma: n("current_now").map(|ua| (ua / 1000) as i32),
                status: rd("status").unwrap_or_default(),
                temp_c: n("temp").map(|t| t as f32 / 10.0),
                // seconds in sysfs; absent or an error while the gauge has no estimate
                time_to_empty_min: n("time_to_empty_now").filter(|s| *s > 0).map(|s| (s / 60) as u32),
                time_to_full_min: n("time_to_full_now").filter(|s| *s > 0).map(|s| (s / 60) as u32),
            };
        }
        let mut net = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.p("sys/class/net")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name == "lo" {
                    continue;
                }
                let st = e.path().join("statistics");
                let rd = |f: &str| std::fs::read_to_string(st.join(f)).ok().and_then(|s| s.trim().parse::<u64>().ok());
                let (Some(rx_bytes), Some(tx_bytes)) = (rd("rx_bytes"), rd("tx_bytes")) else { continue };
                let up = std::fs::read_to_string(e.path().join("operstate")).map(|s| s.trim() == "up").unwrap_or(false);
                net.push(telemetry::NetCounters { name, rx_bytes, tx_bytes, up });
            }
        }
        net.sort_by(|a, b| a.name.cmp(&b.name));
        // The firmware's throttle word lives in a platform device that differs by board.
        let throttled = std::fs::read_dir(self.p("sys/devices/platform/soc")).ok().and_then(|rd| {
            rd.flatten().find_map(|e| std::fs::read_to_string(e.path().join("get_throttled")).ok().and_then(|s| telemetry::parse_throttled(&s)))
        });
        telemetry::Telemetry {
            cpu: self.read("proc/stat").map(|s| telemetry::parse_stat(&s)).unwrap_or_default(),
            freq_mhz: num("sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq").map(|k| (k / 1000) as u32),
            freq_max_mhz: num("sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq").map(|k| (k / 1000) as u32),
            load: self.read("proc/loadavg").map(|s| telemetry::parse_loadavg(&s)).unwrap_or_default(),
            mem: self.read("proc/meminfo").map(|s| telemetry::parse_meminfo(&s)).unwrap_or_default(),
            temp_c: num("sys/class/thermal/thermal_zone0/temp").map(|m| m as f32 / 1000.0),
            throttled,
            power,
            net,
            disk_total_mb,
            disk_free_mb,
            uptime_s: self
                .read("proc/uptime")
                .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
                .unwrap_or(0.0) as u64,
        }
    }

    fn upgradable_count(&self) -> R<usize> {
        let out = run("apt", &["list", "--upgradable"], Duration::from_secs(30))?;
        Ok(out.lines().filter(|l| l.contains("upgradable from")).count())
    }

    fn reboot(&self) -> R<()> {
        run("systemctl", &["reboot"], Duration::from_secs(10))
            .or_else(|_| run("sudo", &["-n", "systemctl", "reboot"], Duration::from_secs(10)))
            .map(|_| ())
            .map_err(|_| "not permitted (needs a privileged helper)".to_string())
    }

    fn poweroff(&self) -> R<()> {
        run("systemctl", &["poweroff"], Duration::from_secs(10))
            .or_else(|_| run("sudo", &["-n", "systemctl", "poweroff"], Duration::from_secs(10)))
            .map(|_| ())
            .map_err(|_| "not permitted (needs a privileged helper)".to_string())
    }

    fn switch_to_official_launcher(&self) -> R<()> {
        // harbour-shell.service declares Conflicts=APPLaunch.service, so
        // starting the official unit stops Harbour.
        use std::os::unix::process::CommandExt;
        let mut c = Command::new("systemctl");
        c.args(["--user", "start", "APPLaunch.service"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        c.spawn().map(|_| ()).map_err(|e| e.to_string())
    }
}

#[allow(dead_code)]
fn _assert_path(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (PathBuf, LinuxBackend) {
        let root = std::env::temp_dir().join(format!("harbour-sysfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let w = |rel: &str, v: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, v).unwrap();
        };
        w("sys/class/backlight/backlight/brightness", "100\n");
        w("sys/class/backlight/backlight/max_brightness", "100\n");
        w("sys/class/power_supply/bq27220-0/capacity", "87\n");
        w("sys/class/power_supply/bq27220-0/status", "Charging\n");
        w("sys/class/power_supply/bq27220-0/voltage_now", "4187000\n");
        w("sys/class/leds/grove_5v_out/brightness", "0\n");
        w("sys/class/leds/ext_5v_out/brightness", "1\n");
        w("proc/meminfo", "MemTotal: 360000 kB\nMemAvailable: 143000 kB\n");
        w("proc/uptime", "842.5 100.0\n");
        w("proc/loadavg", "0.31 0.35 0.31 1/100 1\n");
        w("etc/hostname", "CardputerZero\n");
        w("sys/class/thermal/thermal_zone0/temp", "46200\n");
        let b = LinuxBackend::with_root(root.clone());
        (root, b)
    }

    #[test]
    fn charging_detection() {
        assert!(is_charging("Charging", None, false));
        assert!(!is_charging("Discharging", Some(50_000), true));
        assert!(!is_charging("Full", None, true));
        assert!(!is_charging("Not charging", Some(0), false), "the device at 100%: 0 mA");
        assert!(is_charging("Not charging", Some(120_000), false), "current flowing in");
        assert!(is_charging("Not charging", None, true), "USB online, no gauge current");
        assert!(!is_charging("Unknown", Some(-80_000), false));
    }

    #[test]
    fn battery_brightness_and_ports() {
        let (root, b) = tree();
        let bat = b.battery();
        assert_eq!(bat.percent, Some(87));
        assert!(bat.charging);
        assert_eq!(bat.voltage_mv, Some(4187));

        assert_eq!(b.brightness(), Some((100, 100)));
        b.set_brightness(35).unwrap();
        assert_eq!(b.brightness(), Some((35, 100)));
        b.set_brightness(900).unwrap();
        assert_eq!(b.brightness(), Some((100, 100)), "clamped to max");

        assert_eq!(b.ext_port(ExtPort::Grove5V), Some(false));
        assert_eq!(b.ext_port(ExtPort::Ext5V), Some(true));
        b.set_ext_port(ExtPort::Grove5V, true).unwrap();
        assert_eq!(b.ext_port(ExtPort::Grove5V), Some(true));

        let s = b.sysinfo();
        assert_eq!(s.hostname, "CardputerZero");
        assert_eq!(s.uptime_s, 842);
        assert_eq!(s.mem_avail_kb, 143000);
        assert_eq!(s.temp_c, Some(46.2));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_hardware_is_graceful() {
        let b = LinuxBackend::with_root(std::env::temp_dir().join("harbour-empty-root-xyz"));
        assert_eq!(b.battery(), Battery::default());
        assert_eq!(b.brightness(), None);
        assert_eq!(b.ext_port(ExtPort::Ext5V), None);
        assert!(b.set_brightness(10).is_err());
    }
    #[test]
    fn command_output_larger_than_pipe_buffer_completes() {
        let out = run(
            "sh",
            &["-c", "i=0; while [ $i -lt 10000 ]; do printf 01234567890123456789; i=$((i+1)); done"],
            Duration::from_secs(8),
        ).unwrap();
        assert_eq!(out.len(), 200000);
    }

    #[test]
    fn noisy_command_output_is_bounded_and_drained() {
        let out = run(
            "sh",
            &["-c", "i=0; while [ $i -lt 20000 ]; do printf 01234567890123456789; i=$((i+1)); done"],
            Duration::from_secs(10),
        ).unwrap();
        assert_eq!(out.len(), 256 * 1024);
    }

}

/// (total, free) MB of the filesystem holding `path` (0, 0 when unknown).
fn disk_mb(path: &std::path::Path) -> (u64, u64) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return (0, 0) };
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return (0, 0);
    }
    let unit = st.f_frsize as u64;
    ((st.f_blocks as u64 * unit) >> 20, (st.f_bavail as u64 * unit) >> 20)
}
