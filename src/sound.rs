//! Sounds the shell makes itself: short clicks for menus and windows, and loops an app hands over
//! (Yacht). One SDL audio device stays open for the whole run and mixes everything, so a click
//! starts within a few milliseconds instead of waiting for a player program to start. The web
//! build plays through the same device (the browser's audio); a browser starts sound only after
//! the first click or key.
//!
//! Without an audio device (headless runs, tests, no sound card) everything here quietly does
//! nothing and [`available`] is false.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// The sounds of the shell itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sfx {
    /// The menu highlight moved to another item.
    Tick,
    /// A menu opened.
    MenuOpen,
    /// A menu item was chosen.
    Select,
    /// A window (or a full-screen app) opened.
    Open,
    /// A window closed.
    Close,
    /// Something could not be done.
    Error,
}

/// How loud the shell's own sounds are (Settings > Sound > System sounds volume).
pub const LEVELS: [f32; 3] = [0.35, 0.6, 1.0];

struct Voice {
    id: u64,
    data: Arc<[f32]>,
    pos: usize,
    looping: bool,
    gain: f32,
}

/// Everything that is sounding, mixed by the audio callback.
#[derive(Default)]
pub struct Mixer {
    voices: Vec<Voice>,
    ui_on: bool,
    ui_gain: f32,
}

impl Mixer {
    pub fn new() -> Self {
        Mixer { voices: Vec::new(), ui_on: true, ui_gain: LEVELS[1] }
    }

    fn add(&mut self, id: u64, data: Arc<[f32]>, looping: bool, gain: f32) {
        if data.is_empty() {
            return;
        }
        // A burst of clicks (holding an arrow in a long menu) never piles up: keep the newest few.
        if !looping {
            let one_shots = self.voices.iter().filter(|v| !v.looping).count();
            if one_shots >= 4 {
                if let Some(i) = self.voices.iter().position(|v| !v.looping) {
                    self.voices.remove(i);
                }
            }
        }
        self.voices.push(Voice { id, data, pos: 0, looping, gain });
    }

    fn stop(&mut self, id: u64) {
        self.voices.retain(|v| v.id != id);
    }

    /// Mix into `out` (mono, -1..1). Finished one-shots drop out; loops wrap.
    pub fn fill(&mut self, out: &mut [f32]) {
        out.iter_mut().for_each(|s| *s = 0.0);
        for v in &mut self.voices {
            for s in out.iter_mut() {
                if v.pos >= v.data.len() {
                    if v.looping {
                        v.pos = 0;
                    } else {
                        break;
                    }
                }
                *s += v.data[v.pos] * v.gain;
                v.pos += 1;
            }
        }
        self.voices.retain(|v| v.looping || v.pos < v.data.len());
        out.iter_mut().for_each(|s| *s = s.clamp(-1.0, 1.0));
    }

    fn playing(&self, id: u64) -> bool {
        self.voices.iter().any(|v| v.id == id)
    }
}

struct Cb(Arc<Mutex<Mixer>>);

impl sdl2::audio::AudioCallback for Cb {
    type Channel = f32;
    fn callback(&mut self, out: &mut [f32]) {
        match self.0.lock() {
            Ok(mut m) => m.fill(out),
            Err(_) => out.iter_mut().for_each(|s| *s = 0.0),
        }
    }
}

static MIXER: OnceLock<Arc<Mutex<Mixer>>> = OnceLock::new();
static RATE: AtomicU32 = AtomicU32::new(0);
static NEXT: AtomicU64 = AtomicU64::new(1);
static CLICKS: OnceLock<Vec<(Sfx, Arc<[f32]>)>> = OnceLock::new();

/// The open audio device; keep it alive for as long as sound should play.
pub struct Device(#[allow(dead_code)] sdl2::audio::AudioDevice<Cb>);

/// Open the audio device (once, from the main thread). None when there is no sound to be had.
pub fn init(sdl: &sdl2::Sdl) -> Option<Device> {
    let audio = sdl.audio().map_err(|e| eprintln!("[sound] no audio: {e}")).ok()?;
    let want = sdl2::audio::AudioSpecDesired { freq: Some(44_100), channels: Some(1), samples: Some(1024) };
    let mixer = MIXER.get_or_init(|| Arc::new(Mutex::new(Mixer::new()))).clone();
    let dev = audio
        .open_playback(None, &want, |spec| {
            RATE.store(spec.freq as u32, Ordering::Relaxed);
            Cb(mixer)
        })
        .map_err(|e| eprintln!("[sound] no audio device: {e}"))
        .ok()?;
    dev.resume();
    eprintln!("[sound] {} Hz", rate());
    Some(Device(dev))
}

/// Is there an open audio device?
pub fn available() -> bool {
    RATE.load(Ordering::Relaxed) > 0 && MIXER.get().is_some()
}

/// The device's sample rate (0 without a device).
pub fn rate() -> u32 {
    RATE.load(Ordering::Relaxed)
}

/// Settings > Sound: are the shell's own sounds on, and how loud (an index into [`LEVELS`]).
pub fn set_ui(on: bool, level: usize) {
    if let Some(m) = MIXER.get() {
        if let Ok(mut m) = m.lock() {
            m.ui_on = on;
            m.ui_gain = LEVELS[level.min(LEVELS.len() - 1)];
        }
    }
}

/// Play one of the shell's sounds (when they are on).
pub fn play(sfx: Sfx) {
    if !available() {
        return;
    }
    let rate = rate();
    let clicks = CLICKS.get_or_init(|| {
        [Sfx::Tick, Sfx::MenuOpen, Sfx::Select, Sfx::Open, Sfx::Close, Sfx::Error]
            .into_iter()
            .map(|s| (s, Arc::from(synth(s, rate))))
            .collect()
    });
    let Some(data) = clicks.iter().find(|(s, _)| *s == sfx).map(|(_, d)| d.clone()) else { return };
    if let Some(m) = MIXER.get() {
        if let Ok(mut m) = m.lock() {
            if m.ui_on {
                let g = m.ui_gain;
                m.add(NEXT.fetch_add(1, Ordering::Relaxed), data, false, g);
            }
        }
    }
}

/// Play `samples` (mono, -1..1, at [`rate`]) over and over until [`stop`]. None without a device.
pub fn play_loop(samples: Vec<f32>) -> Option<u64> {
    if !available() {
        return None;
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let m = MIXER.get()?;
    m.lock().ok()?.add(id, Arc::from(samples), true, 1.0);
    Some(id)
}

/// Stop a loop from [`play_loop`].
pub fn stop(id: u64) {
    if let Some(m) = MIXER.get() {
        if let Ok(mut m) = m.lock() {
            m.stop(id);
        }
    }
}

/// Is the loop still sounding?
pub fn playing(id: u64) -> bool {
    MIXER.get().and_then(|m| m.lock().ok().map(|m| m.playing(id))).unwrap_or(false)
}

/// The samples of one of the shell's sounds at `rate`: short square-wave clicks and blips with a
/// fast decay, quiet enough to sit under whatever else is playing.
pub fn synth(sfx: Sfx, rate: u32) -> Vec<f32> {
    let r = rate.max(8000) as f32;
    // (start Hz, end Hz, length ms, level) per part; parts follow each other.
    let parts: &[(f32, f32, f32, f32)] = match sfx {
        Sfx::Tick => &[(3200.0, 3200.0, 4.0, 0.22)],
        Sfx::MenuOpen => &[(1800.0, 2600.0, 9.0, 0.18)],
        Sfx::Select => &[(2400.0, 1600.0, 14.0, 0.2)],
        Sfx::Open => &[(700.0, 1400.0, 45.0, 0.14)],
        Sfx::Close => &[(1300.0, 600.0, 45.0, 0.14)],
        Sfx::Error => &[(330.0, 330.0, 70.0, 0.16), (0.0, 0.0, 40.0, 0.0), (330.0, 330.0, 70.0, 0.16)],
    };
    let mut out = Vec::new();
    for &(f0, f1, ms, level) in parts {
        let n = (r * ms / 1000.0) as usize;
        let mut phase = 0f32;
        for k in 0..n {
            let t = k as f32 / n.max(1) as f32;
            let f = f0 + (f1 - f0) * t;
            phase = (phase + f / r).fract();
            let sq = if f <= 0.0 { 0.0 } else if phase < 0.5 { 1.0 } else { -1.0 };
            // A sharp attack and an exponential decay: a click rather than a beep.
            let attack = (k as f32 / (r * 0.0005)).min(1.0);
            let decay = (-4.0 * t).exp() * (1.0 - t);
            out.push(sq * level * attack * decay);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_are_short_quiet_and_end_silent() {
        for s in [Sfx::Tick, Sfx::MenuOpen, Sfx::Select, Sfx::Open, Sfx::Close, Sfx::Error] {
            let d = synth(s, 44_100);
            assert!(!d.is_empty(), "{s:?}");
            assert!(d.len() < 44_100 / 4, "{s:?} is under a quarter of a second");
            assert!(d.iter().all(|v| v.abs() <= 0.25), "{s:?} stays quiet");
            assert!(d.last().unwrap().abs() < 0.01, "{s:?} ends without a click");
        }
        assert!(synth(Sfx::Tick, 44_100).len() < synth(Sfx::Select, 44_100).len());
    }

    #[test]
    fn the_mixer_adds_voices_drops_finished_ones_and_wraps_loops() {
        let mut m = Mixer::new();
        m.add(1, Arc::from(vec![0.5f32; 3]), false, 1.0);
        m.add(2, Arc::from(vec![0.25f32, -0.25]), true, 1.0);
        let mut out = [0f32; 4];
        m.fill(&mut out);
        assert_eq!(out, [0.75, 0.25, 0.75, -0.25]);
        assert!(!m.playing(1) && m.playing(2), "the one-shot is done, the loop goes on");
        m.stop(2);
        m.fill(&mut out);
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn the_mixer_clamps_and_keeps_only_a_few_clicks() {
        let mut m = Mixer::new();
        for id in 0..10 {
            m.add(id, Arc::from(vec![0.4f32; 8]), false, 1.0);
        }
        assert_eq!(m.voices.len(), 4);
        let mut out = [0f32; 2];
        m.fill(&mut out);
        assert_eq!(out, [1.0, 1.0]);
    }
}
