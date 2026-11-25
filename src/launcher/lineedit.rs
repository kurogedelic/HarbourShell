//! A one-line text field with a caret (columns count characters, never bytes), shared by the
//! Sheet cell editor and the Browser address bar / form fields.

use crate::keys::{Keycode, Mod};
use crate::gfx::Rect;
use crate::gfx::Canvas;

use crate::font::FontSystem;
use crate::theme::ThemeManager;
use crate::ui;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineEdit {
    pub text: String,
    /// Caret position in characters (0..=len).
    pub caret: usize,
}

fn byte_at(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map(|(b, _)| b).unwrap_or(s.len())
}

impl LineEdit {
    /// A field holding `text` with the caret at the end.
    pub fn new(text: &str) -> Self {
        Self { text: text.to_string(), caret: text.chars().count() }
    }

    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }

    /// Insert typed text at the caret (control characters are dropped).
    pub fn insert(&mut self, t: &str) {
        let t: String = t.chars().filter(|c| !c.is_control()).collect();
        let b = byte_at(&self.text, self.caret);
        self.text.insert_str(b, &t);
        self.caret += t.chars().count();
    }

    /// Left / Right / Home / End / Backspace / Delete and Ctrl+U (clear). True if the key was one
    /// of them.
    pub fn key(&mut self, key: Keycode, mods: Mod) -> bool {
        let ctrl = mods.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
        let n = self.len();
        match key {
            Keycode::Left => self.caret = self.caret.saturating_sub(1),
            Keycode::Right => self.caret = (self.caret + 1).min(n),
            Keycode::Home => self.caret = 0,
            Keycode::End => self.caret = n,
            Keycode::Backspace => {
                if self.caret > 0 {
                    let b = byte_at(&self.text, self.caret - 1);
                    self.text.remove(b);
                    self.caret -= 1;
                }
            }
            Keycode::Delete => {
                if self.caret < n {
                    let b = byte_at(&self.text, self.caret);
                    self.text.remove(b);
                }
            }
            Keycode::U if ctrl => self.clear(),
            _ => return false,
        }
        true
    }

    /// X offset of the caret from the start of the text.
    pub fn caret_x(&self, fs: &FontSystem) -> i32 {
        let before: String = self.text.chars().take(self.caret).collect();
        fs.measure_text(&before)
    }

    /// The caret as a 1 px bar in the field's row, with the capitals' height.
    pub fn draw_caret(&self, c: &mut Canvas, fs: &FontSystem, tm: &ThemeManager, x: i32, row: Rect) {
        let y = crate::launcher::widgets::text_origin(row.y(), row.height() as i32) + crate::launcher::widgets::CAP_TOP - 1;
        ui::fill_rect(c, Rect::new(x + self.caret_x(fs), y, 1, 11), tm.get_color("foreground"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_and_editing_count_characters_not_bytes() {
        let mut e = LineEdit::new("日本語");
        assert_eq!(e.caret, 3);
        e.insert("ab\n");
        assert_eq!(e.text, "日本語ab");
        e.key(Keycode::Left, Mod::NOMOD);
        e.key(Keycode::Backspace, Mod::NOMOD);
        assert_eq!(e.text, "日本語b");
        e.key(Keycode::Home, Mod::NOMOD);
        e.key(Keycode::Delete, Mod::NOMOD);
        assert_eq!(e.text, "本語b");
        e.key(Keycode::End, Mod::NOMOD);
        assert_eq!(e.caret, 3);
        e.insert("é");
        assert_eq!(e.text, "本語bé");
        assert!(e.key(Keycode::U, Mod::LCTRLMOD));
        assert!(e.is_empty() && e.caret == 0);
        assert!(!e.key(Keycode::A, Mod::NOMOD), "other keys are not handled");
    }

    #[test]
    fn the_caret_stops_at_both_ends() {
        let mut e = LineEdit::new("ab");
        for _ in 0..5 {
            e.key(Keycode::Right, Mod::NOMOD);
        }
        assert_eq!(e.caret, 2);
        for _ in 0..5 {
            e.key(Keycode::Left, Mod::NOMOD);
        }
        assert_eq!(e.caret, 0);
        e.key(Keycode::Backspace, Mod::NOMOD);
        assert_eq!(e.text, "ab");
    }
}
