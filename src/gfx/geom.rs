//! `Point` and `Rect`, with the same method names and edge rules as `sdl2::rect`.
//!
//! Like SDL a `Rect` is never empty (a zero width or height becomes 1), positions
//! are clamped to roughly +-1 billion and sizes to about 1 billion, and the right and
//! bottom edges are exclusive.

use core::ops::{Add, AddAssign, BitAnd, BitOr, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

const MAX_INT: i32 = i32::MAX / 2;
const MIN_INT: i32 = i32::MIN / 2;

fn clamp_size(v: u32) -> i32 {
    if v == 0 {
        1
    } else if v > MAX_INT as u32 {
        MAX_INT
    } else {
        v as i32
    }
}

fn clamp_position(v: i32) -> i32 {
    v.clamp(MIN_INT, MAX_INT)
}

fn clamped_mul(a: i32, b: i32) -> i32 {
    match a.checked_mul(b) {
        Some(v) => v,
        None if (a < 0) ^ (b < 0) => MIN_INT,
        None => MAX_INT,
    }
}

/// A point. Coordinates are clamped like SDL's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Point {
    x: i32,
    y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Point {
        Point { x: clamp_position(x), y: clamp_position(y) }
    }
    pub fn x(self) -> i32 {
        self.x
    }
    pub fn y(self) -> i32 {
        self.y
    }
    /// A copy moved by (x, y), clamped.
    pub fn offset(self, x: i32, y: i32) -> Point {
        let nx = self.x.checked_add(x).unwrap_or(if x < 0 { MIN_INT } else { MAX_INT });
        let ny = self.y.checked_add(y).unwrap_or(if y < 0 { MIN_INT } else { MAX_INT });
        Point::new(nx, ny)
    }
    /// A copy with both coordinates multiplied by `f`.
    pub fn scale(self, f: i32) -> Point {
        Point::new(clamped_mul(self.x, f), clamped_mul(self.y, f))
    }
}

impl From<(i32, i32)> for Point {
    fn from((x, y): (i32, i32)) -> Point {
        Point::new(x, y)
    }
}
impl From<Point> for (i32, i32) {
    fn from(p: Point) -> (i32, i32) {
        (p.x, p.y)
    }
}
impl Add for Point {
    type Output = Point;
    fn add(self, o: Point) -> Point {
        self.offset(o.x, o.y)
    }
}
impl AddAssign for Point {
    fn add_assign(&mut self, o: Point) {
        *self = Point::new(self.x + o.x, self.y + o.y);
    }
}
impl Sub for Point {
    type Output = Point;
    fn sub(self, o: Point) -> Point {
        self.offset(-o.x, -o.y)
    }
}
impl SubAssign for Point {
    fn sub_assign(&mut self, o: Point) {
        *self = Point::new(self.x - o.x, self.y - o.y);
    }
}
impl Neg for Point {
    type Output = Point;
    fn neg(self) -> Point {
        Point::new(-self.x, -self.y)
    }
}
impl Mul<i32> for Point {
    type Output = Point;
    fn mul(self, f: i32) -> Point {
        self.scale(f)
    }
}
impl MulAssign<i32> for Point {
    fn mul_assign(&mut self, f: i32) {
        *self = self.scale(f);
    }
}
impl Div<i32> for Point {
    type Output = Point;
    fn div(self, d: i32) -> Point {
        Point::new(self.x / d, self.y / d)
    }
}
impl DivAssign<i32> for Point {
    fn div_assign(&mut self, d: i32) {
        self.x /= d;
        self.y /= d;
    }
}
impl core::iter::Sum for Point {
    fn sum<I: Iterator<Item = Point>>(iter: I) -> Point {
        iter.fold(Point::new(0, 0), Add::add)
    }
}

/// A non-empty rectangle; the right and bottom edges are exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect { x: clamp_position(x), y: clamp_position(y), w: clamp_size(width), h: clamp_size(height) }
    }

    pub fn from_center<P: Into<Point>>(center: P, width: u32, height: u32) -> Rect {
        let mut r = Rect { x: 0, y: 0, w: clamp_size(width), h: clamp_size(height) };
        r.center_on(center.into());
        r
    }

    pub fn x(&self) -> i32 {
        self.x
    }
    pub fn y(&self) -> i32 {
        self.y
    }
    pub fn width(&self) -> u32 {
        self.w as u32
    }
    pub fn height(&self) -> u32 {
        self.h as u32
    }
    pub fn size(&self) -> (u32, u32) {
        (self.width(), self.height())
    }
    pub fn set_x(&mut self, x: i32) {
        self.x = clamp_position(x);
    }
    pub fn set_y(&mut self, y: i32) {
        self.y = clamp_position(y);
    }
    pub fn set_width(&mut self, width: u32) {
        self.w = clamp_size(width);
    }
    pub fn set_height(&mut self, height: u32) {
        self.h = clamp_size(height);
    }
    pub fn left(&self) -> i32 {
        self.x
    }
    /// One past the last column (x + width).
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn top(&self) -> i32 {
        self.y
    }
    /// One past the last row (y + height).
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn origin(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    pub fn left_shifted(mut self, offset: i32) -> Rect {
        self.offset(-offset, 0);
        self
    }
    pub fn right_shifted(mut self, offset: i32) -> Rect {
        self.offset(offset, 0);
        self
    }
    pub fn top_shifted(mut self, offset: i32) -> Rect {
        self.offset(0, -offset);
        self
    }
    pub fn bottom_shifted(mut self, offset: i32) -> Rect {
        self.offset(0, offset);
        self
    }

    /// Centre, rounded down for odd sizes.
    pub fn center(&self) -> Point {
        Point::new(self.x + self.w / 2, self.y + self.h / 2)
    }
    pub fn top_left(&self) -> Point {
        Point::new(self.left(), self.top())
    }
    pub fn top_right(&self) -> Point {
        Point::new(self.right(), self.top())
    }
    pub fn bottom_left(&self) -> Point {
        Point::new(self.left(), self.bottom())
    }
    pub fn bottom_right(&self) -> Point {
        Point::new(self.right(), self.bottom())
    }

    pub fn set_right(&mut self, right: i32) {
        self.x = clamp_position(clamp_position(right) - self.w);
    }
    pub fn set_bottom(&mut self, bottom: i32) {
        self.y = clamp_position(clamp_position(bottom) - self.h);
    }
    pub fn center_on<P: Into<(i32, i32)>>(&mut self, point: P) {
        let (x, y) = point.into();
        self.x = clamp_position(clamp_position(x) - self.w / 2);
        self.y = clamp_position(clamp_position(y) - self.h / 2);
    }
    pub fn centered_on<P: Into<(i32, i32)>>(mut self, point: P) -> Rect {
        self.center_on(point);
        self
    }

    /// Move by (x, y), clamping the position.
    pub fn offset(&mut self, x: i32, y: i32) {
        self.x = match self.x.checked_add(x) {
            Some(v) => clamp_position(v),
            None if x >= 0 => MAX_INT,
            None => i32::MIN,
        };
        self.y = match self.y.checked_add(y) {
            Some(v) => clamp_position(v),
            None if y >= 0 => MAX_INT,
            None => i32::MIN,
        };
    }
    pub fn reposition<P: Into<(i32, i32)>>(&mut self, point: P) {
        let (x, y) = point.into();
        self.x = clamp_position(x);
        self.y = clamp_position(y);
    }
    pub fn resize(&mut self, width: u32, height: u32) {
        self.w = clamp_size(width);
        self.h = clamp_size(height);
    }

    /// True if the point is inside; the right and bottom edges are outside.
    pub fn contains_point<P: Into<(i32, i32)>>(&self, point: P) -> bool {
        let (x, y) = point.into();
        x >= self.left() && x < self.right() && y >= self.top() && y < self.bottom()
    }
    /// True if every point of `other` is inside `self`.
    pub fn contains_rect(&self, other: Rect) -> bool {
        other.left() >= self.left()
            && other.right() <= self.right()
            && other.top() >= self.top()
            && other.bottom() <= self.bottom()
    }
    /// Rectangles that only share an edge do not intersect.
    pub fn has_intersection(&self, other: Rect) -> bool {
        self.intersection(other).is_some()
    }
    pub fn intersection(&self, other: Rect) -> Option<Rect> {
        let x1 = self.left().max(other.left());
        let x2 = self.right().min(other.right());
        let y1 = self.top().max(other.top());
        let y2 = self.bottom().min(other.bottom());
        if x2 <= x1 || y2 <= y1 {
            return None;
        }
        Some(Rect { x: x1, y: y1, w: x2 - x1, h: y2 - y1 })
    }
    /// The smallest rectangle containing both.
    pub fn union(&self, other: Rect) -> Rect {
        let x1 = self.left().min(other.left());
        let y1 = self.top().min(other.top());
        let x2 = self.right().max(other.right());
        let y2 = self.bottom().max(other.bottom());
        Rect::new(x1, y1, (x2 as i64 - x1 as i64) as u32, (y2 as i64 - y1 as i64) as u32)
    }
}

impl From<(i32, i32, u32, u32)> for Rect {
    fn from((x, y, w, h): (i32, i32, u32, u32)) -> Rect {
        Rect::new(x, y, w, h)
    }
}
impl From<Rect> for (i32, i32, u32, u32) {
    fn from(r: Rect) -> (i32, i32, u32, u32) {
        (r.x, r.y, r.width(), r.height())
    }
}
impl BitAnd for Rect {
    type Output = Option<Rect>;
    fn bitand(self, o: Rect) -> Option<Rect> {
        self.intersection(o)
    }
}
impl BitOr for Rect {
    type Output = Rect;
    fn bitor(self, o: Rect) -> Rect {
        self.union(o)
    }
}

/// A clip rectangle as `sdl2::render::ClippingRect`: a rectangle, an empty area
/// (nothing is drawn) or no clipping at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipRect {
    Some(Rect),
    Zero,
    None,
}

impl From<Rect> for ClipRect {
    fn from(r: Rect) -> ClipRect {
        ClipRect::Some(r)
    }
}
impl From<Option<Rect>> for ClipRect {
    fn from(r: Option<Rect>) -> ClipRect {
        match r {
            Some(r) => ClipRect::Some(r),
            None => ClipRect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_sizes_become_one_and_edges_are_exclusive() {
        let r = Rect::new(1, 2, 0, 0);
        assert_eq!((r.width(), r.height()), (1, 1));
        let r = Rect::new(1, 2, 3, 4);
        assert_eq!((r.right(), r.bottom()), (4, 6));
        assert!(r.contains_point((3, 5)) && !r.contains_point((4, 6)) && !r.contains_point((0, 2)));
        assert_eq!(Rect::new(1, 0, 2, 3).center(), Point::new(2, 1));
    }

    #[test]
    fn intersection_and_union() {
        let r = Rect::new(0, 0, 5, 5);
        assert_eq!(r.intersection(Rect::new(2, 2, 5, 5)), Some(Rect::new(2, 2, 3, 3)));
        assert_eq!(r.intersection(Rect::new(5, 0, 5, 5)), None);
        assert_eq!(r.union(Rect::new(2, 2, 5, 5)), Rect::new(0, 0, 7, 7));
    }
}
