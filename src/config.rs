//! Settings. There is no config file format: the config is a shell script
//! (`lilyrc`) that lily runs at startup, made of `lily config`, `lily
//! rule` and `lily bind` commands. The same commands work at runtime.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::command::SpaceTarget;
use crate::platform::{Modifier, MouseButton};

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Space windows keep from the edges of the display and from the window
    /// they snap to, in points.
    pub gap: i32,
    /// Points `move` and `resize` go by default.
    pub step: i32,
    /// How long a window takes to reach its place, in seconds: `ANIMATION`
    /// with `animation on`, `0` with `animation off`.
    pub animation_duration: f64,
    /// Held with a click anywhere on a window, moves or resizes it.
    pub mouse_modifier: Option<Modifier>,
    /// What dragging with the left button (with the modifier) does.
    pub mouse_action1: MouseAction,
    /// What dragging with the right button (with the modifier) does.
    pub mouse_action2: MouseAction,
    /// Draw a ring around the focused window.
    pub border: bool,
    /// The ring's colour, RGBA 0..=1.
    pub border_color: [f32; 4],
    /// The ring's thickness, in points.
    pub border_width: i32,
    /// The roundness of window corners the ring follows, in points.
    pub border_radius: i32,
    /// Apps whose windows have corners of their own: (app, radius).
    pub border_radius_rules: Vec<(String, i32)>,
    /// The window under the pointer gets the focus (and comes to the front:
    /// macOS has no focus without raising, short of disabling SIP).
    pub focus_follows_mouse: bool,
    /// Focusing a window from the keyboard puts the pointer in its middle.
    pub mouse_follows_focus: bool,
    /// Tell SketchyBar when the Space or the focus changes.
    pub sketchybar: bool,
    /// Space names, by Space number (from 1, as in Mission Control).
    pub labels: BTreeMap<usize, String>,
    /// Apps whose new windows go to a Space: (app, Space, go along).
    pub space_rules: Vec<(String, SpaceTarget, bool)>,
    /// Apps whose new windows go to a grid cell.
    pub grid_rules: Vec<(String, [u32; 6])>,
    /// Scratchpads: name, app, and the grid cell it floats at.
    pub scratchpads: Vec<(String, String, [u32; 6])>,
    /// How much to log; `None` leaves it as lily was started with.
    pub log_level: Option<log::LevelFilter>,
    /// Keyboard shortcut lines (`lily bind`).
    pub hotkeys: Vec<String>,
}

/// What a modifier drag does to the window under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Move,
    Resize,
}

impl MouseAction {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "move" => Some(Self::Move),
            "resize" => Some(Self::Resize),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Resize => "resize",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gap: 8,
            step: 40,
            animation_duration: ANIMATION,
            mouse_modifier: Some(Modifier::Alt),
            mouse_action1: MouseAction::Move,
            mouse_action2: MouseAction::Resize,
            border: false,
            border_color: [0.541, 0.663, 0.973, 1.0],
            border_width: 2,
            border_radius: 12,
            border_radius_rules: Vec::new(),
            focus_follows_mouse: false,
            mouse_follows_focus: false,
            sketchybar: false,
            labels: BTreeMap::new(),
            space_rules: Vec::new(),
            grid_rules: Vec::new(),
            scratchpads: Vec::new(),
            log_level: None,
            hotkeys: Vec::new(),
        }
    }
}

impl Config {
    /// What dragging with `button` and the modifier does.
    pub fn mouse_action(&self, button: MouseButton) -> MouseAction {
        match button {
            MouseButton::Left => self.mouse_action1,
            MouseButton::Right => self.mouse_action2,
        }
    }
}

/// How long a window takes to slide into place with `animation on`, in
/// seconds: as long as macOS's own (Core Animation's default).
pub const ANIMATION: f64 = 0.25;

/// Names accepted by `lily config`, for help and error messages.
pub const KEYS: &[&str] = &[
    "gap",
    "step",
    "animation",
    "mouse_modifier",
    "mouse_action1",
    "mouse_action2",
    "border",
    "border_color",
    "border_width",
    "border_radius",
    "focus_follows_mouse",
    "mouse_follows_focus",
    "sketchybar",
    "log_level",
];

/// Whether a rule's app pattern matches a window's app name.
///
/// A pattern between slashes, such as `/Webex/`, is a regular expression
/// searched for anywhere in the name (anchor it with `^` and `$`, ignore case
/// with `(?i)`). Any other pattern must equal the name. Names are compared
/// without invisible formatting characters and surrounding whitespace: some
/// apps put such characters in their names (WhatsApp reports itself as
/// "\u{200e}WhatsApp"), and the name typed in `lilyrc` cannot contain them.
pub fn app_matches(rule: &str, app: &str) -> bool {
    let app = clean_app_name(app);
    match app_regex(rule) {
        Some(Ok(re)) => re.is_match(&app),
        Some(Err(_)) => false,
        None => clean_app_name(rule) == app,
    }
}

/// Whether two rules name the same app pattern, so the newer replaces the
/// older. Patterns are compared as written: `WhatsApp` does not replace
/// `/Whats/` even though the regex would match it.
pub fn same_rule(a: &str, b: &str) -> bool {
    clean_app_name(a) == clean_app_name(b)
}

/// Checks a rule's app pattern before it is stored: a `/regex/` must be a
/// valid, non-empty regular expression.
pub fn check_app_pattern(rule: &str) -> Result<(), String> {
    match app_regex(rule) {
        Some(Err(e)) => Err(e),
        _ => Ok(()),
    }
}

/// The regular expression of a `/pattern/` rule, compiled once and cached;
/// `None` for a plain app name.
fn app_regex(rule: &str) -> Option<Result<regex::Regex, String>> {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CACHE: RefCell<HashMap<String, Result<regex::Regex, String>>> =
            RefCell::new(HashMap::new());
    }
    let rule = clean_app_name(rule);
    let source = rule.strip_prefix('/')?.strip_suffix('/')?;
    Some(CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry(rule.clone())
            .or_insert_with(|| {
                if source.is_empty() {
                    return Err(format!("empty regular expression {rule}"));
                }
                regex::Regex::new(source).map_err(|e| format!("bad regular expression {rule}: {e}"))
            })
            .clone()
    }))
}

/// An app name without invisible formatting characters (direction marks,
/// zero-width spaces and joiners, byte order marks, soft hyphens) and
/// without surrounding whitespace.
pub fn clean_app_name(name: &str) -> String {
    let visible: String = name.chars().filter(|&c| !is_invisible(c)).collect();
    visible.trim().to_string()
}

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

fn number<T: std::str::FromStr>(key: &str, value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{key}: '{value}' is not a valid number"))
}

/// A colour as `RRGGBB` or `RRGGBBAA` (a `#` or `0x` in front is fine);
/// opaque unless it says otherwise.
fn parse_color(value: &str) -> Option<[f32; 4]> {
    let hex = value
        .strip_prefix('#')
        .or_else(|| value.strip_prefix("0x"))
        .unwrap_or(value);
    if !(hex.len() == 6 || hex.len() == 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| {
        u8::from_str_radix(&hex[i..i + 2], 16)
            .ok()
            .map(|b| f32::from(b) / 255.0)
    };
    let alpha = if hex.len() == 8 { byte(6)? } else { 1.0 };
    Some([byte(0)?, byte(2)?, byte(4)?, alpha])
}

fn format_color(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        b(c[0]),
        b(c[1]),
        b(c[2]),
        b(c[3])
    )
}

fn on_off(key: &str, value: &str) -> Result<bool, String> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        v => Err(format!("{key}: expected on or off, got '{v}'")),
    }
}

fn on_off_name(v: bool) -> String {
    if v { "on" } else { "off" }.to_string()
}

fn one<'a>(key: &str, args: &[&'a str]) -> Result<&'a str, String> {
    match args {
        [v] => Ok(v),
        [] => Err(format!("{key}: missing value")),
        _ => Err(format!("{key}: expected one value")),
    }
}

impl Config {
    /// Applies `lily config <key> <args...>`.
    pub fn set(&mut self, key: &str, args: &[&str]) -> Result<(), String> {
        match key {
            "gap" | "step" => {
                let v: i32 = number(key, one(key, args)?)?;
                if v < 0 || (key == "step" && v == 0) {
                    return Err(format!("{key}: must be positive"));
                }
                if key == "gap" {
                    self.gap = v;
                } else {
                    self.step = v;
                }
            }
            "animation" => {
                let on = on_off(key, one(key, args)?)?;
                self.animation_duration = if on { ANIMATION } else { 0.0 };
            }
            "mouse_modifier" => {
                let v = one(key, args)?;
                self.mouse_modifier = match v {
                    "off" | "none" => None,
                    _ => Some(
                        Modifier::parse(v)
                            .ok_or_else(|| format!("{key}: unknown modifier '{v}'"))?,
                    ),
                };
            }
            "mouse_action1" | "mouse_action2" => {
                let v = one(key, args)?;
                let action = MouseAction::parse(v)
                    .ok_or_else(|| format!("{key}: expected move or resize, got '{v}'"))?;
                if key == "mouse_action1" {
                    self.mouse_action1 = action;
                } else {
                    self.mouse_action2 = action;
                }
            }
            "border" => self.border = on_off(key, one(key, args)?)?,
            "border_color" => {
                let v = one(key, args)?;
                self.border_color = parse_color(v).ok_or_else(|| {
                    format!("{key}: '{v}' is not a colour like 8aa9f8 or 8aa9f8c0")
                })?;
            }
            "focus_follows_mouse" => self.focus_follows_mouse = on_off(key, one(key, args)?)?,
            "mouse_follows_focus" => self.mouse_follows_focus = on_off(key, one(key, args)?)?,
            "sketchybar" => self.sketchybar = on_off(key, one(key, args)?)?,
            "border_width" => {
                let v: i32 = number(key, one(key, args)?)?;
                if !(1..=20).contains(&v) {
                    return Err(format!("{key}: must be between 1 and 20"));
                }
                self.border_width = v;
            }
            "border_radius" => {
                let v: i32 = number(key, one(key, args)?)?;
                if !(0..=40).contains(&v) {
                    return Err(format!("{key}: must be between 0 and 40"));
                }
                self.border_radius = v;
            }
            "log_level" => {
                let v = one(key, args)?;
                self.log_level = Some(
                    v.parse()
                        .map_err(|_| format!("{key}: unknown level '{v}'"))?,
                );
            }
            _ => {
                return Err(format!(
                    "unknown setting '{key}'; settings: {}",
                    KEYS.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// The value of a setting, as `lily config <key>` prints it.
    pub fn get(&self, key: &str) -> Result<String, String> {
        Ok(match key {
            "gap" => self.gap.to_string(),
            "step" => self.step.to_string(),
            "animation" => on_off_name(self.animation_duration > 0.0),
            "mouse_modifier" => self
                .mouse_modifier
                .map_or("off", Modifier::name)
                .to_string(),
            "mouse_action1" => self.mouse_action1.name().to_string(),
            "mouse_action2" => self.mouse_action2.name().to_string(),
            "border" => on_off_name(self.border),
            "border_color" => format_color(self.border_color),
            "border_width" => self.border_width.to_string(),
            "border_radius" => self.border_radius.to_string(),
            "focus_follows_mouse" => on_off_name(self.focus_follows_mouse),
            "mouse_follows_focus" => on_off_name(self.mouse_follows_focus),
            "sketchybar" => on_off_name(self.sketchybar),
            "log_level" => self
                .log_level
                .map_or("default".into(), |l| l.as_str().to_lowercase()),
            _ => {
                return Err(format!(
                    "unknown setting '{key}'; settings: {}",
                    KEYS.join(", ")
                ));
            }
        })
    }

    /// `lily label <n> <name>`: a name is only ever on one Space.
    pub fn set_label(&mut self, n: usize, name: &str) {
        self.labels.retain(|_, l| l != name);
        self.labels.insert(n, name.to_string());
    }

    /// The Space number a target names, if it names one by itself (a number
    /// or a known label).
    pub fn space_number(&self, target: &SpaceTarget) -> Option<usize> {
        match target {
            SpaceTarget::Number(n) => Some(*n),
            SpaceTarget::Label(name) => self
                .labels
                .iter()
                .find(|(_, l)| *l == name)
                .map(|(n, _)| *n),
            _ => None,
        }
    }

    /// `lily rule space <space> <app> [--follow]`.
    pub fn set_space_rule(&mut self, app: &str, space: SpaceTarget, follow: bool) {
        self.space_rules.retain(|(a, _, _)| !same_rule(a, app));
        self.space_rules.push((app.to_string(), space, follow));
    }

    /// The Space rule for an app's new windows, the latest if several match.
    pub fn space_rule_for(&self, app: &str) -> Option<(&SpaceTarget, bool)> {
        self.space_rules
            .iter()
            .rev()
            .find(|(a, _, _)| app_matches(a, app))
            .map(|(_, s, f)| (s, *f))
    }

    /// `lily rule grid <cell> <app>`.
    pub fn set_grid_rule(&mut self, app: &str, grid: [u32; 6]) {
        self.grid_rules.retain(|(a, _)| !same_rule(a, app));
        self.grid_rules.push((app.to_string(), grid));
    }

    /// The grid cell for an app's new windows, the latest if several match.
    pub fn grid_rule_for(&self, app: &str) -> Option<[u32; 6]> {
        self.grid_rules
            .iter()
            .rev()
            .find(|(a, _)| app_matches(a, app))
            .map(|(_, g)| *g)
    }

    /// `lily scratchpad <name> <app> [cell]`.
    pub fn set_scratchpad(&mut self, name: &str, app: &str, grid: [u32; 6]) {
        self.scratchpads.retain(|(n, _, _)| n != name);
        self.scratchpads
            .push((name.to_string(), app.to_string(), grid));
    }

    /// `lily rule border_radius <radius> <app>`.
    pub fn set_border_radius_rule(&mut self, app: &str, radius: i32) {
        self.border_radius_rules.retain(|(a, _)| !same_rule(a, app));
        self.border_radius_rules.push((app.to_string(), radius));
    }

    /// The roundness of the ring around a window of `app`.
    pub fn border_radius_for(&self, app: &str) -> i32 {
        self.border_radius_rules
            .iter()
            .rev()
            .find(|(a, _)| app_matches(a, app))
            .map_or(self.border_radius, |(_, radius)| *radius)
    }

    /// Where to look for the config script, in priority order.
    pub fn rc_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(p) = std::env::var_os("LILY_CONFIG") {
            paths.push(PathBuf::from(p));
        }
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            paths.push(PathBuf::from(xdg).join("lilypad/lilyrc"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            paths.push(home.join(".config/lilypad/lilyrc"));
            paths.push(home.join(".lilyrc"));
        }
        paths
    }

    /// The config script to run, if there is one.
    pub fn find_rc() -> Option<PathBuf> {
        Self::rc_paths().into_iter().find(|p| p.is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get() {
        let mut c = Config::default();
        c.set("gap", &["12"]).unwrap();
        assert_eq!(c.get("gap").unwrap(), "12");
        c.set("step", &["25"]).unwrap();
        assert_eq!(c.step, 25);
        assert!(c.set("step", &["0"]).is_err());

        assert_eq!(c.get("animation").unwrap(), "on");
        assert_eq!(c.animation_duration, ANIMATION);
        c.set("animation", &["off"]).unwrap();
        assert_eq!(c.animation_duration, 0.0);
        assert_eq!(c.get("animation").unwrap(), "off");
        c.set("animation", &["on"]).unwrap();
        assert_eq!(c.animation_duration, 0.25);
        assert!(c.set("animation", &["0.25"]).is_err(), "no durations");
        assert!(c.set("size_interval", &["30"]).is_err(), "fixed now");
        assert!(c.set("animation_duration", &["0.25"]).is_err());
        assert!(c.set("animation_easing", &["spring"]).is_err());

        assert!(c.set("padding", &["8"]).is_err());

        c.set("mouse_modifier", &["cmd"]).unwrap();
        assert_eq!(c.get("mouse_modifier").unwrap(), "cmd");
        c.set("mouse_modifier", &["off"]).unwrap();
        assert_eq!(c.mouse_modifier, None);
        assert!(c.set("mouse_modifier", &["hyper"]).is_err());

        c.set("mouse_action1", &["resize"]).unwrap();
        assert_eq!(c.mouse_action(MouseButton::Left), MouseAction::Resize);
        assert!(c.set("mouse_action2", &["swap"]).is_err());

        assert!(c.set("gesture_support", &["on"]).is_err(), "gone");

        assert_eq!(c.get("border").unwrap(), "off");
        c.set("border", &["on"]).unwrap();
        assert!(c.border);
        assert!(c.set("border", &["yes"]).is_err());
        c.set("border_width", &["3"]).unwrap();
        assert_eq!(c.border_width, 3);
        assert!(c.set("border_width", &["0"]).is_err());
        c.set("border_radius", &["16"]).unwrap();
        assert_eq!(c.get("border_radius").unwrap(), "16");
        assert!(c.set("border_radius", &["41"]).is_err());

        assert!(c.set("resize_mode", &["axis"]).is_err(), "gone");

        assert!(c.set("nope", &["1"]).is_err());
        assert!(c.get("nope").is_err());
    }

    #[test]
    fn a_border_colour_is_opaque_unless_it_says_otherwise() {
        let mut c = Config::default();
        c.set("border_color", &["90a8f2"]).unwrap();
        assert_eq!(c.get("border_color").unwrap(), "90a8f2ff");
        c.set("border_color", &["#ffffff80"]).unwrap();
        assert!((c.border_color[3] - 0.502).abs() < 0.01);
        assert!(c.set("border_color", &["blue"]).is_err());
        assert!(c.set("border_color", &["fff"]).is_err());
    }

    #[test]
    fn labels_name_one_space_each() {
        let mut c = Config::default();
        c.set_label(2, "web");
        c.set_label(3, "web");
        assert_eq!(c.space_number(&SpaceTarget::Label("web".into())), Some(3));
        assert_eq!(c.labels.len(), 1);
        assert_eq!(c.space_number(&SpaceTarget::Label("nope".into())), None);
        assert_eq!(c.space_number(&SpaceTarget::Number(4)), Some(4));
        assert_eq!(c.space_number(&SpaceTarget::Next), None);
    }

    #[test]
    fn named_rules_replace_their_own_and_match_loosely() {
        let mut c = Config::default();
        c.set_space_rule("Safari", SpaceTarget::Number(2), false);
        c.set_space_rule("Safari", SpaceTarget::Label("web".into()), true);
        assert_eq!(c.space_rules.len(), 1);
        assert_eq!(
            c.space_rule_for("Safari"),
            Some((&SpaceTarget::Label("web".into()), true))
        );
        c.set_grid_rule("/^Ze/", [1, 2, 0, 0, 1, 1]);
        assert_eq!(c.grid_rule_for("Zed"), Some([1, 2, 0, 0, 1, 1]));
        assert_eq!(c.grid_rule_for("Mail"), None);
        c.set_border_radius_rule("Zed", 0);
        assert_eq!(c.border_radius_for("Zed"), 0);
        assert_eq!(c.border_radius_for("Mail"), 12);
        c.set_scratchpad("term", "kitty", [6, 6, 1, 1, 4, 4]);
        c.set_scratchpad("term", "Terminal", [6, 6, 1, 1, 4, 4]);
        assert_eq!(c.scratchpads.len(), 1);
    }

    #[test]
    fn the_new_switches_turn_on_and_off() {
        let mut c = Config::default();
        for key in ["focus_follows_mouse", "mouse_follows_focus", "sketchybar"] {
            assert_eq!(c.get(key).unwrap(), "off");
            c.set(key, &["on"]).unwrap();
            assert_eq!(c.get(key).unwrap(), "on");
        }
    }

    #[test]
    fn rules_match_app_names_loosely() {
        let mut c = Config::default();
        c.set_grid_rule("/^Web/", [1, 1, 0, 0, 1, 1]);
        c.set_grid_rule("WhatsApp", [1, 2, 0, 0, 1, 1]);
        assert!(c.grid_rule_for("Webex").is_some());
        assert_eq!(
            c.grid_rule_for("\u{200e}WhatsApp"),
            Some([1, 2, 0, 0, 1, 1])
        );
        assert!(check_app_pattern("/[/").is_err());
    }
}
