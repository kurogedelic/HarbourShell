//! Music's visualizer: band bars (the equalizer's five bands) and a waveform.
//!
//! ffplay plays the sound and keeps its samples to itself, so a second, quiet `ffmpeg` decodes
//! the same file from the same position through the same equalizer chain, as mono 16-bit PCM at
//! [`RATE`] Hz on a pipe (a [`Tap`]). A reader thread keeps only a little ahead of the playback
//! clock (the pipe then blocks ffmpeg, so it costs next to nothing while paused); the screen takes
//! the [`WINDOW`] samples that end at the clock. The analysis below is plain arithmetic and tested.

use std::f32::consts::PI;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::music_eq::{BANDS, N};

/// Sample rate of the tap: enough for the top band (5.3 - 11 kHz).
pub const RATE: u32 = 22_050;
/// Samples per analysis (46 ms, 21.5 Hz bins).
pub const WINDOW: usize = 1024;
/// Bars show -48 .. 0 dB (0 dB = a full-scale sine).
pub const FLOOR_DB: f32 = -48.0;

// -------------------------------------------------------------- analysis

/// In-place radix-2 FFT (`re.len()` a power of two).
pub fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f32;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (ang * k as f32).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * c - im[b] * s, re[b] * s + im[b] * c);
                (re[b], im[b]) = (re[a] - tr, im[a] - ti);
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

/// Where the bars split: halfway (on a log scale) between neighbouring band frequencies, from
/// 20 Hz to the top of the tap.
pub fn band_edges(rate: f32) -> [f32; N + 1] {
    let mut e = [0.0; N + 1];
    e[0] = 20.0;
    for i in 1..N {
        e[i] = (BANDS[i - 1].freq * BANDS[i].freq).sqrt() as f32;
    }
    e[N] = rate / 2.0;
    e
}

/// Level of each band in dB (a full-scale sine inside a band reads 0 dB), clamped to [`FLOOR_DB`].
/// Hann window; each band is the sum of its bins' power.
pub fn band_db(samples: &[f32], rate: f32) -> [f32; N] {
    let n = samples.len().next_power_of_two().max(2);
    let mut re = vec![0.0f32; n];
    let mut im = vec![0.0f32; n];
    for (i, &s) in samples.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * PI * i as f32 / (samples.len().max(2) - 1) as f32).cos();
        re[i] = s * w;
    }
    fft(&mut re, &mut im);
    // A full-scale sine with a Hann window: peak bin power (n/4)^2, plus its two neighbours at a
    // quarter of that each -> 1.5 (n/4)^2 in all; scale so that reads 1.0.
    let full = 1.5 * (samples.len() as f32 / 4.0).powi(2);
    let edges = band_edges(rate);
    let bin_hz = rate / n as f32;
    let mut out = [FLOOR_DB; N];
    for (b, o) in out.iter_mut().enumerate() {
        let (lo, hi) = (edges[b], edges[b + 1]);
        let p: f32 = (1..n / 2)
            .filter(|&k| {
                let f = k as f32 * bin_hz;
                f >= lo && f < hi
            })
            .map(|k| re[k] * re[k] + im[k] * im[k])
            .sum();
        *o = (10.0 * (p / full).max(1e-12).log10()).clamp(FLOOR_DB, 0.0);
    }
    out
}

/// Bars that jump up at once and fall at [`Meter::FALL`] dB/s, with a peak mark that waits
/// [`Meter::HOLD`] and then falls too.
#[derive(Clone, Debug)]
pub struct Meter {
    pub level: [f32; N],
    pub peak: [f32; N],
    held: [f32; N],
}

impl Default for Meter {
    fn default() -> Self {
        Meter { level: [FLOOR_DB; N], peak: [FLOOR_DB; N], held: [0.0; N] }
    }
}

impl Meter {
    pub const FALL: f32 = 60.0;
    pub const HOLD: f32 = 0.6;

    pub fn update(&mut self, db: &[f32; N], dt: f32) {
        for i in 0..N {
            self.level[i] = db[i].max(self.level[i] - Self::FALL * dt).max(FLOOR_DB);
            if self.level[i] >= self.peak[i] {
                self.peak[i] = self.level[i];
                self.held[i] = 0.0;
            } else {
                self.held[i] += dt;
                if self.held[i] > Self::HOLD {
                    self.peak[i] = (self.peak[i] - Self::FALL * dt).max(self.level[i]);
                }
            }
        }
    }

    /// 0.0 (floor) .. 1.0 (0 dB).
    pub fn frac(db: f32) -> f32 {
        ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
    }
}

/// The waveform squeezed into `cols` columns: (min, max) of each column's samples.
pub fn wave_columns(samples: &[f32], cols: usize) -> Vec<(f32, f32)> {
    if samples.is_empty() || cols == 0 {
        return Vec::new();
    }
    (0..cols)
        .map(|c| {
            let a = c * samples.len() / cols;
            let b = ((c + 1) * samples.len() / cols).max(a + 1).min(samples.len());
            samples[a..b].iter().fold((f32::MAX, f32::MIN), |(lo, hi), &s| (lo.min(s), hi.max(s)))
        })
        .collect()
}

// ------------------------------------------------------------------- tap

/// What the reader thread has read so far.
#[derive(Default)]
struct Ring {
    buf: Vec<f32>,
    /// Samples read since the start (the buffer holds the last `buf.len()` of them).
    total: u64,
    ended: bool,
}

const KEEP: usize = RATE as usize * 2;

/// A sample source that follows playback; replaced by a mock in tests.
pub trait Source {
    /// The `WINDOW` samples ending `at` past the tap's start, if they have been read.
    fn window(&mut self, at: Duration) -> Option<Vec<f32>>;
}

/// `ffmpeg` decoding the track for the visualizer.
pub struct Tap {
    child: Child,
    ring: Arc<Mutex<Ring>>,
    want: Arc<AtomicU64>,
    quit: Arc<AtomicBool>,
}

impl Tap {
    pub fn spawn(path: &Path, from: Duration, af: Option<&str>) -> Result<Tap, String> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-nostdin", "-hide_banner", "-loglevel", "quiet"]);
        if !from.is_zero() {
            cmd.arg("-ss").arg(format!("{:.3}", from.as_secs_f64()));
        }
        cmd.arg("-i").arg(path).args(["-vn", "-sn"]);
        if let Some(af) = af {
            cmd.arg("-af").arg(af);
        }
        let mut child = cmd
            .args(["-ac", "1", "-ar", &RATE.to_string(), "-f", "s16le", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("ffmpeg: {e}"))?;
        let mut out = child.stdout.take().ok_or("ffmpeg: no pipe")?;
        let ring = Arc::new(Mutex::new(Ring::default()));
        let want = Arc::new(AtomicU64::new(0));
        let quit = Arc::new(AtomicBool::new(false));
        let (r, w, q) = (ring.clone(), want.clone(), quit.clone());
        std::thread::spawn(move || {
            use std::io::Read;
            let mut bytes = [0u8; 4096];
            let mut odd: Option<u8> = None;
            while !q.load(Ordering::Relaxed) {
                // Stay at most half a second ahead of the clock.
                if r.lock().map(|g| g.total).unwrap_or(0) > w.load(Ordering::Relaxed) + RATE as u64 / 2 {
                    std::thread::sleep(Duration::from_millis(15));
                    continue;
                }
                let n = match out.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let mut s = Vec::with_capacity(n / 2 + 1);
                let mut it = bytes[..n].iter().copied();
                if let Some(lo) = odd.take() {
                    if let Some(hi) = it.next() {
                        s.push(i16::from_le_bytes([lo, hi]) as f32 / 32768.0);
                    }
                }
                loop {
                    match (it.next(), it.next()) {
                        (Some(lo), Some(hi)) => s.push(i16::from_le_bytes([lo, hi]) as f32 / 32768.0),
                        (Some(lo), None) => {
                            odd = Some(lo);
                            break;
                        }
                        _ => break,
                    }
                }
                let Ok(mut g) = r.lock() else { break };
                g.total += s.len() as u64;
                g.buf.extend(s);
                if g.buf.len() > KEEP {
                    let cut = g.buf.len() - KEEP;
                    g.buf.drain(..cut);
                }
            }
            if let Ok(mut g) = r.lock() {
                g.ended = true;
            }
        });
        Ok(Tap { child, ring, want, quit })
    }
}

/// The `WINDOW` samples ending at sample `end` from a ring holding the last `buf.len()` of `total`.
fn slice_at(buf: &[f32], total: u64, end: u64) -> Option<Vec<f32>> {
    let end = end.min(total);
    let first = total - buf.len() as u64;
    if end < first + WINDOW as u64 {
        return None;
    }
    let e = (end - first) as usize;
    Some(buf[e - WINDOW..e].to_vec())
}

impl Source for Tap {
    fn window(&mut self, at: Duration) -> Option<Vec<f32>> {
        let end = (at.as_secs_f64() * RATE as f64) as u64;
        self.want.store(end, Ordering::Relaxed);
        let g = self.ring.lock().ok()?;
        slice_at(&g.buf, g.total, end)
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * PI * f * i as f32 / RATE as f32).sin()).collect()
    }

    #[test]
    fn fft_of_an_impulse_is_flat_and_of_a_bin_sine_is_one_spike() {
        let mut re = vec![0.0; 8];
        let mut im = vec![0.0; 8];
        re[0] = 1.0;
        fft(&mut re, &mut im);
        assert!(re.iter().all(|&v| (v - 1.0).abs() < 1e-6) && im.iter().all(|&v| v.abs() < 1e-6));
        let n = 64;
        let mut re: Vec<f32> = (0..n).map(|i| (2.0 * PI * 5.0 * i as f32 / n as f32).cos()).collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im);
        assert!((re[5] - 32.0).abs() < 1e-3 && (re[59] - 32.0).abs() < 1e-3);
        assert!((0..n).filter(|&k| k != 5 && k != 59).all(|k| re[k].abs() < 1e-3 && im[k].abs() < 1e-3));
    }

    #[test]
    fn a_sine_lights_its_own_band_at_its_level() {
        // One frequency inside each band, full scale and at -12 dB.
        for (b, f) in [60.0, 250.0, 1000.0, 3500.0, 8000.0].into_iter().enumerate() {
            for (amp, want) in [(1.0, 0.0), (0.25, -12.04)] {
                let db = band_db(&sine(f, amp, WINDOW), RATE as f32);
                assert!((db[b] - want).abs() < 1.6, "band {b} {f} Hz amp {amp}: {db:?}");
                for (o, &v) in db.iter().enumerate() {
                    if o != b {
                        assert!(v < db[b] - 20.0, "band {o} leaks for {f} Hz: {db:?}");
                    }
                }
            }
        }
        assert_eq!(band_db(&vec![0.0; WINDOW], RATE as f32), [FLOOR_DB; N]);
    }

    #[test]
    fn band_edges_sit_between_the_equalizer_bands() {
        let e = band_edges(RATE as f32);
        assert_eq!(e[0], 20.0);
        assert!((e[1] - 122.47).abs() < 0.01 && (e[2] - 500.0).abs() < 0.01);
        assert_eq!(e[N], 11_025.0);
        assert!(e.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn meter_jumps_up_falls_slowly_and_holds_its_peak() {
        let mut m = Meter::default();
        m.update(&[0.0, -10.0, -48.0, -48.0, -48.0], 0.01);
        assert_eq!(m.level[0], 0.0);
        m.update(&[FLOOR_DB; N], 0.1);
        assert!((m.level[0] + 6.0).abs() < 1e-4, "{}", m.level[0]);
        assert_eq!(m.peak[0], 0.0, "the peak waits");
        for _ in 0..6 {
            m.update(&[FLOOR_DB; N], 0.1);
        }
        assert!(m.peak[0] < 0.0 && m.peak[0] >= m.level[0]);
        assert_eq!(m.level[2], FLOOR_DB);
        assert_eq!(Meter::frac(FLOOR_DB), 0.0);
        assert_eq!(Meter::frac(-24.0), 0.5);
        assert_eq!(Meter::frac(3.0), 1.0);
    }

    #[test]
    fn wave_columns_keep_each_columns_extremes() {
        let s = [0.0, 1.0, -1.0, 0.5, 0.25, -0.25];
        assert_eq!(wave_columns(&s, 3), vec![(0.0, 1.0), (-1.0, 0.5), (-0.25, 0.25)]);
        assert_eq!(wave_columns(&s, 6)[1], (1.0, 1.0));
        assert!(wave_columns(&[], 4).is_empty());
    }

    #[test]
    fn the_window_ends_at_the_clock() {
        let buf: Vec<f32> = (0..3000).map(|i| i as f32).collect();
        // The ring holds samples 1000..4000.
        assert_eq!(slice_at(&buf, 4000, 1500), None, "not enough before it");
        let w = slice_at(&buf, 4000, 2500).unwrap();
        assert_eq!((w.len(), w[0], w[WINDOW - 1]), (WINDOW, (2500 - WINDOW - 1000) as f32, 1499.0));
        let w = slice_at(&buf, 4000, 9999).unwrap();
        assert_eq!(w[WINDOW - 1], 2999.0, "past what has been read: the newest");
    }
}
