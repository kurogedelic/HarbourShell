//! Direct evdev keyboard reader (Linux only).
//!
//! The reader never grabs the device (EVIOCGRAB), so external apps started by
//! the launcher can read the keyboard at the same time. While an app runs the
//! launcher stays silent (`suspended`) but this thread keeps watching for the
//! "hold Esc" exit: SIGINT after 1 s, SIGKILL after 2.5 s.

use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::keys::{translate_press, Input, Modifiers, KEY_ESC, KEY_HOME};
use super::Shared;

/// Hold Esc this long inside an external app: SIGINT, then SIGKILL.
const SIGINT_AFTER: Duration = Duration::from_millis(1000);
const SIGKILL_AFTER: Duration = Duration::from_millis(2500);

const EV_KEY: u16 = 1;

/// Find the keyboard event device.
pub fn find_keyboard() -> Option<String> {
    if let Ok(p) = std::env::var("HARBOUR_EVDEV") {
        return Some(p);
    }
    let by_path = "/dev/input/by-path/platform-3f804000.i2c-event";
    if std::path::Path::new(by_path).exists() {
        return Some(by_path.to_string());
    }
    for i in 0..16 {
        let name = std::fs::read_to_string(format!("/sys/class/input/event{i}/device/name"))
            .unwrap_or_default();
        if name.trim().eq_ignore_ascii_case("tca8418c") || name.to_lowercase().contains("keyboard")
        {
            return Some(format!("/dev/input/event{i}"));
        }
    }
    None
}

pub fn spawn(path: String, tx: Sender<Input>, shared: Arc<Shared>) -> io::Result<JoinHandle<()>> {
    let mut file = std::fs::File::open(&path)?;
    Ok(std::thread::Builder::new()
        .name("evdev".into())
        .spawn(move || {
            let fd = file.as_raw_fd();
            let mut mods = Modifiers::default();
            let mut hold_since: Option<Instant> = None;
            let mut sent_int = false;
            let mut sym = false;
            // Push-to-talk: Sym held on its own (no symbol typed meanwhile) for PTT_AFTER.
            let mut ptt = super::keys::PttDetector::default();
            let mut alt_clean = false;
            let mut alt_since = Instant::now();
            let mut buf = [0u8; 24 * 16];
            while !shared.quit.load(Ordering::Relaxed) {
                // Failsafe watchdog for a hung external app.
                let pgid = shared.child_pgid.load(Ordering::Relaxed);
                if pgid > 0 {
                    if let Some(t) = hold_since {
                        let held = t.elapsed();
                        if held >= SIGKILL_AFTER {
                            unsafe { libc::kill(-pgid, libc::SIGKILL) };
                        } else if held >= SIGINT_AFTER && !sent_int {
                            unsafe { libc::kill(-pgid, libc::SIGINT) };
                            sent_int = true;
                        }
                    }
                } else {
                    sent_int = false;
                }

                if !shared.suspended.load(Ordering::Relaxed) {
                    if let Some(down) = ptt.poll(Instant::now()) {
                        if tx.send(Input::Voice(down)).is_err() {
                            return;
                        }
                    }
                }
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let r = unsafe { libc::poll(&mut pfd, 1, 100) };
                if r <= 0 {
                    continue;
                }
                let n = match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                // struct input_event { timeval(16); u16 type; u16 code; i32 value; }
                for ev in buf[..n].chunks_exact(24) {
                    let etype = u16::from_ne_bytes([ev[16], ev[17]]);
                    let code = u16::from_ne_bytes([ev[18], ev[19]]);
                    let value = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
                    if etype != EV_KEY {
                        continue;
                    }
                    if code == super::keys::KEY_SYM {
                        sym = value != 0;
                        if value == 1 {
                            ptt.sym_down(Instant::now());
                        } else if value == 0 {
                            if let Some(down) = ptt.sym_up() {
                                if tx.send(Input::Voice(down)).is_err() {
                                    return;
                                }
                            }
                        }
                        continue;
                    }
                    if value == 1 {
                        ptt.other_key(); // another key was used with Sym: a symbol, not push-to-talk
                    }
                    // Alt tapped on its own (short, no other key meanwhile) = app menu.
                    // Sym, Shift and Fn are latching toggles in the keyboard hardware, so
                    // they cannot serve as a menu key.
                    if code == super::keys::KEY_LEFTALT {
                        if value == 1 {
                            alt_clean = true;
                            alt_since = Instant::now();
                        } else if value == 0 && alt_clean && alt_since.elapsed() < std::time::Duration::from_millis(500) && !shared.suspended.load(Ordering::Relaxed) {
                            if tx.send(Input::Menu).is_err() {
                                return;
                            }
                        }
                    } else if value == 1 {
                        alt_clean = false;
                    }
                    if sym {
                        if let Some(ch) = super::keys::sym_char(code) {
                            if shared.suspended.load(Ordering::Relaxed) {
                                continue;
                            }
                            let inputs = if value == 0 {
                                crate::keys::Keycode::from_i32(ch as i32).map(|key| Input::KeyUp { key }).into_iter().collect()
                            } else {
                                super::keys::sym_press(ch, value == 2, &mods)
                            };
                            for input in inputs {
                                if tx.send(input).is_err() {
                                    return;
                                }
                            }
                            continue;
                        }
                    }
                    mods.update(code, value != 0);
                    if code == KEY_ESC || code == KEY_HOME {
                        match value {
                            1 => {
                                hold_since = Some(Instant::now());
                                sent_int = false;
                            }
                            0 => {
                                hold_since = None;
                                sent_int = false;
                            }
                            _ => {}
                        }
                    }
                    if shared.suspended.load(Ordering::Relaxed) {
                        continue;
                    }
                    if value == 0 {
                        if let Some(key) = super::keys::linux_to_keycode(code) {
                            if tx.send(Input::KeyUp { key }).is_err() {
                                return;
                            }
                        }
                        continue;
                    }
                    for input in translate_press(code, value == 2, &mods) {
                        if tx.send(input).is_err() {
                            return;
                        }
                    }
                }
            }
        })?)
}
