//! Music: an iPod-style player for `~/Music` (no wheel needed).
//!
//! Two screens: the track list and Now Playing. Playback is a child process
//! (`ffplay -nodisp -autoexit`); pausing sends SIGSTOP / SIGCONT, a finished
//! track advances to the next one, and leaving the app stops the sound. The
//! process spawn is behind the [`Audio`] trait so the logic is testable.
//!
//! List: Up/Down select, Enter plays, Esc quits (and stops). Now Playing:
//! Space or Enter pause, Left/Right previous/next, Esc back to the list (the
//! music keeps playing). Volume stays on the host media keys.
//!
//! E (or the menu) opens the equalizer ([`music_eq`]): five bands and presets, kept in the
//! config file. ffplay cannot change its filters while it runs, so a change restarts it at the
//! current position once the knobs have been still for a moment.
//!
//! V (or a click on the cover) switches the Now Playing panel between the cover, band bars and
//! a waveform ([`music_vis`]); on a desk the panel grows with the window.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sdl2::image::{InitFlag, Sdl2ImageContext};
use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::{Point, Rect};
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::chrome;
use crate::dither::{self, Pattern};
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::apps::tone;
use crate::launcher::widgets::{self, contains, MenuItem, PointerEvent, SbPart, ScrollBar};
use crate::launcher::apps::music_eq::{self, Eq};
use crate::launcher::apps::music_vis::{self, Meter, Source};
use crate::platform::keys::arrow_alias;
use crate::system::config::{self, Config};
use crate::ui;

pub const EXTENSIONS: [&str; 5] = ["mp3", "flac", "ogg", "wav", "m4a"];
const MAX_TRACKS: usize = 5000;
const MAX_DEPTH: usize = 8;

// ------------------------------------------------------------------ audio

/// A running playback process.
pub trait Proc {
    /// True once the process has ended (finished or killed).
    fn exited(&mut self) -> bool;
    /// True once sound is really coming out (ffplay starts slowly); mocks are ready at once.
    fn ready(&mut self) -> bool {
        true
    }
    fn pause(&mut self);
    fn resume(&mut self);
    fn stop(&mut self);
}

/// How tracks are played and measured; replaced by a mock in tests.
pub trait Audio {
    /// Play `path` from `from`, through the ffmpeg filter chain `af` when there is one.
    fn spawn(&self, path: &Path, from: Duration, af: Option<&str>) -> Result<Box<dyn Proc>, String>;
    fn duration(&self, path: &Path) -> Option<Duration>;
}

/// Plays with `ffplay`. `min_start` pretends the start is slow (mock mode).
pub struct Ffplay {
    pub min_start: Duration,
}

/// If ffplay's status line is never recognised, call it ready after this long.
const READY_FALLBACK: Duration = Duration::from_millis(2500);

/// The playback position from an ffplay status line (`  1.23 M-A:  0.000 fd=...`), if it
/// is a real one: before the audio clock runs the line says `nan`.
pub fn status_time(line: &str) -> Option<f64> {
    let i = line.find("M-A:").or_else(|| line.find("M-V:"))?;
    let t: f64 = line[..i].trim().parse().ok()?;
    t.is_finite().then_some(t)
}

struct FfProc {
    child: Child,
    ready: Arc<AtomicBool>,
    since: Instant,
    min_start: Duration,
}

impl Proc for FfProc {
    fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }
    fn ready(&mut self) -> bool {
        let age = self.since.elapsed();
        (self.ready.load(Ordering::Relaxed) || age >= READY_FALLBACK) && age >= self.min_start
    }
    fn pause(&mut self) {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGSTOP) };
    }
    fn resume(&mut self) {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGCONT) };
    }
    fn stop(&mut self) {
        // A stopped process must be continued before it can act on SIGTERM/KILL.
        self.resume();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for FfProc {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.stop();
        }
    }
}

impl Audio for Ffplay {
    fn spawn(&self, path: &Path, from: Duration, af: Option<&str>) -> Result<Box<dyn Proc>, String> {
        // `-loglevel info` makes ffplay print its status line on stderr; the first line with a
        // real clock value means the audio is running.
        let mut cmd = Command::new("ffplay");
        cmd.args(["-nodisp", "-autoexit", "-loglevel", "info"]);
        if !from.is_zero() {
            cmd.arg("-ss").arg(format!("{:.3}", from.as_secs_f64()));
        }
        if let Some(af) = af {
            cmd.arg("-af").arg(af);
        }
        let mut child = cmd
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("ffplay: {e}"))?;
        let ready = Arc::new(AtomicBool::new(false));
        if let Some(mut err) = child.stderr.take() {
            let flag = ready.clone();
            std::thread::spawn(move || {
                use std::io::Read;
                let (mut buf, mut line) = ([0u8; 512], Vec::<u8>::new());
                // Keep reading to the end so ffplay never blocks on a full pipe.
                while let Ok(n) = err.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    for &b in &buf[..n] {
                        if b == b'\r' || b == b'\n' {
                            if !flag.load(Ordering::Relaxed) && status_time(&String::from_utf8_lossy(&line)).is_some() {
                                flag.store(true, Ordering::Relaxed);
                            }
                            line.clear();
                        } else if line.len() < 400 {
                            line.push(b);
                        }
                    }
                }
            });
        }
        Ok(Box::new(FfProc { child, ready, since: Instant::now(), min_start: self.min_start }))
    }

    fn duration(&self, path: &Path) -> Option<Duration> {
        probe_duration(path)
    }
}

/// Length of a file from `ffprobe` (slow on the device: run it off the UI thread).
pub fn probe_duration(path: &Path) -> Option<Duration> {
    let out = Command::new("ffprobe")
        .args(["-v", "quiet", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_duration(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_duration(s: &str) -> Option<Duration> {
    let v: f64 = s.trim().parse().ok()?;
    (v.is_finite() && v > 0.0).then(|| Duration::from_secs_f64(v))
}

// -------------------------------------------------------------- scanning

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    /// The folder below the music root, for the second line.
    pub folder: String,
}

pub fn is_audio(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false)
}

/// Recursively collect audio files, sorted by path (case-insensitive).
pub fn scan(root: &Path) -> Vec<Track> {
    let mut files = Vec::new();
    library::walk_files(root, &mut |p| {
        files.push(p);
        files.len() < MAX_TRACKS
    });
    files.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    files.into_iter().map(|p| library::track_for(root, p)).collect()
}

// ---------------------------------------------------- background library

/// Loading the library in the background: scan, tag reads, and an on-disk cache
/// (`~/.cache/harbour/music/`) keyed by path + mtime + size, so a second start is instant.
pub mod library {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::{channel, Receiver};
    use std::sync::Arc;
    use std::time::{Duration, Instant, UNIX_EPOCH};

    use super::tags::Tags;
    use super::{is_audio, Track, MAX_DEPTH, MAX_TRACKS};

    /// Counters shared between the loader thread and the screen.
    #[derive(Default)]
    pub struct Progress {
        /// Audio files found so far.
        pub found: AtomicUsize,
        /// Files whose tags are known (read or cached).
        pub tagged: AtomicUsize,
        /// Of those, how many came from the disk cache.
        pub hits: AtomicUsize,
        /// The folder walk has finished, so `found` is the final total.
        pub scanned: AtomicBool,
        pub done: AtomicBool,
    }

    impl Progress {
        pub fn snapshot(&self) -> (usize, usize, bool) {
            (self.tagged.load(Ordering::Relaxed), self.found.load(Ordering::Relaxed), self.done.load(Ordering::Relaxed))
        }
        /// 0.0 ..= 1.0 for the bar; the walk counts as the first part when the total is unknown.
        pub fn fraction(&self) -> f32 {
            let (t, f, done) = self.snapshot();
            let scanned = self.scanned.load(Ordering::Relaxed);
            if done || (f > 0 && t >= f && scanned) {
                1.0
            } else if f == 0 {
                0.0
            } else if scanned {
                t as f32 / f as f32
            } else {
                (t as f32 / f as f32).min(0.99) // the total is still growing
            }
        }
    }

    pub enum LoadMsg {
        /// New tracks (titles are file names until their tags arrive).
        Found(Vec<Track>),
        Tags(Vec<(PathBuf, Tags)>),
        Done,
    }

    /// Visit every audio file below `root` (hidden entries and symlinked folders are skipped).
    pub fn walk_files(root: &Path, f: &mut dyn FnMut(PathBuf) -> bool) {
        fn walk(dir: &Path, depth: usize, f: &mut dyn FnMut(PathBuf) -> bool) -> bool {
            if depth > MAX_DEPTH {
                return true;
            }
            let Ok(rd) = std::fs::read_dir(dir) else { return true };
            for e in rd.flatten() {
                let p = e.path();
                if e.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let go = match e.file_type() {
                    Ok(t) if t.is_dir() => walk(&p, depth + 1, f),
                    Ok(t) if (t.is_file() || t.is_symlink()) && is_audio(&p) => f(p),
                    _ => true,
                };
                if !go {
                    return false;
                }
            }
            true
        }
        walk(root, 0, f);
    }

    pub fn track_for(root: &Path, p: PathBuf) -> Track {
        Track {
            title: p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            folder: p.parent().and_then(|d| d.strip_prefix(root).ok()).map(|d| d.to_string_lossy().into_owned()).unwrap_or_default(),
            path: p,
        }
    }

    // ---- cache ----------------------------------------------------------------

    fn fnv64(s: &str) -> u64 {
        s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
    }

    /// The cache key of a file: its path, modification time (seconds) and size.
    pub fn cache_key(path: &str, mtime: u64, size: u64) -> String {
        format!("{:016x}", fnv64(&format!("{path}\0{mtime}\0{size}")))
    }

    /// (mtime seconds, size) of a file, or None if it cannot be read.
    pub fn stat(p: &Path) -> Option<(u64, u64)> {
        let md = std::fs::metadata(p).ok()?;
        let m = md.modified().ok()?.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Some((m, md.len()))
    }

    pub fn cache_dir() -> Option<PathBuf> {
        Some(PathBuf::from(std::env::var_os("HOME")?).join(".cache/harbour/music"))
    }

    fn clean(s: &str) -> String {
        s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
    }

    /// path -> (mtime, size, tags), stored as `tags.tsv`.
    #[derive(Default)]
    pub struct TagCache {
        map: HashMap<String, (u64, u64, Tags)>,
    }

    impl TagCache {
        pub fn parse(text: &str) -> Self {
            let mut map = HashMap::new();
            for line in text.lines() {
                let f: Vec<&str> = line.split('\t').collect();
                if f.len() != 6 {
                    continue;
                }
                let (Ok(m), Ok(s)) = (f[1].parse(), f[2].parse()) else { continue };
                let opt = |x: &str| Some(x.to_string()).filter(|x| !x.is_empty());
                map.insert(f[0].to_string(), (m, s, Tags { title: opt(f[3]), artist: opt(f[4]), album: opt(f[5]) }));
            }
            TagCache { map }
        }

        pub fn serialize(&self) -> String {
            let mut keys: Vec<&String> = self.map.keys().collect();
            keys.sort();
            keys.into_iter()
                .map(|k| {
                    let (m, s, t) = &self.map[k];
                    format!("{}\t{m}\t{s}\t{}\t{}\t{}\n", clean(k), clean(t.title.as_deref().unwrap_or("")), clean(t.artist.as_deref().unwrap_or("")), clean(t.album.as_deref().unwrap_or("")))
                })
                .collect()
        }

        pub fn load(dir: &Path) -> Self {
            std::fs::read_to_string(dir.join("tags.tsv")).map(|t| Self::parse(&t)).unwrap_or_default()
        }

        pub fn save(&self, dir: &Path) {
            let _ = std::fs::create_dir_all(dir);
            let tmp = dir.join("tags.tsv.tmp");
            if std::fs::write(&tmp, self.serialize()).is_ok() {
                let _ = std::fs::rename(tmp, dir.join("tags.tsv"));
            }
        }

        /// The cached tags, only if the file still has the same mtime and size.
        pub fn get(&self, path: &str, mtime: u64, size: u64) -> Option<Tags> {
            self.map.get(path).filter(|(m, s, _)| *m == mtime && *s == size).map(|e| e.2.clone())
        }

        pub fn insert(&mut self, path: &str, mtime: u64, size: u64, t: Tags) {
            self.map.insert(clean(path), (mtime, size, t));
        }
    }

    /// Scan `root` and read all tags on a thread. `delay` slows each tag read down (mock mode).
    pub fn spawn_loader(root: PathBuf, cache_dir: Option<PathBuf>, delay: Duration, progress: Arc<Progress>) -> Receiver<LoadMsg> {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut all: Vec<PathBuf> = Vec::new();
            let mut batch: Vec<Track> = Vec::new();
            let mut last = Instant::now();
            let mut alive = true;
            // Mock mode (a slow card) flushes late, so the empty "Loading" bar can be seen.
            let flush_every = Duration::from_millis(150) + delay * 8;
            walk_files(&root, &mut |p| {
                if !delay.is_zero() {
                    std::thread::sleep(delay / 2); // mock mode: a slow card
                }
                progress.found.fetch_add(1, Ordering::Relaxed);
                all.push(p.clone());
                batch.push(track_for(&root, p));
                if batch.len() >= 64 || last.elapsed() > flush_every {
                    alive = tx.send(LoadMsg::Found(std::mem::take(&mut batch))).is_ok();
                    last = Instant::now();
                }
                alive && all.len() < MAX_TRACKS
            });
            if !alive || (!batch.is_empty() && tx.send(LoadMsg::Found(batch)).is_err()) {
                return;
            }
            progress.scanned.store(true, Ordering::Relaxed);
            all.sort_by_key(|p| p.to_string_lossy().to_lowercase());

            let old = cache_dir.as_deref().map(TagCache::load).unwrap_or_default();
            let mut fresh = TagCache::default();
            let mut pending: Vec<(PathBuf, Tags)> = Vec::new();
            let mut last = Instant::now();
            for p in all {
                let key = p.to_string_lossy().into_owned();
                let (m, s) = stat(&p).unwrap_or((0, 0));
                let tags = match old.get(&key, m, s) {
                    Some(t) => {
                        progress.hits.fetch_add(1, Ordering::Relaxed);
                        t
                    }
                    None => {
                        if !delay.is_zero() {
                            std::thread::sleep(delay);
                        }
                        super::tags::read_embedded(&p).tags
                    }
                };
                fresh.insert(&key, m, s, tags.clone());
                progress.tagged.fetch_add(1, Ordering::Relaxed);
                pending.push((p, tags));
                if pending.len() >= 32 || last.elapsed() > Duration::from_millis(150) {
                    if tx.send(LoadMsg::Tags(std::mem::take(&mut pending))).is_err() {
                        return;
                    }
                    last = Instant::now();
                }
            }
            if !pending.is_empty() {
                let _ = tx.send(LoadMsg::Tags(pending));
            }
            if let Some(d) = &cache_dir {
                fresh.save(d);
            }
            progress.done.store(true, Ordering::Relaxed);
            let _ = tx.send(LoadMsg::Done);
        });
        rx
    }
}

// ------------------------------------------------------- tags and covers

/// Tag reading (m4a, mp3, flac) and cover art lookup. Pure Rust, no ffmpeg needed.
pub mod tags {
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::launcher::apps::imageviewer::{self, Picture};
    use crate::launcher::apps::tone;

    /// Edge of the square cover shown on Now Playing: with its 4 px mat the panel is 144 px,
    /// from y 24 down to its shadow on y 168.
    pub const COVER: u32 = 136;

    /// A cover reduced to two tones, one byte per pixel: 1 = light (paper), 0 = dark (ink).
    /// Made once when the cover is loaded; the theme's colours are applied when drawing.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Art {
        pub w: u32,
        pub h: u32,
        pub bits: Vec<u8>,
    }

    /// Grey, stretched, then error-diffused to two tones (1-bit art).
    pub fn art_from(p: &Picture) -> Art {
        let mut gray = tone::to_gray(p);
        tone::stretch(&mut gray);
        Art { w: p.w, h: p.h, bits: tone::quantize_1bit(&gray, p.w as usize, p.h as usize, tone::ART_DITHER) }
    }

    /// Horizontal runs of dark pixels, offset to (ox, oy).
    pub fn dark_runs(a: &Art, ox: i32, oy: i32) -> Vec<sdl2::rect::Rect> {
        let (w, h) = (a.w as usize, a.h as usize);
        let mut v = Vec::new();
        for y in 0..h {
            let mut start: Option<usize> = None;
            for x in 0..=w {
                let dark = x < w && a.bits[y * w + x] == 0;
                match (dark, start) {
                    (true, None) => start = Some(x),
                    (false, Some(st)) => {
                        v.push(sdl2::rect::Rect::new(ox + st as i32, oy + y as i32, (x - st) as u32, 1));
                        start = None;
                    }
                    _ => {}
                }
            }
        }
        v
    }
    const MAX_BLOCK: u64 = 24 * 1024 * 1024;

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Tags {
        pub title: Option<String>,
        pub artist: Option<String>,
        pub album: Option<String>,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Embedded {
        pub tags: Tags,
        pub cover: Option<Vec<u8>>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Source {
        Embedded,
        Folder,
        Cached,
        Placeholder,
    }

    pub struct Meta {
        pub tags: Tags,
        pub cover: Arc<Art>,
        pub source: Source,
    }

    /// Covers per album (folder + album tag), shared with the loader threads.
    pub type CoverCache = Arc<Mutex<HashMap<String, Arc<Art>>>>;

    // ---- m4a / mp4 -------------------------------------------------------

    /// Split a buffer into (fourcc, payload) boxes. Broken sizes end the walk.
    pub fn boxes(buf: &[u8]) -> Vec<([u8; 4], &[u8])> {
        let mut v = Vec::new();
        let mut i = 0usize;
        while i + 8 <= buf.len() {
            let size = u32::from_be_bytes(buf[i..i + 4].try_into().unwrap()) as usize;
            let typ: [u8; 4] = buf[i + 4..i + 8].try_into().unwrap();
            if size < 8 || i + size > buf.len() {
                break;
            }
            v.push((typ, &buf[i + 8..i + size]));
            i += size;
        }
        v
    }

    fn find<'a>(buf: &'a [u8], typ: &[u8; 4]) -> Option<&'a [u8]> {
        boxes(buf).into_iter().find(|(t, _)| t == typ).map(|(_, p)| p)
    }

    pub fn parse_moov(moov: &[u8]) -> Embedded {
        let mut out = Embedded::default();
        let Some(meta) = find(moov, b"udta").and_then(|u| find(u, b"meta")) else { return out };
        let Some(ilst) = meta.get(4..).and_then(|m| find(m, b"ilst")) else { return out };
        for (typ, item) in boxes(ilst) {
            let Some(data) = find(item, b"data") else { continue };
            let Some(payload) = data.get(8..) else { continue };
            let text = || Some(String::from_utf8_lossy(payload).trim().to_string()).filter(|s| !s.is_empty());
            match &typ {
                [0xA9, b'n', b'a', b'm'] => out.tags.title = text(),
                [0xA9, b'A', b'R', b'T'] => out.tags.artist = out.tags.artist.take().or_else(text),
                b"aART" => out.tags.artist = out.tags.artist.take().or_else(text),
                [0xA9, b'a', b'l', b'b'] => out.tags.album = text(),
                b"covr" if !payload.is_empty() && out.cover.is_none() => out.cover = Some(payload.to_vec()),
                _ => {}
            }
        }
        out
    }

    fn read_m4a(f: &mut File) -> Option<Embedded> {
        let len = f.metadata().ok()?.len();
        let mut pos = 0u64;
        while pos + 8 <= len {
            f.seek(SeekFrom::Start(pos)).ok()?;
            let mut h = [0u8; 16];
            let n = f.read(&mut h).ok()?;
            if n < 8 {
                return None;
            }
            let mut size = u32::from_be_bytes(h[0..4].try_into().unwrap()) as u64;
            let mut hdr = 8;
            if size == 1 && n >= 16 {
                size = u64::from_be_bytes(h[8..16].try_into().unwrap());
                hdr = 16;
            } else if size == 0 {
                size = len - pos;
            }
            if size < hdr {
                return None;
            }
            if &h[4..8] == b"moov" {
                if size - hdr > MAX_BLOCK {
                    return None;
                }
                f.seek(SeekFrom::Start(pos + hdr)).ok()?;
                let mut buf = vec![0u8; (size - hdr) as usize];
                f.read_exact(&mut buf).ok()?;
                return Some(parse_moov(&buf));
            }
            pos += size;
        }
        None
    }

    // ---- mp3 / ID3v2 -----------------------------------------------------

    fn syncsafe(b: &[u8]) -> usize {
        b.iter().take(4).fold(0, |a, &x| (a << 7) | (x & 0x7F) as usize)
    }

    pub fn decode_text(enc: u8, b: &[u8]) -> String {
        let s = match enc {
            1 | 2 => {
                let (be, body) = match b {
                    [0xFF, 0xFE, r @ ..] => (false, r),
                    [0xFE, 0xFF, r @ ..] => (true, r),
                    r => (enc == 2, r),
                };
                let u: Vec<u16> = body.chunks_exact(2).map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }).collect();
                String::from_utf16_lossy(&u)
            }
            3 => String::from_utf8_lossy(b).into_owned(),
            _ => b.iter().map(|&c| c as char).collect(),
        };
        s.split('\0').next().unwrap_or("").trim().to_string()
    }

    /// Parse an ID3v2.3 / 2.4 tag (the bytes after the 10 byte header).
    pub fn parse_id3(ver: u8, flags: u8, body: &[u8]) -> Embedded {
        let mut out = Embedded::default();
        let mut i = 0usize;
        if flags & 0x40 != 0 && body.len() >= 4 {
            i = if ver == 4 { syncsafe(body) } else { u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize + 4 };
        }
        let mut best_pic: Option<(u8, Vec<u8>)> = None;
        while i + 10 <= body.len() && body[i] != 0 {
            let id = &body[i..i + 4];
            let size = if ver == 4 { syncsafe(&body[i + 4..i + 8]) } else { u32::from_be_bytes(body[i + 4..i + 8].try_into().unwrap()) as usize };
            let end = i + 10 + size;
            if end > body.len() {
                break;
            }
            let f = &body[i + 10..end];
            let text = || f.split_first().map(|(&e, r)| decode_text(e, r)).filter(|s| !s.is_empty());
            match id {
                b"TIT2" => out.tags.title = text(),
                b"TPE1" => out.tags.artist = text(),
                b"TALB" => out.tags.album = text(),
                b"APIC" => {
                    if let Some((ptype, data)) = parse_apic(f) {
                        if best_pic.as_ref().map_or(true, |(t, _)| ptype == 3 && *t != 3) {
                            best_pic = Some((ptype, data));
                        }
                    }
                }
                _ => {}
            }
            i = end;
        }
        out.cover = best_pic.map(|(_, d)| d);
        out
    }

    fn parse_apic(f: &[u8]) -> Option<(u8, Vec<u8>)> {
        let enc = *f.first()?;
        let mime_end = 1 + f.get(1..)?.iter().position(|&b| b == 0)?;
        let ptype = *f.get(mime_end + 1)?;
        let mut p = mime_end + 2;
        if enc == 1 || enc == 2 {
            while p + 1 < f.len() && !(f[p] == 0 && f[p + 1] == 0) {
                p += 2;
            }
            p += 2;
        } else {
            while p < f.len() && f[p] != 0 {
                p += 1;
            }
            p += 1;
        }
        let data = f.get(p..)?;
        (!data.is_empty()).then(|| (ptype, data.to_vec()))
    }

    fn read_mp3(f: &mut File) -> Option<Embedded> {
        let mut h = [0u8; 10];
        f.read_exact(&mut h).ok()?;
        if &h[0..3] != b"ID3" || !(3..=4).contains(&h[3]) {
            return None;
        }
        let size = syncsafe(&h[6..10]);
        if size as u64 > MAX_BLOCK {
            return None;
        }
        let mut body = vec![0u8; size];
        f.read_exact(&mut body).ok()?;
        Some(parse_id3(h[3], h[5], &body))
    }

    // ---- flac ---------------------------------------------------------------

    fn le32(b: &[u8]) -> usize {
        u32::from_le_bytes(b[0..4].try_into().unwrap()) as usize
    }
    fn be32(b: &[u8]) -> usize {
        u32::from_be_bytes(b[0..4].try_into().unwrap()) as usize
    }

    pub fn parse_vorbis_comments(b: &[u8], t: &mut Tags) {
        let Some(vl) = b.get(0..4).map(le32) else { return };
        let mut i = 4 + vl;
        let Some(n) = b.get(i..i + 4).map(le32) else { return };
        i += 4;
        for _ in 0..n {
            let Some(l) = b.get(i..i + 4).map(le32) else { return };
            let Some(e) = b.get(i + 4..i + 4 + l) else { return };
            i += 4 + l;
            let s = String::from_utf8_lossy(e);
            if let Some((k, v)) = s.split_once('=') {
                let v = Some(v.trim().to_string()).filter(|v| !v.is_empty());
                match k.to_ascii_uppercase().as_str() {
                    "TITLE" => t.title = v,
                    "ARTIST" => t.artist = v,
                    "ALBUM" => t.album = v,
                    _ => {}
                }
            }
        }
    }

    pub fn parse_flac_picture(b: &[u8]) -> Option<(u8, Vec<u8>)> {
        let ptype = be32(b.get(0..4)?);
        let ml = be32(b.get(4..8)?);
        let mut i = 8 + ml;
        let dl = be32(b.get(i..i + 4)?);
        i += 4 + dl + 16; // description, then width, height, depth, colours
        let len = be32(b.get(i..i + 4)?);
        i += 4;
        let data = b.get(i..i + len)?;
        Some((ptype as u8, data.to_vec()))
    }

    fn read_flac(f: &mut File) -> Option<Embedded> {
        let mut m = [0u8; 4];
        f.read_exact(&mut m).ok()?;
        if &m != b"fLaC" {
            return None;
        }
        let mut out = Embedded::default();
        let mut best: Option<(u8, Vec<u8>)> = None;
        loop {
            let mut h = [0u8; 4];
            f.read_exact(&mut h).ok()?;
            let (last, typ, len) = (h[0] & 0x80 != 0, h[0] & 0x7F, ((h[1] as u64) << 16) | ((h[2] as u64) << 8) | h[3] as u64);
            if matches!(typ, 4 | 6) && len <= MAX_BLOCK {
                let mut b = vec![0u8; len as usize];
                f.read_exact(&mut b).ok()?;
                if typ == 4 {
                    parse_vorbis_comments(&b, &mut out.tags);
                } else if let Some((t, d)) = parse_flac_picture(&b) {
                    if best.as_ref().map_or(true, |(bt, _)| t == 3 && *bt != 3) {
                        best = Some((t, d));
                    }
                }
            } else {
                f.seek(SeekFrom::Current(len as i64)).ok()?;
            }
            if last {
                break;
            }
        }
        out.cover = best.map(|(_, d)| d);
        Some(out)
    }

    pub fn read_embedded(path: &Path) -> Embedded {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let Ok(mut f) = File::open(path) else { return Embedded::default() };
        match ext.as_str() {
            "m4a" | "mp4" | "m4b" => read_m4a(&mut f),
            "mp3" => read_mp3(&mut f),
            "flac" => read_flac(&mut f),
            _ => None,
        }
        .unwrap_or_default()
    }

    // ---- folder art and generated art ---------------------------------------

    /// Index of the best `cover.jpg` / `folder.png` style file among `names`.
    pub fn pick_cover_file(names: &[String]) -> Option<usize> {
        const STEMS: [&str; 6] = ["cover", "folder", "front", "albumart", "album", "art"];
        names
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                let lower = n.to_ascii_lowercase();
                let (stem, ext) = lower.rsplit_once('.')?;
                if !matches!(ext, "jpg" | "jpeg" | "png") {
                    return None;
                }
                // "AlbumArt_{GUID}_Large.jpg" and friends count as albumart.
                let rank = STEMS.iter().position(|s| stem == *s).or_else(|| stem.starts_with("albumart").then_some(3))?;
                Some((rank, i))
            })
            .min()
            .map(|(_, i)| i)
    }

    pub fn find_folder_cover(dir: &Path) -> Option<PathBuf> {
        let names: Vec<String> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        pick_cover_file(&names).map(|i| dir.join(&names[i]))
    }

    /// A record: grooves, a bright label, a hole and a glossy highlight. Grey levels, so the
    /// dither turns it into texture.
    pub fn placeholder_record(size: u32) -> Picture {
        let n = size as f32;
        let mut rgb = Vec::with_capacity((size * size * 3) as usize);
        for y in 0..size {
            for x in 0..size {
                let (dx, dy) = ((x as f32 + 0.5) / n * 2.0 - 1.0, (y as f32 + 0.5) / n * 2.0 - 1.0);
                let d = (dx * dx + dy * dy).sqrt();
                let ang = dy.atan2(dx);
                let l: f32 = if d > 0.96 {
                    0.0
                } else if d < 0.05 {
                    0.0
                } else if d < 0.34 {
                    if (0.30..0.34).contains(&d) { 90.0 } else { 215.0 }
                } else {
                    let groove = (d * 90.0).sin() * 18.0;
                    let gloss = ((ang * 2.0).cos().max(0.0)).powi(6) * 70.0;
                    70.0 + groove + gloss
                };
                let v = l.clamp(0.0, 255.0) as u8;
                rgb.extend([v, v, v]);
            }
        }
        Picture { w: size, h: size, rgb }
    }

    /// Fake art for mock mode: a plasma, clearly not a record.
    pub fn fake_cover(size: u32) -> Picture {
        let mut rgb = Vec::with_capacity((size * size * 3) as usize);
        for y in 0..size {
            for x in 0..size {
                let (fx, fy) = (x as f32 / size as f32 * 6.28, y as f32 / size as f32 * 6.28);
                let v = ((fx * 1.5).sin() + (fy * 2.0 + fx).cos() + ((fx - 3.14).powi(2) + (fy - 3.14).powi(2)).sqrt().sin()) / 3.0;
                let l = (128.0 + 120.0 * v).clamp(0.0, 255.0) as u8;
                rgb.extend([l, l, l]);
            }
        }
        Picture { w: size, h: size, rgb }
    }

    /// A cover on disk: "HCV2", width, height (u32 LE), then the pixels packed 8 per byte.
    /// (HCV1 held RGB; those files are ignored and rebuilt.)
    pub fn write_cover(path: &Path, a: &Art) {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let mut v = b"HCV2".to_vec();
        v.extend(a.w.to_le_bytes());
        v.extend(a.h.to_le_bytes());
        v.extend(a.bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |acc, (i, &b)| acc | ((b & 1) << i))));
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, v).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }

    pub fn read_cover(path: &Path) -> Option<Art> {
        let b = std::fs::read(path).ok()?;
        if b.len() < 12 || &b[0..4] != b"HCV2" {
            return None;
        }
        let (w, h) = (u32::from_le_bytes(b[4..8].try_into().ok()?), u32::from_le_bytes(b[8..12].try_into().ok()?));
        let n = (w as usize).checked_mul(h as usize)?;
        if w == 0 || h == 0 || w > 1024 || h > 1024 || b.len() != 12 + (n + 7) / 8 {
            return None;
        }
        let bits = (0..n).map(|i| (b[12 + i / 8] >> (i % 8)) & 1).collect();
        Some(Art { w, h, bits })
    }

    /// Where the decoded cover of a track is cached.
    pub fn cover_cache_file(dir: &Path, track: &Path) -> Option<PathBuf> {
        let (m, s) = super::library::stat(track)?;
        Some(dir.join("covers").join(format!("{}.cov", super::library::cache_key(&track.to_string_lossy(), m, s))))
    }

    /// Steps of `load_meta`, for the progress bar.
    pub const STAGES: usize = 3;

    /// Everything the Now Playing screen needs for a file. Runs on a worker thread and
    /// reports its progress (0..=STAGES) in `stage`. `disk` is the cache directory.
    pub fn load_meta(path: &Path, cache: &CoverCache, mock: bool, stage: &AtomicUsize, disk: Option<&Path>) -> Meta {
        let nap = || {
            if mock {
                std::thread::sleep(std::time::Duration::from_millis(350));
            }
        };
        stage.store(0, Ordering::Relaxed);
        nap();
        let emb = read_embedded(path);
        stage.store(1, Ordering::Relaxed);
        let dir = path.parent().unwrap_or(Path::new(""));
        let key = format!("{}|{}", dir.display(), emb.tags.album.clone().unwrap_or_default());
        let finish = |tags: Tags, cover: Arc<Art>, source: Source| {
            stage.store(STAGES, Ordering::Relaxed);
            Meta { tags, cover, source }
        };
        if let Some(p) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
            return finish(emb.tags, p, Source::Cached);
        }
        let file = disk.and_then(|d| cover_cache_file(d, path));
        // A cover cached at another size (an older COVER) is decoded again.
        if let Some(p) = file.as_deref().and_then(read_cover).filter(|a| a.w.max(a.h) == COVER) {
            let p = Arc::new(p);
            if let Ok(mut c) = cache.lock() {
                c.insert(key, p.clone());
            }
            return finish(emb.tags, p, Source::Cached);
        }
        nap();
        let decode = |bytes: &[u8]| imageviewer::decode_bytes(bytes, COVER, COVER).ok().map(|p| art_from(&p));
        let (pic, source) = if let Some(p) = emb.cover.as_deref().and_then(decode) {
            (p, Source::Embedded)
        } else if let Some(p) = find_folder_cover(dir).and_then(|f| std::fs::read(f).ok()).as_deref().and_then(decode) {
            (p, Source::Folder)
        } else if mock {
            (art_from(&fake_cover(COVER)), Source::Placeholder)
        } else {
            (art_from(&placeholder_record(COVER)), Source::Placeholder)
        };
        stage.store(2, Ordering::Relaxed);
        if source != Source::Placeholder {
            if let Some(f) = &file {
                write_cover(f, &pic);
            }
        }
        let pic = Arc::new(pic);
        if let Ok(mut c) = cache.lock() {
            c.insert(key, pic.clone());
        }
        nap();
        finish(emb.tags, pic, source)
    }
}

// ------------------------------------------------------------- timeline

/// Wall-clock playback position that excludes paused time.
#[derive(Clone, Copy, Debug)]
pub struct Timeline {
    started: Instant,
    paused_total: Duration,
    paused_at: Option<Instant>,
    /// Where in the track the clock started (a restart part-way through).
    offset: Duration,
}

impl Timeline {
    pub fn new(now: Instant) -> Self {
        Self::starting_at(now, Duration::ZERO)
    }
    pub fn starting_at(now: Instant, offset: Duration) -> Self {
        Timeline { started: now, paused_total: Duration::ZERO, paused_at: None, offset }
    }
    pub fn elapsed(&self, now: Instant) -> Duration {
        let end = self.paused_at.unwrap_or(now);
        self.offset + end.saturating_duration_since(self.started).saturating_sub(self.paused_total)
    }
    pub fn pause(&mut self, now: Instant) {
        self.paused_at.get_or_insert(now);
    }
    pub fn resume(&mut self, now: Instant) {
        if let Some(p) = self.paused_at.take() {
            self.paused_total += now.saturating_duration_since(p);
        }
    }
    pub fn paused(&self) -> bool {
        self.paused_at.is_some()
    }
}

pub fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

// ------------------------------------------------------------------ app

struct Playing {
    index: usize,
    proc: Box<dyn Proc>,
    time: Timeline,
    total: Option<Duration>,
    /// Audio is really running (until then: "Starting..." and the clock does not run).
    ready: bool,
    /// Where in the track this process started.
    from: Duration,
    /// The equalizer this process plays with.
    eq: Eq,
    /// Counts processes, so the visualizer's tap knows when to follow a new one.
    gen: u64,
}

/// What the Now Playing panel shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Cover,
    Bars,
    Wave,
}

impl View {
    fn next(self) -> View {
        match self {
            View::Cover => View::Bars,
            View::Bars => View::Wave,
            View::Wave => View::Cover,
        }
    }
    fn key(self) -> &'static str {
        match self {
            View::Cover => "cover",
            View::Bars => "bars",
            View::Wave => "wave",
        }
    }
    fn from_key(s: &str) -> View {
        match s {
            "bars" => View::Bars,
            "wave" => View::Wave,
            _ => View::Cover,
        }
    }
}

/// Starts the visualizer's sample source; a mock in tests.
pub type TapMaker = fn(&Path, Duration, Option<&str>) -> Result<Box<dyn Source>, String>;

fn ffmpeg_tap(path: &Path, from: Duration, af: Option<&str>) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(music_vis::Tap::spawn(path, from, af)?))
}

/// The visualizer's running source: which process it follows and where in the track it started.
struct TapState {
    gen: u64,
    from: Duration,
    src: Box<dyn Source>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    List,
    Now,
    Eq,
}

/// How long the EQ knobs must be still before ffplay is restarted with the new filters.
const EQ_SETTLE: Duration = Duration::from_millis(450);

pub struct Music {
    audio: Box<dyn Audio>,
    tracks: Vec<Track>,
    cursor: usize,
    scroll: usize,
    screen: Screen,
    playing: Option<Playing>,
    error: Option<String>,
    sb_drag: bool,
    // Cover art and tags of the playing track, loaded on a worker thread.
    cache: tags::CoverCache,
    mock: bool,
    async_meta: bool,
    rx: Option<Receiver<(u64, tags::Meta)>>,
    dur_rx: Option<Receiver<(u64, Option<Duration>)>>,
    born: Instant,
    serial: u64,
    meta: Option<tags::Meta>,
    art_runs: Option<Vec<Rect>>,
    _img: Option<Sdl2ImageContext>,
    cover_stage: Arc<AtomicUsize>,
    disk: Option<PathBuf>,
    // The library loading in the background (None when finished).
    loader: Option<(Receiver<library::LoadMsg>, Arc<library::Progress>)>,
    // Equalizer: the settings, when they last changed, unsaved, and the EQ screen's state.
    eq: Eq,
    eq_changed: Option<Instant>,
    prefs_dirty: bool,
    cfg_path: Option<PathBuf>,
    /// 0 = the preset row, 1..=5 = the bands.
    eq_focus: usize,
    eq_back: Screen,
    eq_drag: Option<usize>,
    // Visualizer.
    view: View,
    tap_maker: TapMaker,
    tap: Option<TapState>,
    meter: Meter,
    wave: Vec<f32>,
    vis_at: Option<Instant>,
    vis_error: Option<String>,
    gen: u64,
}

pub fn make() -> Box<dyn LauncherApp> {
    let root = crate::launcher::fileops::user_dir(crate::launcher::fileops::UserDir::Music);
    let mock = std::env::var("HARBOUR_MOCK").map(|v| v == "1" || v == "true").unwrap_or(false);
    let mut m = Music::new(Box::new(Ffplay { min_start: Duration::ZERO }), Vec::new());
    let progress = Arc::new(library::Progress::default());
    // Mock mode pretends the device is slow, so the progress bars can be seen.
    let delay = if mock { Duration::from_millis(400) } else { Duration::ZERO };
    m.disk = library::cache_dir();
    m.loader = Some((library::spawn_loader(root, m.disk.clone(), delay, progress.clone()), progress));
    // The host starts PNG only; covers are often JPEG.
    m._img = sdl2::image::init(InitFlag::PNG | InitFlag::JPG).or_else(|_| sdl2::image::init(InitFlag::PNG)).ok();
    m.async_meta = true;
    m.audio = Box::new(Ffplay { min_start: if mock { Duration::from_millis(3000) } else { Duration::ZERO } });
    m.mock = mock;
    m.use_config(config::default_path());
    Box::new(m)
}

const LIST_Y: i32 = grid::BAR_H + grid::MARGIN;
const LIST_X: i32 = grid::MARGIN;
/// The list's width and rows follow the surface (a desk window can be any size).
fn list_w() -> i32 {
    grid::scroll_x() - grid::GUTTER - grid::MARGIN
}
fn visible() -> usize {
    ((grid::sh() - grid::WINDOW_INSET - LIST_Y) / grid::ROW_H).max(1) as usize
}
/// The list's frame and scroll bar run down to y 168 (`grid::APP_FRAME_BOTTOM`): 9 rows, and the
/// 5 px under the last one are the frame's bottom edge.
fn list_h() -> i32 {
    grid::frame_bottom() + 1 - LIST_Y
}

impl Music {
    pub fn new(audio: Box<dyn Audio>, tracks: Vec<Track>) -> Self {
        Music { audio, tracks, cursor: 0, scroll: 0, screen: Screen::List, playing: None, error: None, sb_drag: false,
            cache: Default::default(), mock: false, async_meta: false, rx: None, dur_rx: None, born: Instant::now(), serial: 0, meta: None, art_runs: None, _img: None,
            cover_stage: Default::default(), disk: None, loader: None,
            eq: Eq::default(), eq_changed: None, prefs_dirty: false, cfg_path: None, eq_focus: 1, eq_back: Screen::List, eq_drag: None,
            view: View::Cover, tap_maker: ffmpeg_tap, tap: None, meter: Meter::default(), wave: Vec::new(), vis_at: None, vis_error: None, gen: 0 }
    }

    fn cycle_view(&mut self) {
        self.view = self.view.next();
        self.prefs_dirty = true;
        self.meter = Meter::default();
        self.wave.clear();
        self.vis_at = None;
        self.vis_error = None;
        if self.screen == Screen::List && self.playing.is_some() && self.view != View::Cover {
            self.screen = Screen::Now;
        }
    }

    /// Keep the visualizer's tap following playback and analyse the samples at the clock.
    /// True when there is something new to draw.
    fn sync_vis(&mut self, now: Instant) -> bool {
        let wanted = self.screen == Screen::Now && self.view != View::Cover;
        let Some(p) = self.playing.as_ref().filter(|_| wanted) else {
            self.tap = None;
            return false;
        };
        let clock = if p.ready { p.time.elapsed(now) } else { p.from };
        if self.tap.as_ref().map(|t| t.gen) != Some(p.gen) {
            self.tap = None;
            let path = self.tracks[p.index].path.clone();
            match (self.tap_maker)(&path, clock, p.eq.ffmpeg_filter().as_deref()) {
                Ok(src) => {
                    self.tap = Some(TapState { gen: p.gen, from: clock, src });
                    self.vis_error = None;
                }
                Err(e) => {
                    let changed = self.vis_error.is_none();
                    self.vis_error = Some(e);
                    // Do not retry every frame: wait for the next process.
                    self.tap = Some(TapState { gen: p.gen, from: clock, src: Box::new(NoSource) });
                    return changed;
                }
            }
        }
        let paused = p.time.paused() || !p.ready;
        let dt = self.vis_at.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32()).min(0.25);
        self.vis_at = Some(now);
        let Some(t) = &mut self.tap else { return false };
        let win = t.src.window(clock.saturating_sub(t.from));
        match win {
            Some(w) => {
                if !paused {
                    self.meter.update(&music_vis::band_db(&w, music_vis::RATE as f32), dt);
                }
                self.wave = w;
            }
            None if !paused => self.meter.update(&[music_vis::FLOOR_DB; music_eq::N], dt),
            None => {}
        }
        !paused
    }

    /// Read the equalizer from the config file and save changes there.
    pub fn use_config(&mut self, path: PathBuf) {
        let cfg = Config::load(path.clone());
        self.eq = Eq::load(&cfg);
        self.view = View::from_key(&cfg.get_str("music_view", "cover"));
        self.cfg_path = Some(path);
    }

    fn save_eq(&mut self) {
        if !self.prefs_dirty {
            return;
        }
        self.prefs_dirty = false;
        let Some(p) = self.cfg_path.clone() else { return };
        let mut cfg = Config::load(p);
        self.eq.store(&mut cfg);
        cfg.set_str("music_view", self.view.key());
        let _ = cfg.save(); // a read-only or foreign config is left alone
    }

    fn set_eq(&mut self, e: Eq, now: Instant) {
        if e != self.eq {
            self.eq = e;
            self.eq_changed = Some(now);
            self.prefs_dirty = true;
        }
    }

    fn nudge_band(&mut self, band: usize, delta: i32, now: Instant) {
        let mut e = self.eq;
        e.set(band, e.gains[band] as i32 + delta);
        self.set_eq(e, now);
    }

    /// Step through the presets (from Custom: to the nearest one in that direction).
    fn cycle_preset(&mut self, delta: isize, now: Instant) {
        let n = music_eq::PRESETS.len() as isize;
        let next = match self.eq.preset() {
            Some(i) => (i as isize + delta).rem_euclid(n),
            None if delta > 0 => 0,
            None => n - 1,
        };
        self.set_eq(Eq { gains: music_eq::PRESETS[next as usize].1 }, now);
    }

    fn open_eq(&mut self) {
        if self.screen != Screen::Eq {
            self.eq_back = self.screen;
            self.screen = Screen::Eq;
        }
    }

    fn close_eq(&mut self) {
        self.screen = self.eq_back;
        self.eq_drag = None;
        self.save_eq();
    }

    /// Restart the playing track at its position when the equalizer has changed and been still
    /// for [`EQ_SETTLE`]. True when it restarted.
    fn apply_eq(&mut self, now: Instant) -> bool {
        let Some(changed) = self.eq_changed else { return false };
        if now.saturating_duration_since(changed) < EQ_SETTLE {
            return false;
        }
        self.eq_changed = None;
        let eq = self.eq;
        let Some(p) = &mut self.playing else { return false };
        if p.eq == eq {
            return false;
        }
        let at = if p.ready { p.time.elapsed(now) } else { p.from };
        let at = p.total.map_or(at, |t| at.min(t));
        let path = self.tracks[p.index].path.clone();
        match self.audio.spawn(&path, at, eq.ffmpeg_filter().as_deref()) {
            Ok(mut proc) => {
                p.proc.stop();
                let paused = p.time.paused();
                if paused {
                    proc.pause();
                }
                p.proc = proc;
                p.from = at;
                p.eq = eq;
                self.gen += 1;
                p.gen = self.gen;
                // The old sound stops now; the clock waits for the new one but shows `at`.
                p.ready = false;
                p.time = Timeline::starting_at(now, at);
                if paused {
                    p.time.pause(now);
                }
                true
            }
            Err(e) => {
                self.error = Some(e);
                true
            }
        }
    }

    fn follow(&mut self) {
        self.scroll = widgets::follow_scroll(self.cursor, self.scroll, visible(), self.tracks.len());
    }

    fn stop(&mut self) {
        if let Some(mut p) = self.playing.take() {
            p.proc.stop();
        }
    }

    fn play(&mut self, index: usize, now: Instant) {
        self.stop();
        let Some(t) = self.tracks.get(index) else { return };
        let eq = self.eq;
        match self.audio.spawn(&t.path, Duration::ZERO, eq.ffmpeg_filter().as_deref()) {
            Ok(proc) => {
                // The real ffprobe takes over a second: it runs on a thread when async_meta is on.
                let total = if self.async_meta { None } else { self.audio.duration(&t.path) };
                self.eq_changed = None;
                self.gen += 1;
                self.playing = Some(Playing { index, proc, time: Timeline::new(now), total, ready: false, from: Duration::ZERO, eq, gen: self.gen });
                self.error = None;
                self.start_meta(index);
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Forget the old cover and ask a worker thread for the new tags and cover.
    fn start_meta(&mut self, index: usize) {
        self.meta = None;
        self.art_runs = None;
        self.rx = None;
        if !self.async_meta {
            return;
        }
        self.serial += 1;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let (serial, path, cache, mock) = (self.serial, self.tracks[index].path.clone(), self.cache.clone(), self.mock);
        let (dtx, drx) = channel();
        self.dur_rx = Some(drx);
        let dpath = self.tracks[index].path.clone();
        std::thread::spawn(move || {
            let _ = dtx.send((serial, probe_duration(&dpath)));
        });
        self.cover_stage = Arc::new(AtomicUsize::new(0));
        let (stage, disk) = (self.cover_stage.clone(), self.disk.clone());
        std::thread::spawn(move || {
            let _ = tx.send((serial, tags::load_meta(&path, &cache, mock, &stage, disk.as_deref())));
        });
    }

    /// Merge new tracks into the sorted list, keeping the cursor and the playing track in place.
    fn insert_sorted(&mut self, new: Vec<Track>) {
        let cur = self.tracks.get(self.cursor).map(|t| t.path.clone());
        let playing = self.playing.as_ref().map(|p| self.tracks[p.index].path.clone());
        self.tracks.extend(new);
        self.tracks.sort_by_cached_key(|t| t.path.to_string_lossy().to_lowercase());
        self.tracks.dedup_by(|a, b| a.path == b.path);
        let at = |p: &Option<PathBuf>, tr: &[Track]| p.as_ref().and_then(|p| tr.iter().position(|t| &t.path == p));
        if let Some(i) = at(&cur, &self.tracks) {
            self.cursor = i;
        }
        if let (Some(i), Some(p)) = (at(&playing, &self.tracks), self.playing.as_mut()) {
            p.index = i;
        }
        self.follow();
    }

    fn apply_tags(&mut self, list: Vec<(PathBuf, tags::Tags)>) {
        for (path, t) in list {
            if let (Some(title), Ok(i)) = (t.title, self.tracks.binary_search_by_key(&path.to_string_lossy().to_lowercase(), |t| t.path.to_string_lossy().to_lowercase())) {
                self.tracks[i].title = title;
            }
        }
    }

    /// Take what the loader thread has produced. True when the screen changed.
    fn poll_loader(&mut self) -> bool {
        let Some((rx, _)) = &self.loader else { return false };
        let msgs: Vec<library::LoadMsg> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let disconnected = matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected));
        let changed = !msgs.is_empty();
        let mut finished = disconnected;
        for m in msgs {
            match m {
                library::LoadMsg::Found(v) => self.insert_sorted(v),
                library::LoadMsg::Tags(v) => self.apply_tags(v),
                library::LoadMsg::Done => finished = true,
            }
        }
        if finished {
            self.loader = None;
        }
        changed || finished
    }

    /// Pick up a finished worker result. True when something new can be drawn.
    fn poll_meta(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.dur_rx {
            if let Ok((serial, d)) = rx.try_recv() {
                if serial == self.serial {
                    if let Some(p) = &mut self.playing {
                        p.total = d;
                    }
                    changed = true;
                }
                self.dur_rx = None;
            }
        }
        changed | self.poll_cover()
    }

    fn poll_cover(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok((serial, m)) if serial == self.serial => {
                self.meta = Some(m);
                self.art_runs = None;
                self.rx = None;
                true
            }
            Ok(_) => false,
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(_) => {
                self.rx = None;
                false
            }
        }
    }

    fn toggle_pause(&mut self, now: Instant) {
        if let Some(p) = &mut self.playing {
            if p.time.paused() {
                p.proc.resume();
                p.time.resume(now);
            } else {
                p.proc.pause();
                p.time.pause(now);
            }
        }
    }

    /// Step to the next (+1) or previous (-1) track; wraps around.
    fn step(&mut self, delta: isize, now: Instant) {
        let Some(cur) = self.playing.as_ref().map(|p| p.index).or((!self.tracks.is_empty()).then_some(self.cursor)) else { return };
        let n = self.tracks.len() as isize;
        let next = (cur as isize + delta).rem_euclid(n) as usize;
        self.cursor = next;
        self.follow();
        self.play(next, now);
    }

    /// Called every frame: advance when a track has finished. Returns true if the screen changed.
    fn poll(&mut self, now: Instant) -> bool {
        let Some(p) = &mut self.playing else { return false };
        if !p.proc.exited() {
            if !p.ready && p.proc.ready() {
                // Sound starts now: the elapsed time counts from here.
                p.ready = true;
                let was_paused = p.time.paused();
                p.time = Timeline::starting_at(now, p.from);
                if was_paused {
                    p.time.pause(now);
                }
                return true;
            }
            return false;
        }
        let (idx, last) = (p.index, p.index + 1 >= self.tracks.len());
        if last {
            self.stop();
            if self.screen == Screen::Now {
                self.screen = Screen::List;
            } else {
                self.eq_back = Screen::List;
            }
        } else {
            self.cursor = idx + 1;
            self.follow();
            self.play(idx + 1, now);
        }
        true
    }

    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let r = Rect::new(LIST_X, LIST_Y, list_w() as u32, (visible() as i32 * grid::ROW_H) as u32);
        if !contains(r, x, y) {
            return None;
        }
        let i = self.scroll + ((y - LIST_Y) / grid::ROW_H) as usize;
        (i < self.tracks.len()).then_some(i)
    }

    fn scrollbar(&self) -> ScrollBar {
        ScrollBar::new(Rect::new(grid::scroll_x(), LIST_Y, grid::SCROLL_W as u32, list_h() as u32), self.tracks.len(), visible(), self.scroll)
    }
}

impl Drop for Music {
    fn drop(&mut self) {
        self.save_eq();
        self.stop();
    }
}

impl LauncherApp for Music {
    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        match self.screen {
            Screen::List => vec![k("Up / Down", "Choose a track"), k("Return / Right", "Play"), k("Space", "Play / Pause"), k("Double click", "Play")],
            Screen::Now => vec![
                k("Space / Return", "Play / Pause"),
                k("Left / Up", "Previous track"),
                k("Right / Down", "Next track"),
                k("Click", "Play / Pause"),
                k("E", "Equalizer"),
                k("V / click the cover", "Cover / bars / wave"),
            ],
            Screen::Eq => vec![
                k("Left / Right", "Choose a band"),
                k("Up / Down", "Raise / lower (preset row: change preset)"),
                k("Delete", "Band to 0 dB"),
                k("Drag / Wheel", "Set a band"),
                k("Esc / E", "Back"),
            ],
        }
    }

    fn title(&self) -> String {
        crate::launcher::i18n::tr_s("Music")
    }

    fn sizing(&self) -> crate::launcher::app::Sizing {
        crate::launcher::app::Sizing::Flexible
    }

    fn menu(&self) -> Vec<MenuItem> {
        vec![
            MenuItem::new("pause", "Play / Pause"),
            MenuItem::new("next", "Next track"),
            MenuItem::new("prev", "Previous track"),
            MenuItem::new("stop", "Stop"),
            MenuItem::new("eq", "Equalizer"),
            MenuItem::new("view", "Cover / bars / wave"),
        ]
    }

    fn context_menu(&mut self, x: i32, y: i32, _ctx: &Ctx) -> Option<Vec<MenuItem>> {
        let mut v = Vec::new();
        if self.screen == Screen::Eq {
            // The presets, the current one ticked.
            let cur = self.eq.preset();
            for (i, (name, _)) in music_eq::PRESETS.iter().enumerate() {
                let mark = if cur == Some(i) { "\u{2713} " } else { "" };
                v.push(MenuItem::new(PRESET_IDS[i], format!("{mark}{}", crate::launcher::i18n::tr(name))));
            }
            return Some(v);
        }
        if self.screen == Screen::List {
            if let Some(i) = self.row_at(x, y) {
                self.cursor = i;
                v.push(MenuItem::new("play", "Play"));
            }
        }
        v.extend(self.menu());
        Some(v)
    }

    fn menu_action(&mut self, id: &str, _ctx: &Ctx) -> AppAction {
        let now = Instant::now();
        match id {
            "play" => {
                self.play(self.cursor, now);
                if self.playing.is_some() {
                    self.screen = Screen::Now;
                }
            }
            "pause" => self.toggle_pause(now),
            "next" => self.step(1, now),
            "prev" => self.step(-1, now),
            "stop" => self.stop(),
            "eq" => self.open_eq(),
            "view" => self.cycle_view(),
            id => {
                if let Some(i) = PRESET_IDS.iter().position(|p| *p == id) {
                    self.set_eq(Eq { gains: music_eq::PRESETS[i].1 }, now);
                }
            }
        }
        AppAction::None
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        if !path.is_file() {
            return Err(crate::launcher::i18n::tr("That file is gone").into());
        }
        if !is_audio(path) {
            return Err(crate::launcher::i18n::tr("Not a sound file Music can play").into());
        }
        // The file may be outside ~/Music: add it to the list and play it.
        let track = library::track_for(path.parent().unwrap_or(Path::new("")), path.to_path_buf());
        self.insert_sorted(vec![track]);
        let Some(i) = self.tracks.iter().position(|t| t.path == path) else { return Err(crate::launcher::i18n::tr("Cannot add the file").into()) };
        self.cursor = i;
        self.follow();
        self.play(i, Instant::now());
        self.screen = Screen::Now;
        Ok(())
    }

    fn key(&mut self, key: Keycode, _mods: Mod, _repeat: bool, _ctx: &Ctx) -> AppAction {
        let now = Instant::now();
        let k = arrow_alias(key);
        if self.screen == Screen::Eq {
            self.eq_key(k, now);
            return AppAction::None;
        }
        if k == Keycode::E {
            self.open_eq();
            return AppAction::None;
        }
        if k == Keycode::V {
            self.cycle_view();
            return AppAction::None;
        }
        if k == Keycode::Escape {
            if self.screen == Screen::Now {
                self.screen = Screen::List;
                return AppAction::None;
            }
            // Top level: the host ignores a short Esc here (hold Esc to leave), so it must not stop
            // the music either; leaving stops it (Drop).
            return AppAction::Quit;
        }
        match (self.screen, k) {
            (Screen::List, Keycode::Up) => {
                self.cursor = self.cursor.saturating_sub(1);
                self.follow();
            }
            (Screen::List, Keycode::Down) => {
                self.cursor = (self.cursor + 1).min(self.tracks.len().saturating_sub(1));
                self.follow();
            }
            (Screen::List, Keycode::Return | Keycode::KpEnter | Keycode::Right) => {
                if self.playing.as_ref().map(|p| p.index) == Some(self.cursor) {
                    self.screen = Screen::Now;
                } else if !self.tracks.is_empty() {
                    self.play(self.cursor, now);
                    if self.playing.is_some() {
                        self.screen = Screen::Now;
                    }
                }
            }
            (Screen::List, Keycode::Space) | (Screen::Now, Keycode::Space | Keycode::Return | Keycode::KpEnter) => self.toggle_pause(now),
            (Screen::Now, Keycode::Right | Keycode::Down) => self.step(1, now),
            (Screen::Now, Keycode::Left | Keycode::Up) => self.step(-1, now),
            _ => {}
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, _ctx: &Ctx) -> AppAction {
        let now = Instant::now();
        if self.screen == Screen::Eq {
            self.eq_pointer(ev, now);
            return AppAction::None;
        }
        match (self.screen, ev) {
            (Screen::List, PointerEvent::Wheel { dy, .. }) => {
                let max = self.tracks.len().saturating_sub(visible());
                self.scroll = if dy > 0 { self.scroll.saturating_sub(1) } else { (self.scroll + 1).min(max) };
            }
            (Screen::List, PointerEvent::Down { x, y, button: 0, clicks }) => match self.scrollbar().hit(x, y) {
                Some(SbPart::Thumb) => self.sb_drag = true,
                Some(SbPart::Up | SbPart::PageUp) => self.scroll = self.scroll.saturating_sub(visible()),
                Some(SbPart::Down | SbPart::PageDown) => self.scroll = (self.scroll + visible()).min(self.tracks.len().saturating_sub(visible())),
                None => {
                    if let Some(i) = self.row_at(x, y) {
                        self.cursor = i;
                        if clicks >= 2 {
                            self.play(i, now);
                            if self.playing.is_some() {
                                self.screen = Screen::Now;
                            }
                        }
                    }
                }
            },
            (Screen::List, PointerEvent::Drag { y, .. }) if self.sb_drag => self.scroll = self.scrollbar().offset_for_y(y),
            (_, PointerEvent::Up { .. }) => self.sb_drag = false,
            (Screen::Now, PointerEvent::Down { x, y, button: 0, .. }) => {
                if contains(self.eq_badge(), x, y) {
                    self.open_eq();
                } else if contains(self.now_panel(), x, y) {
                    self.cycle_view();
                } else {
                    self.toggle_pause(now);
                }
            }
            (Screen::Now, PointerEvent::Wheel { dy, .. }) => self.step(if dy > 0 { -1 } else { 1 }, now),
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, _ctx: &Ctx) -> bool {
        let now = Instant::now();
        let changed = self.poll(now) | self.apply_eq(now) | self.poll_meta() | self.poll_loader() | self.sync_vis(now);
        let loading = self.loader.is_some() || (self.screen == Screen::Now && self.playing.as_ref().map_or(false, |p| !p.ready || self.meta.is_none()));
        let ticking = self.screen == Screen::Now && self.playing.as_ref().map(|p| !p.time.paused()).unwrap_or(false);
        changed || ticking || loading
    }

    fn draw(&mut self, c: &mut Canvas<Window>, _assets: &mut AssetStore, ctx: &Ctx) {
        ui::fill_rect(c, chrome::app_rect(), ctx.tm.get_color("home_bg"));
        match self.screen {
            Screen::List => self.draw_list(c, ctx),
            Screen::Now => self.draw_now(c, ctx),
            Screen::Eq => self.draw_eq(c, ctx),
        }
    }
}

/// Stands in for a tap that could not start.
struct NoSource;

impl Source for NoSource {
    fn window(&mut self, _: Duration) -> Option<Vec<f32>> {
        None
    }
}

// ------------------------------------------------------------ EQ screen

const PRESET_IDS: [&str; music_eq::PRESETS.len()] = ["preset0", "preset1", "preset2", "preset3", "preset4", "preset5"];
/// The preset row, then five slider columns and the response curve on the right.
const EQ_PRESET: (i32, i32, i32, i32) = (LIST_X, LIST_Y, 150, grid::ROW_H);
const EQ_COL_W: i32 = 30;
const EQ_VALUE_Y: i32 = LIST_Y + 20;
const EQ_TRACK_TOP: i32 = LIST_Y + 40;
const EQ_PX_PER_DB: i32 = 3;
const EQ_ZERO_Y: i32 = EQ_TRACK_TOP + music_eq::MAX_DB * EQ_PX_PER_DB;
const EQ_TRACK_BOTTOM: i32 = EQ_ZERO_Y + music_eq::MAX_DB * EQ_PX_PER_DB;
const EQ_LABEL_Y: i32 = EQ_TRACK_BOTTOM + 6;
const EQ_CURVE_X: i32 = LIST_X + 5 * EQ_COL_W + 8;
/// The curve spans +-15 dB, 20 Hz to 20 kHz.
const EQ_CURVE_DB: f64 = 15.0;

fn eq_preset_rect() -> Rect {
    Rect::new(EQ_PRESET.0, EQ_PRESET.1, EQ_PRESET.2 as u32, EQ_PRESET.3 as u32)
}

fn eq_col_rect(band: usize) -> Rect {
    Rect::new(LIST_X + band as i32 * EQ_COL_W, EQ_VALUE_Y, EQ_COL_W as u32, (EQ_LABEL_Y + grid::ROW_H - EQ_VALUE_Y) as u32)
}

fn eq_curve_rect() -> Rect {
    let right = grid::scroll_x() + grid::SCROLL_W;
    let extra = (grid::sh() - grid::SCREEN_H).max(0);
    Rect::new(EQ_CURVE_X, LIST_Y, (right - EQ_CURVE_X) as u32, (EQ_LABEL_Y - LIST_Y + extra) as u32)
}

impl Music {
    /// The panel on the left of Now Playing: the cover's square, or for the visualizer the whole
    /// height and whatever width a bigger window adds.
    fn now_panel(&self) -> Rect {
        let side = tags::COVER as i32 + 8;
        let top = grid::BAR_H + grid::MARGIN * 2;
        if self.view == View::Cover {
            return Rect::new(grid::MARGIN * 2, top, side as u32, side as u32);
        }
        let w = side + (grid::sw() - grid::SCREEN_W).max(0);
        let h = (grid::frame_bottom() - top).max(side);
        Rect::new(grid::MARGIN * 2, top, w as u32, h as u32)
    }

    /// The "EQ" button on Now Playing: in the text column, under the album line.
    fn eq_badge(&self) -> Rect {
        let x = self.now_panel().right() + grid::MARGIN * 3;
        Rect::new(x, grid::BAR_H + grid::MARGIN * 2 + 74, 84, 16)
    }

    /// Band bars or the waveform inside `r` (the panel), dark on the white panel like the cover.
    fn draw_vis(&self, c: &mut Canvas<Window>, ctx: &Ctx, r: Rect, ready: bool, tick: u32) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let (paper, dark) = tone::paper_ink(tm);
        let inner = Rect::new(r.x() + 6, r.y() + 6, r.width() - 12, r.height() - 12);
        if let Some(e) = &self.vis_error {
            let t = chrome::fit_text(fs, e, inner.width() as i32);
            fs.draw_text(c, inner.x(), inner.y(), &t, dark);
            return;
        }
        if !ready && self.wave.is_empty() {
            draw_barber(c, ctx, Rect::new(inner.x() + 2, inner.center().y() - 14, inner.width() - 4, 28), crate::launcher::i18n::tr("Starting..."), tick);
            return;
        }
        match self.view {
            View::Bars => {
                // Five columns of 3 px segments (1 px gaps), the band's name under each, a 2 px
                // peak mark that waits and falls.
                let label_h = 14;
                let area = Rect::new(inner.x(), inner.y(), inner.width(), inner.height() - label_h as u32);
                let gap = 4;
                let bw = (area.width() as i32 - gap * (music_eq::N as i32 - 1)) / music_eq::N as i32;
                let x0 = area.x() + (area.width() as i32 - (bw * music_eq::N as i32 + gap * (music_eq::N as i32 - 1))) / 2;
                let segs = (area.height() as i32 + 1) / 4;
                let bottom = area.y() + segs * 4 - 1;
                for (b, band) in music_eq::BANDS.iter().enumerate() {
                    let x = x0 + b as i32 * (bw + gap);
                    let lit = (Meter::frac(self.meter.level[b]) * segs as f32).round() as i32;
                    for sgm in 0..segs {
                        let y = bottom - sgm * 4 - 2;
                        let seg = Rect::new(x, y, bw as u32, 3);
                        if sgm < lit {
                            ui::fill_rect(c, seg, dark);
                        } else {
                            dither::fill_two_tone(c, seg, paper, dark, Pattern::Eighth);
                        }
                    }
                    let pk = (Meter::frac(self.meter.peak[b]) * segs as f32).round() as i32;
                    if pk > 0 {
                        let y = bottom - (pk - 1) * 4 - 2;
                        ui::fill_rect(c, Rect::new(x, y - 2, bw as u32, 1), dark);
                    }
                    let lw = fs.measure_text(band.label);
                    fs.draw_text(c, x + (bw - lw) / 2, widgets::text_origin(area.bottom() + 1, label_h), band.label, dark);
                }
            }
            View::Wave => {
                let cy = inner.center().y();
                let half = inner.height() as f32 / 2.0 - 1.0;
                c.set_draw_color(dark);
                for x in (inner.x()..inner.right()).step_by(3) {
                    let _ = c.draw_point((x, cy));
                }
                for (i, (lo, hi)) in music_vis::wave_columns(&self.wave, inner.width() as usize).into_iter().enumerate() {
                    let x = inner.x() + i as i32;
                    let (y0, y1) = (cy - (hi.clamp(-1.0, 1.0) * half).round() as i32, cy - (lo.clamp(-1.0, 1.0) * half).round() as i32);
                    let _ = c.draw_line(Point::new(x, y0), Point::new(x, y1));
                }
            }
            View::Cover => {}
        }
    }
}

fn db_for_y(y: i32) -> i32 {
    let d = EQ_ZERO_Y - y;
    (d + d.signum() * EQ_PX_PER_DB / 2) / EQ_PX_PER_DB
}

impl Music {
    fn eq_key(&mut self, k: Keycode, now: Instant) {
        let band = self.eq_focus.checked_sub(1);
        match k {
            Keycode::Escape | Keycode::E | Keycode::Return | Keycode::KpEnter => self.close_eq(),
            Keycode::Left => self.eq_focus = self.eq_focus.saturating_sub(1),
            Keycode::Right => self.eq_focus = (self.eq_focus + 1).min(music_eq::N),
            Keycode::Tab => self.eq_focus = (self.eq_focus + 1) % (music_eq::N + 1),
            Keycode::Up | Keycode::Down => {
                let up = k == Keycode::Up;
                match band {
                    Some(b) => self.nudge_band(b, if up { 1 } else { -1 }, now),
                    None => self.cycle_preset(if up { -1 } else { 1 }, now),
                }
            }
            Keycode::Backspace | Keycode::Delete | Keycode::Num0 => match band {
                Some(b) => {
                    let mut e = self.eq;
                    e.set(b, 0);
                    self.set_eq(e, now);
                }
                None => self.set_eq(Eq::default(), now),
            },
            Keycode::Space => self.toggle_pause(now),
            _ => {}
        }
    }

    fn eq_band_at(&self, x: i32, y: i32) -> Option<usize> {
        (0..music_eq::N).find(|&b| contains(eq_col_rect(b), x, y))
    }

    fn eq_pointer(&mut self, ev: PointerEvent, now: Instant) {
        match ev {
            PointerEvent::Down { x, y, button: 0, .. } => {
                let pr = eq_preset_rect();
                if contains(pr, x, y) {
                    self.eq_focus = 0;
                    self.cycle_preset(if x < pr.x() + pr.width() as i32 / 3 { -1 } else { 1 }, now);
                } else if let Some(b) = self.eq_band_at(x, y) {
                    self.eq_focus = b + 1;
                    if (EQ_TRACK_TOP - 6..=EQ_TRACK_BOTTOM + 6).contains(&y) {
                        let mut e = self.eq;
                        e.set(b, db_for_y(y));
                        self.set_eq(e, now);
                        self.eq_drag = Some(b);
                    }
                }
            }
            PointerEvent::Drag { y, .. } => {
                if let Some(b) = self.eq_drag {
                    let mut e = self.eq;
                    e.set(b, db_for_y(y));
                    self.set_eq(e, now);
                }
            }
            PointerEvent::Up { .. } => self.eq_drag = None,
            PointerEvent::Wheel { x, y, dy, .. } => {
                if contains(eq_preset_rect(), x, y) {
                    self.cycle_preset(if dy > 0 { -1 } else { 1 }, now);
                } else if let Some(b) = self.eq_band_at(x, y).or(self.eq_focus.checked_sub(1)) {
                    self.eq_focus = b + 1;
                    self.nudge_band(b, if dy > 0 { 1 } else { -1 }, now);
                }
            }
            _ => {}
        }
    }

    fn draw_eq(&mut self, c: &mut Canvas<Window>, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let faint = widgets::secondary_ink(tm);
        let line = tm.get_color("line");
        let well = tm.get_color("surface_sunken");
        let paper = tm.get_color("widget_face_active");
        let tr = crate::launcher::i18n::tr;

        // Preset row: < name >, a plate when focused.
        let pr = eq_preset_rect();
        let focus0 = self.eq_focus == 0;
        ui::fill_rect(c, pr, paper);
        if focus0 {
            widgets::draw_row_plate(c, tm, Rect::new(pr.x(), pr.y(), pr.width(), pr.height() + 1));
        }
        let fg = widgets::row_ink(tm, focus0, ink);
        let ty = widgets::text_origin(pr.y(), pr.height() as i32);
        let label = tr("Preset");
        fs.draw_text(c, pr.x() + 4, ty, label, if focus0 { fg } else { faint });
        let name = tr(self.eq.preset_name());
        let lx = pr.x() + 8 + fs.measure_text(label);
        let (ax, bx) = (lx + 2, pr.right() - 8);
        draw_arrow(c, ax, pr.y() + 4, -1, fg);
        draw_arrow(c, bx, pr.y() + 4, 1, fg);
        let nw = fs.measure_text(name);
        fs.draw_text(c, (ax + 6 + bx - 2 - nw) / 2, ty, name, fg);
        ui::draw_rect(c, pr, ink);

        // Sliders.
        for (b, band) in music_eq::BANDS.iter().enumerate() {
            let col = eq_col_rect(b);
            let cx = col.x() + EQ_COL_W / 2;
            let focus = self.eq_focus == b + 1;
            let g = self.eq.gains[b] as i32;
            let val = if g > 0 { format!("+{g}") } else { g.to_string() };
            let vw = fs.measure_text(&val);
            fs.draw_text(c, cx - vw / 2, widgets::text_origin(EQ_VALUE_Y, grid::ROW_H), &val, if g == 0 { faint } else { ink });
            let track = Rect::new(cx - 2, EQ_TRACK_TOP - 3, 5, (EQ_TRACK_BOTTOM - EQ_TRACK_TOP + 7) as u32);
            dither::fill_two_tone(c, track, well, line, Pattern::Half);
            ui::draw_rect(c, track, ink);
            // 0 dB and the +-6 / +-12 ticks.
            for db in [-12, -6, 0, 6, 12] {
                let y = EQ_ZERO_Y - db * EQ_PX_PER_DB;
                let w = if db == 0 { 5 } else { 3 };
                ui::fill_rect(c, Rect::new(cx - 4 - w, y, w as u32, 1), if db == 0 { ink } else { faint });
                ui::fill_rect(c, Rect::new(cx + 5, y, w as u32, 1), if db == 0 { ink } else { faint });
            }
            let thumb = Rect::new(cx - 6, EQ_ZERO_Y - g * EQ_PX_PER_DB - 3, 13, 7);
            ui::fill_rect(c, thumb, if focus { ink } else { paper });
            ui::draw_rect(c, thumb, ink);
            if !focus {
                ui::fill_rect(c, Rect::new(cx - 3, thumb.y() + 3, 7, 1), ink);
            }
            let lab = Rect::new(col.x() + 1, EQ_LABEL_Y, (EQ_COL_W - 2) as u32, grid::ROW_H as u32);
            if focus {
                widgets::draw_row_plate(c, tm, Rect::new(lab.x(), lab.y(), lab.width(), lab.height() + 1));
            }
            let lw = fs.measure_text(band.label);
            fs.draw_text(c, cx - lw / 2, widgets::text_origin(lab.y(), lab.height() as i32), band.label, widgets::row_ink(tm, focus, ink));
        }

        // Response curve: 20 Hz - 20 kHz across, +-15 dB up and down.
        let cr = eq_curve_rect();
        ui::fill_rect(c, cr, paper);
        let inner = Rect::new(cr.x() + 1, cr.y() + 1, cr.width() - 2, cr.height() - 2);
        let x_of = |f: f64| inner.x() + ((f / 20.0).log10() / 3.0 * (inner.width() as f64 - 1.0)).round() as i32;
        let cy = inner.y() + inner.height() as i32 / 2;
        let y_of = |db: f64| cy - (db / EQ_CURVE_DB * (inner.height() as f64 / 2.0 - 1.0)).round() as i32;
        c.set_draw_color(line);
        for f in [100.0, 1000.0, 10_000.0] {
            let x = x_of(f);
            for y in (inner.y()..inner.bottom()).step_by(2) {
                let _ = c.draw_point((x, y));
            }
        }
        for db in [-12.0, 12.0] {
            let y = y_of(db);
            for x in (inner.x()..inner.right()).step_by(4) {
                let _ = c.draw_point((x, y));
            }
        }
        c.set_draw_color(faint);
        for x in (inner.x()..inner.right()).step_by(2) {
            let _ = c.draw_point((x, cy));
        }
        let pts: Vec<Point> = (0..inner.width() as i32)
            .map(|i| {
                let f = 20.0 * 1000f64.powf(i as f64 / (inner.width() as f64 - 1.0));
                let db = self.eq.response_db(f).clamp(-EQ_CURVE_DB, EQ_CURVE_DB);
                Point::new(inner.x() + i, y_of(db))
            })
            .collect();
        c.set_draw_color(ink);
        let _ = c.draw_lines(&pts[..]);
        let thick: Vec<Point> = pts.iter().map(|p| Point::new(p.x(), p.y() + 1)).collect();
        let _ = c.draw_lines(&thick[..]);
        for (b, band) in music_eq::BANDS.iter().enumerate() {
            let (x, y) = (x_of(band.freq), y_of(self.eq.response_db(band.freq).clamp(-EQ_CURVE_DB, EQ_CURVE_DB)));
            let s = if self.eq_focus == b + 1 { 7 } else { 3 };
            let r = Rect::new(x - s / 2, y - s / 2 + 0, s as u32, s as u32);
            ui::fill_rect(c, r, if s > 3 { paper } else { ink });
            ui::draw_rect(c, r, ink);
        }
        ui::draw_rect(c, cr, ink);
        for (f, t) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
            let w = fs.measure_text(t);
            fs.draw_text(c, x_of(f) - w / 2, widgets::text_origin(cr.bottom(), grid::ROW_H), t, faint);
        }
        // Under the sliders: the pre-gain that keeps boosts from clipping.
        let head = self.eq.headroom_db();
        let note = if self.eq.flat() { tr("EQ off").to_string() } else if head > 0.05 { format!("{} -{head:.1} dB", tr("Pre-gain")) } else { tr("No boost").to_string() };
        fs.draw_text(c, LIST_X + 2, widgets::text_origin(EQ_LABEL_Y + grid::ROW_H - 2, grid::ROW_H), &chrome::fit_text(fs, &note, 5 * EQ_COL_W), faint);
    }
}

/// A small solid triangle 8 px high pointing left (`dir` < 0) or right, its tip at `x`.
fn draw_arrow(c: &mut Canvas<Window>, x: i32, y: i32, dir: i32, color: sdl2::pixels::Color) {
    c.set_draw_color(color);
    for i in 0..4 {
        let px = x - dir * i;
        let _ = c.draw_line(Point::new(px, y + 3 - i), Point::new(px, y + 4 + i));
    }
}

impl Music {
    fn draw_list(&mut self, c: &mut Canvas<Window>, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let paper = tm.get_color("widget_face_active");
        let faint = crate::launcher::widgets::secondary_ink(tm);
        let line = tm.get_color("line");
        let area = Rect::new(LIST_X, LIST_Y, list_w() as u32, list_h() as u32);
        ui::fill_rect(c, area, paper);
        if let Some((_, pr)) = &self.loader {
            let (tagged, found, _) = pr.snapshot();
            if self.tracks.is_empty() {
                let w = 200;
                let r = Rect::new(area.center().x() - w / 2, area.center().y() - 16, w as u32, 32);
                draw_progress(c, ctx, r, pr.fraction(), &crate::launcher::i18n::tr_s(&format!("Loading... {tagged} / {found}")));
            }
        } else if self.tracks.is_empty() {
            let msg = self.error.clone().unwrap_or_else(|| "No music in ~/Music".into());
            fs.draw_text(c, area.x() + 6, area.y() + 4, &crate::launcher::i18n::tr_s(&msg), faint);
        }
        let playing = self.playing.as_ref().map(|p| p.index);
        for (n, t) in self.tracks.iter().enumerate().skip(self.scroll).take(visible()) {
            let row = Rect::new(LIST_X, LIST_Y + (n - self.scroll) as i32 * grid::ROW_H, list_w() as u32, grid::ROW_H as u32);
            let sel = n == self.cursor;
            if sel {
                widgets::draw_row_plate(c, tm, row);
            }
            let fg = widgets::row_ink(tm, sel, ink);
            if playing == Some(n) {
                draw_play_icon(c, row.x() + 6, row.y() + 4, 8, fg);
            }
            let name = chrome::fit_text(fs, &t.title, list_w() - 24);
            fs.draw_text(c, row.x() + 20, widgets::text_origin(row.y(), row.height() as i32), &name, fg);
            widgets::draw_row_rule(c, tm, row);
        }
        ui::draw_rect(c, area, line);
        self.scrollbar().draw(c, tm, if self.sb_drag { Some(SbPart::Thumb) } else { None });
        // While the rest of the library is still loading: a small n/total badge.
        if let (Some((_, pr)), false) = (&self.loader, self.tracks.is_empty()) {
            let (tagged, found, _) = pr.snapshot();
            let text = format!("{tagged}/{found}");
            let w = fs.measure_text(&text) + 8;
            let r = Rect::new(area.right() - w - 2, area.bottom() - grid::ROW_H, w as u32, 14);
            ui::fill_rect(c, r, tm.get_color("surface_alt"));
            ui::draw_rect(c, r, ink);
            fs.draw_text(c, r.x() + 4, r.y(), &text, ink);
        }
    }

    fn draw_now(&mut self, c: &mut Canvas<Window>, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let faint = crate::launcher::widgets::secondary_ink(tm);
        let line = tm.get_color("line");
        let well = tm.get_color("surface_sunken");
        let now = Instant::now();
        let tick = (self.born.elapsed().as_millis() / 60) as u32;
        let Some(p) = &self.playing else {
            fs.draw_text(c, LIST_X + 6, LIST_Y + 4, crate::launcher::i18n::tr("Nothing playing"), faint);
            return;
        };
        let t = &self.tracks[p.index];

        // Cover on the left: a white panel (outline + hard shadow) with the picture in two
        // tones, dark on white on every theme.
        let (paper, dark) = tone::paper_ink(tm);
        let art = self.now_panel();
        let side = art.width() as i32;
        widgets::hard_shadow(c, art, ink);
        ui::fill_rect(c, art, paper);
        ui::draw_rect(c, art, ink);
        if self.view != View::Cover {
            self.draw_vis(c, ctx, art, p.ready, tick);
        } else if let Some(m) = &self.meta {
            if self.art_runs.is_none() {
                let (ox, oy) = (art.x() + (side - m.cover.w as i32) / 2, art.y() + (art.height() as i32 - m.cover.h as i32) / 2);
                self.art_runs = Some(tags::dark_runs(&m.cover, ox, oy));
            }
            if let Some(v) = &self.art_runs {
                c.set_draw_color(dark);
                let _ = c.fill_rects(&v[..]);
            }
        } else {
            // Tags first, then the cover; nobody knows how long either takes on the device.
            let label = if self.cover_stage.load(Ordering::Relaxed) == 0 { crate::launcher::i18n::tr("Reading tags...") } else { crate::launcher::i18n::tr("Cover...") };
            draw_barber(c, ctx, Rect::new(art.x() + 8, art.center().y() - 14, art.width() - 16, 28), label, tick);
        }

        // Text column on the right.
        let x = art.right() + grid::MARGIN * 3;
        let w = grid::scroll_x() + grid::SCROLL_W - grid::MARGIN - x;
        let state = if p.time.paused() { crate::launcher::i18n::tr("Paused") } else { crate::launcher::i18n::tr("Playing") };
        fs.draw_text(c, x, art.y(), &format!("{} / {}", p.index + 1, self.tracks.len()), faint);
        fs.draw_text(c, x + w - fs.measure_text(state), art.y(), state, ink);

        let tg = self.meta.as_ref().map(|m| &m.tags);
        let title = tg.and_then(|t| t.title.clone()).unwrap_or_else(|| t.title.clone());
        fs.draw_display_text(c, x, art.y() + 20, &chrome::fit_display_text(fs, &title, w), ink);
        if let Some(a) = tg.and_then(|t| t.artist.as_ref()) {
            fs.draw_text(c, x, art.y() + 40, &chrome::fit_text(fs, a, w), ink);
        }
        let album = tg.and_then(|t| t.album.clone()).or_else(|| (!t.folder.is_empty()).then(|| t.folder.clone()));
        if let Some(a) = album {
            fs.draw_text(c, x, art.y() + 54, &chrome::fit_text(fs, &a, w), faint);
        }

        let eqr = self.eq_badge();
        let eql = format!("EQ {}", crate::launcher::i18n::tr(self.eq.preset_name()));
        widgets::draw_button(c, fs, tm, eqr, &chrome::fit_text(fs, &eql, eqr.width() as i32 - 8), widgets::BtnState::default());

        // Play / pause glyph, progress bar and times at the bottom of the column.
        let gy = art.bottom() - 38;
        if p.time.paused() {
            ui::fill_rect(c, Rect::new(x, gy, 5, 14), ink);
            ui::fill_rect(c, Rect::new(x + 8, gy, 5, 14), ink);
        } else {
            draw_play_icon(c, x, gy, 14, ink);
        }
        if !p.ready {
            // ffplay takes a second or two before the first sound.
            draw_barber(c, ctx, Rect::new(x + 20, gy - 6, (w - 20) as u32, 28), crate::launcher::i18n::tr("Starting..."), tick);
            return;
        }
        let el = p.time.elapsed(now);
        let el = p.total.map(|t| el.min(t)).unwrap_or(el);
        let bar = Rect::new(x + 20, gy + 3, (w - 20) as u32, 8);
        dither::fill_two_tone(c, bar, well, line, Pattern::Half);
        if let Some(total) = p.total {
            let frac = (el.as_secs_f32() / total.as_secs_f32().max(0.001)).clamp(0.0, 1.0);
            let fw = ((bar.width() as i32 - 2) as f32 * frac).round() as u32;
            if fw > 0 {
                ui::fill_rect(c, Rect::new(bar.x() + 1, bar.y() + 1, fw, bar.height() - 2), ink);
            }
        }
        ui::draw_rect(c, bar, ink);
        fs.draw_text(c, bar.x(), bar.bottom() + 4, &fmt_time(el), ink);
        let tot = p.total.map(fmt_time).unwrap_or_else(|| "--:--".into());
        fs.draw_text(c, bar.right() - fs.measure_text(&tot), bar.bottom() + 4, &tot, faint);
        if let Some(e) = &self.error {
            fs.draw_text(c, x, art.bottom() - 10, &chrome::fit_text(fs, &crate::launcher::i18n::tr_s(e), w), faint);
        }
    }
}

/// A progress bar: a label above an outlined bar with a dithered fill.
fn draw_progress(c: &mut Canvas<Window>, ctx: &Ctx, r: Rect, frac: f32, label: &str) {
    let (tm, fs) = (ctx.tm, ctx.fs);
    let ink = tm.get_color("foreground");
    let line = tm.get_color("line");
    let well = tm.get_color("surface_sunken");
    let lw = fs.measure_text(label);
    fs.draw_text(c, r.x() + (r.width() as i32 - lw) / 2, r.y(), label, ink);
    let bar = Rect::new(r.x(), r.y() + 16, r.width(), 12);
    dither::fill_two_tone(c, bar, well, line, Pattern::Quarter);
    let fw = ((bar.width() as i32 - 2) as f32 * frac.clamp(0.0, 1.0)).round() as u32;
    if fw > 0 {
        let f = Rect::new(bar.x() + 1, bar.y() + 1, fw, bar.height() - 2);
        dither::fill_two_tone(c, f, well, ink, Pattern::ThreeQuarter);
    }
    ui::draw_rect(c, bar, ink);
}

/// An indeterminate "barber pole" bar (the same look as the launch screen): a label above an
/// outlined bar with diagonal stripes, 4 px on / 4 px off, sliding right as `tick` grows.
fn draw_barber(c: &mut Canvas<Window>, ctx: &Ctx, r: Rect, label: &str, tick: u32) {
    let (tm, fs) = (ctx.tm, ctx.fs);
    let ink = tm.get_color("foreground");
    let surface = tm.get_color("widget_face_active");
    let lw = fs.measure_text(label);
    fs.draw_text(c, r.x() + (r.width() as i32 - lw) / 2, r.y(), label, ink);
    let bar = Rect::new(r.x(), r.y() + 16, r.width(), 12);
    ui::draw_rect(c, bar, ink);
    let inner = Rect::new(bar.x() + 1, bar.y() + 1, bar.width() - 2, bar.height() - 2);
    ui::fill_rect(c, inner, surface);
    c.set_draw_color(ink);
    let shift = (tick / 2) as i32;
    for y in 0..inner.height() as i32 {
        let mut x = -(y + shift).rem_euclid(8) - 8;
        while x < inner.width() as i32 {
            for k in 0..4 {
                let px = x + k;
                if px >= 0 && px < inner.width() as i32 {
                    let _ = c.draw_point((inner.x() + px, inner.y() + y));
                }
            }
            x += 8;
        }
    }
}

/// A right-pointing triangle `s` high, built from vertical lines.
fn draw_play_icon(c: &mut Canvas<Window>, x: i32, y: i32, s: i32, color: sdl2::pixels::Color) {
    c.set_draw_color(color);
    let pts: Vec<Point> = (0..s)
        .flat_map(|i| {
            let col_h = ((s as f32) * (1.0 - i as f32 / s as f32)).round() as i32;
            (0..col_h).map(move |j| Point::new(x + i, y + (s - col_h) / 2 + j))
        })
        .collect();
    let _ = c.draw_points(&pts[..]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Default)]
    struct Log {
        spawned: Vec<PathBuf>,
        /// (start position, filter) of each spawn.
        opts: Vec<(Duration, Option<String>)>,
        events: Vec<&'static str>,
        done: Vec<Rc<Cell<bool>>>,
    }

    struct Mock(Rc<RefCell<Log>>);
    struct MockProc(Rc<RefCell<Log>>, Rc<Cell<bool>>);

    impl Proc for MockProc {
        fn exited(&mut self) -> bool {
            self.1.get()
        }
        fn pause(&mut self) {
            self.0.borrow_mut().events.push("pause");
        }
        fn resume(&mut self) {
            self.0.borrow_mut().events.push("resume");
        }
        fn stop(&mut self) {
            self.0.borrow_mut().events.push("stop");
            self.1.set(true);
        }
    }

    impl Audio for Mock {
        fn spawn(&self, path: &Path, from: Duration, af: Option<&str>) -> Result<Box<dyn Proc>, String> {
            let done = Rc::new(Cell::new(false));
            let mut l = self.0.borrow_mut();
            l.spawned.push(path.to_path_buf());
            l.opts.push((from, af.map(str::to_string)));
            l.done.push(done.clone());
            Ok(Box::new(MockProc(self.0.clone(), done)))
        }
        fn duration(&self, _: &Path) -> Option<Duration> {
            Some(Duration::from_secs(200))
        }
    }

    fn tr(n: &str) -> Track {
        Track { path: PathBuf::from(format!("/m/{n}.mp3")), title: n.into(), folder: String::new() }
    }

    fn app(n: usize) -> (Music, Rc<RefCell<Log>>) {
        let log = Rc::new(RefCell::new(Log::default()));
        let tracks = (0..n).map(|i| tr(&format!("t{i}"))).collect();
        (Music::new(Box::new(Mock(log.clone())), tracks), log)
    }

    #[test]
    fn timeline_excludes_paused_time() {
        let t0 = Instant::now();
        let mut t = Timeline::new(t0);
        assert_eq!(t.elapsed(t0 + Duration::from_secs(10)), Duration::from_secs(10));
        t.pause(t0 + Duration::from_secs(10));
        assert_eq!(t.elapsed(t0 + Duration::from_secs(50)), Duration::from_secs(10));
        t.resume(t0 + Duration::from_secs(50));
        assert_eq!(t.elapsed(t0 + Duration::from_secs(55)), Duration::from_secs(15));
    }

    #[test]
    fn formats_time_and_parses_probe_output() {
        assert_eq!(fmt_time(Duration::from_secs(65)), "1:05");
        assert_eq!(fmt_time(Duration::from_secs(3725)), "1:02:05");
        assert_eq!(parse_duration("213.456000\n"), Some(Duration::from_secs_f64(213.456)));
        assert_eq!(parse_duration("N/A"), None);
        assert_eq!(parse_duration("0"), None);
    }

    #[test]
    fn plays_pauses_and_skips() {
        let (mut m, log) = app(3);
        let t = Instant::now();
        m.play(0, t);
        m.toggle_pause(t);
        m.toggle_pause(t);
        m.step(1, t);
        assert_eq!(log.borrow().spawned, vec![PathBuf::from("/m/t0.mp3"), PathBuf::from("/m/t1.mp3")]);
        assert_eq!(log.borrow().events, ["pause", "resume", "stop"], "the old track is stopped before the next starts");
        m.step(-1, t);
        m.step(-1, t);
        assert_eq!(m.playing.as_ref().unwrap().index, 2, "previous wraps around");
    }

    #[test]
    fn a_finished_track_advances_and_the_last_one_stops() {
        let (mut m, log) = app(2);
        let t = Instant::now();
        m.play(0, t);
        log.borrow().done[0].set(true);
        assert!(m.poll(t));
        assert_eq!(m.playing.as_ref().unwrap().index, 1);
        assert_eq!(m.cursor, 1);
        log.borrow().done[1].set(true);
        m.poll(t);
        assert!(m.playing.is_none());
    }

    #[test]
    fn an_eq_change_restarts_the_track_where_it_was_once_settled() {
        let (mut m, log) = app(2);
        let t0 = Instant::now();
        m.play(0, t0);
        assert!(m.poll(t0), "the mock is ready at once");
        m.open_eq();
        m.eq_focus = 1;
        m.eq_key(Keycode::Up, t0 + Duration::from_secs(10));
        m.eq_key(Keycode::Up, t0 + Duration::from_secs(10));
        assert_eq!(m.eq.gains[0], 2);
        assert!(!m.apply_eq(t0 + Duration::from_millis(10_100)), "still settling");
        assert!(m.apply_eq(t0 + Duration::from_secs(11)));
        let l = log.borrow();
        assert_eq!(l.opts[0], (Duration::ZERO, None));
        assert_eq!(l.opts[1].0, Duration::from_secs(11));
        assert!(l.opts[1].1.as_deref().unwrap().contains("bass=f=60:t=q:w=0.707:g=2"));
        assert_eq!(l.events, ["stop"], "the old process is stopped");
        drop(l);
        assert!(!m.apply_eq(t0 + Duration::from_secs(12)), "nothing more to do");
        // The clock carries on from the restart position once the new process is ready.
        m.poll(t0 + Duration::from_secs(11));
        assert_eq!(m.playing.as_ref().unwrap().time.elapsed(t0 + Duration::from_secs(13)), Duration::from_secs(13));
        // A new track starts with the current filter, from the top.
        m.step(1, t0 + Duration::from_secs(14));
        assert_eq!(log.borrow().opts[2].0, Duration::ZERO);
        assert!(log.borrow().opts[2].1.is_some());
    }

    #[test]
    fn eq_keys_presets_and_saving() {
        let dir = std::env::temp_dir().join(format!("harbour-music-eq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"volume":40}"#).unwrap();
        let (mut m, log) = app(1);
        m.use_config(path.clone());
        let t = Instant::now();
        m.open_eq();
        assert!(m.screen == Screen::Eq);
        m.eq_focus = 0;
        m.eq_key(Keycode::Down, t);
        assert_eq!(m.eq.preset_name(), "Bass");
        m.eq_key(Keycode::Right, t);
        m.eq_key(Keycode::Delete, t);
        assert_eq!(m.eq.gains, [0, 3, 0, 0, 0]);
        assert_eq!(m.eq.preset_name(), "Custom");
        m.eq_key(Keycode::Left, t);
        m.eq_key(Keycode::Up, t);
        assert_eq!(m.eq.preset_name(), "Night", "from Custom, up goes to the last preset");
        // Dragging a slider: the thumb follows the pointer, a step per 3 px.
        m.eq_pointer(PointerEvent::Down { x: eq_col_rect(2).center().x(), y: EQ_ZERO_Y - 9, button: 0, clicks: 1 }, t);
        assert_eq!((m.eq_focus, m.eq.gains[2]), (3, 3));
        m.eq_pointer(PointerEvent::Drag { x: 0, y: EQ_TRACK_BOTTOM + 40, dx: 0, dy: 40 }, t);
        assert_eq!(m.eq.gains[2], -12);
        m.eq_pointer(PointerEvent::Up { x: 0, y: 0, button: 0 }, t);
        m.eq_key(Keycode::Escape, t);
        assert!(m.screen == Screen::List);
        let saved = Config::load(path.clone());
        assert_eq!(saved.get_str("music_eq", ""), m.eq.serialize());
        assert_eq!(saved.get_int("volume", 0), 40, "other settings stay");
        assert!(log.borrow().spawned.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    thread_local! {
        static TAPS: RefCell<Vec<(Duration, Option<String>)>> = RefCell::new(Vec::new());
    }

    /// A 1 kHz sine at half scale, wherever the clock is.
    struct SineSource;
    impl Source for SineSource {
        fn window(&mut self, _: Duration) -> Option<Vec<f32>> {
            Some((0..music_vis::WINDOW).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / music_vis::RATE as f32).sin()).collect())
        }
    }

    fn mock_tap(_: &Path, from: Duration, af: Option<&str>) -> Result<Box<dyn Source>, String> {
        TAPS.with(|t| t.borrow_mut().push((from, af.map(str::to_string))));
        Ok(Box::new(SineSource))
    }

    #[test]
    fn the_visualizer_follows_playback_and_the_equalizer() {
        TAPS.with(|t| t.borrow_mut().clear());
        let (mut m, _log) = app(2);
        m.tap_maker = mock_tap;
        let t0 = Instant::now();
        m.play(0, t0);
        m.screen = Screen::Now;
        m.poll(t0);
        assert!(!m.sync_vis(t0), "the cover needs no tap");
        assert!(m.tap.is_none());
        m.cycle_view();
        assert_eq!(m.view, View::Bars);
        assert!(m.sync_vis(t0 + Duration::from_secs(2)));
        assert!(m.sync_vis(t0 + Duration::from_millis(2050)));
        let lv = m.meter.level;
        assert!(lv[2] > -8.0 && lv.iter().enumerate().all(|(i, &v)| i == 2 || v < lv[2] - 20.0), "1 kHz lights the 1k bar: {lv:?}");
        // An equalizer change restarts playback, and the tap follows from the new position.
        m.set_eq(Eq { gains: music_eq::PRESETS[1].1 }, t0 + Duration::from_secs(3));
        m.apply_eq(t0 + Duration::from_secs(4));
        m.poll(t0 + Duration::from_secs(4));
        m.sync_vis(t0 + Duration::from_secs(5));
        let taps = TAPS.with(|t| t.borrow().clone());
        assert_eq!(taps.len(), 2);
        assert_eq!(taps[0], (Duration::from_secs(2), None));
        assert_eq!(taps[1].0, Duration::from_secs(5));
        assert!(taps[1].1.as_deref().unwrap().contains("bass="));
        // Off the Now screen the tap stops.
        m.screen = Screen::List;
        m.sync_vis(t0 + Duration::from_secs(6));
        assert!(m.tap.is_none());
        m.cycle_view();
        m.cycle_view();
        assert_eq!(m.view, View::Cover);
    }

    #[test]
    fn the_panel_grows_with_the_window_for_the_visualizer_only() {
        let (mut m, _) = app(1);
        let at = |m: &Music, size| grid::with_surface(size, || (m.now_panel(), m.eq_badge()));
        let (cover, _) = at(&m, (320, 170));
        assert_eq!((cover.width(), cover.height()), (144, 144));
        m.view = View::Wave;
        let (small, badge) = at(&m, (320, 170));
        assert_eq!(small, cover, "on the device it is the cover's square");
        assert_eq!(badge.x(), cover.right() + 12);
        let (big, badge) = at(&m, (520, 300));
        assert_eq!((big.width(), big.height()), (344, 274));
        assert_eq!(badge.x(), big.right() + 12);
    }

    #[test]
    fn dropping_the_app_stops_playback() {
        let (mut m, log) = app(1);
        m.play(0, Instant::now());
        drop(m);
        assert_eq!(log.borrow().events, ["stop"]);
    }

    #[test]
    fn scanning_finds_audio_recursively_and_sorted() {
        let base = std::env::temp_dir().join(format!("harbour_music_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("Album")).unwrap();
        std::fs::create_dir_all(base.join(".hidden")).unwrap();
        for f in ["b.MP3", "a.flac", "notes.txt", "Album/z.ogg", ".hidden/x.mp3"] {
            std::fs::write(base.join(f), b"x").unwrap();
        }
        let v = scan(&base);
        let names: Vec<_> = v.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(names, ["a", "z", "b"], "sorted by path; hidden and non-audio are skipped");
        assert_eq!(v[1].folder, "Album");
        let _ = std::fs::remove_dir_all(&base);
    }

    // ---- tags and covers ----

    fn bx(typ: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend(typ);
        v.extend(payload);
        v
    }

    fn item(typ: &[u8], kind: u32, data: &[u8]) -> Vec<u8> {
        let mut d = kind.to_be_bytes().to_vec();
        d.extend([0, 0, 0, 0]);
        d.extend(data);
        bx(typ, &bx(b"data", &d))
    }

    fn bmp_2x2() -> Vec<u8> {
        let mut f = vec![b'B', b'M'];
        f.extend((54u32 + 16).to_le_bytes());
        f.extend([0, 0, 0, 0]);
        f.extend(54u32.to_le_bytes());
        f.extend(40u32.to_le_bytes());
        f.extend(2u32.to_le_bytes());
        f.extend(2u32.to_le_bytes());
        f.extend([1, 0, 24, 0]);
        f.extend([0u8; 24]);
        f.extend([255, 255, 255, 0, 0, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 0]); // two padded rows
        f
    }

    fn m4a_bytes(cover: Option<&[u8]>) -> Vec<u8> {
        let mut ilst = item(&[0xA9, b'n', b'a', b'm'], 1, b"Computer Love");
        ilst.extend(item(&[0xA9, b'A', b'R', b'T'], 1, b"Kraftwerk"));
        ilst.extend(item(&[0xA9, b'a', b'l', b'b'], 1, b"Computer World"));
        if let Some(c) = cover {
            ilst.extend(item(b"covr", 14, c));
        }
        let mut meta = vec![0, 0, 0, 0];
        meta.extend(bx(b"ilst", &ilst));
        let moov = bx(b"moov", &bx(b"udta", &bx(b"meta", &meta)));
        let mut f = bx(b"ftyp", b"M4A \0\0\0\0");
        f.extend(bx(b"mdat", &[0u8; 100]));
        f.extend(moov); // moov after mdat, like many encoders write it
        f
    }

    #[test]
    fn m4a_tags_and_cover_are_read_even_when_moov_is_last() {
        let base = std::env::temp_dir().join(format!("harbour_tags_{}.m4a", std::process::id()));
        std::fs::write(&base, m4a_bytes(Some(b"IMG"))).unwrap();
        let e = tags::read_embedded(&base);
        let _ = std::fs::remove_file(&base);
        assert_eq!(e.tags.title.as_deref(), Some("Computer Love"));
        assert_eq!(e.tags.artist.as_deref(), Some("Kraftwerk"));
        assert_eq!(e.tags.album.as_deref(), Some("Computer World"));
        assert_eq!(e.cover.as_deref(), Some(&b"IMG"[..]));
        assert_eq!(tags::parse_moov(&[1, 2, 3]), tags::Embedded::default(), "garbage is not an error");
    }

    fn id3_frame(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend((body.len() as u32).to_be_bytes());
        v.extend([0, 0]);
        v.extend(body);
        v
    }

    #[test]
    fn id3_text_encodings_and_front_cover_preference() {
        let mut body = id3_frame(b"TIT2", b"\x00Caf\xe9");
        let mut u16t = vec![1u8, 0xFF, 0xFE];
        for u in "Düsseldorf".encode_utf16() {
            u16t.extend(u.to_le_bytes());
        }
        body.extend(id3_frame(b"TPE1", &u16t));
        body.extend(id3_frame(b"TALB", b"\x03Album\x00ignored"));
        body.extend(id3_frame(b"APIC", b"\x00image/jpeg\x00\x00back\x00BACK"));
        body.extend(id3_frame(b"APIC", b"\x00image/jpeg\x00\x03front\x00FRONT"));
        body.extend([0u8; 8]); // padding
        let e = tags::parse_id3(3, 0, &body);
        assert_eq!(e.tags.title.as_deref(), Some("Café"));
        assert_eq!(e.tags.artist.as_deref(), Some("Düsseldorf"));
        assert_eq!(e.tags.album.as_deref(), Some("Album"));
        assert_eq!(e.cover.as_deref(), Some(&b"FRONT"[..]));
    }

    #[test]
    fn flac_comments_and_picture() {
        let mut vc = 3u32.to_le_bytes().to_vec();
        vc.extend(b"abc");
        vc.extend(2u32.to_le_bytes());
        for c in ["title=Numbers", "ALBUM=Computer World"] {
            vc.extend((c.len() as u32).to_le_bytes());
            vc.extend(c.as_bytes());
        }
        let mut t = tags::Tags::default();
        tags::parse_vorbis_comments(&vc, &mut t);
        assert_eq!((t.title.as_deref(), t.album.as_deref(), t.artist.as_deref()), (Some("Numbers"), Some("Computer World"), None));
        let mut pic = 3u32.to_be_bytes().to_vec();
        pic.extend(9u32.to_be_bytes());
        pic.extend(b"image/png");
        pic.extend(0u32.to_be_bytes());
        pic.extend([0u8; 16]);
        pic.extend(4u32.to_be_bytes());
        pic.extend(b"DATA");
        assert_eq!(tags::parse_flac_picture(&pic), Some((3, b"DATA".to_vec())));
        assert_eq!(tags::parse_flac_picture(&pic[..20]), None, "truncated");
    }

    #[test]
    fn folder_cover_names_are_ranked() {
        let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(tags::pick_cover_file(&n(&["a.mp3", "Folder.jpg", "Cover.PNG"])), Some(2));
        assert_eq!(tags::pick_cover_file(&n(&["front.jpg", "folder.jpg"])), Some(1));
        assert_eq!(tags::pick_cover_file(&n(&["AlbumArt_{X}_Large.jpg"])), Some(0));
        assert_eq!(tags::pick_cover_file(&n(&["photo.jpg", "cover.gif", "notes.txt"])), None);
    }

    #[test]
    fn covers_come_from_the_file_then_the_cache_and_fall_back_to_a_placeholder() {
        let dir = std::env::temp_dir().join(format!("harbour_cover_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.m4a"), dir.join("b.m4a"));
        std::fs::write(&a, m4a_bytes(Some(&bmp_2x2()))).unwrap();
        std::fs::write(&b, m4a_bytes(None)).unwrap();
        let cache = tags::CoverCache::default();
        let m = tags::load_meta(&a, &cache, false, &Default::default(), None);
        assert_eq!(m.source, tags::Source::Embedded);
        assert_eq!(m.cover.w, tags::COVER, "upscaled to fit the box");
        assert!(m.cover.bits.iter().all(|&b| b <= 1), "two tones only");
        assert_eq!(tags::load_meta(&b, &cache, false, &Default::default(), None).source, tags::Source::Cached, "same album, same cover");
        let other = tags::load_meta(&dir.join("missing.ogg"), &tags::CoverCache::default(), false, &Default::default(), None);
        assert_eq!(other.source, tags::Source::Placeholder);
        assert_eq!((other.cover.w, other.cover.h), (tags::COVER, tags::COVER));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_art_has_the_right_size_and_contrast() {
        for p in [tags::placeholder_record(64), tags::fake_cover(64)] {
            assert_eq!(p.rgb.len(), 64 * 64 * 3);
            let (lo, hi) = (*p.rgb.iter().min().unwrap(), *p.rgb.iter().max().unwrap());
            assert!(hi - lo > 100, "needs contrast to dither well");
            let a = tags::art_from(&p);
            let lit = a.bits.iter().filter(|&&b| b == 1).count();
            assert!(a.bits.iter().all(|&b| b <= 1) && lit > 300 && lit < a.bits.len() - 300, "both tones are used: {lit}");
        }
    }

    #[test]
    fn art_runs_cover_exactly_the_dark_pixels() {
        let a = tags::Art { w: 4, h: 2, bits: vec![1, 0, 0, 1, 0, 0, 0, 0] };
        let runs = tags::dark_runs(&a, 10, 20);
        assert_eq!(runs, vec![Rect::new(11, 20, 2, 1), Rect::new(10, 21, 4, 1)]);
    }

    #[test]
    fn art_round_trips_through_the_packed_cache_format() {
        let dir = std::env::temp_dir().join(format!("harbour_art_{}", std::process::id()));
        let f = dir.join("x.cov");
        let a = tags::Art { w: 5, h: 3, bits: (0..15).map(|i| (i % 3 == 0) as u8).collect() };
        tags::write_cover(&f, &a);
        assert_eq!(std::fs::metadata(&f).unwrap().len(), 12 + 2, "15 pixels pack into 2 bytes");
        assert_eq!(tags::read_cover(&f), Some(a));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_cover_result_is_ignored() {
        let (mut m, _log) = app(2);
        let (tx, rx) = channel();
        m.rx = Some(rx);
        m.serial = 2;
        let meta = |s| tags::Meta { tags: tags::Tags::default(), cover: std::sync::Arc::new(tags::art_from(&tags::placeholder_record(8))), source: s };
        tx.send((1, meta(tags::Source::Placeholder))).unwrap();
        assert!(!m.poll_meta() && m.meta.is_none());
        tx.send((2, meta(tags::Source::Placeholder))).unwrap();
        assert!(m.poll_meta() && m.meta.is_some());
    }

    // ---- background library and caches ----

    #[test]
    fn cache_keys_change_with_path_mtime_and_size() {
        let k = library::cache_key("/m/a.m4a", 100, 5000);
        assert_eq!(k, library::cache_key("/m/a.m4a", 100, 5000), "stable");
        assert_eq!(k.len(), 16);
        for other in [library::cache_key("/m/b.m4a", 100, 5000), library::cache_key("/m/a.m4a", 101, 5000), library::cache_key("/m/a.m4a", 100, 5001)] {
            assert_ne!(k, other);
        }
    }

    #[test]
    fn the_tag_cache_round_trips_and_notices_changed_files() {
        let mut c = library::TagCache::default();
        let t = tags::Tags { title: Some("Numbers".into()), artist: None, album: Some("Computer\tWorld".into()) };
        c.insert("/m/a.m4a", 10, 99, t);
        let back = library::TagCache::parse(&c.serialize());
        let got = back.get("/m/a.m4a", 10, 99).unwrap();
        assert_eq!((got.title.as_deref(), got.artist, got.album.as_deref()), (Some("Numbers"), None, Some("Computer World")), "tabs are flattened");
        assert!(back.get("/m/a.m4a", 11, 99).is_none(), "newer file");
        assert!(back.get("/m/a.m4a", 10, 100).is_none(), "different size");
        assert!(back.get("/m/other", 10, 99).is_none());
        assert!(library::TagCache::parse("junk\nonly\tthree\tfields\n").get("junk", 0, 0).is_none());
    }

    fn drain(rx: &Receiver<library::LoadMsg>) -> (Vec<Track>, Vec<(PathBuf, tags::Tags)>) {
        let (mut found, mut tg) = (Vec::new(), Vec::new());
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("loader finished") {
                library::LoadMsg::Found(v) => found.extend(v),
                library::LoadMsg::Tags(v) => tg.extend(v),
                library::LoadMsg::Done => return (found, tg),
            }
        }
    }

    #[test]
    fn the_loader_counts_progress_and_the_second_run_uses_the_cache() {
        let base = std::env::temp_dir().join(format!("harbour_lib_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (music, cache) = (base.join("Music"), base.join("cache"));
        std::fs::create_dir_all(music.join("A")).unwrap();
        for f in ["b.m4a", "A/a.m4a", "A/c.mp3", "x.txt"] {
            std::fs::write(music.join(f), m4a_bytes(None)).unwrap();
        }
        let pr = Arc::new(library::Progress::default());
        assert_eq!(pr.fraction(), 0.0);
        let (found, tg) = drain(&library::spawn_loader(music.clone(), Some(cache.clone()), Duration::ZERO, pr.clone()));
        assert_eq!(found.len(), 3, "txt is not audio");
        assert_eq!(pr.snapshot(), (3, 3, true));
        assert_eq!(pr.hits.load(Ordering::Relaxed), 0, "cold start reads every file");
        assert_eq!(pr.fraction(), 1.0);
        let titled = tg.iter().filter(|(p, t)| p.extension().unwrap() == "m4a" && t.title.as_deref() == Some("Computer Love")).count();
        assert_eq!(titled, 2);
        assert!(cache.join("tags.tsv").is_file());

        let pr2 = Arc::new(library::Progress::default());
        let (_, tg2) = drain(&library::spawn_loader(music.clone(), Some(cache.clone()), Duration::from_millis(20), pr2.clone()));
        assert_eq!(pr2.hits.load(Ordering::Relaxed), 3, "warm start: no tag reads (the delay would have shown)");
        assert_eq!(tg2.len(), 3);

        // A changed file is read again.
        std::fs::write(music.join("b.m4a"), [m4a_bytes(None), vec![0u8; 8]].concat()).unwrap();
        let pr3 = Arc::new(library::Progress::default());
        drain(&library::spawn_loader(music.clone(), Some(cache.clone()), Duration::ZERO, pr3.clone()));
        assert_eq!(pr3.hits.load(Ordering::Relaxed), 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn progress_fraction_while_loading() {
        let p = library::Progress::default();
        p.found.store(10, Ordering::Relaxed);
        p.tagged.store(4, Ordering::Relaxed);
        assert!((p.fraction() - 0.4).abs() < 1e-6);
        p.tagged.store(10, Ordering::Relaxed);
        assert!(p.fraction() < 1.0, "all tagged but the walk may not be over");
        p.scanned.store(true, Ordering::Relaxed);
        assert_eq!(p.fraction(), 1.0);
    }

    #[test]
    fn late_tracks_are_inserted_sorted_without_moving_the_cursor_or_playing_track() {
        let (mut m, _log) = app(0);
        let t = |n: &str| Track { path: PathBuf::from(format!("/m/{n}.mp3")), title: n.into(), folder: String::new() };
        m.insert_sorted(vec![t("d"), t("b")]);
        m.cursor = 1; // "d"
        m.play(1, Instant::now());
        m.insert_sorted(vec![t("a"), t("c"), t("e"), t("b")]);
        let names: Vec<_> = m.tracks.iter().map(|x| x.title.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "d", "e"], "sorted, duplicate b ignored");
        assert_eq!(m.tracks[m.cursor].title, "d");
        assert_eq!(m.tracks[m.playing.as_ref().unwrap().index].title, "d");
        m.apply_tags(vec![(PathBuf::from("/m/c.mp3"), tags::Tags { title: Some("See".into()), ..Default::default() })]);
        assert_eq!(m.tracks[2].title, "See");
    }

    #[test]
    fn decoded_covers_are_cached_on_disk() {
        let dir = std::env::temp_dir().join(format!("harbour_covdisk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let track = dir.join("a.m4a");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&track, m4a_bytes(Some(&bmp_2x2()))).unwrap();
        let cache_dir = dir.join("cache");
        let stage = std::sync::atomic::AtomicUsize::new(0);
        let first = tags::load_meta(&track, &tags::CoverCache::default(), false, &stage, Some(&cache_dir));
        assert_eq!((first.source, stage.load(Ordering::Relaxed)), (tags::Source::Embedded, tags::STAGES));
        let file = tags::cover_cache_file(&cache_dir, &track).unwrap();
        assert!(file.is_file());
        let second = tags::load_meta(&track, &tags::CoverCache::default(), false, &stage, Some(&cache_dir));
        assert_eq!(second.source, tags::Source::Cached, "a fresh process finds it on disk");
        assert_eq!(second.cover.bits, first.cover.bits, "the packed 1-bit cover survives the disk");
        assert!(tags::read_cover(&dir.join("nope")).is_none());
        std::fs::write(&file, b"HCV2\x02\0\0\0\x02\0\0\0short").unwrap();
        assert!(tags::read_cover(&file).is_none(), "truncated cache files are ignored");
        let mut old = b"HCV1\x02\0\0\0\x02\0\0\0".to_vec();
        old.extend([0u8; 12]);
        std::fs::write(&file, old).unwrap();
        assert!(tags::read_cover(&file).is_none(), "the old RGB format is ignored (rebuilt)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- slow start ----

    struct SlowProc(Rc<Cell<bool>>);
    impl Proc for SlowProc {
        fn exited(&mut self) -> bool {
            false
        }
        fn ready(&mut self) -> bool {
            self.0.get()
        }
        fn pause(&mut self) {}
        fn resume(&mut self) {}
        fn stop(&mut self) {}
    }
    struct SlowAudio(Rc<Cell<bool>>);
    impl Audio for SlowAudio {
        fn spawn(&self, _: &Path, _: Duration, _: Option<&str>) -> Result<Box<dyn Proc>, String> {
            Ok(Box::new(SlowProc(self.0.clone())))
        }
        fn duration(&self, _: &Path) -> Option<Duration> {
            Some(Duration::from_secs(10))
        }
    }

    #[test]
    fn ffplay_status_lines_tell_when_audio_runs() {
        assert_eq!(status_time("   1.23 M-A:  0.000 fd=   0 aq=    0KB vq=    0KB sq=    0B f=0/0"), Some(1.23));
        assert_eq!(status_time("   0.00 M-A:   nan fd=   0 aq=    0KB"), Some(0.0));
        assert_eq!(status_time("    nan    M-A:   nan fd=   0 aq=    0KB"), None, "the clock is not running yet");
        assert_eq!(status_time("  12.50 M-V:  0.000 fd=   0"), Some(12.5));
        assert_eq!(status_time("Input #0, mov,mp4,m4a, from 'x.m4a':"), None);
        assert_eq!(status_time(""), None);
    }

    #[test]
    fn the_clock_starts_when_the_audio_does() {
        let ready = Rc::new(Cell::new(false));
        let mut m = Music::new(Box::new(SlowAudio(ready.clone())), vec![tr("a")]);
        let t0 = Instant::now();
        m.play(0, t0);
        assert!(!m.playing.as_ref().unwrap().ready);
        assert!(!m.poll(t0 + Duration::from_secs(2)), "still starting: nothing changes");
        assert!(!m.playing.as_ref().unwrap().ready);
        ready.set(true);
        assert!(m.poll(t0 + Duration::from_secs(3)), "ready: the screen changes");
        let p = m.playing.as_ref().unwrap();
        assert!(p.ready);
        assert_eq!(p.time.elapsed(t0 + Duration::from_secs(5)), Duration::from_secs(2), "counted from the first sound, not from the key press");
    }

    #[test]
    fn the_duration_arrives_later_and_stale_results_are_ignored() {
        let (mut m, _log) = app(1);
        m.play(0, Instant::now());
        let (tx, rx) = channel();
        m.dur_rx = Some(rx);
        m.serial = 5;
        tx.send((4, Some(Duration::from_secs(1)))).unwrap();
        assert!(!m.poll_meta());
        assert_eq!(m.playing.as_ref().unwrap().total, Some(Duration::from_secs(200)), "mock audio answers at once");
        let (tx, rx) = channel();
        m.dur_rx = Some(rx);
        tx.send((5, Some(Duration::from_secs(99)))).unwrap();
        assert!(m.poll_meta());
        assert_eq!(m.playing.as_ref().unwrap().total, Some(Duration::from_secs(99)));
    }

    #[test]
    fn help_rows_are_filled_in() {
        let (mut m, _) = app(2);
        crate::launcher::help::check_app_rows("Music list", &m.help());
        m.screen = Screen::Now;
        crate::launcher::help::check_app_rows("Music now playing", &m.help());
        m.screen = Screen::Eq;
        crate::launcher::help::check_app_rows("Music equalizer", &m.help());
    }
}
