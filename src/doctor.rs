//! `lily doctor`: looks at what lilypad needs and says what to do about
//! whatever is missing, and which ways of using Spaces work on this Mac.

use crate::config::Config;
use crate::{ipc, service};

/// What was found on this Mac.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// The config script, if there is one.
    pub config: Option<String>,
    /// The binary the login service runs, and whether it is still there.
    pub service: Option<(String, bool)>,
    /// What the running window manager says about itself (`status`), or
    /// `None` if none answers.
    pub status: Option<String>,
    /// The Spaces whose *Switch to Desktop N* shortcut is off.
    pub shortcuts_off: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: &'static str,
    /// What was found, when all is well.
    pub found: Option<String>,
    /// What is wrong and what to do about it.
    pub fix: Option<String>,
}

impl Check {
    fn ok(name: &'static str, found: impl Into<String>) -> Self {
        Self {
            name,
            found: Some(found.into()),
            fix: None,
        }
    }

    fn bad(name: &'static str, fix: impl Into<String>) -> Self {
        Self {
            name,
            found: None,
            fix: Some(fix.into()),
        }
    }
}

/// The value of `key` in a `status` reply.
fn status_value<'a>(status: &'a str, key: &str) -> Option<&'a str> {
    status.lines().find_map(|line| {
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(' ').or_else(|| rest.strip_prefix(": ")))
    })
}

/// What `facts` come to, for this `version` of lily.
pub fn checks(facts: &Facts, version: &str) -> Vec<Check> {
    let config = match &facts.config {
        Some(path) => Check::ok("Config", path),
        None => Check::bad(
            "Config",
            "no config script: copy examples/lilyrc to ~/.config/lilypad/lilyrc",
        ),
    };
    let service = match &facts.service {
        Some((path, true)) => Check::ok("Service", path),
        Some((path, false)) => Check::bad(
            "Service",
            format!("{path} is gone: run `lily install`, then `lily restart`"),
        ),
        None => Check::bad("Service", "not installed: run `lily install`"),
    };
    let running = match facts.status.as_deref().map(|s| status_value(s, "version")) {
        Some(Some(v)) if v == version => Check::ok("Running", v),
        Some(Some(v)) => Check::bad(
            "Running",
            format!("version {v} is running, not {version}: run `lily restart`"),
        ),
        Some(None) => Check::bad(
            "Running",
            format!("an older lily is running, not {version}: run `lily restart`"),
        ),
        None => Check::bad(
            "Running",
            "nothing answers: run `lily start`, and enable lily under System Settings > \
             Privacy & Security > Accessibility",
        ),
    };
    let shortcuts = if facts.status.is_none() {
        Check::ok("Shortcuts", "not checked: lilypad is not running")
    } else if facts.shortcuts_off.is_empty() {
        Check::ok("Shortcuts", "Switch to Desktop is on for every Space")
    } else {
        let spaces: Vec<String> = facts
            .shortcuts_off
            .iter()
            .map(ToString::to_string)
            .collect();
        Check::bad(
            "Shortcuts",
            format!(
                "Switch to Desktop {} is off: lilypad falls back on it when switching at \
                 once does not work; turn it on under System Settings > Keyboard > Keyboard \
                 Shortcuts > Mission Control",
                spaces.join(", ")
            ),
        )
    };
    let mut out = vec![config, service, running, shortcuts];
    if let Some(status) = &facts.status {
        let switch = status_value(status, "switch at once");
        out.push(match switch {
            Some("does not work here") => Check::bad(
                "Switch",
                "switching Spaces at once does not work on this macOS: the Switch to \
                 Desktop shortcut is used instead, with its slide",
            ),
            _ => Check::ok(
                "Switch",
                format!(
                    "at once ({})",
                    status_value(status, "last switch").unwrap_or("none yet")
                ),
            ),
        });
        let ways = ["move at once", "move by tag", "move directly"];
        let working: Vec<&str> = ways
            .into_iter()
            .filter(|w| status_value(status, w) != Some("does not work here"))
            .collect();
        out.push(if working.is_empty() {
            Check::bad(
                "Move",
                "no way of moving windows between Spaces works on this macOS: `send` and \
                 Space rules leave windows where they are",
            )
        } else {
            Check::ok(
                "Move",
                format!(
                    "{} (last: {})",
                    working.join(", "),
                    status_value(status, "last move").unwrap_or("none yet")
                ),
            )
        });
    }
    out
}

/// The checks as lines to print.
pub fn report(checks: &[Check]) -> String {
    let mut out = String::new();
    for check in checks {
        let (mark, text) = match (&check.fix, &check.found) {
            (Some(fix), _) => ("✗", fix.as_str()),
            (None, found) => ("✓", found.as_deref().unwrap_or("")),
        };
        out.push_str(&format!("{mark} {:<10}{text}\n", check.name));
    }
    out
}

/// The binary a launchd plist runs: the first of its program arguments.
fn plist_binary(plist: &str) -> Option<String> {
    let rest = plist.split("<key>ProgramArguments</key>").nth(1)?;
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")?;
    Some(rest[start..start + end].to_string())
}

/// Looks at this Mac.
pub fn facts() -> Facts {
    let status = ipc::send(&ipc::socket_path(), "status")
        .ok()
        .map(|reply| reply.unwrap_or_default());
    let spaces = status
        .as_deref()
        .and_then(|s| status_value(s, "spaces"))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let service = service::plist_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|plist| plist_binary(&plist))
        .map(|bin| {
            let there = std::path::Path::new(&bin).exists();
            (bin, there)
        });
    Facts {
        config: Config::find_rc().map(|p| p.display().to_string()),
        service,
        status,
        shortcuts_off: (1..=spaces).filter(|n| !shortcut_on(*n)).collect(),
    }
}

#[cfg(any(target_os = "macos", lily_check_macos))]
fn shortcut_on(n: usize) -> bool {
    crate::platform::macos::desktop_shortcut_on(n)
}

#[cfg(not(any(target_os = "macos", lily_check_macos)))]
fn shortcut_on(_n: usize) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "version 1.0.0\nspaces 3\nswitch at once: not ruled out\n\
        move at once: not ruled out\nmove by tag: does not work here\n\
        move directly: does not work here\nlast switch: at once\nlast move: at once\n";

    fn healthy() -> Facts {
        Facts {
            config: Some("/Users/me/.config/lilypad/lilyrc".into()),
            service: Some(("/Users/me/bin/lily".into(), true)),
            status: Some(STATUS.into()),
            shortcuts_off: Vec::new(),
        }
    }

    fn fixes(facts: &Facts) -> Vec<String> {
        checks(facts, "1.0.0")
            .into_iter()
            .filter_map(|c| c.fix)
            .collect()
    }

    #[test]
    fn a_healthy_mac_has_nothing_to_fix() {
        assert_eq!(fixes(&healthy()), Vec::<String>::new());
        let report = report(&checks(&healthy(), "1.0.0"));
        assert!(
            report.contains("✓ Move      move at once (last: at once)"),
            "{report}"
        );
        assert!(report.contains("✓ Switch    at once (at once)"), "{report}");
    }

    #[test]
    fn a_missing_config_says_where_it_goes() {
        let facts = Facts {
            config: None,
            ..healthy()
        };
        assert!(fixes(&facts)[0].contains("lilyrc"));
    }

    #[test]
    fn a_service_pointing_at_a_missing_binary_is_reinstalled() {
        let facts = Facts {
            service: Some(("/opt/old/lily".into(), false)),
            ..healthy()
        };
        assert!(fixes(&facts)[0].contains("lily install"));
    }

    #[test]
    fn nothing_running_mentions_accessibility_and_checks_no_spaces() {
        let facts = Facts {
            status: None,
            shortcuts_off: vec![1],
            ..healthy()
        };
        let found = fixes(&facts);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("Accessibility"));
    }

    #[test]
    fn an_old_version_still_running_is_restarted() {
        let facts = Facts {
            status: Some(STATUS.replace("1.0.0", "0.9.0")),
            ..healthy()
        };
        assert!(fixes(&facts)[0].contains("lily restart"));
        let older = Facts {
            status: Some(String::new()),
            ..healthy()
        };
        assert!(fixes(&older)[0].contains("older"));
    }

    #[test]
    fn shortcuts_that_are_off_are_named() {
        let facts = Facts {
            shortcuts_off: vec![2, 3],
            ..healthy()
        };
        assert!(fixes(&facts)[0].contains("Switch to Desktop 2, 3"));
    }

    #[test]
    fn spaces_that_do_not_work_say_so() {
        let facts = Facts {
            status: Some(
                STATUS
                    .replace(
                        "switch at once: not ruled out",
                        "switch at once: does not work here",
                    )
                    .replace(
                        "move at once: not ruled out",
                        "move at once: does not work here",
                    ),
            ),
            ..healthy()
        };
        let found = fixes(&facts);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("shortcut is used instead"));
        assert!(found[1].contains("leave windows where they are"));
    }

    #[test]
    fn the_binary_is_read_from_the_plist() {
        let plist = "<key>ProgramArguments</key>\n<array>\n<string>/Users/me/bin/lily</string>\n\
                     <string>run</string>";
        assert_eq!(plist_binary(plist).as_deref(), Some("/Users/me/bin/lily"));
        assert_eq!(plist_binary("<plist/>"), None);
    }
}
