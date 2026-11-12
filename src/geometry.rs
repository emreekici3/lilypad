//! Basic integer geometry in screen points (macOS global coordinates:
//! origin at the top-left of the main display, y grows downwards).

/// A window, as the window system numbers it.
pub type WinId = u32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// The part of this rect a grid cell covers: `[rows, cols, x, y, w, h]`.
    /// Edges are rounded so neighbouring cells meet.
    pub fn grid_cell(&self, [rows, cols, x, y, w, h]: [u32; 6]) -> Rect {
        let cols = i64::from(cols.max(1));
        let rows = i64::from(rows.max(1));
        let col = |i: u32| self.x + (i64::from(self.w) * i64::from(i) / cols) as i32;
        let row = |i: u32| self.y + (i64::from(self.h) * i64::from(i) / rows) as i32;
        let (x0, y0) = (col(x), row(y));
        Rect::new(x0, y0, col(x + w) - x0, row(y + h) - y0)
    }

    /// Where a `w` x `h` window goes in `cell` (a part of `self`) when it
    /// cannot take the cell's size: against each edge of `self` the cell
    /// touches, centred in the cell along the others, and kept inside `self`.
    pub fn align_in(&self, cell: Rect, w: i32, h: i32) -> Rect {
        let along = |start: i32, size: i32, len: i32, lo: i32, hi: i32| {
            let (at_start, at_end) = (start <= lo, start + size >= hi);
            let pos = match (at_start, at_end) {
                (true, false) => start,
                (false, true) => start + size - len,
                _ => start + (size - len) / 2,
            };
            pos.min(hi - len).max(lo)
        };
        Rect::new(
            along(cell.x, cell.w, w, self.x, self.max_x()),
            along(cell.y, cell.h, h, self.y, self.max_y()),
            w,
            h,
        )
    }

    pub const fn max_x(&self) -> i32 {
        self.x + self.w
    }

    pub const fn max_y(&self) -> i32 {
        self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (
            self.x as f32 + self.w as f32 / 2.0,
            self.y as f32 + self.h as f32 / 2.0,
        )
    }

    /// Shrinks the rect by `by` on every side. Never returns a negative size.
    pub fn inset(&self, by: i32) -> Rect {
        Rect {
            x: self.x + by,
            y: self.y + by,
            w: (self.w - 2 * by).max(1),
            h: (self.h - 2 * by).max(1),
        }
    }

    /// Whether the two rects overlap (touching edges do not count).
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.max_x()
            && other.x < self.max_x()
            && self.y < other.max_y()
            && other.y < self.max_y()
    }

    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        x >= self.x as f32
            && x < self.max_x() as f32
            && y >= self.y as f32
            && y < self.max_y() as f32
    }
}

/// A cardinal direction, used by the move, size, focus and snap commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "left" | "west" | "h" => Some(Self::Left),
            "right" | "east" | "l" => Some(Self::Right),
            "up" | "north" | "k" => Some(Self::Up),
            "down" | "south" | "j" => Some(Self::Down),
            _ => None,
        }
    }
}

/// The screen edge the Dock is attached to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockEdge {
    Bottom,
    Left,
    Right,
}

/// A Dock that is shown (not set to hide itself): its edge, and how many
/// points of that edge it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dock {
    pub edge: DockEdge,
    pub thickness: i32,
}

/// The area windows may use on a display: below the menu bar, whose bottom
/// is taken from `visible` (the system's visible frame), and clear of `dock`.
/// With `dock` set to `None` (it hides itself, or sits on another display)
/// windows reach the display's other three edges.
pub fn usable_area(display: Rect, visible: Rect, dock: Option<Dock>) -> Rect {
    let top = visible.y.clamp(display.y, display.max_y() - 1);
    let mut r = Rect::new(display.x, top, display.w, display.max_y() - top);
    if let Some(d) = dock {
        let t = d.thickness.clamp(0, r.w.min(r.h) / 2);
        match d.edge {
            DockEdge::Bottom => r.h -= t,
            DockEdge::Left => {
                r.x += t;
                r.w -= t;
            }
            DockEdge::Right => r.w -= t,
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_cells() {
        let area = Rect::new(10, 30, 1000, 600);
        assert_eq!(area.grid_cell([1, 1, 0, 0, 1, 1]), area);
        assert_eq!(
            area.grid_cell([1, 2, 1, 0, 1, 1]),
            Rect::new(510, 30, 500, 600)
        );
        assert_eq!(
            area.grid_cell([4, 4, 3, 0, 1, 1]),
            Rect::new(760, 30, 250, 150)
        );
        let odd = Rect::new(0, 0, 100, 100);
        let a = odd.grid_cell([1, 3, 0, 0, 1, 1]);
        let b = odd.grid_cell([1, 3, 1, 0, 1, 1]);
        let c = odd.grid_cell([1, 3, 2, 0, 1, 1]);
        assert_eq!((a.max_x(), b.max_x(), c.max_x()), (b.x, c.x, 100));
    }

    const DISPLAY: Rect = Rect::new(0, 0, 1512, 982);
    /// Menu bar 33pt, Dock 70pt at the bottom.
    const VISIBLE: Rect = Rect::new(0, 33, 1512, 879);

    #[test]
    fn hidden_dock_frees_the_bottom() {
        assert_eq!(
            usable_area(DISPLAY, VISIBLE, None),
            Rect::new(0, 33, 1512, 949)
        );
    }

    #[test]
    fn shown_dock_is_kept_clear() {
        let bottom = Dock {
            edge: DockEdge::Bottom,
            thickness: 70,
        };
        assert_eq!(
            usable_area(DISPLAY, VISIBLE, Some(bottom)),
            Rect::new(0, 33, 1512, 879)
        );
        let left = Dock {
            edge: DockEdge::Left,
            thickness: 60,
        };
        assert_eq!(
            usable_area(DISPLAY, VISIBLE, Some(left)),
            Rect::new(60, 33, 1452, 949)
        );
        let right = Dock {
            edge: DockEdge::Right,
            thickness: 60,
        };
        assert_eq!(
            usable_area(DISPLAY, VISIBLE, Some(right)),
            Rect::new(0, 33, 1452, 949)
        );
    }

    #[test]
    fn stale_visible_frame_does_not_matter() {
        // A visible frame that still leaves room for a Dock that now hides.
        let stale = Rect::new(0, 33, 1512, 700);
        assert_eq!(usable_area(DISPLAY, stale, None).max_y(), 982);
    }

    #[test]
    fn absurd_dock_is_bounded() {
        let huge = Dock {
            edge: DockEdge::Bottom,
            thickness: 5000,
        };
        assert!(usable_area(DISPLAY, VISIBLE, Some(huge)).h > 0);
    }
}
