//! Desk: windows on a screen bigger than the Cardputer Zero's.
//!
//! On a 320 x 170 screen Harbour is one full-screen app at a time. Given a bigger screen
//! (`HARBOUR_SCREEN=WxH`), the host shows a desk instead: a menu strip with the status icons across
//! the top, and every open screen (Home, an app, Settings...) in a window of its own.
//!
//! Home is the desk itself: its icons sit on the desk, in columns from the right edge. A window's
//! inside is a Harbour screen whose top 16 px row is the window's title bar (a close box and the
//! name); the menus live in the desk's strip and belong to the focused window (or Home). An app that is drawn pixel by pixel for 320 x 170
//! ([`Sizing::Fixed`](crate::launcher::app::Sizing)) keeps that size; one that lays itself out from
//! the surface ([`Sizing::Flexible`](crate::launcher::app::Sizing)) gets a bigger window that can be
//! resized from its bottom-right corner.
//!
//! This module is the geometry and the pointer gestures only (placement, stacking, focus, moving
//! and resizing); what each window shows (`T`) belongs to the host.

use sdl2::rect::Rect;

use crate::grid;

/// The frame drawn around a window's inside: 1 px line, plus a 1 px shadow right and below.
pub const BORDER: i32 = 1;
pub const SHADOW: i32 = 1;
/// The corner a flexible window is resized from (inside the window).
pub const GRIP: i32 = 10;
/// The close box at the left of a title bar (window coordinates).
pub const CLOSE_BOX: (i32, i32, i32) = (3, 3, 10);
/// A press on the title strip turns into a move once the pointer has gone this far.
const DRAG_SLOP: i32 = 3;
/// Where new windows go: each one this much right and down of the last.
const CASCADE: i32 = 16;

pub type WinId = u32;

pub struct Win<T> {
    pub id: WinId,
    /// The inside: the surface the window's screen is drawn on, in desk coordinates.
    pub rect: Rect,
    pub flexible: bool,
    /// What the window shows. The host keeps the focused window's screen elsewhere (None here).
    pub slot: Option<T>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gesture {
    /// Pressed on the title strip: a click unless the pointer moves.
    Title { id: WinId, at: (i32, i32), origin: (i32, i32) },
    Move { id: WinId, grab: (i32, i32) },
    Resize { id: WinId, grab: (i32, i32), size: (i32, i32) },
    /// Pressed on a close box: it closes when released on it.
    Close { id: WinId },
}

/// What a press on the desk means for the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// Inside the focused window: hand it to the screen (at these window coordinates).
    Pass(i32, i32),
    /// Another window was pressed: it becomes the focused one (the press only focuses).
    Focus(WinId),
    /// The empty desk (Home's icons): Home is now focused; hand the press to Home at desk coordinates.
    Desk,
    /// The desk's menu strip: hand it to the focused screen's strip at desk coordinates.
    Bar,
    /// The desk handles it (title bar, close box, resize grip, frame).
    Taken,
}

/// What a release means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// Not ours: hand it to the screen.
    Pass,
    /// The close box was pressed and released: close this window.
    Close(WinId),
    /// A click on a title bar (no move): the window's menu opens here (desk coordinates).
    Title(i32, i32),
    /// The end of a move, a resize or a click on the title bar.
    Taken,
}

pub struct Desk<T> {
    pub size: (i32, i32),
    /// Bottom to top: the last one is drawn last and is the focused window.
    wins: Vec<Win<T>>,
    next_id: WinId,
    gesture: Option<Gesture>,
    next_place: (i32, i32),
    /// Home (the desk) has the keyboard rather than a window.
    pub desk_focus: bool,
    /// Width kept free at the right edge for Home's icons: new windows open left of it.
    pub reserve_right: i32,
}

impl<T> Desk<T> {
    pub fn new(size: (i32, i32)) -> Self {
        Self { size, wins: Vec::new(), next_id: 1, gesture: None, next_place: Self::first_place(), desk_focus: true, reserve_right: 0 }
    }

    fn first_place() -> (i32, i32) {
        (8, grid::BAR_H + 8)
    }

    /// True on a screen bigger than the Cardputer Zero's in either direction.
    pub fn wanted(size: (i32, i32)) -> bool {
        size.0 > grid::SCREEN_W || size.1 > grid::SCREEN_H
    }

    /// The area windows live in: below the desk's menu strip.
    pub fn area(&self) -> Rect {
        Rect::new(0, grid::BAR_H, self.size.0 as u32, (self.size.1 - grid::BAR_H).max(1) as u32)
    }

    pub fn len(&self) -> usize {
        self.wins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.wins.is_empty()
    }

    /// Bottom to top.
    pub fn windows(&self) -> impl Iterator<Item = &Win<T>> {
        self.wins.iter()
    }

    pub fn windows_mut(&mut self) -> impl Iterator<Item = &mut Win<T>> {
        self.wins.iter_mut()
    }

    pub fn get(&self, id: WinId) -> Option<&Win<T>> {
        self.wins.iter().find(|w| w.id == id)
    }

    pub fn get_mut(&mut self, id: WinId) -> Option<&mut Win<T>> {
        self.wins.iter_mut().find(|w| w.id == id)
    }

    /// The focused window; None while Home (the desk) has the focus.
    pub fn focused(&self) -> Option<WinId> {
        if self.desk_focus {
            return None;
        }
        self.wins.last().map(|w| w.id)
    }

    pub fn focused_rect(&self) -> Option<Rect> {
        self.focused().and_then(|id| self.get(id)).map(|w| w.rect)
    }

    /// The size a new window gets: 320 x 170 for a fixed one; a flexible one three quarters of the
    /// free area, at most 640 x 340 (never less than 320 x 170).
    pub fn initial_size(&self, flexible: bool) -> (i32, i32) {
        if !flexible {
            return (grid::SCREEN_W, grid::SCREEN_H);
        }
        // Three quarters of the free area, at most twice the design size, so on a big desk the next
        // windows still find room beside it.
        let a = self.free_area();
        let w = (a.width() as i32 * 3 / 4).min(2 * grid::SCREEN_W).max(grid::SCREEN_W);
        let h = (a.height() as i32 * 3 / 4).min(2 * grid::SCREEN_H).max(grid::SCREEN_H);
        self.clamp_size((w, h))
    }

    /// Where new windows go: the window area minus the icons' columns (all of it when that leaves
    /// less than one 320 px window).
    fn free_area(&self) -> Rect {
        let a = self.area();
        let w = a.width() as i32 - self.reserve_right;
        if w < grid::SCREEN_W + 2 * (BORDER + SHADOW) + 8 {
            return a;
        }
        Rect::new(a.x(), a.y(), w as u32, a.height())
    }

    fn clamp_size(&self, (w, h): (i32, i32)) -> (i32, i32) {
        let a = self.area();
        let max_w = (a.width() as i32 - 2 * (BORDER + SHADOW)).max(grid::SCREEN_W);
        let max_h = (a.height() as i32 - 2 * (BORDER + SHADOW)).max(grid::SCREEN_H);
        (w.clamp(grid::SCREEN_W, max_w), h.clamp(grid::SCREEN_H, max_h))
    }

    /// Open a window (on top, focused) of `size`: in the first free place, scanning from the
    /// top-left of the free area; when nothing is free, cascading from the last one. Returns its id.
    pub fn open(&mut self, size: (i32, i32), flexible: bool, slot: Option<T>) -> WinId {
        let size = self.clamp_size(size);
        let (x, y) = self.free_place(size).unwrap_or_else(|| self.cascade_place(size));
        self.next_place = (x + CASCADE, y + CASCADE);
        let id = self.next_id;
        self.next_id += 1;
        let rect = self.keep_on_desk(Rect::new(x, y, size.0 as u32, size.1 as u32));
        self.wins.push(Win { id, rect, flexible, slot });
        self.desk_focus = false;
        id
    }

    /// The first place (16 px steps, rows first) where a window of `size` touches no other window.
    fn free_place(&self, size: (i32, i32)) -> Option<(i32, i32)> {
        let a = self.free_area();
        let (x0, y0) = Self::first_place();
        let gap = 4;
        let mut y = y0;
        while y + size.1 + BORDER + SHADOW <= a.bottom() {
            let mut x = x0;
            while x + size.0 + BORDER + SHADOW <= a.right() {
                let r = outer(Rect::new(x, y, size.0 as u32, size.1 as u32));
                let grown = Rect::new(r.x() - gap, r.y() - gap, r.width() + 2 * gap as u32, r.height() + 2 * gap as u32);
                if self.wins.iter().all(|w| !grown.has_intersection(outer(w.rect))) {
                    return Some((x, y));
                }
                x += CASCADE;
            }
            y += CASCADE;
        }
        None
    }

    /// No free place: step down the diagonal from the top-left until no window has its corner
    /// there (so a new window never hides one exactly); wrap round to the top when it would leave
    /// the area.
    fn cascade_place(&self, size: (i32, i32)) -> (i32, i32) {
        let a = self.free_area();
        let (x0, y0) = Self::first_place();
        let fits = |x: i32, y: i32| x + size.0 + BORDER + SHADOW <= a.right() && y + size.1 + BORDER + SHADOW <= a.bottom();
        let taken = |x: i32, y: i32| self.wins.iter().any(|w| (w.rect.x(), w.rect.y()) == (x, y));
        for k in 0..64 {
            let (x, y) = (x0 + k * CASCADE, y0 + k * CASCADE);
            if !fits(x, y) {
                break;
            }
            if !taken(x, y) {
                return (x, y);
            }
        }
        // Every step is taken: next to the last opened window, or the top-left again.
        let (x, y) = self.next_place;
        if fits(x, y) { (x, y) } else { (x0, y0) }
    }

    /// Close a window. Returns what it held.
    pub fn close(&mut self, id: WinId) -> Option<Option<T>> {
        let i = self.wins.iter().position(|w| w.id == id)?;
        if matches!(self.gesture, Some(Gesture::Title { id: g, .. } | Gesture::Move { id: g, .. } | Gesture::Resize { id: g, .. }) if g == id) {
            self.gesture = None;
        }
        let slot = self.wins.remove(i).slot;
        if self.wins.is_empty() {
            self.desk_focus = true;
        }
        Some(slot)
    }

    /// Bring a window to the top (it becomes the focused one).
    pub fn raise(&mut self, id: WinId) {
        if let Some(i) = self.wins.iter().position(|w| w.id == id) {
            let w = self.wins.remove(i);
            self.wins.push(w);
            self.desk_focus = false;
        }
    }

    /// Home (the desk) takes the focus; the windows stay where they are.
    pub fn focus_desk(&mut self) {
        self.desk_focus = true;
    }

    /// Ctrl+`: the next window comes to the front; after the last one, Home (the desk); then again
    /// the window on top.
    pub fn cycle(&mut self) -> Option<WinId> {
        if self.wins.is_empty() {
            return None;
        }
        if self.desk_focus {
            self.desk_focus = false;
            return self.focused();
        }
        if self.wins.len() == 1 {
            self.desk_focus = true;
            return None;
        }
        let w = self.wins.remove(0);
        let id = w.id;
        self.wins.push(w);
        Some(id)
    }

    /// The topmost window whose frame contains (x, y).
    pub fn hit(&self, x: i32, y: i32) -> Option<WinId> {
        self.wins.iter().rev().find(|w| outer(w.rect).contains_point((x, y))).map(|w| w.id)
    }

    /// The window `id` resized to `size` (kept on the desk, never under 320 x 170).
    pub fn resize(&mut self, id: WinId, size: (i32, i32)) {
        let size = self.clamp_size(size);
        if let Some(i) = self.wins.iter().position(|w| w.id == id) {
            let r = self.wins[i].rect;
            let r = self.keep_on_desk(Rect::new(r.x(), r.y(), size.0 as u32, size.1 as u32));
            self.wins[i].rect = r;
        }
    }

    /// A rectangle moved back so its frame stays inside the window area.
    fn keep_on_desk(&self, r: Rect) -> Rect {
        let a = self.area();
        let x = r.x().min(a.right() - r.width() as i32 - BORDER - SHADOW).max(a.x() + BORDER);
        let y = r.y().min(a.bottom() - r.height() as i32 - BORDER - SHADOW).max(a.y() + BORDER);
        Rect::new(x, y, r.width(), r.height())
    }

    /// The desk changed size (a resized host window): every window stays reachable.
    pub fn set_size(&mut self, size: (i32, i32)) {
        self.size = size;
        for i in 0..self.wins.len() {
            let r = self.wins[i].rect;
            let s = self.clamp_size((r.width() as i32, r.height() as i32));
            self.wins[i].rect = self.keep_on_desk(Rect::new(r.x(), r.y(), s.0 as u32, s.1 as u32));
        }
    }

    pub fn busy(&self) -> bool {
        matches!(self.gesture, Some(Gesture::Move { .. } | Gesture::Resize { .. }))
    }

    /// A left press at desk coordinates (x, y).
    pub fn press(&mut self, x: i32, y: i32) -> Press {
        if y < grid::BAR_H {
            return Press::Bar;
        }
        let Some(id) = self.hit(x, y) else {
            self.desk_focus = true;
            return Press::Desk;
        };
        // A window's close box works whether or not it has the focus.
        if let Some(w) = self.get(id) {
            if in_close_box(w.rect, x, y) {
                self.gesture = Some(Gesture::Close { id });
                return Press::Taken;
            }
        }
        if Some(id) != self.focused() {
            self.raise(id);
            return Press::Focus(id);
        }
        let w = self.get(id).expect("hit window");
        let r = w.rect;
        let (lx, ly) = (x - r.x(), y - r.y());
        if !r.contains_point((x, y)) {
            return Press::Taken; // on the frame line or the shadow
        }
        if w.flexible && lx >= r.width() as i32 - GRIP && ly >= r.height() as i32 - GRIP {
            self.gesture = Some(Gesture::Resize { id, grab: (x, y), size: (r.width() as i32, r.height() as i32) });
            return Press::Taken;
        }
        if ly < grid::BAR_H {
            self.gesture = Some(Gesture::Title { id, at: (x, y), origin: (r.x(), r.y()) });
            return Press::Taken;
        }
        Press::Pass(lx, ly)
    }

    /// The pointer moved to (x, y). True when the desk took it (a move or a resize is going on).
    pub fn motion(&mut self, x: i32, y: i32) -> bool {
        match self.gesture {
            Some(Gesture::Close { .. }) => true,
            Some(Gesture::Title { id, at, origin }) => {
                if (x - at.0).abs() >= DRAG_SLOP || (y - at.1).abs() >= DRAG_SLOP {
                    self.gesture = Some(Gesture::Move { id, grab: (at.0 - origin.0, at.1 - origin.1) });
                    self.motion(x, y);
                }
                true
            }
            Some(Gesture::Move { id, grab }) => {
                if let Some(i) = self.wins.iter().position(|w| w.id == id) {
                    let r = self.wins[i].rect;
                    self.wins[i].rect = self.keep_on_desk(Rect::new(x - grab.0, y - grab.1, r.width(), r.height()));
                }
                true
            }
            Some(Gesture::Resize { id, grab, size }) => {
                self.resize(id, (size.0 + x - grab.0, size.1 + y - grab.1));
                true
            }
            None => false,
        }
    }

    /// The left button went up at (x, y).
    pub fn release(&mut self, x: i32, y: i32) -> Release {
        match self.gesture.take() {
            Some(Gesture::Close { id }) => match self.get(id) {
                Some(w) if in_close_box(w.rect, x, y) => Release::Close(id),
                _ => Release::Taken,
            },
            Some(Gesture::Title { .. }) => Release::Title(x, y),
            Some(_) => Release::Taken,
            None => Release::Pass,
        }
    }
}

/// True when (x, y) (desk coordinates) is on window `id`'s close box and nothing covers it there.
pub fn on_close_box<T>(d: &Desk<T>, id: WinId, x: i32, y: i32) -> bool {
    d.hit(x, y) == Some(id) && d.get(id).is_some_and(|w| in_close_box(w.rect, x, y))
}

/// The close box of a window whose inside is `r` (desk coordinates), a little larger to hit.
pub fn close_box(r: Rect) -> Rect {
    let (x, y, s) = CLOSE_BOX;
    Rect::new(r.x() + x, r.y() + y, s as u32, s as u32)
}

fn in_close_box(r: Rect, x: i32, y: i32) -> bool {
    let b = close_box(r);
    Rect::new(b.x() - 2, b.y() - 2, b.width() + 4, b.height() + 4).contains_point((x, y))
}

/// A window's inside plus its frame line and shadow.
pub fn outer(r: Rect) -> Rect {
    Rect::new(r.x() - BORDER, r.y() - BORDER, r.width() + (2 * BORDER + SHADOW) as u32, r.height() + (2 * BORDER + SHADOW) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desk() -> Desk<&'static str> {
        Desk::new((800, 480))
    }

    #[test]
    fn only_a_bigger_screen_gets_a_desk() {
        assert!(!Desk::<()>::wanted((320, 170)));
        assert!(Desk::<()>::wanted((480, 272)));
        assert!(Desk::<()>::wanted((320, 240)));
    }

    #[test]
    fn new_windows_go_to_free_space_then_cascade_and_take_focus() {
        let mut d = desk();
        d.reserve_right = 160;
        let a = d.open((320, 170), false, None);
        let b = d.open((320, 170), false, None);
        assert_eq!(d.focused(), Some(b));
        let (ra, rb) = (d.get(a).unwrap().rect, d.get(b).unwrap().rect);
        assert!(!outer(ra).has_intersection(outer(rb)), "the second one goes beside the first: {ra:?} {rb:?}");
        assert!(ra.y() > grid::BAR_H, "below the desk's strip");
        assert!(outer(rb).right() <= 800 - 160, "not over the icons");
        // The area is full now: the next ones cascade.
        let c = d.open((320, 170), false, None);
        let e = d.open((320, 170), false, None);
        let f = d.open((320, 170), false, None);
        let (rc, re, rf) = (d.get(c).unwrap().rect, d.get(e).unwrap().rect, d.get(f).unwrap().rect);
        assert!(rc.y() > rb.y() || rc.x() != rb.x());
        assert_eq!((rf.x() - re.x(), rf.y() - re.y()), (CASCADE, CASCADE));
    }

    #[test]
    fn fixed_windows_are_320_by_170_flexible_ones_bigger_and_clamped() {
        let d = desk();
        assert_eq!(d.initial_size(false), (320, 170));
        let (w, h) = d.initial_size(true);
        assert!(w > 320 && h > 170 && w < 800 && h < 480);
        let small: Desk<()> = Desk::new((330, 200));
        assert_eq!(small.initial_size(true), (320, 170), "never under the design size");
    }

    #[test]
    fn a_press_focuses_a_back_window_and_passes_inside_the_front_one() {
        let mut d = desk();
        assert_eq!(d.focused(), None, "Home (the desk) has the focus at first");
        let a = d.open((320, 170), false, Some("a"));
        let b = d.open((320, 170), false, Some("b"));
        let ra = d.get(a).unwrap().rect;
        // A point of `a` that `b` (cascaded right/down) does not cover.
        assert_eq!(d.press(ra.x() + 2, ra.y() + 30), Press::Focus(a));
        assert_eq!(d.focused(), Some(a));
        assert_eq!(d.press(ra.x() + 5, ra.y() + 40), Press::Pass(5, 40));
        assert_eq!(d.release(ra.x() + 5, ra.y() + 40), Release::Pass);
        assert_eq!(d.cycle(), Some(b));
        assert_eq!(d.focused(), Some(b));
        assert_eq!(d.press(790, 470), Press::Desk, "the empty desk gives Home the focus");
        assert_eq!(d.focused(), None);
        assert_eq!(d.press(400, 4), Press::Bar);
        assert_eq!(d.cycle(), Some(b), "from Home, Ctrl+` goes back to the window on top");
    }

    #[test]
    fn the_close_box_closes_on_release_and_the_title_bar_drags() {
        let mut d = desk();
        let a = d.open((320, 170), false, None);
        let r = d.get(a).unwrap().rect;
        let b = close_box(r);
        assert_eq!(d.press(b.x() + 2, b.y() + 2), Press::Taken);
        assert_eq!(d.release(b.x() + 2, b.y() + 2), Release::Close(a));
        d.press(b.x() + 2, b.y() + 2);
        assert_eq!(d.release(b.x() + 60, b.y() + 60), Release::Taken, "released elsewhere: nothing");
        assert_eq!(d.press(r.x() + 100, r.y() + 4), Press::Taken);
        assert_eq!(d.release(r.x() + 100, r.y() + 4), Release::Title(r.x() + 100, r.y() + 4), "a click on the title bar opens its menu there");
        d.press(r.x() + 100, r.y() + 4);
        assert!(d.motion(r.x() + 130, r.y() + 44));
        assert!(d.busy());
        assert_eq!(d.release(r.x() + 130, r.y() + 44), Release::Taken);
        let m = d.get(a).unwrap().rect;
        assert_eq!((m.x(), m.y()), (r.x() + 30, r.y() + 40));
    }

    #[test]
    fn windows_stay_on_the_desk_and_resize_from_the_grip() {
        let mut d = desk();
        let a = d.open((400, 250), true, None);
        let r = d.get(a).unwrap().rect;
        d.press(r.x() + 50, r.y() + 4);
        d.motion(5000, 5000);
        d.release(5000, 5000);
        let m = d.get(a).unwrap().rect;
        assert!(outer(m).right() <= 800 && outer(m).bottom() <= 480, "{m:?}");
        // Grip: bottom-right corner of the inside.
        let g = (m.right() - 3, m.bottom() - 3);
        assert_eq!(d.press(g.0, g.1), Press::Taken);
        d.motion(g.0 - 1000, g.1 - 1000);
        d.release(0, 0);
        let s = d.get(a).unwrap().rect;
        assert_eq!((s.width(), s.height()), (320, 170), "never under the design size");
        let f = d.open((320, 170), false, None);
        let fr = d.get(f).unwrap().rect;
        assert!(matches!(d.press(fr.right() - 3, fr.bottom() - 3), Press::Pass(..)), "fixed windows have no grip");
    }

    #[test]
    fn closing_returns_the_slot() {
        let mut d = desk();
        let a = d.open((320, 170), false, Some("home"));
        let b = d.open((320, 170), false, None);
        assert_eq!(d.close(b), Some(None));
        assert_eq!(d.focused(), Some(a));
        assert_eq!(d.close(a), Some(Some("home")));
        assert!(d.is_empty());
        assert_eq!(d.focused(), None, "no windows left: Home has the focus");
    }
}
