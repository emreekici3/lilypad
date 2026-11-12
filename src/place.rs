//! Where windows go. Plain geometry: no window system, nothing remembered.

use crate::geometry::{Direction, Rect, WinId};

/// The smallest a window can be made from the keyboard.
pub const MIN_SIZE: i32 = 50;

/// A place on the display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Center,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Max,
}

impl Position {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "center" | "centre" => Self::Center,
            "left" => Self::Left,
            "right" => Self::Right,
            "top" | "up" => Self::Top,
            "bottom" | "down" => Self::Bottom,
            "top-left" => Self::TopLeft,
            "top-right" => Self::TopRight,
            "bottom-left" => Self::BottomLeft,
            "bottom-right" => Self::BottomRight,
            "max" => Self::Max,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
            Self::Max => "max",
        }
    }

    /// Where a window of `current`'s size goes in `area`. `percent` is its
    /// share of the area: of the width at the sides, of the height at the
    /// top and bottom, of both in the corners and the centre. Without one,
    /// half the area, except that the centre keeps the window's size.
    pub fn rect(self, percent: Option<u32>, area: Rect, current: Rect) -> Rect {
        let share = |len: i32| {
            let p = i64::from(percent.unwrap_or(50).clamp(1, 100));
            (i64::from(len) * p / 100) as i32
        };
        let (w, h) = (share(area.w), share(area.h));
        let right = area.max_x() - w;
        let bottom = area.max_y() - h;
        match self {
            Self::Max => area,
            Self::Left => Rect::new(area.x, area.y, w, area.h),
            Self::Right => Rect::new(right, area.y, w, area.h),
            Self::Top => Rect::new(area.x, area.y, area.w, h),
            Self::Bottom => Rect::new(area.x, bottom, area.w, h),
            Self::TopLeft => Rect::new(area.x, area.y, w, h),
            Self::TopRight => Rect::new(right, area.y, w, h),
            Self::BottomLeft => Rect::new(area.x, bottom, w, h),
            Self::BottomRight => Rect::new(right, bottom, w, h),
            Self::Center => {
                let (w, h) = match percent {
                    Some(_) => (w, h),
                    None => (current.w.min(area.w), current.h.min(area.h)),
                };
                Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h)
            }
        }
    }
}

/// `frame` moved `step` points toward `dir`, no further than the edge of
/// `area`. A window already past that edge does not get pulled in.
pub fn moved(frame: Rect, dir: Direction, step: i32, area: Rect) -> Rect {
    let Rect { x, y, w, h } = frame;
    match dir {
        Direction::Left => Rect::new((x - step).max(area.x.min(x)), y, w, h),
        Direction::Up => Rect::new(x, (y - step).max(area.y.min(y)), w, h),
        Direction::Right => Rect::new((x + step).min((area.max_x() - w).max(x)), y, w, h),
        Direction::Down => Rect::new(x, (y + step).min((area.max_y() - h).max(y)), w, h),
    }
}

/// `frame` with its `dir` edge moved `step` points outward (inward for a
/// negative step), no further out than the edge of `area` and no further in
/// than `min`. The opposite edge stays put.
pub fn sized(frame: Rect, dir: Direction, step: i32, min: (i32, i32), area: Rect) -> Rect {
    let Rect { x, y, w, h } = frame;
    let len = |now: i32, room: i32, least: i32| (now + step).min(room.max(now)).max(least.min(now));
    match dir {
        Direction::Right => Rect::new(x, y, len(w, area.max_x() - x, min.0), h),
        Direction::Down => Rect::new(x, y, w, len(h, area.max_y() - y, min.1)),
        Direction::Left => {
            let w2 = len(w, x + w - area.x, min.0);
            Rect::new(x + w - w2, y, w2, h)
        }
        Direction::Up => {
            let h2 = len(h, y + h - area.y, min.1);
            Rect::new(x, y + h - h2, w, h2)
        }
    }
}

/// `frame` slid toward `dir` until it meets the nearest window ahead (`gap`
/// points short of it) or the edge of `area`. Only windows beside its path
/// count; one it already overlaps is not in its way.
pub fn snapped(frame: Rect, dir: Direction, area: Rect, others: &[Rect], gap: i32) -> Rect {
    let across = |o: &Rect| match dir {
        Direction::Left | Direction::Right => o.y < frame.max_y() && frame.y < o.max_y(),
        Direction::Up | Direction::Down => o.x < frame.max_x() && frame.x < o.max_x(),
    };
    let blockers = others.iter().filter(|o| across(o));
    let mut r = frame;
    match dir {
        Direction::Left => {
            let stop = blockers
                .map(|o| o.max_x() + gap)
                .filter(|s| *s <= frame.x)
                .fold(area.x, i32::max);
            r.x = stop;
        }
        Direction::Right => {
            let stop = blockers
                .map(|o| o.x - gap)
                .filter(|s| *s >= frame.max_x())
                .fold(area.max_x(), i32::min);
            r.x = stop - frame.w;
        }
        Direction::Up => {
            let stop = blockers
                .map(|o| o.max_y() + gap)
                .filter(|s| *s <= frame.y)
                .fold(area.y, i32::max);
            r.y = stop;
        }
        Direction::Down => {
            let stop = blockers
                .map(|o| o.y - gap)
                .filter(|s| *s >= frame.max_y())
                .fold(area.max_y(), i32::min);
            r.y = stop - frame.h;
        }
    }
    r
}

/// The window to focus going `dir` from `frame`: the one whose centre is
/// that way, nearest, counting distance off the line twice.
pub fn neighbour(frame: Rect, dir: Direction, others: &[(WinId, Rect)]) -> Option<WinId> {
    let centre = |r: &Rect| (2 * r.x + r.w, 2 * r.y + r.h);
    let (cx, cy) = centre(&frame);
    others
        .iter()
        .filter_map(|(id, r)| {
            let (x, y) = centre(r);
            let (ahead, off) = match dir {
                Direction::Left => (cx - x, y - cy),
                Direction::Right => (x - cx, y - cy),
                Direction::Up => (cy - y, x - cx),
                Direction::Down => (y - cy, x - cx),
            };
            (ahead > 0).then_some((i64::from(ahead) + 2 * i64::from(off.abs()), *id))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 30, 1000, 600);

    #[test]
    fn sides_take_their_share_of_one_axis() {
        let any = Rect::new(5, 5, 100, 100);
        assert_eq!(
            Position::Left.rect(None, AREA, any),
            Rect::new(0, 30, 500, 600)
        );
        assert_eq!(
            Position::Right.rect(Some(60), AREA, any),
            Rect::new(400, 30, 600, 600)
        );
        assert_eq!(
            Position::Top.rect(Some(25), AREA, any),
            Rect::new(0, 30, 1000, 150)
        );
        assert_eq!(
            Position::Bottom.rect(None, AREA, any),
            Rect::new(0, 330, 1000, 300)
        );
    }

    #[test]
    fn corners_take_their_share_of_both() {
        let any = Rect::new(5, 5, 100, 100);
        assert_eq!(
            Position::BottomRight.rect(None, AREA, any),
            Rect::new(500, 330, 500, 300)
        );
        assert_eq!(
            Position::TopLeft.rect(Some(40), AREA, any),
            Rect::new(0, 30, 400, 240)
        );
    }

    #[test]
    fn the_centre_keeps_the_size_unless_told_otherwise() {
        let w = Rect::new(5, 5, 200, 100);
        assert_eq!(
            Position::Center.rect(None, AREA, w),
            Rect::new(400, 280, 200, 100)
        );
        assert_eq!(
            Position::Center.rect(Some(50), AREA, w),
            Rect::new(250, 180, 500, 300)
        );
        // Bigger than the display: it comes back inside.
        let huge = Rect::new(0, 0, 3000, 2000);
        assert_eq!(Position::Center.rect(None, AREA, huge), AREA);
    }

    #[test]
    fn max_is_the_whole_area() {
        assert_eq!(Position::Max.rect(Some(10), AREA, AREA), AREA);
    }

    #[test]
    fn positions_read_back_as_written() {
        for name in [
            "center",
            "left",
            "right",
            "top",
            "bottom",
            "top-left",
            "top-right",
            "bottom-left",
            "bottom-right",
            "max",
        ] {
            assert_eq!(Position::parse(name).unwrap().name(), name);
        }
        assert_eq!(Position::parse("middle"), None);
    }

    const MIN: (i32, i32) = (MIN_SIZE, MIN_SIZE);

    #[test]
    fn a_move_changes_only_the_position() {
        let f = Rect::new(100, 100, 300, 200);
        assert_eq!(
            moved(f, Direction::Left, 40, AREA),
            Rect::new(60, 100, 300, 200)
        );
        assert_eq!(
            moved(f, Direction::Down, 40, AREA),
            Rect::new(100, 140, 300, 200)
        );
    }

    #[test]
    fn a_move_stops_at_every_edge_of_the_area() {
        let f = Rect::new(20, 40, 300, 200);
        assert_eq!(moved(f, Direction::Left, 40, AREA).x, 0);
        assert_eq!(moved(f, Direction::Up, 40, AREA).y, 30);
        let g = Rect::new(680, 420, 300, 200);
        assert_eq!(moved(g, Direction::Right, 40, AREA).max_x(), 1000);
        assert_eq!(moved(g, Direction::Down, 40, AREA).max_y(), 630);
    }

    #[test]
    fn a_window_already_past_an_edge_is_not_pulled_in_by_moving_along_it() {
        let f = Rect::new(-50, 100, 300, 200);
        assert_eq!(moved(f, Direction::Left, 40, AREA).x, -50);
        // Bigger than the area: it has nowhere to go, and does not jump.
        let big = Rect::new(0, 30, 1200, 200);
        assert_eq!(moved(big, Direction::Right, 40, AREA).x, 0);
    }

    #[test]
    fn sizing_keeps_the_opposite_edge_still() {
        let f = Rect::new(100, 100, 300, 200);
        assert_eq!(
            sized(f, Direction::Right, 40, MIN, AREA),
            Rect::new(100, 100, 340, 200)
        );
        assert_eq!(
            sized(f, Direction::Left, 40, MIN, AREA),
            Rect::new(60, 100, 340, 200)
        );
        assert_eq!(
            sized(f, Direction::Up, 40, MIN, AREA),
            Rect::new(100, 60, 300, 240)
        );
        assert_eq!(
            sized(f, Direction::Down, -40, MIN, AREA),
            Rect::new(100, 100, 300, 160)
        );
        assert_eq!(sized(f, Direction::Left, -40, MIN, AREA).max_x(), f.max_x());
    }

    #[test]
    fn sizing_stops_at_the_smallest_size_without_pushing_the_far_edge() {
        let f = Rect::new(100, 100, 300, 200);
        let small = sized(f, Direction::Left, -1000, MIN, AREA);
        assert_eq!(small.w, MIN_SIZE);
        assert_eq!(small.max_x(), f.max_x());
        assert_eq!(sized(f, Direction::Down, -1000, MIN, AREA).h, MIN_SIZE);
    }

    #[test]
    fn a_window_at_its_own_minimum_does_not_move_at_all() {
        let f = Rect::new(100, 100, 300, 200);
        assert_eq!(sized(f, Direction::Left, -40, (300, 50), AREA), f);
        assert_eq!(sized(f, Direction::Up, -40, (50, 200), AREA), f);
    }

    #[test]
    fn sizing_stops_at_every_edge_of_the_area() {
        let f = Rect::new(10, 40, 980, 580);
        assert_eq!(sized(f, Direction::Left, 40, MIN, AREA).x, 0);
        assert_eq!(sized(f, Direction::Up, 40, MIN, AREA).y, 30);
        assert_eq!(sized(f, Direction::Right, 40, MIN, AREA).max_x(), 1000);
        assert_eq!(sized(f, Direction::Down, 40, MIN, AREA).max_y(), 630);
        // A window at the edge has no more to give, and stays exactly there.
        let full = Rect::new(0, 30, 1000, 600);
        assert_eq!(sized(full, Direction::Left, 40, MIN, AREA), full);
    }

    #[test]
    fn a_window_already_past_an_edge_can_still_shrink() {
        let f = Rect::new(0, 30, 1200, 200);
        assert_eq!(sized(f, Direction::Right, -40, MIN, AREA).w, 1160);
        assert_eq!(sized(f, Direction::Right, 40, MIN, AREA).w, 1200);
    }

    #[test]
    fn snap_goes_to_the_edge_of_the_area() {
        let f = Rect::new(100, 100, 300, 200);
        assert_eq!(snapped(f, Direction::Left, AREA, &[], 8).x, 0);
        assert_eq!(snapped(f, Direction::Right, AREA, &[], 8).max_x(), 1000);
        assert_eq!(snapped(f, Direction::Up, AREA, &[], 8).y, 30);
        assert_eq!(snapped(f, Direction::Down, AREA, &[], 8).max_y(), 630);
    }

    #[test]
    fn snap_stops_short_of_the_window_in_the_way() {
        let f = Rect::new(100, 100, 300, 200);
        let near = Rect::new(600, 150, 200, 100);
        let far = Rect::new(850, 150, 100, 100);
        let r = snapped(f, Direction::Right, AREA, &[far, near], 8);
        assert_eq!(r.max_x(), 592);
        assert_eq!((r.y, r.w, r.h), (100, 300, 200));
    }

    #[test]
    fn snap_ignores_windows_off_its_path_and_those_it_overlaps() {
        let f = Rect::new(100, 100, 300, 200);
        let beside = Rect::new(600, 400, 200, 100);
        let overlapped = Rect::new(350, 150, 100, 100);
        let r = snapped(f, Direction::Right, AREA, &[beside, overlapped], 8);
        assert_eq!(r.max_x(), 1000);
    }

    #[test]
    fn snap_next_to_a_neighbour_stays_there() {
        let f = Rect::new(100, 100, 300, 200);
        let next = Rect::new(408, 100, 300, 200);
        assert_eq!(snapped(f, Direction::Right, AREA, &[next], 8), f);
    }

    #[test]
    fn neighbour_prefers_what_is_straight_ahead() {
        let f = Rect::new(400, 200, 200, 200);
        let others = [
            (1, Rect::new(700, 200, 200, 200)),
            (2, Rect::new(620, 500, 200, 100)),
            (3, Rect::new(50, 200, 200, 200)),
        ];
        assert_eq!(neighbour(f, Direction::Right, &others), Some(1));
        assert_eq!(neighbour(f, Direction::Left, &others), Some(3));
        assert_eq!(neighbour(f, Direction::Down, &others), Some(2));
        assert_eq!(neighbour(f, Direction::Up, &others), None);
    }
}
