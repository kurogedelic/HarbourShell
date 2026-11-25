//! Camera: everything that is drawn (the preview, the side column and the alerts).

use crate::gfx::Color;
use crate::gfx::{Point, Rect};
use crate::gfx::Canvas;

use super::*;
use crate::launcher::i18n;
use crate::ui;

impl Camera {
    pub(super) fn paint(&mut self, c: &mut Canvas, assets: &mut AssetStore, ctx: &Ctx) {
        use crate::launcher::sevenseg as seg;
        let (tm, fs) = (ctx.tm, ctx.fs);
        ui::fill_rect(c, Rect::new(0, grid::BAR_H, grid::SCREEN_W as u32, (grid::SCREEN_H - grid::BAR_H) as u32), tm.get_color("home_bg"));
        if self.probe.devices.is_empty() || self.error.is_some() {
            self.draw_card(c, ctx);
            return;
        }

        // The panel face is the theme's "white"; Invert themes lift it to a dark tone, where the
        // plain light tone is used instead (as on the Radio dial).
        let t = widgets::tones(tm);
        let lum = |c: Color| c.r as u32 * 3 + c.g as u32 * 6 + c.b as u32;
        let white = tm.get_color("icon_white");
        let face = if lum(white) * 2 >= lum(t.light) + lum(t.dark) { white } else { t.light };
        let land = t.dark;
        let rule = tm.get_color("line");
        let flash = self.flash_until.is_some();

        // --- the picture: a framed print, 1 px of ink around the face ---
        let fr = preview();
        ui::fill_rect(c, fr, if flash { land } else { face });
        ui::draw_rect(c, fr, land);
        if self.bits.is_empty() {
            let msg = &i18n::tr_s("Starting camera...");
            let w = fs.measure_text(msg);
            fs.draw_text(c, fr.x() + (fr.width() as i32 - w) / 2, fr.y() + (fr.height() as i32 - 12) / 2, msg, if flash { face } else { crate::launcher::widgets::secondary_ink(tm) });
        } else if !flash {
            let ox = fr.x() + 1 + (PREVIEW_INNER.0 as i32 - self.pw as i32) / 2;
            let oy = fr.y() + 1 + (PREVIEW_INNER.1 as i32 - self.ph as i32) / 2;
            let ramp = tone::ramp(land, face);
            let pw = self.pw as usize;
            // Dither: one ink tone (value 0). Grey: the four tones of the theme ramp.
            let tones: &[(u8, Color)] = if self.prefs.mode == Mode::Grey { &[(3, ramp[3]), (0, ramp[0]), (1, ramp[1]), (2, ramp[2])] } else { &[(0, land)] };
            for &(tn, col) in tones {
                self.points.clear();
                for y in 0..self.ph as usize {
                    for x in 0..pw {
                        if self.bits[y * pw + x] == tn {
                            self.points.push(Point::new(ox + x as i32, oy + y as i32));
                        }
                    }
                }
                c.set_draw_color(col);
                let _ = c.draw_points(&self.points[..]);
            }
        }

        // --- the side panel: status, counter, controls, shutter ---
        let panel = side_panel();
        ui::fill_rect(c, panel, face);
        ui::draw_rect(c, panel, land);
        let (x0, w) = (COL_X, COL_W as i32);
        let text = |c: &mut Canvas, y: i32, t: &str, col: Color| fs.draw_text(c, x0, y, &chrome::fit_text(fs, t, w), col);

        // Row 1: the mode on an ink plate, a lamp with the frame rate (or "Saved") on the right.
        let mode = self.prefs.mode.name().to_uppercase();
        let mw = fs.measure_display_text(&mode);
        ui::fill_rect(c, Rect::new(x0, 22, (mw + 6) as u32, 14), land);
        fs.draw_display_text(c, x0 + 3, 22, &mode, face);
        let live = !self.bits.is_empty();
        let (label, lit) = if flash {
            (i18n::tr_s("Saved"), true)
        } else if live {
            (if self.fps > 0.0 { format!("{:.0} fps", self.fps) } else { i18n::tr_s("Live") }, true)
        } else {
            (i18n::tr_s("Wait"), false)
        };
        let lw = fs.measure_text(&label);
        fs.draw_text(c, x0 + w - lw, 23, &label, land);
        lamp(c, x0 + w - lw - 9, 29, lit, land, face);

        // Rows 2-3: which camera, and what it delivers.
        let dev = self.device().map(|d| d.name.clone()).unwrap_or_default();
        text(c, 40, &dev, land);
        if let Some(i) = &self.info {
            text(c, 52, &format!("{}x{} {}", i.w, i.h, i.fmt), crate::launcher::widgets::secondary_ink(tm));
        }

        // The photo count in 7-segment digits between two rules.
        ui::fill_rect(c, Rect::new(x0, 66, w as u32, 1), rule);
        let n = self.photos.min(999);
        let mut x = x0;
        for d in [n / 100, (n / 10) % 10, n % 10] {
            seg::draw_atlas_digit(c, assets, seg::BIG_KEY, 14, 20, d, x, 69, land);
            x += 17;
        }
        text_at(c, fs, x + 3, 77, &i18n::tr_s("Photos"), crate::launcher::widgets::secondary_ink(tm), x0 + w);
        ui::fill_rect(c, Rect::new(x0, 92, w as u32, 1), rule);

        // Brightness and contrast meters (only what the camera has).
        // 14 px apart: a 12 px line of kanji keeps two clear pixels above the next one.
        let mut y = 96;
        if let Some(i) = &self.info {
            for (label, r) in [("Bright", i.bright), ("Contrast", i.contrast)] {
                if let Some(r) = r {
                    fs.draw_text(c, x0, y, &i18n::tr_s(label), land);
                    widgets::draw_meter(c, tm, Rect::new(x0 + 56, y + 2, (w - 56) as u32, 8), r.frac());
                    y += 14;
                }
            }
        }

        widgets::draw_button(c, fs, tm, shutter_rect(), "Shutter", BtnState { hover: self.hover, pressed: flash, ..Default::default() });
    }

    /// The alert shown when there is no camera (or it failed).
    pub(super) fn alert(&self) -> Alert {
        let busy = self.scan_flash.is_some();
        let checked = if self.last_scan.is_empty() { String::new() } else { format!("\n{}", i18n::tr_s(&format!("Last scan {}", self.last_scan))) };
        let mut a = match &self.error {
            Some(e) if self.unplugged => Alert::new(AlertIcon::Stop, "Camera lost.", e, &["Scan Again"]),
            Some(e) => Alert::new(AlertIcon::Caution, "Camera problem", &format!("{e}{checked}"), &["Scan Again"]),
            None if busy => Alert::new(AlertIcon::Stop, "No camera found.", "Scanning...", &["Scan Again"]),
            None => Alert::new(AlertIcon::Stop, "No camera found.", &format!("{}{checked}", i18n::tr_s("Plug in a USB camera, then scan again.")), &["Scan Again"]),
        };
        a.busy = busy;
        a.pressed = busy.then_some(0);
        a
    }


    fn draw_card(&self, c: &mut Canvas, ctx: &Ctx) {
        self.alert().draw(c, ctx.fs, ctx.tm);
    }
}

fn text_at(c: &mut Canvas, fs: &crate::font::FontSystem, x: i32, y: i32, t: &str, col: Color, right: i32) {
    fs.draw_text(c, x, y, &chrome::fit_text(fs, t, (right - x).max(0)), col);
}

/// A round lamp: an outline, filled when lit.
fn lamp(c: &mut Canvas, cx: i32, cy: i32, lit: bool, on: Color, off: Color) {
    ui::fill_rect(c, Rect::new(cx - 5, cy - 5, 11, 11), off);
    ui::draw_circle(c, (cx, cy), 5, on);
    if lit {
        for r in 0..=3 {
            ui::draw_circle(c, (cx, cy), r, on);
        }
    }
}
