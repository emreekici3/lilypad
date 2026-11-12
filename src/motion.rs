//! Window motion.
//!
//! A window is four edges, not a position and a size. Each edge goes from
//! where it was to its goal along macOS's own timing curve (Core Animation's
//! default, the cubic Bézier `0.25, 0.1, 0.25, 1`): a short soft start, most
//! of the way early, and a long gentle landing.
//!
//! * The curve is a function of the time gone by, so the path does not
//!   depend on the frame rate.
//! * It never overshoots. Sent somewhere new in flight, a window heads there
//!   from where it is, on a new curve.
//!
//! Edges are also what makes the rounding to whole points clean. Rounding
//! `x` and `w` apart lets a still edge wobble by a point; here a still edge
//! is the exact number it started as, and a window only moving keeps its
//! exact size, so it is never asked to lay itself out again.

use std::collections::HashMap;

use crate::geometry::{Rect, WinId};

/// Largest step a single frame may take, in seconds. A late frame still
/// covers the time that went by; only a real stall is cut short.
const MAX_STEP: f64 = 0.05;
/// Core Animation's default timing curve: control points of a cubic Bézier
/// from `(0, 0)` to `(1, 1)`.
const CURVE: (f64, f64, f64, f64) = (0.25, 0.1, 0.25, 1.0);
/// How far a frame's time may be from a whole number of refreshes and still
/// count as exactly that many, as a share of a refresh.
const PACE_TOLERANCE: f64 = 0.2;

/// The time a frame stands for. The clock read at each frame wobbles by
/// a fraction of a millisecond; the display shows whole refreshes, so steps
/// taken in whole refreshes come out even.
pub fn paced(dt: f64, refresh: f64) -> f64 {
    if refresh <= 0.0 {
        return dt;
    }
    let frames = (dt / refresh).round().max(1.0);
    let whole = frames * refresh;
    if (dt - whole).abs() <= PACE_TOLERANCE * refresh {
        whole
    } else {
        dt
    }
}

/// How far along the curve (0 to 1) a motion is when `x` (0 to 1) of its
/// time has gone by.
fn eased(x: f64) -> f64 {
    let (x1, y1, x2, y2) = CURVE;
    let bezier = |s: f64, a: f64, b: f64| {
        let r = 1.0 - s;
        3.0 * r * r * s * a + 3.0 * r * s * s * b + s * s * s
    };
    let x = x.clamp(0.0, 1.0);
    // The curve's x grows with s: halve the interval down to the s for x.
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if bezier(mid, x1, x2) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bezier((lo + hi) / 2.0, y1, y2)
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    from: f64,
    pos: f64,
    goal: f64,
}

impl Edge {
    fn at(pos: i32, goal: i32) -> Self {
        let pos = f64::from(pos);
        Self {
            from: pos,
            pos,
            goal: f64::from(goal),
        }
    }

    /// Exactly where it is going, and was all along.
    fn still(&self) -> bool {
        self.from == self.goal && self.pos == self.goal
    }

    /// A new goal, set out for from where it is.
    fn set_goal(&mut self, goal: i32) {
        self.from = self.pos;
        self.goal = f64::from(goal);
    }

    fn put(&mut self, along: f64) {
        self.pos = self.from + (self.goal - self.from) * along;
    }
}

/// Start and length of the span between two edges, in whole points. Length
/// is rounded on its own, so a span that only moves keeps it exactly; then
/// the edge standing still decides where the span goes.
fn span(low: &Edge, high: &Edge) -> (i32, i32) {
    let len = (high.pos - low.pos).round();
    let start = if high.still() && !low.still() {
        high.pos - len
    } else {
        low.pos.round()
    };
    (start as i32, len as i32)
}

#[derive(Clone, Copy, Debug)]
struct Motion {
    left: Edge,
    top: Edge,
    right: Edge,
    bottom: Edge,
    /// Seconds since it set out for `target`.
    elapsed: f64,
    target: Rect,
    shown: Rect,
}

impl Motion {
    fn new(from: Rect, to: Rect) -> Self {
        Self {
            left: Edge::at(from.x, to.x),
            top: Edge::at(from.y, to.y),
            right: Edge::at(from.max_x(), to.max_x()),
            bottom: Edge::at(from.max_y(), to.max_y()),
            elapsed: 0.0,
            target: to,
            shown: from,
        }
    }

    fn edges(&mut self) -> [&mut Edge; 4] {
        [
            &mut self.left,
            &mut self.top,
            &mut self.right,
            &mut self.bottom,
        ]
    }

    fn aim(&mut self, to: Rect) {
        if to == self.target {
            return;
        }
        self.target = to;
        self.elapsed = 0.0;
        self.left.set_goal(to.x);
        self.top.set_goal(to.y);
        self.right.set_goal(to.max_x());
        self.bottom.set_goal(to.max_y());
    }

    /// Advances `t` seconds of a motion `duration` long. Returns whether it
    /// is done.
    fn advance(&mut self, t: f64, duration: f64) -> bool {
        self.elapsed += t;
        let along = eased(self.elapsed / duration);
        for edge in self.edges() {
            edge.put(along);
        }
        self.elapsed >= duration
    }

    fn frame(&self) -> Rect {
        let (x, w) = span(&self.left, &self.right);
        let (y, h) = span(&self.top, &self.bottom);
        Rect::new(x, y, w, h)
    }
}

/// Drives every window toward where it was sent.
#[derive(Debug)]
pub struct Mover {
    /// Seconds a motion takes; `0` makes moves instant.
    duration: f64,
    motions: HashMap<WinId, Motion>,
}

impl Mover {
    pub fn new(duration: f64) -> Self {
        Self {
            duration,
            motions: HashMap::new(),
        }
    }

    pub fn set_duration(&mut self, duration: f64) {
        self.duration = duration;
    }

    /// Sends window `id` to `target`. `current` is where it is now, unless
    /// it is in motion: then it carries on from where the motion has it.
    ///
    /// Returns the frame to apply at once when motion is off.
    pub fn send(&mut self, id: WinId, current: Rect, target: Rect) -> Option<Rect> {
        if self.duration <= 0.0 {
            self.motions.remove(&id);
            return (current != target).then_some(target);
        }
        if let Some(motion) = self.motions.get_mut(&id) {
            motion.aim(target);
        } else if current != target {
            self.motions.insert(id, Motion::new(current, target));
        }
        None
    }

    /// Stops window `id` where it is.
    pub fn halt(&mut self, id: WinId) {
        self.motions.remove(&id);
    }

    pub fn at_rest(&self) -> bool {
        self.motions.is_empty()
    }

    pub fn in_motion(&self, id: WinId) -> bool {
        self.motions.contains_key(&id)
    }

    /// Where `id` is going, if it is in motion.
    pub fn heading(&self, id: WinId) -> Option<Rect> {
        self.motions.get(&id).map(|m| m.target)
    }

    /// Advances everything by `dt` seconds. Returns the frames that changed.
    pub fn advance(&mut self, dt: f64) -> Vec<(WinId, Rect)> {
        let t = dt.clamp(0.0, MAX_STEP);
        let duration = self.duration.max(f64::EPSILON);
        let mut changed = Vec::with_capacity(self.motions.len());
        self.motions.retain(|&id, motion| {
            let done = motion.advance(t, duration);
            let frame = if done { motion.target } else { motion.frame() };
            if frame != motion.shown {
                motion.shown = frame;
                changed.push((id, frame));
            }
            !done
        });
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f64 = 1.0 / 60.0;
    const FROM: Rect = Rect::new(0, 0, 100, 100);
    const FAR: Rect = Rect::new(1000, 0, 100, 100);

    fn run(a: &mut Mover, dt: f64) -> Vec<Rect> {
        let mut frames = Vec::new();
        for _ in 0..100_000 {
            frames.extend(a.advance(dt).into_iter().map(|(_, f)| f));
            if a.at_rest() {
                return frames;
            }
        }
        panic!("it never arrived");
    }

    fn sent(from: Rect, to: Rect) -> Vec<Rect> {
        let mut a = Mover::new(0.3);
        a.send(1, from, to);
        run(&mut a, FRAME)
    }

    #[test]
    fn it_lands_exactly() {
        let to = Rect::new(500, 40, 800, 600);
        assert_eq!(*sent(FROM, to).last().unwrap(), to);
    }

    #[test]
    fn it_takes_about_its_duration() {
        let n = sent(FROM, FAR).len();
        assert!((17..=19).contains(&n), "{n} frames for 18 at 60 fps");
    }

    #[test]
    fn the_curve_is_core_animations_default() {
        assert!(eased(0.0).abs() < 1e-9);
        assert!((eased(1.0) - 1.0).abs() < 1e-9);
        // Known points of `0.25, 0.1, 0.25, 1`.
        assert!((eased(0.5) - 0.8024).abs() < 0.001, "{}", eased(0.5));
        assert!((eased(0.25) - 0.4094).abs() < 0.001, "{}", eased(0.25));
    }

    #[test]
    fn from_rest_it_never_overshoots_or_turns_back() {
        let xs: Vec<i32> = sent(FROM, FAR).iter().map(|f| f.x).collect();
        assert!(xs.windows(2).all(|p| p[1] >= p[0]), "{xs:?}");
        assert!(xs.iter().all(|x| *x <= 1000), "{xs:?}");
    }

    #[test]
    fn it_starts_softly_goes_most_of_the_way_early_and_lands_gently() {
        let xs: Vec<i32> = sent(FROM, FAR).iter().map(|f| f.x).collect();
        let steps: Vec<i32> = std::iter::once(xs[0])
            .chain(xs.windows(2).map(|p| p[1] - p[0]))
            .collect();
        let peak = *steps.iter().max().unwrap();
        assert!(steps[0] < peak, "{steps:?}");
        assert!(xs[xs.len() / 2] > 750, "{xs:?}");
        assert!(*steps.last().unwrap() <= 3, "{steps:?}");
    }

    #[test]
    fn a_new_goal_in_flight_is_headed_for_from_where_it_is() {
        let mut a = Mover::new(0.3);
        a.send(1, FROM, FAR);
        let mut last = 0;
        for _ in 0..5 {
            last = a.advance(FRAME)[0].1.x;
        }
        a.send(1, FROM, Rect::new(2000, 0, 100, 100));
        assert!(a.advance(FRAME)[0].1.x > last);
        a.send(1, FROM, FROM);
        assert_eq!(*run(&mut a, FRAME).last().unwrap(), FROM);
    }

    #[test]
    fn the_path_does_not_depend_on_the_frame_rate() {
        let at = |fps: f64| {
            let mut a = Mover::new(0.3);
            a.send(1, FROM, FAR);
            let mut x = 0;
            for _ in 0..(0.15 * fps).round() as usize {
                if let Some(f) = a.advance(1.0 / fps).last() {
                    x = f.1.x;
                }
            }
            x
        };
        assert!((at(60.0) - at(120.0)).abs() <= 1);
        assert!((at(60.0) - at(240.0)).abs() <= 1);
    }

    #[test]
    fn a_still_edge_never_moves_a_single_point() {
        // Shrinking from the left, growing from the top: right and bottom.
        let from = Rect::new(600, 200, 401, 301);
        let to = Rect::new(733, 77, 268, 424);
        for frame in sent(from, to) {
            assert_eq!(frame.max_x(), from.max_x(), "{frame:?}");
            assert_eq!(frame.max_y(), from.max_y(), "{frame:?}");
        }
        // Growing to the right: the left edge.
        let to = Rect::new(600, 200, 777, 301);
        for frame in sent(from, to) {
            assert_eq!(frame.x, from.x, "{frame:?}");
            assert_eq!(frame.y, from.y, "{frame:?}");
            assert_eq!(frame.h, from.h, "{frame:?}");
        }
    }

    #[test]
    fn a_window_only_moving_never_changes_size() {
        let from = Rect::new(13, 57, 411, 333);
        let to = Rect::new(977, 402, 411, 333);
        for fps in [59.0, 60.0, 120.0, 144.0] {
            let mut a = Mover::new(0.37);
            a.send(1, from, to);
            for frame in run(&mut a, 1.0 / fps) {
                assert_eq!((frame.w, frame.h), (411, 333), "{frame:?}");
            }
        }
    }

    #[test]
    fn a_stall_does_not_make_it_jump() {
        let mut a = Mover::new(0.3);
        a.send(1, FROM, FAR);
        let late = a.advance(5.0)[0].1.x;
        assert!(late < 900, "{late}");
        assert!(!a.at_rest());
    }

    #[test]
    fn nothing_is_sent_twice() {
        let frames = sent(FROM, Rect::new(3, 0, 100, 100));
        assert!(frames.windows(2).all(|p| p[0] != p[1]), "{frames:?}");
    }

    #[test]
    fn already_there_is_no_motion() {
        let mut a = Mover::new(0.3);
        assert_eq!(a.send(1, FROM, FROM), None);
        assert!(a.at_rest());
    }

    #[test]
    fn duration_zero_goes_at_once() {
        let mut a = Mover::new(0.0);
        assert_eq!(a.send(1, FROM, FAR), Some(FAR));
        assert!(a.at_rest());
        assert_eq!(a.send(1, FAR, FAR), None);
    }

    #[test]
    fn a_shorter_duration_finishes_sooner() {
        let count = |d: f64| {
            let mut a = Mover::new(d);
            a.send(1, FROM, FAR);
            run(&mut a, FRAME).len()
        };
        assert!(count(0.2) < count(0.6));
    }

    #[test]
    fn cancel_leaves_it_where_it_is() {
        let mut a = Mover::new(0.3);
        a.send(1, FROM, FAR);
        a.advance(FRAME);
        a.halt(1);
        assert!(!a.in_motion(1));
        assert_eq!(a.heading(1), None);
        assert!(a.advance(FRAME).is_empty());
    }

    #[test]
    fn windows_move_independently() {
        let mut a = Mover::new(0.3);
        a.send(1, FROM, FAR);
        a.send(2, FAR, FROM);
        assert_eq!(a.advance(FRAME).len(), 2);
        a.halt(1);
        assert!(a.in_motion(2));
    }

    #[test]
    fn frames_snap_to_whole_refreshes() {
        let refresh = 1.0 / 120.0;
        assert_eq!(paced(refresh * 1.1, refresh), refresh);
        assert_eq!(paced(refresh * 0.9, refresh), refresh);
        assert_eq!(paced(refresh * 2.05, refresh), refresh * 2.0);
        // Halfway between two refreshes: the clock knows best.
        assert_eq!(paced(refresh * 2.5, refresh), refresh * 2.5);
        assert_eq!(paced(0.0, refresh), 0.0);
        assert_eq!(paced(0.01, 0.0), 0.01);
    }
}
