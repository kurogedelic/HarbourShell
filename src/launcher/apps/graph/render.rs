//! Turning a document into a picture, a little at a time.
//!
//! A [`Job`] computes into a [`Raster`]: one byte per pixel, either a tone (0 = paper ..
//! [`SHADE`] = ink) or a line ([`GRID`], [`AXIS`], [`INK`]). It knows nothing about the screen or
//! the theme: the app turns tones into theme colours and dither when it draws (`super::paint`).
//!
//! The web build has no threads, so nothing here runs in the background: the app calls
//! [`Job::step`] every frame with a budget of work, shows the picture as it grows, and drops the job
//! (that is all "stop" means) when the document changes.

use super::doc::{Doc, Mode, View};
use super::expr::{self, Prog};

/// Tones are 0..=SHADE (paper .. ink); the values above are lines.
pub const SHADE: u8 = 200;
/// A grid line (faint).
pub const GRID: u8 = 253;
/// An axis or a contour line (stronger).
pub const AXIS: u8 = 254;
/// A curve, a coast, a cell edge, a stream line: full ink.
pub const INK: u8 = 255;

#[derive(Clone, Debug, PartialEq)]
pub struct Raster {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Raster {
    pub fn new(w: usize, h: usize) -> Raster {
        Raster { w, h, px: vec![0; w * h] }
    }

    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.px[y * self.w + x]
    }

    /// Set a pixel; outside the raster nothing happens.
    pub fn set(&mut self, x: i64, y: i64, v: u8) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.px[y as usize * self.w + x as usize] = v;
        }
    }

    /// A square dot `pen` pixels wide centred on (x, y).
    fn dot(&mut self, x: i64, y: i64, pen: i64, v: u8) {
        let lo = -(pen - 1) / 2;
        for dy in lo..lo + pen {
            for dx in lo..lo + pen {
                self.set(x + dx, y + dy, v);
            }
        }
    }

    /// A straight line (Bresenham), each point a `pen` dot.
    pub fn line(&mut self, (x0, y0): (i64, i64), (x1, y1): (i64, i64), pen: i64, v: u8) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut e) = (x0, y0, dx + dy);
        loop {
            self.dot(x, y, pen, v);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * e;
            if e2 >= dy {
                e += dy;
                x += sx;
            }
            if e2 <= dx {
                e += dx;
                y += sy;
            }
        }
    }
}

/// A tone for a value from 0 (paper) to 1 (ink).
pub fn tone(v: f64) -> u8 {
    if v.is_finite() { (v.clamp(0.0, 1.0) * SHADE as f64).round() as u8 } else { 0 }
}

/// Where the plane sits on a raster of `w` x `h` pixels: x grows to the right, y upwards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub view: View,
}

impl Frame {
    /// Plane units per pixel.
    pub fn scale(&self) -> f64 {
        self.view.span / self.w.min(self.h).max(1) as f64
    }

    /// The point of the plane at the centre of pixel (px, py).
    pub fn to_plane(&self, px: f64, py: f64) -> (f64, f64) {
        let s = self.scale();
        (self.view.cx + (px + 0.5 - self.w as f64 / 2.0) * s, self.view.cy - (py + 0.5 - self.h as f64 / 2.0) * s)
    }

    /// The pixel (as a fraction) a point of the plane falls on.
    pub fn to_pixel(&self, x: f64, y: f64) -> (f64, f64) {
        let s = self.scale();
        ((x - self.view.cx) / s + self.w as f64 / 2.0 - 0.5, -(y - self.view.cy) / s + self.h as f64 / 2.0 - 0.5)
    }
}

/// Grid spacing in plane units: 1, 2 or 5 times a power of ten, about 32 pixels apart.
pub fn grid_step(scale: f64) -> f64 {
    let raw = 32.0 * scale;
    let p = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * p).find(|s| *s >= raw).unwrap_or(10.0 * p)
}

/// Draw the grid (and the axes) over whatever is there.
fn draw_grid(r: &mut Raster, f: &Frame) {
    let step = grid_step(f.scale());
    let (x0, y1) = f.to_plane(0.0, 0.0);
    let (x1, y0) = f.to_plane(f.w as f64, f.h as f64);
    let mut k = (x0 / step).floor();
    while k * step <= x1 {
        let v = if k == 0.0 { AXIS } else { GRID };
        let px = f.to_pixel(k * step, 0.0).0.round() as i64;
        for y in 0..f.h as i64 {
            // Grid lines are dotted (every other pixel); the axes are solid.
            if v == AXIS || y % 2 == 0 {
                r.set(px, y, v);
            }
        }
        k += 1.0;
    }
    let mut k = (y0 / step).floor();
    while k * step <= y1 {
        let v = if k == 0.0 { AXIS } else { GRID };
        let py = f.to_pixel(0.0, k * step).1.round() as i64;
        for x in 0..f.w as i64 {
            if v == AXIS || x % 2 == 0 {
                r.set(x, py, v);
            }
        }
        k += 1.0;
    }
}

/// The sliders the drawing reads, looked up once.
#[derive(Clone, Copy, Debug)]
struct Settings {
    pen: i64,
    turns: f64,
    sea: f64,
    contour: f64,
    sun: f64,
    n: usize,
    edge: f64,
    metric: i64,
    gap: usize,
    shade: f64,
}

impl Settings {
    fn of(d: &Doc) -> Settings {
        Settings {
            pen: d.setting("pen").round().clamp(1.0, 4.0) as i64,
            turns: d.setting("turns").round().clamp(1.0, 40.0),
            sea: d.setting("sea"),
            contour: d.setting("lines").max(0.0),
            sun: d.setting("sun"),
            n: d.setting("n").round().clamp(1.0, 300.0) as usize,
            edge: d.setting("edge").max(0.0),
            metric: d.setting("norm").round().clamp(1.0, 3.0) as i64,
            gap: d.setting("gap").round().clamp(3.0, 60.0) as usize,
            shade: d.setting("shade").clamp(0.0, 1.0),
        }
    }
}

/// How strongly slopes show in the hill shading.
const RELIEF: f64 = 0.35;

/// Variable slots (see `doc::VARS`).
const X: usize = 0;
const Y: usize = 1;
const THETA: usize = 3;
const U: usize = 4;
const I: usize = 5;
const N: usize = 6;

/// One picture being computed.
pub struct Job {
    mode: Mode,
    progs: Vec<Prog>,
    vars: Vec<f64>,
    seed: u32,
    frame: Frame,
    grid: bool,
    set: Settings,
    pub raster: Raster,
    /// Which pass is running (what it means depends on the mode) and how far it got.
    pass: u8,
    pos: usize,
    /// Curves: the previous point, in pixels.
    last: Option<(f64, f64)>,
    /// Terrain: the height of every pixel.
    heights: Vec<f64>,
    /// Voronoi: every cell's point and shade.
    sites: Vec<(f64, f64, f64)>,
    /// Field: where stream lines start, and which line owns each cell of a coarse grid.
    starts: Vec<(f64, f64)>,
    owner: Vec<u32>,
}

impl Job {
    /// A job for `doc` at time `t` on a raster of `w` x `h`. A formula that does not compile gives
    /// its slot and the problem.
    pub fn new(doc: &Doc, t: f64, w: usize, h: usize) -> Result<Job, (usize, expr::Error)> {
        let progs = doc.compile()?;
        let (w, h) = (w.max(1), h.max(1));
        let frame = Frame { w, h, view: doc.view };
        let mut job = Job {
            mode: doc.mode,
            progs,
            vars: doc.base_vars(t),
            seed: doc.seed,
            frame,
            grid: doc.grid,
            set: Settings::of(doc),
            raster: Raster::new(w, h),
            pass: 0,
            pos: 0,
            last: None,
            heights: Vec::new(),
            sites: Vec::new(),
            starts: Vec::new(),
            owner: Vec::new(),
        };
        job.start();
        Ok(job)
    }

    /// The quick part every mode does first.
    fn start(&mut self) {
        match self.mode {
            Mode::Function | Mode::Polar | Mode::Parametric => {
                if self.grid {
                    draw_grid(&mut self.raster, &self.frame);
                }
            }
            Mode::Terrain => self.heights = vec![0.0; self.frame.w * self.frame.h],
            Mode::Voronoi => {
                let n = self.set.n;
                self.vars[N] = n as f64;
                self.sites = (0..n)
                    .map(|i| {
                        self.vars[I] = i as f64;
                        let x = self.progs[0].eval(&self.vars, self.seed);
                        let y = self.progs[1].eval(&self.vars, self.seed);
                        let v = self.progs[2].eval(&self.vars, self.seed);
                        (x, y, v)
                    })
                    .filter(|(x, y, _)| x.is_finite() && y.is_finite())
                    .collect();
            }
            Mode::Field => {
                // Stream lines start on a grid `gap` pixels apart, each nudged a little at random
                // (by the seed), and are tried in a shuffled order so no corner is always first.
                let g = self.set.gap;
                let (cols, rows) = (self.frame.w.div_ceil(g), self.frame.h.div_ceil(g));
                let mut starts: Vec<(u32, (f64, f64))> = Vec::new();
                for r in 0..rows {
                    for c in 0..cols {
                        let k = (r * cols + c) as f64;
                        let jx = expr::rand(k, self.seed) - 0.5;
                        let jy = expr::rand(k + 0.5, self.seed) - 0.5;
                        let p = ((c as f64 + 0.5 + jx * 0.8) * g as f64, (r as f64 + 0.5 + jy * 0.8) * g as f64);
                        starts.push((expr::hash32(k as u32 ^ self.seed), p));
                    }
                }
                starts.sort_by_key(|s| s.0);
                self.starts = starts.into_iter().map(|s| s.1).collect();
                let c = self.cell();
                self.owner = vec![0; self.frame.w.div_ceil(c) * self.frame.h.div_ceil(c)];
            }
        }
    }

    /// True once the picture is complete.
    pub fn done(&self) -> bool {
        self.pass == u8::MAX
    }

    fn finish(&mut self) {
        if self.grid && !matches!(self.mode, Mode::Function | Mode::Polar | Mode::Parametric) {
            draw_grid(&mut self.raster, &self.frame);
        }
        self.pass = u8::MAX;
    }

    /// Do about `budget` formula evaluations of work. True when the picture is complete.
    pub fn step(&mut self, budget: usize) -> bool {
        if self.done() {
            return true;
        }
        let mut left = budget as i64;
        while left > 0 && !self.done() {
            left -= match self.mode {
                Mode::Function | Mode::Polar | Mode::Parametric => self.curve_step(),
                Mode::Terrain => self.terrain_step(),
                Mode::Voronoi => self.voronoi_step(),
                Mode::Field => self.field_step(),
            };
        }
        self.done()
    }

    /// Roughly how much is done, 0..=1.
    pub fn progress(&self) -> f32 {
        if self.done() {
            return 1.0;
        }
        let px = (self.frame.w * self.frame.h).max(1) as f32;
        match self.mode {
            Mode::Function | Mode::Polar | Mode::Parametric => self.pos as f32 / self.samples().max(1) as f32,
            Mode::Terrain => (self.pass as f32 + self.pos as f32 / px) / 2.0,
            Mode::Voronoi => self.pos as f32 / px,
            Mode::Field => match self.pass {
                0 => self.pos as f32 / px / 2.0,
                _ => 0.5 + self.pos as f32 / self.starts.len().max(1) as f32 / 2.0,
            },
        }
    }

    // -------------------------------------------------------------------------- curves

    /// How many points a curve is drawn through.
    fn samples(&self) -> usize {
        match self.mode {
            Mode::Function => self.frame.w * 3,
            _ => (1200.0 * self.set.turns + 400.0).min(40_000.0) as usize,
        }
    }

    /// One point of the curve, joined to the one before. Returns the work done.
    fn curve_step(&mut self) -> i64 {
        let n = self.samples();
        if self.pos > n {
            self.finish();
            return 1;
        }
        let k = self.pos as f64 / n as f64;
        let (x, y) = match self.mode {
            Mode::Function => {
                let x = self.frame.to_plane(k * self.frame.w as f64 - 0.5, 0.0).0;
                self.vars[X] = x;
                (x, self.progs[0].eval(&self.vars, self.seed))
            }
            Mode::Polar => {
                let th = k * self.set.turns * std::f64::consts::TAU;
                self.vars[THETA] = th;
                let r = self.progs[0].eval(&self.vars, self.seed);
                (r * th.cos(), r * th.sin())
            }
            _ => {
                self.vars[U] = k * self.set.turns * std::f64::consts::TAU;
                (self.progs[0].eval(&self.vars, self.seed), self.progs[1].eval(&self.vars, self.seed))
            }
        };
        let p = self.frame.to_pixel(x, y);
        // Far outside the canvas a point is not drawn (and the line to it is not either): that is
        // also how a jump to infinity (tan at pi/2) breaks the curve.
        let far = 4.0 * (self.frame.w + self.frame.h) as f64;
        let ok = p.0.is_finite() && p.1.is_finite() && p.0.abs() < far && p.1.abs() < far;
        if ok {
            let to = (p.0.round() as i64, p.1.round() as i64);
            match self.last {
                Some(q) if (q.1 - p.1).abs() < self.frame.h as f64 * 2.0 => self.raster.line((q.0.round() as i64, q.1.round() as i64), to, self.set.pen, INK),
                _ => self.raster.dot(to.0, to.1, self.set.pen, INK),
            }
        }
        self.last = ok.then_some(p);
        self.pos += 1;
        self.progs.len() as i64
    }

    // -------------------------------------------------------------------------- terrain

    fn terrain_step(&mut self) -> i64 {
        let (w, h) = (self.frame.w, self.frame.h);
        if self.pos >= w * h {
            self.pass += 1;
            self.pos = 0;
            if self.pass == 2 {
                self.finish();
            }
            return 1;
        }
        let (x, y) = (self.pos % w, self.pos / w);
        if self.pass == 0 {
            // Pass 1: the height of every pixel.
            let (px, py) = self.frame.to_plane(x as f64, y as f64);
            self.vars[X] = px;
            self.vars[Y] = py;
            let v = self.progs[0].eval(&self.vars, self.seed);
            self.heights[self.pos] = if v.is_finite() { v } else { -1e9 };
        } else {
            // Pass 2: light, sea and lines, from each pixel and its neighbours.
            let v = self.terrain_pixel(x, y);
            self.raster.px[self.pos] = v;
        }
        self.pos += 1;
        if self.pass == 0 { 2 } else { 1 }
    }

    fn height(&self, x: i64, y: i64) -> f64 {
        let (w, h) = (self.frame.w as i64, self.frame.h as i64);
        self.heights[(y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize]
    }

    fn terrain_pixel(&self, x: usize, y: usize) -> u8 {
        let (xi, yi) = (x as i64, y as i64);
        let h = self.height(xi, yi);
        let sea = self.set.sea;
        let wet = |hh: f64| hh < sea;
        if wet(h) {
            // Water: deeper is darker.
            let depth = sea - h;
            return tone(0.2 + 0.35 * (1.0 - (-depth * 4.0).exp()));
        }
        // The coast: land next to water.
        if wet(self.height(xi + 1, yi)) || wet(self.height(xi - 1, yi)) || wet(self.height(xi, yi + 1)) || wet(self.height(xi, yi - 1)) {
            return INK;
        }
        // Contour lines: where the height crosses a multiple of `lines` (above the sea).
        let c = self.set.contour;
        if c > 0.0 {
            let band = |hh: f64| ((hh - sea) / c).floor();
            if band(h) != band(self.height(xi + 1, yi)) || band(h) != band(self.height(xi, yi + 1)) {
                return AXIS;
            }
        }
        // Hill shading: the slope (in plane units, so zooming keeps the look, and softened so fine
        // noise does not turn into grit) against a light from the direction `sun` (0..1 = once
        // round) and 40 degrees up.
        let s = self.frame.scale() / RELIEF;
        let gx = (self.height(xi + 1, yi) - self.height(xi - 1, yi)) / (2.0 * s);
        let gy = (self.height(xi, yi - 1) - self.height(xi, yi + 1)) / (2.0 * s);
        let len = (gx * gx + gy * gy + 1.0).sqrt();
        let (nx, ny, nz) = (-gx / len, -gy / len, 1.0 / len);
        let az = self.set.sun * std::f64::consts::TAU + 2.4;
        let el = 40f64.to_radians();
        let (lx, ly, lz) = (az.cos() * el.cos(), az.sin() * el.cos(), el.sin());
        let lit = (nx * lx + ny * ly + nz * lz).max(0.0);
        // Flat land is a light tone; slopes away from the sun darken, slopes facing it go white.
        tone(0.12 + (lz - lit) * 2.2)
    }

    // -------------------------------------------------------------------------- voronoi

    fn voronoi_step(&mut self) -> i64 {
        let (w, h) = (self.frame.w, self.frame.h);
        if self.pos >= w * h {
            self.finish();
            return 1;
        }
        let (px, py) = self.frame.to_plane((self.pos % w) as f64, (self.pos / w) as f64);
        // The nearest point and the second nearest.
        let (mut d1, mut d2, mut v) = (f64::INFINITY, f64::INFINITY, 0.0);
        for &(sx, sy, sv) in &self.sites {
            let (dx, dy) = ((px - sx).abs(), (py - sy).abs());
            let d = match self.set.metric {
                1 => dx + dy,
                3 => dx.max(dy),
                _ => (dx * dx + dy * dy).sqrt(),
            };
            if d < d1 {
                d2 = d1;
                d1 = d;
                v = sv;
            } else if d < d2 {
                d2 = d;
            }
        }
        // On an edge when the two nearest points are about equally far.
        let edge = self.set.edge > 0.0 && d2 - d1 < self.set.edge * self.frame.scale();
        self.raster.px[self.pos] = if edge { INK } else { tone(v * 0.9) };
        self.pos += 1;
        1 + self.sites.len() as i64 / 16
    }

    // -------------------------------------------------------------------------- field

    /// Side of an occupancy cell: a stream line stops when it comes this close to another one.
    fn cell(&self) -> usize {
        (self.set.gap / 2).max(2)
    }

    /// The direction of the field at pixel (px, py), one pixel long, in pixel coordinates.
    fn flow(&mut self, px: f64, py: f64) -> Option<(f64, f64)> {
        let (x, y) = self.frame.to_plane(px, py);
        self.vars[X] = x;
        self.vars[Y] = y;
        let vx = self.progs[0].eval(&self.vars, self.seed);
        let vy = -self.progs[1].eval(&self.vars, self.seed);
        let len = (vx * vx + vy * vy).sqrt();
        (len.is_finite() && len > 1e-12).then(|| (vx / len, vy / len))
    }

    fn field_step(&mut self) -> i64 {
        let (w, h) = (self.frame.w, self.frame.h);
        if self.pass == 0 {
            // Pass 1: shade by strength (only when `shade` is up).
            if self.set.shade <= 0.0 || self.pos >= w * h {
                self.pass = 1;
                self.pos = 0;
                return 1;
            }
            let (x, y) = self.frame.to_plane((self.pos % w) as f64, (self.pos / w) as f64);
            self.vars[X] = x;
            self.vars[Y] = y;
            let m = self.progs[0].eval(&self.vars, self.seed).hypot(self.progs[1].eval(&self.vars, self.seed));
            self.raster.px[self.pos] = tone(self.set.shade * m / (m + 1.0));
            self.pos += 1;
            return 2;
        }
        // Pass 2: one stream line per start point, unless another line already passes nearby.
        if self.pos >= self.starts.len() {
            self.finish();
            return 1;
        }
        let id = self.pos as u32 + 1;
        let start = self.starts[self.pos];
        self.pos += 1;
        if self.owner_at(start).is_none_or(|o| o != 0) {
            return 1;
        }
        let mut work = 0;
        for dir in [1.0, -1.0] {
            work += self.trace(start, dir, id);
        }
        work
    }

    fn owner_at(&self, (x, y): (f64, f64)) -> Option<u32> {
        if x < 0.0 || y < 0.0 || x >= self.frame.w as f64 || y >= self.frame.h as f64 {
            return None;
        }
        let c = self.cell();
        let cols = self.frame.w.div_ceil(c);
        Some(self.owner[(y as usize / c) * cols + x as usize / c])
    }

    /// Follow the field from `p` (forwards or backwards) and draw the path. Midpoint steps of
    /// one pixel; the line stops at the edge, where the field vanishes, or near another line.
    fn trace(&mut self, mut p: (f64, f64), dir: f64, id: u32) -> i64 {
        let c = self.cell();
        let cols = self.frame.w.div_ceil(c);
        let max_steps = 2 * (self.frame.w + self.frame.h);
        let mut work = 0;
        for _ in 0..max_steps {
            match self.owner_at(p) {
                Some(o) if o == 0 || o == id => {
                    self.owner[(p.1 as usize / c) * cols + p.0 as usize / c] = id;
                }
                _ => break,
            }
            self.raster.set(p.0 as i64, p.1 as i64, INK);
            let Some(k1) = self.flow(p.0, p.1) else { break };
            let mid = (p.0 + dir * k1.0 * 0.5, p.1 + dir * k1.1 * 0.5);
            let Some(k2) = self.flow(mid.0, mid.1) else { break };
            p = (p.0 + dir * k2.0, p.1 + dir * k2.1);
            work += 4;
        }
        work
    }
}

#[cfg(test)]
mod tests {
    use super::super::presets;
    use super::*;

    fn finish(j: &mut Job) {
        for _ in 0..10_000 {
            if j.step(50_000) {
                return;
            }
        }
        panic!("the job never finished");
    }

    fn doc(text: &str) -> Doc {
        Doc::from_text(text).unwrap().0
    }

    #[test]
    fn plane_and_pixels_map_back_and_forth() {
        let f = Frame { w: 200, h: 100, view: View { cx: 1.0, cy: -2.0, span: 4.0 } };
        assert_eq!(f.scale(), 0.04);
        let (x, y) = f.to_plane(37.0, 81.0);
        let (px, py) = f.to_pixel(x, y);
        assert!((px - 37.0).abs() < 1e-9 && (py - 81.0).abs() < 1e-9);
        // The centre of the view is the centre of the raster; up is up.
        let (cx, cy) = f.to_pixel(1.0, -2.0);
        assert!((cx - 99.5).abs() < 1e-9 && (cy - 49.5).abs() < 1e-9);
        assert!(f.to_pixel(1.0, -1.0).1 < cy);
    }

    #[test]
    fn grid_steps_are_round_numbers() {
        assert_eq!(grid_step(0.01), 0.5);
        assert_eq!(grid_step(0.04), 2.0);
        assert_eq!(grid_step(1.0), 50.0);
        assert!((grid_step(0.0001) - 0.005).abs() < 1e-12);
    }

    #[test]
    fn a_line_y_equals_x_goes_corner_to_corner() {
        let d = doc("harbour-graph 1\nmode: function\ny = x\nview: 0 0 2\n");
        let mut j = Job::new(&d, 0.0, 100, 100).unwrap();
        finish(&mut j);
        let r = &j.raster;
        assert_eq!(r.get(50, 49), INK, "through the middle");
        assert_eq!(r.get(1, 98), INK, "bottom left");
        assert_eq!(r.get(98, 1), INK, "top right");
        assert_eq!(r.get(98, 98), 0, "nothing elsewhere");
        assert!(!r.px.contains(&GRID), "the grid is off");
    }

    #[test]
    fn the_grid_has_axes_and_dotted_lines() {
        let d = doc("harbour-graph 1\nmode: function\ny = 100\nview: 0 0 4\ngrid: on\n");
        let mut j = Job::new(&d, 0.0, 128, 128).unwrap();
        finish(&mut j);
        let r = &j.raster;
        // y = 100 is far above: nothing but the grid is drawn.
        assert!(!r.px.contains(&INK));
        let col = (0..128).find(|&x| r.get(x, 10) == AXIS && r.get(x, 11) == AXIS).unwrap();
        assert!((63..=64).contains(&col), "the y axis at the centre: {col}");
        assert!(r.px.contains(&GRID));
    }

    #[test]
    fn a_circle_in_polar_stays_on_its_radius() {
        let d = doc("harbour-graph 1\nmode: polar\nr = 1\nview: 0 0 3\n");
        let f = Frame { w: 90, h: 90, view: d.view };
        let mut j = Job::new(&d, 0.0, 90, 90).unwrap();
        finish(&mut j);
        let mut inked = 0;
        for y in 0..90 {
            for x in 0..90 {
                if j.raster.get(x, y) == INK {
                    inked += 1;
                    let (px, py) = f.to_plane(x as f64, y as f64);
                    assert!((px.hypot(py) - 1.0).abs() < 2.0 * f.scale(), "({x}, {y}) is off the circle");
                }
            }
        }
        // About the circumference in pixels (2 pi 30).
        assert!((150..260).contains(&inked), "{inked}");
    }

    #[test]
    fn breaks_are_not_joined() {
        // tan jumps from +inf to -inf: no vertical line through the whole canvas.
        let d = doc("harbour-graph 1\nmode: function\ny = tan(x)\nview: 0 0 6\n");
        let mut j = Job::new(&d, 0.0, 120, 120).unwrap();
        finish(&mut j);
        let full = (0..120).any(|x| (0..120).all(|y| j.raster.get(x, y) == INK));
        assert!(!full);
    }

    #[test]
    fn terrain_has_sea_land_and_a_coast() {
        let d = doc("harbour-graph 1\nmode: terrain\nh = 1 - hypot(x, y)\nparam: lines 0 0 1\nview: 0 0 4\n");
        let mut j = Job::new(&d, 0.0, 80, 80).unwrap();
        finish(&mut j);
        let r = &j.raster;
        let corner = r.get(0, 0);
        assert!(corner > tone(0.19) && corner <= SHADE, "deep water in the corner: {corner}");
        assert!(r.get(40, 40) < tone(0.2), "light land in the middle: {}", r.get(40, 40));
        // Along the middle row: land, then the coast, then water.
        let row: Vec<u8> = (40..80).map(|x| r.get(x, 40)).collect();
        assert!(row.contains(&INK), "{row:?}");
    }

    #[test]
    fn voronoi_cells_take_their_points_shade() {
        let d = doc("harbour-graph 1\nmode: voronoi\nx = if(i < 1, -1, 1)\ny = 0\nv = i\nparam: n 2 2 300 int\nparam: edge 1 0 4\nview: 0 0 4\n");
        let mut j = Job::new(&d, 0.0, 100, 60).unwrap();
        finish(&mut j);
        let r = &j.raster;
        assert_eq!(r.get(10, 30), tone(0.0), "left cell: point 0");
        assert_eq!(r.get(90, 30), tone(0.9), "right cell: point 1");
        assert!((0..60).all(|y| (48..52).any(|x| r.get(x, y) == INK)), "an edge down the middle");
    }

    #[test]
    fn a_circular_field_draws_lines_that_do_not_crowd() {
        let d = doc("harbour-graph 1\nmode: field\nvx = -y\nvy = x\nparam: gap 10 4 30 int\nparam: shade 0 0 1\nview: 0 0 4\n");
        let mut j = Job::new(&d, 0.0, 100, 100).unwrap();
        finish(&mut j);
        let inked = j.raster.px.iter().filter(|v| **v == INK).count();
        assert!(inked > 300, "lines were drawn: {inked}");
        assert!(inked < 100 * 100 / 3, "but not everywhere: {inked}");
    }

    #[test]
    fn every_preset_finishes_and_draws_something() {
        for (i, (name, _)) in presets::PRESETS.iter().enumerate() {
            let (d, t) = presets::load(i);
            let mut j = Job::new(&d, t + 0.5, 120, 80).unwrap();
            finish(&mut j);
            let first = j.raster.px[0];
            assert!(j.raster.px.iter().any(|v| *v != first), "{name} is blank");
            assert_eq!(j.progress(), 1.0);
        }
    }

    #[test]
    fn small_budgets_reach_the_same_picture() {
        let (d, t) = presets::load(presets::first_of(Mode::Terrain));
        let mut a = Job::new(&d, t, 60, 40).unwrap();
        finish(&mut a);
        let mut b = Job::new(&d, t, 60, 40).unwrap();
        let mut steps = 0;
        let mut last = 0.0;
        while !b.step(37) {
            steps += 1;
            assert!(b.progress() >= last, "progress only grows");
            last = b.progress();
        }
        assert!(steps > 10);
        assert_eq!(a.raster, b.raster);
    }

    #[test]
    fn a_bad_formula_names_its_slot() {
        let mut d = doc("harbour-graph 1\nmode: parametric\nx = sin(u)\ny = cos(u)\n");
        d.formulas[1] = "cos(u".into();
        assert_eq!(Job::new(&d, 0.0, 10, 10).err().map(|e| e.0), Some(1));
    }
}
