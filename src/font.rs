use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

/// A Unicode bitmap font with sparse character support
pub struct UnicodeFont {
    pub width: u8,
    pub height: u8,
    // First character code in the range
    first_char: u32,
    // Number of character codes in the range
    char_count: u32,
    // Index array: maps character code to bitmap data offset (or 0xFFFFFFFF if not found)
    char_index: &'static [u32],
    // Bitmap data: all character bitmaps concatenated
    bitmap_data: &'static [u16],
}

impl UnicodeFont {
    /// Create a new Unicode font from indexed data.
    pub const fn new(
        width: u8,
        height: u8,
        first_char: u32,
        char_count: u32,
        char_index: &'static [u32],
        bitmap_data: &'static [u16],
    ) -> Self {
        UnicodeFont {
            width,
            height,
            first_char,
            char_count,
            char_index,
            bitmap_data,
        }
    }

    /// Get the bitmap data for a character, or None if the character is not in the font.
    pub fn glyph_data(&self, ch: char) -> Option<&'static [u16]> {
        let code = ch as u32;
        if code < self.first_char || code >= self.first_char + self.char_count {
            return None;
        }

        let index = (code - self.first_char) as usize;
        if index >= self.char_index.len() {
            return None;
        }

        let data_offset = self.char_index[index] as usize;
        if data_offset == 0xFFFFFFFF {
            return None; // Character not in font
        }

        // Calculate the end offset (next character or end of data)
        let mut next_offset = data_offset;
        for i in (index + 1)..self.char_index.len() {
            let next_idx = self.char_index[i] as usize;
            if next_idx != 0xFFFFFFFF {
                next_offset = next_idx;
                break;
            }
        }
        if next_offset == data_offset {
            next_offset = self.bitmap_data.len();
        }

        Some(&self.bitmap_data[data_offset..next_offset])
    }

    /// Measure the width of a string in pixels.
    pub fn measure_text(&self, text: &str) -> i32 {
        let mut width = 0;
        for ch in text.chars() {
            if self.glyph_data(ch).is_some() {
                width += self.width as i32;
            } else {
                // For missing characters, use the font's width
                width += self.width as i32;
            }
        }
        width
    }

    /// Draw text at the given position using the canvas.
    pub fn draw_text(&self, c: &mut Canvas<Window>, x: i32, y: i32, text: &str, color: Color) {
        let mut cursor_x = x;
        for ch in text.chars() {
            if let Some(data) = self.glyph_data(ch) {
                self.draw_glyph(c, cursor_x, y, data, color);
                cursor_x += self.width as i32;
            } else {
                // For missing characters, draw a placeholder box
                self.draw_missing(c, cursor_x, y, color);
                cursor_x += self.width as i32;
            }
        }
    }

    fn draw_glyph(&self, c: &mut Canvas<Window>, x: i32, y: i32, data: &[u16], color: Color) {
        // M+ font data is stored as u16 but contains 8-bit bitmap data
        // Each row is one u16 value (but only lower 8 bits are used for 6px wide fonts)
        for (row, word) in data.iter().enumerate() {
            let byte = (*word & 0xFF) as u8; // Extract lower 8 bits

            // For 6px wide fonts, bits 7-2 represent the 6 pixels (MSB first)
            for col in 0..self.width {
                let bit = (byte >> (7 - col)) & 1;
                if bit == 1 {
                    c.set_draw_color(color);
                    let _ = c.fill_rect(Rect::new(x + col as i32, y + row as i32, 1, 1));
                }
            }
        }
    }

    fn draw_missing(&self, c: &mut Canvas<Window>, x: i32, y: i32, color: Color) {
        // Draw a small question mark or box for missing characters
        let w = self.width as i32;
        let h = self.height as i32;

        // Draw a box outline
        c.set_draw_color(color);
        let _ = c.fill_rect(Rect::new(x, y, w as u32, 1));
        let _ = c.fill_rect(Rect::new(x, y + h - 1, w as u32, 1));
        let _ = c.fill_rect(Rect::new(x, y, 1, h as u32));
        let _ = c.fill_rect(Rect::new(x + w - 1, y, 1, h as u32));
    }
}

/// Distance between the inks of neighbouring glyphs on one row (3 = two blank pixels).
const SAME_ROW_GAP: i32 = 3;

/// Width of a space in the proportional Latin face (the blank pixel after the previous glyph comes on top).
const SPACE_W: i32 = 3;

/// First and last inked column of a glyph.
fn ink_cols(g: &crate::bdf::BdfGlyph) -> (i32, i32) {
    let mut min = i32::MAX;
    let mut max = i32::MIN;
    for row in &g.rows {
        for col in 0..g.width as i32 {
            if row & (0x8000 >> col) != 0 {
                min = min.min(col);
                max = max.max(col);
            }
        }
    }
    if min > max { (0, 0) } else { (min, max) }
}

/// Per row: (leftmost, rightmost) inked column, relative to the glyph's own ink start.
fn row_edges(g: &crate::bdf::BdfGlyph) -> Vec<Option<(i32, i32)>> {
    let (base, _) = ink_cols(g);
    g.rows
        .iter()
        .map(|row| {
            let cols: Vec<i32> = (0..g.width as i32).filter(|col| row & (0x8000 >> col) != 0).collect();
            cols.first().zip(cols.last()).map(|(a, b)| (a - base, b - base))
        })
        .collect()
}

/// Distance from the ink start of `a` to the ink start of `b`: the tightest that leaves one blank
/// pixel between them on the same row and lets them touch only diagonally between rows.
fn pair_advance(a: &crate::bdf::BdfGlyph, b: &crate::bdf::BdfGlyph) -> i32 {
    let (ea, eb) = (row_edges(a), row_edges(b));
    let mut d = 1;
    for (r, ra) in ea.iter().enumerate() {
        let Some((_, right)) = ra else { continue };
        for dr in -1i32..=1 {
            let rb = r as i32 + dr;
            if rb < 0 || rb as usize >= eb.len() {
                continue;
            }
            if let Some((left, _)) = eb[rb as usize] {
                let gap = if dr == 0 { SAME_ROW_GAP } else { SAME_ROW_GAP - 1 };
                d = d.max(right + gap - left);
            }
        }
    }
    d
}

thread_local! {
    static BOLD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub struct FontSystem {
    pub font: Option<UnicodeFont>,
    /// 日本語 BDF（JIS X 0208）。Latin-1 にない文字のフォールバック。
    pub jis_font: Option<crate::bdf::BdfFont>,
    /// 日本語フォントのピクセル幅（M+ J12R = 12）。
    pub jis_width: u32,
    /// 日本語フォントのピクセル高（M+ J12R = 13）。
    pub jis_height: u32,
    /// Display face for titles and the menu bar (Cavs.fnt, a Chicago-like mini font).
    pub display: Option<crate::pdfont::PlaydateFont>,
    /// Use `display` for titles; otherwise they use M+ F12 Regular.
    pub display_cavs: bool,
    /// Optional bitmap face for the terminal (none is shipped; the terminal then uses the body face, Shinonome / M+ F12).
    pub terminal: Option<crate::pdfont::PlaydateFont>,
    /// Latin face for body text (Shinonome 6x12); M+ F12 covers what it lacks.
    pub body_latin: Option<crate::bdf::BdfFont>,
    /// Rows the Japanese glyphs are drawn below the text origin (aligns the Shinonome baseline).
    pub jis_dy: i32,
    /// Rows the Latin glyphs are drawn below the text origin.
    pub latin_dy: i32,
}

impl FontSystem {
    pub fn new() -> Self {
        FontSystem {
            font: None,
            jis_font: None,
            jis_width: 12,
            jis_height: 13,
            display: None,
            display_cavs: true,
            terminal: None,
            body_latin: None,
            jis_dy: 0,
            latin_dy: 0,
        }
    }

    /// Titles, menu-bar items and big labels: Cavs when enabled and loaded,
    /// otherwise plain M+ F12 Regular.
    ///
    /// The display face has no Japanese: a text with a character it lacks (a Japanese file name in
    /// a dialog) is drawn whole in the body face instead of leaving gaps.
    pub fn draw_display_text(&self, c: &mut Canvas<Window>, x: i32, y: i32, text: &str, color: Color) {
        match self.display_face(text) {
            Some(f) => f.draw(c, x, y + 2, text, color),
            None => self.draw_text(c, x, y, text, color),
        }
    }

    pub fn measure_display_text(&self, text: &str) -> i32 {
        match self.display_face(text) {
            Some(f) => f.measure(text),
            None => self.measure_text(text),
        }
    }

    /// True when `draw_display_text` draws `text` in the display face (not the text font).
    pub fn has_display_face(&self, text: &str) -> bool {
        self.display_face(text).is_some()
    }

    /// The display face, when it is on and has every character of `text`.
    fn display_face(&self, text: &str) -> Option<&crate::pdfont::PlaydateFont> {
        match (&self.display, self.display_cavs) {
            (Some(f), true) if text.chars().all(|ch| ch == ' ' || ch.is_control() || f.has(ch)) => Some(f),
            _ => None,
        }
    }

    pub fn load_font(&mut self, font: UnicodeFont) {
        self.font = Some(font);
    }

    /// 日本語 BDF をロード。パスは assets/fonts/ 以下の BDF。
    pub fn load_jis_font(&mut self, path: &str) -> Result<(), String> {
        let bdf = crate::bdf::BdfFont::load(path)?;
        println!("[font] loaded JIS BDF: {} ({} glyphs)", path, bdf.len());
        self.jis_font = Some(bdf);
        Ok(())
    }

    /// 1文字の表示幅を返す。日本語（JIS X 0208 対象）は jis_width、
    /// それ以外は Latin-1 フォント幅。
    pub fn char_width(&self, ch: char) -> i32 {
        if let Some(g) = self.body_latin.as_ref().filter(|_| ch.is_ascii()).and_then(|l| l.glyph_unicode(ch as u32)) {
            return g.width as i32;
        }
        if crate::jis::unicode_to_jis(ch).is_some() && self.jis_font.is_some() {
            self.jis_width as i32
        } else {
            self.font.as_ref().map(|f| f.width as i32).unwrap_or(6)
        }
    }

    pub fn draw_text(&self, c: &mut Canvas<Window>, x: i32, y: i32, text: &str, color: Color) {
        let cursor_x = x;
        // ASCII from Shinonome 6x12 with proportional spacing and kerning, everything else
        // through the Japanese / M+ fallback.
        if self.body_latin.is_some() {
            self.layout(Some(c), x, y, text, color);
            return;
        }
        self.draw_text_plain(c, cursor_x, y, text, color);
    }

    /// Lays out `text` (drawing it when `c` is given) and returns its width. ASCII uses the
    /// Latin face proportionally: every glyph is trimmed to its ink and the gap to the next
    /// glyph is the tightest that keeps one blank pixel between inks (so `To`, `AV`, `r.`
    /// tuck together while `ll` and `mm` stay apart).
    fn layout(&self, mut c: Option<&mut Canvas<Window>>, x: i32, y: i32, text: &str, color: Color) -> i32 {
        let lat = self.body_latin.as_ref().expect("latin face");
        let chars: Vec<char> = text.chars().collect();
        let glyph_of = |ch: char| if ch.is_ascii_graphic() { lat.glyph_unicode(ch as u32) } else { None };
        let mut cx = x;
        for (i, &ch) in chars.iter().enumerate() {
            if ch == ' ' && lat.glyph_unicode(32).is_some() {
                cx += SPACE_W;
                continue;
            }
            if let Some(g) = glyph_of(ch) {
                let (min, max) = ink_cols(g);
                if let Some(c) = c.as_deref_mut() {
                    self.draw_jis_glyph(c, cx - min, y + self.latin_dy, g, color);
                }
                cx += match chars.get(i + 1).and_then(|&n| glyph_of(n)) {
                    Some(next) => pair_advance(g, next),
                    None => max - min + SAME_ROW_GAP,
                };
                continue;
            }
            let w = match c.as_deref_mut() {
                Some(c) => self.draw_text_plain(c, cx, y, &ch.to_string(), color),
                None => self.plain_width(ch),
            };
            cx += w;
        }
        cx - x
    }

    fn plain_width(&self, ch: char) -> i32 {
        if crate::jis::unicode_to_jis(ch).is_some() && self.jis_font.is_some() {
            self.jis_width as i32
        } else {
            self.font.as_ref().map(|f| f.width as i32).unwrap_or(6)
        }
    }

    /// Japanese glyphs (Shinonome) and the M+ Latin-1 fallback; returns the width drawn.
    fn draw_text_plain(&self, c: &mut Canvas<Window>, x: i32, y: i32, text: &str, color: Color) -> i32 {
        let mut cursor_x = x;
        for ch in text.chars() {
            // まず日本語フォントで描けるか試す
            if let Some(jis_code) = crate::jis::unicode_to_jis(ch) {
                if let Some(ref jf) = self.jis_font {
                    if let Some(glyph) = jf.glyph_jis(jis_code) {
                        self.draw_jis_glyph(c, cursor_x, y + self.jis_dy, glyph, color);
                        cursor_x += glyph.width as i32;
                        continue;
                    }
                }
            }
            // フォールバック: Latin-1 フォント
            if let Some(ref font) = self.font {
                font.draw_text(c, cursor_x, y, &ch.to_string(), color);
                cursor_x += font.width as i32;
            } else {
                cursor_x += 6;
            }
        }
        cursor_x - x
    }

    /// JIS グリフを描画。各行のビットマップ（MSB first）を Ambe ドットで。
    fn draw_jis_glyph(
        &self,
        c: &mut Canvas<Window>,
        x: i32,
        y: i32,
        glyph: &crate::bdf::BdfGlyph,
        color: Color,
    ) {
        c.set_draw_color(color);
        let w = glyph.width;
        for (row, word) in glyph.rows.iter().enumerate() {
            for col in 0..w {
                // 各行は u16。MSB(15) から描画。幅12なら bit15..bit4 を使う。
                let bit = (word >> (15 - col)) & 1;
                if bit == 1 {
                    let _ = c.fill_rect(Rect::new(x + col as i32, y + row as i32, 1, 1));
                    // Bold: smear one pixel to the right, but never close a 1 px gap
                    // (the inside of w, m, ...), which would turn them into blobs.
                    if BOLD.with(|b| b.get()) && col + 1 < 16 {
                        let n1 = (word >> (14 - col)) & 1;
                        let n2 = if col + 2 < 16 { (word >> (13 - col)) & 1 } else { 0 };
                        if n1 == 0 && n2 == 0 {
                            let _ = c.fill_rect(Rect::new(x + col as i32 + 1, y + row as i32, 1, 1));
                        }
                    }
                }
            }
        }
    }

    pub fn draw_text_bold(&self, c: &mut Canvas<Window>, x: i32, y: i32, text: &str, color: Color) {
        // Bold: glyphs smear right by one pixel (see `draw_jis_glyph`), keeping 1 px gaps open.
        BOLD.with(|b| b.set(true));
        self.draw_text(c, x, y, text, color);
        BOLD.with(|b| b.set(false));
    }

    pub fn measure_text(&self, text: &str) -> i32 {
        if self.body_latin.is_some() {
            return self.layout(None, 0, 0, text, Color::RGB(0, 0, 0));
        }
        let mut width = 0;
        for ch in text.chars() {
            width += self.char_width(ch);
        }
        width
    }

    /// Compact bitmap system face.
    ///
    /// It is deliberately *not* Apple's proprietary Chicago font. ASCII
    /// glyphs are drawn from HarbourOS's existing M+ bitmap, with a heavier
    /// 1px stroke and proportional spacing; other scripts use the regular
    /// M+/JIS fallback. This preserves pixel crispness and licensing.
    pub fn measure_system_text(&self, text: &str) -> i32 {
        text.chars().map(|ch| self.system_char_advance(ch)).sum()
    }

    fn system_char_advance(&self, ch: char) -> i32 {
        if ch == ' ' {
            return 4;
        }
        if let Some(ref font) = self.font {
            if let Some((min, max)) = system_ascii_bounds(font, ch) {
                return (max - min + 1) as i32 + 2;
            }
        }
        self.char_width(ch)
    }

    pub fn draw_system_text(
        &self,
        c: &mut Canvas<Window>,
        x: i32,
        y: i32,
        text: &str,
        color: Color,
    ) {
        let mut cursor_x = x;
        for ch in text.chars() {
            let advance = self.system_char_advance(ch);
            if ch == ' ' {
                cursor_x += advance;
                continue;
            }
            if let Some(ref font) = self.font {
                if let (Some(data), Some((min, max))) =
                    (font.glyph_data(ch), system_ascii_bounds(font, ch))
                {
                    c.set_draw_color(color);
                    for (row, bits) in data.iter().enumerate().take(font.height as usize) {
                        let bits = (*bits & 0xff) as u8;
                        for col in min..=max {
                            if bits & (1 << (7 - col)) != 0 {
                                // Thick Chicago-like strokes, no antialiasing.
                                let px = cursor_x + (col - min) as i32;
                                let py = y + row as i32;
                                let _ = c.draw_point((px, py));
                                // Letters built from alternating columns (m, w, W) fill in
                                // solid when smeared sideways, so they keep a 1px stroke.
                                if !matches!(ch, 'm' | 'w' | 'W') {
                                    let _ = c.draw_point((px + 1, py));
                                }
                            }
                        }
                    }
                    cursor_x += advance;
                    continue;
                }
            }
            self.draw_text(c, cursor_x, y, &ch.to_string(), color);
            cursor_x += advance;
        }
    }
}

/// Visible bounds of an ASCII M+ glyph; used for proportional spacing.
/// The system face is a rendering style, not a bundled commercial font.
fn system_ascii_bounds(font: &UnicodeFont, ch: char) -> Option<(u8, u8)> {
    if !('!'..='~').contains(&ch) {
        return None;
    }
    let data = font.glyph_data(ch)?;
    let mut min = font.width;
    let mut max = 0;
    for word in data.iter().take(font.height as usize) {
        let bits = (*word & 0xff) as u8;
        for col in 0..font.width {
            if bits & (1 << (7 - col)) != 0 {
                min = min.min(col);
                max = max.max(col);
            }
        }
    }
    if min == font.width {
        None
    } else {
        Some((min, max))
    }
}

#[cfg(test)]
mod system_font_tests {
    use super::*;

    #[test]
    fn compact_system_font_is_proportional_and_pixel_sized() {
        let mut fonts = FontSystem::new();
        fonts.load_font(load_mplus_f12r());
        assert!(fonts.measure_system_text("MMMM") > fonts.measure_system_text("IIII"));
        assert!(fonts.measure_system_text("Harbour") > 0);
        assert_eq!(fonts.measure_system_text(" "), 4);
        assert_eq!(fonts.measure_text("あ"), fonts.measure_system_text("あ"));
    }
}

// Include the M+ font
mod mplus_f12r {
    include!("generated/mplus_f12r.rs");
}

pub fn load_mplus_f12r() -> UnicodeFont {
    mplus_f12r::MPLUS_12X13_FONT
}

/// The face for titles, the menu bar, buttons and tile names: ChiKareGo2 (Giles Booth, CC BY; a
/// Chicago-like bitmap face) when `assets/fonts/ChiKareGo2.fnt` is there, otherwise Cavs.
/// Needs SDL_image's PNG support to be initialised.
pub fn load_display_font() -> Option<crate::pdfont::PlaydateFont> {
    use crate::pdfont::PlaydateFont;
    for (path, space) in [("assets/fonts/ChiKareGo2.fnt", 5), ("assets/fonts/Cavs.fnt", 2)] {
        match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| PlaydateFont::parse(&b)) {
            Ok(mut f) => {
                f.set_space(space);
                return Some(f);
            }
            Err(e) => eprintln!("[font] {path}: {e}"),
        }
    }
    None
}
