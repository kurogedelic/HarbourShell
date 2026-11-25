//! 1-bit style dither fills (the 1-bit way to get "grays").
//!
//! Patterns are defined on absolute screen coordinates, so two dithered areas
//! that touch line up without a seam.

use crate::gfx::Color;
use crate::gfx::{Point, Rect};
use crate::gfx::Canvas;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// 12.5%: two pixels in every 4x4 block, offset like a diagonal grid.
    Eighth,
    /// 25%: every other pixel on every other row.
    Quarter,
    /// 50%: checkerboard.
    Half,
    /// 75%: the complement of Quarter.
    ThreeQuarter,
    /// Horizontal scanlines: every other row.
    Scan,
    /// Vertical lines: every other column.
    Bars,
}

impl Pattern {
    /// Is the pixel at absolute (x, y) lit in this pattern?
    pub fn lit(self, x: i32, y: i32) -> bool {
        let (x, y) = (x.rem_euclid(4), y.rem_euclid(4));
        match self {
            Pattern::Eighth => (x == 0 && y == 0) || (x == 2 && y == 2),
            Pattern::Quarter => x % 2 == 0 && y % 2 == 0,
            Pattern::Half => (x + y) % 2 == 0,
            Pattern::ThreeQuarter => !(x % 2 == 0 && y % 2 == 0),
            Pattern::Scan => y % 2 == 1,
            Pattern::Bars => x % 2 == 1,
        }
    }

    /// Fraction of lit pixels.
    pub fn density(self) -> f32 {
        match self {
            Pattern::Eighth => 0.125,
            Pattern::Quarter => 0.25,
            Pattern::Half | Pattern::Scan | Pattern::Bars => 0.5,
            Pattern::ThreeQuarter => 0.75,
        }
    }
}

/// Fill `r` with `color` wherever the pattern is lit (the rest is left as it was).
pub fn fill(c: &mut Canvas, r: Rect, color: Color, pattern: Pattern) {
    let mut pts: Vec<Point> = Vec::with_capacity((r.width() * r.height() / 2) as usize + 1);
    for y in r.y()..r.y() + r.height() as i32 {
        for x in r.x()..r.x() + r.width() as i32 {
            if pattern.lit(x, y) {
                pts.push(Point::new(x, y));
            }
        }
    }
    c.set_draw_color(color);
    let _ = c.draw_points(&pts[..]);
}

/// Solid `base` with a `color` dither on top: a "gray" between two tones.
pub fn fill_two_tone(c: &mut Canvas, r: Rect, base: Color, color: Color, pattern: Pattern) {
    c.set_draw_color(base);
    let _ = c.fill_rect(r);
    fill(c, r, color, pattern);
}

/// Grey out something already drawn in `r`: knock out half of its pixels with
/// the surface colour (the classic "disabled" look).
pub fn gray_out(c: &mut Canvas, r: Rect, surface: Color) {
    fill(c, r, surface, Pattern::Half);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured(p: Pattern) -> f32 {
        let n = (0..64).flat_map(|y| (0..64).map(move |x| (x, y))).filter(|&(x, y)| p.lit(x, y)).count();
        n as f32 / (64.0 * 64.0)
    }

    #[test]
    fn each_pattern_has_its_stated_density() {
        for p in [Pattern::Eighth, Pattern::Quarter, Pattern::Half, Pattern::ThreeQuarter, Pattern::Scan, Pattern::Bars] {
            assert!((measured(p) - p.density()).abs() < 0.01, "{p:?}: {} vs {}", measured(p), p.density());
        }
    }

    #[test]
    fn quarter_and_three_quarter_are_complements() {
        for y in 0..8 {
            for x in 0..8 {
                assert_ne!(Pattern::Quarter.lit(x, y), Pattern::ThreeQuarter.lit(x, y));
            }
        }
    }

    #[test]
    fn patterns_are_defined_on_absolute_coordinates_and_tile() {
        for p in [Pattern::Eighth, Pattern::Half, Pattern::Scan] {
            for y in -4..8 {
                for x in -4..8 {
                    assert_eq!(p.lit(x, y), p.lit(x + 4, y + 4), "{p:?} tiles every 4 px");
                }
            }
        }
        assert!(!Pattern::Eighth.lit(1, 0) && Pattern::Eighth.lit(0, 0), "a sparse, regular dot grid");
    }

    #[test]
    fn the_checkerboard_alternates() {
        assert_ne!(Pattern::Half.lit(0, 0), Pattern::Half.lit(1, 0));
        assert_ne!(Pattern::Half.lit(0, 0), Pattern::Half.lit(0, 1));
        assert_eq!(Pattern::Half.lit(0, 0), Pattern::Half.lit(1, 1));
    }
}
