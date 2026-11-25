//! A small QR code encoder: byte mode, error correction level L, versions 1 to 5 (up to 106 bytes),
//! one Reed-Solomon block. Pure Rust, no dependencies. Enough for a short URL.
//!
//! [`encode`] returns the module matrix; [`draw`] paints it 1-bit black on white with the quiet zone.

use crate::gfx::Color;
use crate::gfx::Rect;
use crate::gfx::Canvas;

/// (data codewords, error correction codewords) at level L, versions 1..=5 (a single block each).
const CAPACITY: [(usize, usize); 5] = [(19, 7), (34, 10), (55, 15), (80, 20), (108, 26)];
/// Alignment pattern centre (one) for versions 2..=5; version 1 has none.
const ALIGN: [usize; 5] = [0, 18, 22, 26, 30];

/// The error correction level L indicator in the format information.
const ECL_L: u32 = 0b01;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Qr {
    pub version: usize,
    pub size: usize,
    pub mask: u8,
    /// Row-major, `true` = dark.
    pub modules: Vec<bool>,
}

impl Qr {
    pub fn get(&self, x: usize, y: usize) -> bool {
        self.modules[y * self.size + x]
    }
}

// ---------------------------------------------------------------- Reed-Solomon over GF(256)

fn gf_tables() -> ([u8; 256], [u8; 256]) {
    let (mut exp, mut log) = ([0u8; 256], [0u8; 256]);
    let mut x: u16 = 1;
    for i in 0..255 {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= 0x11d;
        }
    }
    exp[255] = exp[0];
    (exp, log)
}

fn gf_mul(a: u8, b: u8, t: &([u8; 256], [u8; 256])) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        t.0[(t.1[a as usize] as usize + t.1[b as usize] as usize) % 255]
    }
}

/// Generator polynomial of degree `n` (coefficients without the leading 1, highest power first).
fn generator(n: usize, t: &([u8; 256], [u8; 256])) -> Vec<u8> {
    let mut g = vec![1u8];
    for i in 0..n {
        let mut next = vec![0u8; g.len() + 1];
        for (j, &c) in g.iter().enumerate() {
            next[j] ^= c;
            next[j + 1] ^= gf_mul(c, t.0[i], t);
        }
        g = next;
    }
    g[1..].to_vec()
}

/// The `n` error correction codewords of `data`.
pub fn rs_remainder(data: &[u8], n: usize) -> Vec<u8> {
    let t = gf_tables();
    let g = generator(n, &t);
    let mut rem = vec![0u8; n];
    for &b in data {
        let f = b ^ rem[0];
        rem.remove(0);
        rem.push(0);
        for (r, &gc) in rem.iter_mut().zip(&g) {
            *r ^= gf_mul(gc, f, &t);
        }
    }
    rem
}

// ---------------------------------------------------------------- bit stream

fn data_codewords(bytes: &[u8], version: usize) -> Vec<u8> {
    let cap = CAPACITY[version - 1].0;
    let mut bits: Vec<bool> = vec![];
    let push = |v: u32, n: usize, bits: &mut Vec<bool>| {
        for i in (0..n).rev() {
            bits.push(v >> i & 1 == 1);
        }
    };
    push(0b0100, 4, &mut bits); // byte mode
    push(bytes.len() as u32, 8, &mut bits); // count (8 bits for versions 1..=9)
    for &b in bytes {
        push(b as u32, 8, &mut bits);
    }
    let limit = cap * 8;
    for _ in 0..4.min(limit - bits.len()) {
        bits.push(false); // terminator
    }
    while bits.len() % 8 != 0 {
        bits.push(false);
    }
    let mut out: Vec<u8> = bits.chunks(8).map(|c| c.iter().fold(0u8, |a, &b| a << 1 | b as u8)).collect();
    let mut pad = [0xECu8, 0x11].into_iter().cycle();
    while out.len() < cap {
        out.push(pad.next().unwrap());
    }
    out
}

// ---------------------------------------------------------------- matrix

struct Grid {
    size: usize,
    dark: Vec<bool>,
    /// A function module (finder, timing, alignment, format, dark module): never data, never masked.
    func: Vec<bool>,
}

impl Grid {
    fn new(size: usize) -> Self {
        Self { size, dark: vec![false; size * size], func: vec![false; size * size] }
    }
    fn set(&mut self, x: usize, y: usize, d: bool) {
        self.dark[y * self.size + x] = d;
        self.func[y * self.size + x] = true;
    }
}

fn draw_function_patterns(g: &mut Grid, version: usize) {
    let n = g.size;
    // Finder patterns with their separators.
    for &(cx, cy) in &[(3usize, 3usize), (n - 4, 3), (3, n - 4)] {
        for dy in -4i32..=4 {
            for dx in -4i32..=4 {
                let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                if x < 0 || y < 0 || x >= n as i32 || y >= n as i32 {
                    continue;
                }
                let d = dx.abs().max(dy.abs());
                g.set(x as usize, y as usize, d != 2 && d != 4);
            }
        }
    }
    // Timing patterns.
    for i in 8..n - 8 {
        g.set(i, 6, i % 2 == 0);
        g.set(6, i, i % 2 == 0);
    }
    // Alignment pattern (versions 2..=5: one, at the bottom-right).
    if version >= 2 {
        let c = ALIGN[version - 1];
        for dy in -2i32..=2 {
            for dx in -2i32..=2 {
                g.set((c as i32 + dx) as usize, (c as i32 + dy) as usize, dx.abs().max(dy.abs()) != 1);
            }
        }
    }
    // Reserve the format information areas (filled in later) and the dark module.
    for i in 0..9 {
        if i != 6 {
            g.set(8, i, false); // (8, 6) and (6, 8) stay timing modules
            g.set(i, 8, false);
        }
    }
    for i in 0..8 {
        g.set(n - 1 - i, 8, false);
        g.set(8, n - 1 - i, false);
    }
    g.set(8, n - 8, true);
}

/// The 15 format bits for level L and `mask`: 5 data bits + 10 BCH bits, xor 0x5412.
pub fn format_bits(mask: u8) -> u32 {
    let data = (ECL_L << 3) | mask as u32;
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ ((rem >> 9) * 0x537);
    }
    ((data << 10) | rem) ^ 0x5412
}

fn draw_format(g: &mut Grid, mask: u8) {
    let n = g.size;
    let bits = format_bits(mask);
    let bit = |i: usize| bits >> i & 1 == 1;
    // First copy around the top-left finder.
    for i in 0..=5 {
        g.set(8, i, bit(i));
    }
    g.set(8, 7, bit(6));
    g.set(8, 8, bit(7));
    g.set(7, 8, bit(8));
    for i in 9..15 {
        g.set(14 - i, 8, bit(i));
    }
    // Second copy: below the top-right... and right of the bottom-left finder.
    for i in 0..8 {
        g.set(n - 1 - i, 8, bit(i));
    }
    for i in 8..15 {
        g.set(8, n - 15 + i, bit(i));
    }
    g.set(8, n - 8, true);
}

/// Visit the data module positions in placement order (zigzag from the bottom right).
fn for_each_data_module(g: &Grid, mut f: impl FnMut(usize, usize)) {
    let n = g.size as i32;
    let mut right = n - 1;
    let mut upward = true;
    while right >= 1 {
        if right == 6 {
            right = 5;
        }
        for v in 0..n {
            let y = if upward { n - 1 - v } else { v };
            for j in 0..2 {
                let x = right - j;
                if !g.func[(y * n + x) as usize] {
                    f(x as usize, y as usize);
                }
            }
        }
        upward = !upward;
        right -= 2;
    }
}

fn mask_bit(mask: u8, x: usize, y: usize) -> bool {
    let (x, y) = (x, y);
    match mask {
        0 => (x + y) % 2 == 0,
        1 => y % 2 == 0,
        2 => x % 3 == 0,
        3 => (x + y) % 3 == 0,
        4 => (x / 3 + y / 2) % 2 == 0,
        5 => x * y % 2 + x * y % 3 == 0,
        6 => (x * y % 2 + x * y % 3) % 2 == 0,
        _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
    }
}

fn penalty(g: &Grid) -> u32 {
    let n = g.size;
    let d = |x: usize, y: usize| g.dark[y * n + x];
    let mut score = 0u32;
    // N1: runs of 5 or more; N3: 1:1:3:1:1 finder-like pattern with 4 light modules beside it.
    for horizontal in [true, false] {
        for a in 0..n {
            let line: Vec<bool> = (0..n).map(|b| if horizontal { d(b, a) } else { d(a, b) }).collect();
            let mut run = 1;
            for i in 1..n {
                if line[i] == line[i - 1] {
                    run += 1;
                } else {
                    if run >= 5 {
                        score += 3 + (run - 5) as u32;
                    }
                    run = 1;
                }
            }
            if run >= 5 {
                score += 3 + (run - 5) as u32;
            }
            let pat = [true, false, true, true, true, false, true];
            for i in 0..n.saturating_sub(6) {
                if line[i..i + 7] == pat {
                    let before = i >= 4 && line[i - 4..i].iter().all(|&b| !b);
                    let after = i + 11 <= n && line[i + 7..i + 11].iter().all(|&b| !b);
                    if before || after {
                        score += 40;
                    }
                }
            }
        }
    }
    // N2: 2x2 blocks.
    for y in 0..n - 1 {
        for x in 0..n - 1 {
            let c = d(x, y);
            if c == d(x + 1, y) && c == d(x, y + 1) && c == d(x + 1, y + 1) {
                score += 3;
            }
        }
    }
    // N4: balance of dark modules.
    let dark = g.dark.iter().filter(|&&b| b).count() as i32;
    let pct = dark * 100 / (n * n) as i32;
    score += ((pct - 50).unsigned_abs() / 5) * 10;
    score
}

/// Encode `data` (bytes) at level L in the smallest version that fits (1..=5).
pub fn encode(data: &[u8]) -> Result<Qr, String> {
    let version = (1..=5)
        .find(|&v| data.len() * 8 + 12 <= CAPACITY[v - 1].0 * 8)
        .ok_or_else(|| format!("{} bytes do not fit a version 5 code (106 bytes at most)", data.len()))?;
    let (cap, ecc) = CAPACITY[version - 1];
    let mut words = data_codewords(data, version);
    debug_assert_eq!(words.len(), cap);
    let rem = rs_remainder(&words, ecc);
    words.extend(rem);

    let size = 17 + 4 * version;
    let mut base = Grid::new(size);
    draw_function_patterns(&mut base, version);
    // Data bits, most significant first.
    let bits: Vec<bool> = words.iter().flat_map(|&w| (0..8).rev().map(move |i| w >> i & 1 == 1)).collect();
    let mut positions = vec![];
    for_each_data_module(&base, |x, y| positions.push((x, y)));
    debug_assert!(positions.len() >= bits.len());
    for (i, &(x, y)) in positions.iter().enumerate() {
        // Remainder modules (after the last codeword bit) stay light before masking.
        base.dark[y * size + x] = bits.get(i).copied().unwrap_or(false);
    }
    let mut best: Option<(u32, u8, Grid)> = None;
    for mask in 0..8u8 {
        let mut g = Grid { size, dark: base.dark.clone(), func: base.func.clone() };
        for &(x, y) in &positions {
            if mask_bit(mask, x, y) {
                g.dark[y * size + x] = !g.dark[y * size + x];
            }
        }
        draw_format(&mut g, mask);
        let p = penalty(&g);
        if best.as_ref().map_or(true, |(bp, _, _)| p < *bp) {
            best = Some((p, mask, g));
        }
    }
    let (_, mask, g) = best.unwrap();
    Ok(Qr { version, size, mask, modules: g.dark })
}

/// Paint the code at (x, y) with `scale` pixels per module and a 4 module quiet zone, black on
/// white whatever the theme. Returns the side length of the whole square in pixels.
pub fn draw(c: &mut Canvas, qr: &Qr, x: i32, y: i32, scale: i32) -> i32 {
    let total = (qr.size as i32 + 8) * scale;
    c.set_draw_color(Color::RGB(255, 255, 255));
    let _ = c.fill_rect(Rect::new(x, y, total as u32, total as u32));
    c.set_draw_color(Color::RGB(0, 0, 0));
    for my in 0..qr.size {
        for mx in 0..qr.size {
            if qr.get(mx, my) {
                let _ = c.fill_rect(Rect::new(x + (mx as i32 + 4) * scale, y + (my as i32 + 4) * scale, scale as u32, scale as u32));
            }
        }
    }
    total
}

/// Side in pixels of the drawn code (with quiet zone) at `scale`.
pub fn pixel_size(qr: &Qr, scale: i32) -> i32 {
    (qr.size as i32 + 8) * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_finder(q: &Qr, ox: usize, oy: usize) -> bool {
        for dy in 0..7 {
            for dx in 0..7 {
                let ring = dx.min(6 - dx).min(dy.min(6 - dy));
                // ring 0 dark, 1 light, 2 and 3 dark (the 3x3 core).
                let want = ring != 1;
                if q.get(ox + dx, oy + dy) != want {
                    return false;
                }
            }
        }
        true
    }

    /// Read the 15 format bits back from the first copy (top-left finder).
    fn read_format(q: &Qr, second: bool) -> u32 {
        let n = q.size;
        let mut v = 0u32;
        let mut put = |i: usize, b: bool| v |= (b as u32) << i;
        if !second {
            for i in 0..=5 {
                put(i, q.get(8, i));
            }
            put(6, q.get(8, 7));
            put(7, q.get(8, 8));
            put(8, q.get(7, 8));
            for i in 9..15 {
                put(i, q.get(14 - i, 8));
            }
        } else {
            for i in 0..8 {
                put(i, q.get(n - 1 - i, 8));
            }
            for i in 8..15 {
                put(i, q.get(8, n - 15 + i));
            }
        }
        v
    }

    /// Undo the mask and read the codewords back in placement order.
    fn read_codewords(q: &Qr) -> Vec<u8> {
        let mut g = Grid::new(q.size);
        draw_function_patterns(&mut g, q.version);
        let mut bits = vec![];
        for_each_data_module(&g, |x, y| bits.push(q.get(x, y) ^ mask_bit(q.mask, x, y)));
        bits.chunks(8).filter(|c| c.len() == 8).map(|c| c.iter().fold(0u8, |a, &b| a << 1 | b as u8)).collect()
    }

    #[test]
    fn version_two_has_finders_timing_alignment_and_the_dark_module() {
        // 25 bytes: version 2 at level L (version 1 holds 17).
        let q = encode(b"https://example.com/12345").unwrap();
        assert_eq!((q.version, q.size), (2, 25));
        assert!(is_finder(&q, 0, 0) && is_finder(&q, 18, 0) && is_finder(&q, 0, 18), "three finder patterns");
        // Separators are light.
        for i in 0..8 {
            assert!(!q.get(7, i) && !q.get(i, 7), "separator of the top-left finder");
        }
        for i in 8..17 {
            assert_eq!(q.get(i, 6), i % 2 == 0, "horizontal timing");
            assert_eq!(q.get(6, i), i % 2 == 0, "vertical timing");
        }
        // The alignment pattern is centred on (18, 18): dark centre, light ring, dark border.
        assert!(q.get(18, 18) && !q.get(17, 18) && !q.get(19, 17) && q.get(16, 16) && q.get(20, 20));
        assert!(q.get(8, q.size - 8), "the always-dark module");
    }

    #[test]
    fn format_information_is_valid_and_stored_twice() {
        let q = encode(b"https://example.com/12345").unwrap();
        let (a, b) = (read_format(&q, false), read_format(&q, true));
        assert_eq!(a, b, "both copies agree");
        assert_eq!(a, format_bits(q.mask));
        // BCH check: (value xor 0x5412) is a multiple of the generator 0x537, and the level bits say L (01).
        let mut v = a ^ 0x5412;
        for i in (10..15).rev() {
            if v >> i & 1 == 1 {
                v ^= 0x537 << (i - 10);
            }
        }
        assert_eq!(v, 0, "format BCH remainder");
        assert_eq!((a ^ 0x5412) >> 13 & 0b11, ECL_L);
        // A known value: level L, mask 0 is 111011111000100.
        assert_eq!(format_bits(0), 0b111011111000100);
    }

    #[test]
    fn the_data_reads_back_and_the_reed_solomon_check_is_clean() {
        let text = b"https://github.com/kurogedelic/HarbourShell/blob/main/docs/AGENT.md";
        let q = encode(text).unwrap();
        assert_eq!((q.version, q.size), (4, 33), "72 bytes need version 4 at level L");
        let words = read_codewords(&q);
        let (cap, ecc) = CAPACITY[q.version - 1];
        assert_eq!(&words[..cap + ecc].len(), &(cap + ecc));
        // Data codewords: mode 0100, 8 bit count, then the bytes.
        assert_eq!(words[0] >> 4, 0b0100);
        let count = ((words[0] & 0x0f) << 4 | words[1] >> 4) as usize;
        assert_eq!(count, text.len());
        let got: Vec<u8> = (0..count).map(|i| (words[1 + i] & 0x0f) << 4 | words[2 + i] >> 4).collect();
        assert_eq!(got, text);
        // The stored error correction codewords are exactly the remainder of the data.
        assert_eq!(&words[cap..cap + ecc], rs_remainder(&words[..cap], ecc).as_slice());
    }

    #[test]
    fn reed_solomon_matches_the_well_known_example() {
        // The "HELLO WORLD" version 1-M example of the ISO text uses 10 codewords; check the classic
        // 16-codeword data with 10 EC words (version 1-M): 32 91 11 120 209 114 220 77 67 64 236 17 236 17 236 17.
        let data = [32u8, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17];
        assert_eq!(rs_remainder(&data, 10), [196, 35, 39, 119, 235, 215, 231, 226, 93, 23]);
    }

    #[test]
    fn short_text_is_version_one_and_too_long_text_is_refused() {
        assert_eq!(encode(b"hi").unwrap().version, 1);
        assert_eq!(encode(&[b'a'; 17]).unwrap().version, 1);
        assert_eq!(encode(&[b'a'; 18]).unwrap().version, 2);
        assert_eq!(encode(&[b'a'; 106]).unwrap().version, 5);
        assert!(encode(&[b'a'; 107]).is_err());
    }
}
