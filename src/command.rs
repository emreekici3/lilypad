//! Commands accepted over the socket (`lily <command>`).

use crate::config::check_app_pattern;
use crate::geometry::Direction;
use crate::place::Position;

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Move the focused window; the distance defaults to `step`.
    Move(Direction, Option<i32>),
    /// Grow (right, down) or shrink (left, up) the focused window; the top-left
    /// corner stays. The distance defaults to `step`.
    Resize(Direction, Option<i32>),
    /// Focus the window that way.
    Focus(Direction),
    /// Close the focused window.
    Close,
    /// Put the focused window somewhere on its display. Done with `max`
    /// again, puts it back where it was.
    Place(Position, Option<u32>),
    /// Slide the focused window to the next window or the display's edge.
    Snap(Direction),
    /// Trade places (and sizes) with the window that way.
    /// Put the focused window at a cell of a grid over its display:
    /// `[rows, cols, x, y, w, h]`.
    Grid([u32; 6]),
    /// Show a Space of the current display: a number from 1, `next`,
    /// `prev` or `recent`.
    Space(SpaceTarget),
    /// Move the focused window to a Space and go there with it.
    Send(SpaceTarget),
    /// `label` lists the Spaces, `label <n> <name>` names one.
    Label(Option<(usize, String)>),
    /// New windows of `app` go to `space`; with `follow`, so does the user.
    SpaceRule {
        space: SpaceTarget,
        app: String,
        follow: bool,
    },
    /// New windows of `app` go to a grid cell.
    GridRule { grid: [u32; 6], app: String },
    /// The ring round windows of `app` has corners this round.
    BorderRadiusRule { radius: i32, app: String },
    /// An app at hand on every Space, floating at a grid cell.
    Scratchpad {
        name: String,
        app: String,
        grid: [u32; 6],
    },
    /// Bring a scratchpad here, or put it away.
    ScratchpadToggle(String),
    /// What the running window manager knows, for `lily doctor`.
    Status,
    /// `config <key>` prints a setting, `config <key> <value...>` changes it.
    Config { key: String, args: Vec<String> },
    /// Add a keyboard shortcut line; `None` lists them.
    Bind(Option<String>),
    /// List the windows.
    Query,
    /// Re-run the config script from a clean slate.
    Reload,
    /// Stop the window manager.
    Quit,
}

/// A Space of the current display, as `space` and `send` name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpaceTarget {
    /// Numbered from 1, as in Mission Control.
    Number(usize),
    /// A name given with `label`.
    Label(String),
    Next,
    Prev,
    /// The one shown before this one.
    Recent,
}

impl SpaceTarget {
    pub fn parse(word: &str) -> Result<Self, String> {
        match word {
            "next" => Ok(Self::Next),
            "prev" => Ok(Self::Prev),
            "recent" => Ok(Self::Recent),
            n if n.starts_with(|c: char| c.is_ascii_digit()) => match n.parse() {
                Ok(n) if n >= 1 => Ok(Self::Number(n)),
                _ => Err(format!(
                    "unknown Space '{word}'; give a number from 1, a label, next, prev or \
                     recent"
                )),
            },
            label => Ok(Self::Label(label.to_string())),
        }
    }
}

pub const USAGE: &str = "\
commands:
  move <left|right|up|down> [points]    default: the step setting
  resize <left|right|up|down> [points]  right and down grow, left and up
                                        shrink; default: the step setting
  focus <left|right|up|down>
  snap <left|right|up|down>             slide to the next window or the edge
  place <position> [percent%]           e.g. place left 60%, place center
                                        positions: center, left, right, top,
                                        bottom, top-left, top-right,
                                        bottom-left, bottom-right, max
                                        (max again puts the window back)
  close
  grid <rows:cols:x:y:w:h>              e.g. grid 2:2:1:0:1:2 is the right half
  space <n|label|next|prev|recent>      show that Space of the display
  send <n|label|next|prev|recent>       move the focused window there and
                                        go with it
  label [<n> <name>]                    name a Space; alone, list them
  scratchpad toggle <name>              bring a scratchpad here, or away
  query | reload | quit

settings (put these in ~/.config/lilypad/lilyrc):
  config <key> [value]       e.g. config animation off, config step 20
                             keys: gap, step, animation,
                             mouse_modifier, mouse_action1, mouse_action2,
                             focus_follows_mouse, mouse_follows_focus,
                             border, border_color, border_width,
                             border_radius, sketchybar, log_level
  rule grid <rows:cols:x:y:w:h> <app>
                             where an app's new windows go;
                             /regex/ matches apps by pattern
  rule space <n|label> <app> [--follow]
  rule border_radius <radius> <app>
  scratchpad <name> <app> [rows:cols:x:y:w:h]
  bind ['<hotkey>']          a keyboard shortcut, e.g.
                             bind 'alt - h : lily focus left'; alone, list them";

/// `60%` as 60.
fn percent(word: &str) -> Option<Result<u32, String>> {
    let digits = word.strip_suffix('%')?;
    Some(match digits.parse() {
        Ok(p) if (1..=100).contains(&p) => Ok(p),
        _ => Err(format!("bad percentage '{word}'; give 1% to 100%")),
    })
}

impl Command {
    /// Whether this only moves or resizes a window, so the window list
    /// need not be looked at again. Doing that stalls the main loop for a
    /// moment, which is more than a stream of these can afford.
    pub fn is_motion(&self) -> bool {
        matches!(self, Self::Move(..) | Self::Resize(..))
    }

    pub fn parse(line: &str) -> Result<Self, String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let dir = || -> Result<Direction, String> {
            let w = words.get(1).ok_or("missing direction")?;
            Direction::parse(w).ok_or_else(|| format!("unknown direction '{w}'"))
        };
        let points = || -> Result<Option<i32>, String> {
            match words.get(2) {
                None => Ok(None),
                Some(p) if words.len() == 3 => p
                    .parse()
                    .map(Some)
                    .map_err(|_| format!("bad distance '{p}'")),
                Some(_) => Err(format!("too many arguments for '{}'", words[0])),
            }
        };
        let only = |n: usize, cmd: Command| {
            if words.len() == n {
                Ok(cmd)
            } else {
                Err(format!("wrong number of arguments for '{}'", words[0]))
            }
        };
        match words.first().copied() {
            None => Err("empty command".into()),
            Some("move") => Ok(Command::Move(dir()?, points()?)),
            Some("resize") => match points()? {
                Some(p) if p <= 0 => Err("resize: give a distance above 0".into()),
                p => Ok(Command::Resize(dir()?, p)),
            },
            Some("focus") => only(2, Command::Focus(dir()?)),
            Some("snap") => only(2, Command::Snap(dir()?)),
            Some("grid") => {
                let [_, spec] = words[..] else {
                    return Err("usage: grid <rows:cols:x:y:w:h>".into());
                };
                Ok(Command::Grid(parse_grid(spec)?))
            }
            Some("status") => only(1, Command::Status),
            Some("label") => match words[..] {
                [_] => Ok(Command::Label(None)),
                [_, n, name] => {
                    let n: usize = n
                        .parse()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| format!("label: '{n}' is not a Space number"))?;
                    match SpaceTarget::parse(name)? {
                        SpaceTarget::Label(_) => Ok(Command::Label(Some((n, name.to_string())))),
                        _ => Err(format!("label: '{name}' cannot be a label")),
                    }
                }
                _ => Err("usage: label <n> <name>, e.g. label 2 web".into()),
            },
            Some("scratchpad") if words.get(1) == Some(&"toggle") => {
                let [_, _, name] = words[..] else {
                    return Err("usage: scratchpad toggle <name>".into());
                };
                Ok(Command::ScratchpadToggle(name.to_string()))
            }
            Some("scratchpad") => {
                const USE: &str = "usage: scratchpad <name> <app> [rows:cols:x:y:w:h]";
                let name = words.get(1).ok_or(USE)?;
                let last = words.last().filter(|w| w.contains(':'));
                let grid = last.map_or(Ok(SCRATCHPAD_GRID), |g| parse_grid(g))?;
                let end = words.len() - usize::from(last.is_some());
                if end < 3 {
                    return Err(USE.into());
                }
                let app = words[2..end].join(" ");
                check_app_pattern(&app)?;
                if app.starts_with('/') {
                    return Err("scratchpad: the app must be a name, not a pattern".into());
                }
                Ok(Command::Scratchpad {
                    name: (*name).to_string(),
                    app,
                    grid,
                })
            }
            Some("close") => only(1, Command::Close),
            Some("query") => only(1, Command::Query),
            Some("reload") => only(1, Command::Reload),
            Some("quit") => only(1, Command::Quit),
            Some("place") => {
                let w = words.get(1).ok_or("missing position")?;
                let position =
                    Position::parse(w).ok_or_else(|| format!("unknown position '{w}'"))?;
                let share = match words.get(2) {
                    None => None,
                    Some(p) if words.len() == 3 => {
                        Some(percent(p).ok_or_else(|| format!("bad percentage '{p}'"))??)
                    }
                    Some(_) => return Err("too many arguments for 'place'".into()),
                };
                Ok(Command::Place(position, share))
            }
            Some(what @ ("space" | "send")) => {
                let [_, target] = words[..] else {
                    return Err(format!("usage: {what} <n|label|next|prev|recent>"));
                };
                let target = SpaceTarget::parse(target)?;
                Ok(if what == "space" {
                    Command::Space(target)
                } else {
                    Command::Send(target)
                })
            }
            Some("rule") => Self::parse_rule(&words[1..]),
            Some("config") => {
                let key = words.get(1).ok_or("missing setting")?;
                Ok(Command::Config {
                    key: (*key).to_string(),
                    args: words[2..].iter().map(ToString::to_string).collect(),
                })
            }
            Some("bind") => {
                let rest = line.trim_start().strip_prefix("bind").unwrap_or("").trim();
                if rest.is_empty() {
                    return Ok(Command::Bind(None));
                }
                crate::hotkeys::check(rest)?;
                Ok(Command::Bind(Some(rest.to_string())))
            }
            Some(other) => Err(format!("unknown command '{other}'")),
        }
    }

    /// `rule grid`, `rule space` or `rule border_radius`.
    fn parse_rule(words: &[&str]) -> Result<Self, String> {
        match words.first().copied() {
            Some("grid") => {
                const USE: &str = "usage: rule grid <rows:cols:x:y:w:h> <app>";
                let [_, spec, app @ ..] = words else {
                    return Err(USE.into());
                };
                if app.is_empty() {
                    return Err(USE.into());
                }
                let app = app.join(" ");
                check_app_pattern(&app)?;
                Ok(Command::GridRule {
                    grid: parse_grid(spec)?,
                    app,
                })
            }
            Some("space") => {
                const USE: &str = "usage: rule space <n|label> <app> [--follow]";
                let [_, space, rest @ ..] = words else {
                    return Err(USE.into());
                };
                let follow = rest.last() == Some(&"--follow");
                let app = &rest[..rest.len() - usize::from(follow)];
                if app.is_empty() {
                    return Err(USE.into());
                }
                let space = match SpaceTarget::parse(space)? {
                    t @ (SpaceTarget::Number(_) | SpaceTarget::Label(_)) => t,
                    _ => return Err("rule space: give a Space number or label".into()),
                };
                let app = app.join(" ");
                check_app_pattern(&app)?;
                Ok(Command::SpaceRule { space, app, follow })
            }
            Some("border_radius") => {
                const USE: &str = "usage: rule border_radius <radius> <app>";
                let [_, radius, app @ ..] = words else {
                    return Err(USE.into());
                };
                let radius = radius
                    .parse()
                    .ok()
                    .filter(|r| (0..=40).contains(r))
                    .ok_or("rule border_radius: must be between 0 and 40")?;
                if app.is_empty() {
                    return Err(USE.into());
                }
                let app = app.join(" ");
                check_app_pattern(&app)?;
                Ok(Command::BorderRadiusRule { radius, app })
            }
            _ => Err(
                "usage: rule grid <rows:cols:x:y:w:h> <app>, rule space <n|label> <app> \
                 [--follow] or rule border_radius <radius> <app>"
                    .into(),
            ),
        }
    }
}

/// Where a scratchpad floats unless it says: the middle two thirds.
const SCRATCHPAD_GRID: [u32; 6] = [6, 6, 1, 1, 4, 4];

/// `rows:cols:x:y:w:h`, a cell `w` x `h` cells large, from column `x` and row
/// `y` (both from 0).
pub fn parse_grid(spec: &str) -> Result<[u32; 6], String> {
    let bad = || format!("grid: expected rows:cols:x:y:w:h, got '{spec}'");
    let nums = spec
        .split(':')
        .map(|n| n.parse::<u32>().map_err(|_| bad()))
        .collect::<Result<Vec<_>, _>>()?;
    let grid: [u32; 6] = nums.try_into().map_err(|_| bad())?;
    let [rows, cols, x, y, w, h] = grid;
    if rows == 0 || cols == 0 || w == 0 || h == 0 {
        return Err("grid: rows, cols, width and height must be at least 1".into());
    }
    if x + w > cols || y + h > rows {
        return Err(format!(
            "grid: cell {x}:{y}:{w}:{h} does not fit a {rows}x{cols} grid"
        ));
    }
    Ok(grid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_new_commands_parse() {
        assert!(Command::parse("swap left").is_err());
        assert_eq!(
            Command::parse("grid 2:2:1:0:1:2"),
            Ok(Command::Grid([2, 2, 1, 0, 1, 2]))
        );
        assert!(Command::parse("grid 2:2:2:0:1:1").is_err());
        assert!(Command::parse("grid 2:2:0:0").is_err());
        assert_eq!(
            Command::parse("label 2 web"),
            Ok(Command::Label(Some((2, "web".into()))))
        );
        assert_eq!(Command::parse("label"), Ok(Command::Label(None)));
        assert!(Command::parse("label 0 web").is_err());
        assert!(Command::parse("label 2 3").is_err());
        assert!(Command::parse("label 2 next").is_err());
        assert_eq!(
            Command::parse("space web"),
            Ok(Command::Space(SpaceTarget::Label("web".into())))
        );
        assert_eq!(
            Command::parse("rule space web Safari --follow"),
            Ok(Command::SpaceRule {
                space: SpaceTarget::Label("web".into()),
                app: "Safari".into(),
                follow: true,
            })
        );
        assert_eq!(
            Command::parse("rule space 3 Sistem Ayarları"),
            Ok(Command::SpaceRule {
                space: SpaceTarget::Number(3),
                app: "Sistem Ayarları".into(),
                follow: false,
            })
        );
        assert!(Command::parse("rule space next Safari").is_err());
        assert!(Command::parse("rule space web").is_err());
        assert_eq!(
            Command::parse("rule grid 1:2:0:0:1:1 Zed"),
            Ok(Command::GridRule {
                grid: [1, 2, 0, 0, 1, 1],
                app: "Zed".into(),
            })
        );
        assert_eq!(
            Command::parse("rule border_radius 0 Zed"),
            Ok(Command::BorderRadiusRule {
                radius: 0,
                app: "Zed".into(),
            })
        );
        assert!(Command::parse("rule border_radius 50 Zed").is_err());
        assert_eq!(
            Command::parse("scratchpad term kitty"),
            Ok(Command::Scratchpad {
                name: "term".into(),
                app: "kitty".into(),
                grid: SCRATCHPAD_GRID,
            })
        );
        assert_eq!(
            Command::parse("scratchpad notes Notes 4:4:1:1:2:2"),
            Ok(Command::Scratchpad {
                name: "notes".into(),
                app: "Notes".into(),
                grid: [4, 4, 1, 1, 2, 2],
            })
        );
        assert_eq!(
            Command::parse("scratchpad toggle term"),
            Ok(Command::ScratchpadToggle("term".into()))
        );
        assert!(Command::parse("scratchpad term").is_err());
        // Where a window goes on the screen is a grid rule now.
        assert!(Command::parse("rule Zed left 60%").is_err());
    }

    #[test]
    fn spaces_by_number_or_way() {
        assert_eq!(
            Command::parse("space 2"),
            Ok(Command::Space(SpaceTarget::Number(2)))
        );
        assert_eq!(
            Command::parse("send next"),
            Ok(Command::Send(SpaceTarget::Next))
        );
        assert_eq!(
            Command::parse("space recent"),
            Ok(Command::Space(SpaceTarget::Recent))
        );
        assert!(Command::parse("space 0").is_err());
        assert!(Command::parse("space 2x").is_err());
        assert!(Command::parse("send").is_err());
        assert!(Command::parse("send 1 2").is_err());
    }

    #[test]
    fn moves_and_sizes() {
        assert_eq!(
            Command::parse("move left"),
            Ok(Command::Move(Direction::Left, None))
        );
        assert_eq!(
            Command::parse("resize right 20"),
            Ok(Command::Resize(Direction::Right, Some(20)))
        );
        assert_eq!(
            Command::parse("resize up"),
            Ok(Command::Resize(Direction::Up, None))
        );
        assert!(Command::parse("resize up 0").is_err());
        assert!(Command::parse("resize up -5").is_err());
        assert!(Command::parse("size right").is_err());
        assert!(Command::parse("shrink left").is_err());
        assert!(Command::parse("move").is_err());
        assert!(Command::parse("move sideways").is_err());
        assert!(Command::parse("move left far").is_err());
        assert!(Command::parse("move left 1 2").is_err());
    }

    #[test]
    fn places() {
        assert_eq!(
            Command::parse("place center"),
            Ok(Command::Place(Position::Center, None))
        );
        assert_eq!(
            Command::parse("place left 60%"),
            Ok(Command::Place(Position::Left, Some(60)))
        );
        assert!(Command::parse("place").is_err());
        assert!(Command::parse("place left 60").is_err());
        assert!(Command::parse("place left 0%").is_err());
        assert!(Command::parse("place left 101%").is_err());
        assert!(Command::parse("place nowhere").is_err());
    }

    #[test]
    fn rules_take_app_names_of_several_words() {
        assert_eq!(
            Command::parse("rule grid 1:2:0:0:1:1 Visual Studio Code"),
            Ok(Command::GridRule {
                grid: [1, 2, 0, 0, 1, 1],
                app: "Visual Studio Code".into(),
            })
        );
        assert!(Command::parse("rule").is_err());
        assert!(Command::parse("rule grid 1:1:0:0:1:1").is_err());
        assert!(Command::parse("rule grid 1:1:0:0:1:1 /[/").is_err());
    }

    #[test]
    fn config_and_bind() {
        assert_eq!(
            Command::parse("config padding 1 2"),
            Ok(Command::Config {
                key: "padding".into(),
                args: vec!["1".into(), "2".into()],
            })
        );
        assert!(Command::parse("config").is_err());
        assert_eq!(Command::parse("bind"), Ok(Command::Bind(None)));
        assert_eq!(
            Command::parse("bind alt - h : lily focus left"),
            Ok(Command::Bind(Some("alt - h : lily focus left".into())))
        );
        assert!(Command::parse("bind nonsense").is_err());
    }

    #[test]
    fn plain_commands_take_no_arguments() {
        assert_eq!(Command::parse("close"), Ok(Command::Close));
        assert!(Command::parse("close now").is_err());
        assert!(Command::parse("").is_err());
        assert!(Command::parse("tile").is_err());
    }
}
