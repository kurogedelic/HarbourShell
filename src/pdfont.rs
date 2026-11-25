//! Playdate-format bitmap fonts (`.fnt`), e.g. Cavs.fnt.
//!
//! The file is a text header (`--metrics=...`, `width=`, `height=`, `tracking=`),
//! the glyph sheet as a base64 PNG (`data=`), then one `<char>\t<width>` line per
//! glyph in sheet order (a `space` line gives the space), and kerning pairs, which
//! are ignored here. Glyph cells are laid out left to right, top to bottom.
//! Advance = width field + tracking.

use std::collections::HashMap;

use crate::gfx::Color;
use crate::gfx::Point;
use crate::gfx::Canvas;

pub struct PlaydateFont {
    pub cell_w: i32,
    pub cell_h: i32,
    cols: usize,
    atlas_w: usize,
    /// Lit pixels of the glyph sheet (dark, opaque pixels).
    lit: Vec<bool>,
    /// char -> (cell index, advance in pixels)
    glyphs: HashMap<char, (usize, i32)>,
    /// Kerning: extra advance (usually negative) between two consecutive chars.
    kerning: HashMap<(char, char), i32>,
    space: i32,
    /// Where the ink of a typical capital (E) starts in its cell; words are aligned to it.
    usual_left: i32,
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let val = |c: u8| -> Result<u32, String> {
        Ok(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(format!("bad base64 byte {c}")),
        })
    };
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace() && *b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, b) in chunk.iter().enumerate() {
            n |= val(*b)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

impl PlaydateFont {
    /// Requires SDL_image to be initialised for PNG (done at startup).
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = String::from_utf8_lossy(bytes);
        let num = |key: &str| -> Option<i32> {
            text.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().parse().ok())
        };
        let (cell_w, cell_h) = (num("width=").ok_or("no width=")?, num("height=").ok_or("no height=")?);
        let tracking = num("tracking=").unwrap_or(0);
        let data_at = text.find("\ndata=").ok_or("no data=")? + 1;
        let data_end = text[data_at..].find('\n').map(|i| data_at + i).ok_or("data= is not terminated")?;
        let png = base64_decode(&text[data_at + 5..data_end])?;
        let (px, aw, ah) = crate::imgload::decode_rgba(&png)?;
        let mut lit = vec![false; aw * ah];
        for y in 0..ah {
            for x in 0..aw {
                let p = &px[(y * aw + x) * 4..(y * aw + x) * 4 + 4];
                lit[y * aw + x] = p[3] > 127 && (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3 < 128;
            }
        }
        let cols = aw / cell_w as usize;
        // Glyph lines follow the data line, in sheet order.
        let mut glyphs = HashMap::new();
        let mut kerning = HashMap::new();
        let mut space = cell_w;
        let mut cell = 0usize;
        for line in text[data_end + 1..].lines() {
            let Some((key, val)) = line.split_once('\t') else { continue };
            let Ok(w) = val.trim().parse::<i32>() else { continue };
            if key == "space" {
                space = w;
                glyphs.insert(' ', (cell, w));
                cell += 1;
            } else if key.chars().count() == 1 {
                glyphs.entry(key.chars().next().unwrap()).or_insert((cell, w + tracking));
                cell += 1;
            } else if key.chars().count() == 2 {
                let mut it = key.chars();
                kerning.insert((it.next().unwrap(), it.next().unwrap()), w);
            }
        }
        let mut f = Self { cell_w, cell_h, cols, atlas_w: aw, lit, glyphs, kerning, space, usual_left: 0 };
        f.usual_left = f.glyph_pixels('E').iter().map(|p| p.0).min().unwrap_or(0);
        Ok(f)
    }

    /// Override the width of the space (Cavs' file gives a full 11 px cell, which
    /// reads as a hole; its glyph side bearings already separate words).
    pub fn set_space(&mut self, px: i32) {
        self.space = px;
        if let Some(g) = self.glyphs.get_mut(&' ') {
            g.1 = px;
        }
    }

    /// Advance width of one glyph (no kerning, no lead adjustment).
    pub fn advance_of(&self, ch: char) -> i32 {
        self.advance(ch)
    }

    fn advance(&self, ch: char) -> i32 {
        if ch == ' ' {
            return self.space;
        }
        self.glyphs.get(&ch).map(|g| g.1).unwrap_or(self.cell_w)
    }

    /// Advance from `a` to the next glyph `b` (kerning applied).
    fn step(&self, a: char, b: Option<char>) -> i32 {
        if let Some(k) = b.and_then(|b| self.kerning.get(&(a, b)).copied()) {
            return self.advance(a) + k;
        }
        // Before a space, trim the glyph's own right-hand padding so a word gap
        // is the space width plus one pixel, not a whole empty cell.
        if b == Some(' ') && a != ' ' {
            if let Some(ink) = self.glyph_pixels(a).into_iter().map(|(x, _)| x).max() {
                return self.advance(a).min(ink + 2);
            }
        }
        self.advance(a)
    }

    pub fn measure(&self, text: &str) -> i32 {
        let chars: Vec<char> = text.chars().collect();
        (0..chars.len()).map(|i| self.step(chars[i], chars.get(i + 1).copied())).sum::<i32>() + self.lead_adjust(text)
    }

    pub fn has(&self, ch: char) -> bool {
        self.glyphs.contains_key(&ch)
    }

    /// Lit pixel offsets (x, y) inside one glyph cell.
    pub fn glyph_pixels(&self, ch: char) -> Vec<(i32, i32)> {
        let Some(&(cell, _)) = self.glyphs.get(&ch) else { return Vec::new() };
        if ch == ' ' {
            return Vec::new();
        }
        let (cx, cy) = ((cell % self.cols) as i32 * self.cell_w, (cell / self.cols) as i32 * self.cell_h);
        let mut out = Vec::new();
        for y in 0..self.cell_h {
            for x in 0..self.cell_w {
                if self.lit[(cy + y) as usize * self.atlas_w + (cx + x) as usize] {
                    out.push((x, y));
                }
            }
        }
        out
    }

    /// Draw one glyph centred in a fixed `pitch` (for terminals / monospace grids).
    pub fn draw_char_centered(&self, c: &mut Canvas, x: i32, y: i32, ch: char, pitch: i32, color: Color) {
        let px = self.glyph_pixels(ch);
        if px.is_empty() {
            return;
        }
        let (min, max) = px.iter().fold((i32::MAX, i32::MIN), |(a, b), &(gx, _)| (a.min(gx), b.max(gx)));
        let off = x + (pitch - (max - min + 1)) / 2 - min;
        let pts: Vec<Point> = px.iter().map(|&(gx, gy)| Point::new(off + gx, y + gy)).collect();
        c.set_draw_color(color);
        let _ = c.draw_points(&pts[..]);
    }

    /// Draw `text` with the top of the glyph cell at `y`.
    /// Wide capitals (M, W: ink from x=0; N from x=1) start further left in their cell than
    /// the usual letters (ink from x=2) and narrow ones (I: x=4) further right, so such words
    /// stuck out or sank in. The whole word is moved by the difference so every word starts
    /// at the same x.
    fn lead_adjust(&self, text: &str) -> i32 {
        let usual = self.usual_left;
        let Some(first) = text.chars().find(|c| *c != ' ') else { return 0 };
        match self.glyph_pixels(first).iter().map(|p| p.0).min() {
            Some(min) => usual - min,
            None => 0,
        }
    }

    /// Width of `text` drawn with [`draw_scaled`] at `scale`.
    pub fn measure_scaled(&self, text: &str, scale: i32) -> i32 {
        self.measure(text) * scale
    }

    /// Like `draw`, but every pixel becomes a `scale` x `scale` block (big numerals).
    pub fn draw_scaled(&self, c: &mut Canvas, x: i32, y: i32, text: &str, color: Color, scale: i32) {
        let mut rects: Vec<crate::gfx::Rect> = Vec::new();
        let mut cx = x + self.lead_adjust(text) * scale;
        let chars: Vec<char> = text.chars().collect();
        for (i, &ch) in chars.iter().enumerate() {
            for (gx, gy) in self.glyph_pixels(ch) {
                rects.push(crate::gfx::Rect::new(cx + gx * scale, y + gy * scale, scale as u32, scale as u32));
            }
            cx += self.step(ch, chars.get(i + 1).copied()) * scale;
        }
        c.set_draw_color(color);
        let _ = c.fill_rects(&rects);
    }

    pub fn draw(&self, c: &mut Canvas, x: i32, y: i32, text: &str, color: Color) {
        let mut pts: Vec<Point> = Vec::new();
        let mut cx = x + self.lead_adjust(text);
        let chars: Vec<char> = text.chars().collect();
        for (i, &ch) in chars.iter().enumerate() {
            for (gx, gy) in self.glyph_pixels(ch) {
                pts.push(Point::new(cx + gx, y + gy));
            }
            cx += self.step(ch, chars.get(i + 1).copied());
        }
        c.set_draw_color(color);
        let _ = c.draw_points(&pts[..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(name: &str) -> PlaydateFont {
        let path = format!("{}/assets/fonts/{name}", env!("CARGO_MANIFEST_DIR"));
        PlaydateFont::parse(&std::fs::read(path).unwrap()).unwrap()
    }

    fn cavs() -> PlaydateFont {
        let mut f = load("Cavs.fnt");
        f.set_space(2);
        f
    }

    #[test]
    fn base64() {
        assert_eq!(base64_decode("TWFu").unwrap(), b"Man");
        assert_eq!(base64_decode("TWE=").unwrap(), b"Ma");
        assert_eq!(base64_decode("TQ==").unwrap(), b"M");
        assert!(base64_decode("T!==").is_err());
    }

    #[test]
    fn cavs_loads_with_its_metrics() {
        let f = cavs();
        assert_eq!((f.cell_w, f.cell_h), (11, 12));
        assert_eq!(f.cols, 11);
        for ch in "0123456789ABCXYZabcxyz !?.,:-+/".chars() {
            assert!(f.has(ch), "missing {ch:?}");
        }
    }

    #[test]
    fn kerning_tightens_the_spacing_the_way_the_font_file_says() {
        let f = cavs();
        // Without kerning every glyph advances 11 px; the file's pairs pull them together.
        assert_eq!(f.advance('l'), 11);
        assert_eq!(f.measure("ll") - f.lead_adjust("ll"), 11 + f.kerning[&('l', 'l')] + 11, "ll = -8 in the file (the lead shift aside)");
        assert_eq!(f.kerning[&('l', 'l')], -8);
        assert_eq!(f.kerning[&('T', 'e')], -6);
        assert!(f.measure("Settings") < 88, "kerned text is narrower than the monospace grid: {}", f.measure("Settings"));
        assert!(f.measure("lll") < f.measure("MMM"), "narrow letters take less room");
        assert_eq!(f.measure(""), 0);
        assert_eq!(f.advance(' '), 2, "a narrow space, not a full cell");
        assert!(f.measure("IR Remote") < f.measure("IR") + f.measure("Remote") + 11);
        assert_eq!(f.measure("A"), 11, "a single glyph keeps its full advance");
    }

    #[test]
    fn words_starting_with_wide_capitals_line_up_with_the_others() {
        let f = cavs();
        // M and W are drawn from x=0 in their cell, N from x=1, the usual letters from x=2.
        assert_eq!(f.lead_adjust("Music"), 2);
        assert_eq!(f.lead_adjust("Monitor"), 2);
        assert_eq!(f.lead_adjust("Network"), 1);
        assert_eq!(f.lead_adjust("Settings"), 0);
        assert_eq!(f.lead_adjust("Images"), -2, "narrow I starts at x=4");
        assert_eq!(f.lead_adjust(" Wi-Fi"), 2, "leading spaces are skipped");
        assert_eq!(f.lead_adjust(""), 0);
    }

    #[test]
    fn glyphs_have_pixels_and_land_in_the_right_cells() {
        let f = cavs();
        let zero = f.glyph_pixels('0');
        assert!(zero.len() > 20, "a zero has many lit pixels, got {}", zero.len());
        assert!(zero.iter().all(|&(x, y)| (0..11).contains(&x) && (0..12).contains(&y)));
        // 'i' is a narrow stem; 'M' is wide; they differ.
        let (i, m) = (f.glyph_pixels('i'), f.glyph_pixels('M'));
        assert!(m.len() > i.len());
        assert!(f.glyph_pixels(' ').is_empty());
        // 'A' and 'B' are different shapes.
        assert_ne!(f.glyph_pixels('A'), f.glyph_pixels('B'));
    }
}


