//! Ready-made pictures to start from. Each one is the text of a `.graph` file, so what the editor
//! shows for a preset is exactly what is written here.

use super::doc::{Doc, Mode};

/// (name, file text). The first one is what Graph opens with.
pub const PRESETS: &[(&str, &str)] = &[
    (
        "Rose",
        "harbour-graph 1
mode: polar
r = cos(k*theta)
param: k 5 1 12 int
view: 0 0 2.4
grid: on",
    ),
    (
        "Butterfly",
        "harbour-graph 1
mode: polar
r = exp(sin(theta)) - 2cos(4theta) + sin((2theta - pi)/24)^5
param: turns 12 1 40 int
view: 0 0.6 8.4",
    ),
    (
        "Spiral",
        "harbour-graph 1
mode: polar
r = a*theta + w*sin(k*theta + t)
param: a 0.1 0 0.5
param: w 0.15 0 1
param: k 12 0 40 int
param: turns 5 1 40 int
view: 0 0 7",
    ),
    (
        "Lissajous",
        "harbour-graph 1
mode: parametric
x = sin(a*u + t)
y = sin(b*u)
param: a 3 1 12 int
param: b 4 1 12 int
view: 0 0 2.4
grid: on",
    ),
    (
        "Spirograph",
        "harbour-graph 1
mode: parametric
x = (R - r)cos(u) + d*cos((R - r)/r*u)
y = (R - r)sin(u) - d*sin((R - r)/r*u)
param: R 10 1 12 int
param: r 3 1 12 int
param: d 7 0 12
param: turns 3 1 40 int
view: 0 0 30",
    ),
    (
        "Harmonograph",
        "harbour-graph 1
mode: parametric
x = exp(-k*u)*sin(a*u + t)
y = exp(-k*u)*sin(b*u)
param: a 2 1 6
param: b 3.02 1 6
param: k 0.02 0 0.1
param: turns 12 1 40 int
view: 0 0 2.4
time: 0 0.3",
    ),
    (
        "Waves",
        "harbour-graph 1
mode: function
y = a*sin(b*x - t)*cos(x/c)
param: a 1.5 -3 3
param: b 3 0 10
param: c 2 0.5 5
view: 0 0 6
grid: on",
    ),
    (
        "Wave packet",
        "harbour-graph 1
mode: function
y = exp(-(x - s)^2/w)*cos(k*(x - t))
param: s 0 -5 5
param: w 2 0.1 10
param: k 6 0 20
param: pen 2 1 4 int
view: 0 0 4
grid: on",
    ),
    (
        "Island",
        "harbour-graph 1
mode: terrain
h = fbm(f*x, f*y, 5) + lift - fall*(x^2 + y^2)
param: f 1.2 0.2 4
param: lift 0.35 -1 1
param: fall 0.12 0 0.5
param: sea 0 -1 1
param: lines 0.2 0 0.5
param: sun 0.3 0 1
seed: 7
view: 0 0 6",
    ),
    (
        "Fjords",
        "harbour-graph 1
mode: terrain
h = lift - 2abs(fbm(f*x, f*y, 5))
param: f 0.8 0.2 4
param: lift 0.35 -1 1
param: sea 0 -1 1
param: lines 0 0 0.5
param: sun 0.6 0 1
seed: 3
view: 0 0 6",
    ),
    (
        "Tide",
        "harbour-graph 1
mode: terrain
h = fbm(x, y, 5) + a*sin(t)
param: a 0.12 0 0.5
param: sea 0 -1 1
param: lines 0.15 0 0.5
param: sun 0.3 0 1
seed: 11
view: 0 0 5
time: 0 1",
    ),
    (
        "Mosaic",
        "harbour-graph 1
mode: voronoi
x = 3rand(i) - 1.5
y = 2rand(i + 0.5) - 1
v = rand(i + 0.25)
param: n 60 2 300 int
param: edge 1 0 4
param: norm 2 1 3 int
seed: 5
view: 0 0 2",
    ),
    (
        "Drift",
        "harbour-graph 1
mode: voronoi
x = 3rand(i) - 1.5 + w*sin(t + 6rand(i + 0.3))
y = 2rand(i + 0.5) - 1 + w*cos(0.7t + 6rand(i + 0.7))
v = i/n
param: w 0.2 0 1
param: n 30 2 300 int
param: edge 1 0 4
param: norm 2 1 3 int
seed: 2
view: 0 0 2",
    ),
    (
        "Sunflower",
        "harbour-graph 1
mode: voronoi
x = sqrt(i/n)*cos(g*i)
y = sqrt(i/n)*sin(g*i)
v = i/n
param: g 2.39996 2 3
param: n 200 2 300 int
param: edge 1 0 4
param: norm 2 1 3 int
view: 0 0 2.2",
    ),
    (
        "Vortex",
        "harbour-graph 1
mode: field
vx = a*x - y
vy = x + a*y
param: a -0.2 -1 1
param: gap 10 4 30 int
param: shade 0.4 0 1
view: 0 0 4",
    ),
    (
        "Dipole",
        "harbour-graph 1
mode: field
vx = (x - 1)/hypot(x - 1, y)^3 - q*(x + 1)/hypot(x + 1, y)^3
vy = y/hypot(x - 1, y)^3 - q*y/hypot(x + 1, y)^3
param: q 1 -1 1
param: gap 8 4 30 int
param: shade 0.3 0 1
view: 0 0 5",
    ),
    (
        "Wind",
        "harbour-graph 1
mode: field
vx = cos(k*pi*fbm(f*x, f*y + 0.1t, 3))
vy = sin(k*pi*fbm(f*x, f*y + 0.1t, 3))
param: k 2 0 6
param: f 0.7 0.1 3
param: gap 8 4 30 int
param: shade 0 0 1
seed: 4
view: 0 0 5",
    ),
];

/// Preset `i` as a document (its name is the title) and the time it starts at.
pub fn load(i: usize) -> (Doc, f64) {
    let (name, text) = PRESETS[i.min(PRESETS.len() - 1)];
    let (mut d, t) = Doc::from_text(text).expect("presets are valid");
    d.title = name.to_string();
    (d, t)
}

/// The first preset of a mode (what switching to the mode shows).
pub fn first_of(mode: Mode) -> usize {
    (0..PRESETS.len()).find(|&i| load(i).0.mode == mode).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_reads_and_compiles() {
        for (i, (name, _)) in PRESETS.iter().enumerate() {
            let (d, _) = load(i);
            assert_eq!(&d.title, name);
            assert!(d.compile().is_ok(), "{name}: {:?}", d.compile().err());
            // The file names every slider it uses, so nothing starts at the default 1 by accident,
            // and every slider stays inside its range.
            for p in &d.params {
                assert!(p.min <= p.value && p.value <= p.max, "{name}: {p:?}");
            }
            // Saving and reading back gives the same document.
            let (back, _) = Doc::from_text(&d.to_text(0.0)).unwrap();
            assert_eq!(back.formulas, d.formulas, "{name}");
            assert_eq!(back.params, d.params, "{name}");
        }
    }

    #[test]
    fn every_mode_has_a_preset() {
        for m in Mode::ALL {
            assert_eq!(load(first_of(m)).0.mode, m, "{m:?}");
        }
        assert!(PRESETS.iter().all(|(n, _)| n.len() <= 14), "names fit the panel");
    }
}
