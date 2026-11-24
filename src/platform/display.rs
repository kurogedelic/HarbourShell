//! LCD output abstraction: a DRM/KMS dumb-buffer backend (preferred) and the legacy `/dev/fb0`
//! backend (automatic fallback). Everything that does not need hardware lives here so it can be
//! unit-tested on the Mac.

use std::io;

/// Half-open pixel rectangle (`x1`/`y1` exclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl Rect {
    pub fn full(w: usize, h: usize) -> Self {
        Rect { x0: 0, y0: 0, x1: w, y1: h }
    }
    pub fn width(&self) -> usize {
        self.x1 - self.x0
    }
    pub fn height(&self) -> usize {
        self.y1 - self.y0
    }
    pub fn area(&self) -> usize {
        self.width() * self.height()
    }
}

/// Bounding box of the pixels that differ between two tightly packed RGB565 frames
/// (`w*2` bytes per row). `None` when the frames are identical.
pub fn damage_rect(prev: &[u8], new: &[u8], w: usize, h: usize) -> Option<Rect> {
    let row = w * 2;
    assert!(prev.len() >= row * h && new.len() >= row * h);
    let (mut x0, mut x1) = (usize::MAX, 0usize);
    let (mut y0, mut y1) = (usize::MAX, 0usize);
    for y in 0..h {
        let a = &prev[y * row..(y + 1) * row];
        let b = &new[y * row..(y + 1) * row];
        if a == b {
            continue;
        }
        // first / last differing pixel in this row
        let first = (0..w).find(|&x| a[2 * x..2 * x + 2] != b[2 * x..2 * x + 2]).unwrap();
        let last = (0..w).rev().find(|&x| a[2 * x..2 * x + 2] != b[2 * x..2 * x + 2]).unwrap();
        x0 = x0.min(first);
        x1 = x1.max(last + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    if y0 == usize::MAX {
        None
    } else {
        Some(Rect { x0, y0, x1, y1 })
    }
}

/// RGB565 (little endian bytes) -> XRGB8888 pixel value.
pub fn rgb565_to_xrgb8888(lo: u8, hi: u8) -> u32 {
    let p = u16::from_le_bytes([lo, hi]) as u32;
    let (r, g, b) = ((p >> 11) & 31, (p >> 5) & 63, p & 31);
    let (r, g, b) = ((r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2));
    (r << 16) | (g << 8) | b
}

/// What the launcher needs from the LCD. Frames are tightly packed RGB565, `width*2` bytes/row.
pub trait Display {
    /// Send a frame; returns the bytes pushed to the device. Unchanged frames cost nothing.
    fn present(&mut self, rgb565: &[u8]) -> io::Result<usize>;
    /// Forget the previous frame so the next `present` rewrites everything.
    fn invalidate(&mut self);
    /// Something else drew on the LCD since our last `present` (a launched app).
    fn taken_over(&self) -> bool;
    /// Make sure the panel is lit. (No FBIOBLANK: it once left the LCD dead.)
    fn unblank(&self) {}
    /// An external app is about to use the LCD: hand it over (DRM: back to fbdev emulation).
    fn enter_external(&mut self) {}
    /// The external app is gone: take the LCD back.
    fn leave_external(&mut self) {}
    /// Re-pushing the whole frame shortly after a change heals SPI glitches on fbdev; DRM does not need it.
    fn wants_refresh(&self) -> bool {
        true
    }
    fn backend(&self) -> &'static str;
    /// The panel's size in pixels (what `present` takes).
    fn size(&self) -> (usize, usize);
    /// The panel's physical size in millimetres, when the driver says.
    fn mm(&self) -> Option<(u32, u32)> {
        None
    }
    /// What the panel can show (colour, grey levels, black and white).
    fn kind(&self) -> PanelKind {
        PanelKind::Color
    }
}

/// What a panel can show. A grey or black-and-white panel gets Harbour's grey or 1-bit theme (the
/// colour themes are not offered), and its picture is turned into grey levels or into black and
/// white with an ordered dither before it is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Color,
    Gray,
    Mono,
}

impl PanelKind {
    /// `HARBOUR_PANEL=color|gray|mono` overrides what the driver suggests.
    pub fn from_env(detected: PanelKind) -> PanelKind {
        match std::env::var("HARBOUR_PANEL").as_deref() {
            Ok("color") | Ok("colour") => PanelKind::Color,
            Ok("gray") | Ok("grey") => PanelKind::Gray,
            Ok("mono") | Ok("1bit") => PanelKind::Mono,
            _ => detected,
        }
    }
}

/// Luminance 0..=255 of an RGB565 pixel (little-endian bytes).
pub fn luma565(lo: u8, hi: u8) -> u8 {
    let p = u16::from_le_bytes([lo, hi]) as u32;
    let (r, g, b) = (((p >> 11) & 31) * 255 / 31, ((p >> 5) & 63) * 255 / 63, (p & 31) * 255 / 31);
    ((r * 299 + g * 587 + b * 114) / 1000) as u8
}

fn gray565(v: u8) -> [u8; 2] {
    let v = v as u16;
    let p = ((v >> 3) << 11) | ((v >> 2) << 5) | (v >> 3);
    p.to_le_bytes()
}

/// 4 x 4 ordered-dither thresholds (0..16).
const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Turn a `w` x `h` RGB565 frame into what a grey or black-and-white panel shows. Grey: every pixel
/// becomes its grey. Black and white: dark tones are solid black and light ones solid white (so
/// text and lines stay whole); the tones between become an ordered dither, like Harbour's own
/// dithered shadows.
pub fn tone(frame: &mut [u8], w: usize, h: usize, kind: PanelKind) {
    if kind == PanelKind::Color {
        return;
    }
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 2;
            let l = luma565(frame[i], frame[i + 1]);
            let v = match kind {
                PanelKind::Gray => l,
                _ => {
                    const DARK: u32 = 80;
                    const LIGHT: u32 = 215;
                    let l = l as u32;
                    if l <= DARK {
                        0
                    } else if l >= LIGHT {
                        255
                    } else {
                        let level = (l - DARK) * 16 / (LIGHT - DARK);
                        if level > BAYER4[y % 4][x % 4] as u32 { 255 } else { 0 }
                    }
                }
            };
            let g = gray565(v);
            frame[i] = g[0];
            frame[i + 1] = g[1];
        }
    }
}

/// Quarter turns between Harbour's picture and the panel (`HARBOUR_ROTATE=0|90|180|270`), for a
/// panel mounted on its side (some handhelds have a portrait panel in a landscape case).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    pub fn parse(s: &str) -> Rotation {
        match s.trim() {
            "90" => Rotation::R90,
            "180" => Rotation::R180,
            "270" => Rotation::R270,
            _ => Rotation::R0,
        }
    }

    /// The panel's size as Harbour sees it (sideways turns swap the sides).
    pub fn view(self, (w, h): (usize, usize)) -> (usize, usize) {
        match self {
            Rotation::R90 | Rotation::R270 => (h, w),
            _ => (w, h),
        }
    }
}

/// A panel bigger than Harbour's picture: each picture pixel becomes `k` x `k` panel pixels, the
/// picture sits in the middle (black around it) and may be turned. The panel below still only
/// sends what changed.
pub struct Scaled {
    inner: Box<dyn Display>,
    pub kind: PanelKind,
    toned: Vec<u8>,
    /// Harbour's picture size.
    pub w: usize,
    pub h: usize,
    pub k: usize,
    rot: Rotation,
    buf: Vec<u8>,
}

impl Scaled {
    pub fn new(inner: Box<dyn Display>, (w, h): (usize, usize), k: usize, rot: Rotation) -> Self {
        let (pw, ph) = inner.size();
        let kind = PanelKind::from_env(inner.kind());
        Self { inner, kind, toned: Vec::new(), w, h, k: k.max(1), rot, buf: vec![0u8; pw * ph * 2] }
    }
}

/// Expand a `w` x `h` RGB565 picture `k` times into a `pw` x `ph` panel buffer, centred and turned
/// by `rot` (the area around it is left as it is: black from the start).
pub fn expand(src: &[u8], (w, h): (usize, usize), k: usize, rot: Rotation, dst: &mut [u8], (pw, ph): (usize, usize)) {
    let (vw, vh) = rot.view((pw, ph));
    let (ox, oy) = ((vw.saturating_sub(w * k)) / 2, (vh.saturating_sub(h * k)) / 2);
    for y in 0..h {
        for x in 0..w {
            let p = [src[(y * w + x) * 2], src[(y * w + x) * 2 + 1]];
            for dy in 0..k {
                for dx in 0..k {
                    // (vx, vy): where the pixel lands in the picture's own orientation.
                    let (vx, vy) = (ox + x * k + dx, oy + y * k + dy);
                    if vx >= vw || vy >= vh {
                        continue;
                    }
                    let (px, py) = match rot {
                        Rotation::R0 => (vx, vy),
                        Rotation::R90 => (pw - 1 - vy, vx),
                        Rotation::R180 => (pw - 1 - vx, ph - 1 - vy),
                        Rotation::R270 => (vy, ph - 1 - vx),
                    };
                    let i = (py * pw + px) * 2;
                    dst[i] = p[0];
                    dst[i + 1] = p[1];
                }
            }
        }
    }
}

impl Display for Scaled {
    fn present(&mut self, rgb565: &[u8]) -> io::Result<usize> {
        if rgb565.len() < self.w * self.h * 2 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too small"));
        }
        let ps = self.inner.size();
        let src: &[u8] = if self.kind == PanelKind::Color {
            rgb565
        } else {
            self.toned.clear();
            self.toned.extend_from_slice(&rgb565[..self.w * self.h * 2]);
            tone(&mut self.toned, self.w, self.h, self.kind);
            &self.toned
        };
        if self.k == 1 && self.rot == Rotation::R0 && ps == (self.w, self.h) {
            return self.inner.present(src);
        }
        expand(src, (self.w, self.h), self.k, self.rot, &mut self.buf, ps);
        self.inner.present(&self.buf)
    }
    fn invalidate(&mut self) {
        self.inner.invalidate()
    }
    fn taken_over(&self) -> bool {
        self.inner.taken_over()
    }
    fn unblank(&self) {
        self.inner.unblank()
    }
    fn enter_external(&mut self) {
        self.inner.enter_external()
    }
    fn leave_external(&mut self) {
        self.inner.leave_external()
    }
    fn wants_refresh(&self) -> bool {
        self.inner.wants_refresh()
    }
    fn backend(&self) -> &'static str {
        self.inner.backend()
    }
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }
    fn mm(&self) -> Option<(u32, u32)> {
        self.inner.mm()
    }
    fn kind(&self) -> PanelKind {
        self.kind
    }
}

/// How Harbour uses a panel of `panel` pixels (already turned by `HARBOUR_ROTATE`), `mm` its
/// physical size if known: the picture size and the whole-number scale. Each Harbour pixel should
/// come out about as big as on the Cardputer Zero or a little bigger (about 160 per inch); without
/// the physical size, a scale of one per 360 panel rows. Never smaller than 320 x 170.
pub fn choose_scale(panel: (usize, usize), mm: Option<(u32, u32)>, forced: Option<usize>) -> ((usize, usize), usize) {
    let (pw, ph) = panel;
    let fits = |k: usize| k >= 1 && pw / k >= 320 && ph / k >= 170;
    let mut k = match forced {
        Some(k) => k.clamp(1, 8),
        None => match mm.filter(|(mw, _)| *mw > 0) {
            Some((mw, _)) => {
                let ppi = pw as f32 / (mw as f32 / 25.4);
                ((ppi / 160.0).round() as usize).clamp(1, 8)
            }
            None => (pw.min(ph) / 360).max(1),
        },
    };
    while k > 1 && !fits(k) {
        k -= 1;
    }
    (((pw / k).max(320.min(pw)), (ph / k).max(170.min(ph))), k)
}

/// Open the best available backend: DRM unless `HARBOUR_DISPLAY=fbdev`, else (or on any failure)
/// the framebuffer at `fbdev_path`.
#[cfg(target_os = "linux")]
pub fn open(fbdev_path: &str) -> io::Result<Box<dyn Display>> {
    let want = std::env::var("HARBOUR_DISPLAY").unwrap_or_else(|_| "auto".into());
    if want != "fbdev" {
        let dev = std::env::var("HARBOUR_DRM").ok();
        match super::drm::DrmDisplay::open(dev.as_deref(), fbdev_path) {
            Ok(d) => {
                eprintln!("[display] backend: DRM {}", d.describe());
                return Ok(Box::new(d));
            }
            Err(e) => {
                eprintln!("[display] DRM unavailable ({e}); falling back to fbdev {fbdev_path}");
                if want == "drm" {
                    return Err(e);
                }
            }
        }
    }
    let f = super::fb::Framebuffer::open(fbdev_path)?;
    eprintln!("[display] backend: fbdev {fbdev_path}");
    Ok(Box::new(f))
}

#[cfg(not(target_os = "linux"))]
pub fn open(_: &str) -> io::Result<Box<dyn Display>> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "LCD output needs Linux"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize) -> Vec<u8> {
        vec![0u8; w * h * 2]
    }

    #[test]
    fn identical_frames_have_no_damage() {
        let a = frame(8, 4);
        assert_eq!(damage_rect(&a, &a.clone(), 8, 4), None);
    }

    #[test]
    fn single_pixel() {
        let a = frame(8, 4);
        let mut b = a.clone();
        b[(2 * 8 + 5) * 2 + 1] = 0xff; // x=5, y=2 (high byte)
        assert_eq!(damage_rect(&a, &b, 8, 4), Some(Rect { x0: 5, y0: 2, x1: 6, y1: 3 }));
    }

    #[test]
    fn bounding_box_of_two_regions() {
        let a = frame(10, 10);
        let mut b = a.clone();
        b[(1 * 10 + 7) * 2] = 1; // (7,1)
        b[(8 * 10 + 2) * 2] = 1; // (2,8)
        let r = damage_rect(&a, &b, 10, 10).unwrap();
        assert_eq!(r, Rect { x0: 2, y0: 1, x1: 8, y1: 9 });
        assert_eq!(r.area(), 6 * 8);
    }

    #[test]
    fn full_change_is_full_rect() {
        let a = frame(6, 3);
        let b = vec![0xffu8; 6 * 3 * 2];
        assert_eq!(damage_rect(&a, &b, 6, 3), Some(Rect::full(6, 3)));
    }

    #[test]
    fn scale_follows_the_panel() {
        // The Cardputer Zero: as it is.
        assert_eq!(choose_scale((320, 170), None, None), ((320, 170), 1));
        // A 5" 1280 x 720 handheld (about 294 per inch): twice.
        assert_eq!(choose_scale((1280, 720), Some((110, 62)), None), ((640, 360), 2));
        // Without the size: one per 360 rows.
        assert_eq!(choose_scale((720, 720), None, None), ((360, 360), 2));
        // A 400 x 240 memory LCD: as it is; a 24" monitor (about 92 per inch): as it is.
        assert_eq!(choose_scale((400, 240), Some((60, 36)), None), ((400, 240), 1));
        assert_eq!(choose_scale((1920, 1080), Some((531, 299)), None), ((1920, 1080), 1));
        // Never under 320 x 170: a forced 4 on 800 x 480 comes down to 2.
        assert_eq!(choose_scale((800, 480), None, Some(4)), ((400, 240), 2));
    }

    #[test]
    fn expand_scales_centres_and_turns() {
        // A 2 x 1 picture (pixels A, B) at 2x on a 6 x 4 panel: centred at (1, 1).
        let src = [1u8, 0, 2, 0];
        let mut dst = vec![0u8; 6 * 4 * 2];
        expand(&src, (2, 1), 2, Rotation::R0, &mut dst, (6, 4));
        let at = |d: &[u8], w: usize, x: usize, y: usize| d[(y * w + x) * 2];
        assert_eq!((at(&dst, 6, 1, 1), at(&dst, 6, 2, 2), at(&dst, 6, 3, 1), at(&dst, 6, 4, 2)), (1, 1, 2, 2));
        assert_eq!(at(&dst, 6, 0, 0), 0, "black around");
        // On a portrait 4 x 6 panel turned 90: the picture's top-left lands top-right.
        let mut dst = vec![0u8; 4 * 6 * 2];
        expand(&src, (2, 1), 2, Rotation::R90, &mut dst, (4, 6));
        assert_eq!(at(&dst, 4, 2, 1), 1, "A");
        assert_eq!(at(&dst, 4, 2, 3), 2, "B below it");
    }

    #[test]
    fn tone_keeps_text_whole_and_dithers_the_middle() {
        let px = |v: u8| gray565(v);
        let mut f: Vec<u8> = [0u8, 40, 128, 230, 255].iter().flat_map(|&v| px(v)).collect();
        tone(&mut f, 5, 1, PanelKind::Mono);
        let l: Vec<u8> = f.chunks(2).map(|p| luma565(p[0], p[1])).collect();
        assert_eq!((l[0], l[1], l[3], l[4]), (0, 0, 255, 255), "dark is black, light is white");
        // A mid grey over a 4 x 4 block: some black, some white.
        let mut g: Vec<u8> = (0..16).flat_map(|_| px(150)).collect();
        tone(&mut g, 4, 4, PanelKind::Mono);
        let whites = g.chunks(2).filter(|p| luma565(p[0], p[1]) == 255).count();
        assert!(whites > 4 && whites < 12, "{whites} of 16 white");
        let mut c: Vec<u8> = vec![0x00, 0xf8]; // pure red
        tone(&mut c, 1, 1, PanelKind::Gray);
        let p = u16::from_le_bytes([c[0], c[1]]);
        assert_eq!(p >> 11, p & 31, "red and blue equal: a grey");
    }

    #[test]
    fn xrgb_conversion() {
        assert_eq!(rgb565_to_xrgb8888(0xff, 0xff), 0x00ff_ffff);
        assert_eq!(rgb565_to_xrgb8888(0, 0), 0);
        assert_eq!(rgb565_to_xrgb8888(0x00, 0xf8), 0x00ff_0000); // pure red
        assert_eq!(rgb565_to_xrgb8888(0x1f, 0x00), 0x0000_00ff); // pure blue
    }
}
