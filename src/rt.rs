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
