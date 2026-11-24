//! Yacht's sounds: the Felucca-Engines (LOFI, ANALOG, TRIO, PHASE, WHEEL, NOISE and their presets),
//! through the small C host in `yacht_synth.c` (built by `build.rs`).

use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::Mutex;

#[repr(C)]
#[derive(Clone, Copy)]
struct YsNote {
    step: u8,
    pitch: u8,
    len: u8,
}

extern "C" {
    fn ys_rate() -> u32;
    fn ys_block() -> u32;
    fn ys_engine_count() -> u32;
    fn ys_engine_name(e: u32) -> *const c_char;
    fn ys_preset_count(e: u32) -> u32;
    fn ys_preset_name(e: u32, p: u32) -> *const c_char;
    fn ys_render(eng: u32, preset: u32, notes: *const YsNote, nnotes: u32, step_len: u32, steps: u32, out: *mut i16) -> i32;
}

/// The engines keep static state: one render at a time.
static LOCK: Mutex<()> = Mutex::new(());

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: the C side returns pointers to static, NUL-terminated strings (or "").
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// The engines' sample rate (44.1 kHz).
pub fn rate() -> u32 {
    unsafe { ys_rate() }
}

/// Samples per control block; a step is a whole number of them.
pub fn block() -> u32 {
    unsafe { ys_block() }
}

pub fn engine_count() -> usize {
    unsafe { ys_engine_count() as usize }
}

/// "LOFI", "ANALOG", ...
pub fn engine_name(e: usize) -> String {
    text(unsafe { ys_engine_name(e as u32) })
}

pub fn preset_count(e: usize) -> usize {
    unsafe { ys_preset_count(e as u32) as usize }
}

/// "PULSE LD", ...
pub fn preset_name(e: usize, p: usize) -> String {
    text(unsafe { ys_preset_name(e as u32, p as u32) })
}

/// Render a loop of `steps` steps of `step_len` samples (rounded down to whole blocks). Notes are
/// (step, pitch, length in steps). `None` for an engine or preset that does not exist.
pub fn render(engine: usize, preset: usize, notes: &[(u8, u8, u8)], step_len: usize, steps: usize) -> Option<Vec<i16>> {
    let blk = block() as usize;
    let step_len = (step_len / blk).max(1) * blk;
    let notes: Vec<YsNote> = notes.iter().take(256).map(|&(step, pitch, len)| YsNote { step, pitch, len }).collect();
    let mut out = vec![0i16; step_len * steps];
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: `out` holds steps * step_len samples, as ys_render writes; `notes` is a valid array of
    // `notes.len()` #[repr(C)] records.
    let r = unsafe { ys_render(engine as u32, preset as u32, notes.as_ptr(), notes.len() as u32, step_len as u32, steps as u32, out.as_mut_ptr()) };
    (r == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_and_presets_are_listed() {
        assert_eq!(rate(), 44_100);
        assert_eq!(engine_count(), 6);
        assert_eq!(engine_name(0), "LOFI");
        assert_eq!(engine_name(99), "");
        for e in 0..engine_count() {
            assert!(preset_count(e) > 0, "{}", engine_name(e));
            assert!(!preset_name(e, 0).is_empty());
        }
        assert_eq!(preset_name(0, 999), "");
    }

    #[test]
    fn every_preset_sounds_and_silence_is_silent() {
        let step = 5512;
        for e in 0..engine_count() {
            for p in 0..preset_count(e) {
                let a = render(e, p, &[(0, 60, 2), (4, 67, 1)], step, 16).unwrap();
                assert_eq!(a.len(), step / 32 * 32 * 16);
                let peak = a.iter().map(|s| (*s as i32).abs()).max().unwrap();
                assert!(peak > 500, "{} / {} is silent", engine_name(e), preset_name(e, p));
            }
            let quiet = render(e, 0, &[], step, 16).unwrap();
            assert!(quiet.iter().all(|s| s.unsigned_abs() < 64), "{} hums with no notes", engine_name(e));
        }
        assert!(render(99, 0, &[], step, 16).is_none());
        assert!(render(0, 999, &[], step, 16).is_none());
    }

    #[test]
    fn a_render_is_repeatable() {
        let n = [(0, 48, 1), (3, 55, 4), (15, 60, 1)];
        assert_eq!(render(1, 0, &n, 5512, 16), render(1, 0, &n, 5512, 16));
    }
}
