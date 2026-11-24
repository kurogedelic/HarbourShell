//! Scientific calculator as a compact desk tool (no window frame).
//!
//! * Display window on top (right aligned), bevelled buttons with a hard shadow.
//! * No digit buttons: digits and operators come from the number row and the
//!   symbol keys. The three QWERTY rows are function keys, and each button shows
//!   the key it answers to.
//! * Entry mode: Algebraic (type an expression, Enter evaluates) or RPN
//!   (Enter pushes, operators work on the stack). Tab opens the settings modal.

use std::time::{Duration, Instant};

use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::font::FontSystem;
use crate::theme::ThemeManager;
use crate::ui;

// ------------------------------------------------------------------ settings

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Angle {
    Deg,
    Rad,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryMode {
    Algebraic,
    Rpn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Auto,
    Fix2,
    Sci,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub entry: EntryMode,
    pub angle: Angle,
    pub format: Format,
}

impl Default for Settings {
    fn default() -> Self {
        Self { entry: EntryMode::Algebraic, angle: Angle::Deg, format: Format::Auto }
    }
}

impl Settings {
    /// Settings stored as three small ints in config.json.
    pub fn from_ints(rpn: i64, rad: i64, fmt: i64) -> Self {
        Self {
            entry: if rpn == 1 { EntryMode::Rpn } else { EntryMode::Algebraic },
            angle: if rad == 1 { Angle::Rad } else { Angle::Deg },
            format: match fmt {
                1 => Format::Fix2,
                2 => Format::Sci,
                _ => Format::Auto,
            },
        }
    }

    pub fn to_ints(self) -> (i64, i64, i64) {
        (
            (self.entry == EntryMode::Rpn) as i64,
            (self.angle == Angle::Rad) as i64,
            match self.format {
                Format::Auto => 0,
                Format::Fix2 => 1,
                Format::Sci => 2,
            },
        )
    }
}

// ----------------------------------------------------------------- evaluator

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Id(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
    Bang,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < cs.len() {
        let c = cs[i];
        match c {
            ' ' => i += 1,
            '0'..='9' | '.' => {
                let start = i;
                while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                    i += 1;
                }
                let text: String = cs[start..i].iter().collect();
                out.push(Tok::Num(text.parse::<f64>().map_err(|_| "Syntax".to_string())?));
            }
            'a'..='z' | 'A'..='Z' => {
                let start = i;
                while i < cs.len() && cs[i].is_ascii_alphabetic() {
                    i += 1;
                }
                out.push(Tok::Id(cs[start..i].iter().collect::<String>().to_lowercase()));
            }
            '+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            '-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            '*' | '\u{d7}' => {
                out.push(Tok::Star);
                i += 1;
            }
            '/' | '\u{f7}' => {
                out.push(Tok::Slash);
                i += 1;
            }
            '^' => {
                out.push(Tok::Caret);
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '!' => {
                out.push(Tok::Bang);
                i += 1;
            }
            _ => return Err("Syntax".into()),
        }
    }
    Ok(out)
}

pub fn factorial(x: f64) -> Result<f64, String> {
    if x < 0.0 || x.fract() != 0.0 {
        return Err("Domain".into());
    }
    if x > 170.0 {
        return Err("Overflow".into());
    }
    Ok((1..=x as u64).fold(1.0, |a, n| a * n as f64))
}

/// A named one-argument function. Trig functions honour the angle unit.
pub fn apply_fn(name: &str, x: f64, angle: Angle) -> Result<f64, String> {
    let to_rad = |v: f64| if angle == Angle::Deg { v.to_radians() } else { v };
    let from_rad = |v: f64| if angle == Angle::Deg { v.to_degrees() } else { v };
    let r = match name {
        "sin" => {
            let v = to_rad(x).sin();
            // sin(180 deg) should be exactly 0, not 1.2e-16.
            if v.abs() < 1e-15 { 0.0 } else { v }
        }
        "cos" => {
            let v = to_rad(x).cos();
            if v.abs() < 1e-15 { 0.0 } else { v }
        }
        "tan" => {
            let v = to_rad(x);
            if v.cos().abs() < 1e-15 {
                return Err("Domain".into());
            }
            let t = v.tan();
            if t.abs() < 1e-15 { 0.0 } else { t }
        }
        "asin" => {
            if !(-1.0..=1.0).contains(&x) {
                return Err("Domain".into());
            }
            from_rad(x.asin())
        }
        "acos" => {
            if !(-1.0..=1.0).contains(&x) {
                return Err("Domain".into());
            }
            from_rad(x.acos())
        }
        "atan" => from_rad(x.atan()),
        "sqrt" => {
            if x < 0.0 {
                return Err("Domain".into());
            }
            x.sqrt()
        }
        "cbrt" => x.cbrt(),
        "ln" => {
            if x <= 0.0 {
                return Err("Domain".into());
            }
            x.ln()
        }
        "log" => {
            if x <= 0.0 {
                return Err("Domain".into());
            }
            // Exact for powers of ten.
            let v = x.log10();
            if (v - v.round()).abs() < 1e-12 { v.round() } else { v }
        }
        "exp" => x.exp(),
        "abs" => x.abs(),
        "inv" => {
            if x == 0.0 {
                return Err("Div by 0".into());
            }
            1.0 / x
        }
        "sq" => x * x,
        "tenx" => 10f64.powf(x),
        "fact" => factorial(x)?,
        _ => return Err("Syntax".into()),
    };
    if r.is_finite() { Ok(r) } else { Err("Overflow".into()) }
}

fn is_function(name: &str) -> bool {
    matches!(name, "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sqrt" | "cbrt" | "ln" | "log" | "exp" | "abs")
}

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    angle: Angle,
    ans: f64,
    rnd: f64,
    _src: &'a str,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn expr(&mut self) -> Result<f64, String> {
        let mut v = self.term()?;
        loop {
            match self.peek() {
                Some(Tok::Plus) => {
                    self.pos += 1;
                    v += self.term()?;
                }
                Some(Tok::Minus) => {
                    self.pos += 1;
                    v -= self.term()?;
                }
                _ => return Ok(v),
            }
        }
    }

    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.unary()?;
        loop {
            match self.peek() {
                Some(Tok::Star) => {
                    self.pos += 1;
                    v *= self.unary()?;
                }
                Some(Tok::Slash) => {
                    self.pos += 1;
                    let d = self.unary()?;
                    if d == 0.0 {
                        return Err("Div by 0".into());
                    }
                    v /= d;
                }
                Some(Tok::Id(n)) if n == "mod" => {
                    self.pos += 1;
                    let d = self.unary()?;
                    if d == 0.0 {
                        return Err("Div by 0".into());
                    }
                    v = v.rem_euclid(d);
                }
                // Implicit multiplication: 2pi, 3(4), 2sin(30).
                Some(Tok::Num(_)) | Some(Tok::LParen) => v *= self.unary()?,
                Some(Tok::Id(n)) if n != "mod" => v *= self.unary()?,
                _ => return Ok(v),
            }
        }
    }

    fn unary(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some(Tok::Minus) => {
                self.pos += 1;
                Ok(-self.unary()?)
            }
            Some(Tok::Plus) => {
                self.pos += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Result<f64, String> {
        let base = self.postfix()?;
        if let Some(Tok::Caret) = self.peek() {
            self.pos += 1;
            let e = self.unary()?; // right associative: 2^3^2 = 2^9
            let r = base.powf(e);
            if r.is_nan() {
                return Err("Domain".into());
            }
            if !r.is_finite() {
                return Err("Overflow".into());
            }
            return Ok(r);
        }
        Ok(base)
    }

    fn postfix(&mut self) -> Result<f64, String> {
        let mut v = self.primary()?;
        while let Some(Tok::Bang) = self.peek() {
            self.pos += 1;
            v = factorial(v)?;
        }
        Ok(v)
    }

    fn primary(&mut self) -> Result<f64, String> {
        match self.toks.get(self.pos).cloned() {
            Some(Tok::Num(n)) => {
                self.pos += 1;
                Ok(n)
            }
            Some(Tok::LParen) => {
                self.pos += 1;
                let v = self.expr()?;
                match self.peek() {
                    Some(Tok::RParen) => {
                        self.pos += 1;
                        Ok(v)
                    }
                    _ => Err("Syntax".into()),
                }
            }
            Some(Tok::Id(name)) => {
                self.pos += 1;
                match name.as_str() {
                    "pi" => Ok(std::f64::consts::PI),
                    "e" => Ok(std::f64::consts::E),
                    "ans" => Ok(self.ans),
                    "rnd" => Ok(self.rnd),
                    n if is_function(n) => {
                        if self.peek() != Some(&Tok::LParen) {
                            return Err("Syntax".into());
                        }
                        self.pos += 1;
                        let x = self.expr()?;
                        match self.peek() {
                            Some(Tok::RParen) => self.pos += 1,
                            // An unclosed "sin(30" is accepted, like most calculators.
                            None => {}
                            _ => return Err("Syntax".into()),
                        }
                        apply_fn(n, x, self.angle)
                    }
                    _ => Err("Syntax".into()),
                }
            }
            _ => Err("Syntax".into()),
        }
    }
}

/// Evaluate an algebraic expression.
pub fn evaluate(expr: &str, angle: Angle, ans: f64, rnd: f64) -> Result<f64, String> {
    let toks = tokenize(expr)?;
    if toks.is_empty() {
        return Err("Empty".into());
    }
    let mut p = Parser { toks, pos: 0, angle, ans, rnd, _src: expr };
    let mut v = p.expr()?;
    // Unclosed parentheses at the end are accepted.
    while p.peek() == Some(&Tok::RParen) {
        p.pos += 1;
        let _ = &mut v;
    }
    if p.pos != p.toks.len() {
        return Err("Syntax".into());
    }
    if v.is_finite() { Ok(v) } else { Err("Overflow".into()) }
}

/// Round away floating-point noise (0.1 + 0.2) for display.
fn clean(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let mag = v.abs().log10().floor();
    let scale = 10f64.powf(14.0 - mag);
    if scale.is_finite() { (v * scale).round() / scale } else { v }
}

pub fn format_number(v: f64, f: Format) -> String {
    let v = clean(v);
    if v == 0.0 {
        return match f {
            Format::Fix2 => "0.00".into(),
            _ => "0".into(),
        };
    }
    match f {
        Format::Fix2 => {
            if v.abs() >= 1e12 {
                return format!("{v:.5e}");
            }
            format!("{v:.2}")
        }
        Format::Sci => format!("{v:.6e}"),
        Format::Auto => {
            let a = v.abs();
            if !(1e-6..1e12).contains(&a) {
                let s = format!("{v:.9e}");
                // Trim trailing zeros in the mantissa: 1.500000000e12 -> 1.5e12
                let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
                let m = if m.contains('.') { m.trim_end_matches('0').trim_end_matches('.') } else { m };
                return format!("{m}e{e}");
            }
            let digits_before = a.log10().floor() as i32 + 1;
            let decimals = (10 - digits_before).clamp(0, 10) as usize;
            let s = format!("{v:.decimals$}");
            if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
        }
    }
}

// --------------------------------------------------------------------- keys

/// One QWERTY function key: the letter, its label, and its RPN label.
pub struct FnKey {
    pub ch: char,
    pub label: &'static str,
    pub rpn_label: &'static str,
}

const fn k(ch: char, label: &'static str, rpn_label: &'static str) -> FnKey {
    FnKey { ch, label, rpn_label }
}

pub const ROWS: [&[FnKey]; 3] = [
    &[
        k('q', "sin", "sin"),
        k('w', "cos", "cos"),
        k('e', "tan", "tan"),
        k('r', "sqrt", "sqrt"),
        k('t', "x^2", "x^2"),
        k('y', "x^y", "x^y"),
        k('u', "1/x", "1/x"),
        k('i', "ln", "ln"),
        k('o', "log", "log"),
        k('p', "e^x", "e^x"),
    ],
    &[
        k('a', "asin", "asin"),
        k('s', "acos", "acos"),
        k('d', "atan", "atan"),
        k('f', "cbrt", "cbrt"),
        k('g', "pi", "pi"),
        k('h', "e", "e"),
        k('j', "n!", "n!"),
        k('k', "mod", "mod"),
        k('l', "10^x", "10^x"),
    ],
    &[
        k('z', "(", "swap"),
        k('x', ")", "drop"),
        k('c', "+/-", "+/-"),
        k('v', "abs", "abs"),
        k('b', "rnd", "rnd"),
        k('n', "ans", "dup"),
        k('m', "AC", "AC"),
    ],
];

/// Text an algebraic-mode key inserts.
fn alg_insert(ch: char) -> Option<&'static str> {
    Some(match ch {
        'q' => "sin(",
        'w' => "cos(",
        'e' => "tan(",
        'r' => "sqrt(",
        't' => "^2",
        'y' => "^",
        'u' => "1/(",
        'i' => "ln(",
        'o' => "log(",
        'p' => "exp(",
        'a' => "asin(",
        's' => "acos(",
        'd' => "atan(",
        'f' => "cbrt(",
        'g' => "pi",
        'h' => "e",
        'j' => "!",
        'k' => " mod ",
        'l' => "10^(",
        'z' => "(",
        'x' => ")",
        'c' => "(-",
        'v' => "abs(",
        'b' => "rnd",
        'n' => "ans",
        _ => return None,
    })
}

const TOKENS: [&str; 17] = [
    "asin(", "acos(", "atan(", "sqrt(", "cbrt(", "sin(", "cos(", "tan(", "log(", "exp(", "abs(", "ln(", "10^(", "1/(", " mod ", "rnd", "ans",
];

/// A key on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalcKey {
    /// A QWERTY function key (the letter it answers to).
    Fn(char),
    /// A digit, '.', or an operator of the strip under the keys.
    Pad(char),
    Bksp,
    Enter,
}

const KEY_W: i32 = 28;
/// Keys are 26 px (rows 28 apart, room for the 2 px shadow) under a 44 px display, so the strip of
/// digits ends on y 168 and the calculator uses the screen down to its bottom.
const KEY_H: i32 = 26;
const KEY_PITCH: i32 = 30;
const DISP_H: u32 = 44;
const PAD_KEYS: [char; 16] = ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '.', '+', '-', '*', '/', '^'];
/// Marker `flash` uses for the keys that have no letter.
const FLASH_BKSP: char = '\u{8}';
const FLASH_ENTER: char = '\n';

/// The display window for a calculator drawn at (x, y) with width w.
pub fn display_rect(x: i32, y: i32, w: u32) -> Rect {
    Rect::new(x + 4, y + 4, w.saturating_sub(8), DISP_H)
}

/// Every key with the rectangle it is drawn in (used by both drawing and the mouse).
pub fn key_rects(x: i32, y: i32, w: u32) -> Vec<(Rect, CalcKey)> {
    let w = w as i32;
    let disp = display_rect(x, y, w as u32);
    let top = disp.y() + disp.height() as i32 + 4;
    let mut v = Vec::new();
    for (r, row) in ROWS.iter().enumerate() {
        let off = [0, 8, 16][r];
        for (i, key) in row.iter().enumerate() {
            v.push((Rect::new(x + 4 + off + i as i32 * KEY_PITCH, top + r as i32 * (KEY_H + 2), KEY_W as u32, KEY_H as u32), CalcKey::Fn(key.ch)));
        }
    }
    v.push((Rect::new(x + 4 + 8 + 9 * KEY_PITCH, top + (KEY_H + 2), KEY_W as u32, KEY_H as u32), CalcKey::Bksp));
    let ex = x + 4 + 16 + 7 * KEY_PITCH;
    v.push((Rect::new(ex, top + 2 * (KEY_H + 2), (x + w - 4 - ex - 2) as u32, KEY_H as u32), CalcKey::Enter));
    // The strip of digits and operators below the letters.
    let py = top + 3 * (KEY_H + 2) + 1;
    let pitch = (w - 8) / PAD_KEYS.len() as i32;
    for (i, ch) in PAD_KEYS.iter().enumerate() {
        v.push((Rect::new(x + 4 + i as i32 * pitch, py, (pitch - 2) as u32, 16), CalcKey::Pad(*ch)));
    }
    v
}

/// The Settings dialog's rectangle.
fn modal_rect(x: i32, y: i32, w: i32) -> Rect {
    Rect::new(x + (w - 220) / 2, y + 14, 220, 94)
}

// ------------------------------------------------------------------- state

#[derive(Debug, PartialEq, Eq)]
pub enum CalcEvent {
    None,
    Close,
    SettingsChanged,
}

pub struct Calc {
    pub settings: Settings,
    expr: String,
    result: Option<Result<f64, String>>,
    ans: f64,
    /// RPN stack, top is last.
    stack: Vec<f64>,
    /// Number being typed in RPN mode.
    entry: String,
    /// Settings modal: cursor row when open.
    pub modal: Option<usize>,
    flash: Option<(char, Instant)>,
    rng: u64,
    after_eval: bool,
}

impl Calc {
    pub fn new(settings: Settings) -> Self {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        Self {
            settings,
            expr: String::new(),
            result: None,
            ans: 0.0,
            stack: Vec::new(),
            entry: String::new(),
            modal: None,
            flash: None,
            rng: seed | 1,
            after_eval: false,
        }
    }

    fn rnd(&mut self) -> f64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        (x.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn toggle_modal(&mut self) {
        self.modal = if self.modal.is_some() { None } else { Some(0) };
    }

    pub fn is_flashing(&self) -> bool {
        self.flash.map(|(_, t)| t.elapsed() < Duration::from_millis(160)).unwrap_or(false)
    }

    pub fn expression(&self) -> &str {
        &self.expr
    }

    pub fn x(&self) -> Option<f64> {
        self.stack.last().copied()
    }

    pub fn stack(&self) -> &[f64] {
        &self.stack
    }

    pub fn entry_text(&self) -> &str {
        &self.entry
    }

    pub fn result(&self) -> Option<&Result<f64, String>> {
        self.result.as_ref()
    }

    pub fn clear_all(&mut self) {
        self.all_clear();
    }

    fn all_clear(&mut self) {
        self.expr.clear();
        self.result = None;
        self.after_eval = false;
        self.entry.clear();
        self.stack.clear();
    }

    // ---- algebraic -----------------------------------------------------------

    fn alg_start(&mut self, insert: &str) {
        if self.after_eval {
            // After "=": operators continue from the result, anything else starts over.
            let continues = insert.starts_with(['+', '-', '*', '/', '^', '!', ' ']);
            let prev = match &self.result {
                Some(Ok(v)) => format_number(*v, Format::Auto),
                _ => String::new(),
            };
            self.expr.clear();
            if continues && !prev.is_empty() {
                self.expr.push_str(&prev);
            }
            self.result = None;
            self.after_eval = false;
        }
        if self.expr.len() < 80 {
            self.expr.push_str(insert);
        }
    }

    fn alg_eval(&mut self) {
        if self.expr.trim().is_empty() {
            return;
        }
        let rnd = self.rnd();
        let r = evaluate(&self.expr, self.settings.angle, self.ans, rnd);
        if let Ok(v) = &r {
            self.ans = *v;
        }
        self.result = Some(r);
        self.after_eval = true;
    }

    fn alg_backspace(&mut self) {
        if self.after_eval {
            self.result = None;
            self.after_eval = false;
            return;
        }
        for t in TOKENS {
            if self.expr.ends_with(t) {
                self.expr.truncate(self.expr.len() - t.len());
                return;
            }
        }
        self.expr.pop();
    }

    // ---- RPN -----------------------------------------------------------------

    fn commit_entry(&mut self) {
        if !self.entry.is_empty() {
            let v = self.entry.parse::<f64>().unwrap_or(0.0);
            self.stack.push(v);
            self.entry.clear();
        }
    }

    fn rpn_unary(&mut self, name: &str) {
        self.commit_entry();
        let Some(x) = self.stack.pop() else {
            self.result = Some(Err("Empty".into()));
            return;
        };
        match apply_fn(name, x, self.settings.angle) {
            Ok(v) => {
                self.stack.push(v);
                self.result = None;
            }
            Err(e) => {
                self.stack.push(x); // keep the operand on error
                self.result = Some(Err(e));
            }
        }
    }

    fn rpn_binary(&mut self, op: char) {
        self.commit_entry();
        if self.stack.len() < 2 {
            self.result = Some(Err("Need 2".into()));
            return;
        }
        let b = self.stack.pop().unwrap();
        let a = self.stack.pop().unwrap();
        let r = match op {
            '+' => Ok(a + b),
            '-' => Ok(a - b),
            '*' => Ok(a * b),
            '/' => {
                if b == 0.0 { Err("Div by 0".to_string()) } else { Ok(a / b) }
            }
            '^' => {
                let r = a.powf(b);
                if r.is_nan() { Err("Domain".to_string()) } else if r.is_finite() { Ok(r) } else { Err("Overflow".to_string()) }
            }
            'm' => {
                if b == 0.0 { Err("Div by 0".to_string()) } else { Ok(a.rem_euclid(b)) }
            }
            _ => Err("Syntax".to_string()),
        };
        match r {
            Ok(v) => {
                self.stack.push(v);
                self.result = None;
            }
            Err(e) => {
                self.stack.push(a);
                self.stack.push(b);
                self.result = Some(Err(e));
            }
        }
    }

    fn rpn_key(&mut self, ch: char) {
        match ch {
            'q' => self.rpn_unary("sin"),
            'w' => self.rpn_unary("cos"),
            'e' => self.rpn_unary("tan"),
            'r' => self.rpn_unary("sqrt"),
            't' => self.rpn_unary("sq"),
            'u' => self.rpn_unary("inv"),
            'i' => self.rpn_unary("ln"),
            'o' => self.rpn_unary("log"),
            'p' => self.rpn_unary("exp"),
            'a' => self.rpn_unary("asin"),
            's' => self.rpn_unary("acos"),
            'd' => self.rpn_unary("atan"),
            'f' => self.rpn_unary("cbrt"),
            'l' => self.rpn_unary("tenx"),
            'v' => self.rpn_unary("abs"),
            'j' => self.rpn_unary("fact"),
            'y' => self.rpn_binary('^'),
            'k' => self.rpn_binary('m'),
            'g' => {
                self.commit_entry();
                self.stack.push(std::f64::consts::PI);
            }
            'h' => {
                self.commit_entry();
                self.stack.push(std::f64::consts::E);
            }
            'b' => {
                self.commit_entry();
                let r = self.rnd();
                self.stack.push(r);
            }
            'c' => {
                // Change sign of the number being typed, or of X.
                if !self.entry.is_empty() {
                    if let Some(rest) = self.entry.strip_prefix('-') {
                        self.entry = rest.to_string();
                    } else {
                        self.entry.insert(0, '-');
                    }
                } else if let Some(x) = self.stack.last_mut() {
                    *x = -*x;
                }
            }
            'z' => {
                self.commit_entry();
                let n = self.stack.len();
                if n >= 2 {
                    self.stack.swap(n - 1, n - 2);
                }
            }
            'x' => {
                self.commit_entry();
                self.stack.pop();
            }
            'n' => {
                self.commit_entry();
                if let Some(&x) = self.stack.last() {
                    self.stack.push(x);
                }
            }
            'm' => self.all_clear(),
            _ => {}
        }
    }

    // ---- input ----------------------------------------------------------------

    /// Text typed on the keyboard: digits, operators and the QWERTY function keys.
    pub fn handle_text(&mut self, text: &str) {
        if self.modal.is_some() {
            return;
        }
        for ch in text.chars() {
            self.type_char(ch);
        }
    }

    fn type_char(&mut self, ch: char) {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphabetic() {
            self.flash = Some((lower, Instant::now()));
            match self.settings.entry {
                EntryMode::Algebraic => {
                    if lower == 'm' {
                        self.all_clear();
                    } else if let Some(ins) = alg_insert(lower) {
                        self.alg_start(ins);
                    }
                }
                EntryMode::Rpn => self.rpn_key(lower),
            }
            return;
        }
        match self.settings.entry {
            EntryMode::Algebraic => {
                if ch.is_ascii_digit() || ch == '.' {
                    let s = ch.to_string();
                    self.alg_start(&s);
                } else if matches!(ch, '+' | '-' | '*' | '/' | '^' | '(' | ')' | '!') {
                    let s = ch.to_string();
                    self.alg_start(&s);
                }
            }
            EntryMode::Rpn => {
                if ch.is_ascii_digit() {
                    if self.entry.len() < 20 {
                        self.entry.push(ch);
                    }
                    self.result = None;
                } else if ch == '.' {
                    if !self.entry.contains('.') {
                        if self.entry.is_empty() || self.entry == "-" {
                            self.entry.push('0');
                        }
                        self.entry.push('.');
                    }
                } else if matches!(ch, '+' | '-' | '*' | '/' | '^') {
                    self.rpn_binary(ch);
                }
            }
        }
    }

    pub fn handle_key(&mut self, key: Keycode, _mods: Mod) -> CalcEvent {
        if let Some(row) = self.modal {
            return self.modal_key(key, row);
        }
        match key {
            Keycode::Escape => return CalcEvent::Close,
            Keycode::Return | Keycode::KpEnter => match self.settings.entry {
                EntryMode::Algebraic => self.alg_eval(),
                EntryMode::Rpn => {
                    if self.entry.is_empty() {
                        if let Some(&x) = self.stack.last() {
                            self.stack.push(x); // Enter on an empty entry duplicates X
                        }
                    } else {
                        self.commit_entry();
                    }
                    self.result = None;
                }
            },
            Keycode::Backspace => match self.settings.entry {
                EntryMode::Algebraic => self.alg_backspace(),
                EntryMode::Rpn => {
                    if self.entry.is_empty() {
                        self.stack.pop();
                    } else {
                        self.entry.pop();
                    }
                    self.result = None;
                }
            },
            Keycode::Delete => self.all_clear(),
            _ => {}
        }
        CalcEvent::None
    }

    fn modal_key(&mut self, key: Keycode, row: usize) -> CalcEvent {
        let key = crate::platform::keys::arrow_alias(key);
        let mut changed = false;
        match key {
            Keycode::Escape | Keycode::Return | Keycode::KpEnter => self.modal = None,
            Keycode::Up => self.modal = Some(row.saturating_sub(1)),
            Keycode::Down => self.modal = Some((row + 1).min(2)),
            Keycode::Left | Keycode::Right => {
                let dir: i32 = if key == Keycode::Right { 1 } else { -1 };
                match row {
                    0 => {
                        let to = if dir > 0 { EntryMode::Rpn } else { EntryMode::Algebraic };
                        if to != self.settings.entry {
                            self.settings.entry = to;
                            changed = true;
                            // Start clean so a half typed expression is not misread.
                            self.all_clear();
                        }
                    }
                    1 => {
                        let to = if dir > 0 { Angle::Rad } else { Angle::Deg };
                        if to != self.settings.angle {
                            self.settings.angle = to;
                            changed = true;
                        }
                    }
                    _ => {
                        let order = [Format::Auto, Format::Fix2, Format::Sci];
                        let i = order.iter().position(|f| *f == self.settings.format).unwrap_or(0) as i32;
                        let to = order[(i + dir).clamp(0, 2) as usize];
                        if to != self.settings.format {
                            self.settings.format = to;
                            changed = true;
                        }
                    }
                }
            }
            _ => {}
        }
        if changed { CalcEvent::SettingsChanged } else { CalcEvent::None }
    }

    /// A left click at (px, py) on a calculator drawn at (x, y, w). Keys act like typing them; the
    /// display opens the Settings dialog; inside the dialog a row cycles its value.
    pub fn click(&mut self, px: i32, py: i32, x: i32, y: i32, w: u32) -> CalcEvent {
        if let Some(row) = self.modal {
            let r = modal_rect(x, y, w as i32);
            if !crate::launcher::widgets::contains(r, px, py) {
                self.modal = None;
                return CalcEvent::None;
            }
            let i = (py - (r.y() + 25)) / 16;
            if (0..3).contains(&i) && py >= r.y() + 25 {
                self.modal = Some(i as usize);
                return self.modal_cycle(i as usize);
            }
            let _ = row;
            return CalcEvent::None;
        }
        if crate::launcher::widgets::contains(display_rect(x, y, w), px, py) {
            self.toggle_modal();
            return CalcEvent::None;
        }
        let Some((_, key)) = key_rects(x, y, w).into_iter().find(|(r, _)| crate::launcher::widgets::contains(*r, px, py)) else {
            return CalcEvent::None;
        };
        match key {
            CalcKey::Fn(ch) | CalcKey::Pad(ch) => self.type_char(ch),
            CalcKey::Bksp => {
                self.flash = Some((FLASH_BKSP, Instant::now()));
                return self.handle_key(Keycode::Backspace, Mod::NOMOD);
            }
            CalcKey::Enter => {
                self.flash = Some((FLASH_ENTER, Instant::now()));
                return self.handle_key(Keycode::Return, Mod::NOMOD);
            }
        }
        CalcEvent::None
    }

    /// Step the value of a Settings row to its next choice (wrapping), for a mouse click.
    fn modal_cycle(&mut self, row: usize) -> CalcEvent {
        match row {
            0 => {
                self.settings.entry = if self.settings.entry == EntryMode::Rpn { EntryMode::Algebraic } else { EntryMode::Rpn };
                self.all_clear();
            }
            1 => self.settings.angle = if self.settings.angle == Angle::Deg { Angle::Rad } else { Angle::Deg },
            _ => {
                self.settings.format = match self.settings.format {
                    Format::Auto => Format::Fix2,
                    Format::Fix2 => Format::Sci,
                    Format::Sci => Format::Auto,
                }
            }
        }
        CalcEvent::SettingsChanged
    }

    // ---- drawing -----------------------------------------------------------------

    fn mode_tags(&self) -> String {
        format!(
            "{} {}",
            if self.settings.entry == EntryMode::Rpn { "RPN" } else { "ALG" },
            if self.settings.angle == Angle::Deg { "DEG" } else { "RAD" }
        )
    }

    /// (small line, big line) shown in the display window.
    pub fn display_lines(&self) -> (String, String) {
        let f = self.settings.format;
        match self.settings.entry {
            EntryMode::Algebraic => {
                let big = match &self.result {
                    Some(Ok(v)) => format_number(*v, f),
                    Some(Err(e)) => crate::launcher::i18n::tr_s(e),
                    None => {
                        if self.expr.is_empty() { "0".to_string() } else { String::new() }
                    }
                };
                (self.expr.clone(), big)
            }
            EntryMode::Rpn => {
                let n = self.stack.len();
                let y = if self.entry.is_empty() {
                    if n >= 2 { format!("Y: {}", format_number(self.stack[n - 2], f)) } else { String::new() }
                } else if n >= 1 {
                    format!("Y: {}", format_number(self.stack[n - 1], f))
                } else {
                    String::new()
                };
                let big = match &self.result {
                    Some(Err(e)) => crate::launcher::i18n::tr_s(e),
                    _ => {
                        if !self.entry.is_empty() {
                            format!("{}_", self.entry)
                        } else if let Some(&x) = self.stack.last() {
                            format_number(x, f)
                        } else {
                            "0".to_string()
                        }
                    }
                };
                (y, big)
            }
        }
    }

    pub fn draw(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, x: i32, y: i32, w: u32, _h: u32) {
        let ink = tm.get_color("foreground");
        let muted = tm.get_color("muted");
        let paper = tm.get_color("widget_face_active");
        // The display panel is white (the theme's icon white), like the key tops.
        let lcd = tm.get_color("icon_white");
        let w = w as i32;

        // Display window: sunken, right aligned. The expression is set in Cavs, the
        // entry / result in Cavs at double size.
        let disp = display_rect(x, y, w as u32);
        ui::fill_rect(c, disp, lcd);
        ui::draw_rect(c, disp, ink);
        let (mut small, mut big) = self.display_lines();
        // While there is no result yet, the expression being typed takes the big line.
        if big.is_empty() && !small.is_empty() {
            big = format!("{small}_");
            small.clear();
        }
        fs.draw_text(c, disp.x() + 5, disp.y() + 3, &self.mode_tags(), crate::launcher::widgets::secondary_ink(tm));
        let max_small = disp.width() as i32 - 70;
        let shown_small = {
            let chars: Vec<char> = small.chars().collect();
            let mut start = 0;
            while start < chars.len()
                && fs.measure_display_text(&format!("{}{}", if start > 0 { ".." } else { "" }, chars[start..].iter().collect::<String>())) > max_small
            {
                start += 1;
            }
            format!("{}{}", if start > 0 { ".." } else { "" }, chars[start..].iter().collect::<String>())
        };
        let sw = fs.measure_display_text(&shown_small);
        fs.draw_display_text(c, disp.x() + disp.width() as i32 - sw - 6, disp.y() + 1, &shown_small, crate::launcher::widgets::secondary_ink(tm));
        let big = if big.is_empty() { "_".to_string() } else { big };
        let room = disp.width() as i32 - 12;
        let big = {
            // Too long even at 1x: keep the tail (what was typed last).
            let chars: Vec<char> = big.chars().collect();
            let mut start = 0;
            while start < chars.len() && fs.measure_display_text(&chars[start..].iter().collect::<String>()) > room {
                start += 1;
            }
            chars[start..].iter().collect::<String>()
        };
        match fs.display.as_ref().filter(|_| fs.display_cavs && fs.measure_display_text(&big) * 2 <= room) {
            Some(f) => {
                let bw = f.measure_scaled(&big, 2);
                f.draw_scaled(c, disp.x() + disp.width() as i32 - bw - 6, disp.y() + 20, &big, ink, 2);
            }
            None => {
                let bw = fs.measure_display_text(&big);
                let bx = (disp.x() + disp.width() as i32 - bw - 6).max(disp.x() + 4);
                fs.draw_display_text(c, bx, disp.y() + 28, &big, ink);
            }
        }

        // Keys: three QWERTY rows with the keyboard's stagger, Bksp and Enter, and a strip of
        // digits and operators (the keyboard types those too).
        let flash = self.flash.filter(|(_, t)| t.elapsed() < Duration::from_millis(160)).map(|(ch, _)| ch);
        let rpn = self.settings.entry == EntryMode::Rpn;
        for (r, key) in key_rects(x, y, w as u32) {
            match key {
                CalcKey::Fn(ch) => {
                    let k = ROWS.iter().flat_map(|row| row.iter()).find(|k| k.ch == ch).expect("key");
                    let label = if rpn { k.rpn_label } else { k.label };
                    draw_button(c, fs, tm, r, &ch.to_ascii_uppercase().to_string(), label, flash == Some(ch));
                }
                CalcKey::Bksp => draw_button(c, fs, tm, r, "BS", "<-", flash == Some(FLASH_BKSP)),
                CalcKey::Enter => draw_button(c, fs, tm, r, "Ret", if rpn { "enter" } else { "=" }, flash == Some(FLASH_ENTER)),
                CalcKey::Pad(ch) => draw_pad_key(c, fs, tm, r, ch, flash == Some(ch)),
            }
        }
        let _ = (paper, muted);

        if let Some(row) = self.modal {
            self.draw_modal(c, fs, tm, x, y, w, row);
        }
    }

    fn draw_modal(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, x: i32, y: i32, w: i32, row: usize) {
        let ink = tm.get_color("foreground");
        let muted = tm.get_color("muted");
        let paper = tm.get_color("widget_face_active");
        let r = modal_rect(x, y, w);
        ui::fill_rect(c, Rect::new(r.x() + 3, r.y() + 3, r.width(), r.height()), tm.get_color("widget_shadow"));
        ui::fill_rect(c, r, paper);
        ui::draw_rect_thick(c, r, ink);
        let title = crate::launcher::i18n::tr("Settings");
        let tw = fs.measure_display_text(title);
        fs.draw_display_text(c, r.x() + (r.width() as i32 - tw) / 2, r.y() + 5, title, ink);
        ui::line(c, r.x() + 4, r.y() + 20, r.x() + r.width() as i32 - 5, r.y() + 20, muted);
        let entry = if self.settings.entry == EntryMode::Rpn { "RPN" } else { crate::launcher::i18n::tr("Algebraic") };
        let angle = if self.settings.angle == Angle::Deg { crate::launcher::i18n::tr("Degrees") } else { crate::launcher::i18n::tr("Radians") };
        let format = match self.settings.format {
            Format::Auto => crate::launcher::i18n::tr("Auto"),
            Format::Fix2 => crate::launcher::i18n::tr("Fixed 2"),
            Format::Sci => crate::launcher::i18n::tr("Scientific"),
        };
        for (i, (label, value)) in [(crate::launcher::i18n::tr("Entry"), entry), (crate::launcher::i18n::tr("Angle"), angle), (crate::launcher::i18n::tr("Format"), format)].iter().enumerate() {
            let ry = r.y() + 25 + i as i32 * 16;
            let sel = i == row;
            let plate = Rect::new(r.x() + 6, ry, r.width() - 12, 15);
            if sel {
                ui::fill_rect(c, plate, ink);
            }
            let fg = if sel { paper } else { ink };
            fs.draw_text(c, plate.x() + 5, ry + 1, label, fg);
            let vw = fs.measure_text(value);
            fs.draw_text(c, plate.x() + plate.width() as i32 - vw - 5, ry + 1, value, fg);
        }
    }
}

/// A flat little key of the digit strip.
fn draw_pad_key(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, r: Rect, ch: char, pressed: bool) {
    let ink = tm.get_color("foreground");
    let paper = tm.get_color("icon_white");
    ui::fill_rect(c, r, if pressed { ink } else { paper });
    ui::draw_rect(c, r, ink);
    let t = ch.to_string();
    let tw = fs.measure_text(&t);
    fs.draw_text(c, r.x() + (r.width() as i32 - tw) / 2, r.y() + 1, &t, if pressed { paper } else { ink });
}

/// A classic bevelled button: paper face, 1px frame, hard 2px shadow. `legend` is
/// the keyboard key (small, top left), `label` the function (bottom).
fn draw_button(c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, r: Rect, legend: &str, label: &str, pressed: bool) {
    let ink = tm.get_color("foreground");
    // Key tops are white (the theme's icon white); a pressed key inverts to ink on the key colour.
    let paper = tm.get_color("icon_white");
    let shadow = tm.get_color("widget_shadow");
    let face = if pressed { Rect::new(r.x() + 1, r.y() + 1, r.width(), r.height()) } else { r };
    if !pressed {
        ui::fill_rect(c, Rect::new(r.x() + 2, r.y() + 2, r.width(), r.height()), if shadow == paper { ink } else { shadow });
    }
    ui::fill_rect(c, face, if pressed { ink } else { paper });
    ui::draw_rect(c, face, ink);
    let fg = if pressed { paper } else { ink };
    fs.draw_display_text(c, face.x() + 3, face.y(), legend, fg);
    let lw = fs.measure_text(label);
    let room = face.width() as i32 - 4;
    if lw <= room {
        fs.draw_text(c, face.x() + (face.width() as i32 - lw) / 2, face.y() + 12, label, fg);
    } else {
        // Four-letter names ("acos", "10^x") are a pixel or two wider than the key face: set them a
        // pixel tighter per letter so they clear the key's frame on both sides.
        let n = label.chars().count() as i32;
        let tight = lw - (n - 1);
        let mut x = face.x() + (face.width() as i32 - tight + 1) / 2;
        let mut buf = [0u8; 4];
        for ch in label.chars() {
            let g: &str = ch.encode_utf8(&mut buf);
            fs.draw_text(c, x, face.y() + 12, g, fg);
            x += fs.measure_text(g) - 1;
        }
    }
}

/// Keep the right end of `text` when it does not fit.
fn tail_fit(fs: &FontSystem, text: &str, max_w: i32) -> String {
    if fs.measure_text(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut start = 0;
    while start < chars.len() && fs.measure_text(&format!("..{}", chars[start..].iter().collect::<String>())) > max_w {
        start += 1;
    }
    format!("..{}", chars[start..].iter().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> Result<f64, String> {
        evaluate(s, Angle::Deg, 0.0, 0.5)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn precedence_parentheses_and_associativity() {
        assert_eq!(ev("2+3*4"), Ok(14.0));
        assert_eq!(ev("(2+3)*4"), Ok(20.0));
        assert_eq!(ev("2^3^2"), Ok(512.0), "right associative");
        assert_eq!(ev("-2^2"), Ok(-4.0), "unary minus binds looser than ^");
        assert_eq!(ev("2^-2"), Ok(0.25));
        assert_eq!(ev("10-4-3"), Ok(3.0));
        assert_eq!(ev("100/10/5"), Ok(2.0));
        assert_eq!(ev("7 mod 3"), Ok(1.0));
        assert_eq!(ev("-7 mod 3"), Ok(2.0), "mod is never negative");
        assert_eq!(ev("1.5+.5"), Ok(2.0));
    }

    #[test]
    fn functions_constants_and_implicit_multiplication() {
        assert!(close(ev("sqrt(16)").unwrap(), 4.0));
        assert!(close(ev("cbrt(27)").unwrap(), 3.0));
        assert!(close(ev("ln(e)").unwrap(), 1.0));
        assert_eq!(ev("log(1000)"), Ok(3.0));
        assert!(close(ev("exp(0)").unwrap(), 1.0));
        assert!(close(ev("pi").unwrap(), std::f64::consts::PI));
        assert!(close(ev("2pi").unwrap(), 2.0 * std::f64::consts::PI));
        assert!(close(ev("3(4+1)").unwrap(), 15.0));
        assert!(close(ev("2sqrt(9)").unwrap(), 6.0));
        assert_eq!(ev("abs(-5)"), Ok(5.0));
        assert_eq!(ev("5!"), Ok(120.0));
        assert_eq!(ev("0!"), Ok(1.0));
        assert_eq!(ev("3!+1"), Ok(7.0));
        assert_eq!(evaluate("ans*2", Angle::Deg, 21.0, 0.0), Ok(42.0));
        assert_eq!(ev("rnd"), Ok(0.5));
    }

    #[test]
    fn angle_units() {
        assert_eq!(evaluate("sin(30)", Angle::Deg, 0.0, 0.0).map(|v| (v * 1e9).round() / 1e9), Ok(0.5));
        assert_eq!(evaluate("sin(180)", Angle::Deg, 0.0, 0.0), Ok(0.0), "exact zero, not 1e-16");
        assert_eq!(evaluate("cos(90)", Angle::Deg, 0.0, 0.0), Ok(0.0));
        assert!(close(evaluate("sin(pi/2)", Angle::Rad, 0.0, 0.0).unwrap(), 1.0));
        assert!(close(evaluate("atan(1)", Angle::Deg, 0.0, 0.0).unwrap(), 45.0));
        assert!(close(evaluate("acos(0)", Angle::Deg, 0.0, 0.0).unwrap(), 90.0));
        assert!(close(evaluate("atan(1)", Angle::Rad, 0.0, 0.0).unwrap(), std::f64::consts::FRAC_PI_4));
        assert_eq!(evaluate("tan(90)", Angle::Deg, 0.0, 0.0), Err("Domain".into()));
    }

    #[test]
    fn errors_are_reported_not_panicked() {
        assert_eq!(ev("1/0"), Err("Div by 0".into()));
        assert_eq!(ev("5 mod 0"), Err("Div by 0".into()));
        assert_eq!(ev("sqrt(-1)"), Err("Domain".into()));
        assert_eq!(ev("ln(0)"), Err("Domain".into()));
        assert_eq!(ev("asin(2)"), Err("Domain".into()));
        assert_eq!(ev("2+"), Err("Syntax".into()));
        assert_eq!(ev("*3"), Err("Syntax".into()));
        assert_eq!(ev("2 $ 3"), Err("Syntax".into()));
        assert_eq!(ev("sin"), Err("Syntax".into()));
        assert_eq!(ev(""), Err("Empty".into()));
        assert_eq!(ev("171!"), Err("Overflow".into()));
        assert_eq!(ev("2.5!"), Err("Domain".into()));
        assert_eq!(ev("10^400"), Err("Overflow".into()));
        assert_eq!(ev("(-8)^0.5"), Err("Domain".into()));
    }

    #[test]
    fn unclosed_parentheses_are_forgiven() {
        assert_eq!(ev("(2+3"), Err("Syntax".into()), "a bare open paren is an error");
        assert!(close(ev("sin(30").unwrap(), 0.5));
        assert_eq!(ev("(2+3))"), Ok(5.0));
    }

    #[test]
    fn number_formatting() {
        assert_eq!(format_number(14.0, Format::Auto), "14");
        assert_eq!(format_number(0.1 + 0.2, Format::Auto), "0.3", "float noise is cleaned");
        assert_eq!(format_number(1.0 / 3.0, Format::Auto), "0.3333333333");
        assert_eq!(format_number(12345.678901234, Format::Auto), "12345.6789", "ten significant digits");
        assert_eq!(format_number(-2.5, Format::Auto), "-2.5");
        assert_eq!(format_number(1.5e12, Format::Auto), "1.5e12");
        assert_eq!(format_number(1.0e-7, Format::Auto), "1e-7");
        assert_eq!(format_number(0.0, Format::Auto), "0");
        assert_eq!(format_number(3.14159, Format::Fix2), "3.14");
        assert_eq!(format_number(0.0, Format::Fix2), "0.00");
        assert_eq!(format_number(1234.5, Format::Sci), "1.234500e3");
    }

    fn typed(settings: Settings, input: &str) -> Calc {
        let mut c = Calc::new(settings);
        for ch in input.chars() {
            match ch {
                '\n' => {
                    c.handle_key(Keycode::Return, Mod::NOMOD);
                }
                _ => c.handle_text(&ch.to_string()),
            }
        }
        c
    }

    fn rpn() -> Settings {
        Settings { entry: EntryMode::Rpn, ..Settings::default() }
    }

    #[test]
    fn algebraic_typing_and_evaluation() {
        let c = typed(Settings::default(), "12*(3+4)\n");
        assert_eq!(c.result(), Some(&Ok(84.0)));
        assert_eq!(c.display_lines().1, "84");
        // Function keys insert their call: Q = sin(
        let c = typed(Settings::default(), "q30)\n");
        assert_eq!(c.expression(), "sin(30)");
        assert!(close(*c.result().unwrap().as_ref().unwrap(), 0.5));
    }

    #[test]
    fn after_equals_operators_continue_and_digits_restart() {
        let mut c = typed(Settings::default(), "2+3\n");
        c.handle_text("*2");
        assert_eq!(c.expression(), "5*2", "an operator continues from the result");
        c.handle_key(Keycode::Return, Mod::NOMOD);
        assert_eq!(c.result(), Some(&Ok(10.0)));
        c.handle_text("7");
        assert_eq!(c.expression(), "7", "a digit starts a fresh expression");
        assert!(c.result().is_none());
    }

    #[test]
    fn backspace_removes_a_whole_function_token() {
        let mut c = typed(Settings::default(), "2+q");
        assert_eq!(c.expression(), "2+sin(");
        c.handle_key(Keycode::Backspace, Mod::NOMOD);
        assert_eq!(c.expression(), "2+");
        c.handle_key(Keycode::Backspace, Mod::NOMOD);
        assert_eq!(c.expression(), "2");
        let mut c = typed(Settings::default(), "k");
        assert_eq!(c.expression(), " mod ");
        c.handle_key(Keycode::Backspace, Mod::NOMOD);
        assert_eq!(c.expression(), "");
    }

    #[test]
    fn the_m_key_clears_everything() {
        let c = typed(Settings::default(), "123m");
        assert_eq!(c.expression(), "");
        assert_eq!(c.display_lines().1, "0");
    }

    #[test]
    fn every_letter_key_is_mapped_in_both_modes() {
        let mut seen = std::collections::HashSet::new();
        for row in ROWS {
            for key in row {
                assert!(seen.insert(key.ch), "{} appears twice", key.ch);
                assert!(alg_insert(key.ch).is_some() || key.ch == 'm', "{} has no algebraic action", key.ch);
                assert!(key.label.len() <= 4 && key.rpn_label.len() <= 5, "labels must fit a 28 px button");
            }
        }
        assert_eq!(seen.len(), 26, "all 26 letters are function keys");
    }

    #[test]
    fn rpn_basic_arithmetic() {
        let c = typed(rpn(), "3\n4+");
        assert_eq!(c.x(), Some(7.0));
        let c = typed(rpn(), "2\n3^");
        assert_eq!(c.x(), Some(8.0));
        let c = typed(rpn(), "10\n4-");
        assert_eq!(c.x(), Some(6.0), "Y - X");
        let c = typed(rpn(), "20\n4/");
        assert_eq!(c.x(), Some(5.0));
        // Operators commit the number being typed.
        let c = typed(rpn(), "5\n6*");
        assert_eq!(c.x(), Some(30.0));
        let c = typed(rpn(), "3\n4+*");
        assert_eq!(c.stack().len(), 1, "(3+4) then * needs a second operand");
        assert_eq!(c.result(), Some(&Err("Need 2".into())));
    }

    #[test]
    fn rpn_functions_stack_ops_and_errors() {
        let c = typed(rpn(), "16r");
        assert_eq!(c.x(), Some(4.0), "R = sqrt applies to the typed number");
        let c = typed(rpn(), "3\n4z");
        assert_eq!(c.stack(), &[4.0, 3.0], "Z swaps");
        let c = typed(rpn(), "3\n4x");
        assert_eq!(c.stack(), &[3.0], "X drops");
        let c = typed(rpn(), "7n");
        assert_eq!(c.stack(), &[7.0, 7.0], "N duplicates");
        let c = typed(rpn(), "5\n\n");
        assert_eq!(c.stack(), &[5.0, 5.0], "Enter on an empty entry duplicates X");
        let c = typed(rpn(), "5c");
        assert_eq!(c.x(), None, "still typing");
        assert_eq!(c.entry_text(), "-5");
        let c = typed(rpn(), "5\nc");
        assert_eq!(c.x(), Some(-5.0), "C negates X");
        let c = typed(rpn(), "1\n0/");
        assert_eq!(c.result(), Some(&Err("Div by 0".into())));
        assert_eq!(c.stack(), &[1.0, 0.0].map(|v: f64| v), "operands survive an error");
        let c = typed(rpn(), "5\n1\nm");
        assert!(c.stack().is_empty(), "M clears the stack");
        let c = typed(rpn(), "g");
        assert!(close(c.x().unwrap(), std::f64::consts::PI));
    }

    #[test]
    fn rpn_entry_editing() {
        let mut c = typed(rpn(), "1.5");
        assert_eq!(c.entry_text(), "1.5");
        c.handle_text(".");
        assert_eq!(c.entry_text(), "1.5", "one decimal point only");
        c.handle_key(Keycode::Backspace, Mod::NOMOD);
        assert_eq!(c.entry_text(), "1.");
        let c = typed(rpn(), ".5\n");
        assert_eq!(c.x(), Some(0.5), "a leading point becomes 0.");
        let (small, big) = typed(rpn(), "3\n4").display_lines();
        assert_eq!((small.as_str(), big.as_str()), ("Y: 3", "4_"));
    }

    #[test]
    fn rpn_trig_uses_the_angle_setting() {
        let c = typed(rpn(), "30q");
        assert!(close(c.x().unwrap(), 0.5));
        let s = Settings { entry: EntryMode::Rpn, angle: Angle::Rad, ..Settings::default() };
        let c = typed(s, "0q");
        assert_eq!(c.x(), Some(0.0));
    }

    #[test]
    fn the_settings_modal_changes_modes_and_reports_it() {
        let mut c = Calc::new(Settings::default());
        c.toggle_modal();
        assert_eq!(c.modal, Some(0));
        c.handle_text("5"); // typing is ignored while the modal is open
        assert_eq!(c.expression(), "");
        assert_eq!(c.handle_key(Keycode::Right, Mod::NOMOD), CalcEvent::SettingsChanged);
        assert_eq!(c.settings.entry, EntryMode::Rpn);
        assert_eq!(c.handle_key(Keycode::Right, Mod::NOMOD), CalcEvent::None, "already RPN");
        c.handle_key(Keycode::Down, Mod::NOMOD);
        assert_eq!(c.handle_key(Keycode::C, Mod::NOMOD), CalcEvent::SettingsChanged, "C acts as Right here too");
        assert_eq!(c.settings.angle, Angle::Rad);
        c.handle_key(Keycode::Down, Mod::NOMOD);
        c.handle_key(Keycode::Right, Mod::NOMOD);
        c.handle_key(Keycode::Right, Mod::NOMOD);
        assert_eq!(c.settings.format, Format::Sci);
        c.toggle_modal(); // the app calls this when Tab is released
        assert!(c.modal.is_none(), "Tab closes the modal");
        assert_eq!(c.handle_key(Keycode::Escape, Mod::NOMOD), CalcEvent::Close, "Esc then leaves the app");
    }

    #[test]
    fn settings_round_trip_through_config_ints() {
        let s = Settings { entry: EntryMode::Rpn, angle: Angle::Rad, format: Format::Fix2 };
        let (a, b, c) = s.to_ints();
        assert_eq!(Settings::from_ints(a, b, c), s);
        assert_eq!(Settings::from_ints(0, 0, 0), Settings::default());
        assert_eq!(Settings::from_ints(9, 9, 9).format, Format::Auto, "unknown values fall back");
    }

    #[test]
    fn display_lines_in_algebraic_mode() {
        let c = Calc::new(Settings::default());
        assert_eq!(c.display_lines(), (String::new(), "0".to_string()));
        let c = typed(Settings::default(), "1/0\n");
        assert_eq!(c.display_lines().1, "Div by 0");
    }

    const AREA: (i32, i32, u32) = (0, 16, 320);

    fn center(key: CalcKey) -> (i32, i32) {
        let (r, _) = key_rects(AREA.0, AREA.1, AREA.2).into_iter().map(|(r, k)| (r, k)).find(|(_, k)| *k == key).expect("key");
        (r.x() + r.width() as i32 / 2, r.y() + r.height() as i32 / 2)
    }

    fn tap(c: &mut Calc, key: CalcKey) -> CalcEvent {
        let (x, y) = center(key);
        c.click(x, y, AREA.0, AREA.1, AREA.2)
    }

    #[test]
    fn every_key_is_on_screen_distinct_and_clickable() {
        let keys = key_rects(AREA.0, AREA.1, AREA.2);
        assert_eq!(keys.len(), 26 + 2 + PAD_KEYS.len());
        for (i, (a, ka)) in keys.iter().enumerate() {
            assert!(a.x() >= 0 && a.right() <= 320 && a.bottom() <= 169, "{ka:?} fits on the screen: {a:?}");
            for (b, kb) in &keys[i + 1..] {
                assert!(!a.has_intersection(*b), "{ka:?} overlaps {kb:?}");
            }
        }
        // A letter key does what typing the letter does, in both modes.
        for entry in [EntryMode::Algebraic, EntryMode::Rpn] {
            for (_, key) in keys.iter().filter(|(_, k)| matches!(k, CalcKey::Fn(_))) {
                let CalcKey::Fn(ch) = key else { unreachable!() };
                let mut a = Calc::new(Settings { entry, ..Settings::from_ints(0, 0, 0) });
                let mut b = Calc::new(Settings { entry, ..Settings::from_ints(0, 0, 0) });
                a.rng = 1;
                b.rng = 1;
                if entry == EntryMode::Rpn {
                    a.handle_text("4");
                    b.handle_text("4");
                } else {
                    a.handle_text("4");
                    b.handle_text("4");
                }
                tap(&mut a, *key);
                b.handle_text(&ch.to_string());
                assert_eq!(a.display_lines(), b.display_lines(), "key {ch} in {entry:?}");
            }
        }
    }

    #[test]
    fn clicking_computes_in_algebraic_and_rpn() {
        let mut c = Calc::new(Settings::from_ints(0, 0, 0));
        for ch in ['2', '+', '3'] {
            tap(&mut c, CalcKey::Pad(ch));
        }
        tap(&mut c, CalcKey::Enter);
        assert_eq!(c.display_lines().1, "5");
        tap(&mut c, CalcKey::Fn('m'));
        assert_eq!(c.display_lines().1, "0", "AC");
        tap(&mut c, CalcKey::Pad('7'));
        tap(&mut c, CalcKey::Pad('8'));
        tap(&mut c, CalcKey::Bksp);
        assert_eq!(c.expression(), "7");

        let mut r = Calc::new(Settings::from_ints(1, 0, 0));
        tap(&mut r, CalcKey::Pad('2'));
        tap(&mut r, CalcKey::Enter);
        tap(&mut r, CalcKey::Pad('3'));
        tap(&mut r, CalcKey::Pad('+'));
        assert_eq!(r.stack(), &[5.0]);
    }

    #[test]
    fn display_opens_settings_and_rows_cycle_values() {
        let mut c = Calc::new(Settings::from_ints(0, 0, 0));
        let d = display_rect(AREA.0, AREA.1, AREA.2);
        assert_eq!(c.click(d.x() + 5, d.y() + 5, AREA.0, AREA.1, AREA.2), CalcEvent::None);
        assert!(c.modal.is_some());
        let m = modal_rect(AREA.0, AREA.1, AREA.2 as i32);
        // Row 1 (Angle) toggles; the third click is a full cycle back for the 3-way Format row.
        assert_eq!(c.click(m.x() + 20, m.y() + 25 + 16 + 3, AREA.0, AREA.1, AREA.2), CalcEvent::SettingsChanged);
        assert_eq!(c.settings.angle, Angle::Rad);
        for _ in 0..3 {
            c.click(m.x() + 20, m.y() + 25 + 32 + 3, AREA.0, AREA.1, AREA.2);
        }
        assert_eq!(c.settings.format, Format::Auto);
        c.click(m.x() + 20, m.y() + 25 + 3, AREA.0, AREA.1, AREA.2);
        assert_eq!(c.settings.entry, EntryMode::Rpn);
        // Keys behind an open dialog do not fire; a click outside closes it.
        let before = c.display_lines();
        let (x, y) = center(CalcKey::Pad('5'));
        c.click(x, y, AREA.0, AREA.1, AREA.2);
        assert_eq!(c.display_lines(), before);
        assert!(c.modal.is_none());
    }
}
