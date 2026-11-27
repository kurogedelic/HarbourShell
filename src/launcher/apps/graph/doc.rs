//! A Graph document: the drawing mode, its formulas, the parameters (one slider each), the random
//! seed and the view. It is what Undo / Redo step through and what a `.graph` file holds.
//!
//! A `.graph` file is plain text, one thing per line, so it can be read and changed in the Text
//! Editor too:
//!
//! ```text
//! harbour-graph 1
//! title: Rose
//! mode: polar
//! r = cos(k*theta)
//! param: k 5 1 12 int
//! seed: 1
//! view: 0 0 2.6
//! grid: on
//! time: 0 1
//! ```
//!
//! `name = formula` lines are the formulas; `key: value` lines are everything else. Unknown keys are
//! skipped, so a newer file still opens.

use super::expr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// y = f(x)
    Function,
    /// r = f(theta)
    Polar,
    /// x = f(u), y = g(u)
    Parametric,
    /// A height h(x, y): land and sea, shaded, with contour lines.
    Terrain,
    /// Cells around n points.
    Voronoi,
    /// Arrows (vx, vy) at every point, drawn as stream lines.
    Field,
}

/// A slider the renderer itself reads (not a name in a formula): its default and range.
#[derive(Clone, Copy, Debug)]
pub struct Setting {
    pub name: &'static str,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub int: bool,
}

const fn s(name: &'static str, value: f64, min: f64, max: f64, int: bool) -> Setting {
    Setting { name, value, min, max, int }
}

impl Mode {
    pub const ALL: [Mode; 6] = [Mode::Function, Mode::Polar, Mode::Parametric, Mode::Terrain, Mode::Voronoi, Mode::Field];

    /// The name in files.
    pub fn id(self) -> &'static str {
        match self {
            Mode::Function => "function",
            Mode::Polar => "polar",
            Mode::Parametric => "parametric",
            Mode::Terrain => "terrain",
            Mode::Voronoi => "voronoi",
            Mode::Field => "field",
        }
    }

    /// The name people see (English; translated when drawn).
    pub fn name(self) -> &'static str {
        match self {
            Mode::Function => "Function plot",
            Mode::Polar => "Polar",
            Mode::Parametric => "Parametric",
            Mode::Terrain => "Terrain",
            Mode::Voronoi => "Voronoi",
            Mode::Field => "Vector field",
        }
    }

    pub fn from_id(id: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.id() == id)
    }

    /// The formulas this mode draws from, by the name on the left of `=`.
    pub fn slots(self) -> &'static [&'static str] {
        match self {
            Mode::Function => &["y"],
            Mode::Polar => &["r"],
            Mode::Parametric => &["x", "y"],
            Mode::Terrain => &["h"],
            Mode::Voronoi => &["x", "y", "v"],
            Mode::Field => &["vx", "vy"],
        }
    }

    /// What each formula is, for the editor.
    pub fn slot_hint(self, slot: &str) -> &'static str {
        match (self, slot) {
            (Mode::Function, _) => "Height of the curve at x",
            (Mode::Polar, _) => "Distance from the centre at angle theta",
            (Mode::Parametric, "x") => "Across, as u goes round",
            (Mode::Parametric, _) => "Up, as u goes round",
            (Mode::Terrain, _) => "Height of the land at x, y (sea below 0)",
            (Mode::Voronoi, "x") => "Across: where point i sits",
            (Mode::Voronoi, "y") => "Up: where point i sits",
            (Mode::Voronoi, _) => "Shade of cell i, 0 to 1",
            (Mode::Field, "vx") => "Push across at x, y",
            (Mode::Field, _) => "Push up at x, y",
        }
    }

    /// The variables its formulas may read (besides the parameters).
    pub fn vars(self) -> &'static [&'static str] {
        match self {
            Mode::Function => &["x", "t"],
            Mode::Polar => &["theta", "t"],
            Mode::Parametric => &["u", "t"],
            Mode::Terrain | Mode::Field => &["x", "y", "t"],
            Mode::Voronoi => &["i", "n", "t"],
        }
    }

    /// The sliders the drawing itself reads.
    pub fn settings(self) -> &'static [Setting] {
        const CURVE: [Setting; 1] = [s("pen", 1.0, 1.0, 4.0, true)];
        const ROUND: [Setting; 2] = [s("turns", 1.0, 1.0, 40.0, true), s("pen", 1.0, 1.0, 4.0, true)];
        const TERRAIN: [Setting; 3] = [s("sea", 0.0, -1.0, 1.0, false), s("lines", 0.1, 0.0, 0.5, false), s("sun", 0.3, 0.0, 1.0, false)];
        const VORONOI: [Setting; 3] = [s("n", 40.0, 2.0, 300.0, true), s("edge", 1.0, 0.0, 4.0, false), s("norm", 2.0, 1.0, 3.0, true)];
        const FIELD: [Setting; 2] = [s("gap", 10.0, 4.0, 30.0, true), s("shade", 0.5, 0.0, 1.0, false)];
        match self {
            Mode::Function => &CURVE,
            Mode::Polar | Mode::Parametric => &ROUND,
            Mode::Terrain => &TERRAIN,
            Mode::Voronoi => &VORONOI,
            Mode::Field => &FIELD,
        }
    }
}

/// Every variable any mode has, in slot order: a compiled formula reads variable `VARS[k]` from
/// slot `k` and parameter `j` from slot `VARS.len() + j`.
pub const VARS: [&str; 7] = ["x", "y", "t", "theta", "u", "i", "n"];

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    /// Whole numbers only.
    pub int: bool,
}

impl Param {
    pub fn new(name: &str, value: f64, min: f64, max: f64, int: bool) -> Param {
        let mut p = Param { name: name.to_string(), value, min, max, int };
        p.set(value);
        p
    }

    /// A slider for a new name in a formula: 1, from -5 to 5.
    pub fn fresh(name: &str) -> Param {
        Param::new(name, 1.0, -5.0, 5.0, false)
    }

    pub fn set(&mut self, v: f64) {
        let v = if v.is_finite() { v.clamp(self.min.min(self.max), self.max.max(self.min)) } else { self.min };
        self.value = if self.int { v.round() } else { v };
    }

    /// Where the value sits on the slider, 0..=1.
    pub fn frac(&self) -> f64 {
        if self.max > self.min { ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0) } else { 0.0 }
    }

    pub fn set_frac(&mut self, f: f64) {
        self.set(self.min + (self.max - self.min) * f.clamp(0.0, 1.0));
    }

    /// One keyboard step: a hundredth of the range (a tenth with `coarse`), at least 1 for whole numbers.
    pub fn nudge(&mut self, dir: i32, coarse: bool) {
        let step = (self.max - self.min) / if coarse { 10.0 } else { 100.0 };
        let step = if self.int { step.round().max(1.0) } else { step };
        self.set(self.value + step * dir as f64);
    }
}

/// A number for a slider or a file: short, no trailing zeros.
pub fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return "?".into();
    }
    let s = if v.fract() == 0.0 && v.abs() < 1e9 {
        format!("{}", v as i64)
    } else if v.abs() >= 100.0 {
        format!("{v:.0}")
    } else if v.abs() >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    };
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

/// A CSV of numbers (first column across, second up; a header line and rows without a value are
/// skipped) as the text of a `.graph` file: a function plot through the points. The line is a sum
/// of hinges, `y0 + s0*(x-x0) + (s1-s0)*max(0, x-x1) + ...`, so uneven spacing works; at most
/// `CSV_POINTS` points are kept (every n-th). Sensor logs from Hardware are opened with it.
pub fn csv_to_graph(text: &str, title: &str) -> Result<String, String> {
    const CSV_POINTS: usize = 150;
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut f = l.split(',');
        let (Some(a), Some(b)) = (f.next(), f.next()) else { continue };
        if let (Ok(x), Ok(y)) = (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
            if x.is_finite() && y.is_finite() && pts.last().map_or(true, |p| x > p.0) {
                pts.push((x, y));
            }
        }
    }
    if pts.len() < 2 {
        return Err("Not enough numbers in the file".into());
    }
    if pts.len() > CSV_POINTS {
        let n = pts.len();
        pts = (0..CSV_POINTS).map(|i| pts[i * (n - 1) / (CSV_POINTS - 1)]).collect();
        pts.dedup_by(|b, a| b.0 <= a.0);
    }
    let slope = |i: usize| (pts[i + 1].1 - pts[i].1) / (pts[i + 1].0 - pts[i].0);
    let mut f = format!("{} + {}*(x - {})", fmt_full(pts[0].1), fmt_full(slope(0)), fmt_full(pts[0].0));
    for i in 1..pts.len() - 1 {
        let d = slope(i) - slope(i - 1);
        if d.abs() < 1e-12 {
            continue;
        }
        f += &format!(" {} {}*max(0, x - {})", if d < 0.0 { "-" } else { "+" }, fmt_full(d.abs()), fmt_full(pts[i].0));
    }
    let (x0, x1) = (pts[0].0, pts[pts.len() - 1].0);
    let (ylo, yhi) = pts.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let span = ((yhi - ylo) * 1.3).max((x1 - x0) / 2.0 * 1.1).max(1e-3);
    let mut o = String::from("harbour-graph 1\n");
    o += &format!("title: {}\n", title.replace('\n', " "));
    o += "mode: function\n";
    o += &format!("y = {f}\n");
    o += "seed: 1\n";
    o += &format!("view: {} {} {}\n", fmt_full((x0 + x1) / 2.0), fmt_full((ylo + yhi) / 2.0), fmt_full(span));
    o += "grid: on\n";
    o += "time: 0 1\n";
    Ok(o)
}

/// The window onto the plane: its centre and how much of the plane the canvas's shorter side shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub cx: f64,
    pub cy: f64,
    pub span: f64,
}

impl Default for View {
    fn default() -> Self {
        View { cx: 0.0, cy: 0.0, span: 4.0 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    pub title: String,
    pub mode: Mode,
    /// One per [`Mode::slots`], in that order.
    pub formulas: Vec<String>,
    pub params: Vec<Param>,
    pub seed: u32,
    pub view: View,
    pub grid: bool,
    /// How fast `t` runs (1 = one unit a second).
    pub speed: f64,
}

impl Doc {
    pub fn param(&self, name: &str) -> Option<f64> {
        self.params.iter().find(|p| p.name == name).map(|p| p.value)
    }

    /// The value of a renderer setting (its default when missing).
    pub fn setting(&self, name: &str) -> f64 {
        self.param(name).or_else(|| self.mode.settings().iter().find(|s| s.name == name).map(|s| s.value)).unwrap_or(0.0)
    }

    /// True when a name is the mode's variable or a constant: formulas may use it without a slider.
    pub fn is_var(&self, name: &str) -> bool {
        self.mode.vars().contains(&name)
    }

    /// Make the sliders match the formulas: one for every free name (new ones start at 1, from -5
    /// to 5), plus the mode's settings; sliders nobody uses any more go. Kept sliders keep their value.
    pub fn sync_params(&mut self) {
        let mut wanted: Vec<String> = Vec::new();
        for f in &self.formulas {
            for n in expr::free_names(f, |n| self.is_var(n)) {
                if !wanted.contains(&n) {
                    wanted.push(n);
                }
            }
        }
        let mut out: Vec<Param> = Vec::new();
        for n in &wanted {
            let p = self.params.iter().find(|p| &p.name == n).cloned().unwrap_or_else(|| match self.mode.settings().iter().find(|s| s.name == n) {
                Some(s) => Param::new(s.name, s.value, s.min, s.max, s.int),
                None => Param::fresh(n),
            });
            out.push(p);
        }
        for s in self.mode.settings() {
            if !out.iter().any(|p| p.name == s.name) {
                out.push(self.params.iter().find(|p| p.name == s.name).cloned().unwrap_or_else(|| Param::new(s.name, s.value, s.min, s.max, s.int)));
            }
        }
        self.params = out;
    }

    /// True when a formula reads `t` (then the picture moves while playing).
    pub fn uses_time(&self) -> bool {
        self.formulas.iter().any(|f| expr::free_names(f, |_| false).iter().any(|n| n == "t"))
    }

    /// The slot a name reads from in a compiled formula, for this document's parameters.
    pub fn slot_of(&self, name: &str) -> Option<usize> {
        if self.is_var(name) {
            return VARS.iter().position(|v| *v == name);
        }
        self.params.iter().position(|p| p.name == name).map(|j| VARS.len() + j)
    }

    /// The variable slots with the parameters filled in (the variables are 0 until the renderer
    /// sets them).
    pub fn base_vars(&self, t: f64) -> Vec<f64> {
        let mut v = vec![0.0; VARS.len()];
        v[2] = t;
        v.extend(self.params.iter().map(|p| p.value));
        v
    }

    /// Compile every formula; the first one that fails gives its slot index and the problem.
    pub fn compile(&self) -> Result<Vec<expr::Prog>, (usize, expr::Error)> {
        self.formulas.iter().enumerate().map(|(i, f)| expr::compile(f, &|n| self.slot_of(n)).map_err(|e| (i, e))).collect()
    }

    // -------------------------------------------------------------------------- files

    pub fn to_text(&self, t: f64) -> String {
        let mut o = String::from("harbour-graph 1\n");
        o += &format!("title: {}\n", self.title);
        o += &format!("mode: {}\n", self.mode.id());
        for (slot, f) in self.mode.slots().iter().zip(&self.formulas) {
            o += &format!("{slot} = {f}\n");
        }
        for p in &self.params {
            o += &format!("param: {} {} {} {}{}\n", p.name, fmt_full(p.value), fmt_full(p.min), fmt_full(p.max), if p.int { " int" } else { "" });
        }
        o += &format!("seed: {}\n", self.seed);
        o += &format!("view: {} {} {}\n", fmt_full(self.view.cx), fmt_full(self.view.cy), fmt_full(self.view.span));
        o += &format!("grid: {}\n", if self.grid { "on" } else { "off" });
        o += &format!("time: {} {}\n", fmt_full(t), fmt_full(self.speed));
        o
    }

    /// Read a `.graph` file: the document and the time it was saved at.
    pub fn from_text(text: &str) -> Result<(Doc, f64), String> {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
        match lines.next() {
            Some(l) if l.starts_with("harbour-graph") => {}
            _ => return Err("Not a Graph file".into()),
        }
        let mut mode = None;
        let mut title = String::from("Untitled");
        let mut formulas: Vec<(String, String)> = Vec::new();
        let mut params = Vec::new();
        let (mut seed, mut view, mut grid, mut t, mut speed) = (1u32, View::default(), false, 0.0, 1.0);
        let num = |s: &str| s.parse::<f64>().ok().filter(|v| v.is_finite());
        for l in lines {
            let colon = l.find(':');
            let eq = l.find('=');
            let is_setting = match (colon, eq) {
                (Some(c), Some(e)) => c < e,
                (Some(_), None) => true,
                _ => false,
            };
            if !is_setting {
                if let Some(e) = eq {
                    formulas.push((l[..e].trim().to_string(), l[e + 1..].trim().to_string()));
                }
                continue;
            }
            let (key, val) = l.split_once(':').unwrap_or((l, ""));
            let val = val.trim();
            let w: Vec<&str> = val.split_whitespace().collect();
            match key.trim() {
                "title" => title = val.to_string(),
                "mode" => mode = Mode::from_id(val),
                "seed" => seed = val.parse().unwrap_or(1),
                "grid" => grid = val == "on",
                "view" if w.len() == 3 => {
                    if let (Some(cx), Some(cy), Some(span)) = (num(w[0]), num(w[1]), num(w[2])) {
                        view = View { cx, cy, span: span.clamp(1e-6, 1e6) };
                    }
                }
                "time" => {
                    t = w.first().and_then(|v| num(v)).unwrap_or(0.0);
                    speed = w.get(1).and_then(|v| num(v)).unwrap_or(1.0);
                }
                "param" if w.len() >= 4 => {
                    if let (Some(v), Some(lo), Some(hi)) = (num(w[1]), num(w[2]), num(w[3])) {
                        if expr::parse(w[0]).is_ok_and(|n| matches!(n, expr::Node::Name(..))) {
                            params.push(Param::new(w[0], v, lo, hi, w.get(4) == Some(&"int")));
                        }
                    }
                }
                _ => {}
            }
        }
        let mode = mode.ok_or("The file names no drawing mode")?;
        let formulas = mode
            .slots()
            .iter()
            .map(|slot| formulas.iter().find(|(n, _)| n == slot).map(|(_, f)| f.clone()).unwrap_or_else(|| "0".into()))
            .collect();
        let mut d = Doc { title, mode, formulas, params, seed, view, grid, speed };
        d.sync_params();
        Ok((d, t))
    }
}

/// A number written so it reads back the same.
fn fmt_full(v: f64) -> String {
    let s = format!("{v}");
    if s.parse::<f64>() == Ok(v) { s } else { format!("{v:e}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rose() -> Doc {
        let (mut d, _) = Doc::from_text("harbour-graph 1\nmode: polar\nr = cos(k*theta)\n").unwrap();
        d.title = "Rose".into();
        d
    }

    #[test]
    fn free_names_get_sliders_and_settings_stay() {
        let mut d = rose();
        let names: Vec<_> = d.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["k", "turns", "pen"]);
        assert_eq!(d.param("k"), Some(1.0));
        d.params[0].set(4.0);
        d.formulas[0] = "a + cos(k*theta) * t".into();
        d.sync_params();
        let names: Vec<_> = d.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a", "k", "turns", "pen"], "t is a variable, not a slider");
        assert_eq!(d.param("k"), Some(4.0), "a kept slider keeps its value");
        d.formulas[0] = "theta".into();
        d.sync_params();
        assert_eq!(d.params.len(), 2, "unused sliders go");
        assert!(d.uses_time() == false);
        d.formulas[0] = "sin(theta + t)".into();
        assert!(d.uses_time());
    }

    #[test]
    fn a_setting_named_in_a_formula_keeps_its_range() {
        let (d, _) = Doc::from_text("harbour-graph 1\nmode: terrain\nh = fbm(x, y, 4) - sea\n").unwrap();
        let sea = d.params.iter().find(|p| p.name == "sea").unwrap();
        assert_eq!((sea.min, sea.max), (-1.0, 1.0));
        assert_eq!(d.params.iter().filter(|p| p.name == "sea").count(), 1);
    }

    #[test]
    fn files_round_trip() {
        let mut d = rose();
        d.params[0] = Param::new("k", 2.5, 0.0, 12.0, false);
        d.seed = 99;
        d.view = View { cx: 0.25, cy: -1.0 / 3.0, span: 3.5 };
        d.grid = true;
        d.speed = 0.5;
        let text = d.to_text(1.75);
        assert!(text.contains("r = cos(k*theta)\n"), "{text}");
        assert!(text.contains("param: k 2.5 0 12\n"), "{text}");
        assert!(text.contains("param: turns 1 1 40 int\n"), "{text}");
        let (back, t) = Doc::from_text(&text).unwrap();
        assert_eq!(back, d);
        assert_eq!(t, 1.75);
    }

    #[test]
    fn odd_files_are_read_carefully() {
        assert!(Doc::from_text("hello").is_err());
        assert!(Doc::from_text("harbour-graph 1\ntitle: x\n").is_err(), "no mode");
        let (d, t) = Doc::from_text("harbour-graph 1\nmode: parametric\nx = sin(u)\nfuture: thing\nparam: 2bad 1 0 1\nparam: k nope 0 1\nview: 1 2\n").unwrap();
        assert_eq!(d.formulas, ["sin(u)", "0"], "a missing formula is 0");
        assert!(d.params.iter().all(|p| p.name != "2bad" && p.name != "k"));
        assert_eq!(d.view, View::default());
        assert_eq!(t, 0.0);
        // A title with a colon or an equals sign is still a title.
        let (d, _) = Doc::from_text("harbour-graph 1\ntitle: a=b: c\nmode: function\ny = x\n").unwrap();
        assert_eq!(d.title, "a=b: c");
        assert_eq!(d.formulas, ["x"]);
    }

    #[test]
    fn sliders_clamp_round_and_step() {
        let mut p = Param::new("n", 5.0, 2.0, 300.0, true);
        p.set(7.6);
        assert_eq!(p.value, 8.0);
        p.set(1e9);
        assert_eq!(p.value, 300.0);
        p.set(f64::NAN);
        assert_eq!(p.value, 2.0);
        p.nudge(1, false);
        assert_eq!(p.value, 5.0, "a hundredth of 298, rounded");
        let mut q = Param::new("a", 0.0, -5.0, 5.0, false);
        q.nudge(1, false);
        assert!((q.value - 0.1).abs() < 1e-12);
        q.nudge(-1, true);
        assert!((q.value + 0.9).abs() < 1e-12);
        q.set_frac(1.0);
        assert_eq!(q.value, 5.0);
        assert_eq!(q.frac(), 1.0);
    }

    #[test]
    fn numbers_are_short() {
        assert_eq!(fmt_num(5.0), "5");
        assert_eq!(fmt_num(0.5), "0.5");
        assert_eq!(fmt_num(-0.126), "-0.13");
        assert_eq!(fmt_num(12.34), "12.3");
        assert_eq!(fmt_num(123.4), "123");
        assert_eq!(fmt_num(f64::NAN), "?");
        assert_eq!(fmt_full(0.1), "0.1");
    }

    #[test]
    fn compiled_formulas_read_their_sliders() {
        let mut d = rose();
        d.params[0].set(3.0);
        let progs = d.compile().unwrap();
        let mut vars = d.base_vars(0.0);
        vars[3] = 0.0; // theta
        assert!((progs[0].eval(&vars, d.seed) - 1.0).abs() < 1e-12);
        vars[3] = std::f64::consts::PI / 3.0;
        assert!((progs[0].eval(&vars, d.seed) + 1.0).abs() < 1e-12, "cos(3 * pi/3) = -1");
        d.formulas[0] = "cos(".into();
        assert_eq!(d.compile().unwrap_err().0, 0);
    }

    #[test]
    fn a_csv_becomes_a_function_plot_through_its_points() {
        let text = "time_s,temp\n0.0,20\n5.0,22\n10.0,22\n15.0,\n20.0,18\n";
        let g = csv_to_graph(text, "Demo").unwrap();
        let (d, _) = Doc::from_text(&g).unwrap();
        assert_eq!(d.mode, Mode::Function);
        assert_eq!(d.title, "Demo");
        let prog = expr::compile(&d.formulas[0], &|n| (n == "x").then_some(0)).unwrap();
        let at = |x: f64| prog.eval(&[x], 1);
        for (x, y) in [(0.0, 20.0), (5.0, 22.0), (10.0, 22.0), (20.0, 18.0), (15.0, 20.0)] {
            assert!((at(x) - y).abs() < 1e-6, "y({x}) = {} wanted {y}", at(x));
        }
        assert!(csv_to_graph("time_s,v\n1,2\n", "x").is_err());
        assert!(csv_to_graph("hello", "x").is_err());
    }

    #[test]
    fn a_long_csv_is_thinned() {
        let text: String = std::iter::once("t,v\n".to_string()).chain((0..1000).map(|i| format!("{i},{}\n", (i as f64 / 30.0).sin()))).collect();
        let g = csv_to_graph(&text, "Long").unwrap();
        assert!(g.matches("max(").count() <= 150);
        assert!(Doc::from_text(&g).is_ok());
    }
}
