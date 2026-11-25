//! Compares `harbour_shell::gfx` with SDL2's software renderer, pixel by pixel.
//!
//! Each case draws the same operations on an SDL canvas (on an RGB565 SDL surface, no
//! window) and on a `gfx::Canvas`, then compares every pixel. Random cases use a
//! fixed seed, so a failure can be repeated.

use harbour_shell::gfx;
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{BlendMode, Canvas as SdlCanvas, ClippingRect};
use sdl2::surface::Surface as SdlSurface;
use std::cell::Cell;

// Counters are per test thread, so the numbers printed by each test are its own.
thread_local! {
    static CASES: Cell<usize> = const { Cell::new(0) };
    static SKIPPED_TIES: Cell<usize> = const { Cell::new(0) };
}
fn bump(c: &'static std::thread::LocalKey<Cell<usize>>) {
    c.with(|v| v.set(v.get() + 1));
}
fn count(c: &'static std::thread::LocalKey<Cell<usize>>) -> usize {
    c.with(|v| v.get())
}

// ---------- small seeded random numbers ----------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
    /// A number in lo..=hi.
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1) as u32) as i32
    }
    fn byte(&mut self) -> u8 {
        self.next() as u8
    }
}

// ---------- operations applied to both canvases ----------

#[derive(Clone, Debug)]
enum Op {
    Color(u8, u8, u8),
    Clear,
    Fill(Option<(i32, i32, u32, u32)>),
    FillRects(Vec<(i32, i32, u32, u32)>),
    DrawRect(i32, i32, u32, u32),
    Line((i32, i32), (i32, i32)),
    Lines(Vec<(i32, i32)>),
    Point((i32, i32)),
    Points(Vec<(i32, i32)>),
    /// None = no clip, Some(None) = zero-sized clip.
    Clip(Option<Option<(i32, i32, u32, u32)>>),
    Viewport(Option<(i32, i32, u32, u32)>),
    /// Whole-number scale (SDL: set_scale(k, k)).
    Scale(u32),
    /// image index, src rect, dst rect, tint.
    Blit(usize, Option<(i32, i32, u32, u32)>, Option<(i32, i32, u32, u32)>, (u8, u8, u8)),
}

/// A small RGBA test image (alpha only 0 or 255).
#[derive(Clone)]
struct Img {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

fn random_image(rng: &mut Rng, w: u32, h: u32) -> Img {
    let mut rgba = Vec::new();
    for _ in 0..w * h {
        rgba.extend_from_slice(&[rng.byte(), rng.byte(), rng.byte(), if rng.next() % 4 == 0 { 0 } else { 255 }]);
    }
    Img { w, h, rgba }
}

fn srect(r: (i32, i32, u32, u32)) -> sdl2::rect::Rect {
    sdl2::rect::Rect::new(r.0, r.1, r.2, r.3)
}
fn grect(r: (i32, i32, u32, u32)) -> gfx::Rect {
    gfx::Rect::new(r.0, r.1, r.2, r.3)
}

fn apply_sdl(c: &mut SdlCanvas<SdlSurface<'static>>, imgs: &[Img], ops: &[Op]) {
    use sdl2::pixels::Color;
    use sdl2::rect::Point;
    let creator = c.texture_creator();
    for op in ops {
        match op {
            Op::Color(r, g, b) => c.set_draw_color(Color::RGB(*r, *g, *b)),
            Op::Clear => c.clear(),
            Op::Fill(r) => c.fill_rect(r.map(srect)).unwrap(),
            Op::FillRects(rs) => {
                let v: Vec<_> = rs.iter().map(|r| srect(*r)).collect();
                c.fill_rects(&v).unwrap()
            }
            Op::DrawRect(x, y, w, h) => c.draw_rect(srect((*x, *y, *w, *h))).unwrap(),
            Op::Line(a, b) => c.draw_line(Point::new(a.0, a.1), Point::new(b.0, b.1)).unwrap(),
            Op::Lines(p) => {
                let v: Vec<_> = p.iter().map(|q| Point::new(q.0, q.1)).collect();
                c.draw_lines(&v[..]).unwrap()
            }
            Op::Point(p) => c.draw_point(Point::new(p.0, p.1)).unwrap(),
            Op::Points(p) => {
                let v: Vec<_> = p.iter().map(|q| Point::new(q.0, q.1)).collect();
                c.draw_points(&v[..]).unwrap()
            }
            Op::Clip(None) => c.set_clip_rect(None),
            Op::Clip(Some(None)) => c.set_clip_rect(ClippingRect::Zero),
            Op::Clip(Some(Some(r))) => c.set_clip_rect(srect(*r)),
            Op::Viewport(v) => c.set_viewport(v.map(srect)),
            Op::Scale(k) => c.set_scale(*k as f32, *k as f32).unwrap(),
            Op::Blit(i, src, dst, tint) => {
                let img = &imgs[*i];
                let mut data = img.rgba.clone();
                let surf = SdlSurface::from_data(&mut data, img.w, img.h, img.w * 4, PixelFormatEnum::RGBA32).unwrap();
                let mut tex = creator.create_texture_from_surface(&surf).unwrap();
                tex.set_blend_mode(BlendMode::Blend);
                tex.set_color_mod(tint.0, tint.1, tint.2);
                c.copy(&tex, src.map(srect), dst.map(srect)).unwrap();
            }
        }
    }
}

fn apply_gfx(c: &mut gfx::Canvas, imgs: &[Img], ops: &[Op]) {
    use gfx::{Color, Point};
    for op in ops {
        match op {
            Op::Color(r, g, b) => c.set_draw_color(Color::RGB(*r, *g, *b)),
            Op::Clear => c.clear(),
            Op::Fill(r) => c.fill_rect(r.map(grect)).unwrap(),
            Op::FillRects(rs) => {
                let v: Vec<_> = rs.iter().map(|r| grect(*r)).collect();
                c.fill_rects(&v).unwrap()
            }
            Op::DrawRect(x, y, w, h) => c.draw_rect(grect((*x, *y, *w, *h))).unwrap(),
            Op::Line(a, b) => c.draw_line(*a, *b).unwrap(),
            Op::Lines(p) => {
                let v: Vec<_> = p.iter().map(|q| Point::new(q.0, q.1)).collect();
                c.draw_lines(&v[..]).unwrap()
            }
            Op::Point(p) => c.draw_point(*p).unwrap(),
            Op::Points(p) => {
                let v: Vec<_> = p.iter().map(|q| Point::new(q.0, q.1)).collect();
                c.draw_points(&v[..]).unwrap()
            }
            Op::Clip(None) => c.set_clip_rect(None),
            Op::Clip(Some(None)) => c.set_clip_rect(gfx::ClipRect::Zero),
            Op::Clip(Some(Some(r))) => c.set_clip_rect(grect(*r)),
            Op::Viewport(v) => c.set_viewport(v.map(grect)),
            Op::Scale(k) => c.set_scale(*k),
            Op::Blit(i, src, dst, tint) => {
                let img = &imgs[*i];
                let image = gfx::Image::from_rgba(img.w, img.h, img.rgba.clone()).unwrap();
                c.blit(&image, src.map(grect), dst.map(grect), Color::RGB(tint.0, tint.1, tint.2)).unwrap();
            }
        }
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Does a scaled blit in `ops` fall on a rounding tie that SDL decides with `double`
/// noise? SDL cuts a scaled copy by a clip rectangle in floating point; when a rounded
/// value is exactly x.5 and the scale is not a power-of-two fraction, whether it
/// rounds up or down depends on the last bit of a double, which integer code cannot
/// copy. Those cases are skipped (and counted), not compared.
fn has_rounding_tie(w: u32, h: u32, imgs: &[Img], ops: &[Op]) -> bool {
    let bounds = gfx::Rect::new(0, 0, w, h);
    let mut viewport = bounds;
    let mut clip: Option<Option<gfx::Rect>> = None;
    for op in ops {
        match op {
            Op::Viewport(v) => viewport = v.map(grect).unwrap_or(bounds),
            Op::Clip(c) => clip = c.map(|c| c.map(grect)),
            Op::Blit(i, src, dst, _) => {
                let img = &imgs[*i];
                let src = src.map(grect).and_then(|r| r.intersection(gfx::Rect::new(0, 0, img.w, img.h))).unwrap_or(gfx::Rect::new(0, 0, img.w, img.h));
                let mut dst = dst.map(grect).unwrap_or(gfx::Rect::new(0, 0, viewport.width(), viewport.height()));
                dst.offset(viewport.x(), viewport.y());
                let area = match clip {
                    None => Some(viewport),
                    Some(None) => None,
                    Some(Some(mut c)) => {
                        c.offset(viewport.x(), viewport.y());
                        c.intersection(viewport)
                    }
                };
                let Some(area) = area.and_then(|a| a.intersection(bounds)) else { continue };
                let Some(vis) = dst.intersection(area) else { continue };
                if (src.width(), src.height()) == (dst.width(), dst.height()) || !bounds.contains_rect(dst) {
                    continue;
                }
                for (s, d, cut, visible) in [
                    (src.width() as i64, dst.width() as i64, (vis.x() - dst.x()) as i64, vis.width() as i64),
                    (src.height() as i64, dst.height() as i64, (vis.y() - dst.y()) as i64, vis.height() as i64),
                ] {
                    let reduced = s / gcd(s, d);
                    let dyadic = reduced & (reduced - 1) == 0;
                    let tie = (2 * cut * s) % (2 * d) == d || (2 * s * visible) % (2 * d) == d;
                    if tie && !dyadic {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

/// Run `ops` on both canvases (starting from a `bg` fill) and compare all pixels.
fn compare(w: u32, h: u32, imgs: &[Img], ops: &[Op]) {
    if has_rounding_tie(w, h, imgs, ops) {
        bump(&SKIPPED_TIES);
        return;
    }
    let mut sdl: SdlCanvas<SdlSurface<'static>> = SdlSurface::new(w, h, PixelFormatEnum::RGB565).unwrap().into_canvas().unwrap();
    let mut mine = gfx::Canvas::new(gfx::Surface::new(w, h));
    let mut all = vec![Op::Color(30, 60, 90), Op::Clear, Op::Color(255, 255, 255)];
    all.extend_from_slice(ops);
    apply_sdl(&mut sdl, imgs, &all);
    apply_gfx(&mut mine, imgs, &all);
    sdl.set_scale(1.0, 1.0).unwrap();
    mine.set_scale(1);
    sdl.set_viewport(None);
    mine.set_viewport(None);
    let want = sdl.read_pixels(None, PixelFormatEnum::RGB565).unwrap();
    let got = mine.read_pixels(None, gfx::PixelFormat::Rgb565).unwrap();
    bump(&CASES);
    if want != got {
        let i = want.iter().zip(&got).position(|(a, b)| a != b).unwrap() / 2;
        let (x, y) = (i as u32 % w, i as u32 / w);
        let mut shown = String::new();
        for y in 0..h.min(40) {
            for x in 0..w.min(80) {
                let k = ((y * w + x) * 2) as usize;
                shown.push(match (want[k..k + 2] == got[k..k + 2], want[k] != 0 || want[k + 1] != 0) {
                    (true, true) => '#',
                    (true, false) => '.',
                    (false, _) => 'X',
                });
            }
            shown.push('\n');
        }
        panic!("pixel ({x},{y}) differs: sdl={:?} gfx={:?}\nops: {all:?}\n{shown}", &want[i * 2..i * 2 + 2], &got[i * 2..i * 2 + 2]);
    }
}

fn report(name: &str, before: usize) {
    println!("{name}: {} cases pixel-identical, {} skipped as rounding ties", count(&CASES) - before, count(&SKIPPED_TIES));
}

fn rand_rect(rng: &mut Rng, span: i32) -> (i32, i32, u32, u32) {
    (rng.range(-span, span * 2), rng.range(-span, span * 2), rng.range(0, span + 5) as u32, rng.range(0, span + 5) as u32)
}

// ---------- tests ----------

#[test]
fn hand_picked_lines_match() {
    let before = count(&CASES);
    let mut lines: Vec<((i32, i32), (i32, i32))> = vec![
        ((2, 2), (2, 2)),     // single point
        ((2, 2), (12, 2)),    // horizontal
        ((12, 2), (2, 2)),    // horizontal reversed
        ((2, 2), (2, 12)),    // vertical
        ((2, 12), (2, 2)),    // vertical reversed
        ((2, 2), (12, 12)),   // diagonal
        ((12, 12), (2, 2)),   // diagonal reversed
        ((12, 2), (2, 12)),   // anti-diagonal
        ((2, 2), (20, 7)),    // shallow
        ((20, 7), (2, 2)),    // shallow reversed
        ((2, 2), (7, 20)),    // steep
        ((7, 20), (2, 2)),    // steep reversed
        ((2, 2), (3, 20)),    // very steep
        ((2, 2), (20, 3)),    // very shallow
        ((-10, -5), (40, 25)), // crosses the whole canvas
        ((-10, 5), (40, 5)),  // horizontal, clipped both ends
        ((5, -10), (5, 40)),  // vertical, clipped both ends
        ((-30, -30), (-5, -5)), // fully outside
        ((-5, 10), (-5, 20)), // fully outside, vertical
        ((10, 10), (100, 37)), // one end clipped
        ((100, 37), (10, 10)), // reversed
        ((31, 31), (31, 31)), // single point at the corner
        ((32, 32), (32, 32)), // single point just outside
        ((0, 0), (31, 31)),
        ((31, 0), (0, 31)),
        ((-1, -1), (32, 32)),
    ];
    // Every short line from the middle of the canvas.
    for dx in -9..=9 {
        for dy in -9..=9 {
            lines.push(((16, 16), (16 + dx, 16 + dy)));
        }
    }
    let setups: Vec<Vec<Op>> = vec![
        vec![],
        vec![Op::Clip(Some(Some((5, 5, 15, 12))))],
        vec![Op::Viewport(Some((6, 4, 20, 20)))],
        vec![Op::Viewport(Some((6, 4, 20, 20))), Op::Clip(Some(Some((3, 3, 10, 10))))],
        vec![Op::Clip(Some(None))],
    ];
    for setup in &setups {
        for (a, b) in &lines {
            let mut ops = setup.clone();
            ops.push(Op::Line(*a, *b));
            compare(32, 32, &[], &ops);
        }
    }
    report("hand-picked lines (with clip / viewport / zero clip)", before);
}

#[test]
fn random_lines_and_paths_match() {
    let before = count(&CASES);
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for _ in 0..6000 {
        let mut ops = Vec::new();
        if rng.next() % 3 == 0 {
            ops.push(Op::Viewport(Some(rand_rect(&mut rng, 20))));
        }
        if rng.next() % 3 == 0 {
            ops.push(Op::Clip(Some(Some(rand_rect(&mut rng, 20)))));
        }
        if rng.next() % 2 == 0 {
            ops.push(Op::Line((rng.range(-30, 60), rng.range(-30, 60)), (rng.range(-30, 60), rng.range(-30, 60))));
        } else {
            let n = rng.range(2, 6);
            let pts: Vec<_> = (0..n).map(|_| (rng.range(-30, 60), rng.range(-30, 60))).collect();
            ops.push(Op::Lines(pts));
        }
        compare(40, 30, &[], &ops);
    }
    report("random lines / polylines", before);
}

#[test]
fn rect_outlines_and_fills_match() {
    let before = count(&CASES);
    let mut rng = Rng(0xfeed_beef_1234_5678);
    // Tiny sizes first: 1xN, Nx1, 1x1, 2x2.
    for w in 1..=4u32 {
        for h in 1..=4u32 {
            compare(12, 12, &[], &[Op::DrawRect(3, 3, w, h)]);
            compare(12, 12, &[], &[Op::Fill(Some((3, 3, w, h)))]);
        }
    }
    compare(12, 12, &[], &[Op::DrawRect(0, 0, 0, 0)]);
    compare(12, 12, &[], &[Op::Fill(None)]);
    for _ in 0..4000 {
        let mut ops = Vec::new();
        if rng.next() % 3 == 0 {
            ops.push(Op::Viewport(Some(rand_rect(&mut rng, 20))));
        }
        if rng.next() % 3 == 0 {
            ops.push(Op::Clip(Some(Some(rand_rect(&mut rng, 20)))));
        }
        match rng.next() % 3 {
            0 => {
                let r = rand_rect(&mut rng, 20);
                ops.push(Op::DrawRect(r.0, r.1, r.2, r.3));
            }
            1 => ops.push(Op::Fill(Some(rand_rect(&mut rng, 20)))),
            _ => ops.push(Op::FillRects((0..3).map(|_| rand_rect(&mut rng, 20)).collect())),
        }
        compare(40, 30, &[], &ops);
    }
    report("rect outlines / fills / fill_rects", before);
}

#[test]
fn clip_viewport_clear_and_points_match() {
    let before = count(&CASES);
    let mut rng = Rng(0x0bad_cafe_0000_0001);
    // clear ignores viewport and clip
    compare(20, 20, &[], &[Op::Viewport(Some((5, 5, 5, 5))), Op::Color(200, 10, 10), Op::Clear]);
    compare(20, 20, &[], &[Op::Clip(Some(Some((5, 5, 5, 5)))), Op::Color(200, 10, 10), Op::Clear]);
    compare(20, 20, &[], &[Op::Clip(Some(None)), Op::Color(200, 10, 10), Op::Clear]);
    // clip larger than the viewport, clip outside the viewport, negative origins
    compare(30, 30, &[], &[Op::Viewport(Some((5, 5, 10, 10))), Op::Clip(Some(Some((-5, -5, 40, 40)))), Op::Fill(Some((-8, -8, 50, 50)))]);
    compare(30, 30, &[], &[Op::Viewport(Some((-5, -5, 20, 20))), Op::Fill(Some((0, 0, 30, 30)))]);
    compare(30, 30, &[], &[Op::Viewport(Some((20, 20, 50, 50))), Op::Fill(Some((0, 0, 30, 30)))]);
    compare(30, 30, &[], &[Op::Viewport(Some((50, 50, 5, 5))), Op::Fill(None)]);
    for _ in 0..3000 {
        let mut ops = Vec::new();
        for _ in 0..rng.range(1, 6) {
            match rng.next() % 6 {
                0 => ops.push(Op::Viewport(if rng.next() % 4 == 0 { None } else { Some(rand_rect(&mut rng, 20)) })),
                1 => ops.push(Op::Clip(match rng.next() % 5 {
                    0 => None,
                    1 => Some(None),
                    _ => Some(Some(rand_rect(&mut rng, 20))),
                })),
                2 => ops.push(Op::Point((rng.range(-10, 50), rng.range(-10, 40)))),
                3 => ops.push(Op::Points((0..8).map(|_| (rng.range(-10, 50), rng.range(-10, 40))).collect())),
                4 => ops.push(Op::Color(rng.byte(), rng.byte(), rng.byte())),
                _ => ops.push(Op::Fill(Some(rand_rect(&mut rng, 20)))),
            }
        }
        compare(40, 30, &[], &ops);
    }
    report("clip / viewport / clear / points", before);
}

#[test]
fn random_op_sequences_match() {
    let before = count(&CASES);
    let mut rng = Rng(0x5eed_0000_dead_0001);
    let imgs: Vec<Img> = vec![random_image(&mut rng, 8, 8), random_image(&mut rng, 5, 11), random_image(&mut rng, 16, 16)];
    for _ in 0..1500 {
        let mut ops = Vec::new();
        for _ in 0..rng.range(5, 30) {
            match rng.next() % 12 {
                0 => ops.push(Op::Color(rng.byte(), rng.byte(), rng.byte())),
                1 => ops.push(Op::Fill(Some(rand_rect(&mut rng, 20)))),
                2 => {
                    let r = rand_rect(&mut rng, 20);
                    ops.push(Op::DrawRect(r.0, r.1, r.2, r.3))
                }
                3 | 4 => ops.push(Op::Line((rng.range(-30, 60), rng.range(-30, 60)), (rng.range(-30, 60), rng.range(-30, 60)))),
                5 => ops.push(Op::Lines((0..rng.range(2, 6)).map(|_| (rng.range(-30, 60), rng.range(-30, 60))).collect())),
                6 => ops.push(Op::Point((rng.range(-10, 50), rng.range(-10, 40)))),
                7 => ops.push(Op::Viewport(if rng.next() % 3 == 0 { None } else { Some(rand_rect(&mut rng, 20)) })),
                8 => ops.push(Op::Clip(if rng.next() % 3 == 0 { None } else { Some(Some(rand_rect(&mut rng, 20))) })),
                9 => ops.push(Op::Clear),
                _ => {
                    let i = rng.range(0, 2) as usize;
                    let (w, h) = (imgs[i].w, imgs[i].h);
                    let src = if rng.next() % 2 == 0 { None } else { Some((rng.range(0, w as i32 - 1), rng.range(0, h as i32 - 1), rng.range(1, w as i32) as u32, rng.range(1, h as i32) as u32)) };
                    ops.push(Op::Blit(i, src, Some(rand_rect(&mut rng, 20)), (rng.byte(), rng.byte(), rng.byte())));
                }
            }
        }
        compare(40, 30, &imgs, &ops);
    }
    report("random mixed op sequences", before);
}

#[test]
fn blits_match_sdl() {
    let before = count(&CASES);
    let mut rng = Rng(0xb117_b117_0000_0003);
    let imgs: Vec<Img> = vec![
        random_image(&mut rng, 1, 1),
        random_image(&mut rng, 8, 8),
        random_image(&mut rng, 12, 7),
        random_image(&mut rng, 32, 32),
        random_image(&mut rng, 3, 20),
    ];
    // Same size, integer scales, odd scales, shrinking.
    for (i, img) in imgs.iter().enumerate() {
        for &(dw, dh) in &[(img.w, img.h), (img.w * 2, img.h * 2), (img.w * 3, img.h), (img.w / 2 + 1, img.h / 2 + 1), (img.w + 1, img.h + 1), (5, 5), (17, 9), (1, 1), (64, 48)] {
            for &(dx, dy) in &[(0, 0), (4, 3), (-3, -2), (30, 20), (-100, 5)] {
                compare(48, 40, &imgs, &[Op::Blit(i, None, Some((dx, dy, dw, dh)), (255, 255, 255))]);
            }
        }
    }
    // src rects, tint, clip and viewport.
    for _ in 0..6000 {
        let i = rng.range(0, 4) as usize;
        let (w, h) = (imgs[i].w as i32, imgs[i].h as i32);
        let src = if rng.next() % 2 == 0 { None } else { Some((rng.range(0, w - 1), rng.range(0, h - 1), rng.range(1, w) as u32, rng.range(1, h) as u32)) };
        let mut ops = Vec::new();
        if rng.next() % 3 == 0 {
            ops.push(Op::Viewport(Some(rand_rect(&mut rng, 20))));
        }
        if rng.next() % 3 == 0 {
            ops.push(Op::Clip(Some(Some(rand_rect(&mut rng, 20)))));
        }
        let tint = if rng.next() % 2 == 0 { (255, 255, 255) } else { (rng.byte(), rng.byte(), rng.byte()) };
        let dst = if rng.next() % 10 == 0 { None } else { Some((rng.range(-20, 40), rng.range(-20, 30), rng.range(1, 70) as u32, rng.range(1, 60) as u32)) };
        ops.push(Op::Blit(i, src, dst, tint));
        compare(40, 30, &imgs, &ops);
    }
    report("blits (scale, src rect, tint, clip, viewport)", before);
}

#[test]
fn read_pixels_formats_match() {
    let before = count(&CASES);
    let mut rng = Rng(0x7ead_0000_0000_0005);
    let mut ops = vec![];
    for _ in 0..40 {
        ops.push(Op::Color(rng.byte(), rng.byte(), rng.byte()));
        ops.push(Op::Fill(Some(rand_rect(&mut rng, 15))));
    }
    let mut sdl: SdlCanvas<SdlSurface<'static>> = SdlSurface::new(30, 20, PixelFormatEnum::RGB565).unwrap().into_canvas().unwrap();
    let mut mine = gfx::Canvas::new(gfx::Surface::new(30, 20));
    apply_sdl(&mut sdl, &[], &ops);
    apply_gfx(&mut mine, &[], &ops);
    for (sf, gf) in [
        (PixelFormatEnum::RGB565, gfx::PixelFormat::Rgb565),
        (PixelFormatEnum::RGB24, gfx::PixelFormat::Rgb24),
        (PixelFormatEnum::ARGB8888, gfx::PixelFormat::Argb8888),
    ] {
        for rect in [None, Some((0, 0, 30, 20)), Some((3, 4, 10, 7)), Some((29, 19, 1, 1))] {
            let want = sdl.read_pixels(rect.map(srect), sf).unwrap();
            let got = mine.read_pixels(rect.map(grect), gf).unwrap();
            if want != got {
                let i = want.iter().zip(&got).position(|(a, b)| a != b).unwrap_or(0);
                panic!("{gf:?} {rect:?}: byte {i}: sdl={:?} gfx={:?}", &want[i / 4 * 4..(i / 4 * 4 + 4).min(want.len())], &got[i / 4 * 4..(i / 4 * 4 + 4).min(got.len())]);
            }
            bump(&CASES);
        }
    }
    // Not compared: read_pixels with a viewport set. SDL 2.32 returns rows that are
    // mis-copied (zero-filled gaps) there, so it is no reference; gfx documents its own rule
    // (None = the viewport, a rect is in surface pixels) and a unit test in canvas.rs checks it.
    report("read_pixels", before);
}

#[test]
fn rect_and_point_semantics_match_sdl() {
    let before = count(&CASES);
    let mut rng = Rng(0x2ec7_0000_0000_0007);
    let big = [i32::MIN, i32::MAX, 0, 1, -1, 1_000_000_000, -1_000_000_000];
    let pick = |rng: &mut Rng| if rng.next() % 5 == 0 { big[rng.next() as usize % big.len()] } else { rng.range(-30, 30) };
    for _ in 0..20000 {
        let (x, y, w, h) = (pick(&mut rng), pick(&mut rng), pick(&mut rng).unsigned_abs() % 60, pick(&mut rng).unsigned_abs() % 60);
        let (x2, y2, w2, h2) = (pick(&mut rng), pick(&mut rng), pick(&mut rng).unsigned_abs() % 60, pick(&mut rng).unsigned_abs() % 60);
        let (s, g) = (sdl2::rect::Rect::new(x, y, w, h), gfx::Rect::new(x, y, w, h));
        let (s2, g2) = (sdl2::rect::Rect::new(x2, y2, w2, h2), gfx::Rect::new(x2, y2, w2, h2));
        let t = |r: sdl2::rect::Rect| (r.x(), r.y(), r.width(), r.height());
        let u = |r: gfx::Rect| (r.x(), r.y(), r.width(), r.height());
        let ctx = format!("{:?} {:?}", (x, y, w, h), (x2, y2, w2, h2));
        assert_eq!(t(s), u(g), "new {ctx}");
        assert_eq!((s.left(), s.right(), s.top(), s.bottom()), (g.left(), g.right(), g.top(), g.bottom()), "edges {ctx}");
        assert_eq!(s.size(), g.size());
        assert_eq!(s.has_intersection(s2), g.has_intersection(g2), "has_intersection {ctx}");
        assert_eq!(s.intersection(s2).map(t), g.intersection(g2).map(u), "intersection {ctx}");
        assert_eq!(s.contains_rect(s2), g.contains_rect(g2), "contains_rect {ctx}");
        assert_eq!(s.center().x(), g.center().x());
        assert_eq!(s.center().y(), g.center().y());
        let (px, py) = (rng.range(-35, 35), rng.range(-35, 35));
        assert_eq!(s.contains_point((px, py)), g.contains_point((px, py)), "contains_point {ctx}");
        let (ox, oy) = (pick(&mut rng), pick(&mut rng));
        let (mut a, mut b) = (s, g);
        a.offset(ox, oy);
        b.offset(ox, oy);
        assert_eq!(t(a), u(b), "offset {ctx} {ox},{oy}");
        let (mut a, mut b) = (s, g);
        a.set_x(ox);
        a.set_y(oy);
        a.set_width(w2);
        a.set_height(h2);
        b.set_x(ox);
        b.set_y(oy);
        b.set_width(w2);
        b.set_height(h2);
        assert_eq!(t(a), u(b), "setters {ctx} {ox},{oy}");
        let (mut a, mut b) = (s, g);
        a.center_on((ox, oy));
        b.center_on((ox, oy));
        assert_eq!(t(a), u(b), "center_on {ctx}");
        let (mut a, mut b) = (s, g);
        a.set_right(ox);
        b.set_right(ox);
        a.set_bottom(oy);
        b.set_bottom(oy);
        assert_eq!(t(a), u(b), "set_right/bottom {ctx}");
        assert_eq!(t(s.left_shifted(ox)), u(g.left_shifted(ox)), "left_shifted");
        assert_eq!(t(s.right_shifted(ox)), u(g.right_shifted(ox)), "right_shifted");
        assert_eq!(t(s.top_shifted(oy)), u(g.top_shifted(oy)), "top_shifted");
        assert_eq!(t(s.bottom_shifted(oy)), u(g.bottom_shifted(oy)), "bottom_shifted");
        assert_eq!(t(s.union(s2)), u(g.union(g2)), "union {ctx}");
        let (sp, gp) = (sdl2::rect::Point::new(ox, oy), gfx::Point::new(ox, oy));
        assert_eq!((sp.x(), sp.y()), (gp.x(), gp.y()), "Point::new");
        let (dx, dy) = (pick(&mut rng), pick(&mut rng));
        let (so, go) = (sp.offset(dx, dy), gp.offset(dx, dy));
        assert_eq!((so.x(), so.y()), (go.x(), go.y()), "Point::offset");
        let f = rng.range(-5, 5);
        let (ss, gs) = (sp.scale(f), gp.scale(f));
        assert_eq!((ss.x(), ss.y()), (gs.x(), gs.y()), "Point::scale");
        bump(&CASES);
    }
    report("Rect / Point methods", before);
}

#[test]
fn dither_fill_matches_the_pattern_points() {
    use harbour_shell::dither::Pattern;
    let before = count(&CASES);
    for p in [Pattern::Eighth, Pattern::Quarter, Pattern::Half, Pattern::ThreeQuarter, Pattern::Scan, Pattern::Bars] {
        for r in [(0, 0, 40, 30), (3, 5, 17, 9), (-4, -3, 20, 20), (30, 20, 30, 30)] {
            // SDL side: what dither::fill does, a bundle of draw_points.
            let mut pts = Vec::new();
            for y in r.1..r.1 + r.3 as i32 {
                for x in r.0..r.0 + r.2 as i32 {
                    if p.lit(x, y) {
                        pts.push(sdl2::rect::Point::new(x, y));
                    }
                }
            }
            let mut sdl: SdlCanvas<SdlSurface<'static>> = SdlSurface::new(40, 30, PixelFormatEnum::RGB565).unwrap().into_canvas().unwrap();
            sdl.set_draw_color(sdl2::pixels::Color::RGB(255, 255, 255));
            sdl.draw_points(&pts[..]).unwrap();
            let mut mine = gfx::Canvas::new(gfx::Surface::new(40, 30));
            mine.set_draw_color(gfx::Color::RGB(255, 255, 255));
            mine.dither_fill(grect(r), |x, y| p.lit(x, y)).unwrap();
            assert_eq!(sdl.read_pixels(None, PixelFormatEnum::RGB565).unwrap(), mine.read_pixels(None, gfx::PixelFormat::Rgb565).unwrap(), "{p:?} {r:?}");
            bump(&CASES);
        }
    }
    report("dither_fill vs draw_points of the pattern", before);
}

#[test]
fn bitmaps_and_glyphs_draw_set_bits_only() {
    // Compared with plain draw_point calls on the SDL side (what font.rs does today).
    let mut rng = Rng(0x61f0_0000_0000_0009);
    let rows: Vec<u16> = (0..12).map(|_| rng.next() as u16).collect();
    let mut sdl: SdlCanvas<SdlSurface<'static>> = SdlSurface::new(40, 30, PixelFormatEnum::RGB565).unwrap().into_canvas().unwrap();
    sdl.set_draw_color(sdl2::pixels::Color::RGB(255, 255, 255));
    for (row, bits) in rows.iter().enumerate() {
        for col in 0..10 {
            if bits & (0x8000 >> col) != 0 {
                sdl.draw_point(sdl2::rect::Point::new(35 + col, 4 + row as i32)).unwrap(); // partly clipped on the right
            }
        }
    }
    let mut mine = gfx::Canvas::new(gfx::Surface::new(40, 30));
    mine.set_draw_color(gfx::Color::RGB(255, 255, 255));
    mine.draw_glyph(35, 4, 10, &rows).unwrap();
    assert_eq!(sdl.read_pixels(None, PixelFormatEnum::RGB565).unwrap(), mine.read_pixels(None, gfx::PixelFormat::Rgb565).unwrap());

    // draw_bitmap: 10 wide (2 bytes per row), same bits.
    let bytes: Vec<u8> = rows.iter().flat_map(|r| r.to_be_bytes()).collect();
    let mut mine2 = gfx::Canvas::new(gfx::Surface::new(40, 30));
    mine2.set_draw_color(gfx::Color::RGB(255, 255, 255));
    mine2.draw_bitmap(35, 4, 10, 12, &bytes, 2).unwrap();
    assert_eq!(mine.read_pixels(None, gfx::PixelFormat::Rgb565).unwrap(), mine2.read_pixels(None, gfx::PixelFormat::Rgb565).unwrap());
}

#[test]
fn scaled_drawing_matches() {
    let before = count(&CASES);
    let mut rng = Rng(0x5ca1_ed00_0000_0003);
    let imgs: Vec<Img> = vec![random_image(&mut rng, 8, 8), random_image(&mut rng, 5, 11), random_image(&mut rng, 16, 16)];
    // Hand-picked: lines and points at scale 2 and 3, clipped and not.
    for k in 2..=3u32 {
        for setup in [vec![], vec![Op::Clip(Some(Some((2, 2, 6, 5))))], vec![Op::Viewport(Some((1, 2, 8, 6)))]] {
            for dx in -6..=6 {
                for dy in -6..=6 {
                    let mut ops = vec![Op::Scale(k)];
                    ops.extend(setup.iter().cloned());
                    ops.push(Op::Line((6, 5), (6 + dx, 5 + dy)));
                    compare(40, 30, &[], &ops);
                }
            }
        }
    }
    for _ in 0..3000 {
        let k = rng.range(2, 3) as u32;
        let mut ops = vec![Op::Scale(k)];
        // Lines are only compared while the viewport is the whole surface and the line lies
        // inside the picture: SDL's scaled path cuts a line at the edge in its own way (a
        // clipped end can move the Bresenham ties), and the shell never relies on that.
        let mut vp_full = true;
        for _ in 0..rng.range(3, 14) {
            match rng.next() % 11 {
                0 => {
                    let v = if rng.next() % 4 == 0 { None } else { Some(rand_rect(&mut rng, 6)) };
                    vp_full = v.is_none();
                    ops.push(Op::Viewport(v));
                }
                1 => ops.push(Op::Clip(match rng.next() % 5 {
                    0 => None,
                    1 => Some(None),
                    _ => Some(Some(rand_rect(&mut rng, 6))),
                })),
                2 => ops.push(Op::Point((rng.range(-3, 14), rng.range(-3, 11)))),
                3 => ops.push(Op::Points((0..6).map(|_| (rng.range(-3, 14), rng.range(-3, 11))).collect())),
                4 => ops.push(Op::Color(rng.byte(), rng.byte(), rng.byte())),
                5 if vp_full => ops.push(Op::Line((rng.range(0, 12), rng.range(0, 9)), (rng.range(0, 12), rng.range(0, 9)))),
                6 if vp_full => ops.push(Op::Lines((0..rng.range(2, 5)).map(|_| (rng.range(0, 12), rng.range(0, 9))).collect())),
                5 | 6 => ops.push(Op::Point((rng.range(-3, 14), rng.range(-3, 11)))),
                7 => {
                    let r = rand_rect(&mut rng, 6);
                    ops.push(Op::DrawRect(r.0, r.1, r.2, r.3));
                }
                8 => ops.push(Op::Fill(if rng.next() % 5 == 0 { None } else { Some(rand_rect(&mut rng, 6)) })),
                9 => ops.push(Op::Scale(rng.range(1, 3) as u32)),
                _ => {
                    // a blit at the picture's own size, or stretched
                    let i = rng.range(0, 2) as usize;
                    let (w, h) = (imgs[i].w, imgs[i].h);
                    let dst = if rng.next() % 2 == 0 { (rng.range(-3, 8), rng.range(-3, 6), w, h) } else { rand_rect(&mut rng, 6) };
                    ops.push(Op::Blit(i, None, Some(dst), (rng.byte(), rng.byte(), rng.byte())));
                }
            }
        }
        compare(40, 30, &imgs, &ops);
    }
    report("scaled drawing (set_scale 2..3)", before);
}
