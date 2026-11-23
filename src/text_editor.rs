//! TextEditor v0 - 最初の実用アプリ。
//!
//! 要件:
//! - テキスト入力 / カーソル移動 / 改行 / Backspace / スクロール
//! - しない: 保存・開く・検索・Undo・ファイルシステム連携
//!
//! 方針:
//! - 単色（Black 背景 + Amber 文字/ボーダー/カーソル）
//! - fill は使わず、outline とテキスト描画のみ
//! - キーボードだけで完結
//!
//! データモデル:
//! - 行の Vec（各行は String）。空ドキュメントは [String::new()] 1行。
//! - カーソルは (row, col)。常に 0..=len の範囲。

use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use crate::font::FontSystem;
use crate::theme::ThemeManager;

/// グリッドサイズ（M+ F12R: 6x13）
const CELL_W: i32 = 6;
const CELL_H: i32 = 13;
/// エディタ領域の内側余白
/// Space between the view (`grid::app_content`, already 1 px clear of the frame) and the text: 4 px
/// at the sides, none above and below, so eleven 13 px lines fit in its 143 px.
const PAD_X: i32 = 4;
const PAD_Y: i32 = 0;
const MAX_UNDO: usize = 64;

#[derive(Clone)]
struct EditorSnapshot {
    lines: Vec<String>,
    row: usize,
    col: usize,
    scroll: usize,
    dirty: bool,
}

pub struct TextEditor {
    /// 行データ。必ず1行以上。
    lines: Vec<String>,
    /// カーソル位置 (row, col)
    row: usize,
    col: usize,
    /// 縦スクロール量（表示開始行）
    scroll: usize,
    /// 表示領域サイズ（draw 時に更新）
    view_w: u32,
    view_h: u32,
    /// 保存後に編集されたかどうか。
    dirty: bool,
    undo_stack: Vec<EditorSnapshot>,
    /// Top-left of the text as last drawn (for the mouse).
    text_origin: std::cell::Cell<(i32, i32)>,
}

impl TextEditor {
    pub fn new() -> Self {
        TextEditor {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            scroll: 0,
            view_w: 0,
            view_h: 0,
            dirty: false,
            undo_stack: Vec::new(),
            text_origin: std::cell::Cell::new((PAD_X, PAD_Y)),
        }
    }

    /// 既存テキストから生成（ファイルを開く時）。
    pub fn from_text(content: &str) -> Self {
        let lines: Vec<String> = if content.is_empty() {
            vec![String::new()]
        } else {
            content.split('\n').map(|s| s.to_string()).collect()
        };
        let row = lines.len() - 1;
        TextEditor {
            lines,
            row,
            col: 0,
            scroll: 0,
            view_w: 0,
            view_h: 0,
            dirty: false,
            undo_stack: Vec::new(),
            text_origin: std::cell::Cell::new((PAD_X, PAD_Y)),
        }
    }

    /// 全テキストを結合して返す（保存用）。
    pub fn text_for_save(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    pub fn undo(&mut self) {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.lines = snapshot.lines;
            self.row = snapshot.row.min(self.lines.len().saturating_sub(1));
            self.col = snapshot.col.min(self.lines[self.row].chars().count());
            self.scroll = snapshot.scroll;
            self.dirty = snapshot.dirty;
            self.clamp_scroll();
        }
    }

    /// 表示領域サイズを更新（描画前に呼ぶ）。
    pub fn set_view(&mut self, w: u32, h: u32) {
        self.view_w = w;
        self.view_h = h;
        self.clamp_scroll();
    }

    // ---- 入力 ----------------------------------------------------------

    /// SDL2 のテキスト入力イベント（Unicode 文字）を処理。
    pub fn input_char(&mut self, ch: char) {
        // 制御文字は無視（改行は別途 input_key で処理）
        if ch.is_control() {
            return;
        }
        self.push_undo();
        // そのまま挿入。描画は FontSystem が Latin-1 / JIS フォントで
        // フォールバック描画する（日本語対応）。
        let line = &mut self.lines[self.row];
        let at = byte_at(line, self.col);
        line.insert(at, ch);
        self.col += 1;
        self.dirty = true;
    }

    /// キー入力を処理。終了すべき場合は true を返す（Esc）。
    pub fn input_key(&mut self, key: Keycode, keymod: Mod) -> bool {
        match key {
            Keycode::Escape => return true,

            Keycode::Z if is_ctrl(keymod) => {
                self.undo();
            }

            Keycode::Return | Keycode::KpEnter => {
                self.push_undo();
                // 現在行をカーソル位置で分割
                let at = byte_at(&self.lines[self.row], self.col);
                let after: String = self.lines[self.row].split_off(at);
                self.lines.insert(self.row + 1, after);
                self.row += 1;
                self.col = 0;
                self.dirty = true;
            }

            Keycode::Backspace => {
                if self.col > 0 {
                    self.push_undo();
                    let at = byte_at(&self.lines[self.row], self.col - 1);
                    self.lines[self.row].remove(at);
                    self.col -= 1;
                    self.dirty = true;
                } else if self.row > 0 {
                    self.push_undo();
                    // 前行の末尾に連結
                    let cur = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.lines[self.row].chars().count();
                    self.lines[self.row].push_str(&cur);
                    self.dirty = true;
                }
            }

            Keycode::Delete => {
                if self.col < self.lines[self.row].chars().count() {
                    self.push_undo();
                    let at = byte_at(&self.lines[self.row], self.col);
                    self.lines[self.row].remove(at);
                    self.dirty = true;
                } else if self.row + 1 < self.lines.len() {
                    self.push_undo();
                    let next = self.lines.remove(self.row + 1);
                    self.lines[self.row].push_str(&next);
                    self.dirty = true;
                }
            }

            Keycode::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.lines[self.row].chars().count();
                }
            }
            Keycode::Right => {
                if self.col < self.lines[self.row].chars().count() {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            Keycode::Up => {
                if self.row > 0 {
                    self.row -= 1;
                    self.col = self.col.min(self.lines[self.row].chars().count());
                }
            }
            Keycode::Down => {
                if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = self.col.min(self.lines[self.row].chars().count());
                }
            }

            Keycode::Home => self.col = 0,
            Keycode::End => self.col = self.lines[self.row].chars().count(),

            Keycode::PageUp => {
                let visible = self.visible_rows();
                self.row = self.row.saturating_sub(visible);
                self.col = self.col.min(self.lines[self.row].chars().count());
            }
            Keycode::PageDown => {
                let visible = self.visible_rows();
                self.row = (self.row + visible).min(self.lines.len() - 1);
                self.col = self.col.min(self.lines[self.row].chars().count());
            }

            // タブはスペース2個として扱う（インデント）
            Keycode::Tab => {
                self.push_undo();
                let line = &mut self.lines[self.row];
                let at = byte_at(line, self.col);
                line.insert(at, ' ');
                line.insert(at + 1, ' ');
                self.col += 2;
                self.dirty = true;
            }

            _ => {}
        }

        self.clamp_scroll();
        false
    }

    fn push_undo(&mut self) {
        if self.undo_stack.len() >= MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(EditorSnapshot {
            lines: self.lines.clone(),
            row: self.row,
            col: self.col,
            scroll: self.scroll,
            dirty: self.dirty,
        });
    }

    /// 表示可能な行数を計算。
    fn visible_rows(&self) -> usize {
        let inner = self.view_h as i32 - PAD_Y * 2;
        if inner <= 0 {
            return 1;
        }
        (inner / CELL_H) as usize
    }

    /// カーソルが表示範囲に入るようスクロール量を調整。
    fn clamp_scroll(&mut self) {
        let visible = self.visible_rows();
        if visible == 0 {
            return;
        }
        if self.row < self.scroll {
            self.scroll = self.row;
        }
        if self.row >= self.scroll + visible {
            self.scroll = self.row + 1 - visible;
        }
    }

    // ---- マウス ----------------------------------------------------------

    /// Click at screen (px, py): the caret goes to the nearest character boundary (the text is
    /// drawn with proportional Latin, so columns are found by measuring).
    pub fn click(&mut self, px: i32, py: i32, font: &FontSystem) {
        self.click_with(px, py, |t| font.measure_text(t));
    }

    pub fn click_with(&mut self, px: i32, py: i32, measure: impl Fn(&str) -> i32) {
        let (tx0, ty0) = self.text_origin.get();
        let visible = self.visible_rows();
        let n = ((py - ty0).div_euclid(CELL_H)).max(0) as usize;
        let row = (self.scroll + n.min(visible.saturating_sub(1))).min(self.lines.len() - 1);
        self.row = row;
        self.col = col_at(&self.lines[row], px - tx0, measure);
        self.clamp_scroll();
    }

    /// Wheel (positive = up): scroll by three lines; the caret is kept inside the window.
    pub fn wheel(&mut self, dy: i32) {
        let visible = self.visible_rows().max(1);
        let max = self.lines.len().saturating_sub(visible);
        self.scroll = if dy > 0 { self.scroll.saturating_sub(3) } else { (self.scroll + 3).min(max) };
        let row = self.row.clamp(self.scroll, self.scroll + visible - 1).min(self.lines.len() - 1);
        if row != self.row {
            self.row = row;
            self.col = self.col.min(self.lines[row].chars().count());
        }
    }

    // ---- 描画 ----------------------------------------------------------

    /// エディタを描画。枠 + テキスト + カーソル。
    pub fn draw(
        &self,
        c: &mut Canvas<Window>,
        font: &FontSystem,
        theme: &ThemeManager,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
    ) {
        let amber = theme.get_color("foreground");

        // 1) テキスト領域。外枠はアプリウィンドウ側が描画する。
        let tx0 = x + PAD_X;
        let ty0 = y + PAD_Y;
        self.text_origin.set((tx0, ty0));

        // 2) テキスト描画（スクロール考慮）
        let visible = ((h as i32 - PAD_Y * 2) / CELL_H).max(1) as usize;
        let max_col = ((w as i32 - PAD_X * 2) / CELL_W).max(1) as usize;

        for (i, line) in self
            .lines
            .iter()
            .skip(self.scroll)
            .take(visible)
            .enumerate()
        {
            let ly = ty0 + i as i32 * CELL_H;
            // 表示幅に収まる分だけ描画
            let slice: String = line.chars().take(max_col).collect();
            font.draw_text(c, tx0, ly, &slice, amber);
        }

        // 3) カーソル（Amber のブロック、fill はカーソルのみ許可）
        let cur_row = self.row;
        let cur_col = self.col;
        if cur_row >= self.scroll && cur_row < self.scroll + visible {
            let cy = ty0 + (cur_row - self.scroll) as i32 * CELL_H;
            // 行頭より前なら x=tx0、超過なら直前までの文字幅
            let prefix: String = self.lines[cur_row].chars().take(cur_col).collect();
            let cx = tx0 + font.measure_text(&prefix);
            // 反転カーソル風: 黒背景に Amber 枠の空枠、または Amber 1px 縦線
            // → CRT 風に「縦線」を採用
            c.set_draw_color(amber);
            let _ = c.fill_rect(Rect::new(cx, cy, CELL_W as u32, CELL_H as u32 - 2));
        }
    }

    // ---- テスト用アクセサ ----------------------------------------------
    #[doc(hidden)]
    pub fn lines_for_test(&self) -> &[String] {
        &self.lines
    }
    #[doc(hidden)]
    pub fn row_for_test(&self) -> usize {
        self.row
    }
    #[doc(hidden)]
    pub fn col_for_test(&self) -> usize {
        self.col
    }
    #[doc(hidden)]
    pub fn scroll_for_test(&self) -> usize {
        self.scroll
    }
}

// ---- App トレイト実装 ------------------------------------------------
use crate::app::{App, AppResult};

impl App for TextEditor {
    fn name(&self) -> &str {
        "TextEditor"
    }

    fn draw(
        &mut self,
        c: &mut Canvas<Window>,
        font: &FontSystem,
        theme: &ThemeManager,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
    ) {
        self.set_view(w, h);
        TextEditor::draw(self, c, font, theme, x, y, w, h);
    }

    fn input_key(&mut self, key: Keycode, modifiers: Mod) -> AppResult {
        // 既存ロジック（bool を返す）を AppResult に変換
        let quit = TextEditor::input_key(self, key, modifiers);
        if quit {
            AppResult::Quit
        } else {
            AppResult::Continue
        }
    }

    fn input_char(&mut self, ch: char) {
        TextEditor::input_char(self, ch);
    }
}

fn is_ctrl(keymod: Mod) -> bool {
    keymod & Mod::LCTRLMOD != Mod::NOMOD || keymod & Mod::RCTRLMOD != Mod::NOMOD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_restores_previous_text() {
        let mut editor = TextEditor::new();
        editor.input_char('a');
        editor.input_char('b');
        assert_eq!(editor.text_for_save(), "ab");
        editor.undo();
        assert_eq!(editor.text_for_save(), "a");
    }
}

/// Byte offset of the `col`-th character (columns count characters, not bytes).
fn byte_at(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map(|(i, _)| i).unwrap_or(s.len())
}

/// Column (character index) whose boundary is nearest to `rel_x` pixels from the line start.
pub fn col_at(line: &str, rel_x: i32, measure: impl Fn(&str) -> i32) -> usize {
    let mut prev = 0;
    let mut end = 0;
    for (n, (i, ch)) in line.char_indices().enumerate() {
        end = i + ch.len_utf8();
        let w = measure(&line[..end]);
        if rel_x < (prev + w) / 2 + ((prev + w) & 1) {
            return n;
        }
        prev = w;
    }
    line.chars().count()
}

#[cfg(test)]
mod mouse_tests {
    use super::*;

    fn m(t: &str) -> i32 {
        t.chars().map(|c| if c == 'i' { 3 } else { 7 }).sum()
    }

    #[test]
    fn column_from_x_uses_proportional_widths() {
        // widths: a=7 i=3 b=7 -> boundaries 0, 7, 10, 17
        assert_eq!(col_at("aib", -5, m), 0);
        assert_eq!(col_at("aib", 3, m), 0);
        assert_eq!(col_at("aib", 4, m), 1);
        assert_eq!(col_at("aib", 8, m), 1);
        assert_eq!(col_at("aib", 9, m), 2);
        assert_eq!(col_at("aib", 12, m), 2);
        assert_eq!(col_at("aib", 14, m), 3);
        assert_eq!(col_at("aib", 999, m), 3, "past the end = end of line");
        assert_eq!(col_at("", 10, m), 0);
    }

    #[test]
    fn click_places_the_caret_and_wheel_scrolls() {
        let mut e = TextEditor::from_text(&(0..30).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"));
        e.set_view(300, 13 * 5 + 12);
        e.text_origin.set((6, 6));
        (0..20).for_each(|_| e.wheel(1));
        assert_eq!(e.scroll_for_test(), 0);
        e.click_with(6 + 7 * 3 + 1, 6 + 2 * 13 + 3, |t| t.chars().count() as i32 * 7);
        assert_eq!((e.row_for_test(), e.col_for_test()), (2, 3));
        e.wheel(-1);
        assert_eq!(e.scroll_for_test(), 3);
        assert_eq!(e.row_for_test(), 3, "the caret follows into the window");
        e.wheel(1);
        assert_eq!(e.scroll_for_test(), 0);
        e.click_with(6, 6 + 400, |t| t.chars().count() as i32 * 7);
        assert_eq!(e.row_for_test(), 4, "below the last visible row = the last visible row");
    }
}
