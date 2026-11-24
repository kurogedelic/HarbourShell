//! Yacht: a small piano-roll step sequencer (a port of the Yacht sketch).
//!
//! A keyboard on the left, a 16-step grid on the right, a blinking cursor, a scroll bar and the
//! name of the note under the cursor. Arrows move the cursor (the roll scrolls to keep it in
//! view), Return puts a note there or takes it away, `-` / `=` (or Shift+Left / Right) make the
//! note under the cursor (or the next one placed) shorter / longer, Space plays the 16 steps in a
//! loop with a square wave and stops. The pattern is kept in `~/Yacht/pattern.txt`.
//!
//! Sound: the loop is rendered and played over and over by the shell's own mixer
//! ([`crate::sound`], also in the web build); without it, it goes to a WAV file played with
//! `ffplay -loop 0`. An edit while playing renders it again and restarts the loop.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::assets::AssetStore;
use crate::dither::{self, Pattern as Dith};
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp, Sizing};
use crate::launcher::widgets::{MenuItem, PointerEvent, ScrollBar};
use crate::platform::keys::arrow_alias;
use crate::ui;

/// Steps in the loop.
pub const STEPS: u8 = 16;
/// Lowest and highest note on the roll (MIDI numbers: C1 .. C7).
pub const LOW: u8 = 24;
pub const HIGH: u8 = 96;
/// Rows on the roll.
pub const ROWS: usize = (HIGH - LOW + 1) as usize;
/// Tempo: 120 BPM, a step is a sixteenth note.
pub const STEP_MS: u64 = 125;
const RATE: u32 = 22_050;

const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// `C4`, `F#3`: the note name of a MIDI number (60 = C4).
pub fn note_name(pitch: u8) -> String {
    format!("{}{}", NAMES[pitch as usize % 12], pitch as i32 / 12 - 1)
}

pub fn is_black(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

pub fn freq(pitch: u8) -> f32 {
    440.0 * 2f32.powf((pitch as f32 - 69.0) / 12.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Note {
    pub step: u8,
    pub pitch: u8,
    pub len: u8,
}

impl Note {
    fn covers(&self, step: u8) -> bool {
        step >= self.step && step < self.step + self.len
    }
}

/// The notes of one 16-step loop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pattern {
    pub notes: Vec<Note>,
}

impl Pattern {
    /// The short phrase the original sketch started with.
    pub fn demo() -> Self {
        let n = |step, pitch, len| Note { step, pitch, len };
        Pattern { notes: vec![n(0, 60, 1), n(1, 62, 1), n(3, 60, 1), n(4, 67, 1), n(11, 65, 4)] }
    }

    /// Index of the note sounding at (step, pitch).
    pub fn at(&self, step: u8, pitch: u8) -> Option<usize> {
        self.notes.iter().position(|n| n.pitch == pitch && n.covers(step))
    }

    /// Return on a cell: take away the note there, or put one of `len` steps (cut at the end of
    /// the loop and before the next note on that row). True when a note was added.
    pub fn toggle(&mut self, step: u8, pitch: u8, len: u8) -> bool {
        if let Some(i) = self.at(step, pitch) {
            self.notes.remove(i);
            return false;
        }
        let room = self.room(step, pitch, None);
        self.notes.push(Note { step, pitch, len: len.clamp(1, room) });
        self.notes.sort_by_key(|n| (n.step, n.pitch));
        true
    }

    /// Steps free from `step` on this row (up to the end of the loop), ignoring note `skip`.
    fn room(&self, step: u8, pitch: u8, skip: Option<usize>) -> u8 {
        let next = self
            .notes
            .iter()
            .enumerate()
            .filter(|(i, n)| Some(*i) != skip && n.pitch == pitch && n.step > step)
            .map(|(_, n)| n.step)
            .min()
            .unwrap_or(STEPS);
        next.min(STEPS) - step
    }

    /// Change the length of note `i` by `d` steps (at least 1, never over the next note or the
    /// end of the loop). Returns the new length.
    pub fn resize(&mut self, i: usize, d: i32) -> u8 {
        let n = self.notes[i];
        let room = self.room(n.step, n.pitch, Some(i));
        let len = (n.len as i32 + d).clamp(1, room as i32) as u8;
        self.notes[i].len = len;
        len
    }

    /// One note per line: `step pitch length`.
    pub fn to_text(&self) -> String {
        let mut s = String::from("# Yacht pattern: step pitch length\n");
        for n in &self.notes {
            s.push_str(&format!("{} {} {}\n", n.step, n.pitch, n.len));
        }
        s
    }

    /// Reads [`to_text`](Self::to_text); lines that do not make sense are skipped.
    pub fn from_text(text: &str) -> Self {
        let mut p = Pattern::default();
        for line in text.lines() {
            let v: Vec<u8> = line.split_whitespace().filter_map(|w| w.parse().ok()).collect();
            if let [step, pitch, len] = v[..] {
                if step < STEPS && (LOW..=HIGH).contains(&pitch) && len >= 1 && p.at(step, pitch).is_none() {
                    let room = p.room(step, pitch, None);
                    p.notes.push(Note { step, pitch, len: len.min(room) });
                }
            }
        }
        p.notes.sort_by_key(|n| (n.step, n.pitch));
        p
    }

    /// The loop as mono 16-bit samples: a square wave per note, mixed, with a short fade at each
    /// end of a note so it does not click.
    pub fn render(&self, rate: u32) -> Vec<i16> {
        let step_len = (rate as u64 * STEP_MS / 1000) as usize;
        let total = step_len * STEPS as usize;
        let mut mix = vec![0f32; total];
        let fade = (rate as usize / 500).max(1);
        for n in &self.notes {
            let start = n.step as usize * step_len;
            let len = (n.len as usize * step_len).min(total - start);
            let period = rate as f32 / freq(n.pitch);
            for k in 0..len {
                let s = if (k as f32 % period) < period / 2.0 { 1.0 } else { -1.0 };
                let env = (k.min(len - 1 - k) as f32 / fade as f32).min(1.0);
                mix[start + k] += s * env * 0.18;
            }
        }
        mix.iter().map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16).collect()
    }

    pub fn wav(&self, rate: u32) -> Vec<u8> {
        let pcm = self.render(rate);
        let mut out = crate::launcher::apps::recorder::wav_header(rate, 1, (pcm.len() * 2) as u32).to_vec();
        for s in pcm {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }
}

/// Where the pattern is kept.
pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Yacht").join("pattern.txt")
}

/// Plays the rendered loop.
struct Player {
    child: Option<Child>,
    /// The loop in the shell's mixer.
    voice: Option<u64>,
    since: Instant,
}

impl Player {
    /// Through the shell's mixer, when there is one.
    fn mixer(pattern: &Pattern) -> Option<Player> {
        if cfg!(test) || !crate::sound::available() {
            return None;
        }
        let pcm = pattern.render(crate::sound::rate());
        let voice = crate::sound::play_loop(pcm.iter().map(|&v| v as f32 / 32768.0).collect())?;
        Some(Player { child: None, voice: Some(voice), since: Instant::now() })
    }

    fn start(wav: &Path) -> Result<Player, String> {
        let child = if cfg!(test) {
            None
        } else {
            Some(
                Command::new("ffplay")
                    .args(["-nodisp", "-loop", "0", "-loglevel", "quiet"])
                    .arg(wav)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| format!("ffplay: {e}"))?,
            )
        };
        Ok(Player { child, voice: None, since: Instant::now() })
    }

    fn step(&self, now: Instant) -> u8 {
        ((now.saturating_duration_since(self.since).as_millis() as u64 / STEP_MS) % STEPS as u64) as u8
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if let Some(v) = self.voice.take() {
            crate::sound::stop(v);
        }
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Where everything goes for a surface of `w` x `h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub keys: Rect,
    pub grid: Rect,
    pub cell_w: i32,
    pub row_h: i32,
    pub rows: usize,
    pub bar: Rect,
    pub head_y: i32,
}

const KEYS_MIN_W: i32 = 36;
const BAR_W: i32 = 8;

impl Layout {
    pub fn for_size(w: i32, h: i32) -> Layout {
        let row_h = 12;
        let head_y = grid::BAR_H + 2;
        let top = head_y + 14;
        let rows = ((h - 3 - top) / row_h).max(1) as usize;
        let avail = w - 2 - KEYS_MIN_W - 2 - 2 - BAR_W - 2;
        let cell_w = (avail / STEPS as i32).max(8);
        let keys_w = KEYS_MIN_W + (avail - cell_w * STEPS as i32).clamp(0, 24);
        let gx = 2 + keys_w + 1;
        let gh = rows as i32 * row_h;
        let gw = cell_w * STEPS as i32;
        Layout {
            keys: Rect::new(2, top, keys_w as u32, gh as u32),
            grid: Rect::new(gx, top, gw as u32, gh as u32),
            cell_w,
            row_h,
            rows,
            bar: Rect::new(gx + gw + 3, top, BAR_W as u32, gh as u32),
            head_y,
        }
    }

    fn now() -> Layout {
        Layout::for_size(grid::sw(), grid::sh())
    }

    /// The (step, row) cell under (x, y).
    fn cell_at(&self, x: i32, y: i32) -> Option<(u8, usize)> {
        let g = self.grid;
        if x < g.x() || y < g.y() || x >= g.right() || y >= g.bottom() {
            return None;
        }
        Some((((x - g.x()) / self.cell_w) as u8, ((y - g.y()) / self.row_h) as usize))
    }
}

pub struct Yacht {
    pub pattern: Pattern,
    /// Cursor: step and pitch.
    pub step: u8,
    pub pitch: u8,
    /// Row index (0 = HIGH) shown at the top.
    pub top: usize,
    /// Length of the next note placed.
    pub len: u8,
    path: PathBuf,
    player: Option<Player>,
    started: Instant,
    blink_on: bool,
    shown_step: Option<u8>,
    dragging_bar: bool,
}

pub fn make() -> Box<dyn LauncherApp> {
    Box::new(Yacht::load(default_path()))
}

fn row_of(pitch: u8) -> usize {
    (HIGH - pitch) as usize
}

fn pitch_of(row: usize) -> u8 {
    HIGH - row.min(ROWS - 1) as u8
}

impl Yacht {
    pub fn load(path: PathBuf) -> Yacht {
        let pattern = match std::fs::read_to_string(&path) {
            Ok(t) => Pattern::from_text(&t),
            Err(_) => Pattern::demo(),
        };
        let mut y = Yacht {
            pattern,
            step: 0,
            pitch: 60,
            top: 0,
            len: 1,
            path,
            player: None,
            started: Instant::now(),
            blink_on: true,
            shown_step: None,
            dragging_bar: false,
        };
        y.top = row_of(67);
        y.follow(Layout::now().rows);
        y
    }

    pub fn playing(&self) -> bool {
        self.player.is_some()
    }

    /// The step the loop is on (None when stopped).
    pub fn play_step(&self) -> Option<u8> {
        self.player.as_ref().map(|p| p.step(Instant::now()))
    }

    /// Scroll so the cursor row is visible, keeping a row of context when it can.
    fn follow(&mut self, rows: usize) {
        let r = row_of(self.pitch);
        let max_top = ROWS.saturating_sub(rows);
        if r < self.top + 1 {
            self.top = r.saturating_sub(1);
        } else if r + 2 > self.top + rows {
            self.top = (r + 2).saturating_sub(rows);
        }
        self.top = self.top.min(max_top);
    }

    fn save(&self) -> Result<(), String> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(&self.path, self.pattern.to_text()).map_err(|e| e.to_string())
    }

    fn wav_path(&self) -> PathBuf {
        std::env::temp_dir().join(format!("harbour-yacht-{}.wav", std::process::id()))
    }

    fn play(&mut self) -> AppAction {
        self.player = None;
        if let Some(p) = Player::mixer(&self.pattern) {
            self.player = Some(p);
            return AppAction::None;
        }
        let wav = self.wav_path();
        if let Err(e) = std::fs::write(&wav, self.pattern.wav(RATE)) {
            return AppAction::Toast(format!("Cannot play: {e}"));
        }
        self.player = None;
        match Player::start(&wav) {
            Ok(p) => {
                self.player = Some(p);
                AppAction::None
            }
            Err(e) => AppAction::Toast(format!("Cannot play: {e}")),
        }
    }

    fn stop(&mut self) {
        self.player = None;
        let _ = std::fs::remove_file(self.wav_path());
    }

    /// After an edit: keep the file, and play the new loop if it was playing.
    fn changed(&mut self) -> AppAction {
        let saved = self.save();
        if self.playing() {
            let a = self.play();
            if a != AppAction::None {
                return a;
            }
        }
        match saved {
            Ok(()) => AppAction::None,
            Err(e) => AppAction::Toast(format!("Cannot save: {e}")),
        }
    }

    fn move_cursor(&mut self, dx: i32, dy: i32) {
        self.step = (self.step as i32 + dx).rem_euclid(STEPS as i32) as u8;
        self.pitch = (self.pitch as i32 - dy).clamp(LOW as i32, HIGH as i32) as u8;
        self.follow(Layout::now().rows);
        self.started = Instant::now();
        self.blink_on = true;
    }

    fn resize(&mut self, d: i32) -> AppAction {
        match self.pattern.at(self.step, self.pitch) {
            Some(i) => {
                self.len = self.pattern.resize(i, d);
                self.changed()
            }
            None => {
                self.len = (self.len as i32 + d).clamp(1, STEPS as i32) as u8;
                AppAction::None
            }
        }
    }

    fn toggle(&mut self) -> AppAction {
        self.pattern.toggle(self.step, self.pitch, self.len);
        self.changed()
    }

    fn clear(&mut self) -> AppAction {
        self.pattern.notes.clear();
        self.changed()
    }
}

impl Drop for Yacht {
    fn drop(&mut self) {
        self.stop();
    }
}

impl LauncherApp for Yacht {
    fn title(&self) -> String {
        "Yacht".to_string()
    }

    fn sizing(&self) -> Sizing {
        Sizing::Flexible
    }

    fn help(&self) -> Vec<(String, String)> {
        let k = |a: &str, b: &str| (a.to_string(), b.to_string());
        vec![
            k("Arrows", "Move the cursor"),
            k("Return", "Put or take away a note"),
            k("- / =", "Shorter / longer note"),
            k("Shift+Left / Right", "Shorter / longer note"),
            k("Space", "Play / stop the loop"),
            k("Backspace", "Take away the note"),
        ]
    }

    fn describe(&self) -> String {
        format!(
            "Yacht step sequencer: cursor at step {} on {}, {} notes, {}",
            self.step + 1,
            note_name(self.pitch),
            self.pattern.notes.len(),
            if self.playing() { "playing" } else { "stopped" }
        )
    }

    fn menu(&self) -> Vec<MenuItem> {
        vec![
            MenuItem::new("play", if self.playing() { "Stop" } else { "Play" }),
            MenuItem::new("clear", "Clear pattern"),
            MenuItem::new("demo", "Demo pattern"),
        ]
    }

    fn menu_action(&mut self, id: &str, _ctx: &Ctx) -> AppAction {
        match id {
            "play" if self.playing() => {
                self.stop();
                AppAction::None
            }
            "play" => self.play(),
            "clear" => self.clear(),
            "demo" => {
                self.pattern = Pattern::demo();
                self.changed()
            }
            _ => AppAction::None,
        }
    }

    fn request_close(&mut self) -> bool {
        self.stop();
        true
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, _ctx: &Ctx) -> AppAction {
        let shift = mods.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
        match arrow_alias(key) {
            Keycode::Escape => {
                if self.playing() {
                    self.stop();
                    return AppAction::None;
                }
                return AppAction::Quit;
            }
            Keycode::Left if shift => return self.resize(-1),
            Keycode::Right if shift => return self.resize(1),
            Keycode::Left => self.move_cursor(-1, 0),
            Keycode::Right => self.move_cursor(1, 0),
            Keycode::Up => self.move_cursor(0, -1),
            Keycode::Down => self.move_cursor(0, 1),
            Keycode::Minus | Keycode::KpMinus | Keycode::LeftBracket => return self.resize(-1),
            Keycode::Equals | Keycode::KpPlus | Keycode::RightBracket => return self.resize(1),
            Keycode::Return | Keycode::KpEnter => return self.toggle(),
            Keycode::Backspace | Keycode::Delete => {
                if let Some(i) = self.pattern.at(self.step, self.pitch) {
                    self.pattern.notes.remove(i);
                    return self.changed();
                }
            }
            Keycode::Space => {
                if self.playing() {
                    self.stop();
                } else {
                    return self.play();
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, _ctx: &Ctx) -> AppAction {
        let l = Layout::now();
        let bar = ScrollBar::new(l.bar, ROWS, l.rows, self.top);
        match ev {
            PointerEvent::Wheel { dy, .. } => {
                let max_top = ROWS.saturating_sub(l.rows) as i32;
                self.top = (self.top as i32 - dy.signum() * 3).clamp(0, max_top) as usize;
            }
            PointerEvent::Down { x, y, button: 0, .. } => {
                if let Some((off, drag)) = bar.press(x, y) {
                    self.top = off;
                    self.dragging_bar = drag;
                } else if let Some((s, r)) = l.cell_at(x, y) {
                    let same = s == self.step && pitch_of(self.top + r) == self.pitch;
                    self.step = s;
                    self.pitch = pitch_of(self.top + r);
                    if same {
                        return self.toggle();
                    }
                }
            }
            PointerEvent::Drag { y, .. } if self.dragging_bar => self.top = bar.offset_for_y(y),
            PointerEvent::Up { .. } => self.dragging_bar = false,
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, _ctx: &Ctx) -> bool {
        let on = (self.started.elapsed().as_millis() / 200) % 2 == 0;
        let step = self.play_step();
        let dirty = on != self.blink_on || step != self.shown_step;
        self.blink_on = on;
        self.shown_step = step;
        dirty
    }

    fn draw(&mut self, c: &mut Canvas<Window>, _assets: &mut AssetStore, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        let ink = tm.get_color("foreground");
        let paper = tm.get_color("widget_face_active");
        let faint = crate::launcher::widgets::secondary_ink(tm);
        let l = Layout::now();
        self.follow(l.rows);
        ui::fill_rect(c, Rect::new(0, grid::BAR_H, grid::sw() as u32, (grid::sh() - grid::BAR_H) as u32), tm.get_color("home_bg"));

        // Header: cursor note, step, length, transport.
        let x0 = l.keys.x() + 2;
        fs.draw_text(c, x0, l.head_y, &note_name(self.pitch), ink);
        let info = format!("Step {:>2}/{}   Len {}", self.step + 1, STEPS, self.len);
        fs.draw_text(c, l.grid.x() + 4, l.head_y, &info, faint);
        let tr = if self.playing() { "Play" } else { "Stop" };
        let tx = l.grid.right() - fs.measure_text(tr);
        fs.draw_text(c, tx, l.head_y, tr, ink);
        // A play triangle or a stop square before the word (the font has neither).
        let (ix, iy) = (tx - 11, l.head_y + 3);
        if self.playing() {
            for k in 0..4 {
                ui::line(c, ix + k, iy + k, ix + k, iy + 7 - k, ink);
            }
        } else {
            ui::fill_rect(c, Rect::new(ix, iy, 7, 7), ink);
        }

        let outer = Rect::new(l.keys.x() - 1, l.keys.y() - 1, (l.grid.right() - l.keys.x() + 2) as u32, l.keys.height() + 2);
        ui::fill_rect(c, outer, paper);
        ui::draw_rect(c, outer, ink);

        // Keyboard and rows.
        let cur_row = row_of(self.pitch);
        for i in 0..l.rows {
            let row = self.top + i;
            if row >= ROWS {
                break;
            }
            let p = pitch_of(row);
            let y = l.grid.y() + i as i32 * l.row_h;
            let key = Rect::new(l.keys.x(), y, l.keys.width(), l.row_h as u32);
            if is_black(p) {
                ui::fill_rect(c, Rect::new(key.x(), y, (key.width() * 2 / 3) as u32, l.row_h as u32), ink);
            }
            // A rule under each row on the keys: the full width under B and E (no black key between).
            if matches!(p % 12, 0 | 5) {
                ui::line(c, key.x(), y + l.row_h - 1, key.right() - 1, y + l.row_h - 1, ink);
            }
            let label = if row == cur_row { Some(note_name(p)) } else if p % 12 == 0 { Some(note_name(p)) } else { None };
            if let Some(t) = label {
                let col = if row == cur_row { ink } else { faint };
                fs.draw_text(c, key.right() - 2 - fs.measure_text(&t), y - 1, &t, col);
            }
        }
        ui::line(c, l.grid.x() - 1, l.grid.y(), l.grid.x() - 1, l.grid.bottom() - 1, ink);

        // Beat lines: dotted, solid every 4 steps.
        for s in 1..STEPS as i32 {
            let x = l.grid.x() + s * l.cell_w;
            for y in (l.grid.y()..l.grid.bottom()).step_by(if s % 4 == 0 { 1 } else { 2 }) {
                if s % 4 == 0 || (y - l.grid.y()) % 2 == 0 {
                    ui::fill_rect(c, Rect::new(x, y, 1, 1), faint);
                }
            }
        }

        // Cursor column and row (dithered bands, like the original).
        let crow = cur_row as i32 - self.top as i32;
        let cx = l.grid.x() + self.step as i32 * l.cell_w;
        dither::fill(c, Rect::new(cx, l.grid.y(), l.cell_w as u32, l.grid.height()), faint, Dith::Quarter);
        if (0..l.rows as i32).contains(&crow) {
            dither::fill(c, Rect::new(l.grid.x(), l.grid.y() + crow * l.row_h, l.grid.width(), l.row_h as u32), faint, Dith::Quarter);
        }

        // Play head.
        if let Some(s) = self.play_step() {
            let px = l.grid.x() + s as i32 * l.cell_w;
            ui::fill_rect(c, Rect::new(px, l.grid.y(), 2, l.grid.height()), ink);
        }

        // Notes.
        for n in &self.pattern.notes {
            let r = row_of(n.pitch) as i32 - self.top as i32;
            if !(0..l.rows as i32).contains(&r) {
                continue;
            }
            let x = l.grid.x() + n.step as i32 * l.cell_w;
            let y = l.grid.y() + r * l.row_h;
            let w = n.len as i32 * l.cell_w;
            let body = Rect::new(x + 1, y + 1, (w - 2).max(2) as u32, (l.row_h - 2) as u32);
            ui::fill_rect(c, body, ink);
            // The two handles of the original's note pills.
            if w >= 10 {
                ui::line(c, x + 3, y + 3, x + 3, y + l.row_h - 4, paper);
                ui::line(c, x + w - 4, y + 3, x + w - 4, y + l.row_h - 4, paper);
            }
        }

        // Blinking cursor box.
        if (0..l.rows as i32).contains(&crow) {
            let r = Rect::new(cx, l.grid.y() + crow * l.row_h, l.cell_w as u32, l.row_h as u32);
            let under = self.pattern.at(self.step, self.pitch).is_some();
            let col = if self.blink_on != under { ink } else { paper };
            ui::draw_rect(c, r, col);
            ui::draw_rect(c, Rect::new(r.x() + 1, r.y() + 1, r.width() - 2, r.height() - 2), if self.blink_on { paper } else { ink });
        }

        ScrollBar::new(l.bar, ROWS, l.rows, self.top).draw(c, tm, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("harbour-yacht-{tag}-{}", std::process::id())).join("pattern.txt")
    }

    fn ctx_parts() -> (crate::font::FontSystem, crate::theme::ThemeManager, crate::system::mock::MockBackend) {
        use crate::theme::{Theme, ThemeManager};
        (
            crate::font::FontSystem::new(),
            ThemeManager::new(Theme::load_from_yaml("assets/themes/theme-paper.yaml").unwrap()),
            crate::system::mock::MockBackend::new(),
        )
    }

    #[test]
    fn note_names_and_keys() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(61), "C#4");
        assert_eq!(note_name(LOW), "C1");
        assert_eq!(note_name(HIGH), "C7");
        assert!(is_black(61) && !is_black(64) && !is_black(65));
        assert!((freq(69) - 440.0).abs() < 0.01);
        assert!((freq(81) - 880.0).abs() < 0.1);
    }

    #[test]
    fn toggling_puts_and_takes_away_notes() {
        let mut p = Pattern::default();
        assert!(p.toggle(2, 60, 3));
        assert_eq!(p.notes, [Note { step: 2, pitch: 60, len: 3 }]);
        assert_eq!(p.at(4, 60), Some(0), "a long note covers its steps");
        assert_eq!(p.at(5, 60), None);
        assert!(!p.toggle(3, 60, 1), "Return on any part of a note takes it away");
        assert!(p.notes.is_empty());
    }

    #[test]
    fn lengths_stop_at_the_next_note_and_the_end_of_the_loop() {
        let mut p = Pattern::default();
        p.toggle(6, 60, 1);
        p.toggle(2, 60, 8);
        let i = p.at(2, 60).unwrap();
        assert_eq!(p.notes[i].len, 4, "cut before the note at step 6");
        p.toggle(14, 62, 8);
        assert_eq!(p.notes[p.at(14, 62).unwrap()].len, 2, "cut at the end of the loop");
        let i = p.at(2, 60).unwrap();
        assert_eq!(p.resize(i, -10), 1);
        assert_eq!(p.resize(i, 10), 4);
    }

    #[test]
    fn the_pattern_survives_a_round_trip_and_bad_lines_are_skipped() {
        let p = Pattern::demo();
        assert_eq!(Pattern::from_text(&p.to_text()), p);
        let q = Pattern::from_text("junk\n3 60 2\n99 60 1\n3 200 1\n4 60 1\n");
        assert_eq!(q.notes, [Note { step: 3, pitch: 60, len: 2 }], "overlap, step and pitch out of range dropped");
    }

    #[test]
    fn the_rendered_loop_is_sixteen_steps_of_square_wave() {
        let mut p = Pattern::default();
        let silent = p.render(RATE);
        assert_eq!(silent.len(), (RATE as u64 * STEP_MS / 1000) as usize * STEPS as usize);
        assert!(silent.iter().all(|s| *s == 0));
        p.toggle(0, 69, 1);
        let a = p.render(RATE);
        let step = (RATE as u64 * STEP_MS / 1000) as usize;
        assert!(a[..step].iter().any(|s| *s > 1000) && a[..step].iter().any(|s| *s < -1000));
        assert!(a[step..].iter().all(|s| *s == 0), "only the first step sounds");
        // Sign changes per second ~ 2 x 440.
        let flips = a[100..step - 100].windows(2).filter(|w| (w[0] > 0) != (w[1] > 0)).count();
        let expect = 2.0 * 440.0 * (step - 200) as f32 / RATE as f32;
        assert!((flips as f32 - expect).abs() < 4.0, "{flips} vs {expect}");
        let wav = p.wav(RATE);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(wav.len(), 44 + a.len() * 2);
    }

    #[test]
    fn keys_edit_save_and_play() {
        let (fs, tm, sys) = ctx_parts();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let path = tmp("keys");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let mut y = Yacht::load(path.clone());
        assert_eq!(y.pattern, Pattern::demo(), "no file yet: the demo phrase");
        y.pattern.notes.clear();
        let none = Mod::empty();
        y.key(Keycode::Right, none, false, &ctx);
        y.key(Keycode::Up, none, false, &ctx);
        assert_eq!((y.step, y.pitch), (1, 61));
        y.key(Keycode::Equals, none, false, &ctx);
        assert_eq!(y.len, 2, "no note under the cursor: the next note gets longer");
        y.key(Keycode::Return, none, false, &ctx);
        assert_eq!(y.pattern.notes, [Note { step: 1, pitch: 61, len: 2 }]);
        y.key(Keycode::Right, Mod::LSHIFTMOD, false, &ctx);
        assert_eq!(y.pattern.notes[0].len, 3);
        y.key(Keycode::Minus, none, false, &ctx);
        assert_eq!(y.pattern.notes[0].len, 2);
        assert_eq!(Pattern::from_text(&std::fs::read_to_string(&path).unwrap()), y.pattern, "saved on each edit");
        // Left wraps round the loop.
        y.key(Keycode::Left, none, false, &ctx);
        y.key(Keycode::Left, none, false, &ctx);
        assert_eq!(y.step, 15);
        y.key(Keycode::Space, none, false, &ctx);
        assert!(y.playing() && y.play_step().is_some());
        assert_eq!(y.key(Keycode::Escape, none, false, &ctx), AppAction::None, "Esc stops first");
        assert!(!y.playing());
        assert_eq!(y.key(Keycode::Escape, none, false, &ctx), AppAction::Quit);
        // Reopened: the saved pattern comes back.
        let z = Yacht::load(path.clone());
        assert_eq!(z.pattern.notes, [Note { step: 1, pitch: 61, len: 2 }]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_roll_follows_the_cursor_and_grows_with_the_window() {
        let small = Layout::for_size(320, 170);
        assert_eq!(small.cell_w, 16);
        assert!(small.bar.right() <= 320 - 2, "{:?}", small.bar);
        assert!(small.grid.bottom() <= 170 - 2);
        assert!(small.rows >= 10);
        let big = Layout::for_size(800, 480);
        assert!(big.cell_w > small.cell_w && big.rows > small.rows);
        assert!(big.bar.right() <= 800);

        let mut y = Yacht::load(tmp("follow"));
        for _ in 0..80 {
            y.move_cursor(0, 1);
        }
        assert_eq!(y.pitch, LOW);
        let r = row_of(y.pitch);
        assert!(r >= y.top && r < y.top + small.rows);
        for _ in 0..80 {
            y.move_cursor(0, -1);
        }
        assert_eq!((y.pitch, y.top), (HIGH, 0));
    }

    #[test]
    fn clicking_a_cell_moves_there_and_a_second_click_toggles() {
        let (fs, tm, sys) = ctx_parts();
        let ctx = Ctx { fs: &fs, tm: &tm, sys: &sys, now: chrono::Local::now() };
        let path = tmp("click");
        let mut y = Yacht::load(path.clone());
        y.pattern.notes.clear();
        let l = Layout::now();
        let (x, py) = (l.grid.x() + 3 * l.cell_w + 2, l.grid.y() + 2 * l.row_h + 2);
        let down = PointerEvent::Down { x, y: py, button: 0, clicks: 1 };
        y.pointer(down.clone(), &ctx);
        assert_eq!((y.step, y.pitch), (3, pitch_of(y.top + 2)));
        assert!(y.pattern.notes.is_empty());
        y.pointer(down, &ctx);
        assert_eq!(y.pattern.notes.len(), 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
