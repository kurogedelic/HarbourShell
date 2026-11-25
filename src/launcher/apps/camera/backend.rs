//! The camera backend seam: enumerate capture devices and open a frame source. V4L2 (USB
//! UVC webcams) is the real one; a CSI/libcamera backend can slot in behind the same traits.
//! `Mock` makes an animated test pattern, `Fake` (tests) plays back a fixed list of devices.

use std::time::{Duration, Instant};

use super::convert::{self, Raw};

pub const CAP_VIDEO_CAPTURE: u32 = 0x0000_0001;
pub const CAP_METADATA_CAPTURE: u32 = 0x0080_0000;
pub const CAP_DEVICE_CAPS: u32 = 0x8000_0000;
pub const CAP_STREAMING: u32 = 0x0400_0000;

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
}
pub const MJPG: u32 = fourcc(b"MJPG");
pub const YUYV: u32 = fourcc(b"YUYV");

pub fn fourcc_name(f: u32) -> String {
    match f {
        MJPG => "MJPG".into(),
        YUYV => "YUYV".into(),
        _ => String::from_utf8_lossy(&f.to_le_bytes()).into_owned(),
    }
}

/// What a device node reports (QUERYCAP + ENUM_FMT), before filtering.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    pub driver: String,
    pub card: String,
    pub bus_info: String,
    pub capabilities: u32,
    pub device_caps: u32,
    pub formats: Vec<u32>,
}

/// A usable capture device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub path: String,
    pub name: String,
    pub uvc: bool,
    pub formats: Vec<u32>,
    /// Discrete frame sizes (≤ 1280 wide), ascending, for the formats we can read.
    pub sizes: Vec<(u32, u32)>,
}

/// Result of looking at `/dev/video*`: the usable devices and one note per node probed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub devices: Vec<Device>,
    pub notes: Vec<String>,
}

/// Keep only nodes that capture video by streaming (not metadata nodes, not output-only
/// or read()-only drivers) and offer a format we can read. `Err` is the reason shown to the user.
pub fn classify(path: &str, c: &Caps) -> Result<(), String> {
    // Only USB cameras: the Pi's own ISP / codec nodes (platform:...) also claim "capture" but
    // need another stage to feed them and never deliver a frame. CSI cameras belong to a
    // libcamera backend.
    if !(c.driver == "uvcvideo" || c.bus_info.starts_with("usb-")) {
        return Err(format!("not USB ({})", if c.driver.is_empty() { &c.bus_info } else { &c.driver }));
    }
    let caps = if c.capabilities & CAP_DEVICE_CAPS != 0 { c.device_caps } else { c.capabilities };
    if caps & CAP_METADATA_CAPTURE != 0 {
        return Err("metadata node".into());
    }
    if caps & CAP_VIDEO_CAPTURE == 0 {
        return Err("not a capture device".into());
    }
    if caps & CAP_STREAMING == 0 {
        return Err("no streaming I/O".into());
    }
    if !c.formats.iter().any(|&f| f == MJPG || f == YUYV) {
        let names: Vec<String> = c.formats.iter().map(|&f| fourcc_name(f)).collect();
        return Err(format!("no MJPG/YUYV (has {})", if names.is_empty() { "nothing".into() } else { names.join(",") }));
    }
    let _ = path;
    Ok(())
}

/// Order devices: UVC first, then by path with the number sorted numerically (video2 < video10).
pub fn sort_devices(devs: &mut [Device]) {
    fn num(p: &str) -> u32 {
        p.trim_start_matches(|c: char| !c.is_ascii_digit()).parse().unwrap_or(u32::MAX)
    }
    devs.sort_by_key(|d| (!d.uvc, num(&d.path)));
}

/// The frame sizes we offer: discrete sizes up to 1280x960, ascending.
pub fn offer_sizes(all: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = all.iter().copied().filter(|&(w, h)| w >= 160 && w <= 1280 && h <= 960).collect();
    v.sort();
    v.dedup();
    v
}

/// The size to start with: 640x480 when there, else the largest not above it, else the smallest.
pub fn default_size(sizes: &[(u32, u32)]) -> Option<(u32, u32)> {
    sizes.iter().copied().filter(|&(w, h)| w * h <= 640 * 480).max_by_key(|&(w, h)| w * h).or_else(|| sizes.first().copied())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctrl {
    Brightness,
    Contrast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CtrlRange {
    pub min: i32,
    pub max: i32,
    pub step: i32,
    pub value: i32,
}

impl CtrlRange {
    pub fn frac(&self) -> f32 {
        if self.max > self.min {
            (self.value - self.min) as f32 / (self.max - self.min) as f32
        } else {
            0.0
        }
    }

    /// The value at `pct` percent of the range, on the step grid.
    pub fn at_pct(&self, pct: u8) -> i32 {
        let span = (self.max - self.min).max(0) as i64;
        let v = self.min + (span * pct.min(100) as i64 / 100) as i32;
        let st = self.step.max(1);
        (self.min + (v - self.min) / st * st).clamp(self.min, self.max)
    }

    pub fn pct(&self) -> u8 {
        (self.frac() * 100.0).round().clamp(0.0, 100.0) as u8
    }

    /// The value one notch (a tenth of the range) up or down, on the step grid.
    pub fn nudged(&self, dir: i32) -> i32 {
        let notch = ((self.max - self.min) / 10).max(self.step.max(1));
        let v = (self.value + dir.signum() * notch).clamp(self.min, self.max);
        let st = self.step.max(1);
        (self.min + (v - self.min) / st * st).clamp(self.min, self.max)
    }
}

/// An opened stream of frames.
pub trait Source: Send {
    /// Format and size actually negotiated, e.g. ("MJPG", 640, 480).
    fn describe(&self) -> (String, u32, u32);
    /// The newest frame, waiting up to `timeout`; `Ok(None)` when none came. Older frames
    /// that piled up are discarded.
    fn next(&mut self, timeout: Duration) -> Result<Option<Raw>, String>;
    fn ctrl(&mut self, _c: Ctrl) -> Option<CtrlRange> {
        None
    }
    fn set_ctrl(&mut self, _c: Ctrl, _v: i32) {}
}

pub trait Backend: Send + Sync {
    fn probe(&self) -> Probe;
    /// Open the stream; `size` of `None` picks the default. `prefer_mjpeg` is false when
    /// no JPEG decoder is available.
    fn open(&self, dev: &Device, size: Option<(u32, u32)>, prefer_mjpeg: bool) -> Result<Box<dyn Source>, String>;
    /// Cameras that take full-resolution photos with their own tool (CSI) write a PNG to `out`
    /// here when `has_photo_tool`; otherwise "grab the newest preview frame instead" (UVC).
    fn has_photo_tool(&self, _dev: &Device) -> bool {
        false
    }
    fn photo(&self, _dev: &Device, _out: &std::path::Path) -> Result<(), String> {
        Err("no photo tool".into())
    }
}

// ------------------------------------------------------------------- mock

/// `HARBOUR_CAMERA=none` shows the "no camera" card, `busy` the error card (screenshots, tests).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Mock {
    #[default]
    Pattern,
    None,
    Busy,
}

impl Mock {
    pub fn from_env() -> Mock {
        match std::env::var("HARBOUR_CAMERA").as_deref() {
            Ok("none") => Mock::None,
            Ok("busy") => Mock::Busy,
            _ => Mock::Pattern,
        }
    }
}

struct MockSource {
    t: u32,
    last: Instant,
    bright: i32,
    contrast: i32,
    size: (u32, u32),
}

impl Backend for Mock {
    fn probe(&self) -> Probe {
        if *self == Mock::None {
            return Probe { devices: vec![], notes: vec!["/dev/video*: none".into(), "(HARBOUR_CAMERA=none)".into()] };
        }
        Probe {
            devices: vec![Device {
                path: "mock".into(),
                name: "Test pattern".into(),
                uvc: true,
                formats: vec![MJPG, YUYV],
                sizes: vec![(320, 240), (640, 480), (1280, 720)],
            }],
            notes: vec!["mock: test pattern".into()],
        }
    }

    fn open(&self, dev: &Device, size: Option<(u32, u32)>, _jpeg: bool) -> Result<Box<dyn Source>, String> {
        if *self == Mock::Busy {
            return Err("/dev/video0: busy (in use by another program)".into());
        }
        let size = size.or_else(|| default_size(&dev.sizes)).unwrap_or((640, 480));
        Ok(Box::new(MockSource { t: 0, last: Instant::now(), bright: 50, contrast: 50, size }))
    }
}

impl Source for MockSource {
    fn describe(&self) -> (String, u32, u32) {
        ("TEST".into(), self.size.0, self.size.1)
    }

    fn next(&mut self, timeout: Duration) -> Result<Option<Raw>, String> {
        let frame = Duration::from_millis(66);
        let due = self.last + frame;
        let now = Instant::now();
        if due > now {
            std::thread::sleep((due - now).min(timeout));
            if Instant::now() < due {
                return Ok(None);
            }
        }
        self.last = Instant::now();
        // The test pattern moves with the frame count; a frozen run stays on one frame.
        self.t = if crate::rt::frozen() { 1 } else { self.t + 1 };
        let mut data = convert::test_pattern(self.size.0, self.size.1, self.t);
        let (b, c) = ((self.bright - 50) * 2, 100 + (self.contrast - 50));
        if b != 0 || c != 100 {
            for v in data.iter_mut() {
                *v = ((((*v as i32 - 128) * c / 100) + 128) + b).clamp(0, 255) as u8;
            }
        }
        Ok(Some(Raw::Rgb { w: self.size.0, h: self.size.1, data }))
    }

    fn ctrl(&mut self, c: Ctrl) -> Option<CtrlRange> {
        let v = if c == Ctrl::Brightness { self.bright } else { self.contrast };
        Some(CtrlRange { min: 0, max: 100, step: 1, value: v })
    }

    fn set_ctrl(&mut self, c: Ctrl, v: i32) {
        match c {
            Ctrl::Brightness => self.bright = v,
            Ctrl::Contrast => self.contrast = v,
        }
    }
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(cap: u32, devcaps: u32, fmts: &[u32]) -> Caps {
        Caps { driver: "uvcvideo".into(), card: "Cam".into(), bus_info: "usb-0000:00:14.0-1".into(), capabilities: cap, device_caps: devcaps, formats: fmts.to_vec() }
    }

    #[test]
    fn filtering_by_capabilities() {
        let stream_cap = CAP_VIDEO_CAPTURE | CAP_STREAMING;
        // UVC reports the node's own caps in device_caps when the flag is set.
        assert!(classify("/dev/video0", &caps(CAP_DEVICE_CAPS | stream_cap, stream_cap, &[MJPG, YUYV])).is_ok());
        assert_eq!(classify("/dev/video1", &caps(CAP_DEVICE_CAPS | stream_cap | CAP_METADATA_CAPTURE, CAP_METADATA_CAPTURE | CAP_STREAMING, &[])).unwrap_err(), "metadata node");
        // The driver-wide capabilities must not make a node look like a capture device.
        assert_eq!(classify("/dev/video2", &caps(CAP_DEVICE_CAPS | stream_cap, CAP_STREAMING, &[YUYV])).unwrap_err(), "not a capture device");
        assert_eq!(classify("/dev/video3", &caps(CAP_VIDEO_CAPTURE, 0, &[YUYV])).unwrap_err(), "no streaming I/O");
        assert!(classify("/dev/video4", &caps(stream_cap, 0, &[YUYV])).is_ok(), "old drivers without DEVICE_CAPS use capabilities");
        let isp = Caps { driver: "bcm2835-isp".into(), card: "bcm2835-isp".into(), bus_info: "platform:bcm2835-isp".into(), capabilities: CAP_VIDEO_CAPTURE | CAP_STREAMING, device_caps: 0, formats: vec![YUYV] };
        assert_eq!(classify("/dev/video13", &isp).unwrap_err(), "not USB (bcm2835-isp)");
        let gspca = Caps { driver: "gspca_zc3xx".into(), bus_info: "usb-xhci-1".into(), ..isp.clone() };
        assert!(classify("/dev/video0", &gspca).is_ok(), "non-UVC USB cameras are fine");
        let e = classify("/dev/video5", &caps(stream_cap, 0, &[fourcc(b"H264")])).unwrap_err();
        assert!(e.contains("H264"), "{e}");
    }

    #[test]
    fn uvc_first_then_numeric_order() {
        let d = |p: &str, uvc| Device { path: p.into(), name: p.into(), uvc, formats: vec![], sizes: vec![] };
        let mut v = vec![d("/dev/video10", false), d("/dev/video2", false), d("/dev/video4", true), d("/dev/video0", true)];
        sort_devices(&mut v);
        let order: Vec<_> = v.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(order, ["/dev/video0", "/dev/video4", "/dev/video2", "/dev/video10"]);
    }

    #[test]
    fn size_choice() {
        let all = [(1920, 1080), (1280, 720), (640, 480), (320, 240), (160, 120), (80, 60), (640, 480)];
        let s = offer_sizes(&all);
        assert_eq!(s, [(160, 120), (320, 240), (640, 480), (1280, 720)]);
        assert_eq!(default_size(&s), Some((640, 480)));
        assert_eq!(default_size(&[(1280, 720)]), Some((1280, 720)));
        assert_eq!(default_size(&[]), None);
    }

    #[test]
    fn control_nudging_stays_on_the_step_grid() {
        let r = CtrlRange { min: 0, max: 255, step: 1, value: 128 };
        assert_eq!(r.nudged(1), 153);
        assert_eq!(r.nudged(-1), 103);
        let top = CtrlRange { value: 250, ..r };
        assert_eq!(top.nudged(1), 255);
        let coarse = CtrlRange { min: -64, max: 64, step: 8, value: 0 };
        assert_eq!((coarse.nudged(1) - coarse.min) % 8, 0);
        assert!((coarse.frac() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn mock_delivers_frames() {
        let b = Mock::Pattern;
        let p = b.probe();
        let mut s = b.open(&p.devices[0], Some((160, 120)), true).unwrap();
        let mut got = None;
        for _ in 0..10 {
            if let Some(f) = s.next(Duration::from_millis(100)).unwrap() {
                got = Some(f);
                break;
            }
        }
        assert!(matches!(got, Some(Raw::Rgb { w: 160, h: 120, .. })));
        let r = s.ctrl(Ctrl::Brightness).unwrap();
        s.set_ctrl(Ctrl::Brightness, 70);
        assert_eq!(s.ctrl(Ctrl::Brightness).unwrap().value, 70);
        assert_eq!(r.value, 50);
    }
}
