//! Linux evdev key codes → SDL keycodes / text, and the launcher input type.
//!
//! The Cardputer Zero keyboard (TCA8418 matrix) reports ordinary Linux key
//! codes. Its Fn layer produces arrows, Home/End, Page Up/Down, F-keys and the
//! media keys (volume, brightness) as real key codes, so no extra decoding is
//! needed here.

use crate::keys::{Keycode, Mod};

/// Hardware keys the launcher handles globally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKey {
    VolumeUp,
    VolumeDown,
    Mute,
    BrightnessUp,
    BrightnessDown,
}

/// The gamepad's buttons, as the keyboard and the host read them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pad {
    Up,
    Down,
    Left,
    Right,
    A,
    B,
    X,
    Y,
    L,
    R,
    Start,
    Select,
    /// The stick pressed in: a click where the pointer is.
    Stick,
}

impl Pad {
    /// Script and config names: up, down, left, right, a, b, x, y, l, r, start, select, stick.
    pub fn from_name(s: &str) -> Option<Pad> {
        Some(match s.to_ascii_lowercase().as_str() {
            "up" => Pad::Up,
            "down" => Pad::Down,
            "left" => Pad::Left,
            "right" => Pad::Right,
            "a" => Pad::A,
            "b" => Pad::B,
            "x" => Pad::X,
            "y" => Pad::Y,
            "l" => Pad::L,
            "r" => Pad::R,
            "start" => Pad::Start,
            "select" => Pad::Select,
            "stick" => Pad::Stick,
            _ => return None,
        })
    }
}

/// One input event delivered to the launcher main loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Key {
        key: Keycode,
        mods: Mod,
        repeat: bool,
    },
    Text(String),
    /// A key was released (needed to tell a short Tab press from a long one).
    KeyUp {
        key: Keycode,
    },
    Media(MediaKey),
    /// Push-to-talk: Sym held on its own for a while (`true`), released again (`false`).
    Voice(bool),
    /// Relative mouse movement in pixels.
    MouseMove {
        dx: i32,
        dy: i32,
    },
    /// 0 = left, 1 = right, 2 = middle.
    MouseButton {
        button: u8,
        down: bool,
    },
    /// Positive = wheel up (away from the user).
    MouseWheel {
        dy: i32,
    },
    /// Alt tapped on its own: open the app menu.
    Menu,
    /// A gamepad button went down or up.
    Pad {
        pad: Pad,
        down: bool,
    },
    /// The gamepad's stick: where it is held now, each axis -1000..=1000 (0, 0 = let go).
    Stick {
        x: i16,
        y: i16,
    },
    Quit,
}

// Linux input-event-codes used by the modifier / special handling.
pub const KEY_ESC: u16 = 1;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_HOME: u16 = 102;

/// F / X / Z / C double as Up / Down / Left / Right (they are the Fn-layer arrows
/// on the Cardputer keyboard). Use this on screens that do not take text input.
pub fn arrow_alias(key: Keycode) -> Keycode {
    match key {
        Keycode::F => Keycode::Up,
        Keycode::X => Keycode::Down,
        Keycode::Z => Keycode::Left,
        Keycode::C => Keycode::Right,
        k => k,
    }
}

pub fn media_key(code: u16) -> Option<MediaKey> {
    match code {
        113 => Some(MediaKey::Mute),
        114 => Some(MediaKey::VolumeDown),
        115 => Some(MediaKey::VolumeUp),
        224 => Some(MediaKey::BrightnessDown),
        225 => Some(MediaKey::BrightnessUp),
        _ => None,
    }
}

pub fn is_modifier(code: u16) -> bool {
    matches!(
        code,
        KEY_LEFTCTRL | KEY_RIGHTCTRL | KEY_LEFTSHIFT | KEY_RIGHTSHIFT | KEY_LEFTALT | KEY_RIGHTALT
    )
}

/// Linux key code → SDL keycode (US layout).
pub fn linux_to_keycode(code: u16) -> Option<Keycode> {
    Some(match code {
        1 => Keycode::Escape,
        2 => Keycode::Num1,
        3 => Keycode::Num2,
        4 => Keycode::Num3,
        5 => Keycode::Num4,
        6 => Keycode::Num5,
        7 => Keycode::Num6,
        8 => Keycode::Num7,
        9 => Keycode::Num8,
        10 => Keycode::Num9,
        11 => Keycode::Num0,
        12 => Keycode::Minus,
        13 => Keycode::Equals,
        14 => Keycode::Backspace,
        15 => Keycode::Tab,
        16 => Keycode::Q,
        17 => Keycode::W,
        18 => Keycode::E,
        19 => Keycode::R,
        20 => Keycode::T,
        21 => Keycode::Y,
        22 => Keycode::U,
        23 => Keycode::I,
        24 => Keycode::O,
        25 => Keycode::P,
        26 => Keycode::LeftBracket,
        27 => Keycode::RightBracket,
        28 => Keycode::Return,
        29 => Keycode::LCtrl,
        30 => Keycode::A,
        31 => Keycode::S,
        32 => Keycode::D,
        33 => Keycode::F,
        34 => Keycode::G,
        35 => Keycode::H,
        36 => Keycode::J,
        37 => Keycode::K,
        38 => Keycode::L,
        39 => Keycode::Semicolon,
        40 => Keycode::Quote,
        41 => Keycode::Backquote,
        42 => Keycode::LShift,
        43 => Keycode::Backslash,
        44 => Keycode::Z,
        45 => Keycode::X,
        46 => Keycode::C,
        47 => Keycode::V,
        48 => Keycode::B,
        49 => Keycode::N,
        50 => Keycode::M,
        51 => Keycode::Comma,
        52 => Keycode::Period,
        53 => Keycode::Slash,
        54 => Keycode::RShift,
        55 => Keycode::KpMultiply,
        56 => Keycode::LAlt,
        57 => Keycode::Space,
        59 => Keycode::F1,
        60 => Keycode::F2,
        61 => Keycode::F3,
        62 => Keycode::F4,
        63 => Keycode::F5,
        64 => Keycode::F6,
        65 => Keycode::F7,
        66 => Keycode::F8,
        67 => Keycode::F9,
        68 => Keycode::F10,
        71 => Keycode::Kp7,
        72 => Keycode::Kp8,
        73 => Keycode::Kp9,
        74 => Keycode::KpMinus,
        75 => Keycode::Kp4,
        76 => Keycode::Kp5,
        77 => Keycode::Kp6,
        78 => Keycode::KpPlus,
        79 => Keycode::Kp1,
        80 => Keycode::Kp2,
        81 => Keycode::Kp3,
        82 => Keycode::Kp0,
        83 => Keycode::KpPeriod,
        87 => Keycode::F11,
        88 => Keycode::F12,
        96 => Keycode::KpEnter,
        97 => Keycode::RCtrl,
        100 => Keycode::RAlt,
        102 => Keycode::Home,
        103 => Keycode::Up,
        104 => Keycode::PageUp,
        105 => Keycode::Left,
        106 => Keycode::Right,
        107 => Keycode::End,
        108 => Keycode::Down,
        109 => Keycode::PageDown,
        110 => Keycode::Insert,
        111 => Keycode::Delete,
        _ => return None,
    })
}

/// Printable character produced by a key (US layout), if any.
pub fn text_for(code: u16, shift: bool) -> Option<char> {
    let (lo, hi) = match code {
        2 => ('1', '!'),
        3 => ('2', '@'),
        4 => ('3', '#'),
        5 => ('4', '$'),
        6 => ('5', '%'),
        7 => ('6', '^'),
        8 => ('7', '&'),
        9 => ('8', '*'),
        10 => ('9', '('),
        11 => ('0', ')'),
        12 => ('-', '_'),
        13 => ('=', '+'),
        16 => ('q', 'Q'),
        17 => ('w', 'W'),
        18 => ('e', 'E'),
        19 => ('r', 'R'),
        20 => ('t', 'T'),
        21 => ('y', 'Y'),
        22 => ('u', 'U'),
        23 => ('i', 'I'),
        24 => ('o', 'O'),
        25 => ('p', 'P'),
        26 => ('[', '{'),
        27 => (']', '}'),
        30 => ('a', 'A'),
        31 => ('s', 'S'),
        32 => ('d', 'D'),
        33 => ('f', 'F'),
        34 => ('g', 'G'),
        35 => ('h', 'H'),
        36 => ('j', 'J'),
        37 => ('k', 'K'),
        38 => ('l', 'L'),
        39 => (';', ':'),
        40 => ('\'', '"'),
        41 => ('`', '~'),
        43 => ('\\', '|'),
        44 => ('z', 'Z'),
        45 => ('x', 'X'),
        46 => ('c', 'C'),
        47 => ('v', 'V'),
        48 => ('b', 'B'),
        49 => ('n', 'N'),
        50 => ('m', 'M'),
        51 => (',', '<'),
        52 => ('.', '>'),
        53 => ('/', '?'),
        55 => ('*', '*'),
        57 => (' ', ' '),
        71 => ('7', '7'),
        72 => ('8', '8'),
        73 => ('9', '9'),
        74 => ('-', '-'),
        75 => ('4', '4'),
        76 => ('5', '5'),
        77 => ('6', '6'),
        78 => ('+', '+'),
        79 => ('1', '1'),
        80 => ('2', '2'),
        81 => ('3', '3'),
        82 => ('0', '0'),
        83 => ('.', '.'),
        _ => return None,
    };
    Some(if shift { hi } else { lo })
}

/// Modifier state tracked from press/release events.
#[derive(Default, Clone, Copy)]
pub struct Modifiers {
    pub lshift: bool,
    pub rshift: bool,
    pub lctrl: bool,
    pub rctrl: bool,
    pub lalt: bool,
    pub ralt: bool,
}

impl Modifiers {
    pub fn update(&mut self, code: u16, down: bool) {
        match code {
            KEY_LEFTSHIFT => self.lshift = down,
            KEY_RIGHTSHIFT => self.rshift = down,
            KEY_LEFTCTRL => self.lctrl = down,
            KEY_RIGHTCTRL => self.rctrl = down,
            KEY_LEFTALT => self.lalt = down,
            KEY_RIGHTALT => self.ralt = down,
            _ => {}
        }
    }
    pub fn shift(&self) -> bool {
        self.lshift || self.rshift
    }
    pub fn ctrl(&self) -> bool {
        self.lctrl || self.rctrl
    }
    pub fn alt(&self) -> bool {
        self.lalt || self.ralt
    }
    pub fn sdl_mod(&self) -> Mod {
        let mut m = Mod::NOMOD;
        if self.lshift {
            m |= Mod::LSHIFTMOD;
        }
        if self.rshift {
            m |= Mod::RSHIFTMOD;
        }
        if self.lctrl {
            m |= Mod::LCTRLMOD;
        }
        if self.rctrl {
            m |= Mod::RCTRLMOD;
        }
        if self.lalt {
            m |= Mod::LALTMOD;
        }
        if self.ralt {
            m |= Mod::RALTMOD;
        }
        m
    }
}

/// Translate one evdev press (value 1) or repeat (value 2) into launcher inputs.
/// Modifier presses produce nothing. Ctrl/Alt chords produce no text.
/// The Sym key (a held modifier on the Cardputer keyboard).
pub const KEY_SYM: u16 = 383;

/// Character produced by `Sym + key` (the TCA8418 sym layer reports these codes
/// while Sym is held; see /usr/share/keymaps/tca8418_keypad_m5stack_keymap.map).
pub fn sym_char(code: u16) -> Option<char> {
    Some(match code {
        26 => '!',
        27 => '@',
        39 => '#',
        40 => '$',
        41 => '%',
        43 => '^',
        51 => '&',
        52 => '*',
        53 => '(',
        94 => ')',
        55 => '~',
        69 => '`',
        70 => '_',
        71 => '-',
        72 => '+',
        73 => '=',
        74 => '[',
        75 => ']',
        76 => '{',
        77 => '}',
        79 => ';',
        80 => ':',
        81 => '\'',
        82 => '"',
        83 => '<',
        85 => '>',
        86 => '\\',
        89 => '|',
        90 => ',',
        91 => '.',
        92 => '/',
        93 => '?',
        _ => return None,
    })
}

/// Key + text events for a symbol typed with Sym held.
pub fn sym_press(ch: char, repeat: bool, mods: &Modifiers) -> Vec<Input> {
    let mut out = Vec::new();
    if let Some(key) = Keycode::from_i32(ch as i32) {
        out.push(Input::Key { key, mods: mods.sdl_mod(), repeat });
    }
    if !mods.ctrl() && !mods.alt() {
        out.push(Input::Text(ch.to_string()));
    }
    out
}

pub fn translate_press(code: u16, repeat: bool, mods: &Modifiers) -> Vec<Input> {
    let mut out = Vec::new();
    if is_modifier(code) {
        return out;
    }
    if let Some(m) = media_key(code) {
        out.push(Input::Media(m));
        return out;
    }
    if let Some(key) = linux_to_keycode(code) {
        out.push(Input::Key {
            key,
            mods: mods.sdl_mod(),
            repeat,
        });
    }
    if !mods.ctrl() && !mods.alt() {
        if let Some(ch) = text_for(code, mods.shift()) {
            out.push(Input::Text(ch.to_string()));
        }
    }
    out
}

/// Push-to-talk from the Sym key: Sym held on its own (no other key used meanwhile) for
/// [`PttDetector::AFTER`] starts it; releasing Sym ends it. The clock is passed in. (If the keyboard
/// latches Sym instead of reporting a real release, the second press ends it: a toggle.)
#[derive(Debug, Default)]
pub struct PttDetector {
    since: Option<std::time::Instant>,
    clean: bool,
    active: bool,
}

impl PttDetector {
    pub const AFTER: std::time::Duration = std::time::Duration::from_millis(400);

    pub fn sym_down(&mut self, now: std::time::Instant) {
        self.since = Some(now);
        self.clean = true;
    }

    /// Any other key went down: this Sym press is for typing a symbol, not for talking.
    pub fn other_key(&mut self) {
        self.clean = false;
    }

    /// Sym came up. `Some(false)` when that ends a push-to-talk.
    pub fn sym_up(&mut self) -> Option<bool> {
        self.since = None;
        self.clean = false;
        std::mem::take(&mut self.active).then_some(false)
    }

    /// Call often. `Some(true)` once, when the hold is long enough.
    pub fn poll(&mut self, now: std::time::Instant) -> Option<bool> {
        match self.since {
            Some(t) if self.clean && !self.active && now.saturating_duration_since(t) >= Self::AFTER => {
                self.active = true;
                Some(true)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod ptt_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_long_clean_hold_starts_and_the_release_ends() {
        let t0 = Instant::now();
        let mut p = PttDetector::default();
        assert_eq!(p.poll(t0), None);
        p.sym_down(t0);
        assert_eq!(p.poll(t0 + Duration::from_millis(399)), None);
        assert_eq!(p.poll(t0 + Duration::from_millis(400)), Some(true));
        assert_eq!(p.poll(t0 + Duration::from_millis(900)), None, "starts only once");
        assert_eq!(p.sym_up(), Some(false));
        assert_eq!(p.sym_up(), None);
    }

    #[test]
    fn typing_a_symbol_with_sym_never_starts_it() {
        let t0 = Instant::now();
        let mut p = PttDetector::default();
        p.sym_down(t0);
        p.other_key(); // e.g. Sym + 1 = "!"
        assert_eq!(p.poll(t0 + Duration::from_secs(2)), None);
        assert_eq!(p.sym_up(), None);
        // A short tap is nothing either, and the next hold starts fresh.
        p.sym_down(t0 + Duration::from_secs(3));
        assert_eq!(p.sym_up(), None);
        p.sym_down(t0 + Duration::from_secs(4));
        assert_eq!(p.poll(t0 + Duration::from_millis(4500)), Some(true));
    }
}

/// Double tap of one key (the Agent's Tab Tab). Feed it every *completed short tap* (key up after
/// less than the long-press time); it says whether this tap is the second of a pair. The clock is
/// passed in, so tests need no sleeping.
#[derive(Debug, Default)]
pub struct DoubleTap {
    last: Option<std::time::Instant>,
}

impl DoubleTap {
    /// Two taps closer than this (release to release) are a double tap.
    pub const WINDOW: std::time::Duration = std::time::Duration::from_millis(300);

    pub fn tap(&mut self, now: std::time::Instant) -> bool {
        match self.last.take() {
            Some(prev) if now.saturating_duration_since(prev) <= Self::WINDOW => true,
            _ => {
                self.last = Some(now);
                false
            }
        }
    }

    /// Forget a half-finished pair (a long press, another key in between, the screen locked).
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// True while a first tap is waiting for its partner.
    pub fn pending(&self, now: std::time::Instant) -> bool {
        self.last.map_or(false, |p| now.saturating_duration_since(p) <= Self::WINDOW)
    }
}

/// A key with a "hold it" action (Esc long = leave, Tab long = lock): remembers when it went down
/// and fires the long action once. The clock is passed in.
#[derive(Debug, Default, Clone, Copy)]
pub struct HoldKey {
    down: Option<std::time::Instant>,
    fired: bool,
}

impl HoldKey {
    /// Esc held this long leaves the app in front.
    pub const ESC_LONG: std::time::Duration = std::time::Duration::from_millis(900);
    /// Tab held this long locks / unlocks.
    pub const TAB_LONG: std::time::Duration = std::time::Duration::from_millis(1000);
    /// The hold gauge shows after this long (a normal press never flashes it).
    pub const GAUGE_AFTER: std::time::Duration = std::time::Duration::from_millis(250);

    pub fn press(&mut self, now: std::time::Instant) {
        self.down = Some(now);
        self.fired = false;
    }

    /// Forget the press (it became part of a chord).
    pub fn cancel(&mut self) {
        self.down = None;
    }

    /// Key up. True for a short press (released before the long action fired).
    pub fn release(&mut self) -> bool {
        self.down.take().is_some() && !self.fired
    }

    /// Still held and the long action not fired yet.
    pub fn active(&self) -> bool {
        self.down.is_some() && !self.fired
    }

    /// Held for `after` and not fired yet: fire (true once). Call only while the action is allowed.
    pub fn poll(&mut self, now: std::time::Instant, after: std::time::Duration) -> bool {
        match self.down {
            Some(t) if !self.fired && now.saturating_duration_since(t) >= after => {
                self.fired = true;
                true
            }
            _ => false,
        }
    }

    /// Seconds held, for the gauge (None before [`Self::GAUGE_AFTER`] or once fired).
    pub fn gauge(&self, now: std::time::Instant) -> Option<f32> {
        let held = now.saturating_duration_since(self.down?);
        (!self.fired && held > Self::GAUGE_AFTER).then(|| held.as_secs_f32())
    }
}

/// What a Tab release meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabUp {
    /// Not a tap (a long press that locked, a chord, or no press seen).
    None,
    /// A short tap (the first of a possible pair, or a tap where the gesture is off).
    Single,
    /// The second short tap within [`DoubleTap::WINDOW`]: the Agent gesture.
    Double,
}

/// The host's Tab: tap, double tap (Agent), long press (lock), and Ctrl/Alt+Tab chords that are none
/// of those. Any other key between two taps breaks the pair.
#[derive(Debug, Default)]
pub struct TabGesture {
    pub hold: HoldKey,
    dt: DoubleTap,
}

impl TabGesture {
    /// Tab went down. `chord`: Ctrl or Alt held (menu / Agent chords).
    pub fn down(&mut self, now: std::time::Instant, chord: bool) {
        if chord {
            self.hold.cancel();
            self.dt.reset();
        } else {
            self.hold.press(now);
        }
    }

    /// Some other (non-modifier) key went down.
    pub fn other_key(&mut self) {
        self.dt.reset();
    }

    /// Tab came up. `gesture`: whether a double tap may mean the Agent on this screen right now
    /// (off where Tab belongs to the app, and while a dialog or menu is up).
    pub fn up(&mut self, now: std::time::Instant, gesture: bool) -> TabUp {
        if !self.hold.release() {
            self.dt.reset();
            return TabUp::None;
        }
        if !gesture {
            self.dt.reset();
            return TabUp::Single;
        }
        if self.dt.tap(now) {
            TabUp::Double
        } else {
            TabUp::Single
        }
    }
}

#[cfg(test)]
mod tab_gesture_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn two_quick_taps_are_a_double_tap() {
        let t0 = Instant::now();
        let mut g = TabGesture::default();
        g.down(t0, false);
        assert_eq!(g.up(ms(t0, 60), true), TabUp::Single);
        g.down(ms(t0, 150), false);
        assert_eq!(g.up(ms(t0, 210), true), TabUp::Double);
        // The third tap starts a new pair.
        g.down(ms(t0, 300), false);
        assert_eq!(g.up(ms(t0, 350), true), TabUp::Single);
    }

    #[test]
    fn slow_taps_are_two_singles() {
        let t0 = Instant::now();
        let mut g = TabGesture::default();
        g.down(t0, false);
        assert_eq!(g.up(ms(t0, 50), true), TabUp::Single);
        g.down(ms(t0, 360), false);
        assert_eq!(g.up(ms(t0, 400), true), TabUp::Single, "350 ms apart is past the window");
    }

    #[test]
    fn another_key_between_breaks_the_pair() {
        let t0 = Instant::now();
        let mut g = TabGesture::default();
        g.down(t0, false);
        g.up(ms(t0, 40), true);
        g.other_key();
        g.down(ms(t0, 100), false);
        assert_eq!(g.up(ms(t0, 140), true), TabUp::Single);
    }

    #[test]
    fn where_tab_belongs_to_the_app_a_double_tap_is_two_tabs() {
        let t0 = Instant::now();
        let mut g = TabGesture::default();
        g.down(t0, false);
        assert_eq!(g.up(ms(t0, 40), false), TabUp::Single);
        g.down(ms(t0, 100), false);
        assert_eq!(g.up(ms(t0, 140), false), TabUp::Single);
        // and a tap there does not half-arm the gesture for the next screen
        g.down(ms(t0, 200), false);
        assert_eq!(g.up(ms(t0, 240), true), TabUp::Single);
    }

    #[test]
    fn a_long_press_locks_once_and_is_not_a_tap() {
        let t0 = Instant::now();
        let mut g = TabGesture::default();
        g.down(t0, false);
        assert!(g.hold.gauge(ms(t0, 200)).is_none(), "no gauge for a normal press");
        assert!(g.hold.gauge(ms(t0, 500)).is_some());
        assert!(!g.hold.poll(ms(t0, 999), HoldKey::TAB_LONG));
        assert!(g.hold.poll(ms(t0, 1000), HoldKey::TAB_LONG));
        assert!(!g.hold.poll(ms(t0, 1500), HoldKey::TAB_LONG), "fires once");
        assert!(g.hold.gauge(ms(t0, 1500)).is_none());
        assert_eq!(g.up(ms(t0, 1600), true), TabUp::None);
        // A tap right after the long press is a first tap, not the second of a pair.
        g.down(ms(t0, 1650), false);
        assert_eq!(g.up(ms(t0, 1700), true), TabUp::Single);
    }

    #[test]
    fn chords_are_neither_taps_nor_long_presses() {
        let t0 = Instant::now();
        let mut g = TabGesture::default();
        g.down(t0, false);
        g.up(ms(t0, 40), true);
        g.down(ms(t0, 100), true); // Alt+Tab / Ctrl+Tab
        assert!(!g.hold.poll(ms(t0, 1500), HoldKey::TAB_LONG));
        assert_eq!(g.up(ms(t0, 1600), true), TabUp::None);
        g.down(ms(t0, 1650), false);
        assert_eq!(g.up(ms(t0, 1700), true), TabUp::Single);
    }

    #[test]
    fn esc_hold_fires_after_its_time() {
        let t0 = Instant::now();
        let mut e = HoldKey::default();
        e.press(t0);
        assert!(!e.poll(ms(t0, 899), HoldKey::ESC_LONG));
        assert!(e.poll(ms(t0, 900), HoldKey::ESC_LONG));
        assert!(!e.release(), "a fired hold is not a short press");
        e.press(ms(t0, 1000));
        assert!(e.release(), "a quick press is short");
        assert!(!e.active());
    }
}

#[cfg(test)]
mod double_tap_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn two_quick_taps_make_a_double_tap_and_the_third_starts_over() {
        let t0 = Instant::now();
        let mut d = DoubleTap::default();
        assert!(!d.tap(t0));
        assert!(d.pending(t0 + Duration::from_millis(100)));
        assert!(d.tap(t0 + Duration::from_millis(250)));
        assert!(!d.tap(t0 + Duration::from_millis(400)), "a third tap is a new first tap");
        assert!(d.tap(t0 + Duration::from_millis(600)));
    }

    #[test]
    fn slow_taps_never_pair_up() {
        let t0 = Instant::now();
        let mut d = DoubleTap::default();
        assert!(!d.tap(t0));
        assert!(!d.pending(t0 + Duration::from_millis(301)));
        assert!(!d.tap(t0 + Duration::from_millis(301)));
        d.reset();
        assert!(!d.tap(t0 + Duration::from_millis(400)), "reset forgets the first tap");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sym_layer_codes_become_symbols() {
        assert_eq!(sym_char(26), Some('!'));
        assert_eq!(sym_char(27), Some('@'));
        assert_eq!(sym_char(79), Some(';'));
        assert_eq!(sym_char(30), None);
        let out = sym_press('!', false, &Modifiers::default());
        assert!(out.contains(&Input::Text("!".into())));
    }

    #[test]
    fn letters_and_shift() {
        let mut m = Modifiers::default();
        let inputs = translate_press(30, false, &m);
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[1], Input::Text("a".into()));
        m.update(KEY_LEFTSHIFT, true);
        let inputs = translate_press(30, false, &m);
        assert_eq!(inputs[1], Input::Text("A".into()));
        assert!(matches!(&inputs[0], Input::Key { key: Keycode::A, .. }));
    }

    #[test]
    fn fn_layer_arrows_are_plain_keys() {
        let m = Modifiers::default();
        for (code, key) in [
            (103, Keycode::Up),
            (108, Keycode::Down),
            (105, Keycode::Left),
            (106, Keycode::Right),
            (104, Keycode::PageUp),
            (109, Keycode::PageDown),
        ] {
            let inputs = translate_press(code, false, &m);
            assert_eq!(inputs.len(), 1, "arrows must not generate text");
            assert!(matches!(&inputs[0], Input::Key { key: k, .. } if *k == key));
        }
    }

    #[test]
    fn fzxc_alias_to_arrows() {
        assert_eq!(arrow_alias(Keycode::F), Keycode::Up);
        assert_eq!(arrow_alias(Keycode::X), Keycode::Down);
        assert_eq!(arrow_alias(Keycode::Z), Keycode::Left);
        assert_eq!(arrow_alias(Keycode::C), Keycode::Right);
        assert_eq!(arrow_alias(Keycode::A), Keycode::A);
    }

    #[test]
    fn media_keys_are_global() {
        let m = Modifiers::default();
        assert_eq!(
            translate_press(115, false, &m),
            vec![Input::Media(MediaKey::VolumeUp)]
        );
        assert_eq!(
            translate_press(224, false, &m),
            vec![Input::Media(MediaKey::BrightnessDown)]
        );
    }

    #[test]
    fn ctrl_chords_produce_no_text() {
        let mut m = Modifiers::default();
        m.update(KEY_LEFTCTRL, true);
        let inputs = translate_press(46, false, &m); // Ctrl+C
        assert_eq!(inputs.len(), 1);
        assert!(m.ctrl());
    }

    #[test]
    fn modifier_press_is_silent() {
        let m = Modifiers::default();
        assert!(translate_press(KEY_LEFTSHIFT, false, &m).is_empty());
    }
}
