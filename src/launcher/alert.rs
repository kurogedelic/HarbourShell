//! An alert as a reusable UI part: a double frame (outer rule, a gap, inner rule) on a white
//! plate with a hard shadow, an icon at the left (stop sign, caution triangle, note bubble, bomb: the 32 x 32
//! system icon from `assets/images/system` when it is drawn, else a built-in 24 x 24 shape), the
//! message to its right, and one or more rounded buttons at the bottom right (the default one wears the ring).
//!
//! The owner keeps the `Alert` as state, calls `draw` and routes keys and clicks through `key` / `click`, which
//! return the index of the button that was chosen.

use sdl2::keyboard::Keycode;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::font::FontSystem;
use crate::grid;
use crate::launcher::i18n;
use crate::launcher::widgets::{contains, draw_button, BtnState};
use crate::theme::ThemeManager;
use crate::ui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertIcon {
    /// An octagonal stop sign with an exclamation mark: something cannot be done.
    Stop,
    /// A caution triangle: be careful.
    Caution,
    /// A speech bubble with an "i": information.
    Note,
    /// The bomb: a system-level failure.
    Bomb,
}

pub struct Alert {
    pub icon: AlertIcon,
    /// First line, in the display face.
    pub title: String,
    /// Explanation, word-wrapped under the title.
    pub text: String,
    pub buttons: Vec<String>,
    /// The button Return chooses.
    pub sel: usize,
    /// Pointer hover, for the buttons.
    hover: Option<usize>,
    /// A button drawn pressed (inverted): the owner sets it for a moment after the button was chosen.
    pub pressed: Option<usize>,
    /// Busy: the buttons are greyed out (the owner shows a progress text instead of the message).
    pub busy: bool,
}

const W: i32 = 272;
const ICON: i32 = 24;
const BTN_W: i32 = 72;
const BTN_H: i32 = 20;

impl Alert {
    pub fn new(icon: AlertIcon, title: &str, text: &str, buttons: &[&str]) -> Self {
        Self { icon, title: title.to_string(), text: text.to_string(), buttons: buttons.iter().map(|b| b.to_string()).collect(), sel: buttons.len().saturating_sub(1), hover: None, pressed: None, busy: false }
    }

    /// Greedy word wrap that also breaks long unspaced runs (Japanese).
    fn wrap(&self, fs: &FontSystem, width: i32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut cur = String::new();
        for ch in i18n::tr_s(&self.text).chars() {
            if ch == '\n' {
                lines.push(std::mem::take(&mut cur));
                continue;
            }
            let mut next = cur.clone();
            next.push(ch);
            if fs.measure_text(&next) > width && !cur.is_empty() {
                // Prefer to break at the last space.
                if let Some(sp) = cur.rfind(' ') {
                    let (head, tail) = cur.split_at(sp);
                    lines.push(head.to_string());
                    cur = format!("{}{}", tail.trim_start(), ch);
                } else {
                    lines.push(std::mem::take(&mut cur));
                    cur.push(ch);
                }
            } else {
                cur = next;
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        lines
    }

    /// The card, centred in the area under the menu strip.
    pub fn rect(&self, fs: &FontSystem) -> Rect {
        let lines = self.wrap(fs, W - 24 - ICON - 14).len() as i32;
        let h = (14 + 16 + lines * 14 + 10 + BTN_H + 14).max(ICON + 44 + BTN_H);
        let h = h.min(grid::sh() - grid::BAR_H - 8);
        Rect::new((grid::sw() - W) / 2, grid::BAR_H + (grid::sh() - grid::BAR_H - h) / 2, W as u32, h as u32)
    }

    fn button_rects(&self, r: Rect) -> Vec<Rect> {
        let n = self.buttons.len() as i32;
        let y = r.bottom() - 10 - BTN_H;
        (0..n).map(|i| Rect::new(r.right() - 14 - (n - i) * BTN_W - (n - 1 - i) * 12, y, BTN_W as u32, BTN_H as u32)).collect()
    }

    /// Index of the button under a click.
    pub fn click(&mut self, fs: &FontSystem, x: i32, y: i32) -> Option<usize> {
        let r = self.rect(fs);
        self.button_rects(r).iter().position(|b| contains(*b, x, y))
    }

    pub fn hover(&mut self, fs: &FontSystem, x: i32, y: i32) {
        let r = self.rect(fs);
        self.hover = self.button_rects(r).iter().position(|b| contains(*b, x, y));
    }

    /// Return / Space choose the default button, Left / Right move between buttons, Esc chooses the last
    /// button when there is only one, otherwise the first (the "cancel" side).
    pub fn key(&mut self, key: Keycode) -> Option<usize> {
        match key {
            Keycode::Return | Keycode::KpEnter | Keycode::Space => return Some(self.sel),
            Keycode::Left => self.sel = self.sel.saturating_sub(1),
            Keycode::Right => self.sel = (self.sel + 1).min(self.buttons.len().saturating_sub(1)),
            Keycode::Escape => return Some(if self.buttons.len() == 1 { 0 } else { 0 }),
            _ => {}
        }
        None
    }

    pub fn draw(&self, c: &mut Canvas<Window>, fs: &FontSystem, tm: &ThemeManager) {
        let ink = tm.get_color("foreground");
        let white = tm.get_color("icon_white");
        let r = self.rect(fs);
        // Hard shadow, then the double frame on a white plate.
        ui::fill_rect(c, Rect::new(r.x() + 3, r.y() + 3, r.width(), r.height()), ink);
        ui::fill_rect(c, r, white);
        ui::draw_rect(c, r, ink);
        ui::draw_rect(c, Rect::new(r.x() + 3, r.y() + 3, r.width() - 6, r.height() - 6), ink);
        // The drawn system icon (32 x 32, assets/images/system) when there is one; else the built-in
        // 24 x 24 shape. Both end where the text starts (x + 52), so the layout is the same.
        if !crate::launcher::sysicon::draw(c, tm, self.icon, r.x() + 10, r.y() + 10) {
            draw_icon(c, self.icon, r.x() + 14, r.y() + 12, ink, white);
        }
        let tx = r.x() + 14 + ICON + 14;
        let tw = r.width() as i32 - (tx - r.x()) - 12;
        if !self.title.is_empty() {
            fs.draw_display_text(c, tx, r.y() + 10, &crate::chrome::fit_display_text(fs, &i18n::tr_s(&self.title), tw), ink);
        }
        let mut y = r.y() + 10 + if self.title.is_empty() { 0 } else { 16 };
        for l in self.wrap(fs, tw) {
            fs.draw_text(c, tx, y, &l, ink);
            y += 14;
        }
        for (i, (b, rect)) in self.buttons.iter().zip(self.button_rects(r)).enumerate() {
            draw_button(c, fs, tm, rect, b, BtnState { default: i == self.sel, hover: self.hover == Some(i), pressed: self.pressed == Some(i), disabled: self.busy && self.pressed != Some(i) });
        }
    }
}

/// Draws a 24 x 24 alert icon at (x, y): shapes in `ink`, detail cut out in `white`.
pub fn draw_icon(c: &mut Canvas<Window>, icon: AlertIcon, x: i32, y: i32, ink: sdl2::pixels::Color, white: sdl2::pixels::Color) {
    let px = |c: &mut Canvas<Window>, col: sdl2::pixels::Color, rows: &[(i32, i32, i32)]| {
        c.set_draw_color(col);
        for &(row, x0, x1) in rows {
            let _ = c.draw_line((x + x0, y + row), (x + x1, y + row));
        }
    };
    match icon {
        AlertIcon::Stop => {
            // Octagon: filled, with a white exclamation mark.
            let rows: Vec<(i32, i32, i32)> = (0..24).map(|r| (r, [7, 5, 3, 2, 1, 1, 0, 0].get(r.min(23 - r) as usize).copied().unwrap_or(0), 23 - [7, 5, 3, 2, 1, 1, 0, 0].get(r.min(23 - r) as usize).copied().unwrap_or(0))).collect();
            px(c, ink, &rows);
            px(c, white, &(4..15).map(|r| (r, 10, 13)).collect::<Vec<_>>());
            px(c, white, &(17..20).map(|r| (r, 10, 13)).collect::<Vec<_>>());
        }
        AlertIcon::Caution => {
            // Triangle: a 2 px outline with an exclamation mark.
            for r in 2..23 {
                let half = (r - 2) * 11 / 20;
                px(c, ink, &[(r, 11 - half, 12 + half)]);
                if r > 4 && r < 22 {
                    let inner = ((r - 2) * 11 / 20 - 2).max(0);
                    px(c, white, &[(r, 12 - inner.min(11), 11 + inner.min(11))]);
                }
            }
            px(c, ink, &(8..16).map(|r| (r, 11, 12)).collect::<Vec<_>>());
            px(c, ink, &(18..20).map(|r| (r, 11, 12)).collect::<Vec<_>>());
        }
        AlertIcon::Note => {
            // Speech bubble with an "i".
            let rows: Vec<(i32, i32, i32)> = (1..18).map(|r| (r, [6, 3, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2].get((r - 1) as usize).copied().unwrap_or(0), 23 - [6, 3, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2].get((r - 1) as usize).copied().unwrap_or(0))).collect();
            px(c, ink, &rows);
            px(c, ink, &[(18, 5, 11), (19, 4, 9), (20, 3, 7), (21, 3, 5)]);
            px(c, white, &(2..17).map(|r| (r, 3, 20)).collect::<Vec<_>>());
            px(c, ink, &[(5, 11, 12), (6, 11, 12)]);
            px(c, ink, &(9..14).map(|r| (r, 11, 12)).collect::<Vec<_>>());
        }
        AlertIcon::Bomb => {
            // A round bomb (filled disc with a white highlight), a fuse and a spark.
            c.set_draw_color(ink);
            for row in 0..24i32 {
                for col in 0..24i32 {
                    let (dx, dy) = (col as f32 - 10.0, row as f32 - 15.0);
                    if dx * dx + dy * dy <= 8.6 * 8.6 {
                        let _ = c.draw_point((x + col, y + row));
                    }
                }
            }
            px(c, white, &[(11, 5, 6), (12, 4, 5), (13, 4, 4)]);
            px(c, ink, &[(7, 14, 15), (6, 15, 16), (5, 16, 17), (4, 17, 18), (3, 18, 19)]);
            px(c, ink, &[(0, 21, 21), (1, 20, 22), (2, 19, 23), (1, 18, 18), (3, 21, 21), (2, 22, 22)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_choose_buttons_and_escape_picks_the_first() {
        let mut a = Alert::new(AlertIcon::Stop, "No camera found.", "Plug in a USB camera.", &["Cancel", "Scan Again"]);
        assert_eq!(a.sel, 1, "the last button is the default");
        assert_eq!(a.key(Keycode::Left), None);
        assert_eq!(a.sel, 0);
        assert_eq!(a.key(Keycode::Return), Some(0));
        assert_eq!(a.key(Keycode::Escape), Some(0));
        let mut one = Alert::new(AlertIcon::Caution, "", "x", &["OK"]);
        assert_eq!(one.key(Keycode::Return), Some(0));
    }

    #[test]
    fn buttons_sit_inside_the_card_at_the_bottom_right() {
        let fs = FontSystem::new();
        let a = Alert::new(AlertIcon::Note, "Hi", "A short message.", &["Cancel", "OK"]);
        let r = a.rect(&fs);
        assert!(r.bottom() <= grid::sh() && r.y() >= grid::BAR_H, "the card fits under the menu strip");
        let b = a.button_rects(r);
        assert_eq!(b.len(), 2);
        assert!(b[0].right() < b[1].x(), "buttons do not overlap");
        assert!(b[1].right() <= r.right() - 10 && b[0].bottom() <= r.bottom() - 6);
    }
}
