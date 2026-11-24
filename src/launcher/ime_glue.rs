//! The launcher side of the Japanese input method (`crate::ime`): turns `Input` events into
//! engine keys, hands committed text on as ordinary `Input::Text`, and draws the composition.
//!
//! The one seam is [`ImeGlue::process`], called once per frame on the gathered input list before
//! anything else looks at it. Committed text arrives downstream as the same `Input::Text` a
//! keyboard would have produced, so the line editors, Text Editor, Terminal, Agent chat and Sheet
//! need no changes.

use sdl2::keyboard::{Keycode, Mod};
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::font::FontSystem;
use crate::ime::{Ime, Key, KeyIn, Mode, View, CAND_PAGE};
use crate::platform::keys::Input;
use crate::theme::ThemeManager;
use crate::ui;

/// The key that turns the input method on and off. Shift + it cycles あ → ア → ｱ → Ａ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Toggle {
    pub key: Keycode,
    pub ctrl: bool,
    pub alt: bool,
}

pub const DEFAULT_TOGGLE: &str = "ctrl+space";

/// Parse `ctrl+space`, `ctrl+j`, `alt+space`, `f10`, `grave`... (SDL key names, case-insensitive).
/// Plain letters and digits are refused: they would swallow typing.
pub fn parse_toggle(spec: &str) -> Option<Toggle> {
    let mut ctrl = false;
    let mut alt = false;
    let mut name = "";
    for part in spec.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            _ => name = part,
        }
    }
    let key = Keycode::from_name(if name.eq_ignore_ascii_case("space") { "Space" } else { name })?;
    let printable = (0x21..=0x7e).contains(&(i32::from(key)));
    if printable && !ctrl && !alt {
        return None;
    }
    Some(Toggle { key, ctrl, alt })
}

pub struct ImeGlue {
    ime: Ime,
    enabled: bool,
    toggle: Toggle,
    /// The text event that follows a Space key press we already used must not insert a space.
    swallow_space_text: bool,
    /// Keys whose press the engine used: their release is dropped too.
    swallowed: Vec<Keycode>,
    /// When the learning history first became unsaved (it is written after a quiet period).
    learn_dirty_since: Option<std::time::Instant>,
}

/// Where the dictionary and the learning history live (`HARBOUR_IME_DIR` overrides, for tests).
pub fn data_dir() -> std::path::PathBuf {
    crate::ime::data_dir()
}

const LEARN_FLUSH_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

fn is_printable_key(key: Keycode) -> bool {
    (0x21..=0x7e).contains(&(i32::from(key)))
}

fn engine_key(key: Keycode) -> Option<Key> {
    Some(match key {
        Keycode::Return | Keycode::KpEnter => Key::Enter,
        Keycode::Backspace => Key::Backspace,
        Keycode::Delete => Key::Delete,
        Keycode::Escape => Key::Esc,
        Keycode::Tab => Key::Tab,
        Keycode::Left => Key::Left,
        Keycode::Right => Key::Right,
        Keycode::Up => Key::Up,
        Keycode::Down => Key::Down,
        Keycode::Home => Key::Home,
        Keycode::End => Key::End,
        Keycode::Space => Key::Space,
        Keycode::F1 => Key::F(1),
        Keycode::F2 => Key::F(2),
        Keycode::F3 => Key::F(3),
        Keycode::F4 => Key::F(4),
        Keycode::F5 => Key::F(5),
        Keycode::F6 => Key::F(6),
        Keycode::F7 => Key::F(7),
        Keycode::F8 => Key::F(8),
        Keycode::F9 => Key::F(9),
        Keycode::F10 => Key::F(10),
        Keycode::F11 => Key::F(11),
        Keycode::F12 => Key::F(12),
        k if is_printable_key(k) => Key::Char(char::from_u32(i32::from(k) as u32)?.to_ascii_lowercase()),
        _ => Key::Other,
    })
}

fn push_text(out: &mut Vec<Input>, text: String) {
    if text.is_empty() {
        return;
    }
    // Merge with a text event we just emitted, so a burst of commits stays one event.
    if let Some(Input::Text(prev)) = out.last_mut() {
        prev.push_str(&text);
    } else {
        out.push(Input::Text(text));
    }
}

impl ImeGlue {
    pub fn new(enabled: bool, toggle: Toggle, wide_punct: bool) -> Self {
        let mut ime = Ime::new();
        ime.wide_punct = wide_punct;
        Self { ime, enabled, toggle, swallow_space_text: false, swallowed: Vec::new(), learn_dirty_since: None }
    }

    /// From the shared config: `ime` (1 = on, default), `ime_toggle` (`ctrl+space`), `ime_wide_punct` (0).
    pub fn from_config(cfg: &crate::system::config::Config) -> Self {
        let toggle = parse_toggle(&cfg.get_str("ime_toggle", DEFAULT_TOGGLE))
            .or_else(|| parse_toggle(DEFAULT_TOGGLE))
            .expect("default toggle parses");
        Self::new(cfg.get_int("ime", 1) != 0, toggle, cfg.get_int("ime_wide_punct", 0) != 0)
    }

    /// Load the dictionary (if the user installed one) and the learning history from `dir`.
    /// A missing or damaged dictionary is not an error: the input method then converts to kana only.
    /// Returns a short note for the log.
    pub fn load_data(&mut self, dir: &std::path::Path) -> String {
        let learn = dir.join("learn");
        self.ime.load_learn(&learn);
        match crate::ime::dict::Dict::open(&dir.join(crate::ime::DICT_FILE)) {
            Ok(d) => {
                let note = format!("ime: dictionary {} keys, {} KiB", d.len(), d.file_size() / 1024);
                self.ime.set_dict(Some(d));
                note
            }
            Err(e) => format!("ime: no dictionary ({e}); kana input only"),
        }
    }

    /// Whether committed conversions are remembered (off in the terminal: commands should not leak into history).
    pub fn set_learning(&mut self, on: bool) {
        self.ime.conv.learning = on;
    }

    /// Call every frame: writes the learning history once it has been unsaved for a while.
    pub fn tick(&mut self) {
        if !self.ime.learn_dirty() {
            self.learn_dirty_since = None;
            return;
        }
        let since = *self.learn_dirty_since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed() >= LEARN_FLUSH_AFTER && !self.ime.is_composing() {
            self.flush();
        }
    }

    /// Write the learning history now (on exit).
    pub fn flush(&mut self) {
        let _ = self.ime.flush_learn();
        self.learn_dirty_since = None;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn mode(&self) -> Mode {
        self.ime.mode()
    }

    pub fn is_composing(&self) -> bool {
        self.ime.is_composing()
    }

    pub fn view(&self) -> Option<View> {
        self.ime.view()
    }

    /// Mode letter for the status bar: shown while the screen takes text (`text_screen`), hidden otherwise.
    pub fn indicator(&self, text_screen: bool) -> &'static str {
        if self.enabled && (text_screen || !self.ime.mode().is_direct()) {
            self.ime.mode().indicator()
        } else {
            ""
        }
    }

    fn is_toggle(&self, key: Keycode, mods: Mod) -> bool {
        key == self.toggle.key
            && mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD) == self.toggle.ctrl
            && mods.intersects(Mod::LALTMOD | Mod::RALTMOD) == self.toggle.alt
    }

    /// Run the frame's input through the input method. `active` is false on screens that take no
    /// text (the home grid, the lock screen, menus): the keys then pass untouched.
    pub fn process(&mut self, inputs: Vec<Input>, active: bool) -> Vec<Input> {
        if !self.enabled {
            return inputs;
        }
        let mut out = Vec::with_capacity(inputs.len() + 1);
        if !active {
            if self.ime.is_composing() {
                push_text(&mut out, self.ime.set_mode(self.ime.mode()));
            }
            out.extend(inputs);
            return out;
        }
        for input in inputs {
            match input {
                Input::Key { key, mods, repeat } => {
                    if self.is_toggle(key, mods) {
                        if !repeat {
                            let shift = mods.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
                            let text = if shift { self.ime.cycle_kana() } else { self.ime.toggle() };
                            push_text(&mut out, text);
                        }
                        if key == Keycode::Space {
                            self.swallow_space_text = true;
                        }
                        self.swallowed.push(key);
                        continue;
                    }
                    self.swallow_space_text = false;
                    let ctrl = mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
                    let alt = mods.intersects(Mod::LALTMOD | Mod::RALTMOD);
                    let shift = mods.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
                    // Typed characters arrive as text events; their key presses are not needed.
                    if is_printable_key(key) && !ctrl && !alt {
                        if self.ime.mode().is_direct() {
                            out.push(Input::Key { key, mods, repeat });
                        } else {
                            self.swallowed.push(key);
                        }
                        continue;
                    }
                    let Some(k) = engine_key(key) else {
                        out.push(Input::Key { key, mods, repeat });
                        continue;
                    };
                    let o = self.ime.on_key(KeyIn { key: k, ctrl, shift, alt });
                    push_text(&mut out, o.commit);
                    if o.consumed {
                        if k == Key::Space {
                            self.swallow_space_text = true;
                        }
                        self.swallowed.push(key);
                    } else {
                        out.push(Input::Key { key, mods, repeat });
                    }
                }
                Input::KeyUp { key } => {
                    if let Some(i) = self.swallowed.iter().position(|k| *k == key) {
                        self.swallowed.remove(i);
                    } else {
                        out.push(Input::KeyUp { key });
                    }
                }
                Input::Text(text) => {
                    if self.swallow_space_text && text == " " {
                        self.swallow_space_text = false;
                        continue;
                    }
                    self.swallow_space_text = false;
                    if self.ime.mode().is_direct() {
                        out.push(Input::Text(text));
                        continue;
                    }
                    for c in text.chars() {
                        if c.is_control() {
                            push_text(&mut out, self.ime.set_mode(self.ime.mode()));
                            push_text(&mut out, c.to_string());
                            continue;
                        }
                        let key = if c == ' ' { Key::Space } else { Key::Char(c) };
                        let o = self.ime.on_key(KeyIn::new(key));
                        push_text(&mut out, o.commit);
                        if !o.consumed {
                            push_text(&mut out, c.to_string());
                        }
                    }
                }
                // Anything that moves attention elsewhere settles the text first.
                other @ (Input::Voice(true) | Input::Menu | Input::MouseButton { down: true, .. } | Input::Quit) => {
                    if self.ime.is_composing() {
                        push_text(&mut out, self.ime.set_mode(self.ime.mode()));
                    }
                    out.push(other);
                }
                other => out.push(other),
            }
        }
        out
    }

    // ---- drawing -----------------------------------------------------------------------------

    /// Draw the composition (an underlined line at the bottom of `area`) and, while converting, the
    /// candidate window above it. Nothing is drawn when there is no composition.
    pub fn draw(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager, area: Rect) {
        let Some(view) = self.ime.view().filter(|_| self.enabled) else { return };
        let ink = tm.get_color("foreground");
        let paper = tm.get_color("background");
        let layout = Layout::new(fs, &view, area);
        // -- input window: mode cell + text
        let win = layout.window;
        ui::fill_rect(c, Rect::new(win.x() + 1, win.y() + 1, win.width(), win.height()), ink); // shadow
        ui::fill_rect(c, win, paper);
        ui::draw_rect_thick(c, win, ink);
        let cell = Rect::new(win.x() + 2, win.y() + 2, MODE_W as u32, win.height() - 4);
        ui::fill_rect(c, cell, ink);
        let glyph = view.mode.indicator();
        fs.draw_text(c, cell.x() + (MODE_W - fs.measure_text(glyph)) / 2, cell.y() + 1, glyph, paper);
        let text_x = layout.text_x;
        let text_y = win.y() + 3;
        let clip = Rect::new(win.x() + 2 + MODE_W + 1, win.y() + 2, layout.text_w as u32, win.height() - 4);
        c.set_clip_rect(clip);
        let chars: Vec<char> = view.preedit.chars().collect();
        let x_at = |i: usize| -> i32 { text_x - layout.scroll + fs.measure_text(&chars[..i].iter().collect::<String>()) };
        let converting = view.cand.is_some();
        let total = x_at(chars.len()) - (text_x - layout.scroll);
        if converting {
            ui::fill_rect(c, Rect::new(text_x - layout.scroll, text_y - 1, total.max(1) as u32, 14), ink);
            fs.draw_text(c, text_x - layout.scroll, text_y, &view.preedit, paper);
        } else {
            fs.draw_text(c, text_x - layout.scroll, text_y, &view.preedit, ink);
            // Solid underline for the kana, dotted under the romaji still being typed.
            let uy = text_y + 13;
            let (ps, pe) = view.pending.unwrap_or((chars.len(), chars.len()));
            ui::line(c, x_at(0), uy, x_at(ps) - 1, uy, ink);
            let (px0, px1) = (x_at(ps), x_at(pe));
            for x in (px0..px1).step_by(2) {
                ui::fill_rect(c, Rect::new(x, uy, 1, 1), ink);
            }
            if pe < chars.len() {
                ui::line(c, x_at(pe), uy, x_at(chars.len()) - 1, uy, ink);
            }
            if let Some(cp) = view.caret {
                let cx = x_at(cp);
                ui::fill_rect(c, Rect::new(cx, text_y - 1, 1, 14), ink);
            }
        }
        c.set_clip_rect(None);
        // -- candidate window above it
        if let Some(cand) = &view.cand {
            let page = cand.sel / CAND_PAGE;
            let first = page * CAND_PAGE;
            let rows = (cand.items.len() - first).min(CAND_PAGE);
            let pages = cand.items.len().div_ceil(CAND_PAGE);
            let cw = layout.cand_w;
            let ch = rows as i32 * CAND_ROW + 4;
            let cx = layout.cand_x;
            let cy = win.y() - ch - 3;
            let r = Rect::new(cx, cy, cw as u32, ch as u32);
            ui::fill_rect(c, Rect::new(cx + 2, cy + 2, cw as u32, ch as u32), ink); // 2px shadow
            ui::fill_rect(c, r, paper);
            ui::draw_rect_thick(c, r, ink);
            ui::line(c, cx + NUM_W + 2, cy + 2, cx + NUM_W + 2, cy + ch - 3, ink);
            for (i, item) in cand.items.iter().skip(first).take(rows).enumerate() {
                let ry = cy + 2 + i as i32 * CAND_ROW;
                let (fg, row) = if first + i == cand.sel {
                    ui::fill_rect(c, Rect::new(cx + 2, ry, (cw - 4) as u32, CAND_ROW as u32), ink);
                    (paper, true)
                } else {
                    (ink, false)
                };
                let _ = row;
                fs.draw_text(c, cx + 2 + (NUM_W - 6) / 2, ry, &format!("{}", i + 1), fg);
                let shown = crate::chrome::fit_text(fs, item, cw - NUM_W - 8);
                fs.draw_text(c, cx + NUM_W + 6, ry, &shown, fg);
            }
            if pages > 1 {
                // "2/3" under the window, in the corner, so the user knows more candidates follow.
                let label = format!("{}/{}", page + 1, pages);
                let lw = fs.measure_text(&label) + 4;
                let lr = Rect::new(cx + cw - lw, cy + ch + 1, lw as u32, 13);
                ui::fill_rect(c, lr, paper);
                ui::draw_rect_thick(c, lr, ink);
                fs.draw_text(c, lr.x() + 2, lr.y(), &label, ink);
            }
        }
    }
}

const MODE_W: i32 = 15;
const CAND_ROW: i32 = 13;
const NUM_W: i32 = 11;
const WIN_H: i32 = 19;

struct Layout {
    window: Rect,
    text_x: i32,
    text_w: i32,
    scroll: i32,
    cand_x: i32,
    cand_w: i32,
}

impl Layout {
    fn new(fs: &FontSystem, view: &View, area: Rect) -> Self {
        let margin = 3;
        let w = area.width() as i32 - 2 * margin - 2;
        // Its 1 px shadow ends at y 166, 1 px above the app frame's bottom line (y 168).
        // (A shorter `area`, as above the on-screen keyboard, keeps it above that area's bottom.)
        let bottom = (crate::grid::frame_bottom() - 2).min(area.bottom() - 2);
        let window = Rect::new(area.x() + margin, (bottom - WIN_H).max(0), w.max(40) as u32, WIN_H as u32);
        let text_x = window.x() + 2 + MODE_W + 4;
        let text_w = window.width() as i32 - (text_x - window.x()) - 4;
        // Keep the caret (or the end of the text) inside the window.
        let chars: Vec<char> = view.preedit.chars().collect();
        let focus = view.caret.unwrap_or(chars.len()).min(chars.len());
        let focus_x = fs.measure_text(&chars[..focus].iter().collect::<String>());
        let scroll = (focus_x + 6 - text_w).max(0);
        let cand_w = view
            .cand
            .as_ref()
            .map(|c| c.items.iter().map(|i| fs.measure_text(i)).max().unwrap_or(0) + NUM_W + 14)
            .unwrap_or(0)
            .clamp(NUM_W + 40, area.width() as i32 - 14);
        let cand_x = text_x.min(area.x() + area.width() as i32 - cand_w - 3).max(area.x() + 2);
        Layout { window, text_x, text_w, scroll, cand_x, cand_w }
    }
}

#[allow(dead_code)]
fn _unused(_: Color, _: u32) {
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glue() -> ImeGlue {
        ImeGlue::new(true, parse_toggle(DEFAULT_TOGGLE).unwrap(), false)
    }

    fn key(k: Keycode) -> Input {
        Input::Key { key: k, mods: Mod::NOMOD, repeat: false }
    }
    fn up(k: Keycode) -> Input {
        Input::KeyUp { key: k }
    }
    fn ctrl(k: Keycode) -> Input {
        Input::Key { key: k, mods: Mod::LCTRLMOD, repeat: false }
    }
    /// A letter key as the keyboard layer delivers it: the key press, then the text.
    fn typed(c: char) -> Vec<Input> {
        let k = Keycode::from_i32(c.to_ascii_lowercase() as i32).unwrap();
        vec![key(k), Input::Text(c.to_string()), up(k)]
    }
    fn type_str(g: &mut ImeGlue, s: &str) -> Vec<Input> {
        let mut out = Vec::new();
        for c in s.chars() {
            out.extend(g.process(typed(c), true));
        }
        out
    }
    fn texts(v: &[Input]) -> String {
        v.iter().filter_map(|i| if let Input::Text(t) = i { Some(t.as_str()) } else { None }).collect()
    }

    #[test]
    fn toggle_parsing() {
        assert_eq!(parse_toggle("ctrl+space"), Some(Toggle { key: Keycode::Space, ctrl: true, alt: false }));
        assert_eq!(parse_toggle("Ctrl+J"), Some(Toggle { key: Keycode::J, ctrl: true, alt: false }));
        assert_eq!(parse_toggle("alt+space").map(|t| t.alt), Some(true));
        assert_eq!(parse_toggle("f10").map(|t| t.key), Some(Keycode::F10));
        assert_eq!(parse_toggle("a"), None, "a bare letter would eat typing");
        assert_eq!(parse_toggle("nonsense+key"), None);
        assert!(ImeGlue::from_config(&crate::system::config::Config::load(std::path::PathBuf::from("/nonexistent/x.json"))).enabled());
    }

    #[test]
    fn direct_mode_passes_everything_through() {
        let mut g = glue();
        let input = vec![key(Keycode::A), Input::Text("a".into()), up(Keycode::A), key(Keycode::Tab), up(Keycode::Tab), key(Keycode::Escape), Input::Menu];
        assert_eq!(g.process(input.clone(), true), input);
    }

    #[test]
    fn toggle_then_type_romaji_commits_kana_as_text() {
        let mut g = glue();
        let out = g.process(vec![ctrl(Keycode::Space), Input::Text(" ".into()), up(Keycode::Space)], true);
        assert!(out.is_empty(), "the toggle and its stray space are eaten: {out:?}");
        assert_eq!(g.mode(), Mode::Hiragana);
        let out = type_str(&mut g, "nihon");
        assert!(out.is_empty(), "nothing reaches the app while composing: {out:?}");
        assert_eq!(g.view().unwrap().preedit, "にほn");
        let out = g.process(vec![key(Keycode::Return), up(Keycode::Return)], true);
        assert_eq!(out, vec![Input::Text("にほん".into())], "Enter commits and is not forwarded");
        assert!(!g.is_composing());
    }

    #[test]
    fn tab_and_escape_still_reach_the_launcher_when_not_composing() {
        let mut g = glue();
        g.process(vec![ctrl(Keycode::Space)], true);
        let input = vec![key(Keycode::Tab), up(Keycode::Tab), key(Keycode::Escape), up(Keycode::Escape), ctrl(Keycode::Q), ctrl(Keycode::Tab), Input::Menu, Input::Voice(true), Input::Voice(false), key(Keycode::Return), key(Keycode::Left), key(Keycode::F5)];
        assert_eq!(g.process(input.clone(), true), input);
    }

    #[test]
    fn tab_and_escape_while_composing_belong_to_the_ime() {
        let mut g = glue();
        g.process(vec![ctrl(Keycode::Space)], true);
        type_str(&mut g, "ka");
        // Tab settles the text and is not forwarded (no home switch / Agent double-tap mid-word); same for its release
        let out = g.process(vec![key(Keycode::Tab), up(Keycode::Tab)], true);
        assert_eq!(out, vec![Input::Text("か".into())]);
        type_str(&mut g, "ka");
        let out = g.process(vec![key(Keycode::Escape), up(Keycode::Escape)], true);
        assert!(out.is_empty());
        assert!(!g.is_composing());
        // and with nothing composing Esc is the launcher's again
        let out = g.process(vec![key(Keycode::Escape), up(Keycode::Escape)], true);
        assert_eq!(out, vec![key(Keycode::Escape), up(Keycode::Escape)]);
    }

    #[test]
    fn digits_and_space_when_idle_in_kana_mode() {
        let mut g = glue();
        g.process(vec![ctrl(Keycode::Space)], true);
        let out = g.process(vec![key(Keycode::Space), Input::Text(" ".into()), up(Keycode::Space)], true);
        assert_eq!(texts(&out), " ");
        assert!(out.contains(&key(Keycode::Space)));
        let out = g.process(typed('5'), true);
        assert_eq!(out, vec![Input::Text("5".into())]);
    }

    #[test]
    fn space_converts_and_number_picks() {
        let mut g = glue();
        g.process(vec![ctrl(Keycode::Space)], true);
        type_str(&mut g, "kana");
        let out = g.process(vec![key(Keycode::Space), Input::Text(" ".into()), up(Keycode::Space)], true);
        assert!(out.is_empty(), "no space leaks into the app: {out:?}");
        assert_eq!(g.view().unwrap().cand.unwrap().items[0], "カナ");
        let out = g.process(typed('2'), true);
        assert_eq!(out, vec![Input::Text("かな".into())]);
    }

    #[test]
    fn typing_symbols_with_sym_works() {
        // Sym + key produces a Key (the symbol's keycode) and a Text, like a plain character.
        let mut g = glue();
        g.process(vec![ctrl(Keycode::Space)], true);
        type_str(&mut g, "a");
        let out = g.process(vec![key(Keycode::Comma), Input::Text(",".into()), up(Keycode::Comma)], true);
        assert!(out.is_empty());
        assert_eq!(g.view().unwrap().preedit, "あ、");
    }

    #[test]
    fn terminal_gets_utf8_text_and_plain_control_keys() {
        let mut g = glue();
        g.process(vec![ctrl(Keycode::Space)], true);
        type_str(&mut g, "ls");
        let out = g.process(vec![ctrl(Keycode::C)], true);
        // Ctrl+C while composing settles the (kana) text first, then the key goes on to the terminal
        assert_eq!(out, vec![Input::Text("ls".into()), ctrl(Keycode::C)]);
        // with nothing composing, Ctrl+C is untouched
        assert_eq!(g.process(vec![ctrl(Keycode::C)], true), vec![ctrl(Keycode::C)]);
        // a committed kana string is plain UTF-8 `Text`, which the terminal writes as bytes
        type_str(&mut g, "a");
        let out = g.process(vec![key(Keycode::Return)], true);
        assert_eq!(out, vec![Input::Text("あ".into())]);
        if let Input::Text(t) = &out[0] {
            assert_eq!(t.as_bytes(), "あ".as_bytes());
            assert_eq!(t.len(), 3);
        }
    }

    #[test]
    fn inactive_screens_are_untouched_and_settle_a_composition() {
        let mut g = glue();
        g.process(vec![ctrl(Keycode::Space)], true);
        type_str(&mut g, "ka");
        let input = vec![key(Keycode::A), Input::Text("a".into())];
        let out = g.process(input.clone(), false);
        assert_eq!(out[0], Input::Text("か".into()));
        assert_eq!(&out[1..], &input[..]);
        assert_eq!(g.process(input.clone(), false), input);
    }

    #[test]
    fn disabled_ime_changes_nothing() {
        let mut g = ImeGlue::new(false, parse_toggle(DEFAULT_TOGGLE).unwrap(), false);
        let input = vec![ctrl(Keycode::Space), Input::Text("a".into())];
        assert_eq!(g.process(input.clone(), true), input);
        assert_eq!(g.indicator(true), "");
    }

    #[test]
    fn shift_toggle_cycles_the_kana_mode_and_indicator_follows() {
        let mut g = glue();
        assert_eq!(g.indicator(false), "");
        assert_eq!(g.indicator(true), "A");
        g.process(vec![ctrl(Keycode::Space)], true);
        assert_eq!(g.indicator(false), "あ");
        g.process(vec![Input::Key { key: Keycode::Space, mods: Mod::LCTRLMOD | Mod::LSHIFTMOD, repeat: false }], true);
        assert_eq!(g.indicator(true), "ア");
        let out = type_str(&mut g, "ka");
        assert!(out.is_empty());
        assert_eq!(g.view().unwrap().preedit, "カ");
    }

    #[test]
    fn full_width_mode_commits_directly() {
        let mut g = glue();
        g.process(vec![ctrl(Keycode::Space)], true);
        for _ in 0..3 {
            g.process(vec![Input::Key { key: Keycode::Space, mods: Mod::LCTRLMOD | Mod::LSHIFTMOD, repeat: false }], true);
        }
        assert_eq!(g.mode(), Mode::FullAlnum);
        let out = type_str(&mut g, "a1");
        assert_eq!(texts(&out), "ａ１");
        // a space key press in this mode gives the ideographic space exactly once
        let out = g.process(vec![key(Keycode::Space), Input::Text(" ".into()), up(Keycode::Space)], true);
        assert_eq!(texts(&out), "\u{3000}");
    }

    #[test]
    fn composition_is_settled_before_voice_and_menu() {
        let mut g = glue();
        g.process(vec![ctrl(Keycode::Space)], true);
        type_str(&mut g, "ka");
        let out = g.process(vec![Input::Voice(true)], true);
        assert_eq!(out, vec![Input::Text("か".into()), Input::Voice(true)]);
    }

    #[test]
    fn a_configured_toggle_key_is_used() {
        let mut g = ImeGlue::new(true, parse_toggle("ctrl+j").unwrap(), false);
        // Ctrl+Space is no longer special
        let input = vec![ctrl(Keycode::Space)];
        assert_eq!(g.process(input.clone(), true), input);
        g.process(vec![ctrl(Keycode::J)], true);
        assert_eq!(g.mode(), Mode::Hiragana);
    }
}
