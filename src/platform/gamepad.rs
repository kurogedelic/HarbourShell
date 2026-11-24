//! Gamepad input (Linux evdev): handhelds without a keyboard (Anbernic and kin) and USB / Bluetooth
//! pads. Buttons become [`Input::Pad`], the left stick [`Input::Stick`]; the host turns them into
//! keys, the pointer and the on-screen keyboard (`launcher::osk`).
//!
//! Which physical button is "A" differs between makers: by default the bottom face button is A
//! (the Xbox layout); `HARBOUR_PAD_LAYOUT=nintendo` takes the right one as A (and the bottom as B).

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::keys::{Input, Pad};
use super::Shared;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;
const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_NORTH: u16 = 0x133;
const BTN_WEST: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_THUMBL: u16 = 0x13d;
const BTN_DPAD_UP: u16 = 0x220;
const BTN_DPAD_DOWN: u16 = 0x221;
const BTN_DPAD_LEFT: u16 = 0x222;
const BTN_DPAD_RIGHT: u16 = 0x223;

/// `capabilities/key` lists 64-bit hex words, most significant first: a gamepad has BTN_SOUTH.
pub fn has_gamepad_button(caps: &str) -> bool {
    let words: Vec<u64> = caps.split_whitespace().filter_map(|w| u64::from_str_radix(w, 16).ok()).collect();
    let bit = BTN_SOUTH as usize;
    let (word, off) = (bit / 64, bit % 64);
    words.len() > word && words[words.len() - 1 - word] & (1u64 << off) != 0
}

/// Event nodes that look like gamepads right now.
pub fn find_pads() -> Vec<String> {
    (0..32)
        .filter(|i| has_gamepad_button(&std::fs::read_to_string(format!("/sys/class/input/event{i}/device/capabilities/key")).unwrap_or_default()))
        .map(|i| format!("/dev/input/event{i}"))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The bottom face button is A, the right one B (Xbox; most PC pads).
    Xbox,
    /// The right face button is A, the bottom one B (Nintendo; many handhelds' labels).
    Nintendo,
}

impl Layout {
    pub fn from_env() -> Layout {
        match std::env::var("HARBOUR_PAD_LAYOUT").as_deref() {
            Ok("nintendo") => Layout::Nintendo,
            _ => Layout::Xbox,
        }
    }
}

/// A stick axis' range (from the driver), to scale its value to -1000..=1000.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub min: i32,
    pub max: i32,
}

impl Range {
    fn scale(self, v: i32) -> i16 {
        let span = (self.max - self.min).max(1) as i64;
        (((v - self.min) as i64 * 2000 / span) - 1000).clamp(-1000, 1000) as i16
    }
}

/// One pad's events to inputs.
pub struct Mapper {
    layout: Layout,
    x: Range,
    y: Range,
    stick: (i16, i16),
    moved: bool,
    hat: (i32, i32),
    out: Vec<Input>,
}

impl Mapper {
    pub fn new(layout: Layout, x: Range, y: Range) -> Self {
        Self { layout, x, y, stick: (0, 0), moved: false, hat: (0, 0), out: Vec::new() }
    }

    fn button(&self, code: u16) -> Option<Pad> {
        let (south, east, north, west) = match self.layout {
            Layout::Xbox => (Pad::A, Pad::B, Pad::Y, Pad::X),
            Layout::Nintendo => (Pad::B, Pad::A, Pad::X, Pad::Y),
        };
        Some(match code {
            BTN_SOUTH => south,
            BTN_EAST => east,
            BTN_NORTH => north,
            BTN_WEST => west,
            BTN_TL => Pad::L,
            BTN_TR => Pad::R,
            BTN_SELECT => Pad::Select,
            BTN_START => Pad::Start,
            BTN_THUMBL => Pad::Stick,
            BTN_DPAD_UP => Pad::Up,
            BTN_DPAD_DOWN => Pad::Down,
            BTN_DPAD_LEFT => Pad::Left,
            BTN_DPAD_RIGHT => Pad::Right,
            _ => return None,
        })
    }

    /// A d-pad reported as a hat axis (-1, 0, 1): presses and releases of the arrow buttons.
    fn hat(&mut self, horizontal: bool, v: i32) {
        let (old, neg, pos) = if horizontal { (self.hat.0, Pad::Left, Pad::Right) } else { (self.hat.1, Pad::Up, Pad::Down) };
        let v = v.signum();
        if old == v {
            return;
        }
        if old != 0 {
            self.out.push(Input::Pad { pad: if old < 0 { neg } else { pos }, down: false });
        }
        if v != 0 {
            self.out.push(Input::Pad { pad: if v < 0 { neg } else { pos }, down: true });
        }
        if horizontal {
            self.hat.0 = v;
        } else {
            self.hat.1 = v;
        }
    }

    /// Feed one raw event; returns the inputs of a completed frame on `EV_SYN`.
    pub fn feed(&mut self, etype: u16, code: u16, value: i32) -> Vec<Input> {
        match etype {
            EV_KEY if value != 2 => {
                if let Some(pad) = self.button(code) {
                    self.out.push(Input::Pad { pad, down: value == 1 });
                }
            }
            EV_ABS => match code {
                ABS_X => {
                    self.stick.0 = self.x.scale(value);
                    self.moved = true;
                }
                ABS_Y => {
                    self.stick.1 = self.y.scale(value);
                    self.moved = true;
                }
                ABS_HAT0X => self.hat(true, value),
                ABS_HAT0Y => self.hat(false, value),
                _ => {}
            },
            EV_SYN => {
                if std::mem::take(&mut self.moved) {
                    self.out.push(Input::Stick { x: self.stick.0, y: self.stick.1 });
                }
                return std::mem::take(&mut self.out);
            }
            _ => {}
        }
        Vec::new()
    }
}

/// EVIOCGABS(axis): the axis' range from the driver (a centred -32768..32767 when it will not say).
fn abs_range(file: &File, axis: u16) -> Range {
    let mut info = [0i32; 6];
    let req: u64 = (2u64 << 30) | (24u64 << 16) | (0x45u64 << 8) | (0x40 + axis as u64);
    let r = unsafe { libc::ioctl(file.as_raw_fd(), req as _, info.as_mut_ptr()) };
    if r < 0 || info[2] <= info[1] {
        return Range { min: -32768, max: 32767 };
    }
    Range { min: info[1], max: info[2] }
}

struct Dev {
    file: File,
    map: Mapper,
}

pub fn spawn(tx: Sender<Input>, shared: Arc<Shared>) -> io::Result<JoinHandle<()>> {
    let layout = Layout::from_env();
    std::thread::Builder::new().name("gamepad".into()).spawn(move || {
        let mut devs: HashMap<String, Dev> = HashMap::new();
        let mut next_scan = Instant::now();
        let mut buf = [0u8; 24 * 32];
        while !shared.quit.load(Ordering::Relaxed) {
            if Instant::now() >= next_scan {
                next_scan = Instant::now() + Duration::from_secs(2);
                for path in find_pads() {
                    if !devs.contains_key(&path) {
                        if let Ok(file) = File::open(&path) {
                            eprintln!("[pad] connected: {path}");
                            let map = Mapper::new(layout, abs_range(&file, ABS_X), abs_range(&file, ABS_Y));
                            devs.insert(path, Dev { file, map });
                        }
                    }
                }
            }
            if devs.is_empty() {
                std::thread::sleep(Duration::from_millis(400));
                continue;
            }
            let paths: Vec<String> = devs.keys().cloned().collect();
            let mut pfds: Vec<libc::pollfd> = paths.iter().map(|p| libc::pollfd { fd: devs[p].file.as_raw_fd(), events: libc::POLLIN, revents: 0 }).collect();
            let r = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, 300) };
            if r <= 0 {
                continue;
            }
            for (i, pfd) in pfds.iter().enumerate() {
                if pfd.revents == 0 {
                    continue;
                }
                let path = &paths[i];
                if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    eprintln!("[pad] disconnected: {path}");
                    devs.remove(path);
                    continue;
                }
                let dev = devs.get_mut(path).unwrap();
                match dev.file.read(&mut buf) {
                    Ok(0) => {
                        devs.remove(path);
                    }
                    Ok(n) => {
                        for ev in buf[..n].chunks_exact(24) {
                            let etype = u16::from_ne_bytes([ev[16], ev[17]]);
                            let code = u16::from_ne_bytes([ev[18], ev[19]]);
                            let value = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
                            let inputs = dev.map.feed(etype, code, value);
                            if shared.suspended.load(Ordering::Relaxed) {
                                continue;
                            }
                            for input in inputs {
                                if tx.send(input).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        eprintln!("[pad] disconnected: {path}");
                        devs.remove(path);
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: Range = Range { min: -32768, max: 32767 };

    #[test]
    fn a_gamepad_has_btn_south() {
        // BTN_SOUTH = 0x130 = bit 304: word 4 from the right, bit 48.
        assert!(has_gamepad_button("1000000000000 0 0 0 0"));
        assert!(!has_gamepad_button("0 0 0 0 0"));
        assert!(!has_gamepad_button("70000 0 0 0 0"), "a mouse (BTN_LEFT) is not a pad");
    }

    #[test]
    fn buttons_follow_the_layout() {
        let mut x = Mapper::new(Layout::Xbox, FULL, FULL);
        x.feed(EV_KEY, BTN_SOUTH, 1);
        assert_eq!(x.feed(EV_SYN, 0, 0), vec![Input::Pad { pad: Pad::A, down: true }]);
        let mut n = Mapper::new(Layout::Nintendo, FULL, FULL);
        n.feed(EV_KEY, BTN_SOUTH, 1);
        n.feed(EV_KEY, BTN_EAST, 0);
        n.feed(EV_KEY, BTN_EAST, 2);
        assert_eq!(n.feed(EV_SYN, 0, 0), vec![Input::Pad { pad: Pad::B, down: true }, Input::Pad { pad: Pad::A, down: false }]);
    }

    #[test]
    fn the_stick_is_scaled_and_the_hat_is_the_dpad() {
        let mut m = Mapper::new(Layout::Xbox, Range { min: 0, max: 4096 }, Range { min: 0, max: 4096 });
        m.feed(EV_ABS, ABS_X, 4096);
        m.feed(EV_ABS, ABS_Y, 2048);
        assert_eq!(m.feed(EV_SYN, 0, 0), vec![Input::Stick { x: 1000, y: 0 }]);
        m.feed(EV_ABS, ABS_HAT0Y, -1);
        assert_eq!(m.feed(EV_SYN, 0, 0), vec![Input::Pad { pad: Pad::Up, down: true }]);
        m.feed(EV_ABS, ABS_HAT0Y, 1);
        assert_eq!(m.feed(EV_SYN, 0, 0), vec![Input::Pad { pad: Pad::Up, down: false }, Input::Pad { pad: Pad::Down, down: true }]);
        m.feed(EV_ABS, ABS_HAT0Y, 0);
        assert_eq!(m.feed(EV_SYN, 0, 0), vec![Input::Pad { pad: Pad::Down, down: false }]);
    }
}
