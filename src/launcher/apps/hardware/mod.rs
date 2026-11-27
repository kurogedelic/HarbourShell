//! Devices: the wired parts (USB, Serial, I2C, GPIO, 1-Wire, Grove, MIDI) and the device tree.
//! The pages share their widgets and state with Interceptor (the wireless / network half), so this
//! app is the same core opened on the other [`Side`](super::interceptor::Side).

use crate::launcher::app::LauncherApp;

use super::interceptor::{make_side, Side};

pub fn make() -> Box<dyn LauncherApp> {
    make_side(Side::Hardware)
}
