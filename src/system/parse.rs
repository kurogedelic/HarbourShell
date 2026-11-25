//! Pure parsers for the command output the Linux backend consumes.

use super::{Ap, BtDevice};

/// Split one `nmcli -t` line on unescaped `:` (`\:` and `\\` are literals).
pub fn split_terse(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ':' => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// `Volume: 0.30` / `Volume: 0.30 [MUTED]` → `(30, muted)`.
pub fn wpctl_volume(text: &str) -> Option<(u8, bool)> {
    let rest = text.trim().strip_prefix("Volume:")?.trim();
    let num = rest.split_whitespace().next()?;
    let v: f32 = num.parse().ok()?;
    Some(((v * 100.0).round().clamp(0.0, 150.0) as u8, rest.contains("[MUTED]")))
}

/// Output of `nmcli -t -f IN-USE,SSID,SIGNAL,SECURITY dev wifi list`.
/// Hidden SSIDs are dropped; duplicates keep the strongest signal.
pub fn wifi_list(out: &str, saved: &[String]) -> Vec<Ap> {
    let mut aps: Vec<Ap> = Vec::new();
    for line in out.lines() {
        let f = split_terse(line);
        if f.len() < 4 || f[1].is_empty() {
            continue;
        }
        let ap = Ap {
            ssid: f[1].clone(),
            signal: f[2].parse().unwrap_or(0),
            secure: !f[3].trim().is_empty() && f[3].trim() != "--",
            in_use: f[0].trim() == "*",
            saved: saved.iter().any(|s| s == &f[1]),
        };
        match aps.iter_mut().find(|a| a.ssid == ap.ssid) {
            Some(existing) => {
                let keep_in_use = existing.in_use || ap.in_use;
                if ap.signal > existing.signal {
                    *existing = ap;
                }
                existing.in_use = keep_in_use;
            }
            None => aps.push(ap),
        }
    }
    aps.sort_by(|a, b| b.in_use.cmp(&a.in_use).then(b.signal.cmp(&a.signal)));
    aps
}

/// Names of saved Wi-Fi profiles from `nmcli -t -f NAME,TYPE con show`.
pub fn saved_wifi_names(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| {
            let f = split_terse(l);
            (f.len() >= 2 && (f[1] == "802-11-wireless" || f[1] == "wifi")).then(|| f[0].clone())
        })
        .collect()
}

/// `Device AA:BB:CC:DD:EE:FF Name` lines from `bluetoothctl devices`.
pub fn bt_devices(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("Device ")?;
            let (addr, name) = rest.split_once(' ').unwrap_or((rest, ""));
            valid_bt_addr(addr).then(|| (addr.to_string(), name.trim().to_string()))
        })
        .collect()
}

pub fn valid_bt_addr(a: &str) -> bool {
    let p: Vec<&str> = a.split(':').collect();
    p.len() == 6 && p.iter().all(|x| x.len() == 2 && x.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Combine the three views into [`BtDevice`] records.
pub fn bt_merge(all: &[(String, String)], paired: &[(String, String)], connected: &[(String, String)]) -> Vec<BtDevice> {
    all.iter()
        .map(|(addr, name)| BtDevice {
            addr: addr.clone(),
            name: if name.is_empty() { addr.clone() } else { name.clone() },
            paired: paired.iter().any(|(a, _)| a == addr),
            connected: connected.iter().any(|(a, _)| a == addr),
        })
        .collect()
}

/// `Powered: yes` in `bluetoothctl show`.
pub fn bt_powered(show: &str) -> bool {
    show.lines().any(|l| l.trim() == "Powered: yes")
}

/// `meminfo` → (total, available) in kB.
pub fn meminfo(text: &str) -> (u64, u64) {
    let get = |key: &str| {
        text.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    (get("MemTotal:"), get("MemAvailable:"))
}

/// `/etc/os-release` → PRETTY_NAME.
pub fn os_pretty_name(text: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches('"').to_string())
        .unwrap_or_default()
}

/// `/proc/cpuinfo` → "4 x ARMv8 Processor rev 4 (v8l)" (core count and model name).
pub fn cpu_summary(text: &str) -> String {
    let cores = text.lines().filter(|l| l.starts_with("processor")).count();
    let name = text
        .lines()
        .find(|l| l.starts_with("model name") || l.starts_with("Hardware"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default();
    match (cores, name.is_empty()) {
        (0, true) => String::new(),
        (0, false) => name,
        (n, true) => format!("{n} cores"),
        (n, false) => format!("{n} x {name}"),
    }
}

/// `nmcli -g IP4.DNS dev show` output (one server per line, or `|`-joined) → the addresses.
pub fn dns_list(text: &str) -> Vec<String> {
    text.lines()
        .flat_map(|l| l.split(|c| c == '|' || c == ','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `nmcli -g IP4.GATEWAY dev show` output → the gateway (`--` and empty mean none).
pub fn gateway(text: &str) -> Option<String> {
    text.lines().map(str::trim).find(|l| !l.is_empty() && *l != "--").map(String::from)
}

/// The airplane-mode memory file (`wifi=1`, `bt=0`) → (Wi-Fi was on, Bluetooth was on); a missing key means on.
pub fn airplane_prev(text: &str) -> (bool, bool) {
    let get = |k: &str| text.lines().find_map(|l| l.trim().strip_prefix(k)).map_or(true, |v| v.trim() != "0");
    (get("wifi="), get("bt="))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_and_gateway() {
        assert_eq!(dns_list("192.168.3.1\n8.8.8.8\n"), ["192.168.3.1", "8.8.8.8"]);
        assert_eq!(dns_list("1.1.1.1 | 9.9.9.9"), ["1.1.1.1", "9.9.9.9"]);
        assert!(dns_list("\n").is_empty());
        assert_eq!(gateway("192.168.3.1\n"), Some("192.168.3.1".into()));
        assert_eq!(gateway("--\n"), None);
        assert_eq!(gateway(""), None);
        assert_eq!(airplane_prev("wifi=1\nbt=0\n"), (true, false));
        assert_eq!(airplane_prev("wifi=0\n"), (false, true));
        assert_eq!(airplane_prev(""), (true, true));
    }

    #[test]
    fn terse_escapes() {
        assert_eq!(split_terse(r"*:My\:Net:80:WPA2"), ["*", "My:Net", "80", "WPA2"]);
        assert_eq!(split_terse(r"a\\b:c"), [r"a\b", "c"]);
    }

    #[test]
    fn volume() {
        assert_eq!(wpctl_volume("Volume: 0.30\n"), Some((30, false)));
        assert_eq!(wpctl_volume("Volume: 0.55 [MUTED]"), Some((55, true)));
        assert_eq!(wpctl_volume("garbage"), None);
    }

    #[test]
    fn wifi_dedup_sort_and_security() {
        let out = " :Cafe:40:WPA2\n*:Home:70:WPA2 WPA3\n :Home:55:WPA2\n :Open:90:\n ::10:WPA2\n";
        let aps = wifi_list(out, &["Cafe".to_string()]);
        let names: Vec<_> = aps.iter().map(|a| a.ssid.as_str()).collect();
        assert_eq!(names, ["Home", "Open", "Cafe"], "in-use first, then signal; hidden dropped");
        assert!(aps[0].in_use && aps[0].secure && !aps[0].saved);
        assert!(!aps[1].secure);
        assert!(aps[2].saved);
    }

    #[test]
    fn saved_names() {
        let s = saved_wifi_names("Nurse Cafe 2G:802-11-wireless\nlo:loopback\nHome:802-11-wireless\n");
        assert_eq!(s, ["Nurse Cafe 2G", "Home"]);
    }

    #[test]
    fn bluetooth_lists() {
        let all = bt_devices("Device AA:BB:CC:DD:EE:01 Speaker\nDevice 11:22:33:44:55:66 \nbad line\nDevice ZZ:BB:CC:DD:EE:01 X\n");
        assert_eq!(all.len(), 2);
        let m = bt_merge(&all, &all[..1], &all[..1]);
        assert!(m[0].paired && m[0].connected && !m[1].paired);
        assert_eq!(m[1].name, "11:22:33:44:55:66");
        assert!(bt_powered("Controller X\n\tPowered: yes\n"));
        assert!(!bt_powered("\tPowered: no\n"));
    }

    #[test]
    fn mem() {
        assert_eq!(meminfo("MemTotal:  360000 kB\nMemFree: 1 kB\nMemAvailable:  143000 kB\n"), (360000, 143000));
    }

    #[test]
    fn os_release_and_cpuinfo_summaries() {
        assert_eq!(os_pretty_name("NAME=\"Debian\"\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\n"), "Debian GNU/Linux 13 (trixie)");
        assert_eq!(os_pretty_name("x=1"), "");
        let cpu = "processor\t: 0\nmodel name\t: ARMv8 Processor rev 4 (v8l)\nprocessor\t: 1\nmodel name\t: ARMv8 Processor rev 4 (v8l)\n";
        assert_eq!(cpu_summary(cpu), "2 x ARMv8 Processor rev 4 (v8l)");
        assert_eq!(cpu_summary(""), "");
    }
}
