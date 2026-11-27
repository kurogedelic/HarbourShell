//! The local network as the Interceptor's LAN page sees it: which subnet the device is on, who
//! the neighbour table lists, makers from MAC prefixes, service names of common ports, and the
//! few wire formats (NetBIOS name query, unicast mDNS pointer query) the discovery uses. Pure
//! functions only; the sockets and threads live in `launcher/apps/interceptor/lan.rs`.
//!
//! The guard is here too: [`LanInfo::allows`] is the one place that decides whether an address
//! may be probed at all. Only addresses of the connected interface's own private subnet pass
//! (RFC 1918 or link-local); public addresses, other subnets, the network and broadcast
//! addresses and multicast are refused.

use std::net::Ipv4Addr;

/// The connected interface's IPv4 setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanInfo {
    pub iface: String,
    pub ip: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Option<Ipv4Addr>,
}

/// One row of the neighbour (ARP) table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neighbor {
    pub ip: Ipv4Addr,
    /// Lower-case, colon separated.
    pub mac: String,
    pub reachable: bool,
}

/// Why an address may not be probed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The interface itself has no private address (e.g. a public one).
    NotPrivateNet,
    /// The target is outside the private ranges (a public address).
    Public,
    /// Private, but not in the connected subnet.
    OtherSubnet,
    /// The network or broadcast address, multicast, loopback or unspecified.
    NotAHost,
}

impl Refusal {
    pub fn text(self) -> &'static str {
        match self {
            Refusal::NotPrivateNet => "This network is not a private one: scan refused",
            Refusal::Public => "Public addresses are never scanned",
            Refusal::OtherSubnet => "Only the connected subnet is scanned",
            Refusal::NotAHost => "Not a device address",
        }
    }
}

/// RFC 1918 or link-local (169.254/16).
pub fn is_local_range(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local()
}

impl LanInfo {
    fn mask(&self) -> u32 {
        match self.prefix {
            0 => 0,
            p => u32::MAX << (32 - p.min(32) as u32),
        }
    }

    fn network(&self) -> u32 {
        u32::from(self.ip) & self.mask()
    }

    fn broadcast(&self) -> u32 {
        self.network() | !self.mask()
    }

    /// May `ip` be probed? The single gate for every probe.
    pub fn allows(&self, ip: Ipv4Addr) -> Result<(), Refusal> {
        if !is_local_range(self.ip) {
            return Err(Refusal::NotPrivateNet);
        }
        if ip.is_loopback() || ip.is_multicast() || ip.is_unspecified() || ip.is_broadcast() {
            return Err(Refusal::NotAHost);
        }
        if !is_local_range(ip) {
            return Err(Refusal::Public);
        }
        let n = u32::from(ip);
        if n & self.mask() != self.network() {
            return Err(Refusal::OtherSubnet);
        }
        if self.prefix < 31 && (n == self.network() || n == self.broadcast()) {
            return Err(Refusal::NotAHost);
        }
        Ok(())
    }

    /// The addresses a sweep visits: the subnet's hosts, at most the /24 around the device's
    /// own address (a wider network is swept only in that part). Never includes the device itself.
    pub fn sweep_hosts(&self) -> Vec<Ipv4Addr> {
        if !is_local_range(self.ip) || self.prefix >= 31 {
            return Vec::new();
        }
        let eff = LanInfo { prefix: self.prefix.max(24), ..self.clone() };
        let (first, last) = (eff.network() + 1, eff.broadcast() - 1);
        (first..=last).map(Ipv4Addr::from).filter(|h| *h != self.ip && self.allows(*h).is_ok()).collect()
    }
}

// ---------------------------------------------------------------------- parsers

/// `6E:44:B3:64:D5:9E`, `6e-44-...` -> `6e:44:b3:64:d5:9e`; None when it is not 6 hex octets or all zero.
pub fn norm_mac(s: &str) -> Option<String> {
    let parts: Vec<&str> = s.trim().split(|c| c == ':' || c == '-').collect();
    if parts.len() != 6 || parts.iter().any(|p| p.len() != 2 || !p.chars().all(|c| c.is_ascii_hexdigit())) {
        return None;
    }
    let m = parts.join(":").to_ascii_lowercase();
    (m != "00:00:00:00:00:00").then_some(m)
}

/// `ip -4 neigh show`: `192.168.3.1 dev wlan0 lladdr 6e:44:b3:64:d5:9e REACHABLE`. Entries without
/// a hardware address (FAILED / INCOMPLETE) are left out.
pub fn parse_ip_neigh(text: &str) -> Vec<Neighbor> {
    let mut v = Vec::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        let Some(ip) = f.first().and_then(|s| s.parse::<Ipv4Addr>().ok()) else { continue };
        let Some(mac) = f.iter().position(|w| *w == "lladdr").and_then(|i| f.get(i + 1)).and_then(|m| norm_mac(m)) else { continue };
        let state = f.last().copied().unwrap_or("");
        if matches!(state, "FAILED" | "INCOMPLETE") {
            continue;
        }
        v.push(Neighbor { ip, mac, reachable: state == "REACHABLE" });
    }
    v
}

/// `/proc/net/arp` (flag 0x2 = complete entry).
pub fn parse_proc_arp(text: &str) -> Vec<Neighbor> {
    let mut v = Vec::new();
    for l in text.lines().skip(1) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let (Ok(ip), Some(mac)) = (f[0].parse::<Ipv4Addr>(), norm_mac(f[3])) else { continue };
        if f[2] == "0x2" {
            v.push(Neighbor { ip, mac, reachable: true });
        }
    }
    v
}

/// `ip -4 -o addr show`: `3: wlan0    inet 192.168.3.94/24 brd ... scope global ...` -> (iface, ip, prefix).
pub fn parse_ip_addr(text: &str) -> Vec<(String, Ipv4Addr, u8)> {
    let mut v = Vec::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        let Some(i) = f.iter().position(|w| *w == "inet") else { continue };
        let Some(iface) = f.get(1) else { continue };
        let Some((ip, p)) = f.get(i + 1).and_then(|s| s.split_once('/')) else { continue };
        if let (Ok(ip), Ok(p)) = (ip.parse::<Ipv4Addr>(), p.parse::<u8>()) {
            if p <= 32 {
                v.push((iface.trim_end_matches(':').to_string(), ip, p));
            }
        }
    }
    v
}

/// `ip -4 route show default`: `default via 192.168.3.1 dev wlan0 proto dhcp` -> (gateway, iface).
pub fn parse_default_route(text: &str) -> Option<(Ipv4Addr, String)> {
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.first() != Some(&"default") {
            continue;
        }
        let via = f.iter().position(|w| *w == "via").and_then(|i| f.get(i + 1)).and_then(|s| s.parse().ok());
        let dev = f.iter().position(|w| *w == "dev").and_then(|i| f.get(i + 1));
        if let (Some(g), Some(d)) = (via, dev) {
            return Some((g, d.to_string()));
        }
    }
    None
}

/// The connected interface: the default route's, else the first one with a private address.
pub fn pick_lan(route: &str, addrs: &str) -> Option<LanInfo> {
    let all = parse_ip_addr(addrs);
    if let Some((gw, dev)) = parse_default_route(route) {
        if let Some((iface, ip, prefix)) = all.iter().find(|a| a.0 == dev).cloned() {
            return Some(LanInfo { iface, ip, prefix, gateway: Some(gw) });
        }
    }
    all.into_iter().find(|a| is_local_range(a.1) && a.0 != "lo").map(|(iface, ip, prefix)| LanInfo { iface, ip, prefix, gateway: None })
}

// ----------------------------------------------------------------------- makers

/// A small table of common makers by the first three octets of the MAC (not the whole registry:
/// anything else shows the prefix itself).
const OUI: &[(&str, &str)] = &[
    ("b8:27:eb", "Raspberry Pi"),
    ("dc:a6:32", "Raspberry Pi"),
    ("e4:5f:01", "Raspberry Pi"),
    ("28:cd:c1", "Raspberry Pi"),
    ("d8:3a:dd", "Raspberry Pi"),
    ("2c:cf:67", "Raspberry Pi"),
    ("24:0a:c4", "Espressif"),
    ("30:ae:a4", "Espressif"),
    ("3c:71:bf", "Espressif"),
    ("84:0d:8e", "Espressif"),
    ("a4:cf:12", "Espressif"),
    ("7c:9e:bd", "Espressif"),
    ("24:6f:28", "Espressif"),
    ("08:3a:f2", "Espressif"),
    ("30:c6:f7", "Espressif"),
    ("ec:62:60", "Espressif"),
    ("40:f5:20", "Espressif"),
    ("4c:11:ae", "Espressif"),
    ("80:7d:3a", "Espressif"),
    ("94:b9:7e", "Espressif"),
    ("bc:dd:c2", "Espressif"),
    ("cc:50:e3", "Espressif"),
    ("dc:4f:22", "Espressif"),
    ("10:52:1c", "Espressif"),
    ("24:62:ab", "Espressif"),
    ("00:03:93", "Apple"),
    ("00:1c:b3", "Apple"),
    ("3c:22:fb", "Apple"),
    ("a4:83:e7", "Apple"),
    ("f0:18:98", "Apple"),
    ("28:cf:e9", "Apple"),
    ("b8:e8:56", "Apple"),
    ("d0:03:4b", "Apple"),
    ("3c:5a:b4", "Google"),
    ("f4:f5:d8", "Google"),
    ("54:60:09", "Google"),
    ("1c:f2:9a", "Google"),
    ("a4:77:33", "Google"),
    ("44:65:0d", "Amazon"),
    ("68:54:fd", "Amazon"),
    ("fc:a6:67", "Amazon"),
    ("74:c2:46", "Amazon"),
    ("84:d6:d0", "Amazon"),
    ("0c:47:c9", "Amazon"),
    ("40:b4:cd", "Amazon"),
    ("00:12:47", "Samsung"),
    ("00:16:32", "Samsung"),
    ("5c:0a:5b", "Samsung"),
    ("78:47:1d", "Samsung"),
    ("00:04:1f", "Sony"),
    ("00:13:a9", "Sony"),
    ("00:1d:0d", "Sony"),
    ("fc:0f:e6", "Sony"),
    ("00:09:bf", "Nintendo"),
    ("00:16:56", "Nintendo"),
    ("00:17:ab", "Nintendo"),
    ("00:19:1d", "Nintendo"),
    ("7c:bb:8a", "Nintendo"),
    ("98:b6:e9", "Nintendo"),
    ("e8:4e:ce", "Nintendo"),
    ("00:1b:21", "Intel"),
    ("3c:97:0e", "Intel"),
    ("00:15:00", "Intel"),
    ("a4:4e:31", "Intel"),
    ("8c:8d:28", "Intel"),
    ("00:03:ff", "Microsoft"),
    ("00:15:5d", "Microsoft Hyper-V"),
    ("28:18:78", "Microsoft"),
    ("50:c7:bf", "TP-Link"),
    ("14:cc:20", "TP-Link"),
    ("f4:f2:6d", "TP-Link"),
    ("98:da:c4", "TP-Link"),
    ("b0:95:75", "TP-Link"),
    ("60:e3:27", "TP-Link"),
    ("00:09:5b", "Netgear"),
    ("00:14:6c", "Netgear"),
    ("20:e5:2a", "Netgear"),
    ("a0:40:a0", "Netgear"),
    ("00:07:40", "Buffalo"),
    ("00:16:01", "Buffalo"),
    ("10:6f:3f", "Buffalo"),
    ("4c:e6:76", "Buffalo"),
    ("00:00:0c", "Cisco"),
    ("00:06:25", "Linksys"),
    ("00:0c:41", "Linksys"),
    ("00:12:17", "Linksys"),
    ("24:a4:3c", "Ubiquiti"),
    ("44:d9:e7", "Ubiquiti"),
    ("78:8a:20", "Ubiquiti"),
    ("f0:9f:c2", "Ubiquiti"),
    ("80:2a:a8", "Ubiquiti"),
    ("fc:ec:da", "Ubiquiti"),
    ("74:ac:b9", "Ubiquiti"),
    ("00:11:32", "Synology"),
    ("00:08:9b", "QNAP"),
    ("24:5e:be", "QNAP"),
    ("00:0e:58", "Sonos"),
    ("5c:aa:fd", "Sonos"),
    ("78:28:ca", "Sonos"),
    ("b8:e9:37", "Sonos"),
    ("48:a6:b8", "Sonos"),
    ("00:17:88", "Philips Hue"),
    ("ec:b5:fa", "Philips Hue"),
    ("00:80:77", "Brother"),
    ("00:1b:a9", "Brother"),
    ("30:05:5c", "Brother"),
    ("00:00:85", "Canon"),
    ("00:1e:8f", "Canon"),
    ("00:00:48", "Epson"),
    ("00:26:ab", "Epson"),
    ("64:eb:8c", "Epson"),
    ("00:01:e6", "HP"),
    ("00:0b:cd", "HP"),
    ("3c:d9:2b", "HP"),
    ("94:57:a5", "HP"),
    ("28:6c:07", "Xiaomi"),
    ("64:09:80", "Xiaomi"),
    ("78:11:dc", "Xiaomi"),
    ("34:ce:00", "Xiaomi"),
    ("00:e0:fc", "Huawei"),
    ("48:46:fb", "Huawei"),
    ("00:0c:29", "VMware"),
    ("00:50:56", "VMware"),
    ("00:e0:4c", "Realtek"),
    ("18:b4:30", "Nest"),
    ("b0:a7:37", "Roku"),
    ("d8:31:34", "Roku"),
];

/// The maker of a MAC address: from the table, "Private address" for a randomised (locally
/// administered) one, else the three-octet prefix.
pub fn oui_vendor(mac: &str) -> String {
    let m = mac.to_ascii_lowercase();
    let prefix = m.get(..8).unwrap_or("");
    if let Some((_, name)) = OUI.iter().find(|(p, _)| *p == prefix) {
        return (*name).to_string();
    }
    // Bit 1 of the first octet: locally administered, what phones use for "private Wi-Fi address".
    if let Some(b) = m.get(..2).and_then(|h| u8::from_str_radix(h, 16).ok()) {
        if b & 0x02 != 0 {
            return "Private address".to_string();
        }
    }
    prefix.to_ascii_uppercase()
}

// ------------------------------------------------------------------------ ports

/// The ports the short check visits (TCP connect, nothing is sent).
pub const COMMON_PORTS: [u16; 18] = [21, 22, 23, 53, 80, 139, 443, 445, 548, 554, 631, 1883, 3389, 5353, 5900, 8080, 8443, 9100];

/// A port's usual service.
pub fn service_name(port: u16) -> &'static str {
    match port {
        21 => "FTP",
        22 => "SSH",
        23 => "Telnet",
        25 => "SMTP",
        53 => "DNS",
        80 => "HTTP",
        110 => "POP3",
        111 => "RPC",
        139 => "NetBIOS",
        143 => "IMAP",
        443 => "HTTPS",
        445 => "SMB",
        515 => "LPD",
        548 => "AFP",
        554 => "RTSP",
        631 => "IPP",
        873 => "rsync",
        1883 => "MQTT",
        1900 => "UPnP",
        3000 => "Web (3000)",
        3306 => "MySQL",
        3389 => "RDP",
        5000 => "UPnP / web",
        5353 => "mDNS",
        5432 => "PostgreSQL",
        5900 => "VNC",
        8000 => "HTTP (8000)",
        8080 => "HTTP-alt",
        8443 => "HTTPS-alt",
        8883 => "MQTT/TLS",
        9100 => "Printer (raw)",
        32400 => "Plex",
        62078 => "iOS sync",
        _ => "",
    }
}

/// Most ports one custom range may hold (at 50 probes a second that is about 20 s).
pub const MAX_CUSTOM_PORTS: usize = 1024;

/// `22,80,8000-8100` -> sorted, unique ports. Err when it does not parse or holds too many.
pub fn parse_ports(spec: &str) -> Result<Vec<u16>, &'static str> {
    let mut v: Vec<u16> = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim().parse::<u32>(), b.trim().parse::<u32>()),
            None => (part.parse::<u32>(), part.parse::<u32>()),
        };
        let (Ok(a), Ok(b)) = (a, b) else { return Err("Ports like 22,80,8000-8100") };
        if a == 0 || b < a || b > 65535 {
            return Err("Ports are 1 to 65535");
        }
        if v.len() + (b - a + 1) as usize > MAX_CUSTOM_PORTS {
            return Err("At most 1024 ports");
        }
        v.extend((a..=b).map(|p| p as u16));
    }
    if v.is_empty() {
        return Err("Ports like 22,80,8000-8100");
    }
    v.sort_unstable();
    v.dedup();
    Ok(v)
}

// ------------------------------------------------------------------ name queries

/// A NetBIOS node status request ("what are your names?") for UDP 137.
pub fn nbstat_query() -> Vec<u8> {
    let mut q = vec![0x48, 0x42, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    q.push(0x20);
    q.extend_from_slice(b"CKAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    q.extend_from_slice(&[0, 0, 0x21, 0, 1]);
    q
}

/// The machine name in a node status answer (first unique name of type 0x00).
pub fn parse_nbstat(buf: &[u8]) -> Option<String> {
    if buf.len() < 12 {
        return None;
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    if an == 0 {
        return None;
    }
    let mut pos = 12;
    for _ in 0..qd {
        pos = dns_skip_name(buf, pos)? + 4;
    }
    pos = dns_skip_name(buf, pos)?;
    // type, class, ttl, rdlength
    let rd = buf.get(pos + 8..pos + 10)?;
    let _rdlen = u16::from_be_bytes([rd[0], rd[1]]);
    pos += 10;
    let n = *buf.get(pos)? as usize;
    pos += 1;
    for _ in 0..n {
        let e = buf.get(pos..pos + 18)?;
        pos += 18;
        let flags = u16::from_be_bytes([e[16], e[17]]);
        if e[15] == 0 && flags & 0x8000 == 0 {
            let name = String::from_utf8_lossy(&e[..15]).trim().to_string();
            if !name.is_empty() && name.chars().all(|c| !c.is_control()) {
                return Some(name);
            }
        }
    }
    None
}

/// A unicast mDNS question for the pointer record of `ip` (what its `.local` name is).
pub fn mdns_ptr_query(ip: Ipv4Addr) -> Vec<u8> {
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    let o = ip.octets();
    for b in o.iter().rev() {
        let s = b.to_string();
        q.push(s.len() as u8);
        q.extend_from_slice(s.as_bytes());
    }
    for label in ["in-addr", "arpa"] {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    // PTR, class IN with the unicast-response bit.
    q.extend_from_slice(&[0, 12, 0x80, 1]);
    q
}

/// The name in the first PTR answer of an mDNS / DNS reply, without a trailing `.local`.
pub fn parse_dns_ptr(buf: &[u8]) -> Option<String> {
    if buf.len() < 12 {
        return None;
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut pos = 12;
    for _ in 0..qd {
        pos = dns_skip_name(buf, pos)? + 4;
    }
    for _ in 0..an {
        pos = dns_skip_name(buf, pos)?;
        let h = buf.get(pos..pos + 10)?;
        let ty = u16::from_be_bytes([h[0], h[1]]);
        let rdlen = u16::from_be_bytes([h[8], h[9]]) as usize;
        pos += 10;
        if ty == 12 {
            let name = dns_read_name(buf, pos)?;
            let name = name.trim_end_matches('.');
            let name = name.strip_suffix(".local").unwrap_or(name);
            return (!name.is_empty()).then(|| name.to_string());
        }
        pos += rdlen;
    }
    None
}

/// Position after the (possibly compressed) name at `pos`.
fn dns_skip_name(buf: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let l = *buf.get(pos)? as usize;
        if l == 0 {
            return Some(pos + 1);
        }
        if l & 0xC0 == 0xC0 {
            buf.get(pos + 1)?;
            return Some(pos + 2);
        }
        pos += 1 + l;
    }
}

/// The dotted name at `pos`, following compression pointers (a bounded number of them).
fn dns_read_name(buf: &[u8], mut pos: usize) -> Option<String> {
    let mut out = String::new();
    let mut jumps = 0;
    loop {
        let l = *buf.get(pos)? as usize;
        if l == 0 {
            return Some(out);
        }
        if l & 0xC0 == 0xC0 {
            jumps += 1;
            if jumps > 8 {
                return None;
            }
            pos = ((l & 0x3F) << 8) | *buf.get(pos + 1)? as usize;
            continue;
        }
        let label = buf.get(pos + 1..pos + 1 + l)?;
        out.push_str(&String::from_utf8_lossy(label));
        out.push('.');
        pos += 1 + l;
    }
}

/// `192.168.3.5   printer.lan` (getent hosts) -> `printer.lan`.
pub fn parse_getent_hosts(text: &str) -> Option<String> {
    let name = text.lines().next()?.split_whitespace().nth(1)?;
    (name.parse::<Ipv4Addr>().is_err()).then(|| name.trim_end_matches('.').to_string())
}

/// A short age: `now`, `5m`, `3h`, `2d`.
pub fn ago_short(secs: u64) -> String {
    match secs {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

// ------------------------------------------------------------------- mock network

/// The mock backend's network (`HARBOUR_MOCK=1`): Wi-Fi 192.168.3.0/24, a router and six devices.
pub fn mock_info() -> LanInfo {
    LanInfo { iface: "wlan0".into(), ip: Ipv4Addr::new(192, 168, 3, 94), prefix: 24, gateway: Some(Ipv4Addr::new(192, 168, 3, 1)) }
}

/// (last octet, MAC, name the mock "resolves", open TCP ports)
pub const MOCK_DEVICES: [(u8, &str, &str, &[u16]); 7] = [
    (1, "10:6f:3f:64:d5:9e", "router.lan", &[53, 80, 443]),
    (20, "b8:27:eb:12:34:56", "kitchen-pi", &[22, 80, 1883]),
    (31, "3c:22:fb:90:ab:cd", "Yuki-iPhone", &[]),
    (45, "24:0a:c4:55:66:77", "", &[80]),
    (50, "fc:0f:e6:11:22:33", "PS5-living", &[]),
    (60, "00:80:77:aa:bb:cc", "BRW-office", &[80, 443, 515, 631, 9100]),
    (88, "be:12:34:56:78:9a", "", &[]),
];

pub fn mock_neighbors() -> Vec<Neighbor> {
    MOCK_DEVICES.iter().map(|(o, mac, _, _)| Neighbor { ip: Ipv4Addr::new(192, 168, 3, *o), mac: (*mac).to_string(), reachable: *o != 50 }).collect()
}

pub fn mock_name(ip: Ipv4Addr) -> Option<String> {
    let o = ip.octets()[3];
    MOCK_DEVICES.iter().find(|d| d.0 == o).map(|d| d.2).filter(|n| !n.is_empty()).map(str::to_string)
}

pub fn mock_open_ports(ip: Ipv4Addr) -> Vec<u16> {
    let o = ip.octets()[3];
    MOCK_DEVICES.iter().find(|d| d.0 == o).map(|d| d.3.to_vec()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(ip: &str, prefix: u8) -> LanInfo {
        LanInfo { iface: "wlan0".into(), ip: ip.parse().unwrap(), prefix, gateway: Some("192.168.3.1".parse().unwrap()) }
    }

    #[test]
    fn the_guard_passes_only_the_own_private_subnet() {
        let i = info("192.168.3.94", 24);
        let ok = |s: &str| i.allows(s.parse().unwrap());
        assert!(ok("192.168.3.1").is_ok());
        assert!(ok("192.168.3.254").is_ok());
        assert_eq!(ok("192.168.4.1"), Err(Refusal::OtherSubnet));
        assert_eq!(ok("10.0.0.5"), Err(Refusal::OtherSubnet));
        assert_eq!(ok("8.8.8.8"), Err(Refusal::Public));
        assert_eq!(ok("1.1.1.1"), Err(Refusal::Public));
        assert_eq!(ok("192.168.3.0"), Err(Refusal::NotAHost));
        assert_eq!(ok("192.168.3.255"), Err(Refusal::NotAHost));
        assert_eq!(ok("224.0.0.251"), Err(Refusal::NotAHost));
        assert_eq!(ok("127.0.0.1"), Err(Refusal::NotAHost));
        assert_eq!(ok("255.255.255.255"), Err(Refusal::NotAHost));
        // Link-local on a link-local network.
        assert!(info("169.254.7.9", 16).allows("169.254.200.1".parse().unwrap()).is_ok());
        // A public address on the interface: nothing at all is scanned.
        let p = info("203.0.113.9", 24);
        assert_eq!(p.allows("203.0.113.10".parse().unwrap()), Err(Refusal::NotPrivateNet));
        assert!(p.sweep_hosts().is_empty());
        // 172.16/12 is private, 172.32 is not.
        assert!(info("172.16.5.5", 16).allows("172.16.9.9".parse().unwrap()).is_ok());
        assert_eq!(info("172.16.5.5", 16).allows("172.32.0.1".parse().unwrap()), Err(Refusal::Public));
    }

    #[test]
    fn a_sweep_visits_the_hosts_of_the_subnet_only() {
        let h = info("192.168.3.94", 24).sweep_hosts();
        assert_eq!(h.len(), 253, "254 hosts minus the device itself");
        assert!(!h.contains(&"192.168.3.94".parse().unwrap()));
        assert!(!h.contains(&"192.168.3.0".parse().unwrap()) && !h.contains(&"192.168.3.255".parse().unwrap()));
        // A /16 is swept in the /24 around the device only.
        let w = info("10.1.2.3", 16).sweep_hosts();
        assert_eq!(w.len(), 253);
        assert!(w.iter().all(|a| a.octets()[..3] == [10, 1, 2]));
        // A /28 is swept whole.
        assert_eq!(info("192.168.3.5", 28).sweep_hosts().len(), 13);
        assert!(info("192.168.3.5", 32).sweep_hosts().is_empty());
        for a in &w {
            assert!(info("10.1.2.3", 16).allows(*a).is_ok());
        }
    }

    #[test]
    fn the_neighbour_table_parses() {
        let t = "192.168.3.1 dev wlan0 lladdr 6e:44:b3:64:d5:9e REACHABLE
192.168.3.20 dev wlan0 lladdr B8:27:EB:12:34:56 STALE
192.168.3.31 dev wlan0  FAILED
192.168.3.40 dev wlan0  INCOMPLETE
fe80::1 dev wlan0 lladdr 6e:44:b3:64:d5:9e router STALE
192.168.3.50 dev wlan0 lladdr 00:00:00:00:00:00 STALE
";
        let v = parse_ip_neigh(t);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], Neighbor { ip: "192.168.3.1".parse().unwrap(), mac: "6e:44:b3:64:d5:9e".into(), reachable: true });
        assert_eq!(v[1].mac, "b8:27:eb:12:34:56");
        assert!(!v[1].reachable);
    }

    #[test]
    fn proc_arp_parses_complete_entries() {
        let t = "IP address       HW type     Flags       HW address            Mask     Device
192.168.3.1      0x1         0x2         6e:44:b3:64:d5:9e     *        wlan0
192.168.3.77     0x1         0x0         00:00:00:00:00:00     *        wlan0
";
        let v = parse_proc_arp(t);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].ip, "192.168.3.1".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn the_connected_interface_comes_from_route_and_addr() {
        let route = "default via 192.168.3.1 dev wlan0 proto dhcp src 192.168.3.94 metric 600\n";
        let addr = "1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever\n2: eth0    inet 10.9.9.2/16 brd 10.9.255.255 scope global eth0\\       valid_lft forever\n3: wlan0    inet 192.168.3.94/24 brd 192.168.3.255 scope global dynamic noprefixroute wlan0\\       valid_lft 86000sec\n";
        let l = pick_lan(route, addr).unwrap();
        assert_eq!((l.iface.as_str(), l.ip, l.prefix, l.gateway), ("wlan0", "192.168.3.94".parse().unwrap(), 24, Some("192.168.3.1".parse().unwrap())));
        // No default route: the first private non-loopback address.
        let l = pick_lan("", addr).unwrap();
        assert_eq!((l.iface.as_str(), l.prefix, l.gateway), ("eth0", 16, None));
        assert!(pick_lan("", "1: lo    inet 127.0.0.1/8 scope host lo\n").is_none());
    }

    #[test]
    fn makers_come_from_the_prefix() {
        assert_eq!(oui_vendor("b8:27:eb:12:34:56"), "Raspberry Pi");
        assert_eq!(oui_vendor("B8:27:EB:00:00:01"), "Raspberry Pi");
        assert_eq!(oui_vendor("24:0a:c4:00:11:22"), "Espressif");
        // Random "private" address: bit 1 of the first octet set.
        assert_eq!(oui_vendor("be:12:34:56:78:9a"), "Private address");
        assert_eq!(oui_vendor("00:aa:bb:cc:dd:ee"), "00:AA:BB");
    }

    #[test]
    fn mac_addresses_are_normalised() {
        assert_eq!(norm_mac("6E-44-B3-64-D5-9E").as_deref(), Some("6e:44:b3:64:d5:9e"));
        assert_eq!(norm_mac("00:00:00:00:00:00"), None);
        assert_eq!(norm_mac("6e:44:b3"), None);
        assert_eq!(norm_mac("zz:44:b3:64:d5:9e"), None);
    }

    #[test]
    fn port_lists_parse_and_are_capped() {
        assert_eq!(parse_ports("80, 22,80,8000-8002"), Ok(vec![22, 80, 8000, 8001, 8002]));
        assert!(parse_ports("").is_err());
        assert!(parse_ports("0").is_err());
        assert!(parse_ports("70000").is_err());
        assert!(parse_ports("20-10").is_err());
        assert!(parse_ports("a-b").is_err());
        assert!(parse_ports("1-1025").is_err());
        assert_eq!(parse_ports("1-1024").unwrap().len(), 1024);
        assert_eq!(service_name(22), "SSH");
        assert_eq!(service_name(9100), "Printer (raw)");
        assert_eq!(service_name(4), "");
        assert!(COMMON_PORTS.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn a_node_status_answer_gives_the_machine_name() {
        // Header (1 answer, no question), name pointer-free 34 bytes, type NBSTAT, 2 names.
        let mut b = vec![0x48, 0x42, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        b.push(0x20);
        b.extend_from_slice(b"CKAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        b.push(0);
        b.extend_from_slice(&[0, 0x21, 0, 1, 0, 0, 0, 0, 0, 41]);
        b.push(2);
        let mut group = *b"WORKGROUP      ";
        group[14] = b' ';
        b.extend_from_slice(&group);
        b.push(0);
        b.extend_from_slice(&[0xC4, 0]); // group name
        let mut me = *b"LIVINGROOM-PC  ";
        me[14] = b' ';
        b.extend_from_slice(&me);
        b.push(0);
        b.extend_from_slice(&[0x04, 0]); // unique, active
        assert_eq!(parse_nbstat(&b).as_deref(), Some("LIVINGROOM-PC"));
        assert_eq!(parse_nbstat(&b[..20]), None);
        assert_eq!(parse_nbstat(&[]), None);
        let q = nbstat_query();
        assert_eq!(q.len(), 50);
    }

    #[test]
    fn an_mdns_pointer_answer_gives_the_local_name() {
        let q = mdns_ptr_query("192.168.3.20".parse().unwrap());
        assert_eq!(&q[12..14], &[2, b'2']);
        // Reply: question echoed, answer name is a pointer to it, PTR rdata "kitchen-pi.local".
        let mut r = q[..12].to_vec();
        r[2] = 0x84;
        r[5] = 1;
        r[7] = 1;
        r.extend_from_slice(&q[12..]);
        r.extend_from_slice(&[0xC0, 12, 0, 12, 0, 1, 0, 0, 0, 120]);
        let rdata: Vec<u8> = [&[10u8][..], b"kitchen-pi", &[5], b"local", &[0]].concat();
        r.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        r.extend_from_slice(&rdata);
        assert_eq!(parse_dns_ptr(&r).as_deref(), Some("kitchen-pi"));
        assert_eq!(parse_dns_ptr(&r[..30]), None);
        // A pointer loop must not hang.
        let mut bad = vec![0u8; 12];
        bad[7] = 1;
        bad.extend_from_slice(&[0xC0, 12, 0, 12, 0, 1, 0, 0, 0, 1, 0, 2, 0xC0, 12]);
        assert_eq!(parse_dns_ptr(&bad), None);
    }

    #[test]
    fn reverse_lookups_and_ages_read_well() {
        assert_eq!(parse_getent_hosts("192.168.3.5     printer.lan\n").as_deref(), Some("printer.lan"));
        assert_eq!(parse_getent_hosts("192.168.3.5     192.168.3.5\n"), None);
        assert_eq!(parse_getent_hosts(""), None);
        assert_eq!([ago_short(5), ago_short(300), ago_short(7300), ago_short(200_000)], ["now", "5m", "2h", "2d"]);
    }
}
