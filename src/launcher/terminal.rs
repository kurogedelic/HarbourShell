//! Built-in terminal: a PTY plus a small VT100 emulator drawn with Harbour's
//! bitmap font in a grid of 6 x 11 px cells inside the app frame (52 x 13 cells on the 320 x 170
//! screen; in a bigger window the grid grows to fill it and the program is told the new size).
//!
//! Supported: printable UTF-8 (CJK as double-width), CR/LF/BS/TAB/BEL, cursor
//! movement (CUU/CUD/CUF/CUB/CUP/CHA/VPA), erase (ED/EL/ECH), insert/delete
//! line and character, scroll region, save/restore cursor, SGR reverse video.
//! Everything else is parsed and ignored. Used for `Terminal=true` apps and the
//! home screen's Terminal item.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::chrome::BAR_H;
use crate::font::FontSystem;
use crate::platform::Shared;
use crate::theme::ThemeManager;
use crate::ui;

pub const CELL_W: i32 = 6;
pub const CELL_H: i32 = 11;
/// The text grid fills the app frame's content rectangle (`grid::app_content`: x 3..316, y 19..161
/// on the 320 x 170 screen, 1 px clear of the line): 52 x 13 cells (312 x 143) there.
pub const COLS: usize = 52;
pub const ROWS: usize = 13;
/// Top-left of the first cell on the 320 x 170 screen (x 4..315, y 19..161).
pub const ORIGIN: (i32, i32) = (4, 19);

/// The text grid for the surface being drawn now: the top-left of the first cell and how many
/// columns and rows fit in the content rectangle. The first column starts 1 px in from the content's
/// left edge (a pixel more clear of the frame's line), as it always has.
pub fn layout() -> ((i32, i32), usize, usize) {
    let ct = crate::grid::app_content();
    let cols = ((ct.width() as i32 - 1) / CELL_W).max(1) as usize;
    let rows = (ct.height() as i32 / CELL_H).max(1) as usize;
    ((ct.x() + 1, ct.y()), cols, rows)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub reverse: bool,
}

const BLANK: Cell = Cell { ch: ' ', reverse: false };
/// Right half of a double-width character.
const CONT: Cell = Cell { ch: '\0', reverse: false };

fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    Csi,
    Osc,
    /// ESC ( / ESC ) etc.: swallow one more byte.
    Skip,
}

/// Terminal screen model; no I/O, fully testable.
pub struct Vt {
    pub cols: usize,
    pub rows: usize,
    pub grid: Vec<Vec<Cell>>,
    pub cx: usize,
    pub cy: usize,
    saved: (usize, usize),
    top: usize,
    bottom: usize,
    reverse: bool,
    wrap_pending: bool,
    state: State,
    params: Vec<u16>,
    private: bool,
    cur: Option<u16>,
    utf8: Vec<u8>,
    pub cursor_visible: bool,
}

impl Vt {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            grid: vec![vec![BLANK; cols]; rows],
            cx: 0,
            cy: 0,
            saved: (0, 0),
            top: 0,
            bottom: rows - 1,
            reverse: false,
            wrap_pending: false,
            state: State::Ground,
            params: Vec::new(),
            private: false,
            cur: None,
            utf8: Vec::new(),
            cursor_visible: true,
        }
    }

    /// Change the grid to `cols` x `rows` (the window was resized). The text keeps its place at the
    /// top-left; when rows go away the top lines scroll off so the cursor's line stays on screen.
    /// The scroll region becomes the whole screen again (the program redraws on SIGWINCH anyway).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        if self.cy >= rows {
            let n = self.cy + 1 - rows;
            self.grid.drain(..n);
            self.cy -= n;
            self.saved.1 = self.saved.1.saturating_sub(n);
        }
        self.grid.resize(rows, vec![BLANK; cols]);
        for row in &mut self.grid {
            row.resize(cols, BLANK);
            // A double-width character cut in half at the new right edge goes.
            if row.last().is_some_and(|c| c.ch != '\0' && is_wide(c.ch)) {
                row[cols - 1] = BLANK;
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.cx = self.cx.min(cols - 1);
        self.cy = self.cy.min(rows - 1);
        self.saved = (self.saved.0.min(cols - 1), self.saved.1.min(rows - 1));
        self.wrap_pending = false;
    }

    pub fn row_text(&self, y: usize) -> String {
        self.grid[y].iter().filter(|c| c.ch != '\0').map(|c| c.ch).collect::<String>().trim_end().to_string()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }

    fn byte(&mut self, b: u8) {
        match self.state {
            State::Ground => self.ground(b),
            State::Esc => self.esc(b),
            State::Csi => self.csi(b),
            State::Osc => {
                // Ends with BEL or ESC \ (the ESC restarts parsing; the '\' is swallowed by Esc).
                if b == 0x07 {
                    self.state = State::Ground;
                } else if b == 0x1b {
                    self.state = State::Esc;
                }
            }
            State::Skip => self.state = State::Ground,
        }
    }

    fn ground(&mut self, b: u8) {
        match b {
            0x1b => {
                self.utf8.clear();
                self.state = State::Esc;
            }
            b'\n' | 0x0b | 0x0c => self.linefeed(),
            b'\r' => {
                self.cx = 0;
                self.wrap_pending = false;
            }
            0x08 => {
                self.cx = self.cx.saturating_sub(1);
                self.wrap_pending = false;
            }
            b'\t' => {
                self.cx = ((self.cx / 8 + 1) * 8).min(self.cols - 1);
                self.wrap_pending = false;
            }
            0x07 | 0x00 => {}
            0x20..=0x7e => self.put(b as char),
            0x80..=0xff => {
                self.utf8.push(b);
                let need = match self.utf8[0] {
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf7 => 4,
                    _ => 1,
                };
                if self.utf8.len() >= need {
                    let s = String::from_utf8_lossy(&self.utf8).to_string();
                    self.utf8.clear();
                    for ch in s.chars() {
                        self.put(ch);
                    }
                }
            }
            _ => {}
        }
    }

    fn esc(&mut self, b: u8) {
        self.state = State::Ground;
        match b {
            b'[' => {
                self.state = State::Csi;
                self.params.clear();
                self.private = false;
                self.cur = None;
            }
            b']' => self.state = State::Osc,
            b'(' | b')' | b'*' | b'+' | b'#' => self.state = State::Skip,
            b'7' => self.saved = (self.cx, self.cy),
            b'8' => {
                self.cx = self.saved.0.min(self.cols - 1);
                self.cy = self.saved.1.min(self.rows - 1);
            }
            b'D' => self.linefeed(),
            b'E' => {
                self.cx = 0;
                self.linefeed();
            }
            b'M' => self.reverse_index(),
            b'c' => {
                *self = Vt::new(self.cols, self.rows);
            }
            _ => {}
        }
    }

    fn csi(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                self.cur = Some(self.cur.unwrap_or(0).saturating_mul(10).saturating_add((b - b'0') as u16));
            }
            b';' => {
                self.params.push(self.cur.take().unwrap_or(0));
            }
            b'?' | b'>' | b'=' | b'!' => self.private = true,
            0x20..=0x2f => {}
            0x40..=0x7e => {
                if let Some(c) = self.cur.take() {
                    self.params.push(c);
                } else if !self.params.is_empty() {
                    self.params.push(0);
                }
                self.state = State::Ground;
                self.dispatch(b);
            }
            _ => self.state = State::Ground,
        }
    }

    fn p(&self, i: usize, default: usize) -> usize {
        match self.params.get(i) {
            Some(&v) if v > 0 => v as usize,
            _ => default,
        }
    }

    fn dispatch(&mut self, fin: u8) {
        self.wrap_pending = false;
        match fin {
            b'A' => self.cy = self.cy.saturating_sub(self.p(0, 1)).max(if self.cy >= self.top { self.top } else { 0 }),
            b'B' => self.cy = (self.cy + self.p(0, 1)).min(if self.cy <= self.bottom { self.bottom } else { self.rows - 1 }),
            b'C' => self.cx = (self.cx + self.p(0, 1)).min(self.cols - 1),
            b'D' => self.cx = self.cx.saturating_sub(self.p(0, 1)),
            b'E' => {
                self.cy = (self.cy + self.p(0, 1)).min(self.rows - 1);
                self.cx = 0;
            }
            b'F' => {
                self.cy = self.cy.saturating_sub(self.p(0, 1));
                self.cx = 0;
            }
            b'G' | b'`' => self.cx = (self.p(0, 1) - 1).min(self.cols - 1),
            b'd' => self.cy = (self.p(0, 1) - 1).min(self.rows - 1),
            b'H' | b'f' => {
                self.cy = (self.p(0, 1) - 1).min(self.rows - 1);
                self.cx = (self.p(1, 1) - 1).min(self.cols - 1);
            }
            b'J' => match self.params.first().copied().unwrap_or(0) {
                0 => {
                    self.erase_line_from(self.cy, self.cx);
                    for y in self.cy + 1..self.rows {
                        self.grid[y] = vec![BLANK; self.cols];
                    }
                }
                1 => {
                    for y in 0..self.cy {
                        self.grid[y] = vec![BLANK; self.cols];
                    }
                    for x in 0..=self.cx.min(self.cols - 1) {
                        self.grid[self.cy][x] = BLANK;
                    }
                }
                _ => {
                    for y in 0..self.rows {
                        self.grid[y] = vec![BLANK; self.cols];
                    }
                }
            },
            b'K' => match self.params.first().copied().unwrap_or(0) {
                0 => self.erase_line_from(self.cy, self.cx),
                1 => {
                    for x in 0..=self.cx.min(self.cols - 1) {
                        self.grid[self.cy][x] = BLANK;
                    }
                }
                _ => self.grid[self.cy] = vec![BLANK; self.cols],
            },
            b'X' => {
                for x in self.cx..(self.cx + self.p(0, 1)).min(self.cols) {
                    self.grid[self.cy][x] = BLANK;
                }
            }
            b'L' => self.insert_lines(self.p(0, 1)),
            b'M' => self.delete_lines(self.p(0, 1)),
            b'@' => {
                let n = self.p(0, 1).min(self.cols - self.cx);
                let row = &mut self.grid[self.cy];
                for _ in 0..n {
                    row.insert(self.cx, BLANK);
                    row.pop();
                }
            }
            b'P' => {
                let n = self.p(0, 1).min(self.cols - self.cx);
                let row = &mut self.grid[self.cy];
                for _ in 0..n {
                    row.remove(self.cx);
                    row.push(BLANK);
                }
            }
            b'S' => (0..self.p(0, 1)).for_each(|_| self.scroll_up()),
            b'T' => (0..self.p(0, 1)).for_each(|_| self.scroll_down()),
            b'r' => {
                let (t, b) = (self.p(0, 1) - 1, self.p(1, self.rows) - 1);
                if t < b && b < self.rows {
                    self.top = t;
                    self.bottom = b;
                } else {
                    self.top = 0;
                    self.bottom = self.rows - 1;
                }
                self.cx = 0;
                self.cy = 0;
            }
            b's' => self.saved = (self.cx, self.cy),
            b'u' => {
                self.cx = self.saved.0.min(self.cols - 1);
                self.cy = self.saved.1.min(self.rows - 1);
            }
            b'm' => {
                if self.params.is_empty() {
                    self.reverse = false;
                }
                for &v in &self.params.clone() {
                    match v {
                        0 | 27 => self.reverse = false,
                        7 => self.reverse = true,
                        _ => {}
                    }
                }
            }
            b'h' | b'l' if self.private => {
                let on = fin == b'h';
                for &v in &self.params.clone() {
                    match v {
                        25 => self.cursor_visible = on,
                        // Alternate screen: start from a clean screen.
                        47 | 1047 | 1049 if on => {
                            for y in 0..self.rows {
                                self.grid[y] = vec![BLANK; self.cols];
                            }
                            self.cx = 0;
                            self.cy = 0;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn erase_line_from(&mut self, y: usize, x: usize) {
        for c in x.min(self.cols)..self.cols {
            self.grid[y][c] = BLANK;
        }
    }

    fn put(&mut self, ch: char) {
        let w = if is_wide(ch) { 2 } else { 1 };
        if self.wrap_pending || self.cx + w > self.cols {
            self.cx = 0;
            self.linefeed();
        }
        self.wrap_pending = false;
        let cell = Cell { ch, reverse: self.reverse };
        self.grid[self.cy][self.cx] = cell;
        if w == 2 {
            self.grid[self.cy][self.cx + 1] = CONT;
        }
        self.cx += w;
        if self.cx >= self.cols {
            self.cx = self.cols - 1;
            self.wrap_pending = true;
        }
    }

    fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.cy == self.bottom {
            self.scroll_up();
        } else if self.cy + 1 < self.rows {
            self.cy += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cy == self.top {
            self.scroll_down();
        } else {
            self.cy = self.cy.saturating_sub(1);
        }
    }

    fn scroll_up(&mut self) {
        self.grid.remove(self.top);
        self.grid.insert(self.bottom, vec![BLANK; self.cols]);
    }

    fn scroll_down(&mut self) {
        self.grid.remove(self.bottom);
        self.grid.insert(self.top, vec![BLANK; self.cols]);
    }

    fn insert_lines(&mut self, n: usize) {
        if self.cy < self.top || self.cy > self.bottom {
            return;
        }
        for _ in 0..n.min(self.bottom - self.cy + 1) {
            self.grid.remove(self.bottom);
            self.grid.insert(self.cy, vec![BLANK; self.cols]);
        }
    }

    fn delete_lines(&mut self, n: usize) {
        if self.cy < self.top || self.cy > self.bottom {
            return;
        }
        for _ in 0..n.min(self.bottom - self.cy + 1) {
            self.grid.remove(self.cy);
            self.grid.insert(self.bottom, vec![BLANK; self.cols]);
        }
    }
}

/// Bytes a key press sends to the program, for keys that are not text.
pub fn key_bytes(key: Keycode, mods: Mod) -> Option<Vec<u8>> {
    let ctrl = mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
    if ctrl {
        let name = key.name();
        if name.len() == 1 {
            let c = name.as_bytes()[0].to_ascii_uppercase();
            if c.is_ascii_uppercase() {
                return Some(vec![c - b'A' + 1]);
            }
        }
    }
    Some(match key {
        Keycode::Return | Keycode::KpEnter => b"\r".to_vec(),
        Keycode::Backspace => vec![0x7f],
        Keycode::Tab => b"\t".to_vec(),
        Keycode::Escape => vec![0x1b],
        Keycode::Up => b"\x1b[A".to_vec(),
        Keycode::Down => b"\x1b[B".to_vec(),
        Keycode::Right => b"\x1b[C".to_vec(),
        Keycode::Left => b"\x1b[D".to_vec(),
        Keycode::Home => b"\x1b[H".to_vec(),
        Keycode::End => b"\x1b[F".to_vec(),
        Keycode::PageUp => b"\x1b[5~".to_vec(),
        Keycode::PageDown => b"\x1b[6~".to_vec(),
        Keycode::Delete => b"\x1b[3~".to_vec(),
        Keycode::Insert => b"\x1b[2~".to_vec(),
        _ => return None,
    })
}

/// A running program attached to a PTY.
pub struct Terminal {
    pub vt: Vt,
    master: OwnedFd,
    child: Child,
    shared: Arc<Shared>,
    pub exit_code: Option<i32>,
    /// Show "press any key" after the program exits.
    pub pause_on_exit: bool,
}

impl Terminal {
    pub fn spawn(argv: &[String], workdir: Option<&std::path::Path>, shared: Arc<Shared>, pause_on_exit: bool) -> Result<Self, String> {
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        // The size of the surface it starts on; `draw` follows it when the window changes.
        let (_, cols, rows) = layout();
        let mut ws = libc::winsize { ws_row: rows as u16, ws_col: cols as u16, ws_xpixel: 0, ws_ypixel: 0 };
        let r = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws) };
        if r != 0 {
            return Err(format!("openpty: {}", std::io::Error::last_os_error()));
        }
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        unsafe {
            let fl = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        let dup = |fd: &OwnedFd| fd.try_clone().map_err(|e| e.to_string());
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .env("TERM", "vt100")
            .env("COLUMNS", cols.to_string())
            .env("LINES", rows.to_string())
            .stdin(Stdio::from(dup(&slave)?))
            .stdout(Stdio::from(dup(&slave)?))
            .stderr(Stdio::from(dup(&slave)?));
        if let Some(d) = workdir.filter(|d| d.is_dir()) {
            cmd.current_dir(d);
        }
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| format!("{}: {e}", argv[0]))?;
        drop(slave);
        shared.child_pgid.store(child.id() as i32, Ordering::SeqCst);
        Ok(Self { vt: Vt::new(cols, rows), master, child, shared, exit_code: None, pause_on_exit })
    }

    pub fn write(&self, bytes: &[u8]) {
        if self.exit_code.is_none() {
            unsafe { libc::write(self.master.as_raw_fd(), bytes.as_ptr() as *const _, bytes.len()) };
        }
    }

    pub fn send_key(&self, key: Keycode, mods: Mod) {
        if let Some(b) = key_bytes(key, mods) {
            self.write(&b);
        }
    }

    /// Read pending output and check for exit. Returns true when anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(self.master.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len()) };
            if n > 0 {
                self.vt.feed(&buf[..n as usize]);
                changed = true;
            } else {
                break;
            }
        }
        if self.exit_code.is_none() {
            if let Ok(Some(st)) = self.child.try_wait() {
                self.exit_code = Some(st.code().unwrap_or(-1));
                self.shared.child_pgid.store(0, Ordering::SeqCst);
                unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
                changed = true;
            }
        }
        changed
    }

    /// Follow the surface's size: resize the screen and tell the program (TIOCSWINSZ on the pty,
    /// which sends it SIGWINCH). Nothing happens while the size stays the same.
    pub fn fit(&mut self) {
        let (_, cols, rows) = layout();
        if (cols, rows) == (self.vt.cols, self.vt.rows) {
            return;
        }
        self.vt.resize(cols, rows);
        let ws = libc::winsize { ws_row: rows as u16, ws_col: cols as u16, ws_xpixel: 0, ws_ypixel: 0 };
        unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) };
    }

    pub fn exited(&self) -> bool {
        self.exit_code.is_some()
    }

    /// Draw on the current surface; resizes the grid first when the surface changed size.
    pub fn draw(&mut self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager) {
        self.fit();
        let ink = tm.get_color("foreground");
        let paper = tm.get_color("widget_face_active");
        let area = Rect::new(0, BAR_H, crate::chrome::screen_w(), crate::chrome::screen_h() - BAR_H as u32);
        ui::fill_rect(c, area, paper);
        crate::grid::draw_app_frame(c, crate::grid::app_frame().y(), ink, None);
        let ((x0, y0), _, _) = layout();
        for (y, row) in self.vt.grid.iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                let (px, py) = (x0 + x as i32 * CELL_W, y0 + y as i32 * CELL_H);
                let cursor = self.vt.cursor_visible && !self.exited() && x == self.vt.cx && y == self.vt.cy;
                let inv = cell.reverse != cursor;
                let wide = row.get(x + 1).map(|n| n.ch == '\0').unwrap_or(false);
                if inv {
                    let w = if wide { CELL_W * 2 } else { CELL_W };
                    ui::fill_rect(c, Rect::new(px, py, w as u32, CELL_H as u32), ink);
                }
                if cell.ch != ' ' && cell.ch != '\0' {
                    let color = if inv { paper } else { ink };
                    match &fs.terminal {
                        // ASCII and the other characters the terminal face has: fixed 6 px pitch.
                        Some(tf) if tf.has(cell.ch) && !wide => tf.draw_char_centered(c, px, py, cell.ch, CELL_W, color),
                        // Everything else (e.g. Japanese) falls back to M+, nudged up to fit the row.
                        _ => {
                            let mut tmp = [0u8; 4];
                            fs.draw_text(c, px, py - 1, cell.ch.encode_utf8(&mut tmp), color);
                        }
                    }
                }
            }
        }
        if let Some(code) = self.exit_code {
            if self.pause_on_exit {
                let msg = crate::launcher::i18n::tr_s(&format!("[exited {code}]  press any key"));
                let w = fs.measure_text(&msg) + 8;
                // Over the last rows of the text, inside the content area.
                let r = Rect::new((crate::grid::sw() - w) / 2, crate::grid::app_content().bottom() - 2 - 16, w as u32, 16);
                ui::fill_rect(c, r, ink);
                fs.draw_text(c, r.x() + 4, r.y() + 1, &msg, paper);
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Hang up the program and reap it.
        if self.exit_code.is_none() {
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGHUP) };
            std::thread::sleep(std::time::Duration::from_millis(30));
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            let _ = self.child.wait();
        }
        self.shared.child_pgid.store(0, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vt(s: &str) -> Vt {
        let mut v = Vt::new(20, 5);
        v.feed(s.as_bytes());
        v
    }

    #[test]
    fn the_text_grid_keeps_a_pixel_to_the_frame() {
        let ct = crate::grid::app_content();
        let (x0, y0) = ORIGIN;
        let (x1, y1) = (x0 + COLS as i32 * CELL_W, y0 + ROWS as i32 * CELL_H);
        assert!(x0 >= ct.x() && y0 >= ct.y() && x1 <= ct.right() && y1 <= ct.bottom(), "inside the content area, clear of the line and its corners");
        assert_eq!(ROWS, 13);
        assert!((ct.width() as i32) < (COLS as i32 + 1) * CELL_W, "no room for another column");
        assert!((ct.height() as i32) < (ROWS as i32 + 1) * CELL_H, "no room for another row");
    }

    #[test]
    fn the_layout_at_320x170_is_the_fixed_grid_and_grows_with_the_surface() {
        assert_eq!(layout(), (ORIGIN, COLS, ROWS));
        crate::grid::with_surface((640, 400), || {
            let ct = crate::grid::app_content();
            let ((x0, y0), cols, rows) = layout();
            assert!(cols > COLS && rows > ROWS, "{cols} x {rows}");
            assert!(x0 + cols as i32 * CELL_W <= ct.right() && y0 + rows as i32 * CELL_H <= ct.bottom());
            assert!((ct.width() as i32) < (cols as i32 + 1) * CELL_W + 1, "no room for another column");
            assert!((ct.height() as i32) < (rows as i32 + 1) * CELL_H, "no room for another row");
        });
    }

    #[test]
    fn resizing_keeps_the_text_and_the_cursor_line() {
        let mut v = vt("1\r\n2\r\n3\r\n4\r\nabcdef");
        v.resize(30, 8);
        assert_eq!((v.cols, v.rows, v.grid.len(), v.grid[0].len()), (30, 8, 8, 30));
        assert_eq!(v.row_text(0), "1");
        assert_eq!(v.row_text(4), "abcdef");
        assert_eq!((v.cx, v.cy), (6, 4));
        // Fewer rows: the top lines scroll off, the cursor's line stays.
        v.resize(4, 2);
        assert_eq!(v.grid.iter().map(|r| r.len()).collect::<Vec<_>>(), [4, 4]);
        assert_eq!(v.row_text(0), "4");
        assert_eq!(v.row_text(1), "abcd");
        assert_eq!((v.cx, v.cy), (3, 1));
        // The scroll region is the whole new screen: a line feed at the bottom scrolls.
        v.feed(b"\r\nz");
        assert_eq!(v.row_text(0), "abcd");
        assert_eq!(v.row_text(1), "z");
        // A wide character cut at the new right edge is cleared, not left half-drawn.
        let mut v = vt("ab日");
        v.resize(3, 5);
        assert_eq!(v.row_text(0), "ab");
    }

    #[test]
    fn plain_text_and_newlines() {
        let v = vt("hello\r\nworld");
        assert_eq!(v.row_text(0), "hello");
        assert_eq!(v.row_text(1), "world");
        assert_eq!((v.cx, v.cy), (5, 1));
    }

    #[test]
    fn wraps_at_the_right_edge() {
        let v = vt(&"x".repeat(25));
        assert_eq!(v.row_text(0).len(), 20);
        assert_eq!(v.row_text(1), "xxxxx");
    }

    #[test]
    fn scrolls_when_full() {
        let v = vt("1\r\n2\r\n3\r\n4\r\n5\r\n6");
        assert_eq!(v.row_text(0), "2");
        assert_eq!(v.row_text(4), "6");
    }

    #[test]
    fn cursor_movement_and_erase() {
        let v = vt("abcdef\x1b[3D\x1b[K");
        assert_eq!(v.row_text(0), "abc");
        let v = vt("\x1b[3;5Hx");
        assert_eq!(v.grid[2][4].ch, 'x');
        let v = vt("one\r\ntwo\x1b[2J\x1b[H");
        assert!((0..5).all(|y| v.row_text(y).is_empty()));
        assert_eq!((v.cx, v.cy), (0, 0));
    }

    #[test]
    fn backspace_tab_and_cr_overwrite() {
        let v = vt("abc\x08\x08X\rZ");
        assert_eq!(v.row_text(0), "ZXc");
        let v = vt("a\tb");
        assert_eq!(v.grid[0][8].ch, 'b');
    }

    #[test]
    fn utf8_and_wide_characters() {
        let v = vt("é日本");
        assert_eq!(v.grid[0][0].ch, 'é');
        assert_eq!(v.grid[0][1].ch, '日');
        assert_eq!(v.grid[0][2], CONT);
        assert_eq!(v.grid[0][3].ch, '本');
        assert_eq!(v.row_text(0), "é日本");
        // Split multi-byte sequence across feeds.
        let mut v = Vt::new(10, 2);
        v.feed(&[0xe6, 0x97]);
        v.feed(&[0xa5]);
        assert_eq!(v.grid[0][0].ch, '日');
    }

    #[test]
    fn sgr_reverse_and_unknown_sequences_are_ignored() {
        let v = vt("a\x1b[7mb\x1b[0mc\x1b[1;32mz\x1b]0;title\x07!");
        assert!(!v.grid[0][0].reverse);
        assert!(v.grid[0][1].reverse);
        assert!(!v.grid[0][2].reverse);
        assert_eq!(v.row_text(0), "abcz!");
    }

    #[test]
    fn scroll_region_and_line_insert_delete() {
        let mut v = Vt::new(10, 5);
        v.feed(b"A\r\nB\r\nC\r\nD\r\nE");
        v.feed(b"\x1b[2;4r\x1b[4;1H\n"); // scroll inside rows 2..4
        assert_eq!(v.row_text(0), "A");
        assert_eq!(v.row_text(1), "C");
        assert_eq!(v.row_text(3), "");
        assert_eq!(v.row_text(4), "E");
        let mut v = Vt::new(10, 4);
        v.feed(b"1\r\n2\r\n3\x1b[2;1H\x1b[L");
        assert_eq!(v.row_text(1), "");
        assert_eq!(v.row_text(2), "2");
        v.feed(b"\x1b[M");
        assert_eq!(v.row_text(1), "2");
    }

    #[test]
    fn key_translation() {
        assert_eq!(key_bytes(Keycode::Return, Mod::NOMOD).unwrap(), b"\r");
        assert_eq!(key_bytes(Keycode::Up, Mod::NOMOD).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(Keycode::C, Mod::LCTRLMOD).unwrap(), vec![3]);
        assert_eq!(key_bytes(Keycode::D, Mod::LCTRLMOD).unwrap(), vec![4]);
        assert!(key_bytes(Keycode::A, Mod::NOMOD).is_none(), "printable keys arrive as text");
    }

    #[test]
    fn runs_a_real_program_through_a_pty() {
        let shared = Arc::new(Shared::default());
        let mut t = Terminal::spawn(&["/bin/sh".into(), "-c".into(), "printf 'hi\\n'; exit 3".into()], None, shared, true).unwrap();
        for _ in 0..100 {
            t.poll();
            if t.exited() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        t.poll();
        assert_eq!(t.exit_code, Some(3));
        assert_eq!((t.vt.cols, t.vt.rows), (COLS, ROWS));
        assert_eq!(t.vt.row_text(0), "hi");
    }

    #[test]
    fn input_reaches_the_program() {
        let shared = Arc::new(Shared::default());
        let mut t = Terminal::spawn(&["/bin/cat".into()], None, shared, false).unwrap();
        t.write(b"ping\r");
        let mut seen = false;
        for _ in 0..100 {
            t.poll();
            if t.vt.row_text(0).starts_with("ping") {
                seen = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(seen, "echoed input should appear: {:?}", t.vt.row_text(0));
    }
}
