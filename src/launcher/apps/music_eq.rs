//! Music's equalizer: five bands (a low shelf, three peaks, a high shelf) with presets.
//!
//! The sound is decoded and played by `ffplay`, so the filtering itself runs there: the
//! settings become an `-af` chain of ffmpeg's `bass`, `equalizer` and `treble` filters, which
//! are the same RBJ "Audio EQ Cookbook" biquads as [`Biquad`] here. The local biquads draw the
//! response curve on the EQ screen and work out the pre-gain that keeps a boost from clipping.

use std::f64::consts::PI;

use crate::system::config::Config;

/// Gains are whole decibels in this range.
pub const MAX_DB: i32 = 12;
/// Sample rate the curve and the headroom are worked out at.
pub const CURVE_RATE: f64 = 48_000.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    LowShelf,
    Peak,
    HighShelf,
}

#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub kind: Kind,
    pub freq: f64,
    pub q: f64,
    pub label: &'static str,
}

pub const BANDS: [Band; 5] = [
    Band { kind: Kind::LowShelf, freq: 60.0, q: std::f64::consts::FRAC_1_SQRT_2, label: "60" },
    Band { kind: Kind::Peak, freq: 250.0, q: 1.0, label: "250" },
    Band { kind: Kind::Peak, freq: 1000.0, q: 1.0, label: "1k" },
    Band { kind: Kind::Peak, freq: 3500.0, q: 1.0, label: "3.5k" },
    Band { kind: Kind::HighShelf, freq: 8000.0, q: std::f64::consts::FRAC_1_SQRT_2, label: "8k" },
];
pub const N: usize = BANDS.len();

/// Named settings. "Custom" is whatever does not match one of these.
pub const PRESETS: [(&str, [i8; N]); 6] = [
    ("Flat", [0, 0, 0, 0, 0]),
    ("Bass", [6, 3, 0, 0, 0]),
    ("Treble", [0, 0, 0, 3, 6]),
    ("Vocal", [-2, -1, 3, 3, 0]),
    ("Loud", [5, 1, -2, 1, 4]),
    ("Night", [-4, 0, 2, 1, -3]),
];

// ---------------------------------------------------------------- biquad

/// A second-order IIR section, normalised so a0 = 1 (direct form I state).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }

    /// RBJ peaking EQ: `gain_db` at `f`, bandwidth set by `q`.
    pub fn peak(rate: f64, f: f64, q: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w = 2.0 * PI * f / rate;
        let (sn, cs) = w.sin_cos();
        let al = sn / (2.0 * q);
        Self::norm(1.0 + al * a, -2.0 * cs, 1.0 - al * a, 1.0 + al / a, -2.0 * cs, 1.0 - al / a)
    }

    /// RBJ low shelf: `gain_db` below `f` (half of it at `f`).
    pub fn low_shelf(rate: f64, f: f64, q: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w = 2.0 * PI * f / rate;
        let (sn, cs) = w.sin_cos();
        let s = 2.0 * a.sqrt() * sn / (2.0 * q);
        Self::norm(
            a * ((a + 1.0) - (a - 1.0) * cs + s),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
            a * ((a + 1.0) - (a - 1.0) * cs - s),
            (a + 1.0) + (a - 1.0) * cs + s,
            -2.0 * ((a - 1.0) + (a + 1.0) * cs),
            (a + 1.0) + (a - 1.0) * cs - s,
        )
    }

    /// RBJ high shelf: `gain_db` above `f`.
    pub fn high_shelf(rate: f64, f: f64, q: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w = 2.0 * PI * f / rate;
        let (sn, cs) = w.sin_cos();
        let s = 2.0 * a.sqrt() * sn / (2.0 * q);
        Self::norm(
            a * ((a + 1.0) + (a - 1.0) * cs + s),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
            a * ((a + 1.0) + (a - 1.0) * cs - s),
            (a + 1.0) - (a - 1.0) * cs + s,
            2.0 * ((a - 1.0) - (a + 1.0) * cs),
            (a + 1.0) - (a - 1.0) * cs - s,
        )
    }

    pub fn for_band(b: &Band, rate: f64, gain_db: f64) -> Self {
        match b.kind {
            Kind::LowShelf => Self::low_shelf(rate, b.freq, b.q, gain_db),
            Kind::Peak => Self::peak(rate, b.freq, b.q, gain_db),
            Kind::HighShelf => Self::high_shelf(rate, b.freq, b.q, gain_db),
        }
    }

    /// Filter one sample.
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        (self.x2, self.x1, self.y2, self.y1) = (self.x1, x, self.y1, y);
        y
    }

    /// Gain in dB at frequency `f`: |H(e^jw)|.
    pub fn magnitude_db(&self, rate: f64, f: f64) -> f64 {
        let w = 2.0 * PI * f / rate;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let (nr, ni) = (self.b0 + self.b1 * c1 + self.b2 * c2, -(self.b1 * s1 + self.b2 * s2));
        let (dr, di) = (1.0 + self.a1 * c1 + self.a2 * c2, -(self.a1 * s1 + self.a2 * s2));
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10()
    }
}

// --------------------------------------------------------------- settings

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Eq {
    pub gains: [i8; N],
}

impl Eq {
    pub fn flat(&self) -> bool {
        self.gains.iter().all(|&g| g == 0)
    }

    /// The preset these gains are, if any.
    pub fn preset(&self) -> Option<usize> {
        PRESETS.iter().position(|(_, g)| *g == self.gains)
    }

    pub fn preset_name(&self) -> &'static str {
        self.preset().map_or("Custom", |i| PRESETS[i].0)
    }

    pub fn set(&mut self, band: usize, db: i32) {
        if let Some(g) = self.gains.get_mut(band) {
            *g = db.clamp(-MAX_DB, MAX_DB) as i8;
        }
    }

    pub fn filters(&self, rate: f64) -> Vec<Biquad> {
        BANDS.iter().zip(self.gains).map(|(b, g)| Biquad::for_band(b, rate, g as f64)).collect()
    }

    /// The whole chain's gain in dB at `f`.
    pub fn response_db(&self, f: f64) -> f64 {
        self.filters(CURVE_RATE).iter().map(|b| b.magnitude_db(CURVE_RATE, f)).sum()
    }

    /// The largest boost anywhere from 20 Hz to 20 kHz (0 when nothing is boosted): the
    /// chain is turned down by this much first so a full-scale track cannot clip.
    pub fn headroom_db(&self) -> f64 {
        let fs = self.filters(CURVE_RATE);
        (0..=240)
            .map(|i| 20.0 * 1000f64.powf(i as f64 / 240.0))
            .map(|f| fs.iter().map(|b| b.magnitude_db(CURVE_RATE, f)).sum::<f64>())
            .fold(0.0, f64::max)
    }

    /// ffplay's `-af` argument, or `None` when flat (no filtering at all).
    pub fn ffmpeg_filter(&self) -> Option<String> {
        if self.flat() {
            return None;
        }
        let mut v = Vec::new();
        let head = self.headroom_db();
        if head > 0.05 {
            v.push(format!("volume={:.1}dB", -head));
        }
        for (b, &g) in BANDS.iter().zip(&self.gains) {
            if g == 0 {
                continue;
            }
            let name = match b.kind {
                Kind::LowShelf => "bass",
                Kind::Peak => "equalizer",
                Kind::HighShelf => "treble",
            };
            v.push(format!("{name}=f={}:t=q:w={:.3}:g={g}", b.freq, b.q));
        }
        Some(v.join(","))
    }

    /// "g0,g1,g2,g3,g4"; anything malformed reads as `None`.
    pub fn parse(s: &str) -> Option<Eq> {
        let v: Vec<i32> = s.split(',').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
        let mut e = Eq::default();
        if v.len() != N {
            return None;
        }
        for (i, g) in v.into_iter().enumerate() {
            e.set(i, g);
        }
        Some(e)
    }

    pub fn serialize(&self) -> String {
        self.gains.iter().map(|g| g.to_string()).collect::<Vec<_>>().join(",")
    }

    pub fn load(cfg: &Config) -> Eq {
        Eq::parse(&cfg.get_str("music_eq", "")).unwrap_or_default()
    }

    pub fn store(&self, cfg: &mut Config) {
        cfg.set_str("music_eq", &self.serialize());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn a_peak_has_its_gain_at_the_centre_and_none_far_away() {
        for g in [-12.0, -3.0, 6.0, 12.0] {
            let b = Biquad::peak(48_000.0, 1000.0, 1.0, g);
            assert!(close(b.magnitude_db(48_000.0, 1000.0), g, 1e-9), "{g}");
            assert!(b.magnitude_db(48_000.0, 20.0).abs() < 0.1);
            assert!(b.magnitude_db(48_000.0, 20_000.0).abs() < 0.2);
        }
    }

    #[test]
    fn shelves_reach_their_gain_and_pass_half_at_the_corner() {
        let lo = Biquad::low_shelf(48_000.0, 60.0, std::f64::consts::FRAC_1_SQRT_2, 6.0);
        assert!(close(lo.magnitude_db(48_000.0, 1e-3), 6.0, 1e-6));
        assert!(close(lo.magnitude_db(48_000.0, 60.0), 3.0, 1e-6));
        assert!(lo.magnitude_db(48_000.0, 5000.0).abs() < 0.01);
        let hi = Biquad::high_shelf(48_000.0, 8000.0, std::f64::consts::FRAC_1_SQRT_2, -6.0);
        assert!(close(hi.magnitude_db(48_000.0, 23_999.0), -6.0, 0.05));
        assert!(close(hi.magnitude_db(48_000.0, 8000.0), -3.0, 1e-6));
        assert!(hi.magnitude_db(48_000.0, 100.0).abs() < 0.01);
    }

    #[test]
    fn zero_gain_is_an_identity_filter() {
        for b in BANDS {
            let mut f = Biquad::for_band(&b, 44_100.0, 0.0);
            for (i, x) in [1.0, -0.5, 0.25, 0.0, 0.75].into_iter().enumerate() {
                assert!(close(f.process(x), x, 1e-12), "band {} sample {i}", b.label);
            }
        }
        assert!(Eq::default().response_db(1234.0).abs() < 1e-9);
    }

    #[test]
    fn processing_a_sine_matches_the_magnitude_response() {
        // Steady-state amplitude of a filtered 1 kHz sine = the computed response.
        let rate = 48_000.0;
        let mut f = Biquad::peak(rate, 1000.0, 1.0, 6.0);
        let mut peak: f64 = 0.0;
        for n in 0..48_000 {
            let y = f.process((2.0 * PI * 1000.0 * n as f64 / rate).sin());
            if n > 24_000 {
                peak = peak.max(y.abs());
            }
        }
        assert!(close(20.0 * peak.log10(), 6.0, 0.01), "{}", 20.0 * peak.log10());
        // The impulse response is a fixed sequence: b0 first.
        let mut g = Biquad::peak(rate, 1000.0, 1.0, 6.0);
        assert!(close(g.process(1.0), g.b0, 0.0));
        assert!(close(g.process(0.0), g.b1 - g.a1 * g.b0, 1e-15));
    }

    #[test]
    fn presets_are_recognised_and_edits_make_custom() {
        let mut e = Eq { gains: PRESETS[1].1 };
        assert_eq!(e.preset_name(), "Bass");
        e.set(2, 1);
        assert_eq!(e.preset_name(), "Custom");
        e.set(0, 99);
        assert_eq!(e.gains[0], MAX_DB as i8);
        e.set(0, -99);
        assert_eq!(e.gains[0], -MAX_DB as i8);
        assert_eq!(Eq::default().preset_name(), "Flat");
    }

    #[test]
    fn the_filter_chain_for_ffplay() {
        assert_eq!(Eq::default().ffmpeg_filter(), None);
        let cut = Eq { gains: [0, 0, -3, 0, 0] };
        assert_eq!(cut.headroom_db(), 0.0);
        assert_eq!(cut.ffmpeg_filter().unwrap(), "equalizer=f=1000:t=q:w=1.000:g=-3");
        let bass = Eq { gains: PRESETS[1].1 };
        let h = bass.headroom_db();
        assert!(h > 5.0 && h < 9.0, "{h}");
        let s = bass.ffmpeg_filter().unwrap();
        assert!(s.starts_with(&format!("volume=-{h:.1}dB,bass=f=60:t=q:w=0.707:g=6,equalizer=f=250:t=q:w=1.000:g=3")), "{s}");
        assert!(!s.contains("treble"));
    }

    #[test]
    fn settings_round_trip_and_bad_text_is_flat() {
        let e = Eq { gains: [-12, 3, 0, 7, 12] };
        assert_eq!(Eq::parse(&e.serialize()), Some(e));
        assert_eq!(Eq::parse("1,2,3"), None);
        assert_eq!(Eq::parse("a,b,c,d,e"), None);
        assert_eq!(Eq::parse("40,0,0,0,-40").unwrap().gains, [12, 0, 0, 0, -12]);
        let dir = std::env::temp_dir().join(format!("harbour-eq-{}", std::process::id()));
        let path = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "{}").unwrap();
        let mut cfg = Config::load(path.clone());
        assert_eq!(Eq::load(&cfg), Eq::default());
        e.store(&mut cfg);
        cfg.save().unwrap();
        assert_eq!(Eq::load(&Config::load(path)), e);
        let _ = std::fs::remove_dir_all(dir);
    }
}
