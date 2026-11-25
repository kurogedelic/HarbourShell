//! The on-screen keyboard: text entry on a device with only a gamepad (Anbernic and kin).
//!
//! Opened with the gamepad's X button. The d-pad walks the keys, A types the key under the
//! cursor, B deletes the character before the text cursor, L / R move the text cursor, Y types a
//! space, X switches the page, Start is Return and Select closes the keyboard. A click on a key
//! types it too. Those buttons are the keyboard's only space, return, delete and arrows: the panel
//! is just four rows of keys, so it takes as little of a small screen as it can.
//!
//! Pages (X, in turn): lower case, upper case, symbols and **kana** (Japanese input goes on with
//! the kana page and off after it). The kana page is the twelve-key layout of handheld consoles: あ か さ / た な は /
//! ま や ら / ゛゜小 わ 、。. Pressing a key again within a moment steps through its column
//! (か き く け こ); ゛゜小 turns the last kana into its voiced, half-voiced or small form.
//!
//! Everything goes out the way a real keyboard's typing does (a key press plus its text), and kana
//! as their romaji, so the input method composes them and can convert to kanji as usual.

use std::time::{Duration, Instant};

use crate::keys::{Keycode, Mod};
use crate::gfx::Rect;
use crate::gfx::Canvas;

use crate::font::FontSystem;
use crate::grid;
use crate::launcher::widgets;
use crate::platform::keys::Input;
use crate::theme::ThemeManager;
use crate::ui;

pub use crate::platform::keys::Pad;

/// The character pages: lower case, upper case, symbols. Four rows of ten.
const PAGES: [[&str; 4]; 3] = [
    ["1234567890", "qwertyuiop", "asdfghjkl-", "zxcvbnm,./"],
    ["!@#$%^&*()", "QWERTYUIOP", "ASDFGHJKL_", "ZXCVBNM<>?"],
    ["~`|\\{}[]+=", "\"':;?!/*-_", "<>()#$%&@^", "\u{3001}\u{3002}\u{300c}\u{300d}\u{30fb}\u{30fc}\u{301c}\u{2026}\u{ff01}\u{ff1f}"],
];
const KANA_PAGE: usize = 3;
const PAGE_COUNT: usize = 4;

/// The kana page: three keys per row (the twelve-key layout of handheld consoles). A key steps
/// through its column when pressed again; the column shows to the right of the keys, the kana it is
/// on now dark and the others faint.
const KANA: [[&str; 3]; 4] = [
    ["あいうえお", "かきくけこ", "さしすせそ"],
    ["たちつてと", "なにぬねの", "はひふへほ"],
    ["まみむめも", "やゆよ", "らりるれろ"],
    ["゛", "わをんー", "、。？！「」・"],
];
const KANA_SPAN: [usize; 3] = [2, 2, 2];
/// The kana keys take six of the ten columns; the column preview the other four.
const KANA_COLS: usize = 6;

/// A second press of the same kana key within this steps through its column.
const TAP_AGAIN: Duration = Duration::from_millis(1200);

/// What a button or a click on the keyboard did.
#[derive(Debug, PartialEq)]
pub enum Out {
    /// Hand these to the screen behind (typed keys and text).
    Send(Vec<Input>),
    /// Close the keyboard.
    Close,
    Nothing,
}

pub struct Osk {
    page: usize,
    /// Cursor: row 0..4, `col` indexes the row's keys.
    row: usize,
    col: usize,
    /// Japanese input is on (the host keeps this in step with the input method).
    pub ime_on: bool,
    /// The last kana typed from the kana page: (key, index in its column, character, when), for the
    /// next press of the same key and for ゛゜小.
    last: Option<((usize, usize), usize, char, Instant)>,
    /// The cursor has not moved since that kana: a press of the same key steps through its column.
    /// (Moving away keeps `last` for ゛゜小 but starts a new kana on the next press.)
    tapping: bool,
}

const ROWS: usize = 4;
const COLS: usize = 10;
const KEY_H: i32 = 16;
const PAD: i32 = 3;

impl Default for Osk {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Osk {
    /// `ime_on`: Japanese input is on, so the kana page comes first.
    pub fn new(ime_on: bool) -> Self {
        Self { page: if ime_on { KANA_PAGE } else { 0 }, row: 1, col: 0, ime_on, last: None, tapping: false }
    }

    /// The keyboard's panel: across the bottom of the surface.
    pub fn rect() -> Rect {
        let h = ROWS as i32 * KEY_H + 2 * PAD + 1;
        let w = (grid::sw() - 2 * grid::MARGIN).min(COLS as i32 * 30 + 2 * PAD + 1);
        Rect::new((grid::sw() - w) / 2, grid::sh() - h - 2, w as u32, h as u32)
    }

    fn key_w() -> i32 {
        (Self::rect().width() as i32 - 2 * PAD) / COLS as i32
    }

    /// The column spans of a row's keys.
    fn spans(&self, row: usize) -> Vec<usize> {
        let _ = row;
        if self.page == KANA_PAGE {
            KANA_SPAN.to_vec()
        } else {
            vec![1; COLS]
        }
    }

    fn row_len(&self, row: usize) -> usize {
        self.spans(row).len()
    }

    /// The rectangle of key `col` of `row`.
    fn key_rect(&self, row: usize, col: usize) -> Rect {
        let r = Self::rect();
        let kw = Self::key_w();
        let spans = self.spans(row);
        let start: usize = spans[..col].iter().sum();
        let y = r.y() + PAD + row as i32 * KEY_H;
        Rect::new(r.x() + PAD + start as i32 * kw, y, (kw * spans[col] as i32) as u32, KEY_H as u32)
    }

    /// The kana page's column preview: right of the kana keys, as high as their four rows.
    fn preview_rect() -> Rect {
        let r = Self::rect();
        let kw = Self::key_w();
        let x = r.x() + PAD + KANA_COLS as i32 * kw;
        Rect::new(x, r.y() + PAD, (kw * (COLS - KANA_COLS) as i32) as u32, (4 * KEY_H) as u32)
    }

    /// What the preview shows: the column of the kana key being tapped and where in it the tapping
    /// is (dark), or the column of the key under the cursor (all faint).
    pub fn preview(&self) -> Option<(Vec<char>, Option<usize>)> {
        if self.page != KANA_PAGE {
            return None;
        }
        if let Some((key, i, ch, _)) = self.last.filter(|_| self.tapping) {
            let column: Vec<char> = KANA[key.0][key.1].chars().collect();
            if column.get(i) == Some(&ch) {
                return Some((column, Some(i)));
            }
        }
        if (self.row, self.col) != (3, 0) {
            return Some((KANA[self.row][self.col].chars().collect(), None));
        }
        None
    }

    /// The key of `row` over grid column `gcol` (moving up and down keeps the cursor in place).
    fn key_at(&self, row: usize, gcol: usize) -> usize {
        let mut end = 0;
        for (i, span) in self.spans(row).iter().enumerate() {
            end += span;
            if gcol < end {
                return i;
            }
        }
        self.row_len(row) - 1
    }

    fn step(&mut self, dr: i32, dc: i32) {
        self.tapping = false;
        if dr != 0 {
            let gcol: usize = self.spans(self.row)[..self.col].iter().sum();
            self.row = (self.row as i32 + dr).rem_euclid(ROWS as i32) as usize;
            self.col = self.key_at(self.row, gcol);
        }
        if dc != 0 {
            self.col = (self.col as i32 + dc).rem_euclid(self.row_len(self.row) as i32) as usize;
        }
    }

    fn key(k: Keycode) -> Vec<Input> {
        vec![Input::Key { key: k, mods: Mod::NOMOD, repeat: false }, Input::KeyUp { key: k }]
    }

    /// A character as a real keyboard sends it: the key press (letters and digits, so the input
    /// method can compose) and the text.
    fn type_char(ch: char) -> Vec<Input> {
        let mut v = Vec::new();
        let lower = ch.to_ascii_lowercase();
        // Space as a key too: with Japanese input on it converts what is being composed.
        if ch == ' ' {
            return vec![Input::Key { key: Keycode::Space, mods: Mod::NOMOD, repeat: false }, Input::Text(" ".into()), Input::KeyUp { key: Keycode::Space }];
        }
        if ch.is_ascii_alphanumeric() {
            if let Some(k) = Keycode::from_name(&lower.to_string()) {
                let mods = if ch.is_ascii_uppercase() { Mod::LSHIFTMOD } else { Mod::NOMOD };
                v.push(Input::Key { key: k, mods, repeat: false });
                v.push(Input::Text(ch.to_string()));
                v.push(Input::KeyUp { key: k });
                return v;
            }
        }
        v.push(Input::Text(ch.to_string()));
        v
    }

    /// A kana as the romaji the input method composes it from.
    fn type_kana(ch: char) -> Vec<Input> {
        romaji(ch).chars().flat_map(Self::type_char).collect()
    }

    fn ime_toggle() -> Vec<Input> {
        vec![Input::Key { key: Keycode::Space, mods: Mod::LCTRLMOD, repeat: false }, Input::KeyUp { key: Keycode::Space }]
    }

    /// A kana key (row, col) pressed: a new kana, or the next one of its column when pressed again.
    fn kana(&mut self, row: usize, col: usize) -> Out {
        let column: Vec<char> = KANA[row][col].chars().collect();
        if (row, col) == (3, 0) {
            return self.modify();
        }
        let now = Instant::now();
        if let Some((key, i, _, at)) = self.last {
            if self.tapping && key == (row, col) && now.duration_since(at) < TAP_AGAIN && column.len() > 1 {
                let ni = (i + 1) % column.len();
                let ch = column[ni];
                self.last = Some(((row, col), ni, ch, now));
                self.tapping = true;
                let mut v = Self::key(Keycode::Backspace);
                v.extend(Self::type_kana(ch));
                return Out::Send(v);
            }
        }
        let ch = column[0];
        self.last = Some(((row, col), 0, ch, now));
        self.tapping = true;
        Out::Send(Self::type_kana(ch))
    }

    /// ゛゜小: the last kana becomes its next form (か が, は ば ぱ, つ っ づ, や ゃ...).
    fn modify(&mut self) -> Out {
        let Some((key, i, ch, _)) = self.last else { return Out::Nothing };
        let Some(next) = variant(ch) else { return Out::Nothing };
        self.last = Some((key, i, next, Instant::now()));
        self.tapping = false;
        let mut v = Self::key(Keycode::Backspace);
        v.extend(Self::type_kana(next));
        Out::Send(v)
    }

    fn activate(&mut self, row: usize, col: usize) -> Out {
        if self.page == KANA_PAGE {
            return self.kana(row, col);
        }
        self.last = None;
        let ch = PAGES[self.page][row].chars().nth(col).unwrap_or(' ');
        Out::Send(Self::type_char(ch))
    }

    /// X: the next page (lower case, upper case, symbols, kana). Japanese input follows: on with
    /// the kana page, off after it.
    fn next_page(&mut self) -> Out {
        self.last = None;
        self.page = (self.page + 1) % PAGE_COUNT;
        self.clamp_cursor();
        let want = self.page == KANA_PAGE;
        if want != self.ime_on {
            self.ime_on = want;
            return Out::Send(Self::ime_toggle());
        }
        Out::Nothing
    }

    fn clamp_cursor(&mut self) {
        self.col = self.col.min(self.row_len(self.row) - 1);
    }

    /// Keep the page in step with the input method (it can be switched with Ctrl+Space too).
    pub fn set_ime(&mut self, on: bool) {
        if on != self.ime_on {
            self.ime_on = on;
            if !on && self.page == KANA_PAGE {
                self.page = 0;
                self.clamp_cursor();
            }
        }
    }

    /// A gamepad button went down.
    pub fn pad(&mut self, b: Pad) -> Out {
        match b {
            Pad::Up => self.step(-1, 0),
            Pad::Down => self.step(1, 0),
            Pad::Left => self.step(0, -1),
            Pad::Right => self.step(0, 1),
            Pad::A | Pad::Stick => return self.activate(self.row, self.col),
            Pad::B => {
                self.last = None;
                return Out::Send(Self::key(Keycode::Backspace));
            }
            Pad::L => {
                self.last = None;
                return Out::Send(Self::key(Keycode::Left));
            }
            Pad::R => {
                self.last = None;
                return Out::Send(Self::key(Keycode::Right));
            }
            Pad::Y => {
                self.last = None;
                return Out::Send(Self::type_char(' '));
            }
            Pad::X => return self.next_page(),
            Pad::Start => return Out::Send(Self::key(Keycode::Return)),
            Pad::Select => return Out::Close,
        }
        Out::Nothing
    }

    /// The key under (x, y), if any (surface coordinates).
    pub fn hit(&self, x: i32, y: i32) -> Option<(usize, usize)> {
        (0..ROWS).flat_map(|r| (0..self.row_len(r)).map(move |c| (r, c))).find(|&(r, c)| self.key_rect(r, c).contains_point((x, y)))
    }

    /// A click: on a key it types it (and the cursor moves there); elsewhere on the panel nothing.
    pub fn click(&mut self, x: i32, y: i32) -> Out {
        match self.hit(x, y) {
            Some((r, c)) => {
                (self.row, self.col) = (r, c);
                self.activate(r, c)
            }
            None => Out::Nothing,
        }
    }

    fn label(&self, row: usize, col: usize) -> String {
        if self.page == KANA_PAGE {
            return match (row, col) {
                (3, 0) => "\u{309b}\u{309c}\u{5c0f}".to_string(),
                (3, 2) => "\u{3001}\u{3002}\u{ff1f}".to_string(),
                _ => KANA[row][col].chars().next().map(String::from).unwrap_or_default(),
            };
        }
        PAGES[self.page][row].chars().nth(col).map(String::from).unwrap_or_default()
    }

    pub fn draw(&self, c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        let r = Self::rect();
        widgets::draw_panel(c, tm, r);
        let sel = widgets::selection(tm);
        for row in 0..ROWS {
            for col in 0..self.row_len(row) {
                let k = self.key_rect(row, col);
                let label = self.label(row, col);
                let on = (row, col) == (self.row, self.col);
                let inner = Rect::new(k.x() + 1, k.y() + 1, k.width() - 2, k.height() - 2);
                if on {
                    ui::fill_rect(c, inner, sel.bg);
                } else {
                    ui::draw_rect(c, inner, tm.get_color("line"));
                }
                // The label in the middle of the key, inside its line.
                let w = fs.measure_text(&label);
                let x = k.x() + (k.width() as i32 - w) / 2;
                // 、。 sit at the bottom of their cell: lifted so the key reads as centred.
                let lift = if self.page == KANA_PAGE && (row, col) == (3, 2) { 2 } else { 0 };
                let y = widgets::text_origin(inner.y(), inner.height() as i32) - lift;
                let clip = Rect::new(inner.x() + 1, inner.y() + 1, inner.width() - 2, inner.height() - 2);
                let prev = c.clip_rect();
                c.set_clip_rect(clip);
                fs.draw_text(c, x, y, &label, if on { sel.fg } else { ink });
                c.set_clip_rect(prev);
            }
        }
        // The kana column: where the tapping is, dark on a plate; the rest faint.
        if let Some((column, at)) = self.preview() {
            let p = Self::preview_rect();
            let inner = Rect::new(p.x() + 1, p.y() + 1, p.width() - 2, p.height() - 2);
            ui::draw_rect(c, inner, tm.get_color("line"));
            let cell = 15;
            let n = column.len() as i32;
            let x0 = inner.x() + (inner.width() as i32 - n * cell) / 2;
            let row = Rect::new(inner.x(), inner.y() + (inner.height() as i32 - KEY_H) / 2, inner.width(), KEY_H as u32);
            let y = widgets::text_origin(row.y(), row.height() as i32);
            for (k, ch) in column.iter().enumerate() {
                let x = x0 + k as i32 * cell;
                let s = ch.to_string();
                let on = at == Some(k);
                if on {
                    ui::fill_rect(c, Rect::new(x, row.y() + 1, cell as u32, (KEY_H - 2) as u32), sel.bg);
                }
                let w = fs.measure_text(&s);
                fs.draw_text(c, x + (cell - w) / 2, y, &s, if on { sel.fg } else { tm.get_color("muted") });
            }
        }
    }
}

/// The romaji the input method composes `ch` from.
pub fn romaji(ch: char) -> String {
    const VOWELS: [&str; 5] = ["a", "i", "u", "e", "o"];
    const ROWS: [(&str, &str); 14] = [
        ("あいうえお", ""),
        ("かきくけこ", "k"),
        ("さしすせそ", "s"),
        ("たちつてと", "t"),
        ("なにぬねの", "n"),
        ("はひふへほ", "h"),
        ("まみむめも", "m"),
        ("らりるれろ", "r"),
        ("がぎぐげご", "g"),
        ("ざじずぜぞ", "z"),
        ("だぢづでど", "d"),
        ("ばびぶべぼ", "b"),
        ("ぱぴぷぺぽ", "p"),
        ("ぁぃぅぇぉ", "x"),
    ];
    for (kana, cons) in ROWS {
        if let Some(i) = kana.chars().position(|k| k == ch) {
            return format!("{cons}{}", VOWELS[i]);
        }
    }
    match ch {
        'や' => "ya",
        'ゆ' => "yu",
        'よ' => "yo",
        'ゃ' => "xya",
        'ゅ' => "xyu",
        'ょ' => "xyo",
        'わ' => "wa",
        'を' => "wo",
        'ん' => "xn",
        'ゎ' => "xwa",
        'っ' => "xtu",
        'ー' => "-",
        '、' => ",",
        '。' => ".",
        '？' => "?",
        '！' => "!",
        '「' => "[",
        '」' => "]",
        '・' => "/",
        _ => return ch.to_string(),
    }
    .to_string()
}

/// ゛゜小: the next form of `ch` (voiced, half-voiced, small), back to the plain one at the end.
pub fn variant(ch: char) -> Option<char> {
    const CYCLES: [&str; 32] = [
        "かが", "きぎ", "くぐ", "けげ", "こご", "さざ", "しじ", "すず", "せぜ", "そぞ", "ただ", "ちぢ", "つっづ", "てで", "とど",
        "はばぱ", "ひびぴ", "ふぶぷ", "へべぺ", "ほぼぽ", "あぁ", "いぃ", "うぅ", "えぇ", "おぉ", "やゃ", "ゆゅ", "よょ", "わゎ",
        "", "", "",
    ];
    for cyc in CYCLES.iter().filter(|c| !c.is_empty()) {
        let v: Vec<char> = cyc.chars().collect();
        if let Some(i) = v.iter().position(|c| *c == ch) {
            return Some(v[(i + 1) % v.len()]);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(o: &Out) -> String {
        match o {
            Out::Send(v) => v.iter().filter_map(|i| if let Input::Text(t) = i { Some(t.as_str()) } else { None }).collect(),
            _ => String::new(),
        }
    }

    fn backspaces(o: &Out) -> usize {
        match o {
            Out::Send(v) => v.iter().filter(|i| matches!(i, Input::Key { key: Keycode::Backspace, .. })).count(),
            _ => 0,
        }
    }

    #[test]
    fn the_dpad_walks_and_a_types() {
        let mut k = Osk::new(false);
        assert_eq!(texts(&k.pad(Pad::A)), "q", "starts on q");
        k.pad(Pad::Right);
        assert_eq!(texts(&k.pad(Pad::A)), "w");
        k.pad(Pad::Up);
        assert_eq!(texts(&k.pad(Pad::A)), "2");
        k.pad(Pad::Up);
        assert_eq!((k.row, k.col), (3, 1), "wraps to the bottom row");
        k.pad(Pad::Down);
        assert_eq!((k.row, k.col), (0, 1));
        k.pad(Pad::X);
        assert_eq!(texts(&k.pad(Pad::A)), "@", "X switches the page (upper case and symbols: @ over 2)");
    }

    #[test]
    fn letters_go_out_as_a_key_and_its_text() {
        let mut k = Osk::new(false);
        match k.pad(Pad::A) {
            Out::Send(v) => {
                assert!(matches!(v[0], Input::Key { key: Keycode::Q, .. }));
                assert!(matches!(&v[1], Input::Text(t) if t == "q"));
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn the_other_buttons() {
        let mut k = Osk::new(false);
        assert_eq!(backspaces(&k.pad(Pad::B)), 1);
        assert!(matches!(k.pad(Pad::L), Out::Send(v) if matches!(v[0], Input::Key { key: Keycode::Left, .. })));
        assert!(matches!(k.pad(Pad::R), Out::Send(v) if matches!(v[0], Input::Key { key: Keycode::Right, .. })));
        assert_eq!(texts(&k.pad(Pad::Y)), " ");
        assert!(matches!(k.pad(Pad::Start), Out::Send(v) if matches!(v[0], Input::Key { key: Keycode::Return, .. })));
        assert_eq!(k.pad(Pad::Select), Out::Close);
    }

    #[test]
    fn kana_keys_step_through_their_column_and_modify_the_last() {
        let mut k = Osk::new(true);
        assert_eq!(k.page, KANA_PAGE, "Japanese on: the kana page first");
        k.row = 0;
        k.col = 1; // か
        assert_eq!(texts(&k.pad(Pad::A)), "ka");
        let again = k.pad(Pad::A);
        assert_eq!((backspaces(&again), texts(&again)), (1, "ki".into()), "pressed again: き replaces か");
        // ゛゜小 (row 3, key 0), walked to with the d-pad: き -> ぎ.
        for _ in 0..3 {
            k.pad(Pad::Down);
        }
        k.pad(Pad::Left);
        assert_eq!((k.row, k.col), (3, 0));
        let m = k.pad(Pad::A);
        assert_eq!((backspaces(&m), texts(&m)), (1, "gi".into()));
        // After moving away the same key starts a new kana.
        k.row = 0;
        k.col = 1;
        k.pad(Pad::Right);
        k.pad(Pad::Left);
        assert_eq!(texts(&k.pad(Pad::A)), "ka");
        k.pad(Pad::A);
        k.pad(Pad::A);
        assert_eq!(k.preview(), Some(("かきくけこ".chars().collect(), Some(2))), "tapped three times: く dark");
        k.pad(Pad::Right);
        assert_eq!(k.preview(), Some(("さしすせそ".chars().collect(), None)), "moved: the next key's column, all faint");
        assert_eq!(variant('つ'), Some('っ'));
        assert_eq!(variant('っ'), Some('づ'));
        assert_eq!(variant('づ'), Some('つ'));
        assert_eq!(variant('は'), Some('ば'));
        assert_eq!(variant('ぽ'), Some('ほ'));
        assert_eq!(variant('ん'), None);
        assert_eq!(romaji('ん'), "xn");
        assert_eq!(romaji('っ'), "xtu");
        assert_eq!(romaji('ぢ'), "di");
    }

    #[test]
    fn x_steps_through_the_pages_and_japanese_input_follows_the_kana_page() {
        let mut k = Osk::new(false);
        assert_eq!(k.pad(Pad::X), Out::Nothing);
        assert_eq!(k.pad(Pad::X), Out::Nothing);
        let on = k.pad(Pad::X);
        assert!(matches!(&on, Out::Send(v) if matches!(v[0], Input::Key { key: Keycode::Space, mods, .. } if mods.contains(Mod::LCTRLMOD))));
        assert_eq!(k.page, KANA_PAGE);
        assert!(matches!(k.pad(Pad::X), Out::Send(_)), "and off again after it");
        assert_eq!(k.page, 0);
        k.set_ime(true);
        k.page = KANA_PAGE;
        k.set_ime(false);
        assert_eq!(k.page, 0, "Japanese switched off elsewhere: back to letters");
    }

    #[test]
    fn keys_fit_and_a_click_types_what_is_drawn() {
        for size in [(320, 170), (400, 240), (800, 480)] {
            grid::with_surface(size, || {
                let r = Osk::rect();
                assert!(r.x() >= 0 && r.right() <= size.0 && r.bottom() <= size.1, "{size:?} {r:?}");
                for ime in [false, true] {
                    let k = Osk::new(ime);
                    for row in 0..ROWS {
                        let last = k.key_rect(row, k.row_len(row) - 1);
                        assert!(last.right() <= r.right(), "row {row} fits: {last:?} {r:?}");
                    }
                }
                let mut k = Osk::new(false);
                let e = k.key_rect(1, 2);
                assert_eq!(texts(&k.click(e.x() + 2, e.y() + 2)), "e");
            });
        }
    }
}
