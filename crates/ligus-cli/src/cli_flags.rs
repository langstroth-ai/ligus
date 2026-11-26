//! Flags every `ligus` command takes, read before the command runs:
//!
//! - `-o FORMAT` / `--output FORMAT`: `json` is `--json`, `jsonl` is
//!   `--jsonl` (streams), `text` the default;
//! - `-q` / `--quiet`: only what needs attention (`ligus doctor` prints the
//!   checks that aren't ok);
//! - `--color auto|always|never`: colour on a terminal only, honouring
//!   `NO_COLOR` and `CLICOLOR_FORCE` (JSON is never coloured).
//!
//! They apply anywhere before a `--`.

use std::io::IsTerminal as _;
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Globals {
    pub quiet: bool,
    pub color: Color,
}

static GLOBALS: OnceLock<Globals> = OnceLock::new();

pub fn globals() -> Globals {
    GLOBALS.get().copied().unwrap_or_default()
}

/// Takes the global flags out of `args` (rewriting `-o json` as `--json`)
/// and remembers them.
pub fn extract(args: Vec<String>) -> anyhow::Result<Vec<String>> {
    let (args, g) = parse(args)?;
    let _ = GLOBALS.set(g);
    Ok(args)
}

fn parse(args: Vec<String>) -> anyhow::Result<(Vec<String>, Globals)> {
    let mut g = Globals::default();
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => {
                out.push(a);
                out.extend(it.by_ref());
                break;
            }
            "-q" | "--quiet" => g.quiet = true,
            "-o" | "--output" => match it.next().as_deref() {
                Some("json") => out.push("--json".into()),
                Some("jsonl") => out.push("--jsonl".into()),
                Some("text") => {}
                other => {
                    return Err(crate::help::usage_error(format!(
                        "{a} takes json, jsonl (streaming commands) or text, not {}",
                        other
                            .map(|o| format!("{o:?}"))
                            .unwrap_or_else(|| "nothing".into())
                    )))
                }
            },
            "--color" | "--colour" => {
                g.color = match it.next().as_deref() {
                    Some("auto") => Color::Auto,
                    Some("always") => Color::Always,
                    Some("never") => Color::Never,
                    _ => {
                        return Err(crate::help::usage_error(
                            "--color takes auto, always or never",
                        ))
                    }
                }
            }
            _ => out.push(a),
        }
    }
    Ok((out, g))
}

/// Whether to colour text for a stream (`stdout_tty`: it is a terminal).
pub fn color_on(
    color: Color,
    stdout_tty: bool,
    no_color: Option<&str>,
    force: Option<&str>,
) -> bool {
    match color {
        Color::Always => true,
        Color::Never => false,
        Color::Auto => {
            if no_color.is_some_and(|v| !v.is_empty()) {
                return false;
            }
            if force.is_some_and(|v| !v.is_empty() && v != "0") {
                return true;
            }
            stdout_tty
        }
    }
}

/// Colour on stdout for this process.
pub fn stdout_color() -> bool {
    color_on(
        globals().color,
        std::io::stdout().is_terminal(),
        std::env::var("NO_COLOR").ok().as_deref(),
        std::env::var("CLICOLOR_FORCE").ok().as_deref(),
    )
}

/// The longest `--timeout` accepted: 30 days.
pub const MAX_DURATION_SECS: u64 = 30 * 24 * 3600;

/// `--timeout` values: `90` or `90s`, `5m`, `1h` (at most 30 days).
pub fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num.parse().map_err(|_| {
        crate::help::usage_error(format!("not a duration: {s:?} (try 90s, 5m or 1h)"))
    })?;
    let secs = match unit {
        "" | "s" => Some(n),
        "m" => n.checked_mul(60),
        "h" => n.checked_mul(3600),
        _ => {
            return Err(crate::help::usage_error(format!(
                "not a duration: {s:?} (try 90s, 5m or 1h)"
            )))
        }
    };
    // Capped (an overflowing or absurd value is a usage error, not a panic).
    match secs {
        Some(secs) if secs <= MAX_DURATION_SECS => Ok(Duration::from_secs(secs)),
        _ => Err(crate::help::usage_error(format!(
            "duration too long: {s:?} (at most 30 days)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn durations_overflow_and_cap_are_usage_errors() {
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(
            parse_duration("720h").unwrap(),
            Duration::from_secs(MAX_DURATION_SECS)
        );
        for bad in [
            "9999999999999999h",
            "18446744073709551615m",
            "721h",
            "99999999999999999999",
            "5d",
            "x",
        ] {
            let e = parse_duration(bad).unwrap_err();
            assert!(crate::help::is_usage(&e), "{bad}: {e:#}");
        }
    }

    #[test]
    fn global_flags_are_taken_out() {
        let (a, g) = parse(v("doctor -o json -q --color never")).unwrap();
        assert_eq!(a, v("doctor --json"));
        assert_eq!(
            g,
            Globals {
                quiet: true,
                color: Color::Never
            }
        );
        let (a, _) = parse(v("facts export --output jsonl")).unwrap();
        assert_eq!(a, v("facts export --jsonl"));
        let (a, _) = parse(v("status -o text")).unwrap();
        assert_eq!(a, v("status"));
        // After -- nothing is a flag.
        let (a, g) = parse(v("send x -- -q -o json")).unwrap();
        assert_eq!(a, v("send x -- -q -o json"));
        assert!(!g.quiet);
        assert!(parse(v("status -o yaml")).is_err());
        assert!(parse(v("status -o")).is_err());
        assert!(parse(v("status --color sometimes")).is_err());
    }

    #[test]
    fn colour_only_on_a_terminal_unless_asked() {
        assert!(color_on(Color::Auto, true, None, None));
        assert!(!color_on(Color::Auto, false, None, None));
        assert!(!color_on(Color::Auto, true, Some("1"), None));
        assert!(color_on(Color::Auto, false, None, Some("1")));
        assert!(!color_on(Color::Auto, false, None, Some("0")));
        assert!(color_on(Color::Always, false, Some("1"), None));
        assert!(!color_on(Color::Never, true, None, Some("1")));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("5d").is_err());
    }
}
