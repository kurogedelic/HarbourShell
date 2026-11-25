//! Recorder: a voice recorder for the built-in microphone and USB audio devices (UAC1 / UAC2).
//!
//! * **Capture** is a child process (`arecord -q -D plughw:CARD=<id>,DEV=<n> -f S16_LE -r <rate> -c <ch>
//!   -t raw`) whose stdout is read on a worker thread; the UI thread never waits for audio or disk.
//!   The worker also writes the WAV file (header rewritten every second, so a killed app still
//!   leaves a valid file) and sends levels / waveform columns to the UI over a channel.
//!   `plughw:` lets ALSA convert rate and sample format, so any USB microphone works; the
//!   rate / channel count are picked from `arecord --dump-hw-params` ([`negotiate`]).
//! * **Devices** come from `arecord -l` (USB cards are recognised by `/proc/asound/cardN/usbid`) and are
//!   rescanned every 2 s while the app is idle, so a hot-plugged headset shows up by itself.
//! * **Files** go to `~/Music/Recordings/REC_YYYYMMDD_HHMMSS.wav`, a sub-folder the Music app scans.
//! * `HARBOUR_MOCK=1` uses a synthetic microphone (and a scratch folder in the temp dir);
//!   `HARBOUR_MOCK_NODEV=1` makes the mock list empty.
//!
//! Keys: Return / Space record / stop, P pause, Left / Right input device, Down (or 2) the Files
//! tab, Up (or 1) back. Files: Up / Down select, Return plays, R renames, Delete moves to the Trash.
//! A short Esc never leaves while recording: it asks. The chosen input and rate are remembered in
//! `~/.config/harbour/recorder.txt` (apps cannot reach `config.json`).

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use crate::keys::{Keycode, Mod};
use crate::gfx::Rect;
use crate::gfx::Canvas;

use crate::assets::AssetStore;
use crate::chrome;
use crate::font::FontSystem;
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::apps::music::{Audio, Ffplay, Proc};
use crate::launcher::fileops;
use crate::launcher::i18n;
use crate::launcher::dialogs::{ConfirmAlert, ConfirmChoice, NamePrompt, PromptResult};
use crate::launcher::sevenseg;
use crate::launcher::widgets::{self, contains, BtnState, MenuItem, PointerEvent, ScrollBar, Tabs};
use crate::platform::keys::arrow_alias;
use crate::theme::ThemeManager;
use crate::ui;

/// Sample rates tried in this order.
pub const RATES: [u32; 3] = [48_000, 44_100, 16_000];
/// One waveform column covers this many milliseconds.
const COL_MS: u32 = 25;
/// Columns kept for the scrolling waveform (1 px each).
const WAVE_COLS: usize = 312;

// ------------------------------------------------------------------ devices

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub card: u32,
    /// The short card id (`ES8389Audio`, `Device`, `USB`...), used in ALSA names and remembered.
    pub id: String,
    /// Human name from the brackets of `arecord -l`.
    pub name: String,
    pub dev: u32,
    pub usb: bool,
}

impl Device {
    pub fn hw(&self) -> String {
        format!("hw:CARD={},DEV={}", self.id, self.dev)
    }
    pub fn plug(&self) -> String {
        format!("plughw:CARD={},DEV={}", self.id, self.dev)
    }
    /// The name shown on the screen.
    pub fn label(&self) -> String {
        if self.usb && !self.name.to_lowercase().contains("usb") {
            format!("{} (USB)", self.name)
        } else {
            self.name.clone()
        }
    }
    /// Key kept in the settings file.
    pub fn key(&self) -> String {
        format!("{}:{}", self.id, self.dev)
    }
}

/// Parse `arecord -l`. `is_usb(card)` tells whether a card sits on the USB bus.
pub fn parse_arecord_l(text: &str, is_usb: &dyn Fn(u32) -> bool) -> Vec<Device> {
    let mut v = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("card ") else { continue };
        let Some((num, rest)) = rest.split_once(':') else { continue };
        let Ok(card) = num.trim().parse::<u32>() else { continue };
        let rest = rest.trim_start();
        let Some((id, rest)) = rest.split_once(" [") else { continue };
        let Some((name, rest)) = rest.split_once(']') else { continue };
        let Some(d) = rest.split("device ").nth(1) else { continue };
        let Some(dev) = d.split(':').next().and_then(|n| n.trim().parse::<u32>().ok()) else { continue };
        v.push(Device { card, id: id.trim().to_string(), name: name.trim().to_string(), dev, usb: is_usb(card) });
    }
    v
}

pub struct Scan {
    pub devices: Vec<Device>,
    /// What was looked at, for the "no device" card.
    pub notes: Vec<String>,
}

fn real_is_usb(card: u32) -> bool {
    Path::new(&format!("/proc/asound/card{card}/usbid")).exists() || Path::new(&format!("/proc/asound/card{card}/usbbus")).exists()
}

fn mock_devices() -> Vec<Device> {
    if std::env::var_os("HARBOUR_MOCK_NODEV").is_some() {
        return Vec::new();
    }
    vec![
        Device { card: 1, id: "ES8389Audio".into(), name: "ES8389-Audio".into(), dev: 0, usb: false },
        Device { card: 2, id: "Headset".into(), name: "USB Headset Mic".into(), dev: 0, usb: true },
    ]
}

pub fn scan_devices(mock: bool) -> Scan {
    if mock {
        let devices = mock_devices();
        let notes = if devices.is_empty() {
            vec!["arecord -l: no capture cards (mock)".into(), "/proc/asound/cards: 1 card, 0 capture (mock)".into(), "USB audio: none (mock)".into()]
        } else {
            Vec::new()
        };
        return Scan { devices, notes };
    }
    let mut notes = Vec::new();
    let devices = match Command::new("arecord").arg("-l").stdin(Stdio::null()).output() {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            let d = parse_arecord_l(&text, &real_is_usb);
            notes.push(format!("arecord -l: {} capture device(s)", d.len()));
            d
        }
        Err(e) => {
            notes.push(format!("arecord -l: {e} (alsa-utils?)"));
            Vec::new()
        }
    };
    if devices.is_empty() {
        let cards = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
        let n = cards.lines().filter(|l| l.trim_start().chars().next().map_or(false, |c| c.is_ascii_digit())).count();
        notes.push(format!("/proc/asound/cards: {n} card(s), none can record"));
        let usb = (0..8).filter(|c| real_is_usb(*c)).count();
        notes.push(format!("USB audio cards: {usb}"));
    }
    Scan { devices, notes }
}

// ------------------------------------------------------------ rate negotiation

/// What a device can do, from `arecord --dump-hw-params`: inclusive (min, max) ranges.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Caps {
    pub rates: Vec<(u32, u32)>,
    pub channels: Vec<(u32, u32)>,
}

fn parse_ranges(v: &str) -> Vec<(u32, u32)> {
    // `[8000 96000]` = a range; bare numbers (`44100 48000` or `2`) are single values.
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let n: Vec<u32> = inner.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if n.len() == 2 {
            return vec![(n[0], n[1])];
        }
    }
    v.split_whitespace().filter_map(|x| x.parse::<u32>().ok()).map(|x| (x, x)).collect()
}

pub fn parse_caps(text: &str) -> Option<Caps> {
    if !text.contains("HW Params") {
        return None;
    }
    let mut caps = Caps::default();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("RATE:") {
            caps.rates = parse_ranges(v);
        } else if let Some(v) = line.strip_prefix("CHANNELS:") {
            caps.channels = parse_ranges(v);
        }
    }
    Some(caps)
}

fn within(r: &[(u32, u32)], x: u32) -> bool {
    r.iter().any(|&(a, b)| (a..=b).contains(&x))
}

/// Pick (rate, channels): the preferred rate if the device offers it, then 48000, 44100, 16000;
/// mono if possible, else stereo, else the smallest the device has. Without caps (probe failed):
/// 48000 mono, and `plughw:` converts.
pub fn negotiate(caps: Option<&Caps>, preferred: Option<u32>) -> (u32, u16) {
    let mut order: Vec<u32> = Vec::new();
    if let Some(p) = preferred {
        order.push(p);
    }
    order.extend(RATES);
    let Some(c) = caps else { return (order[0], 1) };
    let rate = order.iter().copied().find(|&r| c.rates.is_empty() || within(&c.rates, r)).or_else(|| {
        // None of ours: take the highest the device has that is at most 48000, else its lowest.
        let max = c.rates.iter().map(|r| r.1).max()?;
        let min = c.rates.iter().map(|r| r.0).min()?;
        Some(if max >= 48_000 { 48_000 } else { max.max(min) })
    });
    let ch = if c.channels.is_empty() || within(&c.channels, 1) {
        1
    } else if within(&c.channels, 2) {
        2
    } else {
        c.channels.iter().map(|r| r.0).min().unwrap_or(1) as u16
    };
    (rate.unwrap_or(48_000), ch.max(1))
}

// --------------------------------------------------------------- errors

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecError {
    pub title: String,
    pub lines: Vec<String>,
}

impl RecError {
    fn new(title: &str, lines: Vec<String>) -> Self {
        Self { title: title.to_string(), lines }
    }
}

/// Turn arecord's complaint into a message card.
pub fn explain_error(text: &str, hw: &str) -> RecError {
    let low = text.to_lowercase();
    if low.contains("busy") {
        RecError::new("Input is busy", vec![format!("{hw} is in use by another program."), "Close what uses the microphone, then try again.".into(), "Probed: hw params (busy)".into()])
    } else if low.contains("permission denied") || low.contains("operation not permitted") {
        RecError::new("Permission denied", vec![format!("This user may not open {hw}."), "Add the user to the 'audio' group:".into(), "  usermod -aG audio <user>".into()])
    } else if low.contains("no such file") || low.contains("no such device") || low.contains("cannot find card") || low.contains("unable to open") && low.contains("no such") {
        RecError::new("Input not found", vec![format!("{hw} is gone."), "Was the USB device unplugged?".into()])
    } else {
        let mut lines: Vec<String> = text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).take(4).collect();
        if lines.is_empty() {
            lines.push("arecord ended without a message.".into());
        }
        RecError::new("Recording failed", lines)
    }
}

// ------------------------------------------------------------------ WAV

pub fn wav_header(rate: u32, ch: u16, data_bytes: u32) -> [u8; 44] {
    let block = ch * 2;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36u32.saturating_add(data_bytes)).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&ch.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * block as u32).to_le_bytes());
    h[32..34].copy_from_slice(&block.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    h
}

/// (rate, channels, data bytes) of a canonical 44-byte PCM16 header.
pub fn parse_wav_header(h: &[u8]) -> Option<(u32, u16, u32)> {
    if h.len() < 44 || &h[0..4] != b"RIFF" || &h[8..12] != b"WAVE" || &h[36..40] != b"data" {
        return None;
    }
    let ch = u16::from_le_bytes([h[22], h[23]]);
    let rate = u32::from_le_bytes([h[24], h[25], h[26], h[27]]);
    let data = u32::from_le_bytes([h[40], h[41], h[42], h[43]]);
    (ch > 0 && rate > 0).then_some((rate, ch, data))
}

/// Seconds of audio in a WAV file of `len` bytes with this header (a killed writer may have left
/// a stale size: the real file length wins when it is smaller or the size is 0).
pub fn wav_seconds(header: &[u8], len: u64) -> Option<f64> {
    let (rate, ch, data) = parse_wav_header(header)?;
    let real = len.saturating_sub(44);
    let bytes = if data == 0 { real } else { (data as u64).min(real) };
    Some(bytes as f64 / (rate as f64 * ch as f64 * 2.0))
}

/// Rewrite the header in place, leaving the write position at the end.
fn patch_header(f: &mut std::fs::File, rate: u32, ch: u16, data: u64) -> std::io::Result<()> {
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&wav_header(rate, ch, data.min(u32::MAX as u64) as u32))?;
    f.seek(SeekFrom::End(0))?;
    Ok(())
}

pub fn rec_file_name(now: DateTime<Local>) -> String {
    now.format("REC_%Y%m%d_%H%M%S.wav").to_string()
}

/// `name`, or `name_2`, `name_3`... until `exists` says it is free.
pub fn unique_path(dir: &Path, file: &str, exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    let p = dir.join(file);
    if !exists(&p) {
        return p;
    }
    let stem = file.strip_suffix(".wav").unwrap_or(file);
    (2..)
        .map(|n| dir.join(format!("{stem}_{n}.wav")))
        .find(|p| !exists(p))
        .unwrap()
}

/// A typed name -> a safe file name ending in `.wav`.
pub fn sanitize_name(input: &str) -> Option<String> {
    let s: String = input.trim().chars().filter(|c| !c.is_control() && !"/\\:*?\"<>|".contains(*c)).collect();
    let s = s.trim();
    let s = if s.to_lowercase().ends_with(".wav") { &s[..s.len() - 4] } else { s };
    let s = s.trim().trim_start_matches('.').trim();
    (!s.is_empty()).then(|| format!("{s}.wav"))
}

// --------------------------------------------------------------- levels

/// (peak, rms) of interleaved samples, 0.0 ..= 1.0 of full scale.
pub fn peak_rms(samples: &[i16]) -> (f32, f32) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut peak = 0i32;
    let mut sum = 0f64;
    for &s in samples {
        let a = (s as i32).abs();
        peak = peak.max(a);
        sum += (s as f64) * (s as f64);
    }
    (peak as f32 / 32768.0, (sum / samples.len() as f64).sqrt() as f32 / 32768.0)
}

pub fn to_db(x: f32) -> f32 {
    if x <= 0.000_03 { -90.0 } else { 20.0 * x.log10() }
}

/// Meter position for a level in dB: -60 dB = empty, 0 dB = full.
pub fn meter_frac(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

pub fn level_hint(peak_db: f32) -> &'static str {
    if peak_db > -0.5 {
        "CLIP"
    } else if peak_db > -6.0 {
        "HOT"
    } else if peak_db < -40.0 {
        "LOW"
    } else {
        "OK"
    }
}

pub fn fmt_clock(secs: u64) -> (u64, u64, u64) {
    (secs / 3600, secs / 60 % 60, secs % 60)
}

pub fn fmt_duration(secs: f64) -> String {
    let (h, m, s) = fmt_clock(secs.max(0.0) as u64);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

pub fn fmt_size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1}G", bytes as f64 / (1u64 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.1}M", bytes as f64 / (1u64 << 20) as f64)
    } else {
        format!("{}K", bytes.div_ceil(1024))
    }
}

/// "Free 20.1 GB" style text.
pub fn fmt_free(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MB", bytes >> 20)
    }
}

/// How long the free space lasts at this format.
pub fn fmt_remaining(free: u64, rate: u32, ch: u16) -> String {
    let bps = (rate as u64 * ch as u64 * 2).max(1);
    let secs = free / bps;
    let (h, m, _) = fmt_clock(secs);
    if h >= 100 {
        format!("{h} h")
    } else if h > 0 {
        format!("{h} h {m:02} min")
    } else {
        format!("{m} min")
    }
}

fn free_bytes(dir: &Path) -> Option<u64> {
    if crate::rt::frozen() {
        return Some(8 << 30); // the real disk changes between runs
    }
    let mut p = dir;
    while !p.exists() {
        p = p.parent()?;
    }
    let c = std::ffi::CString::new(p.to_string_lossy().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0).then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

// -------------------------------------------------------------- capture

pub enum Msg {
    Started { rate: u32, ch: u16, path: PathBuf },
    Level { peak: f32, rms: f32, frames: u64 },
    Wave(Vec<(i16, i16)>),
    /// Capture ended; `error` is set when it was not asked for.
    Done { frames: u64, error: Option<RecError> },
    Failed(RecError),
}

struct Job {
    dev: Device,
    rate_pref: Option<u32>,
    dir: PathBuf,
    mock: bool,
    now: DateTime<Local>,
}

/// A synthetic microphone: a voice-like tone with a slow envelope, noise and the odd pop.
pub struct MockSignal {
    n: u64,
    rate: u32,
    seed: u32,
}

impl MockSignal {
    pub fn new(rate: u32) -> Self {
        Self { n: 0, rate, seed: 12345 }
    }
    pub fn fill(&mut self, out: &mut [i16], ch: usize) {
        for frame in out.chunks_mut(ch.max(1)) {
            let t = self.n as f32 / self.rate as f32;
            self.seed = self.seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((self.seed >> 16) as f32 / 32768.0 - 1.0) * 0.02;
            let env = (0.5 + 0.5 * (t * 1.7).sin()).powi(2) * (0.35 + 0.35 * (t * 0.31).sin().abs());
            let v = env * ((t * 2.0 * std::f32::consts::PI * 180.0).sin() * 0.6 + (t * 2.0 * std::f32::consts::PI * 540.0).sin() * 0.3) + noise;
            let s = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
            for x in frame.iter_mut() {
                *x = s;
            }
            self.n += 1;
        }
    }
}

enum Source {
    Child(std::process::ChildStdout),
    Mock { sig: MockSignal, ch: usize, started: Instant, given: u64 },
}

impl Source {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Source::Child(o) => o.read(buf),
            Source::Mock { sig, ch, started, given } => {
                // Pace to real time: hand out what has "arrived" since the start.
                let rate = sig.rate as f64;
                loop {
                    let due = (started.elapsed().as_secs_f64() * rate) as u64;
                    if due > *given + (rate / 50.0) as u64 {
                        let frames = ((due - *given) as usize).min(buf.len() / (2 * *ch));
                        let mut samples = vec![0i16; frames * *ch];
                        sig.fill(&mut samples, *ch);
                        for (i, s) in samples.iter().enumerate() {
                            buf[i * 2..i * 2 + 2].copy_from_slice(&s.to_le_bytes());
                        }
                        *given += frames as u64;
                        return Ok(frames * 2 * *ch);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }
}

/// Handle to a running capture; dropping it stops the child and joins the worker.
pub struct Session {
    rx: Receiver<Msg>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    pid: Arc<AtomicI32>,
    join: Option<JoinHandle<()>>,
}

impl Session {
    fn start(job: Job) -> Session {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let (s2, p2, pid2) = (stop.clone(), paused.clone(), pid.clone());
        let join = std::thread::spawn(move || {
            if let Err(e) = run_capture(job, &tx, &s2, &p2, &pid2) {
                let _ = tx.send(Msg::Failed(e));
            }
        });
        Session { rx, stop, paused, pid, join: Some(join) }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let pid = self.pid.load(Ordering::SeqCst);
        if pid > 0 {
            // The child gets TERM; its closed pipe ends the worker's read.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }

    fn set_paused(&self, p: bool) {
        self.paused.store(p, Ordering::SeqCst);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn run_capture(job: Job, tx: &Sender<Msg>, stop: &AtomicBool, paused: &AtomicBool, pid: &AtomicI32) -> Result<(), RecError> {
    let (rate, ch, mut src, child) = if job.mock {
        let (rate, _) = negotiate(None, job.rate_pref);
        let ch: u16 = if job.dev.usb { 1 } else { 2 };
        (rate, ch, Source::Mock { sig: MockSignal::new(rate), ch: ch as usize, started: Instant::now(), given: 0 }, None)
    } else {
        let probe = Command::new("arecord")
            .args(["--dump-hw-params", "-D", &job.dev.hw(), "-f", "S16_LE", "-d", "1", "/dev/null"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| RecError::new("arecord not found", vec![format!("{e}"), "Install alsa-utils (apt install alsa-utils).".into()]))?;
        let text = format!("{}{}", String::from_utf8_lossy(&probe.stdout), String::from_utf8_lossy(&probe.stderr));
        let caps = parse_caps(&text);
        if caps.is_none() {
            return Err(explain_error(&text, &job.dev.hw()));
        }
        let (rate, ch) = negotiate(caps.as_ref(), job.rate_pref);
        let mut child = Command::new("arecord")
            .args(["-q", "-D", &job.dev.plug(), "-f", "S16_LE", "-r", &rate.to_string(), "-c", &ch.to_string(), "-t", "raw"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RecError::new("arecord not found", vec![format!("{e}"), "Install alsa-utils (apt install alsa-utils).".into()]))?;
        pid.store(child.id() as i32, Ordering::SeqCst);
        let out = child.stdout.take().ok_or_else(|| RecError::new("Recording failed", vec!["no pipe from arecord".into()]))?;
        (rate, ch, Source::Child(out), Some(child))
    };
    // stderr is collected for the error message when capture ends early.
    let err_text = Arc::new(Mutex::new(String::new()));
    let mut child = child;
    if let Some(Some(mut e)) = child.as_mut().map(|c| c.stderr.take()) {
        let t = err_text.clone();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = e.read_to_string(&mut s);
            if let Ok(mut g) = t.lock() {
                *g = s;
            }
        });
    }
    let kill_child = |child: &mut Option<std::process::Child>| {
        if let Some(c) = child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        pid.store(0, Ordering::SeqCst);
    };

    let setup = (|| -> Result<(PathBuf, std::fs::File), RecError> {
        std::fs::create_dir_all(&job.dir).map_err(|e| RecError::new("Cannot save here", vec![format!("{}: {e}", job.dir.display())]))?;
        let path = unique_path(&job.dir, &rec_file_name(job.now), &|p| p.exists());
        let mut f = std::fs::OpenOptions::new().create_new(true).read(true).write(true).open(&path).map_err(|e| RecError::new("Cannot save here", vec![format!("{}: {e}", path.display())]))?;
        f.write_all(&wav_header(rate, ch, 0)).map_err(|e| RecError::new("Cannot write the file", vec![e.to_string()]))?;
        Ok((path, f))
    })();
    let (path, mut file) = match setup {
        Ok(v) => v,
        Err(e) => {
            kill_child(&mut child);
            return Err(e);
        }
    };
    let _ = tx.send(Msg::Started { rate, ch, path: path.clone() });

    let frame_bytes = ch as usize * 2;
    let col_frames = (rate * COL_MS / 1000) as usize;
    let mut buf = vec![0u8; 8192 / frame_bytes * frame_bytes];
    let mut carry: Vec<u8> = Vec::new();
    let (mut frames, mut data_bytes) = (0u64, 0u64);
    let (mut peak, mut sumsq, mut n_samples) = (0f32, 0f64, 0u64);
    let (mut col_min, mut col_max, mut col_n) = (i16::MAX, i16::MIN, 0usize);
    let mut cols: Vec<(i16, i16)> = Vec::new();
    let (mut last_level, mut last_header) = (Instant::now(), Instant::now());
    let mut write_err: Option<RecError> = None;

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let n = match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        carry.extend_from_slice(&buf[..n]);
        let usable = carry.len() / frame_bytes * frame_bytes;
        let samples: Vec<i16> = carry[..usable].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        let raw: Vec<u8> = carry.drain(..usable).collect();
        let (p, r) = peak_rms(&samples);
        peak = peak.max(p);
        sumsq += (r as f64) * (r as f64) * samples.len() as f64;
        n_samples += samples.len() as u64;
        for fr in samples.chunks_exact(ch as usize) {
            let v = fr[0];
            col_min = col_min.min(v);
            col_max = col_max.max(v);
            col_n += 1;
            if col_n >= col_frames {
                cols.push((col_min, col_max));
                (col_min, col_max, col_n) = (i16::MAX, i16::MIN, 0);
            }
        }
        if !paused.load(Ordering::SeqCst) {
            if let Err(e) = file.write_all(&raw) {
                write_err = Some(RecError::new("Cannot write the file", vec![e.to_string(), "Disk full?".into()]));
                break;
            }
            frames += (usable / frame_bytes) as u64;
            data_bytes += usable as u64;
        }
        if last_header.elapsed() >= Duration::from_secs(1) {
            let _ = patch_header(&mut file, rate, ch, data_bytes);
            let _ = file.flush();
            last_header = Instant::now();
        }
        if last_level.elapsed() >= Duration::from_millis(50) {
            let rms = if n_samples > 0 { (sumsq / n_samples as f64).sqrt() as f32 } else { 0.0 };
            let _ = tx.send(Msg::Level { peak, rms, frames });
            if !cols.is_empty() {
                let _ = tx.send(Msg::Wave(std::mem::take(&mut cols)));
            }
            (peak, sumsq, n_samples) = (0.0, 0.0, 0);
            last_level = Instant::now();
        }
    }
    let stopped = stop.load(Ordering::SeqCst);
    kill_child(&mut child);
    let _ = patch_header(&mut file, rate, ch, data_bytes);
    let _ = file.flush();
    let error = if let Some(e) = write_err {
        Some(e)
    } else if stopped {
        None
    } else {
        std::thread::sleep(Duration::from_millis(50));
        let t = err_text.lock().map(|g| g.clone()).unwrap_or_default();
        Some(if t.trim().is_empty() { RecError::new("Input lost", vec!["The microphone stopped delivering audio.".into(), "Was it unplugged? The recording so far is saved.".into()]) } else { explain_error(&t, &job.dev.hw()) })
    };
    let _ = tx.send(Msg::Done { frames, error });
    Ok(())
}

// ---------------------------------------------------------------- files

#[derive(Clone, Debug)]
pub struct RecFile {
    pub path: PathBuf,
    pub name: String,
    pub secs: f64,
    pub size: u64,
}

/// WAV files in `dir`, newest name first (the names are timestamps).
pub fn list_files(dir: &Path) -> Vec<RecFile> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<RecFile> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter(|e| e.path().extension().map_or(false, |x| x.eq_ignore_ascii_case("wav")))
        .take(500)
        .filter_map(|e| {
            let path = e.path();
            let size = e.metadata().ok()?.len();
            let mut h = [0u8; 44];
            let secs = std::fs::File::open(&path).ok().and_then(|mut f| f.read_exact(&mut h).ok().map(|_| h)).and_then(|h| wav_seconds(&h, size)).unwrap_or(0.0);
            Some(RecFile { name: path.file_stem()?.to_string_lossy().into_owned(), path, secs, size })
        })
        .collect();
    v.sort_by(|a, b| b.name.to_lowercase().cmp(&a.name.to_lowercase()));
    v
}

/// Write a short synthetic WAV (mock mode's sample files).
fn write_sample(path: &Path, secs: u32, rate: u32) {
    let mut sig = MockSignal::new(rate);
    sig.n = rate as u64 * (secs as u64 * 7);
    let mut s = vec![0i16; (rate * secs) as usize];
    sig.fill(&mut s, 1);
    let mut out = wav_header(rate, 1, (s.len() * 2) as u32).to_vec();
    for x in s {
        out.extend_from_slice(&x.to_le_bytes());
    }
    let _ = std::fs::write(path, out);
}

// ---------------------------------------------------------------- settings

fn settings_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".config/harbour/recorder.txt"))
}

pub fn parse_settings(text: &str) -> (Option<String>, Option<u32>) {
    let (mut dev, mut rate) = (None, None);
    for l in text.lines() {
        if let Some(v) = l.strip_prefix("device=") {
            dev = Some(v.trim().to_string()).filter(|v| !v.is_empty());
        } else if let Some(v) = l.strip_prefix("rate=") {
            rate = v.trim().parse().ok().filter(|r| RATES.contains(r));
        }
    }
    (dev, rate)
}

// -------------------------------------------------------------------- app

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Record,
    Files,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Recording,
    Paused,
}

enum Modal {
    Name(NamePrompt, PathBuf),
    Delete(ConfirmAlert, PathBuf),
    Leave(ConfirmAlert),
}

struct Playing {
    path: PathBuf,
    proc: Box<dyn Proc>,
    since: Instant,
}

pub struct Recorder {
    tab: Tab,
    state: State,
    mock: bool,
    dir: PathBuf,
    /// Where Delete sends recordings (the freedesktop Trash; a temp one in mock mode).
    trash: Option<PathBuf>,
    devices: Vec<Device>,
    scan_notes: Vec<String>,
    scanned: bool,
    selected: Option<String>,
    rate_pref: Option<u32>,
    scan_rx: Option<Receiver<Scan>>,
    last_scan: Instant,
    session: Option<Session>,
    rate: u32,
    ch: u16,
    frames: u64,
    wave: VecDeque<(i16, i16)>,
    level: f32,
    hold: f32,
    hold_at: Instant,
    cur_path: Option<PathBuf>,
    card: Option<RecError>,
    files: Vec<RecFile>,
    cursor: usize,
    scroll: usize,
    modal: Option<Modal>,
    audio: Box<dyn Audio>,
    playing: Option<Playing>,
    pressed: Option<usize>,
    mouse: (i32, i32),
    toast: Option<String>,
    sb_drag: bool,
    dirty: bool,
}

pub fn make() -> Box<dyn LauncherApp> {
    let mock = matches!(std::env::var("HARBOUR_MOCK").as_deref(), Ok("1" | "true" | "yes" | "on"));
    let dir = if mock {
        std::env::temp_dir().join("harbour-mock-recordings")
    } else {
        fileops::user_dir(fileops::UserDir::Music).join("Recordings")
    };
    let trash = if mock { Some(std::env::temp_dir().join("harbour-mock-trash")) } else { fileops::trash_dir() };
    let (selected, rate_pref) = if mock { (None, None) } else { settings_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| parse_settings(&t)).unwrap_or((None, None)) };
    if mock && list_files(&dir).is_empty() {
        let _ = std::fs::create_dir_all(&dir);
        for (n, s) in [("REC_20261116_091503", 12), ("REC_20261117_184020", 47), ("REC_20261118_075511", 5)] {
            write_sample(&dir.join(format!("{n}.wav")), s, 16_000);
        }
    }
    let mut r = Recorder {
        tab: Tab::Record,
        state: State::Idle,
        mock,
        dir,
        trash,
        devices: Vec::new(),
        scan_notes: Vec::new(),
        scanned: false,
        selected,
        rate_pref,
        scan_rx: None,
        last_scan: Instant::now(),
        session: None,
        rate: 48_000,
        ch: 1,
        frames: 0,
        wave: VecDeque::new(),
        level: 0.0,
        hold: 0.0,
        hold_at: Instant::now(),
        cur_path: None,
        card: None,
        files: Vec::new(),
        cursor: 0,
        scroll: 0,
        modal: None,
        audio: Box::new(Ffplay { min_start: Duration::ZERO }),
        playing: None,
        pressed: None,
        mouse: (-1, -1),
        toast: None,
        sb_drag: false,
        dirty: true,
    };
    if mock {
        let s = scan_devices(true);
        r.apply_scan(s);
    } else {
        r.begin_scan();
    }
    r.refresh_files();
    Box::new(r)
}

/// The page starts 4 px under the shared folder tab strip (y 36).
const BODY_Y: i32 = widgets::FOLDER_BODY_Y + grid::GUTTER;
/// The file list: 8 rows in an outline whose bottom line (the last row's rule) is on y 168.
const FILE_ROWS: usize = 8;
const FILES_Y: i32 = grid::APP_FRAME_BOTTOM + 1 - FILE_ROWS as i32 * grid::ROW_H;
const SCAN_EVERY: Duration = Duration::from_secs(2);

fn btn_rects() -> [Rect; 3] {
    let (w, gap) = (96, 6);
    let x0 = grid::MARGIN + (grid::SCREEN_W - 2 * grid::MARGIN - (3 * w + 2 * gap)) / 2;
    // The default button's 3 px ring ends on y 168.
    let y = grid::APP_FRAME_BOTTOM + 1 - 3 - 24;
    [Rect::new(x0, y, w as u32, 24), Rect::new(x0 + w + gap, y, w as u32, 24), Rect::new(x0 + 2 * (w + gap), y, w as u32, 24)]
}

fn files_rect() -> Rect {
    Rect::new(grid::MARGIN, FILES_Y, (grid::SCROLL_X - grid::GUTTER - grid::MARGIN) as u32, (FILE_ROWS as i32 * grid::ROW_H) as u32)
}

fn fit(fs: &FontSystem, s: &str, w: i32) -> String {
    if fs.measure_text(s) <= w {
        return s.to_string();
    }
    let mut out: String = s.to_string();
    while !out.is_empty() && fs.measure_text(&format!("{out}…")) > w {
        out.pop();
    }
    format!("{out}…")
}

impl Recorder {
    fn begin_scan(&mut self) {
        if self.scan_rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let mock = self.mock;
        std::thread::spawn(move || {
            let _ = tx.send(scan_devices(mock));
        });
        self.scan_rx = Some(rx);
        self.last_scan = Instant::now();
    }

    fn apply_scan(&mut self, s: Scan) -> bool {
        let changed = !self.scanned || s.devices != self.devices || s.notes != self.scan_notes;
        self.scanned = true;
        self.devices = s.devices;
        self.scan_notes = s.notes;
        // Keep the remembered / chosen input if it is still there, else the first one.
        let keep = self.selected.as_ref().map_or(false, |k| self.devices.iter().any(|d| &d.key() == k));
        if !keep {
            self.selected = self.devices.first().map(|d| d.key()).or(self.selected.take());
            if self.devices.is_empty() {
                // Remember the choice across an unplug: keep `selected` as it was.
            }
        }
        changed
    }

    fn current(&self) -> Option<&Device> {
        let k = self.selected.as_ref()?;
        self.devices.iter().find(|d| &d.key() == k)
    }

    fn pick(&mut self, delta: isize) {
        if self.state != State::Idle || self.devices.is_empty() {
            return;
        }
        let i = self.current().and_then(|d| self.devices.iter().position(|x| x == d)).unwrap_or(0) as isize;
        let n = self.devices.len() as isize;
        self.selected = Some(self.devices[(i + delta).rem_euclid(n) as usize].key());
        self.save_settings();
    }

    fn cycle_rate(&mut self) {
        self.rate_pref = match self.rate_pref {
            None => Some(RATES[0]),
            Some(r) => RATES.iter().position(|x| *x == r).and_then(|i| RATES.get(i + 1)).copied(),
        };
        self.save_settings();
    }

    fn save_settings(&self) {
        if self.mock {
            return;
        }
        let Some(p) = settings_path() else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, format!("device={}\nrate={}\n", self.selected.clone().unwrap_or_default(), self.rate_pref.map(|r| r.to_string()).unwrap_or_default()));
    }

    fn refresh_files(&mut self) {
        self.files = list_files(&self.dir);
        self.cursor = self.cursor.min(self.files.len().saturating_sub(1));
        self.scroll = widgets::follow_scroll(self.cursor, self.scroll, FILE_ROWS, self.files.len());
    }

    fn stop_playback(&mut self) {
        if let Some(mut p) = self.playing.take() {
            p.proc.stop();
        }
    }

    fn start(&mut self, now: DateTime<Local>) {
        if self.state != State::Idle {
            return;
        }
        let Some(dev) = self.current().cloned() else {
            self.toast = Some("No input device".into());
            return;
        };
        self.stop_playback();
        self.card = None;
        self.wave.clear();
        self.frames = 0;
        self.level = 0.0;
        self.hold = 0.0;
        self.session = Some(Session::start(Job { dev, rate_pref: self.rate_pref, dir: self.dir.clone(), mock: self.mock, now }));
        self.state = State::Recording;
        self.dirty = true;
    }

    fn stop(&mut self) {
        if let Some(s) = &self.session {
            s.stop();
        }
        // The worker answers with Done; until then show idle so a second press does nothing odd.
        if self.state != State::Idle {
            self.state = State::Idle;
        }
        self.dirty = true;
    }

    fn end_session(&mut self) {
        // Drop joins the worker (it was told to stop and the child was killed).
        self.session = None;
        self.state = State::Idle;
        self.refresh_files();
        self.dirty = true;
    }

    fn toggle_pause(&mut self) {
        match self.state {
            State::Recording => {
                self.state = State::Paused;
                if let Some(s) = &self.session {
                    s.set_paused(true);
                }
            }
            State::Paused => {
                self.state = State::Recording;
                if let Some(s) = &self.session {
                    s.set_paused(false);
                }
            }
            State::Idle => {}
        }
    }

    fn poll_session(&mut self) -> bool {
        let mut changed = false;
        let mut done: Option<Option<RecError>> = None;
        if let Some(s) = &self.session {
            while let Ok(m) = s.rx.try_recv() {
                changed = true;
                match m {
                    Msg::Started { rate, ch, path } => {
                        self.rate = rate;
                        self.ch = ch;
                        self.cur_path = Some(path);
                    }
                    Msg::Level { peak, frames, .. } => {
                        self.frames = frames;
                        self.level = self.level.max(meter_frac(to_db(peak)));
                        if peak >= self.hold {
                            self.hold = peak;
                            self.hold_at = Instant::now();
                        }
                    }
                    Msg::Wave(cols) => {
                        for c in cols {
                            if self.wave.len() >= WAVE_COLS {
                                self.wave.pop_front();
                            }
                            self.wave.push_back(c);
                        }
                    }
                    Msg::Done { error, .. } => done = Some(error),
                    Msg::Failed(e) => done = Some(Some(e)),
                }
            }
        }
        if let Some(err) = done {
            self.end_session();
            self.card = err;
            if self.card.is_none() {
                self.toast = Some("Saved".into());
            }
        }
        changed
    }

    fn play_selected(&mut self) {
        if self.state != State::Idle {
            self.toast = Some("Stop recording first".into());
            return;
        }
        let Some(f) = self.files.get(self.cursor).cloned() else { return };
        if self.playing.as_ref().map_or(false, |p| p.path == f.path) {
            self.stop_playback();
            return;
        }
        self.stop_playback();
        match self.audio.spawn(&f.path, Duration::ZERO, None) {
            Ok(proc) => self.playing = Some(Playing { path: f.path, proc, since: Instant::now() }),
            Err(e) => self.toast = Some(e),
        }
    }

    fn ask_delete(&mut self) {
        if let Some(f) = self.files.get(self.cursor) {
            self.modal = Some(Modal::Delete(ConfirmAlert::new(vec![format!("Move \"{}\" to the Trash?", f.name), "Get it back from Files > Go to Trash.".into()], "Trash", false), f.path.clone()));
        }
    }

    fn ask_rename(&mut self) {
        if let Some(f) = self.files.get(self.cursor) {
            let mut p = NamePrompt::new("Rename recording:", &f.name);
            p.max = 40;
            self.modal = Some(Modal::Name(p, f.path.clone()));
        }
    }

    fn do_modal(&mut self, m: Modal, ok: bool, name: Option<String>) -> AppAction {
        match (m, ok) {
            (Modal::Delete(_, path), true) => {
                if self.playing.as_ref().map_or(false, |p| p.path == path) {
                    self.stop_playback();
                }
                let r = match &self.trash {
                    Some(t) => fileops::trash(&path, t).map(|_| ()),
                    None => Err("No Trash folder (HOME is not set)".to_string()),
                };
                match r {
                    Ok(()) => {
                        self.refresh_files();
                        return AppAction::Toast("Moved to the Trash".into());
                    }
                    Err(e) => return AppAction::Toast(format!("Delete failed: {e}")),
                }
            }
            (Modal::Name(_, path), true) => {
                let Some(new) = name.as_deref().and_then(sanitize_name) else { return AppAction::Toast("Not a valid name".into()) };
                let to = path.with_file_name(&new);
                if to == path {
                    return AppAction::None;
                }
                if to.exists() {
                    return AppAction::Toast("That name exists".into());
                }
                if self.playing.as_ref().map_or(false, |p| p.path == path) {
                    self.stop_playback();
                }
                match std::fs::rename(&path, &to) {
                    Ok(()) => {
                        self.refresh_files();
                        if let Some(i) = self.files.iter().position(|f| f.path == to) {
                            self.cursor = i;
                            self.scroll = widgets::follow_scroll(i, self.scroll, FILE_ROWS, self.files.len());
                        }
                    }
                    Err(e) => return AppAction::Toast(format!("Rename failed: {e}")),
                }
            }
            (Modal::Leave(_), true) => {
                self.stop();
                self.end_session();
                return AppAction::Quit;
            }
            _ => {}
        }
        AppAction::None
    }

    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let r = files_rect();
        if !contains(r, x, y) {
            return None;
        }
        let i = self.scroll + ((y - r.y()) / grid::ROW_H) as usize;
        (i < self.files.len()).then_some(i)
    }

    fn scrollbar(&self) -> ScrollBar {
        ScrollBar::new(Rect::new(grid::SCROLL_X, FILES_Y, grid::SCROLL_W as u32, (FILE_ROWS as i32 * grid::ROW_H) as u32), self.files.len(), FILE_ROWS, self.scroll)
    }

    fn press_button(&mut self, i: usize, now: DateTime<Local>) {
        match (i, self.state) {
            (0, State::Idle) => self.start(now),
                        (1, State::Recording | State::Paused) => self.toggle_pause(),
            (2, State::Recording | State::Paused) => self.stop(),
            _ => {}
        }
    }

    fn btn_disabled(&self, i: usize) -> bool {
        let no_dev = self.current().is_none();
        match i {
            0 => no_dev || self.state != State::Idle,
            _ => self.state == State::Idle,
        }
    }

    fn decay(&mut self) {
        // The meter falls back after each frame; the peak tick holds 1.5 s.
        self.level *= 0.85;
        if self.hold_at.elapsed() > Duration::from_millis(1500) {
            self.hold *= 0.9;
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop_playback();
        if let Some(s) = &self.session {
            s.stop();
        }
        self.session = None;
    }
}

impl LauncherApp for Recorder {
    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        let mut v = vec![k("Tab / Shift+Tab", "Next / previous tab"), k("1 / 2", "Record / Files")];
        v.extend(match self.tab {
            Tab::Record => vec![k("Return / Space", "Start / stop recording"), k("P", "Pause / resume"), k("Left / Right", "Choose the input")],
            Tab::Files => vec![k("Up / Down", "Choose a row"), k("Return / Space", "Play / Stop"), k("R / F2", "Rename"), k("Backspace", "Delete")],
        });
        v
    }

    /// Tab switches Record / Files, so a double Tab here is not the Agent (Alt+Tab is).
    fn wants_tab(&self) -> bool {
        true
    }

    fn title(&self) -> String {
        crate::launcher::i18n::app_name("voicememo")
    }

    /// Hold Esc / menu Quit while recording: ask first (Stop, save the file and leave?).
    fn request_close(&mut self) -> bool {
        if self.state == State::Idle {
            return true;
        }
        if !matches!(self.modal, Some(Modal::Leave(_))) {
            self.modal = Some(Modal::Leave(ConfirmAlert::new(vec!["Recording in progress".into(), "Stop, save the file and leave?".into()], "Stop", false)));
        }
        self.dirty = true;
        false
    }

    fn menu(&self) -> Vec<MenuItem> {
        let input = self.current().map(|d| d.label()).unwrap_or_else(|| i18n::tr("none").into());
        let rate = self.rate_pref.map(|r| format!("{} Hz", r)).unwrap_or_else(|| i18n::tr("Auto").into());
        let mut v = vec![
            MenuItem::new("input", format!("{}: {input}", i18n::tr("Input"))),
            MenuItem::new("rate", format!("{}: {rate}", i18n::tr("Rate"))),
            MenuItem::new("toggle", if self.state == State::Idle { "Start recording" } else { "Stop recording" }),
        ];
        if self.state != State::Idle {
            v.push(MenuItem::new("pause", if self.state == State::Paused { "Resume" } else { "Pause" }));
        }
        if self.tab == Tab::Files {
            v.push(MenuItem::new("play", "Play / Stop"));
            v.push(MenuItem::new("rename", "Rename"));
            v.push(MenuItem::new("delete", "Delete"));
        }
        v
    }

    fn context_menu(&mut self, x: i32, y: i32, _ctx: &Ctx) -> Option<Vec<MenuItem>> {
        if self.tab == Tab::Files {
            if let Some(i) = self.row_at(x, y) {
                self.cursor = i;
            }
        }
        Some(self.menu())
    }

    fn menu_action(&mut self, id: &str, ctx: &Ctx) -> AppAction {
        match id {
            "input" => self.pick(1),
            "rate" => {
                if self.state == State::Idle {
                    self.cycle_rate()
                }
            }
            "toggle" => {
                if self.state == State::Idle {
                    self.start(ctx.now)
                } else {
                    self.stop()
                }
            }
            "pause" => self.toggle_pause(),
            "play" => self.play_selected(),
            "rename" => self.ask_rename(),
            "delete" => self.ask_delete(),
            _ => {}
        }
        self.dirty = true;
        AppAction::None
    }

    fn key(&mut self, key: Keycode, _mods: Mod, _repeat: bool, ctx: &Ctx) -> AppAction {
        self.dirty = true;
        if let Some(m) = self.modal.take() {
            return match m {
                Modal::Name(mut p, path) => match p.key(key) {
                    Some(PromptResult::Ok(n)) => self.do_modal(Modal::Name(p, path), true, Some(n)),
                    Some(PromptResult::Cancel) => AppAction::None,
                    None => {
                        self.modal = Some(Modal::Name(p, path));
                        AppAction::None
                    }
                },
                Modal::Delete(mut a, path) => match a.key(key) {
                    Some(ConfirmChoice::Ok) => self.do_modal(Modal::Delete(a, path), true, None),
                    Some(ConfirmChoice::Cancel) => AppAction::None,
                    None => {
                        self.modal = Some(Modal::Delete(a, path));
                        AppAction::None
                    }
                },
                Modal::Leave(mut a) => match a.key(key) {
                    Some(ConfirmChoice::Ok) => self.do_modal(Modal::Leave(a), true, None),
                    Some(ConfirmChoice::Cancel) => AppAction::None,
                    None => {
                        self.modal = Some(Modal::Leave(a));
                        AppAction::None
                    }
                },
            };
        }
        if self.card.is_some() {
            if matches!(key, Keycode::Return | Keycode::KpEnter | Keycode::Escape | Keycode::Space) {
                self.card = None;
            }
            return AppAction::None;
        }
        let k = arrow_alias(key);
        if k == Keycode::Escape {
            if self.state != State::Idle {
                self.modal = Some(Modal::Leave(ConfirmAlert::new(vec!["Recording in progress".into(), "Stop, save the file and leave?".into()], "Stop", false)));
                return AppAction::None;
            }
            if self.tab == Tab::Files {
                self.tab = Tab::Record;
                return AppAction::None;
            }
            // Top level: a short Esc does not leave (hold Esc does), so playback keeps going.
            return AppAction::Quit;
        }
        match k {
            // Tab / Shift+Tab: the other tab (two tabs), as in every tabbed app.
            Keycode::Tab if self.tab == Tab::Files => self.tab = Tab::Record,
            Keycode::Tab => {
                self.tab = Tab::Files;
                self.refresh_files();
            }
            Keycode::Num1 | Keycode::Kp1 => self.tab = Tab::Record,
            Keycode::Num2 | Keycode::Kp2 => {
                self.tab = Tab::Files;
                self.refresh_files();
            }
            _ => {}
        }
        match self.tab {
            Tab::Record => match k {
                Keycode::Return | Keycode::KpEnter | Keycode::Space => {
                    if self.state == State::Idle {
                        self.start(ctx.now)
                    } else {
                        self.stop()
                    }
                }
                Keycode::P => self.toggle_pause(),
                Keycode::Left => self.pick(-1),
                Keycode::Right => self.pick(1),
                Keycode::Down => {
                    self.tab = Tab::Files;
                    self.refresh_files();
                }
                _ => {}
            },
            Tab::Files => match k {
                Keycode::Up => {
                    if self.cursor == 0 {
                        self.tab = Tab::Record;
                    } else {
                        self.cursor -= 1;
                    }
                }
                Keycode::Down => self.cursor = (self.cursor + 1).min(self.files.len().saturating_sub(1)),
                Keycode::Return | Keycode::KpEnter | Keycode::Space => self.play_selected(),
                Keycode::Delete | Keycode::Backspace => self.ask_delete(),
                Keycode::R | Keycode::F2 => self.ask_rename(),
                _ => {}
            },
        }
        self.scroll = widgets::follow_scroll(self.cursor, self.scroll, FILE_ROWS, self.files.len());
        AppAction::None
    }

    fn text(&mut self, text: &str, _ctx: &Ctx) -> AppAction {
        if let Some(Modal::Name(p, _)) = &mut self.modal {
            p.text(text);
            self.dirty = true;
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> AppAction {
        self.dirty = true;
        if let PointerEvent::Move { x, y } | PointerEvent::Drag { x, y, .. } = ev {
            self.mouse = (x, y);
        }
        if let Some(m) = self.modal.take() {
            if let PointerEvent::Down { x, y, button: 0, .. } = ev {
                return match m {
                    Modal::Name(mut p, path) => match p.click(x, y) {
                        Some(PromptResult::Ok(n)) => self.do_modal(Modal::Name(p, path), true, Some(n)),
                        Some(PromptResult::Cancel) => AppAction::None,
                        None => {
                            self.modal = Some(Modal::Name(p, path));
                            AppAction::None
                        }
                    },
                    Modal::Delete(mut a, path) => match a.click(x, y) {
                        Some(ConfirmChoice::Ok) => self.do_modal(Modal::Delete(a, path), true, None),
                        Some(ConfirmChoice::Cancel) => AppAction::None,
                        None => {
                            self.modal = Some(Modal::Delete(a, path));
                            AppAction::None
                        }
                    },
                    Modal::Leave(mut a) => match a.click(x, y) {
                        Some(ConfirmChoice::Ok) => self.do_modal(Modal::Leave(a), true, None),
                        Some(ConfirmChoice::Cancel) => AppAction::None,
                        None => {
                            self.modal = Some(Modal::Leave(a));
                            AppAction::None
                        }
                    },
                };
            }
            self.modal = Some(m);
            return AppAction::None;
        }
        if self.card.is_some() {
            if let PointerEvent::Down { button: 0, .. } = ev {
                self.card = None;
            }
            return AppAction::None;
        }
        let tabs = self.tabs_widget(ctx.fs, ctx.tm);
        match ev {
            PointerEvent::Down { x, y, button: 0, clicks } => {
                if let Some(t) = tabs.hit(x, y) {
                    self.tab = if t == 0 { Tab::Record } else { Tab::Files };
                    if t == 1 {
                        self.refresh_files();
                    }
                    return AppAction::None;
                }
                match self.tab {
                    Tab::Record => {
                        if let Some(i) = btn_rects().iter().position(|r| contains(*r, x, y)) {
                            if !self.btn_disabled(i) {
                                self.pressed = Some(i);
                            }
                        } else if y >= BODY_Y && y < BODY_Y + grid::ROW_H {
                            if x < 40 {
                                self.pick(-1)
                            } else if x > grid::SCREEN_W - 40 {
                                self.pick(1)
                            }
                        }
                    }
                    Tab::Files => match self.scrollbar().hit(x, y) {
                        Some(widgets::SbPart::Thumb) => self.sb_drag = true,
                        Some(widgets::SbPart::Up | widgets::SbPart::PageUp) => self.scroll = self.scroll.saturating_sub(FILE_ROWS),
                        Some(widgets::SbPart::Down | widgets::SbPart::PageDown) => self.scroll = (self.scroll + FILE_ROWS).min(self.files.len().saturating_sub(FILE_ROWS)),
                        None => {
                            if let Some(i) = self.row_at(x, y) {
                                self.cursor = i;
                                if clicks >= 2 {
                                    self.play_selected();
                                }
                            }
                        }
                    },
                }
            }
            PointerEvent::Up { x, y, .. } => {
                self.sb_drag = false;
                if let Some(i) = self.pressed.take() {
                    if contains(btn_rects()[i], x, y) {
                        self.press_button(i, ctx.now);
                    }
                }
            }
            PointerEvent::Drag { y, .. } if self.sb_drag => self.scroll = self.scrollbar().offset_for_y(y),
            PointerEvent::Wheel { dy, .. } if self.tab == Tab::Files => {
                let max = self.files.len().saturating_sub(FILE_ROWS);
                self.scroll = if dy > 0 { self.scroll.saturating_sub(1) } else { (self.scroll + 1).min(max) };
            }
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, _ctx: &Ctx) -> bool {
        let mut changed = self.poll_session();
        if let Some(rx) = &self.scan_rx {
            if let Ok(s) = rx.try_recv() {
                self.scan_rx = None;
                changed |= self.apply_scan(s);
            }
        }
        // Hot-plug: rescan while idle (the device list is what Record shows).
        if self.scan_rx.is_none() && self.state == State::Idle && !self.mock && self.last_scan.elapsed() >= SCAN_EVERY {
            self.begin_scan();
        }
        if let Some(p) = &mut self.playing {
            if p.proc.exited() {
                self.playing = None;
                changed = true;
            }
        }
        if self.state != State::Idle || self.level > 0.01 {
            self.decay();
            changed = true;
        } else if self.level > 0.0 {
            self.level = 0.0;
            self.hold = 0.0;
            changed = true;
        }
        if self.playing.is_some() || self.state == State::Recording {
            changed = true;
        }
        std::mem::take(&mut self.dirty) | changed
    }

    fn draw(&mut self, c: &mut Canvas, _assets: &mut AssetStore, ctx: &Ctx) {
        let tm = ctx.tm;
        ui::fill_rect(c, chrome::app_rect(), tm.get_color("home_bg"));
        self.tabs_widget(ctx.fs, tm).draw_strip(c, ctx.fs, tm);
        self.draw_status(c, ctx);
        match self.tab {
            Tab::Record => self.draw_record(c, ctx),
            Tab::Files => self.draw_files(c, ctx),
        }
        if let Some(t) = self.toast.take() {
            // A one-shot note over the bottom row.
            let r = Rect::new(grid::MARGIN, grid::APP_FRAME_BOTTOM + 1 - 16, (grid::SCREEN_W - 2 * grid::MARGIN) as u32, 16);
            ui::fill_rect(c, r, tm.get_color("foreground"));
            ctx.fs.draw_text(c, r.x() + 4, widgets::text_origin(r.y(), 16), &chrome::fit_text(ctx.fs, &i18n::tr_s(&t), r.width() as i32 - 8), tm.get_color("widget_face_active"));
        }
        if let Some(card) = &self.card {
            draw_card(c, ctx, &card.title, &card.lines, "");
        }
        match &self.modal {
            Some(Modal::Name(p, _)) => p.draw(c, ctx.fs, tm),
            Some(Modal::Delete(a, _)) | Some(Modal::Leave(a)) => a.draw(c, ctx.fs, tm),
            None => {}
        }
    }
}

/// "20 min left" / "残り 20分".
fn left_text(rem: &str) -> String {
    if i18n::get() == i18n::Lang::Ja { format!("{} {}", i18n::tr("remaining"), i18n::tr_s(rem)) } else { format!("{rem} left") }
}

fn draw_card(c: &mut Canvas, ctx: &Ctx, title: &str, lines: &[String], footer: &str) {
    let tm = ctx.tm;
    let title = &i18n::tr_s(title);
    let footer = i18n::tr_s(footer);
    let r = Rect::new(12, grid::BAR_H + 8, 296, 112);
    widgets::draw_panel(c, tm, r);
    let ink = tm.get_color("foreground");
    ctx.fs.draw_display_text(c, r.x() + 10, r.y() + 7, &chrome::fit_display_text(ctx.fs, title, r.width() as i32 - 20), ink);
    let faint = widgets::secondary_ink(tm);
    for (i, l) in lines.iter().take(7).enumerate() {
        ctx.fs.draw_text(c, r.x() + 10, r.y() + 26 + i as i32 * 12, &fit(ctx.fs, &i18n::tr_s(l), r.width() as i32 - 20), faint);
    }
    if !footer.is_empty() {
        ctx.fs.draw_text(c, r.x() + 10, r.y() + r.height() as i32 - 14, &footer, ink);
    }
}

static JA_TABS: std::sync::OnceLock<[&str; 2]> = std::sync::OnceLock::new();

impl Recorder {
    fn tabs_widget(&self, fs: &FontSystem, tm: &ThemeManager) -> Tabs<'static> {
        let labels: &'static [&'static str] = if i18n::get() == i18n::Lang::Ja { JA_TABS.get_or_init(|| [i18n::tr_in(i18n::Lang::Ja, "Record"), i18n::tr_in(i18n::Lang::Ja, "Files")]) } else { &["Record", "Files"] };
        Tabs::folder_fit(fs, labels, if self.tab == Tab::Record { 0 } else { 1 }, tm)
    }

    fn draw_status(&self, c: &mut Canvas, ctx: &Ctx) {
        let (label, on) = match self.state {
            State::Recording => (i18n::tr("REC"), true),
            State::Paused => (i18n::tr("PAUSED"), false),
            State::Idle => (i18n::tr("READY"), false),
        };
        let ink = ctx.tm.get_color("foreground");
        let w = ctx.fs.measure_text(label);
        let x = grid::SCREEN_W - grid::MARGIN - w;
        // On the tab strip, level with the idle tabs' labels.
        let y = widgets::folder_strip_text_y();
        ctx.fs.draw_text(c, x, y, label, ink);
        if on && ctx.now.timestamp_subsec_millis() < 650 {
            ui::fill_rect(c, Rect::new(x - 12, y + widgets::CAP_TOP + 2, 6, 6), ink);
        }
    }

    fn draw_record(&self, c: &mut Canvas, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let faint = widgets::secondary_ink(tm);
        let row = Rect::new(grid::MARGIN, BODY_Y, (grid::SCREEN_W - 2 * grid::MARGIN) as u32, grid::ROW_H as u32);

        // Device row: < name >   rate.
        let idle = self.state == State::Idle;
        let name = self.current().map(|d| d.label()).unwrap_or_else(|| i18n::tr(if self.scanned { "No input device" } else { "Looking for inputs..." }).into());
        let n = self.devices.len();
        let arrows = idle && n > 1;
        let fmt = if self.state == State::Idle {
            self.rate_pref.map(|r| format!("{}k", r / 1000)).unwrap_or_else(|| i18n::tr("auto").into())
        } else {
            format!("{}k {}", self.rate / 1000, i18n::tr(if self.ch == 1 { "mono" } else { "stereo" }))
        };
        let fw = fs.measure_text(&fmt);
        if arrows {
            fs.draw_text(c, row.x(), widgets::text_origin(row.y(), 16), "<", ink);
            fs.draw_text(c, row.x() + row.width() as i32 - fw - 14 - 6, widgets::text_origin(row.y(), 16), ">", ink);
        }
        let nx = row.x() + 12;
        let kind = if self.current().map_or(false, |d| d.usb) { "USB " } else { "" };
        let shown = fit(fs, &format!("{kind}{name}").replace("USB USB", "USB"), row.width() as i32 - 12 - fw - 24);
        fs.draw_text(c, nx, widgets::text_origin(row.y(), 16), &shown, ink);
        fs.draw_text(c, row.x() + row.width() as i32 - fw, widgets::text_origin(row.y(), 16), &fmt, faint);

        // A rule closes the device row.
        widgets::draw_row_rule(c, tm, row);

        // Elapsed time (7 segments) and free space.
        let secs = self.frames / self.rate.max(1) as u64;
        let (h, m, s) = fmt_clock(secs);
        let (dw, dh, th) = (12, 20, 3);
        let y = BODY_Y + 20;
        let mut x = grid::MARGIN + 2;
        let col = if self.state == State::Paused { faint } else { ink };
        let mut seq: Vec<Option<usize>> = Vec::new();
        if h > 0 {
            seq.extend([Some(h as usize % 10), None]);
        }
        seq.extend([Some(m as usize / 10), Some(m as usize % 10), None, Some(s as usize / 10), Some(s as usize % 10)]);
        for e in seq {
            match e {
                Some(d) => {
                    sevenseg::draw_digit(c, x, y, dw, dh, th, d, col);
                    x += dw + 3;
                }
                None => {
                    sevenseg::draw_colon(c, x + 1, y, dh, th, col);
                    x += th + 5;
                }
            }
        }
        let free = free_bytes(&self.dir);
        let (rate, ch) = if idle { (self.rate_pref.unwrap_or(48_000), 1u16) } else { (self.rate, self.ch) };
        let rx = 150;
        if let Some(f) = free {
            // Two 12 px lines between the device row's rule and the graph: a clear pixel between
            // them and two above the graph's frame, in either language.
            fs.draw_text(c, rx, y - 4, &format!("{} {}", i18n::tr("Free space"), fmt_free(f)), ink);
            fs.draw_text(c, rx, y + 9, &left_text(&fmt_remaining(f, rate, ch)), faint);
        } else {
            fs.draw_text(c, rx, y, i18n::tr("Free space: ?"), faint);
        }

        // Waveform: an outlined graph of the last 7.8 s.
        let g = Rect::new(grid::MARGIN, 86, (grid::SCREEN_W - 2 * grid::MARGIN) as u32, 38);
        ui::fill_rect(c, g, tm.get_color("widget_face_active"));
        ui::draw_rect(c, g, ink);
        let cy = g.y() + g.height() as i32 / 2;
        let amp = (g.height() as i32 - 4) / 2;
        let line = tm.get_color("line");
        let mut dx = g.x() + 2;
        while dx < g.x() + g.width() as i32 - 2 {
            ui::fill_rect(c, Rect::new(dx, cy, 1, 1), line);
            dx += 2;
        }
        let first = g.x() + g.width() as i32 - 1 - self.wave.len() as i32;
        for (i, &(lo, hi)) in self.wave.iter().enumerate() {
            let px = first + i as i32;
            if px <= g.x() || px >= g.x() + g.width() as i32 - 1 {
                continue;
            }
            let top = cy - (hi.max(0) as i32 * amp / 32768);
            let bot = cy - (lo.min(0) as i32 * amp / 32768);
            ui::fill_rect(c, Rect::new(px, top, 1, (bot - top + 1).max(1) as u32), ink);
        }

        // Level meter with a peak-hold tick, dB and the hint.
        let mr = Rect::new(grid::MARGIN, 128, 232, 8);
        widgets::draw_meter(c, tm, mr, self.level);
        let hold_db = to_db(self.hold);
        let hx = mr.x() + 2 + ((mr.width() as i32 - 4) as f32 * meter_frac(hold_db)) as i32;
        if self.hold > 0.0 {
            ui::fill_rect(c, Rect::new((hx - 1).clamp(mr.x() + 2, mr.x() + mr.width() as i32 - 4), mr.y() + 1, 2, mr.height() - 2), tm.get_color("widget_face_active"));
            ui::fill_rect(c, Rect::new((hx - 1).clamp(mr.x() + 2, mr.x() + mr.width() as i32 - 4), mr.y() + 2, 2, mr.height() - 4), ink);
        }
        let txt = if self.state == State::Idle { "-- dB".to_string() } else { format!("{:.0} dB {}", hold_db.max(-90.0), i18n::tr(level_hint(hold_db))) };
        fs.draw_text(c, mr.x() + mr.width() as i32 + 8, widgets::text_origin(mr.y() - 4, 16), &txt, ink);

        // Buttons.
        let labels = ["Record", "Pause", "Stop"];
        let rects = btn_rects();
        for (i, r) in rects.iter().enumerate() {
            let st = BtnState {
                hover: contains(*r, self.mouse.0, self.mouse.1),
                pressed: self.pressed == Some(i),
                default: i == 0 && self.state == State::Idle && self.current().is_some(),
                disabled: self.btn_disabled(i),
            };
            let label = if i == 1 && self.state == State::Paused { "Resume" } else { labels[i] };
            widgets::draw_button(c, fs, tm, *r, label, st);
        }

        if self.current().is_none() && self.scanned {
            let mut lines = vec!["No microphone found. Probed:".to_string()];
            lines.extend(self.scan_notes.iter().cloned());
            lines.push("Plug in a USB microphone or headset;".into());
            lines.push("this list refreshes by itself.".into());
            draw_card(c, ctx, "No input device", &lines, "");
        }
    }

    fn draw_files(&self, c: &mut Canvas, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let faint = widgets::secondary_ink(tm);
        let r = files_rect();
        let outline = tm.get_color("line");
        ui::draw_rect(c, r, outline);
        if self.files.is_empty() {
            fs.draw_text(c, r.x() + 4, widgets::text_origin(r.y(), 16), i18n::tr("No recordings yet."), faint);
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let shown = home.as_deref().and_then(|h| self.dir.strip_prefix(h).ok()).unwrap_or(&self.dir).to_string_lossy().into_owned();
            let line = i18n::tr_s(&format!("Folder: {shown}"));
            fs.draw_text(c, r.x() + 4, widgets::text_origin(r.y() + 16, 16), &chrome::fit_text(fs, &line, r.width() as i32 - 8), faint);
            return;
        }
        for i in 0..FILE_ROWS {
            let idx = self.scroll + i;
            let Some(f) = self.files.get(idx) else { break };
            let row = Rect::new(r.x(), r.y() + i as i32 * grid::ROW_H, r.width(), grid::ROW_H as u32);
            let sel = idx == self.cursor;
            if sel {
                widgets::draw_row_plate(c, tm, row);
            }
            let is_playing = self.playing.as_ref().map_or(false, |p| p.path == f.path);
            let right = if is_playing {
                let t = self.playing.as_ref().map(|p| p.since.elapsed().as_secs_f64()).unwrap_or(0.0);
                format!("> {} / {}", fmt_duration(t), fmt_duration(f.secs))
            } else {
                format!("{}  {}", fmt_duration(f.secs), fmt_size(f.size))
            };
            let rw = fs.measure_text(&right);
            widgets::draw_row_text(c, fs, tm, row, row.x() + 4, &fit(fs, &f.name, row.width() as i32 - rw - 16), sel, ink);
            widgets::draw_row_text(c, fs, tm, row, row.x() + row.width() as i32 - rw - 4, &right, sel, if sel { ink } else { faint });
            widgets::draw_row_rule(c, tm, row);
        }
        let sb = self.scrollbar();
        if sb.needed() {
            sb.draw(c, tm, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const ARECORD_L: &str = "**** List of CAPTURE Hardware Devices ****\n\
card 1: ES8389Audio [ES8389-Audio], device 0: (null)-ES8389 HiFi ES8389 HiFi-0 [(null)-ES8389 HiFi ES8389 HiFi-0]\n  Subdevices: 1/1\n  Subdevice #0: subdevice #0\n\
card 2: Device [USB Audio Device], device 0: USB Audio [USB Audio]\n  Subdevices: 1/1\n\
card 3: Scarlett [Scarlett 2i2 USB], device 0: USB Audio [USB Audio]\n";

    #[test]
    fn parses_builtin_and_usb_cards() {
        let d = parse_arecord_l(ARECORD_L, &|c| c >= 2);
        assert_eq!(d.len(), 3);
        assert_eq!((d[0].card, d[0].id.as_str(), d[0].name.as_str(), d[0].usb), (1, "ES8389Audio", "ES8389-Audio", false));
        assert_eq!(d[1].hw(), "hw:CARD=Device,DEV=0");
        assert_eq!(d[1].plug(), "plughw:CARD=Device,DEV=0");
        assert!(d[1].usb && d[2].usb);
        assert_eq!(d[2].label(), "Scarlett 2i2 USB");
        assert_eq!(d[0].key(), "ES8389Audio:0");
    }

    #[test]
    fn empty_or_garbage_listing_has_no_devices() {
        assert!(parse_arecord_l("arecord: device_list:274: no soundcards found...\n", &|_| false).is_empty());
        assert!(parse_arecord_l("", &|_| false).is_empty());
    }

    #[test]
    fn usb_without_usb_in_name_gets_a_tag() {
        let d = Device { card: 2, id: "Headset".into(), name: "Jabra Evolve".into(), dev: 0, usb: true };
        assert_eq!(d.label(), "Jabra Evolve (USB)");
    }

    #[test]
    fn parses_hw_params() {
        let t = "HW Params of device \"hw:1,0\":\nCHANNELS: 2\nRATE: [8000 96000]\nPERIOD_TIME: (333 8192000]\n";
        let c = parse_caps(t).unwrap();
        assert_eq!(c.rates, vec![(8000, 96000)]);
        assert_eq!(c.channels, vec![(2, 2)]);
        assert!(parse_caps("arecord: main:850: audio open error: Device or resource busy").is_none());
        let u = parse_caps("HW Params of device:\nCHANNELS: [1 2]\nRATE: 44100 48000\n").unwrap();
        assert_eq!(u.rates, vec![(44100, 44100), (48000, 48000)]);
    }

    #[test]
    fn negotiates_rate_and_channels() {
        let builtin = parse_caps("HW Params of device:\nCHANNELS: 2\nRATE: [8000 96000]\n").unwrap();
        assert_eq!(negotiate(Some(&builtin), None), (48_000, 2));
        let only44 = Caps { rates: vec![(44_100, 44_100)], channels: vec![(1, 1)] };
        assert_eq!(negotiate(Some(&only44), None), (44_100, 1));
        let only16 = Caps { rates: vec![(16_000, 16_000)], channels: vec![(1, 2)] };
        assert_eq!(negotiate(Some(&only16), None), (16_000, 1));
        let odd = Caps { rates: vec![(96_000, 96_000)], channels: vec![(4, 4)] };
        assert_eq!(negotiate(Some(&odd), None), (48_000, 4)); // plughw converts the rate
        let low = Caps { rates: vec![(8_000, 11_025)], channels: vec![(1, 1)] };
        assert_eq!(negotiate(Some(&low), None).0, 11_025);
        assert_eq!(negotiate(Some(&builtin), Some(16_000)), (16_000, 2)); // the user's choice wins
        assert_eq!(negotiate(Some(&only44), Some(16_000)).0, 44_100); // unless the device lacks it
        assert_eq!(negotiate(None, None), (48_000, 1));
    }

    #[test]
    fn explains_arecord_errors() {
        assert_eq!(explain_error("arecord: main:850: audio open error: Device or resource busy", "hw:1,0").title, "Input is busy");
        assert_eq!(explain_error("audio open error: Permission denied", "hw:1,0").title, "Permission denied");
        assert_eq!(explain_error("audio open error: No such file or directory", "hw:1,0").title, "Input not found");
        let e = explain_error("weird\nthings", "hw:1,0");
        assert_eq!((e.title.as_str(), e.lines.len()), ("Recording failed", 2));
    }

    #[test]
    fn wav_header_is_canonical() {
        let h = wav_header(48_000, 2, 1000);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 1036);
        assert_eq!(&h[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([h[20], h[21]]), 1);
        assert_eq!(u32::from_le_bytes(h[28..32].try_into().unwrap()), 192_000);
        assert_eq!(u16::from_le_bytes([h[32], h[33]]), 4);
        assert_eq!(parse_wav_header(&h), Some((48_000, 2, 1000)));
        assert!(parse_wav_header(&h[..40]).is_none());
        assert!(parse_wav_header(&[0u8; 44]).is_none());
    }

    #[test]
    fn duration_survives_a_stale_header() {
        let h = wav_header(16_000, 1, 0);
        // header says 0 (killed before the first patch) but 32000 bytes of audio exist
        assert_eq!(wav_seconds(&h, 44 + 32_000), Some(1.0));
        let h = wav_header(16_000, 1, 64_000);
        assert_eq!(wav_seconds(&h, 44 + 32_000), Some(1.0)); // file shorter than the header claims
    }

    #[test]
    fn header_patch_keeps_file_valid_and_appendable() {
        let dir = std::env::temp_dir().join(format!("rec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.wav");
        let mut f = std::fs::OpenOptions::new().create(true).read(true).write(true).truncate(true).open(&p).unwrap();
        f.write_all(&wav_header(8000, 1, 0)).unwrap();
        f.write_all(&[1u8; 16000]).unwrap();
        patch_header(&mut f, 8000, 1, 16000).unwrap();
        f.write_all(&[2u8; 400]).unwrap(); // continues at the end, not at the header
        patch_header(&mut f, 8000, 1, 16400).unwrap();
        drop(f);
        let b = std::fs::read(&p).unwrap();
        assert_eq!(b.len(), 44 + 16400);
        assert_eq!(parse_wav_header(&b), Some((8000, 1, 16400)));
        assert_eq!(b[44 + 16000], 2);
        let listed = list_files(&dir);
        assert_eq!(listed.len(), 1);
        assert!((listed[0].secs - 1.025).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn peak_and_rms() {
        assert_eq!(peak_rms(&[]), (0.0, 0.0));
        let (p, r) = peak_rms(&[0, 16384, -32768, 0]);
        assert_eq!(p, 1.0);
        assert!((r - ((16384f32.powi(2) + 32768f32.powi(2)) / 4.0).sqrt() / 32768.0).abs() < 1e-4);
        assert!((to_db(1.0)).abs() < 1e-6);
        assert!((to_db(0.5) + 6.02).abs() < 0.01);
        assert_eq!(to_db(0.0), -90.0);
        assert_eq!(meter_frac(0.0), 1.0);
        assert_eq!(meter_frac(-30.0), 0.5);
        assert_eq!(meter_frac(-90.0), 0.0);
        assert_eq!(level_hint(0.0), "CLIP");
        assert_eq!(level_hint(-3.0), "HOT");
        assert_eq!(level_hint(-20.0), "OK");
        assert_eq!(level_hint(-50.0), "LOW");
    }

    #[test]
    fn mock_signal_is_alive_and_bounded() {
        let mut m = MockSignal::new(16_000);
        let mut s = vec![0i16; 16_000];
        m.fill(&mut s, 1);
        let (p, r) = peak_rms(&s);
        assert!(p > 0.05 && p < 1.0 && r > 0.01, "peak {p} rms {r}");
    }

    #[test]
    fn names_are_timestamps_and_never_collide() {
        let t = Local.with_ymd_and_hms(2026, 11, 18, 9, 5, 7).unwrap();
        assert_eq!(rec_file_name(t), "REC_20261118_090507.wav");
        let dir = Path::new("/x");
        let taken = |p: &Path| p == Path::new("/x/REC_20261118_090507.wav") || p == Path::new("/x/REC_20261118_090507_2.wav");
        assert_eq!(unique_path(dir, "REC_20261118_090507.wav", &taken), PathBuf::from("/x/REC_20261118_090507_3.wav"));
        assert_eq!(unique_path(dir, "other.wav", &taken), PathBuf::from("/x/other.wav"));
    }

    #[test]
    fn rename_input_is_sanitised() {
        assert_eq!(sanitize_name("  meeting notes "), Some("meeting notes.wav".into()));
        assert_eq!(sanitize_name("a/b:c.WAV"), Some("abc.wav".into()));
        assert_eq!(sanitize_name("..hidden"), Some("hidden.wav".into()));
        assert_eq!(sanitize_name("  .wav "), None);
        assert_eq!(sanitize_name("///"), None);
    }

    #[test]
    fn formats_time_size_and_remaining() {
        assert_eq!(fmt_duration(65.4), "1:05");
        assert_eq!(fmt_duration(3725.0), "1:02:05");
        assert_eq!(fmt_size(500), "1K");
        assert_eq!(fmt_size(3 << 20), "3.0M");
        assert_eq!(fmt_free(20 << 30), "20.0 GB");
        assert_eq!(fmt_free(300 << 20), "300 MB");
        // 48 kHz mono = 96000 B/s: 10 GiB lasts ~29.8 h
        assert_eq!(fmt_remaining(10 << 30, 48_000, 1), "31 h 04 min");
        assert_eq!(fmt_remaining(10 << 20, 48_000, 2), "0 min");
        assert_eq!(fmt_remaining(1 << 40, 16_000, 1), "9544 h");
    }

    #[test]
    fn settings_round_trip() {
        assert_eq!(parse_settings("device=Device:0\nrate=44100\n"), (Some("Device:0".into()), Some(44_100)));
        assert_eq!(parse_settings("device=\nrate=12345\n"), (None, None));
    }

    #[test]
    fn mock_recording_writes_a_valid_wav() {
        let dir = std::env::temp_dir().join(format!("rec-mock-{}", std::process::id()));
        let dev = mock_devices().remove(1);
        let s = Session::start(Job { dev, rate_pref: Some(16_000), dir: dir.clone(), mock: true, now: Local::now() });
        let mut frames = 0;
        let t0 = Instant::now();
        let mut done = false;
        while t0.elapsed() < Duration::from_secs(5) && !done {
            if let Ok(m) = s.rx.recv_timeout(Duration::from_millis(100)) {
                match m {
                    Msg::Level { frames: f, .. } => {
                        frames = f;
                        if frames >= 8000 {
                            s.stop();
                        }
                    }
                    Msg::Done { error, .. } => {
                        assert!(error.is_none());
                        done = true;
                    }
                    Msg::Failed(e) => panic!("{e:?}"),
                    _ => {}
                }
            }
        }
        assert!(done && frames >= 8000);
        drop(s);
        let files = list_files(&dir);
        assert_eq!(files.len(), 1);
        assert!(files[0].secs >= 0.5, "{}", files[0].secs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_moves_the_recording_to_the_trash() {
        let mut r = make_test();
        r.dir = std::env::temp_dir().join(format!("rec-test-trash-dir-{}", std::process::id()));
        let trash = r.trash.clone().unwrap();
        let _ = std::fs::remove_dir_all(&r.dir);
        let _ = std::fs::remove_dir_all(&trash);
        std::fs::create_dir_all(&r.dir).unwrap();
        write_sample(&r.dir.join("REC_1.wav"), 1, 16_000);
        r.refresh_files();
        assert_eq!(r.files.len(), 1);
        r.ask_delete();
        match &r.modal {
            Some(Modal::Delete(a, _)) => {
                assert!(a.lines.iter().all(|l| !l.contains("for good")), "{:?}", a.lines);
                assert!(a.lines.iter().any(|l| l.contains("Trash")));
            }
            _ => panic!("no delete dialog"),
        }
        let Some(Modal::Delete(a, path)) = r.modal.take() else { unreachable!() };
        let out = r.do_modal(Modal::Delete(a, path), true, None);
        assert!(matches!(out, AppAction::Toast(t) if t.contains("Trash")));
        assert!(r.files.is_empty() && !r.dir.join("REC_1.wav").exists());
        assert!(trash.join("files/REC_1.wav").exists(), "recoverable from the Trash");
        assert!(trash.join("info/REC_1.wav.trashinfo").exists());
        let _ = std::fs::remove_dir_all(&r.dir);
        let _ = std::fs::remove_dir_all(&trash);
    }

    #[test]
    fn app_can_be_made_in_mock_mode() {
        let r = Recorder::new_for_test();
        assert_eq!(r.devices.len(), 2);
        assert_eq!(r.current().map(|d| d.id.as_str()), Some("ES8389Audio"));
    }

    impl Recorder {
        fn new_for_test() -> Recorder {
            let mut r = match make_test() {
                r => r,
            };
            r.apply_scan(scan_devices(true));
            r
        }
    }

    fn make_test() -> Recorder {
        Recorder {
            tab: Tab::Record,
            state: State::Idle,
            mock: true,
            dir: std::env::temp_dir().join("rec-test-app"),
            trash: Some(std::env::temp_dir().join(format!("rec-test-trash-{}", std::process::id()))),
            devices: Vec::new(),
            scan_notes: Vec::new(),
            scanned: false,
            selected: None,
            rate_pref: None,
            scan_rx: None,
            last_scan: Instant::now(),
            session: None,
            rate: 48_000,
            ch: 1,
            frames: 0,
            wave: VecDeque::new(),
            level: 0.0,
            hold: 0.0,
            hold_at: Instant::now(),
            cur_path: None,
            card: None,
            files: Vec::new(),
            cursor: 0,
            scroll: 0,
            modal: None,
            audio: Box::new(Ffplay { min_start: Duration::ZERO }),
            playing: None,
            pressed: None,
            mouse: (-1, -1),
            toast: None,
            sb_drag: false,
            dirty: true,
        }
    }

    #[test]
    fn help_rows_are_filled_in() {
        let mut r = Recorder::new_for_test();
        crate::launcher::help::check_app_rows("Voice Memo record", &r.help());
        r.tab = Tab::Files;
        crate::launcher::help::check_app_rows("Voice Memo files", &r.help());
    }
}
