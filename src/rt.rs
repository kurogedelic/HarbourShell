//! The few things that differ when Harbour runs as a web page (Emscripten) instead of a program.
//!
//! In the browser there are no threads and the page must get control back between frames, so
//! [`sleep`] yields to the browser (Asyncify) and [`spawn`] runs the work at once on the caller.

use std::time::Duration;

/// True in the web build.
pub const WEB: bool = cfg!(target_os = "emscripten");

#[cfg(target_os = "emscripten")]
extern "C" {
    fn emscripten_sleep(ms: u32);
}

/// Wait `d`. On the web this hands the frame back to the browser.
pub fn sleep(d: Duration) {
    #[cfg(target_os = "emscripten")]
    unsafe {
        emscripten_sleep(d.as_millis().min(u32::MAX as u128) as u32)
    }
    #[cfg(not(target_os = "emscripten"))]
    std::thread::sleep(d)
}

/// Run `f` in the background. The web build has no threads: `f` runs now, on the caller.
pub fn spawn<F: FnOnce() + Send + 'static>(f: F) {
    #[cfg(target_os = "emscripten")]
    f();
    #[cfg(not(target_os = "emscripten"))]
    {
        std::thread::spawn(f);
    }
}

// ---- Frozen mode (golden-image tests) --------------------------------------------------------
//
// `HARBOUR_FROZEN=1` makes a headless run reproducible: the wall clock stands still at one moment
// (`HARBOUR_FROZEN_TIME=<unix seconds>`, default 2026-03-14 09:26:53 UTC), the fake values of the
// mock backend stop moving, and clock-driven blinking is pinned. Normal runs never look at it.

use chrono::{DateTime, Local, TimeZone};
use std::sync::OnceLock;

/// The moment a frozen run lives in (Unix seconds).
const FROZEN_DEFAULT: i64 = 1_773_480_413;

/// True when `HARBOUR_FROZEN` is set (and not `0`). Read once.
pub fn frozen() -> bool {
    static F: OnceLock<bool> = OnceLock::new();
    *F.get_or_init(|| std::env::var("HARBOUR_FROZEN").map_or(false, |v| !v.is_empty() && v != "0"))
}

/// The frozen Unix time.
fn frozen_secs() -> i64 {
    static T: OnceLock<i64> = OnceLock::new();
    *T.get_or_init(|| std::env::var("HARBOUR_FROZEN_TIME").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(FROZEN_DEFAULT))
}

/// The current local date and time (the frozen moment when frozen). Use this, not `Local::now()`.
pub fn now() -> DateTime<Local> {
    if frozen() {
        if let Some(t) = Local.timestamp_opt(frozen_secs(), 0).single() {
            return t;
        }
    }
    Local::now()
}

/// The current Unix time in seconds (the frozen moment when frozen).
pub fn unix_now() -> i64 {
    now().timestamp()
}

/// Milliseconds into the current second, for blinking by the wall clock (0 when frozen).
pub fn subsec_millis() -> u32 {
    if frozen() {
        return 0;
    }
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_millis())
}
