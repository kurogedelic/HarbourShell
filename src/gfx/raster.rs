//! Line clipping and walking, ported from SDL2's software renderer so the pixels
//! match exactly (`SDL_IntersectRectAndLine`, `SDL_DrawLines`).

use super::geom::{Point, Rect};

const CODE_BOTTOM: u8 = 1;
const CODE_TOP: u8 = 2;
const CODE_LEFT: u8 = 4;
const CODE_RIGHT: u8 = 8;

fn outcode(x0: i32, y0: i32, x1: i32, y1: i32, x: i32, y: i32) -> u8 {
    let mut code = 0;
    if y < y0 {
        code |= CODE_TOP;
    } else if y > y1 {
        code |= CODE_BOTTOM;
    }
    if x < x0 {
        code |= CODE_LEFT;
    } else if x > x1 {
        code |= CODE_RIGHT;
    }
    code
}

/// Clip the segment to `rect`; moves the end points and returns false if nothing is
/// left. Same integer arithmetic as `SDL_IntersectRectAndLine`.
pub fn clip_line(rect: Rect, p1: &mut (i32, i32), p2: &mut (i32, i32)) -> bool {
    let (mut x1, mut y1) = *p1;
    let (mut x2, mut y2) = *p2;
    let (rx1, ry1) = (rect.x(), rect.y());
    let (rx2, ry2) = (rect.right() - 1, rect.bottom() - 1);

    // Entirely inside.
    if x1 >= rx1 && x1 <= rx2 && x2 >= rx1 && x2 <= rx2 && y1 >= ry1 && y1 <= ry2 && y2 >= ry1 && y2 <= ry2 {
        return true;
    }
    // Entirely on one side.
    if (x1 < rx1 && x2 < rx1) || (x1 > rx2 && x2 > rx2) || (y1 < ry1 && y2 < ry1) || (y1 > ry2 && y2 > ry2) {
        return false;
    }
    if y1 == y2 {
        p1.0 = x1.clamp(rx1, rx2);
        p2.0 = x2.clamp(rx1, rx2);
        return true;
    }
    if x1 == x2 {
        p1.1 = y1.clamp(ry1, ry2);
        p2.1 = y2.clamp(ry1, ry2);
        return true;
    }

    // Cohen-Sutherland.
    let mut code1 = outcode(rx1, ry1, rx2, ry2, x1, y1);
    let mut code2 = outcode(rx1, ry1, rx2, ry2, x2, y2);
    while code1 != 0 || code2 != 0 {
        if code1 & code2 != 0 {
            return false;
        }
        // SDL measures from point 1 for both ends, using whatever point 1 is by now.
        let (dx, dy) = ((x2 - x1) as i64, (y2 - y1) as i64);
        let from_first = code1 != 0;
        let code = if from_first { code1 } else { code2 };
        let (x, y);
        if code & CODE_TOP != 0 {
            y = ry1;
            x = (x1 as i64 + dx * (y - y1) as i64 / dy) as i32;
        } else if code & CODE_BOTTOM != 0 {
            y = ry2;
            x = (x1 as i64 + dx * (y - y1) as i64 / dy) as i32;
        } else if code & CODE_LEFT != 0 {
            x = rx1;
            y = (y1 as i64 + dy * (x - x1) as i64 / dx) as i32;
        } else {
            x = rx2;
            y = (y1 as i64 + dy * (x - x1) as i64 / dx) as i32;
        }
        if from_first {
            x1 = x;
            y1 = y;
            code1 = outcode(rx1, ry1, rx2, ry2, x, y);
        } else {
            x2 = x;
            y2 = y;
            code2 = outcode(rx1, ry1, rx2, ry2, x, y);
        }
    }
    *p1 = (x1, y1);
    *p2 = (x2, y2);
    true
}

/// Walk a Bresenham line from (x1, y1) to (x2, y2), calling `plot` for each pixel.
/// With `draw_end` false the last pixel is left out (SDL does this so that joined
/// segments do not draw the shared point twice).
pub fn walk_line(x1: i32, y1: i32, x2: i32, y2: i32, draw_end: bool, mut plot: impl FnMut(i32, i32)) {
    let deltax = (x2 - x1).abs();
    let deltay = (y2 - y1).abs();
    let (mut numpixels, mut d, dinc1, dinc2, mut xinc1, mut xinc2, mut yinc1, mut yinc2);
    if deltax >= deltay {
        numpixels = deltax + 1;
        d = 2 * deltay - deltax;
        dinc1 = deltay * 2;
        dinc2 = (deltay - deltax) * 2;
        xinc1 = 1;
        xinc2 = 1;
        yinc1 = 0;
        yinc2 = 1;
    } else {
        numpixels = deltay + 1;
        d = 2 * deltax - deltay;
        dinc1 = deltax * 2;
        dinc2 = (deltax - deltay) * 2;
        xinc1 = 0;
        xinc2 = 1;
        yinc1 = 1;
        yinc2 = 1;
    }
    if x1 > x2 {
        xinc1 = -xinc1;
        xinc2 = -xinc2;
    }
    if y1 > y2 {
        yinc1 = -yinc1;
        yinc2 = -yinc2;
    }
    let (mut x, mut y) = (x1, y1);
    if !draw_end {
        numpixels -= 1;
    }
    for _ in 0..numpixels {
        plot(x, y);
        if d < 0 {
            d += dinc1;
            x += xinc1;
            y += yinc1;
        } else {
            d += dinc2;
            x += xinc2;
            y += yinc2;
        }
    }
}

/// Draw a connected polyline the way `SDL_DrawLines` does: every segment is clipped
/// and drawn without its last pixel (unless clipping moved that end), and the very
/// last point is drawn on its own when the path is not closed.
pub fn polyline(clip: Rect, points: &[Point], mut plot: impl FnMut(i32, i32)) {
    if points.len() < 2 {
        return;
    }
    for pair in points.windows(2) {
        let mut a = (pair[0].x(), pair[0].y());
        let mut b = (pair[1].x(), pair[1].y());
        if !clip_line(clip, &mut a, &mut b) {
            continue;
        }
        let draw_end = a == b || b != (pair[1].x(), pair[1].y());
        walk_line(a.0, a.1, b.0, b.1, draw_end, &mut plot);
    }
    let first = points[0];
    let last = points[points.len() - 1];
    if first != last && clip.contains_point((last.x(), last.y())) {
        plot(last.x(), last.y());
    }
}
