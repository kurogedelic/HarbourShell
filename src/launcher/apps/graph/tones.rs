//! From a [`Raster`] to colours: the theme's tones from paper to ink, with the shell's dither
//! patterns in between, so a picture looks at home on every theme (Invert, Platinum, black and
//! white panels). The same mapping feeds the screen and PNG export.

use crate::gfx::Color;
use crate::gfx::Rect;
use crate::gfx::Canvas;

use super::render::{Raster, AXIS, GRID, INK, SHADE};
use crate::dither::Pattern;
use crate::theme::ThemeManager;

pub struct Ramp {
    /// Solid tones from the paper to the ink.
    tones: Vec<Color>,
    grid: Color,
    axis: Color,
    ink: Color,
}

fn lum(c: Color) -> i32 {
    c.r as i32 * 3 + c.g as i32 * 6 + c.b as i32
}

impl Ramp {
    /// The theme's paper and ink, and the palette tones that lie between them. A black-and-white
    /// panel (`mono`) gets just paper and ink: every grey is a dither.
    pub fn of(tm: &ThemeManager, mono: bool) -> Ramp {
        let paper = tm.get_color("widget_face_active");
        let ink = tm.get_color("foreground");
        let (lp, li) = (lum(paper), lum(ink));
        let mut mids: Vec<Color> = Vec::new();
        if !mono {
            for k in ["palette0", "palette1", "palette2", "palette3"] {
                let c = tm.get_color(k);
                let l = lum(c);
                // Strictly between paper and ink, and clearly apart from both.
                let between = (l - lp) * (l - li) < 0 && (l - lp).abs() > 60 && (l - li).abs() > 60;
                if between && !mids.iter().any(|m| lum(*m) == l) {
                    mids.push(c);
                }
            }
            mids.sort_by_key(|c| (lum(*c) - lp).abs());
        }
        let mut tones = vec![paper];
        tones.extend(mids);
        tones.push(ink);
        let (grid, axis) = if mono { (ink, ink) } else { (tm.get_color("line"), crate::launcher::widgets::secondary_ink(tm)) };
        Ramp { tones, grid, axis, ink }
    }

    /// How many solid tones there are (paper and ink included).
    pub fn steps(&self) -> usize {
        self.tones.len()
    }

    /// The colour of raster value `v` at screen pixel (x, y) (the dither follows the screen, so
    /// neighbouring areas line up).
    pub fn colour(&self, v: u8, x: i32, y: i32) -> Color {
        match v {
            INK => self.ink,
            AXIS => self.axis,
            GRID => self.grid,
            _ => {
                let pos = v.min(SHADE) as f32 / SHADE as f32 * (self.tones.len() - 1) as f32;
                let i = (pos.floor() as usize).min(self.tones.len() - 2);
                let upper = match pattern_for(pos - i as f32) {
                    Dither::Lower => false,
                    Dither::Upper => true,
                    Dither::Mix(p) => p.lit(x, y),
                };
                self.tones[i + upper as usize]
            }
        }
    }
}

enum Dither {
    Lower,
    Mix(Pattern),
    Upper,
}

/// The dither nearest to `frac` (0..1) of the upper tone.
fn pattern_for(frac: f32) -> Dither {
    match frac {
        f if f < 1.0 / 16.0 => Dither::Lower,
        f if f < 3.0 / 16.0 => Dither::Mix(Pattern::Eighth),
        f if f < 3.0 / 8.0 => Dither::Mix(Pattern::Quarter),
        f if f < 5.0 / 8.0 => Dither::Mix(Pattern::Half),
        f if f < 7.0 / 8.0 => Dither::Mix(Pattern::ThreeQuarter),
        _ => Dither::Upper,
    }
}

/// Draw `r` into `dest` on the screen, each raster pixel `scale` x `scale` screen pixels, shifted by
/// `shift` screen pixels (a picture being dragged). Runs of one colour go out as one rectangle.
pub fn blit(c: &mut Canvas, r: &Raster, scale: usize, dest: Rect, shift: (i32, i32), ramp: &Ramp, paper: Color) {
    let s = scale.max(1) as i32;
    for sy in 0..dest.height() as i32 {
        let ry = (sy - shift.1).div_euclid(s);
        let y = dest.y() + sy;
        let mut run: Option<(i32, Color)> = None;
        for sx in 0..=dest.width() as i32 {
            let col = if sx == dest.width() as i32 {
                None
            } else {
                let rx = (sx - shift.0).div_euclid(s);
                Some(if rx < 0 || ry < 0 || rx >= r.w as i32 || ry >= r.h as i32 { paper } else { ramp.colour(r.get(rx as usize, ry as usize), dest.x() + sx, y) })
            };
            match (run, col) {
                (Some((_, rc)), Some(cc)) if rc == cc => {}
                _ => {
                    if let Some((x0, rc)) = run {
                        c.set_draw_color(rc);
                        let _ = c.fill_rect(Rect::new(dest.x() + x0, y, (sx - x0) as u32, 1));
                    }
                    run = col.map(|cc| (sx, cc));
                }
            }
        }
    }
}

/// The picture as packed RGB (for PNG export), dithered from its own top-left corner.
pub fn rgb(r: &Raster, ramp: &Ramp) -> Vec<u8> {
    let mut out = Vec::with_capacity(r.w * r.h * 3);
    for y in 0..r.h {
        for x in 0..r.w {
            let c = ramp.colour(r.get(x, y), x as i32, y as i32);
            out.extend([c.r, c.g, c.b]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn ramp(theme: &str, mono: bool) -> (Ramp, ThemeManager) {
        let tm = ThemeManager::new(Theme::load_from_yaml(&format!("assets/themes/{theme}.yaml")).unwrap());
        (Ramp::of(&tm, mono), tm)
    }

    #[test]
    fn tones_run_from_paper_to_ink_on_every_theme() {
        for theme in ["theme-paper", "theme-paper-invert", "theme-mono", "theme-mono-invert", "theme-platinum", "theme-platinum-invert", "theme-black", "theme-terminal"] {
            let (r, tm) = ramp(theme, false);
            let (paper, ink) = (tm.get_color("widget_face_active"), tm.get_color("foreground"));
            assert_eq!(r.colour(0, 0, 0), paper, "{theme}");
            assert_eq!(r.colour(SHADE, 0, 0), ink, "{theme}");
            assert_eq!(r.colour(INK, 3, 3), ink, "{theme}");
            assert!(r.steps() >= 2, "{theme}");
            // Each step is further from the paper than the one before.
            let d: Vec<i32> = r.tones.iter().map(|c| (lum(*c) - lum(paper)).abs()).collect();
            assert!(d.windows(2).all(|w| w[0] < w[1]), "{theme}: {d:?}");
        }
    }

    #[test]
    fn a_black_and_white_panel_only_gets_paper_and_ink() {
        let (r, tm) = ramp("theme-platinum", true);
        assert_eq!(r.steps(), 2);
        let (paper, ink) = (tm.get_color("widget_face_active"), tm.get_color("foreground"));
        // Half way is a checkerboard of the two.
        let half = SHADE / 2;
        let cols: Vec<Color> = (0..4).map(|x| r.colour(half, x, 0)).collect();
        assert!(cols.contains(&paper) && cols.contains(&ink));
        for v in [0, 40, 100, 160, SHADE, GRID, AXIS, INK] {
            for (x, y) in [(0, 0), (1, 0), (0, 1), (3, 2)] {
                let c = r.colour(v, x, y);
                assert!(c == paper || c == ink, "only two colours");
            }
        }
    }

    #[test]
    fn darker_values_have_more_ink() {
        let (r, tm) = ramp("theme-mono", true);
        let ink = tm.get_color("foreground");
        let count = |v: u8| (0..4).flat_map(|y| (0..4).map(move |x| (x, y))).filter(|(x, y)| r.colour(v, *x, *y) == ink).count();
        let counts: Vec<usize> = (0..=10).map(|k| count((k * SHADE as usize / 10) as u8)).collect();
        assert!(counts.windows(2).all(|w| w[0] <= w[1]), "{counts:?}");
        assert_eq!(counts[0], 0);
        assert_eq!(counts[10], 16);
    }

    #[test]
    fn rgb_has_three_bytes_a_pixel() {
        let (r, _) = ramp("theme-paper", false);
        let mut ras = Raster::new(5, 3);
        ras.set(2, 1, INK);
        let px = rgb(&ras, &r);
        assert_eq!(px.len(), 45);
        let i = (5 + 2) * 3;
        let ink = r.colour(INK, 0, 0);
        assert_eq!(&px[i..i + 3], &[ink.r, ink.g, ink.b]);
    }
}
