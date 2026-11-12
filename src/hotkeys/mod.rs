//! Keyboard shortcuts.
//!
//! Every `lily bind '<line>'` in lilyrc adds one shortcut:
//!
//! ```text
//! shift + alt - left : lily focus left
//! ^ modifiers   ^ key  ^ command
//! ```
//!
//! Modifiers are joined with `+` and end with `-`; a shortcut can have none.
//! The key is a character, a named key (`return`, `left`, `f5`, ...) or a
//! key code (`0x32`). `->` before the `:` lets the key press through to the
//! app as well. A command that is just `lily ...` runs inside lily; anything
//! else runs in the shell.
//!
//! This module only parses and matches; reading the keyboard is the
//! platform's business.

use std::collections::HashMap;

/// One side, both or either of a modifier pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    /// Not held.
    #[default]
    Off,
    /// Either key, or both.
    Any,
    Left,
    Right,
}

/// The modifiers of a shortcut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub alt: Side,
    pub shift: Side,
    pub cmd: Side,
    pub ctrl: Side,
    pub fun: bool,
}

/// The modifiers held during a key press: for each pair, whether the left
/// and the right key are down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub alt: (bool, bool),
    pub shift: (bool, bool),
    pub cmd: (bool, bool),
    pub ctrl: (bool, bool),
    pub fun: bool,
}

fn side_matches(want: Side, (left, right): (bool, bool)) -> bool {
    match want {
        Side::Off => !left && !right,
        Side::Any => left || right,
        Side::Left => left && !right,
        Side::Right => right && !left,
    }
}

impl Mods {
    /// Whether these modifiers are the ones held. `ignore_fn`: the key is
    /// one macOS always reports with fn (arrows, function keys, ...).
    pub fn matches(&self, held: &Held, ignore_fn: bool) -> bool {
        side_matches(self.alt, held.alt)
            && side_matches(self.shift, held.shift)
            && side_matches(self.cmd, held.cmd)
            && side_matches(self.ctrl, held.ctrl)
            && (ignore_fn || self.fun == held.fun)
    }

    fn add(&mut self, name: &str) -> Result<(), String> {
        let set = |slot: &mut Side, side: Side| -> Result<(), String> {
            // Both sides named: either will do.
            *slot = if *slot == Side::Off || *slot == side {
                side
            } else {
                Side::Any
            };
            Ok(())
        };
        match name {
            "alt" | "opt" | "option" => set(&mut self.alt, Side::Any),
            "lalt" => set(&mut self.alt, Side::Left),
            "ralt" => set(&mut self.alt, Side::Right),
            "shift" => set(&mut self.shift, Side::Any),
            "lshift" => set(&mut self.shift, Side::Left),
            "rshift" => set(&mut self.shift, Side::Right),
            "cmd" | "command" => set(&mut self.cmd, Side::Any),
            "lcmd" => set(&mut self.cmd, Side::Left),
            "rcmd" => set(&mut self.cmd, Side::Right),
            "ctrl" | "control" => set(&mut self.ctrl, Side::Any),
            "lctrl" => set(&mut self.ctrl, Side::Left),
            "rctrl" => set(&mut self.ctrl, Side::Right),
            "fn" => {
                self.fun = true;
                Ok(())
            }
            "hyper" => ["cmd", "alt", "shift", "ctrl"]
                .iter()
                .try_for_each(|m| self.add(m)),
            "meh" => ["alt", "shift", "ctrl"]
                .iter()
                .try_for_each(|m| self.add(m)),
            other => Err(format!("unknown modifier '{other}'")),
        }
    }
}

/// The key of a shortcut, before the keyboard layout is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Whatever key types this character on the current layout.
    Char(char),
    /// A fixed key code (`kVK_*`).
    Code(u16),
}

/// Named keys and their codes (`kVK_*` in Carbon's Events.h). The `bool`:
/// macOS reports the key with the fn flag set.
const NAMED_KEYS: &[(&str, u16, bool)] = &[
    ("return", 0x24, false),
    ("enter", 0x24, false),
    ("tab", 0x30, false),
    ("space", 0x31, false),
    ("backspace", 0x33, false),
    ("escape", 0x35, false),
    ("esc", 0x35, false),
    ("delete", 0x75, true),
    ("insert", 0x72, true),
    ("home", 0x73, true),
    ("end", 0x77, true),
    ("pageup", 0x74, true),
    ("pagedown", 0x79, true),
    ("left", 0x7B, true),
    ("right", 0x7C, true),
    ("down", 0x7D, true),
    ("up", 0x7E, true),
    ("f1", 0x7A, true),
    ("f2", 0x78, true),
    ("f3", 0x63, true),
    ("f4", 0x76, true),
    ("f5", 0x60, true),
    ("f6", 0x61, true),
    ("f7", 0x62, true),
    ("f8", 0x64, true),
    ("f9", 0x65, true),
    ("f10", 0x6D, true),
    ("f11", 0x67, true),
    ("f12", 0x6F, true),
    ("f13", 0x69, true),
    ("f14", 0x6B, true),
    ("f15", 0x71, true),
    ("f16", 0x6A, true),
    ("f17", 0x40, true),
    ("f18", 0x4F, true),
    ("f19", 0x50, true),
    ("f20", 0x5A, true),
];

/// Whether macOS sets the fn flag on presses of key `code`.
fn reports_fn(code: u16) -> bool {
    NAMED_KEYS.iter().any(|&(_, c, f)| c == code && f)
}

/// A shortcut as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub mods: Mods,
    pub key: Key,
    pub passthrough: bool,
    pub command: String,
}

fn parse_key(text: &str) -> Result<Key, String> {
    let lower = text.to_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        let code = u16::from_str_radix(hex, 16).map_err(|_| format!("bad key code '{text}'"))?;
        return Ok(Key::Code(code));
    }
    if let Some(&(_, code, _)) = NAMED_KEYS.iter().find(|(n, _, _)| *n == lower) {
        return Ok(Key::Code(code));
    }
    let mut chars = lower.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(Key::Char(c)),
        (None, _) => Err("missing key".into()),
        _ => Err(format!("unknown key '{text}'")),
    }
}

impl Binding {
    /// Parses one shortcut line.
    pub fn parse(line: &str) -> Result<Self, String> {
        let (keys, command) = line
            .split_once(':')
            .ok_or("expected ':' and a command after the key")?;
        let command = command.trim();
        if command.is_empty() {
            return Err("missing command after ':'".into());
        }
        let mut keys = keys.trim();
        let passthrough = match keys.strip_suffix("->") {
            Some(rest) => {
                keys = rest.trim_end();
                true
            }
            None => false,
        };
        // "mods - key", or just "key". The key itself may be '-'.
        let (mods_text, key_text) = match keys.rfind('-') {
            Some(i) if i + 1 < keys.len() && !keys[i + 1..].trim().is_empty() => {
                (Some(&keys[..i]), keys[i + 1..].trim())
            }
            Some(i) if i > 0 && keys[..i].trim_end().ends_with('-') => {
                let head = keys[..i].trim_end();
                (Some(&head[..head.len() - 1]), "-")
            }
            _ => (None, keys),
        };
        let mut mods = Mods::default();
        if let Some(text) = mods_text {
            if text.trim().is_empty() {
                return Err("missing modifier before '-'".into());
            }
            for name in text.split('+') {
                let name = name.trim().to_lowercase();
                if name.is_empty() {
                    return Err("empty modifier between '+'".into());
                }
                mods.add(&name)?;
            }
        }
        Ok(Self {
            mods,
            key: parse_key(key_text)?,
            passthrough,
            command: command.to_string(),
        })
    }
}

/// Checks shortcut lines; returns how many there are, or the first error.
pub fn check(text: &str) -> Result<usize, String> {
    let mut n = 0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        Binding::parse(line).map_err(|e| format!("'{line}': {e}"))?;
        n += 1;
    }
    Ok(n)
}

/// What to do with a key press.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// A lily command line, run by the daemon.
    Lily(String),
    /// A command for the shell.
    Shell(String),
}

/// A command that only runs lily with plain words runs in the daemon; one
/// that needs the shell (quotes, pipes, variables, several commands) gets
/// the shell.
pub fn action(command: &str) -> Action {
    const SHELL_CHARS: &[char] = &[
        ';', '&', '|', '$', '`', '<', '>', '"', '\'', '\\', '(', ')', '{', '}', '*', '?', '[', ']',
        '~', '#', '=', '!', '\n',
    ];
    let mut words = command.split_whitespace();
    let is_lily = words
        .next()
        .is_some_and(|w| w == "lily" || w.ends_with("/lily"));
    let rest: Vec<&str> = words.collect();
    let plain = !command.contains(SHELL_CHARS);
    let first = rest.first().copied().unwrap_or("");
    // Service commands manage the daemon itself, from outside.
    let service = matches!(
        first,
        "run" | "install" | "uninstall" | "start" | "stop" | "restart" | "help" | "-h" | "--help"
    );
    if is_lily && plain && !rest.is_empty() && !service {
        Action::Lily(rest.join(" "))
    } else {
        Action::Shell(command.to_string())
    }
}

/// Whether a shortcut keeps going while its key is held, side by side with
/// any other such key: `move` and `resize`, so two arrows make a diagonal.
pub fn repeats(line: &str) -> bool {
    matches!(line.split_whitespace().next(), Some("move" | "resize"))
}

/// The side a `place` shortcut sends a window to, for the ones two of which
/// pressed together make a corner.
fn side(line: &str) -> Option<&str> {
    match line.split_whitespace().collect::<Vec<_>>()[..] {
        ["place", side @ ("left" | "right" | "top" | "bottom")] => Some(side),
        _ => None,
    }
}

/// The corner two sides make, if they meet at one.
fn corner(a: &str, b: &str) -> Option<&'static str> {
    let vertical = |s| matches!(s, "top" | "bottom");
    let (across, up_down) = match (vertical(a), vertical(b)) {
        (false, true) => (a, b),
        (true, false) => (b, a),
        _ => return None,
    };
    Some(match (up_down, across) {
        ("top", "left") => "top-left",
        ("top", _) => "top-right",
        (_, "left") => "bottom-left",
        _ => "bottom-right",
    })
}

/// How long a `place` shortcut to a side waits for a second one to make a
/// corner, in seconds: about how far apart two keys pressed together go down.
pub const CORNER_WAIT: f64 = 0.1;

/// Where a `place` shortcut to a side held down is at.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Wait {
    /// Since when it waits for a second side.
    Since(f64),
    /// It went to its side.
    Went,
    /// It made a corner with another.
    Corner,
}

/// Two `place` shortcuts to a side pressed together put the window in that
/// corner. So that two together go straight there, a side waits a moment
/// (`CORNER_WAIT`) for the other before it goes, or until its key comes up.
#[derive(Debug, Default)]
pub struct Corners {
    /// The key held for a side: its code, the shortcut, and where it is at.
    held: Option<(u16, String, Wait)>,
}

impl Corners {
    pub const fn new() -> Self {
        Self { held: None }
    }

    /// Whether this shortcut waits here instead of going at once.
    pub fn takes(line: &str) -> bool {
        side(line).is_some()
    }

    /// A key for `line` (one `takes` said yes to) went down at `now`.
    /// Returns what to run now: a corner, or nothing yet.
    pub fn press(&mut self, code: u16, line: &str, now: f64) -> Option<String> {
        let side_now = side(line)?;
        if let Some((held_code, held_line, wait)) = &mut self.held
            && *held_code != code
            && let Some(corner) = side(held_line).and_then(|held| corner(held, side_now))
        {
            *wait = Wait::Corner;
            return Some(format!("place {corner}"));
        }
        // Alone, or along the same line as the one held: this one counts.
        self.held = Some((code, line.to_string(), Wait::Since(now)));
        None
    }

    /// The side whose wait is over at `now`, to run.
    pub fn due(&mut self, now: f64) -> Option<String> {
        let (_, line, wait) = self.held.as_mut()?;
        match *wait {
            Wait::Since(since) if now - since >= CORNER_WAIT => {
                *wait = Wait::Went;
                Some(line.clone())
            }
            _ => None,
        }
    }

    /// A key came up. Returns what to run now: its side, if it still waited.
    pub fn release(&mut self, code: u16) -> Option<String> {
        let (_, line, wait) = self.held.take_if(|(c, _, _)| *c == code)?;
        matches!(wait, Wait::Since(_)).then_some(line)
    }
}

/// Repeats held shortcuts. macOS only repeats the key pressed last, and
/// stops when it is released, so two keys held together need a clock of
/// their own. Times are in seconds, from anywhere.
#[derive(Debug)]
pub struct Repeater {
    /// Seconds before a held key starts repeating, and between repeats.
    delay: f64,
    interval: f64,
    held: Vec<HeldKey>,
}

#[derive(Debug)]
struct HeldKey {
    code: u16,
    line: String,
    next: f64,
}

impl Repeater {
    pub const fn new(delay: f64, interval: f64) -> Self {
        Self {
            delay,
            interval,
            held: Vec::new(),
        }
    }

    pub fn set_timing(&mut self, delay: f64, interval: f64) {
        self.delay = delay;
        self.interval = interval;
    }

    /// Key `code` went down and ran `line`.
    pub fn press(&mut self, code: u16, line: &str, now: f64) {
        self.release(code);
        self.held.push(HeldKey {
            code,
            line: line.to_string(),
            next: now + self.delay,
        });
    }

    pub fn release(&mut self, code: u16) {
        self.held.retain(|k| k.code != code);
    }

    /// Lets go of every key: a modifier changed, so what they mean did too.
    pub fn clear(&mut self) {
        self.held.clear();
    }

    pub fn holds(&self, code: u16) -> bool {
        self.held.iter().any(|k| k.code == code)
    }

    /// The lines to run now. A late frame does not catch up: a window must
    /// not jump because the daemon was busy.
    pub fn due(&mut self, now: f64) -> Vec<String> {
        let interval = self.interval;
        self.held
            .iter_mut()
            .filter(|k| now >= k.next)
            .map(|k| {
                k.next = now + interval;
                k.line.clone()
            })
            .collect()
    }
}

/// A shortcut with its key code known.
#[derive(Clone, Debug)]
struct Compiled {
    code: u16,
    mods: Mods,
    passthrough: bool,
    command: String,
}

/// The shortcuts, ready to match key presses.
#[derive(Debug, Default)]
pub struct Hotkeys {
    by_code: HashMap<u16, Vec<Compiled>>,
    count: usize,
}

impl Hotkeys {
    /// Builds the shortcuts from `text`, one per line, using `layout` to
    /// find the key that types a character. Returns the problems found,
    /// such as a character no key types; those shortcuts are left out.
    pub fn build(text: &str, layout: &HashMap<char, u16>) -> (Self, Vec<String>) {
        let mut hotkeys = Self::default();
        let mut problems = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let binding = match Binding::parse(line) {
                Ok(b) => b,
                Err(e) => {
                    problems.push(format!("'{line}': {e}"));
                    continue;
                }
            };
            let code = match binding.key {
                Key::Code(c) => c,
                Key::Char(c) => match layout.get(&c) {
                    Some(&code) => code,
                    None => {
                        problems.push(format!("'{line}': no key types '{c}' on this layout"));
                        continue;
                    }
                },
            };
            hotkeys.by_code.entry(code).or_default().push(Compiled {
                code,
                mods: binding.mods,
                passthrough: binding.passthrough,
                command: binding.command,
            });
            hotkeys.count += 1;
        }
        (hotkeys, problems)
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// The shortcut for a press of key `code`: what to run, and whether the
    /// press is kept from the app. A later shortcut for the same keys wins.
    pub fn press(&self, code: u16, held: &Held) -> Option<(Action, bool)> {
        let ignore_fn = reports_fn(code);
        self.by_code
            .get(&code)?
            .iter()
            .rev()
            .find(|h| h.code == code && h.mods.matches(held, ignore_fn))
            .map(|h| (action(&h.command), !h.passthrough))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIGHT: u16 = 0x7C;
    const DOWN: u16 = 0x7D;
    const LEFT: u16 = 0x7B;

    #[test]
    fn one_side_goes_after_a_moment_or_when_its_key_comes_up() {
        let mut c = Corners::default();
        assert!(Corners::takes("place right"));
        assert!(!Corners::takes("place right 60%"));
        assert!(!Corners::takes("place max"));
        assert_eq!(c.press(RIGHT, "place right", 0.0), None, "not yet");
        assert_eq!(c.due(0.05), None);
        assert_eq!(c.due(CORNER_WAIT).as_deref(), Some("place right"));
        assert_eq!(c.due(1.0), None, "once");
        assert_eq!(c.release(RIGHT), None);
        // A quick tap goes when its key comes up.
        c.press(RIGHT, "place right", 2.0);
        assert_eq!(c.release(RIGHT).as_deref(), Some("place right"));
        assert_eq!(c.due(3.0), None);
    }

    #[test]
    fn two_sides_together_make_a_corner_and_nothing_else() {
        let mut c = Corners::default();
        assert_eq!(c.press(RIGHT, "place right", 0.0), None);
        assert_eq!(
            c.press(DOWN, "place bottom", 0.04).as_deref(),
            Some("place bottom-right")
        );
        assert_eq!(c.due(1.0), None);
        assert_eq!(c.release(DOWN), None);
        assert_eq!(c.release(RIGHT), None);
        assert_eq!(c.press(DOWN, "place bottom", 2.0), None);
        assert_eq!(
            c.press(LEFT, "place left", 2.03).as_deref(),
            Some("place bottom-left")
        );
        assert_eq!(c.release(LEFT), None);
        assert_eq!(c.release(DOWN), None);
        assert_eq!(corner("top", "right"), Some("top-right"));
        assert_eq!(corner("left", "top"), Some("top-left"));
    }

    #[test]
    fn a_late_second_side_still_makes_the_corner() {
        let mut c = Corners::default();
        c.press(RIGHT, "place right", 0.0);
        assert!(c.due(0.5).is_some());
        assert_eq!(
            c.press(DOWN, "place bottom", 0.6).as_deref(),
            Some("place bottom-right")
        );
    }

    #[test]
    fn sides_along_one_line_make_no_corner() {
        let mut c = Corners::default();
        c.press(RIGHT, "place right", 0.0);
        assert_eq!(
            c.press(LEFT, "place left", 0.02),
            None,
            "the later one counts"
        );
        assert_eq!(c.release(RIGHT), None);
        assert_eq!(c.release(LEFT).as_deref(), Some("place left"));
    }

    #[test]
    fn a_held_key_repeats_after_the_delay() {
        let mut r = Repeater::new(0.3, 0.1);
        r.press(1, "move left", 0.0);
        assert!(r.due(0.29).is_empty());
        assert_eq!(r.due(0.3), ["move left"]);
        assert!(r.due(0.35).is_empty());
        assert_eq!(r.due(0.4), ["move left"]);
    }

    #[test]
    fn two_held_keys_both_repeat() {
        let mut r = Repeater::new(0.3, 0.1);
        r.press(1, "move left", 0.0);
        r.press(2, "move down", 0.0);
        assert_eq!(r.due(0.3), ["move left", "move down"]);
        // The one pressed last going up does not stop the other.
        r.release(2);
        assert_eq!(r.due(0.4), ["move left"]);
    }

    #[test]
    fn a_late_frame_does_not_catch_up() {
        let mut r = Repeater::new(0.3, 0.1);
        r.press(1, "move left", 0.0);
        assert_eq!(r.due(2.0).len(), 1);
        assert!(r.due(2.05).is_empty());
    }

    #[test]
    fn a_modifier_changing_lets_go_of_everything() {
        let mut r = Repeater::new(0.3, 0.1);
        r.press(1, "move left", 0.0);
        assert!(r.holds(1));
        r.clear();
        assert!(!r.holds(1));
        assert!(r.due(1.0).is_empty());
    }

    #[test]
    fn pressing_again_does_not_double_up() {
        let mut r = Repeater::new(0.3, 0.1);
        r.press(1, "move left", 0.0);
        r.press(1, "move left", 0.1);
        assert!(r.due(0.3).is_empty());
        assert_eq!(r.due(0.4).len(), 1);
    }

    fn layout() -> HashMap<char, u16> {
        [
            ('h', 0x04),
            ('l', 0x25),
            ('1', 0x12),
            ('-', 0x1B),
            ('ş', 0x29),
        ]
        .into_iter()
        .collect()
    }

    fn held_alt_left() -> Held {
        Held {
            alt: (true, false),
            ..Held::default()
        }
    }

    #[test]
    fn parses_lines() {
        let b = Binding::parse("shift + alt - left : lily focus left").unwrap();
        assert_eq!(b.mods.shift, Side::Any);
        assert_eq!(b.mods.alt, Side::Any);
        assert_eq!(b.key, Key::Code(0x7B));
        assert_eq!(b.command, "lily focus left");
        assert!(!b.passthrough);

        let b = Binding::parse("hyper - 1 : echo one").unwrap();
        assert_eq!(
            (b.mods.cmd, b.mods.alt, b.mods.shift, b.mods.ctrl),
            (Side::Any, Side::Any, Side::Any, Side::Any)
        );
        let b = Binding::parse("lalt - 0x32 -> : echo grave").unwrap();
        assert_eq!(
            (b.mods.alt, b.key.clone(), b.passthrough),
            (Side::Left, Key::Code(0x32), true)
        );
        let b = Binding::parse("f5 : echo five").unwrap();
        assert_eq!((b.mods, b.key), (Mods::default(), Key::Code(0x60)));
        let b = Binding::parse("alt - - : echo minus").unwrap();
        assert_eq!(b.key, Key::Char('-'));
        let b = Binding::parse("cmd - H : x").unwrap();
        assert_eq!(b.key, Key::Char('h'));
        let b = Binding::parse("alt - return : open -na Terminal").unwrap();
        assert_eq!(b.command, "open -na Terminal");
    }

    #[test]
    fn rejects_bad_lines() {
        assert!(Binding::parse("alt - h").is_err());
        assert!(Binding::parse("alt - h :").is_err());
        assert!(
            Binding::parse("super - h : x")
                .unwrap_err()
                .contains("super")
        );
        assert!(
            Binding::parse("alt - nokey : x")
                .unwrap_err()
                .contains("nokey")
        );
        assert!(Binding::parse("- h : x").is_err());
        assert!(
            check("alt - h : a\nalt - : b")
                .unwrap_err()
                .contains("alt - : b")
        );
        assert_eq!(check("alt - h : a\n\nalt - l : b\n"), Ok(2));
    }

    #[test]
    fn matches_presses() {
        let (hk, problems) = Hotkeys::build(
            "alt - h : lily focus left\n\
             ralt - l : lily focus right\n\
             alt - left -> : echo through\n\
             alt - ş : echo turkish\n",
            &layout(),
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(hk.count(), 4);

        // Either alt key takes a plain `alt`.
        assert_eq!(
            hk.press(0x04, &held_alt_left()),
            Some((Action::Lily("focus left".into()), true))
        );
        let right = Held {
            alt: (false, true),
            ..Held::default()
        };
        assert!(hk.press(0x04, &right).is_some());
        // Extra modifiers or none: no match.
        let with_shift = Held {
            shift: (true, false),
            ..held_alt_left()
        };
        assert_eq!(hk.press(0x04, &with_shift), None);
        assert_eq!(hk.press(0x04, &Held::default()), None);
        // `ralt` wants the right one only.
        assert_eq!(hk.press(0x25, &held_alt_left()), None);
        assert!(hk.press(0x25, &right).is_some());
        // Arrows come with fn set; passthrough lets the key through.
        let arrow = Held {
            fun: true,
            ..held_alt_left()
        };
        assert_eq!(
            hk.press(0x7B, &arrow),
            Some((Action::Shell("echo through".into()), false))
        );
        assert!(hk.press(0x29, &held_alt_left()).is_some());
    }

    #[test]
    fn later_shortcut_wins_and_missing_keys_are_reported() {
        let (hk, problems) = Hotkeys::build(
            "alt - h : first\nalt - h : second\nalt - ğ : nowhere\n",
            &layout(),
        );
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("'ğ'"));
        assert_eq!(
            hk.press(0x04, &held_alt_left()),
            Some((Action::Shell("second".into()), true))
        );
    }

    #[test]
    fn lily_commands_run_inside() {
        assert_eq!(action("lily focus left"), Action::Lily("focus left".into()));
        assert_eq!(
            action("~/bin/lily focus up"),
            Action::Shell("~/bin/lily focus up".into())
        );
        assert_eq!(
            action("/Users/me/bin/lily  space 2"),
            Action::Lily("space 2".into())
        );
        assert_eq!(action("lily restart"), Action::Shell("lily restart".into()));
        assert_eq!(
            action("lily focus left && echo hi"),
            Action::Shell("lily focus left && echo hi".into())
        );
        assert_eq!(
            action("open -a Safari"),
            Action::Shell("open -a Safari".into())
        );
    }
}
