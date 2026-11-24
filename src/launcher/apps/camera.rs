//! Camera: a live preview from a V4L2 capture device (a USB UVC webcam) in the 1-bit,
//! error-diffused 1-bit look, and photos saved as full-resolution PNG files in
//! `~/Pictures` (where the Images app looks).
//!
//! Keys: Return / Space = shutter, Left / Right = other camera, Up / Down = brightness,
//! Shift + Up / Down = contrast (when the camera has those controls), Esc quits (holding it
//! does as everywhere). Mouse: click the preview or the Shutter button.
//!
//! A background thread owns the device: it reads frames, decodes (MJPEG through SDL2_image,
//! YUYV by hand), makes the dithered preview and keeps the newest full frame for the photo.
//! Older frames are dropped, never queued. The thread stops the stream when the app closes
//! and when nothing has been drawn for a while (the screen turned off).
//!
//! With `HARBOUR_MOCK=1` (or off Linux) an animated test pattern replaces the camera.

mod backend;
mod convert;
#[cfg(unix)]
mod csi;
mod png;
mod view;
#[cfg(target_os = "linux")]
mod v4l2;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sdl2::image::{InitFlag, Sdl2ImageContext};
use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::{Point, Rect};
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome;
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::apps::tone;
use crate::launcher::alert::{Alert, AlertIcon};
use crate::launcher::widgets::{self, contains, BtnState, MenuItem, PointerEvent};
use crate::platform::keys::arrow_alias;
use crate::system::config::{self, Config};

use backend::{Backend, Ctrl, CtrlRange, Device, Probe};
use convert::Still;

/// The preview box (frame included) and the right-hand column. Both run from y 18 down to the
/// screen's bottom line (y 168): a 4:3 picture fills the box's width (198 x 148).
fn preview() -> Rect {
    Rect::new(4, 18, PREVIEW_INNER.0 + 2, PREVIEW_INNER.1 + 2)
}
const PREVIEW_INNER: (u32, u32) = (198, (grid::APP_FRAME_BOTTOM + 1 - 18 - 2) as u32);
const COL_X: i32 = 216;
const COL_W: u32 = 96;
/// The side panel behind the column (status, counter, meters, shutter).
fn side_panel() -> Rect {
    Rect::new(COL_X - 4, 18, COL_W + 8, (grid::APP_FRAME_BOTTOM + 1 - 18) as u32)
}
fn shutter_rect() -> Rect {
    Rect::new(COL_X, 140, COL_W, 24)
}
/// Stop streaming when nothing was drawn for this long (screen off).
const IDLE_STOP: Duration = Duration::from_millis(2500);
const FREEZE: Duration = Duration::from_millis(900);
const RESCAN: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------- photos

pub fn pictures_dir() -> PathBuf {
    crate::launcher::fileops::user_dir(crate::launcher::fileops::UserDir::Pictures)
}

/// How many pictures the Images app would list in `dir` (hidden files and other types skipped).
pub fn count_photos(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            !n.starts_with('.') && ["png", "jpg", "jpeg", "bmp", "gif"].iter().any(|x| n.ends_with(&format!(".{x}")))
        })
        .count()
}

/// `IMG_YYYYMMDD_HHMMSS.png`
pub fn photo_name(now: chrono::DateTime<chrono::Local>) -> String {
    format!("IMG_{}.png", now.format("%Y%m%d_%H%M%S"))
}

/// A file name in `dir` that does not exist yet (`IMG_..._2.png` when two shots share a second).
pub fn free_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let stem = name.trim_end_matches(".png");
    (2..1000).map(|n| format!("{stem}_{n}.png")).find(|n| !dir.join(n).exists()).unwrap_or_else(|| name.to_string())
}

/// Write the still as PNG into `dir` (created when missing); returns the file name.
pub fn save_still(still: &Still, dir: &Path, now: chrono::DateTime<chrono::Local>) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = free_name(dir, &photo_name(now));
    let (w, h, rgb) = still.to_rgb();
    let bytes = png::encode_rgb(w, h, &rgb);
    let tmp = dir.join(format!(".{name}.part"));
    std::fs::write(&tmp, bytes).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, dir.join(&name)).map_err(|e| format!("write: {e}"))?;
    Ok(name)
}

// ---------------------------------------------------------------- preferences

/// How the preview is reduced: 1-bit error diffusion, or four grey tones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Dither,
    Grey,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Dither => "Dither",
            Mode::Grey => "Grey",
        }
    }
    fn toggled(self) -> Mode {
        match self {
            Mode::Dither => Mode::Grey,
            Mode::Grey => Mode::Dither,
        }
    }
}

/// Remembered across runs in the shared config (`camera_*` keys; other keys are kept).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Prefs {
    pub mode: Mode,
    /// Brightness / contrast as a percentage of the camera's range; `None` = leave the camera's own value.
    pub bright: Option<u8>,
    pub contrast: Option<u8>,
    /// Device path of the last camera used.
    pub device: String,
}

impl Prefs {
    pub fn load(cfg: &Config) -> Prefs {
        let pct = |k: &str| {
            let v = cfg.get_int(k, -1);
            (0..=100).contains(&v).then_some(v as u8)
        };
        Prefs {
            mode: if cfg.get_str("camera_mode", "dither") == "grey" { Mode::Grey } else { Mode::Dither },
            bright: pct("camera_bright"),
            contrast: pct("camera_contrast"),
            device: cfg.get_str("camera_device", ""),
        }
    }

    pub fn store(&self, cfg: &mut Config) {
        cfg.set_str("camera_mode", if self.mode == Mode::Grey { "grey" } else { "dither" });
        cfg.set_int("camera_bright", self.bright.map_or(-1, i64::from));
        cfg.set_int("camera_contrast", self.contrast.map_or(-1, i64::from));
        cfg.set_str("camera_device", &self.device);
    }
}

// ---------------------------------------------------------------- capture thread

#[derive(Clone, Default)]
struct Info {
    fmt: String,
    w: u32,
    h: u32,
    bright: Option<CtrlRange>,
    contrast: Option<CtrlRange>,
}

#[derive(Default)]
struct Out {
    seq: u64,
    bits: Vec<u8>,
    pw: u32,
    ph: u32,
    still: Option<Still>,
    info: Option<Info>,
    fps: f32,
    error: Option<String>,
    finished: bool,
}

struct Shared {
    stop: AtomicBool,
    base: Instant,
    last_draw_ms: AtomicU64,
    /// 0 = Dither, 1 = Grey (read for every frame).
    grey: AtomicBool,
    out: Mutex<Out>,
}

impl Shared {
    fn touch(&self) {
        self.last_draw_ms.store(self.base.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
    fn idle(&self) -> bool {
        self.base.elapsed().as_millis() as u64 > self.last_draw_ms.load(Ordering::Relaxed) + IDLE_STOP.as_millis() as u64
    }
}

enum Cmd {
    Nudge(Ctrl, i32),
}

struct Session {
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join(); // the stream is turned off before this returns
        }
    }
}

/// Dither: 1 = paper, 0 = ink. Grey: tones 0 (dark) to 3 (paper).
fn preview_bits(still: &Still, mode: Mode) -> (u32, u32, Vec<u8>) {
    let (w, h, mut gray) = convert::gray_fit(still, PREVIEW_INNER.0, PREVIEW_INNER.1);
    tone::stretch(&mut gray);
    let bits = match mode {
        Mode::Dither => tone::quantize_1bit(&gray, w as usize, h as usize, tone::ART_DITHER),
        Mode::Grey => tone::quantize(&gray, w as usize, h as usize, tone::DEFAULT),
    };
    (w, h, bits)
}

fn run(backend: Arc<dyn Backend>, dev: Device, size: Option<(u32, u32)>, jpg: bool, init: [Option<u8>; 2], shared: Arc<Shared>, rx: Receiver<Cmd>) {
    let finish = |err: Option<String>| {
        let mut o = shared.out.lock().unwrap();
        if err.is_some() {
            o.error = err;
        }
        o.finished = true;
    };
    let mut src = match backend.open(&dev, size, jpg) {
        Ok(s) => s,
        Err(e) => return finish(Some(e)),
    };
    let (fmt, w, h) = src.describe();
    // Remembered brightness / contrast go to the camera before the first frame.
    for (c, pct) in [(Ctrl::Brightness, init[0]), (Ctrl::Contrast, init[1])] {
        if let (Some(p), Some(r)) = (pct, src.ctrl(c)) {
            src.set_ctrl(c, r.at_pct(p));
        }
    }
    let mut info = Info { fmt, w, h, bright: src.ctrl(Ctrl::Brightness), contrast: src.ctrl(Ctrl::Contrast) };
    shared.out.lock().unwrap().info = Some(info.clone());
    let (mut frames, mut window, mut silent) = (0u32, Instant::now(), 0u32);
    loop {
        if shared.stop.load(Ordering::Relaxed) || shared.idle() {
            break;
        }
        let mut changed = false;
        while let Ok(Cmd::Nudge(c, dir)) = rx.try_recv() {
            let range = if c == Ctrl::Brightness { info.bright } else { info.contrast };
            if let Some(r) = range {
                src.set_ctrl(c, r.nudged(dir));
                changed = true;
            }
        }
        if changed {
            info.bright = src.ctrl(Ctrl::Brightness);
            info.contrast = src.ctrl(Ctrl::Contrast);
            shared.out.lock().unwrap().info = Some(info.clone());
        }
        match src.next(Duration::from_millis(100)) {
            Ok(Some(raw)) => {
                silent = 0;
                match convert::to_still(raw) {
                    Ok(still) => {
                        let mode = if shared.grey.load(Ordering::Relaxed) { Mode::Grey } else { Mode::Dither };
                        let (pw, ph, bits) = preview_bits(&still, mode);
                        frames += 1;
                        let mut o = shared.out.lock().unwrap();
                        o.seq += 1;
                        o.pw = pw;
                        o.ph = ph;
                        o.bits = bits;
                        o.still = Some(still);
                        if window.elapsed() >= Duration::from_secs(1) {
                            o.fps = frames as f32 / window.elapsed().as_secs_f32();
                            frames = 0;
                            window = Instant::now();
                        }
                    }
                    Err(_) => {} // a corrupt frame now and then is normal on USB: skip it
                }
            }
            Ok(None) => {
                silent += 1;
                if silent > 40 {
                    return finish(Some("No frames from the camera (4 s).".into()));
                }
            }
            Err(e) => return finish(Some(e)),
        }
    }
    drop(src);
    finish(None);
}

// ---------------------------------------------------------------- the app

pub struct Camera {
    backend: Arc<dyn Backend>,
    jpg_ok: bool,
    _img: Option<Sdl2ImageContext>,
    probe: Probe,
    probed_at: Instant,
    dev: usize,
    size: Option<(u32, u32)>,
    sess: Option<Session>,
    error: Option<String>,
    seen_seq: u64,
    bits: Vec<u8>,
    pw: u32,
    ph: u32,
    info: Option<Info>,
    fps: f32,
    freeze_until: Option<Instant>,
    flash_until: Option<Instant>,
    /// The "Scan Again" button was chosen: it looks pressed and the alert says "Scanning..." until this time.
    scan_flash: Option<Instant>,
    /// When the devices were last probed (shown in the alert so a rescan is visible).
    last_scan: String,
    last_saved: Option<String>,
    pictures: PathBuf,
    hover: bool,
    points: Vec<Point>,
    prefs: Prefs,
    /// Where `prefs` are stored (`None` in tests).
    prefs_path: Option<PathBuf>,
    prefs_dirty: bool,
    prefs_since: Option<Instant>,
    /// The camera in use vanished: wait for it and restart by itself when it is back.
    unplugged: bool,
    /// Pictures in the Pictures folder (the counter on the panel).
    photos: usize,
    lost_path: Option<String>,
}

pub fn make() -> Box<dyn LauncherApp> {
    let mock = cfg!(not(target_os = "linux")) || matches!(std::env::var("HARBOUR_MOCK").as_deref(), Ok("1" | "true" | "yes" | "on"));
    let backend: Arc<dyn Backend> = if mock { Arc::new(backend::Mock::from_env()) } else { real_backend() };
    let mut cam = Camera::new(backend, pictures_dir());
    cam.use_config(config::default_path());
    Box::new(cam)
}

#[cfg(target_os = "linux")]
fn real_backend() -> Arc<dyn Backend> {
    Arc::new(Combined { v4l2: v4l2::V4l2, csi: csi::Csi::system() })
}

/// UVC webcams through V4L2 plus the built-in CSI camera through the Pi tools, in one list.
#[cfg(target_os = "linux")]
struct Combined {
    v4l2: v4l2::V4l2,
    csi: csi::Csi,
}

#[cfg(target_os = "linux")]
impl Backend for Combined {
    fn probe(&self) -> Probe {
        let mut p = self.v4l2.probe();
        let (dev, note) = self.csi.probe();
        p.devices.extend(dev); // after the USB cameras
        p.notes.extend(note);
        p
    }
    fn open(&self, dev: &Device, size: Option<(u32, u32)>, jpeg: bool) -> Result<Box<dyn backend::Source>, String> {
        if dev.path == csi::CSI_PATH {
            self.csi.open(size)
        } else {
            self.v4l2.open(dev, size, jpeg)
        }
    }
    fn has_photo_tool(&self, dev: &Device) -> bool {
        dev.path == csi::CSI_PATH
    }
    fn photo(&self, dev: &Device, out: &Path) -> Result<(), String> {
        if self.has_photo_tool(dev) {
            self.csi.take_photo(out)
        } else {
            Err("no photo tool".into())
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn real_backend() -> Arc<dyn Backend> {
    Arc::new(backend::Mock::from_env())
}

impl Camera {
    pub fn new(backend: Arc<dyn Backend>, pictures: PathBuf) -> Self {
        let img = sdl2::image::init(InitFlag::JPG).ok();
        let probe = backend.probe();
        let photos = count_photos(&pictures);
        Camera {
            backend,
            jpg_ok: img.is_some(),
            _img: img,
            probe,
            probed_at: Instant::now(),
            dev: 0,
            size: None,
            sess: None,
            error: None,
            seen_seq: 0,
            bits: Vec::new(),
            pw: 0,
            ph: 0,
            info: None,
            fps: 0.0,
            freeze_until: None,
            flash_until: None,
            scan_flash: None,
            last_scan: String::new(),
            last_saved: None,
            pictures,
            hover: false,
            points: Vec::new(),
            prefs: Prefs::default(),
            prefs_path: None,
            prefs_dirty: false,
            prefs_since: None,
            unplugged: false,
            photos,
            lost_path: None,
        }
    }

    /// Load the remembered settings from the config file and keep it updated from now on.
    pub fn use_config(&mut self, path: PathBuf) {
        self.prefs = Prefs::load(&Config::load(path.clone()));
        self.prefs_path = Some(path);
        if let Some(i) = self.probe.devices.iter().position(|d| d.path == self.prefs.device) {
            self.dev = i;
        }
    }

    fn save_prefs(&mut self) {
        self.prefs_dirty = false;
        self.prefs_since = None;
        let Some(p) = self.prefs_path.clone() else { return };
        let mut cfg = Config::load(p);
        self.prefs.store(&mut cfg);
        let _ = cfg.save(); // a read-only or foreign config is left alone
    }

    fn set_mode(&mut self, mode: Mode) -> AppAction {
        self.prefs.mode = mode;
        self.prefs_dirty = true;
        if let Some(s) = &self.sess {
            s.shared.grey.store(mode == Mode::Grey, Ordering::Relaxed);
        }
        AppAction::Toast(format!("Mode: {}", mode.name()))
    }

    fn device(&self) -> Option<&Device> {
        self.probe.devices.get(self.dev)
    }

    fn stop(&mut self) {
        self.sess = None; // joins the thread, which closes the device
        self.bits.clear();
        self.seen_seq = 0;
        self.info = None;
        self.fps = 0.0;
    }

    fn start(&mut self) {
        let Some(dev) = self.device().cloned() else { return };
        let (tx, rx) = channel();
        let shared = Arc::new(Shared { stop: AtomicBool::new(false), base: Instant::now(), last_draw_ms: AtomicU64::new(0), grey: AtomicBool::new(self.prefs.mode == Mode::Grey), out: Mutex::new(Out::default()) });
        shared.touch();
        let (b, sh, size, jpg) = (self.backend.clone(), shared.clone(), self.size, self.jpg_ok);
        let init = [self.prefs.bright, self.prefs.contrast];
        let handle = std::thread::Builder::new().name("camera".into()).spawn(move || run(b, dev, size, jpg, init, sh, rx)).ok();
        if handle.is_none() {
            self.error = Some("Cannot start the capture thread.".into());
            return;
        }
        self.sess = Some(Session { shared, tx, handle });
    }

    fn restart(&mut self) {
        self.stop();
        self.error = None;
        self.unplugged = false;
    }

    fn rescan(&mut self) {
        let was: Vec<String> = self.probe.devices.iter().map(|d| d.path.clone()).collect();
        let cur = self.device().map(|d| d.path.clone());
        self.probe = self.backend.probe();
        self.probed_at = Instant::now();
        self.last_scan = chrono::Local::now().format("%H:%M:%S").to_string();
        // Keep the same camera selected (by path) when the list changes around it.
        let want = cur.unwrap_or_else(|| self.prefs.device.clone());
        self.dev = self.probe.devices.iter().position(|d| d.path == want).unwrap_or_else(|| self.dev.min(self.probe.devices.len().saturating_sub(1)));
        if was != self.probe.devices.iter().map(|d| d.path.clone()).collect::<Vec<_>>() {
            self.restart();
        }
    }

    fn switch_device(&mut self, dir: isize) -> AppAction {
        let n = self.probe.devices.len() as isize;
        if n < 2 {
            return AppAction::Toast("Only one camera".into());
        }
        self.dev = (self.dev as isize + dir).rem_euclid(n) as usize;
        self.prefs.device = self.probe.devices[self.dev].path.clone();
        self.prefs_dirty = true;
        self.size = None;
        self.restart();
        AppAction::Toast(format!("Camera: {}", self.probe.devices[self.dev].name))
    }

    fn cycle_size(&mut self) -> AppAction {
        let Some(dev) = self.device() else { return AppAction::None };
        if dev.sizes.len() < 2 {
            return AppAction::Toast("One resolution only".into());
        }
        let cur = self.info.as_ref().map(|i| (i.w, i.h)).or(self.size).or_else(|| backend::default_size(&dev.sizes));
        let i = cur.and_then(|c| dev.sizes.iter().position(|&s| s == c)).unwrap_or(0);
        let next = dev.sizes[(i + 1) % dev.sizes.len()];
        self.size = Some(next);
        self.restart();
        AppAction::Toast(format!("Resolution {}x{}", next.0, next.1))
    }

    /// A camera that takes its photos with its own tool (CSI): stop the preview (the sensor has
    /// one owner), shoot, and let the next draw restart the preview.
    fn external_photo(&mut self, now: chrono::DateTime<chrono::Local>) -> Option<AppAction> {
        let dev = self.device()?.clone();
        if !self.backend.has_photo_tool(&dev) {
            return None;
        }
        let name = free_name(&self.pictures, &photo_name(now));
        let out = self.pictures.join(format!(".{name}.part.png"));
        let _ = std::fs::create_dir_all(&self.pictures);
        let had_session = self.sess.is_some();
        if had_session {
            self.stop();
        }
        match self.backend.photo(&dev, &out) {
            Ok(()) => match std::fs::rename(&out, self.pictures.join(&name)) {
                Ok(()) => {
                    self.freeze_until = Some(Instant::now() + FREEZE);
                    self.flash_until = Some(Instant::now() + Duration::from_millis(120));
                    self.last_saved = Some(name.clone());
                self.photos = count_photos(&self.pictures);
                    Some(AppAction::Toast(format!("Saved {name}")))
                }
                Err(e) => Some(AppAction::Toast(format!("Not saved: {e}"))),
            },
            Err(e) => {
                let _ = std::fs::remove_file(&out);
                Some(AppAction::Toast(format!("Not saved: {e}")))
            }
        }
    }

    fn shutter(&mut self, now: chrono::DateTime<chrono::Local>) -> AppAction {
        if let Some(a) = self.external_photo(now) {
            return a;
        }
        let Some(s) = &self.sess else { return AppAction::Toast("No camera".into()) };
        let o = s.shared.out.lock().unwrap();
        let Some(still) = &o.still else { return AppAction::Toast("No picture yet".into()) };
        let res = save_still(still, &self.pictures, now);
        drop(o);
        match res {
            Ok(name) => {
                self.freeze_until = Some(Instant::now() + FREEZE);
                self.flash_until = Some(Instant::now() + Duration::from_millis(120));
                self.last_saved = Some(name.clone());
                self.photos = count_photos(&self.pictures);
                AppAction::Toast(format!("Saved {name}"))
            }
            Err(e) => AppAction::Toast(format!("Not saved: {e}")),
        }
    }

    fn nudge(&mut self, c: Ctrl, dir: i32) -> AppAction {
        let has = self.info.as_ref().map(|i| if c == Ctrl::Brightness { i.bright.is_some() } else { i.contrast.is_some() }).unwrap_or(false);
        if !has {
            return AppAction::Toast(format!("No {} control", if c == Ctrl::Brightness { "brightness" } else { "contrast" }));
        }
        if let Some(s) = &self.sess {
            let _ = s.tx.send(Cmd::Nudge(c, dir));
        }
        if let Some(r) = self.info.as_ref().and_then(|i| if c == Ctrl::Brightness { i.bright } else { i.contrast }) {
            let pct = CtrlRange { value: r.nudged(dir), ..r }.pct();
            if c == Ctrl::Brightness {
                self.prefs.bright = Some(pct);
            } else {
                self.prefs.contrast = Some(pct);
            }
            self.prefs_dirty = true;
        }
        AppAction::None
    }

    fn frozen(&self) -> bool {
        self.freeze_until.map(|t| Instant::now() < t).unwrap_or(false)
    }
}

impl LauncherApp for Camera {
    fn takes_text(&self) -> bool {
        false
    }

    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Return / Space", "Capture"),
            k("M", "Dither / grey"),
            k("Left / Right", "Switch camera"),
            k("Up / Down", "Brightness"),
            k("Shift + Up / Down", "Contrast"),
        ]
    }

    fn title(&self) -> String {
        crate::launcher::i18n::app_name("camera")
    }

    fn menu(&self) -> Vec<MenuItem> {
        let dev = self.device().map(|d| chrome_short(&d.name)).unwrap_or_else(|| "none".into());
        let res = self.info.as_ref().map(|i| format!("{}x{}", i.w, i.h)).unwrap_or_else(|| "-".into());
        let mut v = vec![
            MenuItem::new("save", "Capture"),
            MenuItem::new("dev", format!("Device: {dev}")),
            MenuItem::new("mode", format!("Mode: {}", self.prefs.mode.name())),
            MenuItem::new("res", format!("Resolution: {res}")),
            MenuItem::new("reset", "Reset Controls"),
        ];
        if self.last_saved.is_some() {
            v.push(MenuItem::new("open", "Open Last in Images"));
        }
        v
    }

    fn menu_action(&mut self, id: &str, ctx: &Ctx) -> AppAction {
        match id {
            "dev" => self.switch_device(1),
            "res" => self.cycle_size(),
            "save" => self.shutter(ctx.now),
            "mode" => self.set_mode(self.prefs.mode.toggled()),
            "reset" => {
                self.prefs.bright = None;
                self.prefs.contrast = None;
                self.prefs_dirty = true;
                self.restart(); // the camera comes back with its own defaults
                AppAction::Toast("Controls reset".into())
            }
            "open" => match &self.last_saved {
                Some(n) => AppAction::Open { app: Some("Images".into()), path: self.pictures.join(n) },
                None => AppAction::None,
            },
            _ => AppAction::None,
        }
    }

    fn key(&mut self, key: Keycode, mods: Mod, repeat: bool, ctx: &Ctx) -> AppAction {
        let shift = mods.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
        match arrow_alias(key) {
            Keycode::Escape => AppAction::Quit,
            Keycode::Return | Keycode::KpEnter | Keycode::Space if !repeat => {
                if self.error.is_some() || self.probe.devices.is_empty() {
                    self.scan_again();
                    return AppAction::None;
                }
                self.shutter(ctx.now)
            }
            Keycode::M if !repeat => self.set_mode(self.prefs.mode.toggled()),
            Keycode::Left => self.switch_device(-1),
            Keycode::Right => self.switch_device(1),
            Keycode::Up => self.nudge(if shift { Ctrl::Contrast } else { Ctrl::Brightness }, 1),
            Keycode::Down => self.nudge(if shift { Ctrl::Contrast } else { Ctrl::Brightness }, -1),
            _ => AppAction::None,
        }
    }

    fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> AppAction {
        match ev {
            PointerEvent::Move { x, y } | PointerEvent::Drag { x, y, .. } => {
                self.hover = contains(shutter_rect(), x, y);
                AppAction::None
            }
            PointerEvent::Down { x, y, button: 0, .. } if self.error.is_some() || self.probe.devices.is_empty() => {
                if self.alert().click(ctx.fs, x, y).is_some() {
                    self.scan_again();
                }
                AppAction::None
            }
            PointerEvent::Down { x, y, button: 0, .. } if contains(shutter_rect(), x, y) || contains(preview(), x, y) => self.shutter(ctx.now),
            PointerEvent::Wheel { dy, .. } => self.nudge(Ctrl::Brightness, if dy > 0 { 1 } else { -1 }),
            _ => AppAction::None,
        }
    }

    fn tick(&mut self, _ctx: &Ctx) -> bool {
        let mut dirty = false;
        if let Some(t) = self.flash_until {
            dirty = true;
            if Instant::now() >= t {
                self.flash_until = None;
            }
        }
        if let Some(t) = self.scan_flash {
            dirty = true;
            if Instant::now() >= t {
                self.scan_flash = None;
            }
        }
        if self.freeze_until.is_some() {
            dirty = true;
            if !self.frozen() {
                self.freeze_until = None;
            }
        }
        dirty |= self.poll_rescan();
        if self.prefs_dirty {
            // Written a second after the last change, not on every key repeat.
            let since = *self.prefs_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_secs(1) {
                self.save_prefs();
            }
        }
        dirty |= self.poll_session();
        dirty
    }

    fn draw(&mut self, c: &mut Canvas<Window>, assets: &mut AssetStore, ctx: &Ctx) {
        if let Some(s) = &self.sess {
            s.shared.touch();
        } else if self.error.is_none() && !self.probe.devices.is_empty() {
            self.start();
        }
        self.paint(c, assets, ctx);
    }
}

impl Camera {
    /// Look for a camera again when there is none (or the one in use was unplugged).
    fn poll_rescan(&mut self) -> bool {
        let mut dirty = false;
        if self.unplugged && self.sess.is_none() && self.probed_at.elapsed() >= RESCAN {
            let path = self.lost_path.clone().unwrap_or_default();
            self.probe = self.backend.probe();
            self.probed_at = Instant::now();
            if let Some(i) = self.probe.devices.iter().position(|d| d.path == path) {
                self.dev = i;
                self.unplugged = false;
                self.error = None; // the next draw starts the stream again
            }
            dirty = true;
        } else if self.sess.is_none() && self.probe.devices.is_empty() && self.probed_at.elapsed() >= RESCAN {
            let before = self.probe.clone();
            self.rescan();
            dirty |= before != self.probe;
        }
        dirty
    }

    /// Take the newest frame / state from the capture thread; handle its end.
    fn poll_session(&mut self) -> bool {
        let mut dirty = false;
        let mut stopped = false;
        if let Some(s) = &self.sess {
            let mut o = s.shared.out.lock().unwrap();
            if o.info.is_some() && self.info.as_ref().map(|i| (i.w, i.h, i.fmt.clone(), i.bright.map(|r| r.value), i.contrast.map(|r| r.value))) != o.info.as_ref().map(|i| (i.w, i.h, i.fmt.clone(), i.bright.map(|r| r.value), i.contrast.map(|r| r.value))) {
                self.info = o.info.clone();
                dirty = true;
            }
            if o.seq != self.seen_seq && !self.frozen() {
                self.seen_seq = o.seq;
                self.pw = o.pw;
                self.ph = o.ph;
                std::mem::swap(&mut self.bits, &mut o.bits);
                self.fps = o.fps;
                dirty = true;
            }
            if o.finished {
                if let Some(e) = o.error.take() {
                    self.error = Some(e);
                }
                stopped = true;
                dirty = true;
            }
        }
        if stopped {
            let had_error = self.error.is_some();
            if had_error {
                // Is the camera still on the bus? A UVC camera that browns out (USB power)
                // disappears from the list: say so, and come back by ourselves.
                let path = self.device().map(|d| d.path.clone());
                self.probe = self.backend.probe();
                self.probed_at = Instant::now();
                if let Some(p) = path {
                    if !self.probe.devices.iter().any(|d| d.path == p) {
                        self.unplugged = true;
                        self.lost_path = Some(p.clone());
                        self.error = Some("Camera unplugged.\nReconnect it; this resumes by itself.".into());
                        self.dev = self.dev.min(self.probe.devices.len().saturating_sub(1));
                    }
                }
            }
            self.sess = None; // the next draw restarts it after an idle stop
            if had_error {
                self.info = None;
                self.bits.clear();
            }
        }
        dirty
    }

    /// "Scan Again": show the pressed button and "Scanning..." for a moment, then rescan.
    fn scan_again(&mut self) {
        self.scan_flash = Some(Instant::now() + Duration::from_millis(700));
        self.restart();
        self.rescan();
    }

}

fn chrome_short(name: &str) -> String {
    let n: String = name.chars().take(14).collect();
    n
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.sess = None;
        if self.prefs_dirty {
            self.save_prefs();
        }
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::backend::*;
    use super::*;
    use chrono::TimeZone;

    struct Fake(Vec<Device>);
    impl Backend for Fake {
        fn probe(&self) -> Probe {
            Probe { devices: self.0.clone(), notes: vec![] }
        }
        fn open(&self, _d: &Device, _s: Option<(u32, u32)>, _j: bool) -> Result<Box<dyn Source>, String> {
            Err("busy".into())
        }
    }

    fn dev(path: &str) -> Device {
        Device { path: path.into(), name: format!("Cam {path}"), uvc: true, formats: vec![MJPG], sizes: vec![(320, 240), (640, 480)] }
    }

    #[test]
    fn photo_names_follow_the_pattern() {
        let t = chrono::Local.with_ymd_and_hms(2026, 11, 18, 9, 5, 7).unwrap();
        assert_eq!(photo_name(t), "IMG_20261118_090507.png");
    }

    #[test]
    fn same_second_shots_do_not_overwrite() {
        let dir = std::env::temp_dir().join(format!("harbour_cam_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t = chrono::Local.with_ymd_and_hms(2026, 11, 18, 9, 5, 7).unwrap();
        let still = Still::Rgb { w: 2, h: 2, data: vec![200; 12] };
        let a = save_still(&still, &dir, t).unwrap();
        let b = save_still(&still, &dir, t).unwrap();
        assert_eq!(a, "IMG_20261118_090507.png");
        assert_eq!(b, "IMG_20261118_090507_2.png");
        assert!(dir.join(&a).exists() && dir.join(&b).exists());
        assert!(std::fs::read_dir(&dir).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".part")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn yuyv_stills_are_saved_at_full_size() {
        let dir = std::env::temp_dir().join(format!("harbour_cam_y_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let still = Still::Yuyv { w: 8, h: 2, data: vec![128; 8 * 2 * 2] };
        let name = save_still(&still, &dir, chrono::Local::now()).unwrap();
        let bytes = std::fs::read(dir.join(name)).unwrap();
        assert_eq!(&bytes[1..4], b"PNG");
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_failure_is_shown_not_retried_in_a_loop() {
        let mut cam = Camera::new(Arc::new(Fake(vec![dev("/dev/video0")])), std::env::temp_dir());
        cam.start();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            let o = cam.sess.as_ref().unwrap().shared.out.lock().unwrap();
            if o.finished {
                assert_eq!(o.error.as_deref(), Some("busy"));
                return;
            }
        }
        panic!("thread did not finish");
    }

    #[test]
    fn mock_session_produces_preview_and_a_photo() {
        let dir = std::env::temp_dir().join(format!("harbour_cam_m_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cam = Camera::new(Arc::new(Mock::Pattern), dir.clone());
        cam.start();
        let mut ok = false;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(30));
            if cam.sess.as_ref().unwrap().shared.out.lock().unwrap().still.is_some() {
                ok = true;
                break;
            }
        }
        assert!(ok);
        {
            let o = cam.sess.as_ref().unwrap().shared.out.lock().unwrap();
            assert!(o.pw <= PREVIEW_INNER.0 && o.ph <= PREVIEW_INNER.1 && o.bits.len() == (o.pw * o.ph) as usize);
        }
        let act = cam.shutter(chrono::Local::now());
        assert!(matches!(act, AppAction::Toast(ref t) if t.starts_with("Saved IMG_")), "{act:?}");
        assert!(cam.frozen());
        cam.stop();
        assert!(cam.sess.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_stream_stops_when_nothing_is_drawn() {
        let cam_shared = Arc::new(Shared { stop: AtomicBool::new(false), base: Instant::now() - Duration::from_secs(10), last_draw_ms: AtomicU64::new(0), grey: AtomicBool::new(false), out: Mutex::new(Out::default()) });
        assert!(cam_shared.idle());
        cam_shared.touch();
        assert!(!cam_shared.idle());
    }

    #[test]
    fn switching_devices_wraps_and_needs_two() {
        let mut cam = Camera::new(Arc::new(Fake(vec![dev("/dev/video0")])), std::env::temp_dir());
        assert!(matches!(cam.switch_device(1), AppAction::Toast(_)));
        let mut cam = Camera::new(Arc::new(Fake(vec![dev("/dev/video0"), dev("/dev/video2")])), std::env::temp_dir());
        cam.switch_device(-1);
        assert_eq!(cam.dev, 1);
        cam.switch_device(1);
        assert_eq!(cam.dev, 0);
    }

    #[test]
    fn menu_names_device_and_resolution() {
        let cam = Camera::new(Arc::new(Fake(vec![dev("/dev/video0")])), std::env::temp_dir());
        let labels: Vec<String> = cam.menu().iter().map(|m| m.label.clone()).collect();
        assert_eq!(labels[0], "Capture");
        assert!(labels[1].starts_with("Device: Cam"), "{labels:?}");
        assert_eq!(labels[2], "Mode: Dither");
        assert!(labels[3].starts_with("Resolution:"));
        assert!(!labels.iter().any(|l| l.starts_with("Open Last")), "nothing captured yet");
    }

    #[test]
    fn prefs_round_trip_and_keep_other_keys() {
        let dir = std::env::temp_dir().join(format!("harbour_cam_p_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"volume": 7}"#).unwrap();
        let p = Prefs { mode: Mode::Grey, bright: Some(30), contrast: None, device: "/dev/video2".into() };
        let mut cfg = Config::load(path.clone());
        p.store(&mut cfg);
        cfg.save().unwrap();
        let cfg = Config::load(path);
        assert_eq!(Prefs::load(&cfg), p);
        assert_eq!(cfg.get_int("volume", 0), 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn controls_map_percent_onto_the_step_grid() {
        let r = CtrlRange { min: 0, max: 255, step: 5, value: 100 };
        assert_eq!(r.at_pct(0), 0);
        assert_eq!(r.at_pct(100), 255);
        assert_eq!(r.at_pct(50) % 5, 0);
        assert!(CtrlRange { value: r.at_pct(40), ..r }.pct().abs_diff(40) <= 2);
    }

    #[test]
    fn mode_toggle_changes_tones_and_is_remembered() {
        let dir = std::env::temp_dir().join(format!("harbour_cam_c_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfgp = dir.join("config.json");
        let mut cam = Camera::new(Arc::new(Fake(vec![dev("/dev/video0")])), dir.clone());
        cam.use_config(cfgp.clone());
        assert!(matches!(cam.set_mode(Mode::Grey), AppAction::Toast(ref t) if t == "Mode: Grey"));
        cam.save_prefs();
        let mut again = Camera::new(Arc::new(Fake(vec![dev("/dev/video0")])), dir.clone());
        again.use_config(cfgp);
        assert_eq!(again.prefs.mode, Mode::Grey);
        let still = Still::Rgb { w: 8, h: 8, data: (0..8 * 8 * 3).map(|i| (i * 4 % 256) as u8).collect() };
        let (_, _, one) = preview_bits(&still, Mode::Dither);
        let (_, _, four) = preview_bits(&still, Mode::Grey);
        assert!(one.iter().all(|&b| b <= 1));
        assert!(four.iter().any(|&b| b > 1), "grey mode uses more than two tones");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn photo_count_matches_what_images_lists() {
        let dir = std::env::temp_dir().join(format!("harbour_cam_n_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["a.png", "B.JPG", "c.txt", ".hidden.png", ".IMG_x.png.part"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        assert_eq!(count_photos(&dir), 2);
        assert_eq!(count_photos(&dir.join("missing")), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_layout_keeps_to_the_grid_and_the_panel_clear_of_the_picture() {
        let (p, b) = (preview(), shutter_rect());
        assert!(p.y() >= grid::BAR_H + 2 && p.bottom() == grid::APP_FRAME_BOTTOM + 1, "down to the bottom line");
        assert!(p.right() + 4 <= COL_X - 4, "gap between picture frame and side panel");
        let s = side_panel();
        assert_eq!(s.bottom(), p.bottom());
        assert!(b.bottom() <= s.bottom() - 4 && b.right() <= 316 - 4);
        assert_eq!((PREVIEW_INNER.0 as i32 + 2, PREVIEW_INNER.1 as i32 + 2), (p.width() as i32, p.height() as i32));
    }

    #[test]
    fn open_last_needs_a_capture_and_names_images() {
        let mut cam = Camera::new(Arc::new(Fake(vec![dev("/dev/video0")])), PathBuf::from("/p"));
        cam.last_saved = Some("IMG_1.png".into());
        assert!(cam.menu().iter().any(|m| m.id == "open"));
    }

    /// A backend whose camera list can change under the app.
    struct Flaky(Mutex<Vec<Device>>);
    impl Backend for Flaky {
        fn probe(&self) -> Probe {
            Probe { devices: self.0.lock().unwrap().clone(), notes: vec![] }
        }
        fn open(&self, _d: &Device, _s: Option<(u32, u32)>, _j: bool) -> Result<Box<dyn Source>, String> {
            Err("gone".into())
        }
    }

    #[test]
    fn an_unplugged_camera_is_noticed_and_picked_up_again() {
        let flaky = Arc::new(Flaky(Mutex::new(vec![dev("/dev/video0")])));
        let mut cam = Camera::new(flaky.clone(), std::env::temp_dir());
        cam.start();
        flaky.0.lock().unwrap().clear(); // unplug, the stream then fails
        let ctx_free = || ();
        let _ = ctx_free;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(20));
            cam.poll_session();
            if cam.sess.is_none() {
                break;
            }
        }
        assert!(cam.unplugged, "{:?}", cam.error);
        assert!(cam.error.as_deref().unwrap().starts_with("Camera unplugged"));
        // back on the bus
        *flaky.0.lock().unwrap() = vec![dev("/dev/video0")];
        cam.probed_at = Instant::now() - RESCAN * 2;
        cam.poll_rescan();
        assert!(!cam.unplugged && cam.error.is_none());
    }

    #[test]
    fn help_rows_are_filled_in() {
        crate::launcher::help::check_app_rows("Camera", &Camera::new(Arc::new(Fake(vec![])), std::env::temp_dir()).help());
    }
}
