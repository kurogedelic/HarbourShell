//! The layout grid of the 320x170 LCD (see docs/DESIGN.md).
//!
//! Everything is laid out on a **4 px unit grid**: margins, gutters, row heights,
//! icon cells, button sizes and window furniture are multiples of [`UNIT`].
//! Text baselines are the one exception (a glyph sits wherever its font puts it
//! inside its row). Dense key clusters (the calculator) may use a 2 px hairline gap.

use crate::gfx::Rect;

pub const UNIT: i32 = 4;

/// The design size: the Cardputer Zero's LCD. A [`Sizing::Fixed`](crate::launcher::app::Sizing) app
/// is always drawn at this size (scaled by whole numbers on a bigger screen), so its constants stay
/// right. Code that also runs in a resizable window reads the surface size instead: [`sw`], [`sh`].
pub const SCREEN_W: i32 = 320;
pub const SCREEN_H: i32 = 170;

thread_local! {
    static SURFACE: std::cell::Cell<(i32, i32)> = const { std::cell::Cell::new((SCREEN_W, SCREEN_H)) };
}

/// The lowest surface: a flexible screen above the on-screen keyboard on a 170 px screen.
pub const MIN_SURFACE_H: i32 = 64;

/// Width of the surface being drawn now: the screen, or the window the host is drawing.
pub fn sw() -> i32 {
    SURFACE.with(|s| s.get().0)
}

/// Height of the surface being drawn now.
pub fn sh() -> i32 {
    SURFACE.with(|s| s.get().1)
}

/// Set the surface size (the host, before it draws a window or hands it input). Returns the
/// previous size so the caller can put it back.
pub fn set_surface(w: i32, h: i32) -> (i32, i32) {
    // Never narrower than the design width; a screen above the on-screen keyboard may be lower.
    SURFACE.with(|s| s.replace((w.max(SCREEN_W), h.max(MIN_SURFACE_H))))
}

/// Run `f` with the surface set to `size`, then put the old size back.
pub fn with_surface<R>(size: (i32, i32), f: impl FnOnce() -> R) -> R {
    let old = set_surface(size.0, size.1);
    let r = f();
    set_surface(old.0, old.1);
    r
}

/// [`SCROLL_X`] for the current surface.
pub fn scroll_x() -> i32 {
    sw() - MARGIN - SCROLL_W
}

/// [`APP_FRAME_BOTTOM`] for the current surface.
pub fn frame_bottom() -> i32 {
    sh() - 2
}

/// Menu strip at the top (4 units).
pub const BAR_H: i32 = 16;
/// Everything below the menu strip. The content is a whole number of 16 px rows
/// (9 of them = 144 px, y 16..160); the 10 px left over at the bottom stay EMPTY on every
/// screen. Nothing is stretched or squeezed to use them (and the rounded corners hide them).
pub const CONTENT_Y: i32 = BAR_H;
pub const CONTENT_ROWS: i32 = 9;
pub const CONTENT_H: i32 = CONTENT_ROWS * ROW_H;
pub const CONTENT_BOTTOM: i32 = CONTENT_Y + CONTENT_H;

/// Distance from content to the screen edge, and between neighbouring controls.
pub const MARGIN: i32 = 4;
pub const GUTTER: i32 = 4;
/// Windows keep this gap to the screen edges and the menu strip: 1 px, so the window is 318 px wide.
pub const WINDOW_INSET: i32 = 1;

/// Window title bar and standard list / menu row (4 units).
pub const TITLE_H: i32 = 16;
pub const ROW_H: i32 = 16;
/// Buttons: regular (6 units) and small (4 units).
pub const BTN_H: i32 = 24;
pub const BTN_H_SM: i32 = 16;
/// Home screen icon (6 units) and its cell.
pub const ICON: i32 = 24;
pub const CELL_W: i32 = 76;
pub const CELL_H: i32 = 48;
/// Scroll bars live in a 4 px column at the right edge of the content.
pub const SCROLL_W: i32 = 4;
pub const SCROLL_X: i32 = SCREEN_W - MARGIN - SCROLL_W;

/// Round to the nearest multiple of [`UNIT`].
pub fn snap(v: i32) -> i32 {
    ((v + UNIT / 2).div_euclid(UNIT)) * UNIT
}

/// `n` units in pixels.
pub const fn u(n: i32) -> i32 {
    n * UNIT
}

pub fn on_grid(v: i32) -> bool {
    v % UNIT == 0
}

/// A rectangle given in units, relative to the screen origin.
pub fn rect_u(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect::new(u(x), u(y), u(w) as u32, u(h) as u32)
}

/// The window: a 1 px gap to the screen edges and to the menu bar, so 318 x 152 at (1, 17).
pub fn window() -> Rect {
    let g = WINDOW_INSET;
    Rect::new(g, BAR_H + g, (sw() - 2 * g) as u32, (sh() - BAR_H - 2 * g) as u32)
}

/// 1 px frame, then 1 px of breathing room on the left, right and bottom.
pub const WINDOW_PAD: i32 = 1;
/// A window's inner rows are TITLE_H high; whatever does not fill another row stays empty.
pub const WINDOW_INNER_ROWS: i32 = 8;

/// The usable area of a window's inside: below the title bar and its rule (y + 17), inside the
/// frame plus [`WINDOW_PAD`] on the other three sides. For the standard window: (3, 34, 314, 133).
pub fn window_inner(window: Rect) -> Rect {
    let inset = 1 + WINDOW_PAD;
    Rect::new(
        window.x() + inset,
        window.y() + TITLE_H + 1,
        window.width().saturating_sub(2 * inset as u32),
        window.height().saturating_sub((TITLE_H + 1 + inset) as u32),
    )
}

/// The screen's two rounded top corners (`chrome::draw_corners`): pixels masked on each row, from
/// the corner. The bottom corners are square (nothing is masked there).
pub const CORNER_MASK: [i32; 6] = [6, 4, 3, 2, 1, 1];

/// The last row of the app frame's bottom line: 1 px above the screen's last row (like its sides,
/// 1 px in from the screen's edges).
pub const APP_FRAME_BOTTOM: i32 = SCREEN_H - 2;

/// Where `app_content` stops (exclusive): y 161 is its last row, as when the frame's corners were
/// rounded, so the framed apps keep their rows and cells.
/// 8 px above the screen's last row on every surface.
fn app_content_end() -> i32 {
    sh() - 8
}

/// The 1 px frame of a full-screen app whose content is one big area (Terminal, Text Editor,
/// Browser, Images): the old window outline without its title bar. It keeps 1 px to the screen
/// edges and to the menu strip's rule and runs down to y 168 (1 px above the screen's last row), a
/// plain rectangle with square corners: (1, 17, 318, 152). These screens use the screen down to its
/// bottom (docs/DESIGN.md).
pub fn app_frame() -> Rect {
    app_frame_from(BAR_H + WINDOW_INSET)
}

/// The same frame starting at row `top` (the Browser puts it under its toolbar).
pub fn app_frame_from(top: i32) -> Rect {
    let g = WINDOW_INSET;
    Rect::new(g, top, (sw() - 2 * g) as u32, (frame_bottom() + 1 - top) as u32)
}

/// Inside the frame's line: (2, 18, 316, 150).
pub fn app_frame_inner() -> Rect {
    let f = app_frame();
    Rect::new(f.x() + 1, f.y() + 1, f.width() - 2, f.height() - 2)
}

/// The rectangle the framed apps lay their content in, 1 px clear of the line: (3, 19, 314, 143),
/// x 3..316, y 19..161 (the rows below it, down to y 166, are free for a scroll bar or a strip).
pub fn app_content() -> Rect {
    app_content_from(BAR_H + WINDOW_INSET)
}

pub fn app_content_from(top: i32) -> Rect {
    let f = app_frame_from(top);
    Rect::new(f.x() + 2, top + 2, f.width() - 4, (app_content_end() - (top + 2)) as u32)
}

/// Every pixel of the frame's line, starting at row `top` (top line, sides, bottom line).
pub fn app_frame_points(top: i32) -> Vec<(i32, i32)> {
    let (l, r) = (WINDOW_INSET, sw() - 1 - WINDOW_INSET);
    let bottom = frame_bottom();
    let mut v = Vec::new();
    for x in l..=r {
        v.push((x, top));
    }
    for y in top + 1..bottom {
        v.push((l, y));
        v.push((r, y));
    }
    for x in l..=r {
        v.push((x, bottom));
    }
    v
}

/// Row `y`'s inside span (first and last x) of the frame, or None on and below the bottom line.
pub fn app_frame_span(y: i32) -> Option<(i32, i32)> {
    if y >= frame_bottom() {
        return None;
    }
    Some((WINDOW_INSET + 1, sw() - 2 - WINDOW_INSET))
}

/// Draws the app frame from row `top` down: the line in `line`, and when `fill` is given the inside
/// in that colour first.
pub fn draw_app_frame(c: &mut crate::gfx::Canvas, top: i32, line: crate::gfx::Color, fill: Option<crate::gfx::Color>) {
    if let Some(f) = fill {
        c.set_draw_color(f);
        for y in top + 1..frame_bottom() {
            if let Some((x0, x1)) = app_frame_span(y) {
                let _ = c.fill_rect(Rect::new(x0, y, (x1 - x0 + 1) as u32, 1));
            }
        }
    }
    c.set_draw_color(line);
    for (x, y) in app_frame_points(top) {
        let _ = c.draw_point((x, y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_structural_sizes_are_multiples_of_the_unit() {
        for (name, v) in [
            ("BAR_H", BAR_H),
            ("MARGIN", MARGIN),
            ("GUTTER", GUTTER),
            ("TITLE_H", TITLE_H),
            ("ROW_H", ROW_H),
            ("BTN_H", BTN_H),
            ("BTN_H_SM", BTN_H_SM),
            ("ICON", ICON),
            ("CELL_W", CELL_W),
            ("CELL_H", CELL_H),
            ("SCROLL_W", SCROLL_W),
            ("SCROLL_X", SCROLL_X),
        ] {
            assert!(on_grid(v), "{name} = {v} is not on the {UNIT} px grid");
        }
        for (name, v) in [("CONTENT_H", CONTENT_H), ("CONTENT_BOTTOM", CONTENT_BOTTOM)] {
            assert!(on_grid(v), "{name} = {v} is not on the {UNIT} px grid");
        }
    }

    #[test]
    fn the_home_grid_is_whole_cells_and_leftover_space_stays_empty() {
        // 4 columns of CELL_W from the margin, a gutter, then the scroll bar column.
        assert_eq!(MARGIN + 4 * CELL_W + GUTTER, SCROLL_X, "four cells, a gutter, then the scroll bar");
        assert_eq!(SCROLL_X + SCROLL_W + MARGIN, SCREEN_W);
        // Three rows of CELL_H (tile = cell minus a gutter) end exactly on the last content row.
        assert_eq!(CONTENT_Y + MARGIN + 3 * CELL_H - GUTTER, CONTENT_BOTTOM);
    }

    #[test]
    fn the_content_is_whole_rows_and_the_rest_of_the_screen_is_left_empty() {
        assert_eq!(CONTENT_H % ROW_H, 0, "content is a whole number of rows");
        assert_eq!(CONTENT_ROWS, 9);
        assert_eq!(SCREEN_H - CONTENT_BOTTOM, 10, "ten pixels stay empty at the bottom");
    }

    #[test]
    fn snapping_and_unit_helpers() {
        assert_eq!(snap(5), 4);
        assert_eq!(snap(6), 8);
        assert_eq!(snap(-1), 0);
        assert_eq!(u(6), 24);
        assert_eq!(rect_u(1, 5, 20, 6), Rect::new(4, 20, 80, 24));
    }

    #[test]
    fn the_window_keeps_one_pixel_to_the_screen_and_the_inner_area_is_whole_rows() {
        let win = window();
        assert_eq!((win.x(), win.y(), win.width(), win.height()), (1, 17, 318, 152));
        assert_eq!(win.right(), SCREEN_W - 1, "1 px gap right");
        assert_eq!(win.bottom(), SCREEN_H - 1, "1 px gap below");
        let inner = window_inner(win);
        assert_eq!((inner.x(), inner.y(), inner.width(), inner.height()), (3, 34, 314, 133));
        assert!(inner.height() as i32 >= WINDOW_INNER_ROWS * ROW_H, "eight whole rows fit");
        assert!((inner.height() as i32) < (WINDOW_INNER_ROWS + 1) * ROW_H, "a ninth does not");
        assert!(inner.right() < win.right() && inner.bottom() < win.bottom(), "inside the frame");
    }

    #[test]
    fn the_app_frame_runs_to_the_screen_bottom_with_square_corners() {
        let f = app_frame();
        assert_eq!((f.x(), f.y(), f.width(), f.height()), (1, 17, 318, 152));
        assert_eq!(f.y(), BAR_H + 1, "1 px below the menu strip's rule");
        assert_eq!(f.bottom(), SCREEN_H - 1, "the bottom line is y 168, 1 px above the screen's last row");
        let i = app_frame_inner();
        assert_eq!((i.x(), i.y(), i.width(), i.height()), (2, 18, 316, 150));
        let ct = app_content();
        assert_eq!((ct.x(), ct.y(), ct.width(), ct.height()), (3, 19, 314, 143), "the framed apps keep their rows");
        // The line is exactly the rectangle's outline: square corners, nothing missing, nothing extra.
        let pts = app_frame_points(f.y());
        let set: std::collections::HashSet<_> = pts.iter().copied().collect();
        let mut want = std::collections::HashSet::new();
        for x in f.x()..f.right() {
            want.insert((x, f.y()));
            want.insert((x, APP_FRAME_BOTTOM));
        }
        for y in f.y()..f.bottom() {
            want.insert((f.x(), y));
            want.insert((f.right() - 1, y));
        }
        assert_eq!(set, want);
        for corner in [(1, 168), (318, 168)] {
            assert!(set.contains(&corner), "{corner:?}: the bottom corners are square");
        }
        // Nothing at the screen's edges or last row (1 px clear, like the sides).
        assert!(pts.iter().all(|&(x, y)| x >= 1 && x <= SCREEN_W - 2 && y <= SCREEN_H - 2));
        // The content rectangle keeps a clear pixel to the line all round.
        for &(x, y) in &pts {
            let near = x >= ct.x() - 1 && x <= ct.right() && y >= ct.y() - 1 && y <= ct.bottom();
            assert!(!near, "({x},{y}) is next to the content");
        }
        // The fill is the whole inside, never on the line.
        for y in f.y() + 1..APP_FRAME_BOTTOM {
            assert_eq!(app_frame_span(y), Some((2, 317)), "row {y}");
        }
        assert_eq!(app_frame_span(APP_FRAME_BOTTOM), None);
    }

    #[test]
    fn only_the_top_corners_are_masked() {
        // chrome::draw_corners masks rows 0..6 at the top; the bottom rows are left alone.
        assert_eq!(CORNER_MASK.len(), 6);
        assert!(CORNER_MASK.iter().all(|&n| n >= 1) && CORNER_MASK.windows(2).all(|w| w[0] >= w[1]));
        assert!((CORNER_MASK.len() as i32) < BAR_H, "the mask stays inside the menu strip");
    }
}
