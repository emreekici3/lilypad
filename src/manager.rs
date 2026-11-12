//! Keeps track of the windows and carries out what the user asks for.
//!
//! One rule above all: a window the user did not touch does not move. The
//! only exception is a new window with a rule for its app.

use std::collections::{HashMap, HashSet};

use log::{debug, info, warn};

use crate::command::{Command, SpaceTarget};
use crate::config::{Config, MouseAction, app_matches};
use crate::geometry::{Direction, Rect, WinId};
use crate::motion::Mover;
use crate::place::{self, MIN_SIZE, Position};
use crate::platform::{Backend, Border, Display, Modifier, MouseButton, WindowEvent};

/// Pointer movement below this many points is a click, not a drag.
const DRAG_THRESHOLD: i32 = 4;

/// After going to a Space with a window sent there, how long it is kept in
/// focus (macOS may hand the focus to another window of that Space), and
/// how often that is looked at.
const HOLD_FOCUS: std::time::Duration = std::time::Duration::from_millis(500);
const HOLD_FOCUS_RETRY: std::time::Duration = std::time::Duration::from_millis(30);

/// How long the pointer rests on a window before focus follows it, seconds.
const FOCUS_FOLLOWS_MOUSE_DELAY: f64 = 0.1;

pub struct Manager<B: Backend> {
    backend: B,
    config: Config,
    mover: Mover,
    /// Last frame each window was seen at or sent to.
    frames: HashMap<WinId, Rect>,
    /// Windows on screen, front to back, that lily can move, from the last
    /// look.
    stack: Vec<WinId>,
    apps: HashMap<WinId, String>,
    /// Those of them that can also be resized.
    resizable: HashSet<WinId>,
    /// Windows seen before, so a rule only applies to new ones.
    seen: HashSet<WinId>,
    /// Where each maximised window was.
    before_max: HashMap<WinId, Rect>,
    /// Windows sent a new size since the last look: the window list can lag
    /// behind them, so they are asked where they are.
    resized: HashSet<WinId>,
    /// Seconds since start, advanced by `tick`.
    clock: f64,
    /// The displays, as of when the window in hand was taken.
    displays: Vec<Display>,
    synced: bool,
    grab: Option<Grab>,
    /// The modifier last given to the backend.
    mouse_modifier: Option<Modifier>,
    /// The gesture button at the previous frame.
    gesture_was: Option<MouseButton>,
    /// The focused window, as of the last look or focus command.
    focus: Option<WinId>,
    /// The ring last handed to the backend.
    shown_border: Option<Border>,
    /// The Space on screen as of the last look, and the one before it.
    space: u64,
    previous_space: Option<u64>,
    /// A window to keep in focus until the first instant, looked at again
    /// at the second.
    holding_focus: Option<(WinId, std::time::Instant, std::time::Instant)>,
    /// Focus follows mouse: where the pointer was, the window under it, and
    /// the window it came to rest on, since when (`clock`).
    last_cursor: Option<(f32, f32)>,
    under: Option<WinId>,
    hover: Option<(WinId, f64)>,
    /// Scratchpads: the window of each, the one being started, and the
    /// window in focus before one was brought out.
    scratch_windows: HashMap<String, WinId>,
    scratch_starting: Option<String>,
    scratch_before: Option<WinId>,
    /// Scratchpad windows brought out and not put away since.
    scratch_out: HashSet<WinId>,
    /// The Space and focus SketchyBar was last told about.
    told_sketchybar: Option<(u64, Option<WinId>)>,
    /// Every window that exists, on any Space, as of the last look, and
    /// the app of each when the backend can tell.
    alive: HashSet<WinId>,
    alive_apps: HashMap<WinId, String>,
    /// A scratchpad brought from another Space, to put at its cell once it
    /// is seen here.
    scratch_placing: Option<(WinId, [u32; 6])>,
}

/// A window held with the modifier and a mouse button.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Grab {
    win: WinId,
    button: MouseButton,
    action: MouseAction,
    /// Pointer and window frame when the button went down.
    start: (f32, f32),
    frame: Rect,
    /// For a resize, the edges that follow the pointer.
    horizontal: Direction,
    vertical: Direction,
    /// The pointer has gone further than a click's wobble.
    moved: bool,
}

impl<B: Backend> Manager<B> {
    pub fn new(backend: B, config: Config) -> Self {
        Self {
            mover: Mover::new(config.animation_duration),
            backend,
            config,
            frames: HashMap::new(),
            stack: Vec::new(),
            apps: HashMap::new(),
            resizable: HashSet::new(),
            seen: HashSet::new(),
            before_max: HashMap::new(),
            resized: HashSet::new(),
            clock: 0.0,
            displays: Vec::new(),
            synced: false,
            grab: None,
            mouse_modifier: None,
            gesture_was: None,
            focus: None,
            shown_border: None,
            space: 0,
            previous_space: None,
            holding_focus: None,
            last_cursor: None,
            under: None,
            hover: None,
            scratch_windows: HashMap::new(),
            scratch_starting: None,
            scratch_before: None,
            scratch_out: HashSet::new(),
            told_sketchybar: None,
            alive: HashSet::new(),
            alive_apps: HashMap::new(),
            scratch_placing: None,
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// `lily ...` command lines from keyboard shortcuts.
    pub fn hotkey_commands(&mut self) -> Vec<String> {
        self.backend.hotkey_commands()
    }

    /// Whether the backend wants another look soon without an event: see
    /// [`Backend::wants_another_look`].
    pub fn wants_another_look(&mut self) -> bool {
        self.backend.wants_another_look()
    }

    /// How long an animation frame lasts: one refresh of the display.
    pub fn frame_interval(&mut self) -> std::time::Duration {
        let rate = self
            .backend
            .frame_rate()
            .unwrap_or(120.0)
            .clamp(30.0, 240.0);
        std::time::Duration::from_secs_f64(1.0 / rate)
    }

    /// Waits for the next frame of a motion. `remaining` is what is left of
    /// this one by the clock.
    pub fn wait_frame(&mut self, remaining: std::time::Duration) {
        let refresh = self.frame_interval();
        self.backend.wait_frame(remaining, refresh);
    }

    /// Nothing is moving any more.
    pub fn stop_frames(&mut self) {
        self.backend.stop_frames();
    }

    /// Whether a window is in hand, or on the fingers: frames must not wait.
    pub fn is_held(&self) -> bool {
        self.grab.is_some()
    }

    pub fn is_animating(&self) -> bool {
        !self.mover.at_rest()
    }

    /// Lets the platform UI process its events; call once per frame.
    pub fn pump(&mut self) {
        self.hold_focus();
        self.update_border();
        self.tell_sketchybar();
        self.backend.pump();
    }

    /// Where the ring goes this frame: around the focused window, where the
    /// window really is, moving with it.
    fn wanted_border(&mut self) -> Option<Border> {
        // Its window slides away with the Space; the ring would not.
        if !self.config.border || self.backend.space_swiping() {
            return None;
        }
        let id = self.focus.filter(|id| self.stack.contains(id))?;
        let known = self.frames.get(&id).copied()?;
        // `frames` has where lily sent the window, which is not where it
        // is when its app refuses a size, or while the user drags it by its
        // title bar. The window server knows.
        let rect = self.backend.server_frame(id).unwrap_or(known);
        Some(Border {
            rect,
            window: id,
            color: self.config.border_color,
            width: self.config.border_width,
            radius: self.radius_of(id),
        })
    }

    fn radius_of(&self, id: WinId) -> i32 {
        self.config
            .border_radius_for(self.apps.get(&id).map_or("", String::as_str))
    }

    /// Tells SketchyBar the Space or the focus changed:
    /// `lily_space_change` with `LILY_SPACE` (from 1) and
    /// `LILY_SPACE_LABEL`, `lily_focus_change` with `LILY_APP` and
    /// `LILY_WINDOW`.
    fn tell_sketchybar(&mut self) {
        if !self.config.sketchybar || self.space == 0 {
            return;
        }
        let now = (self.space, self.focus);
        let before = self.told_sketchybar.replace(now);
        if before.is_some_and(|(space, _)| space == now.0) {
            if before.is_some_and(|(_, focus)| focus == now.1) {
                return;
            }
        } else {
            let n = self
                .backend
                .display_spaces(self.space)
                .iter()
                .position(|s| *s == self.space)
                .map_or(0, |i| i + 1);
            let label = self.config.labels.get(&n).cloned().unwrap_or_default();
            self.backend.trigger_sketchybar(
                "lily_space_change",
                &[("LILY_SPACE", n.to_string()), ("LILY_SPACE_LABEL", label)],
            );
        }
        let app = self
            .focus
            .and_then(|id| self.apps.get(&id).cloned())
            .unwrap_or_default();
        let window = self.focus.map(|id| id.to_string()).unwrap_or_default();
        self.backend.trigger_sketchybar(
            "lily_focus_change",
            &[("LILY_APP", app), ("LILY_WINDOW", window)],
        );
    }

    /// The Space on screen is `now`: the one before it becomes `recent`.
    fn note_space(&mut self, now: u64) {
        if now != 0 && now != self.space {
            if self.space != 0 {
                self.previous_space = Some(self.space);
                self.put_away_scratchpads_left_behind();
            }
            self.space = now;
        }
    }

    /// A scratchpad is for the Space it was brought to: one left behind
    /// goes out of sight. Call with `stack` up to date.
    fn put_away_scratchpads_left_behind(&mut self) {
        let left: Vec<WinId> = self
            .scratch_out
            .iter()
            .copied()
            .filter(|w| !self.stack.contains(w))
            .collect();
        for win in left {
            self.scratch_out.remove(&win);
            self.holding_focus = self.holding_focus.filter(|(w, _, _)| *w != win);
            self.scratch_before = None;
            self.backend.set_app_hidden(win, true);
        }
    }

    /// The window of a scratchpad, if its app has one.
    fn scratchpad_window(&self, name: &str, app: &str) -> Option<WinId> {
        self.scratch_windows
            .get(name)
            .copied()
            .filter(|w| self.alive.contains(w) || self.apps.contains_key(w))
            .or_else(|| {
                // Its window may be on another Space, or its app hidden.
                let mut windows: Vec<WinId> = self
                    .apps
                    .iter()
                    .chain(self.alive_apps.iter())
                    .filter(|(_, a)| app_matches(app, a))
                    .map(|(w, _)| *w)
                    .collect();
                windows.sort_unstable();
                windows.first().copied()
            })
    }

    /// Brings a scratchpad's window to the Space on screen, floating at its
    /// grid cell, in focus.
    fn show_scratchpad(&mut self, win: WinId, grid: [u32; 6]) {
        let before = self.focus.filter(|f| *f != win);
        if before.is_some() {
            self.scratch_before = before;
        }
        self.backend.set_app_hidden(win, false);
        let here = self.current_space();
        if !self.stack.contains(&win) && here != 0 && !self.backend.place_on_space(win, here) {
            warn!("could not bring window {win} to this Space");
        }
        let display = self.focus_display_area();
        let cell = self.grid_frame(display, grid);
        let was = self.frames.get(&win).copied().unwrap_or(cell);
        self.mover.halt(win);
        self.backend.update_frame(win, cell, was);
        self.frames.insert(win, cell);
        if !self.stack.contains(&win) {
            // From another Space, the frame may not take until it is here.
            self.scratch_placing = Some((win, grid));
            self.stack.insert(0, win);
        }
        self.scratch_out.insert(win);
        self.keep_focus(win);
    }

    /// Puts a scratchpad's window out of sight, the focus back where it was.
    fn hide_scratchpad(&mut self, win: WinId) {
        self.holding_focus = None;
        self.scratch_out.remove(&win);
        if let Some(before) = self
            .scratch_before
            .take()
            .filter(|b| self.stack.contains(b))
        {
            self.backend.focus(before);
            self.focus = Some(before);
        }
        self.backend.set_app_hidden(win, true);
    }

    /// The usable area of the display with the focused window (or the main
    /// one), inside the gap.
    fn focus_display_area(&mut self) -> Rect {
        let frame = self.focus.and_then(|id| self.frames.get(&id).copied());
        match frame {
            Some(f) => self.area_of(f),
            None => {
                let displays = self.backend.displays();
                displays
                    .first()
                    .map_or_else(|| self.backend.tiling_area(), |d| d.area)
                    .inset(self.config.gap)
            }
        }
    }

    /// A grid cell of `area`, with the gap between it and its neighbours.
    fn grid_frame(&self, area: Rect, grid: [u32; 6]) -> Rect {
        let cell = area.grid_cell(grid);
        let half = self.config.gap / 2;
        let left = if cell.x > area.x { half } else { 0 };
        let top = if cell.y > area.y { half } else { 0 };
        let right = if cell.max_x() < area.max_x() { half } else { 0 };
        let bottom = if cell.max_y() < area.max_y() { half } else { 0 };
        Rect::new(
            cell.x + left,
            cell.y + top,
            cell.w - left - right,
            cell.h - top - bottom,
        )
    }

    /// Mouse follows focus: puts the pointer in the middle of `win`, unless
    /// it is on it already.
    fn warp_to(&mut self, win: WinId) {
        if !self.config.mouse_follows_focus {
            return;
        }
        let Some(rect) = self.destination(win) else {
            return;
        };
        if self
            .backend
            .cursor()
            .is_some_and(|(x, y)| rect.contains_point(x, y))
        {
            return;
        }
        let (x, y) = rect.center();
        self.backend.warp_cursor(x, y);
        // The pointer did not travel there: nothing for focus follows mouse.
        self.last_cursor = Some((x, y));
        self.under = Some(win);
        self.hover = None;
    }

    /// Focus follows mouse: focuses the window the pointer came to rest on.
    /// Only a moving pointer counts, so a window sliding under a still one
    /// does not take the focus.
    fn follow_mouse(&mut self) {
        let Some((x, y)) = self.backend.cursor() else {
            return;
        };
        let moved = self.last_cursor.is_some_and(|last| last != (x, y));
        self.last_cursor = Some((x, y));
        let under = self
            .stack
            .iter()
            .copied()
            .find(|id| self.frames.get(id).is_some_and(|f| f.contains_point(x, y)));
        if under != self.under {
            self.under = under;
            self.hover = under.filter(|_| moved).map(|w| (w, self.clock));
        }
        let Some((win, since)) = self.hover else {
            return;
        };
        if self.clock - since < FOCUS_FOLLOWS_MOUSE_DELAY {
            return;
        }
        self.hover = None;
        if self.focus == Some(win) {
            return;
        }
        // Focusing raises, and Mission Control would show the new order.
        if self.backend.overview_open() {
            return;
        }
        // Spotlight and the like close when they lose the keyboard.
        if self.backend.panel_has_focus() {
            return;
        }
        // Menus and popovers are not in the window list; focus would close them.
        if self.backend.covered_at(x, y) {
            return;
        }
        debug!("focus follows mouse: window {win}");
        self.backend.focus(win);
        self.focus = Some(win);
    }

    /// The Space of the display commands act on: the focused window's, or
    /// the main display's.
    fn current_space(&mut self) -> u64 {
        let displays = self.backend.displays();
        let focused = self.focus.and_then(|id| self.frames.get(&id).copied());
        focused
            .and_then(|f| {
                let (x, y) = f.center();
                displays.iter().find(|d| d.frame.contains_point(x, y))
            })
            .or_else(|| displays.first())
            .map_or(self.space, |d| d.space)
    }

    /// The number (from 1) and id of the Space `target` names, on the
    /// display commands act on, and the Space that display shows now.
    fn resolve_space(&mut self, target: &SpaceTarget) -> Result<(usize, u64, u64), String> {
        let current = self.current_space();
        let spaces = self.backend.display_spaces(current);
        let here = spaces.iter().position(|s| *s == current);
        let count = spaces.len();
        let at = match (target, here) {
            (SpaceTarget::Number(n), _) => n - 1,
            (SpaceTarget::Label(name), _) => {
                self.config
                    .space_number(target)
                    .ok_or_else(|| format!("no Space is labelled '{name}'"))?
                    - 1
            }
            (SpaceTarget::Next, Some(i)) => (i + 1) % count,
            (SpaceTarget::Prev, Some(i)) => (i + count - 1) % count,
            (SpaceTarget::Next | SpaceTarget::Prev, None) => {
                return Err("the Space on screen is not a desktop".into());
            }
            (SpaceTarget::Recent, _) => {
                let last = self
                    .previous_space
                    .ok_or("no Space was shown before this one")?;
                spaces
                    .iter()
                    .position(|s| *s == last)
                    .ok_or("the last Space is not on this display")?
            }
        };
        let id = *spaces
            .get(at)
            .ok_or_else(|| format!("there is no Space {} on this display", at + 1))?;
        Ok((at + 1, id, current))
    }

    /// Shows Space `to` instead of `from`, taking window `along` there (if
    /// any). Scratchpads out on `from` are put away.
    fn go_to_space(&mut self, from: u64, to: u64, along: Option<WinId>) -> Result<(), String> {
        self.backend.show_space(to)?;
        let left: Vec<WinId> = self
            .scratch_out
            .iter()
            .copied()
            .filter(|w| Some(*w) != along)
            .collect();
        for win in left {
            self.scratch_out.remove(&win);
            self.holding_focus = self.holding_focus.filter(|(w, _, _)| *w != win);
            self.scratch_before = None;
            self.backend.set_app_hidden(win, true);
        }
        self.previous_space = Some(from);
        self.space = to;
        Ok(())
    }

    /// Focuses `win` and keeps it focused for a moment.
    fn keep_focus(&mut self, win: WinId) {
        self.backend.focus(win);
        self.focus = Some(win);
        let now = std::time::Instant::now();
        self.holding_focus = Some((win, now + HOLD_FOCUS, now + HOLD_FOCUS_RETRY));
    }

    /// Takes the focus back for the window it belongs to, for as long as
    /// macOS may still hand it elsewhere.
    fn hold_focus(&mut self) {
        let Some((win, until, next)) = self.holding_focus else {
            return;
        };
        let now = std::time::Instant::now();
        if now >= until {
            self.holding_focus = None;
        } else if now >= next {
            self.holding_focus = Some((win, until, now + HOLD_FOCUS_RETRY));
            if self.backend.focused_window() != Some(win) {
                debug!("keeping window {win} in focus");
                self.backend.focus(win);
                self.focus = Some(win);
            }
        }
    }

    fn update_border(&mut self) {
        let wanted = self.wanted_border();
        if wanted != self.shown_border {
            self.shown_border = wanted;
            self.backend.show_border(wanted);
        }
    }

    pub fn set_config(&mut self, config: Config) {
        if let Some(level) = config.log_level {
            log::set_max_level(level);
        }
        self.mover.set_duration(config.animation_duration);
        if config.hotkeys != self.config.hotkeys {
            self.backend.set_hotkeys(&config.hotkeys.join("\n"));
        }
        self.config = config;
    }

    /// Looks at the windows: picks up new and closed ones, and where the
    /// ones not in motion really are.
    pub fn refresh(&mut self) {
        let windows = self.backend.windows();
        let movable: Vec<_> = windows.into_iter().filter(|w| w.movable).collect();
        self.stack = movable.iter().map(|w| w.id).collect();
        self.resizable = movable
            .iter()
            .filter(|w| w.resizable)
            .map(|w| w.id)
            .collect();
        let on_screen: HashSet<WinId> = self.stack.iter().copied().collect();
        self.frames.retain(|id, _| on_screen.contains(id));
        self.apps.retain(|id, _| on_screen.contains(id));

        let alive = self
            .backend
            .alive_windows()
            .unwrap_or_else(|| on_screen.clone());
        self.seen
            .retain(|id| alive.contains(id) || on_screen.contains(id));
        self.before_max.retain(|id, _| alive.contains(id));
        self.resized.retain(|id| on_screen.contains(id));
        self.scratch_windows.retain(|_, w| alive.contains(w));
        self.scratch_out.retain(|w| alive.contains(w));
        self.alive = alive;
        self.alive_apps = self.backend.window_apps();
        self.focus = self.backend.focused_window();
        let shown = self.backend.shown_space();
        self.note_space(shown);

        for info in movable {
            if !self.mover.in_motion(info.id) {
                // After a resize the window list can lag behind: what the
                // app has is what counts.
                let frame = if self.resized.remove(&info.id) {
                    self.backend.window_frame(info.id).unwrap_or(info.frame)
                } else {
                    info.frame
                };
                self.frames.insert(info.id, frame);
            }
            self.apps.insert(info.id, info.app.clone());
            if self.seen.insert(info.id) && self.synced {
                self.new_window(info.id, &info.app);
            }
        }
        self.synced = true;
        if let Some((win, grid)) = self.scratch_placing
            && self.alive.contains(&win)
            && self.stack.contains(&win)
        {
            self.scratch_placing = None;
            let area = self.focus_display_area();
            let cell = self.grid_frame(area, grid);
            if self.frames.get(&win) != Some(&cell) {
                self.send(win, cell);
            }
            // It could not take the focus before lily knew it.
            if self.focus != Some(win) {
                self.keep_focus(win);
            }
        }
    }

    /// A window just opened: a scratchpad being started, or one an app's
    /// rules put somewhere.
    fn new_window(&mut self, id: WinId, app: &str) {
        if let Some(name) = self.scratch_starting.clone() {
            let pad = self
                .config
                .scratchpads
                .iter()
                .find(|(n, a, _)| *n == name && app_matches(a, app))
                .map(|(_, _, g)| *g);
            if let Some(grid) = pad {
                self.scratch_starting = None;
                self.scratch_windows.insert(name, id);
                self.show_scratchpad(id, grid);
                return;
            }
        }
        if self
            .config
            .scratchpads
            .iter()
            .any(|(_, a, _)| app_matches(a, app))
        {
            return; // a scratchpad's app is never put anywhere else
        }
        if let Some(grid) = self.config.grid_rule_for(app) {
            debug!("window {id} of {app} goes to grid {grid:?}");
            let Some(now) = self.destination(id) else {
                return;
            };
            let area = self.area_of(now);
            let cell = self.grid_frame(area, grid);
            self.send(id, cell);
        }
        let Some((target, follow)) = self.config.space_rule_for(app).map(|(t, f)| (t.clone(), f))
        else {
            return;
        };
        let Ok((n, to, from)) = self.resolve_space(&target) else {
            warn!("rule space for {app}: no such Space");
            return;
        };
        if to == from {
            return;
        }
        if !self.backend.place_on_space(id, to) {
            warn!("could not send window {id} of {app} to Space {n}");
            return;
        }
        info!("window {id} of {app} sent to Space {n}");
        if follow {
            match self.go_to_space(from, to, Some(id)) {
                Ok(()) => self.keep_focus(id),
                Err(e) => warn!("could not go to Space {n}: {e}"),
            }
        }
    }

    /// Reacts to window events. Returns whether to look at the window list
    /// again now.
    pub fn handle_events(&mut self) -> bool {
        let mut refresh = false;
        for event in self.backend.take_events() {
            match event {
                WindowEvent::Destroyed(id) => {
                    self.mover.halt(id);
                    self.frames.remove(&id);
                    self.before_max.remove(&id);
                    self.resized.remove(&id);
                    self.stack.retain(|w| *w != id);
                    if self.focus == Some(id) {
                        self.focus = None;
                    }
                    if self.grab.is_some_and(|g| g.win == id) {
                        self.grab = None;
                    }
                    refresh = true;
                }
                WindowEvent::Changed => refresh = true,
                // While lily animates, the moves are mostly its own: one
                // look once the animation ends covers them. While the mouse
                // is down, the user is dragging: the release takes a look.
                WindowEvent::Moved(_) => {
                    refresh |=
                        self.mover.at_rest() && !self.backend.mouse_down() && !self.is_held();
                }
            }
        }
        refresh
    }

    /// Advances animations by `dt` seconds and pushes the new frames.
    pub fn tick(&mut self, dt: f64) {
        self.clock += dt;
        for (id, frame) in self.mover.advance(dt) {
            let more = self.mover.in_motion(id);
            match self.frames.insert(id, frame) {
                Some(prev) if more => self.backend.animate_frame(id, frame, prev),
                Some(prev) => self.backend.update_frame(id, frame, prev),
                None => self.backend.set_frame(id, frame),
            }
        }
    }

    /// Follows modifier drags. Call this every frame.
    pub fn track_pointer(&mut self) {
        if self.mouse_modifier != self.config.mouse_modifier {
            self.mouse_modifier = self.config.mouse_modifier;
            self.backend.set_mouse_modifier(self.mouse_modifier);
        }
        let gesture = self.backend.mouse_gesture();
        let pressed = gesture.filter(|_| self.gesture_was.is_none());
        self.gesture_was = gesture;
        match self.grab {
            Some(grab) if gesture == Some(grab.button) => self.drag(grab),
            Some(_) => self.grab = None,
            None => {
                if let Some(button) = pressed {
                    self.take_hold(button);
                } else if self.config.focus_follows_mouse
                    && gesture.is_none()
                    && !self.backend.mouse_down()
                {
                    self.follow_mouse();
                }
            }
        }
    }

    fn take_hold(&mut self, button: MouseButton) {
        let Some((x, y)) = self.backend.cursor() else {
            return;
        };
        let under = self.stack.iter().find_map(|id| {
            let f = *self.frames.get(id)?;
            f.contains_point(x, y).then_some((*id, f))
        });
        let Some((win, frame)) = under else {
            debug!("mouse gesture at {x:.0},{y:.0}: no window there");
            return;
        };
        self.mover.halt(win);
        self.backend.focus(win);
        self.displays = self.backend.displays();
        let (cx, cy) = frame.center();
        self.grab = Some(Grab {
            win,
            button,
            action: self.config.mouse_action(button),
            start: (x, y),
            frame,
            horizontal: if x < cx {
                Direction::Left
            } else {
                Direction::Right
            },
            vertical: if y < cy {
                Direction::Up
            } else {
                Direction::Down
            },
            moved: false,
        });
    }

    fn drag(&mut self, mut grab: Grab) {
        let Some((x, y)) = self.backend.cursor() else {
            return;
        };
        let dx = (x - grab.start.0).round() as i32;
        let dy = (y - grab.start.1).round() as i32;
        if !grab.moved && dx.abs() <= DRAG_THRESHOLD && dy.abs() <= DRAG_THRESHOLD {
            return; // a click
        }
        grab.moved = true;
        self.grab = Some(grab);
        let f = grab.frame;
        let frame = match grab.action {
            // The mouse goes wherever the user takes it, past the edges too.
            MouseAction::Move => Rect::new(f.x + dx, f.y + dy, f.w, f.h),
            MouseAction::Resize => {
                let (min_w, min_h) = (MIN_SIZE, MIN_SIZE);
                let area = self.drag_area(f);
                let (mut left, mut top, mut right, mut bottom) = (f.x, f.y, f.max_x(), f.max_y());
                match grab.horizontal {
                    Direction::Left => left = (left + dx).max(area.x.min(left)).min(right - min_w),
                    _ => {
                        right = (right + dx).min(area.max_x().max(right)).max(left + min_w);
                    }
                }
                match grab.vertical {
                    Direction::Up => top = (top + dy).max(area.y.min(top)).min(bottom - min_h),
                    _ => {
                        bottom = (bottom + dy).min(area.max_y().max(bottom)).max(top + min_h);
                    }
                }
                Rect::new(left, top, right - left, bottom - top)
            }
        };
        let prev = self.frames.get(&grab.win).copied().unwrap_or(f);
        if frame != prev {
            self.backend.update_frame(grab.win, frame, prev);
            self.frames.insert(grab.win, frame);
            self.before_max.remove(&grab.win);
        }
    }

    /// What a window resized with the mouse may not leave, on its display.
    fn drag_area(&self, frame: Rect) -> Rect {
        let (x, y) = frame.center();
        self.displays
            .iter()
            .find(|d| d.frame.contains_point(x, y))
            .or_else(|| self.displays.first())
            .map_or(frame, |d| d.area.inset(self.config.gap))
    }

    /// What `dir` growing by `step` makes of `frame`; the window is asked
    /// where it is at the next look.
    fn note_resize(
        &mut self,
        id: WinId,
        frame: Rect,
        dir: Direction,
        area: Rect,
        step: i32,
    ) -> Rect {
        let target = place::sized(frame, dir, step, (MIN_SIZE, MIN_SIZE), area);
        if target != frame {
            self.resized.insert(id);
        }
        target
    }

    /// Where a window is, or is on its way to.
    fn destination(&self, id: WinId) -> Option<Rect> {
        self.mover
            .heading(id)
            .or_else(|| self.frames.get(&id).copied())
    }

    /// Sends a window to `target`, animated.
    fn send(&mut self, id: WinId, target: Rect) {
        let target = if self.resizable.contains(&id) {
            target
        } else {
            // It cannot take that size: same place, its own size.
            let now = self.destination(id).unwrap_or(target);
            let area = self.area_of(now);
            area.align_in(target, now.w, now.h)
        };
        let current = self.frames.get(&id).copied().unwrap_or(target);
        if let Some(frame) = self.mover.send(id, current, target) {
            self.backend.update_frame(id, frame, current);
            self.frames.insert(id, frame);
        }
    }

    /// What windows may not leave: the usable part of the display showing
    /// most of `frame` (menu bar and Dock excluded), inside the gap.
    fn area_of(&mut self, frame: Rect) -> Rect {
        let (cx, cy) = frame.center();
        let displays = self.backend.displays();
        let area = displays
            .iter()
            .find(|d| d.frame.contains_point(cx, cy))
            .or_else(|| displays.first())
            .map_or_else(|| self.backend.tiling_area(), |d| d.area);
        area.inset(self.config.gap)
    }

    fn put(&mut self, id: WinId, position: Position, percent: Option<u32>) {
        let Some(now) = self.destination(id) else {
            return;
        };
        let target = position.rect(percent, self.area_of(now), now);
        self.send(id, target);
    }

    /// The other windows on screen.
    fn others(&self, id: WinId) -> Vec<(WinId, Rect)> {
        self.stack
            .iter()
            .filter(|w| **w != id)
            .filter_map(|w| Some((*w, self.destination(*w)?)))
            .collect()
    }

    fn focused(&mut self) -> Result<(WinId, Rect), String> {
        let id = self
            .backend
            .focused_window()
            .ok_or("no window is focused")?;
        let frame = self
            .destination(id)
            .ok_or_else(|| format!("window {id} is not one lily can move"))?;
        Ok((id, frame))
    }

    /// Runs a command. `Reload` and `Quit` are the caller's business.
    pub fn execute(&mut self, cmd: &Command) -> Result<String, String> {
        match *cmd {
            Command::Move(dir, points) => {
                let (id, frame) = self.focused()?;
                let step = points.unwrap_or(self.config.step);
                let area = self.area_of(frame);
                self.before_max.remove(&id);
                self.send(id, place::moved(frame, dir, step, area));
            }
            Command::Resize(arrow, points) => {
                let (id, frame) = self.focused()?;
                let step = points.unwrap_or(self.config.step);
                // The right and bottom edges move; the top-left corner stays.
                let (dir, step) = match arrow {
                    Direction::Right => (Direction::Right, step),
                    Direction::Left => (Direction::Right, -step),
                    Direction::Down => (Direction::Down, step),
                    Direction::Up => (Direction::Down, -step),
                };
                if !self.resizable.contains(&id) {
                    return Err("this window cannot be resized".into());
                }
                let area = self.area_of(frame);
                let target = self.note_resize(id, frame, dir, area, step);
                if target == frame {
                    return Ok(String::new()); // up against an edge or its minimum
                }
                self.before_max.remove(&id);
                self.send(id, target);
            }
            Command::Focus(dir) => {
                let (id, frame) = self.focused()?;
                let to =
                    place::neighbour(frame, dir, &self.others(id)).ok_or("no window that way")?;
                self.backend.focus(to);
                self.focus = Some(to);
                self.warp_to(to);
            }
            Command::Grid(grid) => {
                let (id, frame) = self.focused()?;
                let area = self.area_of(frame);
                let cell = self.grid_frame(area, grid);
                self.before_max.remove(&id);
                self.send(id, cell);
            }
            Command::Label(None) => {
                let current = self.current_space();
                let spaces = self.backend.display_spaces(current);
                let mut out = String::new();
                for (i, sid) in spaces.iter().enumerate() {
                    let n = i + 1;
                    let label = self.config.labels.get(&n).map_or("-", String::as_str);
                    let here = if *sid == current { "\tcurrent" } else { "" };
                    out += &format!("{n}\t{label}{here}\n");
                }
                return Ok(out);
            }
            Command::Label(Some((n, ref name))) => {
                let mut config = self.config.clone();
                config.set_label(n, name);
                self.set_config(config);
            }
            Command::SpaceRule {
                ref space,
                ref app,
                follow,
            } => {
                let mut config = self.config.clone();
                config.set_space_rule(app, space.clone(), follow);
                self.set_config(config);
            }
            Command::GridRule { grid, ref app } => {
                let mut config = self.config.clone();
                config.set_grid_rule(app, grid);
                self.set_config(config);
            }
            Command::BorderRadiusRule { radius, ref app } => {
                let mut config = self.config.clone();
                config.set_border_radius_rule(app, radius);
                self.set_config(config);
            }
            Command::Scratchpad {
                ref name,
                ref app,
                grid,
            } => {
                let mut config = self.config.clone();
                config.set_scratchpad(name, app, grid);
                self.set_config(config);
            }
            Command::ScratchpadToggle(ref name) => {
                let (app, grid) = self
                    .config
                    .scratchpads
                    .iter()
                    .find(|(n, _, _)| n == name)
                    .map(|(_, a, g)| (a.clone(), *g))
                    .ok_or_else(|| format!("no scratchpad is called '{name}'"))?;
                match self.scratchpad_window(name, &app) {
                    None => {
                        self.scratch_starting = Some(name.clone());
                        self.scratch_before = self.focus;
                        self.backend.launch_app(&app)?;
                    }
                    Some(win) => {
                        self.scratch_windows.insert(name.clone(), win);
                        if self.focus == Some(win) && self.stack.contains(&win) {
                            self.hide_scratchpad(win);
                        } else {
                            self.show_scratchpad(win, grid);
                        }
                    }
                }
            }
            Command::Status => {
                let current = self.current_space();
                let spaces = self.backend.display_spaces(current).len();
                return Ok(format!(
                    "version {}\nspaces {spaces}\n{}",
                    env!("CARGO_PKG_VERSION"),
                    self.backend.space_methods()
                ));
            }
            Command::Close => {
                let id = self
                    .backend
                    .focused_window()
                    .ok_or("no window is focused")?;
                if !self.backend.close(id) {
                    return Err("this window cannot be closed".into());
                }
            }
            // Back to where it was only while it still fills the display: a
            // window moved or resized since (by hand, by the app) is maxed
            // again. Terminals round their size down to whole cells.
            Command::Place(Position::Max, _) => {
                let (id, frame) = self.focused()?;
                let full = self.area_of(frame);
                let at_max = (frame.x - full.x).abs() <= 2
                    && (frame.y - full.y).abs() <= 2
                    && (full.w - frame.w).abs() <= 40
                    && (full.h - frame.h).abs() <= 40;
                match self.before_max.remove(&id) {
                    Some(before) if at_max => self.send(id, before),
                    _ => {
                        self.before_max.insert(id, frame);
                        self.send(id, full);
                    }
                }
            }
            Command::Place(position, percent) => {
                let (id, _) = self.focused()?;
                self.before_max.remove(&id);
                self.put(id, position, percent);
            }
            Command::Snap(dir) => {
                let (id, frame) = self.focused()?;
                let others: Vec<Rect> = self.others(id).into_iter().map(|(_, r)| r).collect();
                let area = self.area_of(frame);
                self.before_max.remove(&id);
                let target = place::snapped(frame, dir, area, &others, self.config.gap);
                self.send(id, target);
            }
            Command::Space(ref target) => {
                let (_, to, from) = self.resolve_space(target)?;
                if to != from {
                    self.go_to_space(from, to, None)?;
                }
            }
            Command::Send(ref target) => {
                let (id, _) = self.focused()?;
                let (n, to, from) = self.resolve_space(target)?;
                if to == from {
                    return Err(format!("the window is already on Space {n}"));
                }
                self.mover.halt(id);
                if !self.backend.place_on_space(id, to) {
                    return Err(format!("macOS would not move the window to Space {n}"));
                }
                info!("window {id} sent to Space {n}");
                // Wherever the window goes, the user goes with it.
                if let Err(e) = self.go_to_space(from, to, Some(id)) {
                    warn!("window {id} is on Space {n}, but going there failed: {e}");
                    return Err(format!(
                        "the window is on Space {n}, but going there failed: {e}"
                    ));
                }
                self.keep_focus(id);
            }
            Command::Config { ref key, ref args } => {
                if args.is_empty() {
                    return self.config.get(key).map(|v| v + "\n");
                }
                let mut config = self.config.clone();
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                config.set(key, &args)?;
                self.set_config(config);
            }
            Command::Bind(None) => {
                return Ok(self
                    .config
                    .hotkeys
                    .iter()
                    .map(|line| format!("{line}\n"))
                    .collect());
            }
            Command::Bind(Some(ref line)) => {
                let mut config = self.config.clone();
                config.hotkeys.push(line.clone());
                self.set_config(config);
            }
            Command::Query => return Ok(self.describe()),
            Command::Reload | Command::Quit => return Err("handled by the daemon".into()),
        }
        Ok(String::new())
    }

    /// One line per window, front to back: `id app x,y wxh [focused]`.
    fn describe(&mut self) -> String {
        let focused = self.backend.focused_window();
        let mut out = String::new();
        for &id in &self.stack {
            let Some(f) = self.frames.get(&id) else {
                continue;
            };
            let app = self.apps.get(&id).map_or("?", String::as_str);
            let mark = if focused == Some(id) { " focused" } else { "" };
            out += &format!("{id} {app} {},{} {}x{}{mark}\n", f.x, f.y, f.w, f.h);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Display, WindowInfo};

    #[derive(Default)]
    struct Mock {
        windows: Vec<WindowInfo>,
        focused: Option<WinId>,
        focus_calls: Vec<WinId>,
        closed: Vec<WinId>,
        displays: Vec<Display>,
        events: Vec<WindowEvent>,
        mouse: bool,
        cursor: (f32, f32),
        gesture: Option<MouseButton>,
        modifier: Option<Modifier>,
        writes: Vec<WinId>,
        /// Windows that refuse to get smaller than this, like real apps.
        min: HashMap<WinId, (i32, i32)>,
        border: Option<Border>,
        border_calls: usize,
        /// Where the window server has windows dragged by their title bar.
        server_frames: HashMap<WinId, Rect>,
        /// Frames the window list still shows, though the windows moved on.
        stale: HashMap<WinId, Rect>,
        swiping: bool,
        /// The Spaces of the display, the one shown, and where windows are
        /// (on the one shown unless listed).
        spaces: Vec<u64>,
        active: u64,
        space_of: HashMap<WinId, u64>,
        /// Whether moves to another Space and switches work.
        moves: bool,
        switches: bool,
        /// Apps hidden, apps started, pointer warps, SketchyBar triggers.
        hidden: HashSet<String>,
        launched: Vec<String>,
        warps: Vec<(f32, f32)>,
        triggers: Vec<(String, Vec<(String, String)>)>,
    }

    impl Mock {
        fn open(&mut self, id: WinId, app: &str, frame: Rect) {
            self.windows.insert(
                0,
                WindowInfo {
                    id,
                    frame,
                    app: app.into(),
                    resizable: true,
                    movable: true,
                },
            );
            self.focused = Some(id);
        }
        fn frame(&self, id: WinId) -> Rect {
            self.windows.iter().find(|w| w.id == id).unwrap().frame
        }
    }

    impl Backend for Mock {
        fn windows(&mut self) -> Vec<WindowInfo> {
            let active = self.active;
            self.windows
                .iter()
                .filter(|w| self.space_of.get(&w.id).is_none_or(|s| *s == active))
                .filter(|w| !self.hidden.contains(&w.app))
                .cloned()
                .map(|mut w| {
                    if let Some(old) = self.stale.get(&w.id) {
                        w.frame = *old;
                    }
                    w
                })
                .collect()
        }
        fn window_frame(&mut self, id: WinId) -> Option<Rect> {
            self.windows.iter().find(|w| w.id == id).map(|w| w.frame)
        }
        fn alive_windows(&mut self) -> Option<HashSet<WinId>> {
            Some(self.windows.iter().map(|w| w.id).collect())
        }
        fn window_apps(&mut self) -> HashMap<WinId, String> {
            self.windows.iter().map(|w| (w.id, w.app.clone())).collect()
        }
        fn set_app_hidden(&mut self, id: WinId, hidden: bool) {
            let app = self
                .windows
                .iter()
                .find(|w| w.id == id)
                .unwrap()
                .app
                .clone();
            if hidden {
                self.hidden.insert(app);
                if self.focused == Some(id) {
                    self.focused = None;
                }
            } else {
                self.hidden.remove(&app);
            }
        }
        fn launch_app(&mut self, app: &str) -> Result<(), String> {
            self.launched.push(app.to_string());
            Ok(())
        }
        fn warp_cursor(&mut self, x: f32, y: f32) {
            self.warps.push((x, y));
            self.cursor = (x, y);
        }
        fn trigger_sketchybar(&mut self, event: &str, vars: &[(&str, String)]) {
            let vars = vars.iter().map(|(k, v)| ((*k).to_string(), v.clone()));
            self.triggers.push((event.to_string(), vars.collect()));
        }
        fn focused_window(&mut self) -> Option<WinId> {
            self.focused
        }
        fn tiling_area(&mut self) -> Rect {
            self.displays.first().map_or(Rect::default(), |d| d.area)
        }
        fn displays(&mut self) -> Vec<Display> {
            let mut displays = self.displays.clone();
            if let Some(main) = displays.first_mut().filter(|_| self.active != 0) {
                main.space = self.active;
            }
            displays
        }
        fn shown_space(&mut self) -> u64 {
            self.active
        }
        fn display_spaces(&mut self, shown: u64) -> Vec<u64> {
            if self.spaces.contains(&shown) {
                self.spaces.clone()
            } else {
                vec![shown]
            }
        }
        fn show_space(&mut self, space: u64) -> Result<(), String> {
            if !self.switches {
                return Err("the shortcut is off".into());
            }
            self.active = space;
            if self
                .focused
                .is_some_and(|f| self.space_of.get(&f).is_some_and(|s| *s != space))
            {
                // macOS gives the focus to some window there.
                self.focused = None;
            }
            Ok(())
        }
        fn place_on_space(&mut self, id: WinId, space: u64) -> bool {
            if self.moves {
                self.space_of.insert(id, space);
            }
            self.moves
        }
        fn set_frame(&mut self, id: WinId, frame: Rect) {
            self.writes.push(id);
            let (min_w, min_h) = self.min.get(&id).copied().unwrap_or((0, 0));
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.frame = Rect::new(frame.x, frame.y, frame.w.max(min_w), frame.h.max(min_h));
            }
        }
        fn focus(&mut self, id: WinId) {
            self.focus_calls.push(id);
            self.focused = Some(id);
        }
        fn close(&mut self, id: WinId) -> bool {
            self.closed.push(id);
            true
        }
        fn take_events(&mut self) -> Vec<WindowEvent> {
            std::mem::take(&mut self.events)
        }
        fn mouse_down(&mut self) -> bool {
            self.mouse
        }
        fn cursor(&mut self) -> Option<(f32, f32)> {
            Some(self.cursor)
        }
        fn mouse_gesture(&mut self) -> Option<MouseButton> {
            self.gesture.filter(|_| self.modifier.is_some())
        }
        fn set_mouse_modifier(&mut self, modifier: Option<Modifier>) {
            self.modifier = modifier;
        }
        fn server_frame(&mut self, id: WinId) -> Option<Rect> {
            self.server_frames.get(&id).copied()
        }
        fn space_swiping(&mut self) -> bool {
            self.swiping
        }
        fn show_border(&mut self, border: Option<Border>) {
            self.border_calls += 1;
            self.border = border;
        }
    }

    const SCREEN: Rect = Rect::new(0, 30, 1000, 600);

    /// No padding, no animation: frames land at once.
    fn manager() -> Manager<Mock> {
        let backend = Mock {
            displays: vec![Display {
                frame: Rect::new(0, 0, 1000, 630),
                area: SCREEN,
                space: 1,
            }],
            ..Default::default()
        };
        let config = Config {
            gap: 0,
            animation_duration: 0.0,
            ..Config::default()
        };
        Manager::new(backend, config)
    }

    fn one_window() -> Manager<Mock> {
        let mut m = manager();
        m.backend.open(1, "Term", Rect::new(100, 100, 300, 200));
        m.refresh();
        m
    }

    fn run(m: &mut Manager<Mock>, line: &str) -> Result<String, String> {
        m.execute(&Command::parse(line).unwrap())
    }

    fn settle(m: &mut Manager<Mock>) {
        for _ in 0..600 {
            m.tick(0.016);
        }
    }

    #[test]
    fn nothing_moves_on_its_own() {
        let mut m = manager();
        m.backend.open(1, "Term", Rect::new(10, 40, 300, 200));
        m.backend.open(2, "Mail", Rect::new(20, 50, 300, 200));
        m.refresh();
        m.refresh();
        m.backend.open(3, "Notes", Rect::new(30, 60, 300, 200));
        m.refresh();
        m.backend.windows.retain(|w| w.id != 2);
        m.backend.events.push(WindowEvent::Destroyed(2));
        assert!(m.handle_events());
        m.refresh();
        settle(&mut m);
        assert!(m.backend.writes.is_empty());
    }

    #[test]
    fn a_window_moved_by_the_user_stays_there() {
        let mut m = one_window();
        m.backend.windows[0].frame = Rect::new(500, 300, 300, 200);
        m.backend.events.push(WindowEvent::Moved(1));
        assert!(m.handle_events());
        m.refresh();
        settle(&mut m);
        assert_eq!(m.backend.frame(1), Rect::new(500, 300, 300, 200));
        assert!(m.backend.writes.is_empty());
    }

    #[test]
    fn move_goes_by_the_step_or_the_distance_given() {
        let mut m = one_window();
        run(&mut m, "move left").unwrap();
        assert_eq!(m.backend.frame(1), Rect::new(60, 100, 300, 200));
        run(&mut m, "move down 10").unwrap();
        assert_eq!(m.backend.frame(1), Rect::new(60, 110, 300, 200));
    }

    #[test]
    fn moves_add_up_while_the_window_is_on_its_way() {
        let mut m = one_window();
        m.mover.set_duration(0.3);
        run(&mut m, "move right").unwrap();
        m.tick(0.016);
        run(&mut m, "move right").unwrap();
        settle(&mut m);
        assert_eq!(m.backend.frame(1).x, 180);
    }

    #[test]
    fn resize_by_axis_keeps_the_top_left_corner() {
        let mut m = one_window();
        run(&mut m, "config step 10").unwrap();
        run(&mut m, "resize right").unwrap();
        run(&mut m, "resize down").unwrap();
        settle(&mut m);
        assert_eq!(m.backend.frame(1), Rect::new(100, 100, 310, 210));
        run(&mut m, "resize left 30").unwrap();
        run(&mut m, "resize up 30").unwrap();
        settle(&mut m);
        assert_eq!(m.backend.frame(1), Rect::new(100, 100, 280, 180));
    }

    #[test]
    fn resize_goes_by_the_step_unless_told() {
        let mut m = one_window();
        run(&mut m, "resize right").unwrap();
        assert_eq!(m.backend.frame(1), Rect::new(100, 100, 340, 200));
        run(&mut m, "resize left 20").unwrap();
        assert_eq!(m.backend.frame(1), Rect::new(100, 100, 320, 200));
    }

    #[test]
    fn a_window_stops_at_the_menu_bar_and_every_other_edge() {
        let mut m = one_window();
        run(&mut m, "move up 1000").unwrap();
        assert_eq!(m.backend.frame(1).y, 30);
        run(&mut m, "move left 1000").unwrap();
        assert_eq!(m.backend.frame(1).x, 0);
        run(&mut m, "move right 2000").unwrap();
        assert_eq!(m.backend.frame(1).max_x(), 1000);
        run(&mut m, "move down 2000").unwrap();
        assert_eq!(m.backend.frame(1).max_y(), 630);
    }

    #[test]
    fn moving_and_growing_keep_the_gap() {
        let mut m = one_window();
        m.config.gap = 10;
        run(&mut m, "move up 1000").unwrap();
        assert_eq!(m.backend.frame(1).y, 40);
        run(&mut m, "move right 2000").unwrap();
        assert_eq!(m.backend.frame(1).max_x(), 990);
        run(&mut m, "resize down 2000").unwrap();
        assert_eq!(m.backend.frame(1).max_y(), 620);
        run(&mut m, "move left 2000").unwrap();
        run(&mut m, "resize right 2000").unwrap();
        assert_eq!(m.backend.frame(1).x, 10);
        assert_eq!(m.backend.frame(1).max_x(), 990);
    }

    #[test]
    fn growing_stops_at_the_edges_too() {
        let mut m = one_window();
        run(&mut m, "resize down 1000").unwrap();
        assert_eq!(m.backend.frame(1).y, 100, "the top stayed");
        assert_eq!(m.backend.frame(1).max_y(), 630);
        run(&mut m, "resize right 2000").unwrap();
        assert_eq!(m.backend.frame(1).max_x(), 1000);
    }

    #[test]
    fn resize_keeps_working_when_the_window_list_lags_behind() {
        let mut m = one_window();
        run(&mut m, "config step 25").unwrap();
        let start = m.backend.frame(1);
        // Grow; the window list still shows the old frame for a moment.
        run(&mut m, "resize right").unwrap();
        m.backend.stale.insert(1, start);
        m.refresh();
        assert_eq!(m.frames[&1].w, 325, "what the app has counts");
        // Shrink; again the list lags.
        run(&mut m, "resize left").unwrap();
        m.backend.stale.insert(1, Rect::new(100, 100, 325, 200));
        m.refresh();
        assert_eq!(m.backend.frame(1).w, 300);
        // And again, and again: nothing was taken for a smallest size.
        for w in [275, 250, 225] {
            run(&mut m, "resize left").unwrap();
            m.backend.stale.clear();
            m.refresh();
            assert_eq!(m.backend.frame(1).w, w);
        }
    }

    #[test]
    fn a_window_that_will_not_shrink_keeps_its_size_and_lily_sees_it() {
        let mut m = one_window();
        m.backend.min.insert(1, (300, 50));
        let f = m.backend.frame(1);
        run(&mut m, "resize left 40").unwrap();
        m.refresh();
        assert_eq!(m.backend.frame(1), f);
        assert_eq!(m.frames[&1], f, "what the window really took");
        // Nothing is remembered: once it can shrink, it does.
        m.backend.min.clear();
        run(&mut m, "resize left 40").unwrap();
        assert_eq!(m.backend.frame(1).w, 260);
    }

    #[test]
    fn dragging_goes_past_the_edges() {
        let mut m = one_window();
        drag(&mut m, MouseButton::Left, (200.0, 150.0), (-100.0, 900.0));
        assert_eq!(m.backend.frame(1), Rect::new(-200, 850, 300, 200));
    }

    #[test]
    fn a_window_dragged_out_comes_back_with_the_keyboard() {
        let mut m = one_window();
        drag(&mut m, MouseButton::Left, (200.0, 150.0), (-100.0, 150.0));
        run(&mut m, "place left").unwrap();
        assert_eq!(m.backend.frame(1).x, 0);
    }

    #[test]
    fn resizing_with_the_mouse_stops_at_the_edges() {
        let mut m = one_window();
        drag(&mut m, MouseButton::Right, (120.0, 120.0), (-300.0, -300.0));
        let f = m.backend.frame(1);
        assert_eq!((f.x, f.y), (0, 30));
        assert_eq!((f.max_x(), f.max_y()), (400, 300));
    }

    #[test]
    fn a_fixed_size_window_says_so_and_stays() {
        let mut m = manager();
        m.backend
            .open(1, "Calculator", Rect::new(100, 100, 200, 300));
        m.backend.windows[0].resizable = false;
        m.refresh();
        assert!(run(&mut m, "resize right").is_err());
        run(&mut m, "place right").unwrap();
        let f = m.backend.frame(1);
        assert_eq!((f.w, f.h), (200, 300));
        assert_eq!(f.max_x(), SCREEN.max_x());
    }

    #[test]
    fn place_uses_the_area_inside_the_gap() {
        let mut m = one_window();
        m.config.gap = 10;
        run(&mut m, "place left 50%").unwrap();
        assert_eq!(m.backend.frame(1), Rect::new(10, 40, 490, 580));
    }

    #[test]
    fn place_max_toggles_back_to_where_the_window_was() {
        let mut m = one_window();
        run(&mut m, "place max").unwrap();
        assert_eq!(m.backend.frame(1), SCREEN);
        run(&mut m, "place max").unwrap();
        assert_eq!(m.backend.frame(1), Rect::new(100, 100, 300, 200));
    }

    #[test]
    fn moving_a_maximised_window_forgets_where_it_was() {
        let mut m = one_window();
        run(&mut m, "place max").unwrap();
        run(&mut m, "move left").unwrap();
        run(&mut m, "place max").unwrap();
        assert_eq!(m.backend.frame(1), SCREEN);
    }

    #[test]
    fn place_works_on_the_display_the_window_is_on() {
        let mut m = one_window();
        let second = Rect::new(1000, 0, 800, 600);
        m.backend.displays.push(Display {
            frame: second,
            area: second,
            space: 2,
        });
        m.backend.windows[0].frame = Rect::new(1100, 100, 300, 200);
        m.refresh();
        run(&mut m, "place left").unwrap();
        assert_eq!(m.backend.frame(1), Rect::new(1000, 0, 400, 600));
    }

    #[test]
    fn snap_stops_at_the_neighbour() {
        let mut m = one_window();
        m.backend.open(2, "Mail", Rect::new(600, 100, 300, 200));
        m.backend.focused = Some(1);
        m.refresh();
        m.config.gap = 8;
        run(&mut m, "snap right").unwrap();
        assert_eq!(m.backend.frame(1).max_x(), 592);
    }

    #[test]
    fn focus_goes_to_the_window_that_way() {
        let mut m = one_window();
        m.backend.open(2, "Mail", Rect::new(600, 100, 300, 200));
        m.refresh();
        run(&mut m, "focus left").unwrap();
        assert_eq!(m.backend.focus_calls, [1]);
        assert!(run(&mut m, "focus left").is_err());
    }

    #[test]
    fn close_closes_the_focused_window() {
        let mut m = one_window();
        run(&mut m, "close").unwrap();
        assert_eq!(m.backend.closed, [1]);
        m.backend.focused = None;
        assert!(run(&mut m, "close").is_err());
    }

    #[test]
    fn commands_need_a_focused_window_lily_knows() {
        let mut m = one_window();
        m.backend.focused = None;
        assert!(run(&mut m, "move left").is_err());
        m.backend.focused = Some(9);
        assert!(run(&mut m, "move left").is_err());
    }

    #[test]
    fn windows_there_at_startup_are_left_alone() {
        let mut m = manager();
        m.config.set_grid_rule("Term", [1, 1, 0, 0, 1, 1]);
        m.backend.open(1, "Term", Rect::new(100, 100, 300, 200));
        m.refresh();
        assert!(m.backend.writes.is_empty());
    }

    #[test]
    fn panels_and_dialogs_are_not_touched() {
        let mut m = manager();
        m.backend.open(1, "Term", Rect::new(100, 100, 300, 200));
        m.backend.windows[0].movable = false;
        m.refresh();
        assert!(run(&mut m, "move left").is_err());
    }

    fn drag(m: &mut Manager<Mock>, button: MouseButton, from: (f32, f32), to: (f32, f32)) {
        m.track_pointer();
        m.backend.cursor = from;
        m.backend.gesture = Some(button);
        m.track_pointer();
        m.backend.cursor = to;
        m.track_pointer();
        m.backend.gesture = None;
        m.track_pointer();
    }

    #[test]
    fn a_modifier_drag_moves_the_window_under_the_pointer() {
        let mut m = one_window();
        m.backend.open(2, "Mail", Rect::new(600, 100, 300, 200));
        m.refresh();
        drag(&mut m, MouseButton::Left, (200.0, 150.0), (250.0, 200.0));
        assert_eq!(m.backend.frame(1), Rect::new(150, 150, 300, 200));
        assert_eq!(m.backend.frame(2), Rect::new(600, 100, 300, 200));
        assert_eq!(m.backend.focus_calls, [1]);
    }

    #[test]
    fn a_right_drag_resizes_from_the_nearest_corner() {
        let mut m = one_window();
        drag(&mut m, MouseButton::Right, (350.0, 250.0), (400.0, 300.0));
        assert_eq!(m.backend.frame(1), Rect::new(100, 100, 350, 250));
        drag(&mut m, MouseButton::Right, (120.0, 120.0), (100.0, 100.0));
        assert_eq!(m.backend.frame(1), Rect::new(80, 80, 370, 270));
    }

    #[test]
    fn a_click_with_the_modifier_moves_nothing() {
        let mut m = one_window();
        drag(&mut m, MouseButton::Left, (200.0, 150.0), (201.0, 151.0));
        assert_eq!(m.backend.frame(1), Rect::new(100, 100, 300, 200));
    }

    #[test]
    fn a_window_closed_while_held_is_let_go() {
        let mut m = one_window();
        m.track_pointer();
        m.backend.cursor = (200.0, 150.0);
        m.backend.gesture = Some(MouseButton::Left);
        m.track_pointer();
        m.backend.windows.clear();
        m.backend.events.push(WindowEvent::Destroyed(1));
        m.handle_events();
        m.backend.cursor = (300.0, 150.0);
        m.track_pointer();
        assert!(m.backend.writes.is_empty());
    }

    #[test]
    fn query_lists_the_windows_front_to_back() {
        let mut m = one_window();
        m.backend.open(2, "Mail", Rect::new(600, 100, 300, 200));
        m.refresh();
        assert_eq!(
            run(&mut m, "query").unwrap(),
            "2 Mail 600,100 300x200 focused\n1 Term 100,100 300x200\n"
        );
    }

    #[test]
    fn rules_and_bindings_come_through_commands() {
        let mut m = manager();
        run(&mut m, "rule grid 1:2:0:0:1:1 Zed").unwrap();
        assert_eq!(m.config.grid_rule_for("Zed"), Some([1, 2, 0, 0, 1, 1]));
        run(&mut m, "bind alt - h : lily focus left").unwrap();
        assert_eq!(run(&mut m, "bind").unwrap(), "alt - h : lily focus left\n");
        run(&mut m, "config step 25").unwrap();
        assert_eq!(run(&mut m, "config step").unwrap(), "25\n");
    }

    fn bordered() -> Manager<Mock> {
        let mut m = manager();
        run(&mut m, "config border on").unwrap();
        m.backend.open(1, "Term", Rect::new(100, 100, 300, 200));
        m.backend.open(2, "Mail", Rect::new(500, 100, 300, 200));
        m.refresh();
        m.pump();
        m
    }

    #[test]
    fn no_border_unless_asked_for() {
        let mut m = one_window();
        m.pump();
        assert_eq!(m.backend.border_calls, 0);
    }

    #[test]
    fn the_border_goes_round_the_focused_window() {
        let m = bordered();
        let border = m.backend.border.unwrap();
        assert_eq!(border.window, 2);
        assert_eq!(border.rect, m.backend.frame(2));
        assert_eq!(border.width, 2);
        assert_eq!(border.radius, 12);
    }

    #[test]
    fn the_border_follows_focus() {
        let mut m = bordered();
        m.backend.focused = Some(1);
        m.refresh();
        m.pump();
        assert_eq!(m.backend.border.unwrap().window, 1);
        run(&mut m, "focus right").unwrap();
        m.pump();
        assert_eq!(m.backend.border.unwrap().window, 2);
    }

    #[test]
    fn the_border_moves_with_the_window_in_flight() {
        let mut m = bordered();
        run(&mut m, "config animation on").unwrap();
        run(&mut m, "move down 100").unwrap();
        m.tick(0.05);
        m.pump();
        assert!(m.is_animating());
        let mid = m.backend.border.unwrap().rect;
        assert_eq!(mid, m.backend.frame(2));
        assert!(mid.y > 100 && mid.y < 200, "{mid:?}");
        settle(&mut m);
        m.pump();
        assert_eq!(m.backend.border.unwrap().rect, m.backend.frame(2));
    }

    #[test]
    fn the_border_follows_a_modifier_drag() {
        let mut m = bordered();
        drag(&mut m, MouseButton::Left, (600.0, 200.0), (650.0, 260.0));
        m.pump();
        assert_eq!(m.backend.border.unwrap().rect, m.backend.frame(2));
        assert_eq!(m.backend.frame(2), Rect::new(550, 160, 300, 200));
    }

    #[test]
    fn a_still_border_is_left_alone() {
        let mut m = bordered();
        let calls = m.backend.border_calls;
        for _ in 0..10 {
            m.pump();
        }
        assert_eq!(m.backend.border_calls, calls);
    }

    #[test]
    fn the_border_goes_when_turned_off_or_the_window_closes() {
        let mut m = bordered();
        m.backend.windows.clear();
        m.backend.focused = None;
        m.backend.events.push(WindowEvent::Destroyed(2));
        m.handle_events();
        m.pump();
        assert_eq!(m.backend.border, None);

        let mut m = bordered();
        run(&mut m, "config border off").unwrap();
        m.pump();
        assert_eq!(m.backend.border, None);
    }

    #[test]
    fn the_border_follows_a_window_dragged_by_its_title_bar() {
        let mut m = bordered();
        m.backend.mouse = true;
        let moved = Rect::new(530, 95, 300, 200);
        m.backend.server_frames.insert(2, moved);
        m.pump();
        assert_eq!(m.backend.border.unwrap().rect, moved);
    }

    #[test]
    fn the_border_keeps_to_a_window_that_will_not_shrink() {
        let mut m = bordered();
        run(&mut m, "config animation on").unwrap();
        let before = m.backend.frame(2);
        // The app keeps its size whatever lily asks for.
        m.backend.server_frames.insert(2, before);
        run(&mut m, "resize left 200").unwrap();
        for _ in 0..30 {
            m.tick(0.016);
            m.pump();
            assert_eq!(m.backend.border.unwrap().rect, before);
        }
    }

    #[test]
    fn the_border_hides_while_the_spaces_slide() {
        let mut m = bordered();
        m.backend.swiping = true;
        m.pump();
        assert_eq!(m.backend.border, None);
        m.backend.swiping = false;
        m.pump();
        assert!(m.backend.border.is_some());
    }

    #[test]
    fn border_settings_reach_the_ring() {
        let mut m = bordered();
        run(&mut m, "config border_color 90a8f2").unwrap();
        run(&mut m, "config border_width 3").unwrap();
        run(&mut m, "config border_radius 16").unwrap();
        m.pump();
        let b = m.backend.border.unwrap();
        assert_eq!((b.width, b.radius), (3, 16));
        assert!((b.color[0] - 0.565).abs() < 0.01 && b.color[3] == 1.0);
    }

    fn three_spaces() -> Manager<Mock> {
        let mut m = manager();
        m.backend.spaces = vec![10, 20, 30];
        m.backend.active = 10;
        m.backend.moves = true;
        m.backend.switches = true;
        m.backend.open(1, "Term", Rect::new(100, 100, 300, 200));
        m.refresh();
        m
    }

    #[test]
    fn space_goes_by_number_and_round_the_display() {
        let mut m = three_spaces();
        run(&mut m, "space 3").unwrap();
        assert_eq!(m.backend.active, 30);
        run(&mut m, "space next").unwrap();
        assert_eq!(m.backend.active, 10, "next wraps round");
        run(&mut m, "space prev").unwrap();
        assert_eq!(m.backend.active, 30);
        assert!(run(&mut m, "space 4").is_err());
        assert_eq!(m.backend.active, 30);
    }

    #[test]
    fn recent_goes_back_and_forth() {
        let mut m = three_spaces();
        assert!(run(&mut m, "space recent").is_err());
        run(&mut m, "space 2").unwrap();
        run(&mut m, "space recent").unwrap();
        assert_eq!(m.backend.active, 10);
        run(&mut m, "space recent").unwrap();
        assert_eq!(m.backend.active, 20);
    }

    #[test]
    fn recent_counts_a_switch_lily_did_not_make() {
        let mut m = three_spaces();
        m.backend.active = 30; // a swipe
        m.refresh();
        run(&mut m, "space recent").unwrap();
        assert_eq!(m.backend.active, 10);
    }

    #[test]
    fn send_takes_the_user_along_and_keeps_the_window_in_focus() {
        let mut m = three_spaces();
        run(&mut m, "send 2").unwrap();
        assert_eq!(m.backend.space_of[&1], 20);
        assert_eq!(m.backend.active, 20, "the user went too");
        assert_eq!(m.backend.focused, Some(1));
        // macOS hands the focus elsewhere a moment later; it comes back.
        m.backend.focused = Some(99);
        std::thread::sleep(HOLD_FOCUS_RETRY);
        m.pump();
        assert_eq!(m.backend.focused, Some(1));
        run(&mut m, "send recent").unwrap();
        assert_eq!((m.backend.space_of[&1], m.backend.active), (10, 10));
    }

    #[test]
    fn send_does_nothing_on_its_own_space_or_without_a_window() {
        let mut m = three_spaces();
        assert!(run(&mut m, "send 1").is_err());
        m.backend.focused = None;
        m.refresh();
        assert!(run(&mut m, "send 2").is_err());
        assert_eq!(m.backend.active, 10);
    }

    #[test]
    fn a_window_macos_would_not_move_stays_and_so_does_the_user() {
        let mut m = three_spaces();
        m.backend.moves = false;
        assert!(run(&mut m, "send 2").is_err());
        assert_eq!(m.backend.active, 10);
        assert!(!m.backend.space_of.contains_key(&1));
    }

    #[test]
    fn a_failed_switch_says_where_the_window_went() {
        let mut m = three_spaces();
        m.backend.switches = false;
        let e = run(&mut m, "send 3").unwrap_err();
        assert!(e.contains("Space 3"), "{e}");
        assert_eq!(m.backend.space_of[&1], 30);
    }

    #[test]
    fn sending_moves_nothing_on_screen() {
        let mut m = three_spaces();
        let before = m.backend.frame(1);
        run(&mut m, "send 2").unwrap();
        settle(&mut m);
        m.refresh();
        assert_eq!(m.backend.frame(1), before);
        assert!(m.backend.writes.is_empty());
    }

    #[test]
    fn labels_name_spaces_for_space_and_send() {
        let mut m = three_spaces();
        run(&mut m, "label 2 web").unwrap();
        run(&mut m, "space web").unwrap();
        assert_eq!(m.backend.active, 20);
        assert_eq!(
            run(&mut m, "label").unwrap(),
            "1\t-\n2\tweb\tcurrent\n3\t-\n"
        );
        run(&mut m, "space 1").unwrap();
        m.refresh();
        run(&mut m, "send web").unwrap();
        assert_eq!((m.backend.space_of[&1], m.backend.active), (20, 20));
        assert!(run(&mut m, "space mail").unwrap_err().contains("labelled"));
    }

    #[test]
    fn a_space_rule_sends_new_windows_and_follows_if_asked() {
        let mut m = three_spaces();
        run(&mut m, "label 2 web").unwrap();
        run(&mut m, "rule space web Safari").unwrap();
        run(&mut m, "rule space 3 Mail --follow").unwrap();
        m.backend.open(2, "Safari", Rect::new(200, 200, 300, 200));
        m.refresh();
        assert_eq!(m.backend.space_of[&2], 20);
        assert_eq!(m.backend.active, 10, "the user stays");
        m.backend.open(3, "Mail", Rect::new(200, 200, 300, 200));
        m.refresh();
        assert_eq!(m.backend.space_of[&3], 30);
        assert_eq!(m.backend.active, 30, "and goes with this one");
        assert_eq!(m.backend.focused, Some(3));
    }

    #[test]
    fn windows_there_before_a_space_rule_stay() {
        let mut m = three_spaces();
        m.backend.open(2, "Safari", Rect::new(200, 200, 300, 200));
        m.refresh();
        run(&mut m, "rule space 2 Safari").unwrap();
        m.refresh();
        assert!(!m.backend.space_of.contains_key(&2));
    }

    #[test]
    fn max_again_restores_only_while_still_maxed() {
        let mut m = one_window();
        let start = m.backend.frame(1);
        run(&mut m, "place max").unwrap();
        settle(&mut m);
        let full = m.backend.frame(1);
        assert_ne!(full, start);
        run(&mut m, "place max").unwrap();
        settle(&mut m);
        assert_eq!(m.backend.frame(1), start);

        run(&mut m, "place max").unwrap();
        settle(&mut m);
        // Dragged off by hand: the next press maxes it again.
        m.backend.windows[0].frame = Rect::new(300, 300, 400, 300);
        m.backend.events.push(WindowEvent::Moved(1));
        m.handle_events();
        m.refresh();
        run(&mut m, "place max").unwrap();
        settle(&mut m);
        assert_eq!(m.backend.frame(1), full);
    }

    #[test]
    fn grid_puts_the_window_in_a_cell_with_the_gap_between() {
        let mut m = one_window();
        run(&mut m, "grid 1:2:1:0:1:1").unwrap();
        settle(&mut m);
        assert_eq!(m.backend.frame(1), Rect::new(500, 30, 500, 600));
        run(&mut m, "config gap 10").unwrap();
        run(&mut m, "grid 1:2:0:0:1:1").unwrap();
        settle(&mut m);
        // 10 from the screen's edges, 5 from the middle.
        assert_eq!(m.backend.frame(1), Rect::new(10, 40, 485, 580));
    }

    #[test]
    fn a_grid_rule_places_new_windows_only() {
        let mut m = one_window();
        run(&mut m, "rule grid 2:2:0:0:1:1 Mail").unwrap();
        m.backend.open(2, "Mail", Rect::new(300, 300, 300, 200));
        m.refresh();
        settle(&mut m);
        assert_eq!(m.backend.frame(2), Rect::new(0, 30, 500, 300));
        assert_eq!(m.backend.frame(1), Rect::new(100, 100, 300, 200));
    }

    fn with_scratchpad() -> Manager<Mock> {
        let mut m = three_spaces();
        run(&mut m, "scratchpad term kitty 2:2:0:0:1:1").unwrap();
        m
    }

    #[test]
    fn a_scratchpad_starts_its_app_and_comes_out_where_it_says() {
        let mut m = with_scratchpad();
        run(&mut m, "scratchpad toggle term").unwrap();
        assert_eq!(m.backend.launched, ["kitty"]);
        m.backend.open(5, "kitty", Rect::new(300, 300, 300, 200));
        m.refresh();
        settle(&mut m);
        assert_eq!(m.backend.frame(5), Rect::new(0, 30, 500, 300));
        assert_eq!(m.backend.focused, Some(5));
    }

    #[test]
    fn a_scratchpad_toggles_away_and_back_with_the_focus() {
        let mut m = with_scratchpad();
        m.backend.open(5, "kitty", Rect::new(300, 300, 300, 200));
        m.refresh();
        // kitty is in focus: toggling puts it away, Term gets the focus.
        m.scratch_before = Some(1);
        run(&mut m, "scratchpad toggle term").unwrap();
        assert!(m.backend.hidden.contains("kitty"));
        assert_eq!(m.backend.focused, Some(1));
        m.refresh();
        run(&mut m, "scratchpad toggle term").unwrap();
        assert!(!m.backend.hidden.contains("kitty"));
        assert_eq!(m.backend.focused, Some(5));
        assert!(m.backend.launched.is_empty());
    }

    #[test]
    fn a_scratchpad_comes_to_the_space_on_screen_and_goes_when_left() {
        let mut m = with_scratchpad();
        m.backend.open(5, "kitty", Rect::new(300, 300, 300, 200));
        m.backend.space_of.insert(5, 30);
        m.backend.focused = Some(1);
        m.refresh();
        run(&mut m, "scratchpad toggle term").unwrap();
        assert_eq!(m.backend.space_of[&5], 10, "brought here");
        m.refresh();
        run(&mut m, "space 2").unwrap();
        m.refresh();
        assert!(m.backend.hidden.contains("kitty"), "left behind, put away");
    }

    #[test]
    fn a_missing_scratchpad_is_an_error() {
        let mut m = three_spaces();
        assert!(run(&mut m, "scratchpad toggle term").is_err());
    }

    #[test]
    fn mouse_follows_focus_when_asked() {
        let mut m = manager();
        m.backend.open(1, "Term", Rect::new(0, 30, 400, 300));
        m.backend.open(2, "Mail", Rect::new(500, 30, 400, 300));
        m.refresh();
        run(&mut m, "focus left").unwrap();
        assert!(m.backend.warps.is_empty(), "off by default");
        run(&mut m, "config mouse_follows_focus on").unwrap();
        run(&mut m, "focus right").unwrap();
        assert_eq!(m.backend.warps, [(700.0, 180.0)]);
    }

    #[test]
    fn focus_follows_a_pointer_that_comes_to_rest() {
        let mut m = manager();
        m.backend.open(1, "Term", Rect::new(0, 30, 400, 300));
        m.backend.open(2, "Mail", Rect::new(500, 30, 400, 300));
        m.refresh();
        run(&mut m, "config focus_follows_mouse on").unwrap();
        m.backend.cursor = (600.0, 100.0);
        m.track_pointer();
        m.backend.cursor = (100.0, 100.0);
        m.track_pointer();
        assert_eq!(m.backend.focused, Some(2), "not at once");
        m.tick(0.2);
        m.track_pointer();
        assert_eq!(m.backend.focused, Some(1));
        // A window sliding under a still pointer does not take the focus.
        m.backend.focused = Some(2);
        m.refresh();
        m.tick(0.2);
        m.track_pointer();
        assert_eq!(m.backend.focused, Some(2));
    }

    #[test]
    fn an_app_can_have_corners_of_its_own() {
        let mut m = bordered();
        run(&mut m, "rule border_radius 0 Mail").unwrap();
        m.pump();
        assert_eq!(m.backend.border.unwrap().radius, 0);
        m.backend.focused = Some(1);
        m.refresh();
        m.pump();
        assert_eq!(m.backend.border.unwrap().radius, 12);
    }

    #[test]
    fn sketchybar_hears_of_spaces_and_focus_when_asked() {
        let mut m = three_spaces();
        m.pump();
        assert!(m.backend.triggers.is_empty(), "off by default");
        run(&mut m, "config sketchybar on").unwrap();
        run(&mut m, "label 2 web").unwrap();
        m.pump();
        let events: Vec<&str> = m.backend.triggers.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(events, ["lily_space_change", "lily_focus_change"]);
        m.backend.triggers.clear();
        m.pump();
        assert!(m.backend.triggers.is_empty(), "nothing changed");
        run(&mut m, "send web").unwrap();
        m.pump();
        let (event, vars) = &m.backend.triggers[0];
        assert_eq!(event, "lily_space_change");
        assert!(vars.contains(&("LILY_SPACE".into(), "2".into())));
        assert!(vars.contains(&("LILY_SPACE_LABEL".into(), "web".into())));
        let (event, vars) = &m.backend.triggers[1];
        assert_eq!(event, "lily_focus_change");
        assert!(vars.contains(&("LILY_APP".into(), "Term".into())));
    }

    #[test]
    fn status_says_the_version_and_the_spaces() {
        let mut m = three_spaces();
        let out = run(&mut m, "status").unwrap();
        assert!(out.starts_with(&format!(
            "version {}\nspaces 3\n",
            env!("CARGO_PKG_VERSION")
        )));
    }

    #[test]
    fn a_move_alone_still_slides_from_where_it_is() {
        let mut m = one_window();
        run(&mut m, "config animation on").unwrap();
        run(&mut m, "move right 100").unwrap();
        assert_eq!(
            m.backend.frame(1),
            Rect::new(100, 100, 300, 200),
            "nothing yet"
        );
        settle(&mut m);
        assert_eq!(m.backend.frame(1), Rect::new(200, 100, 300, 200));
    }
}
