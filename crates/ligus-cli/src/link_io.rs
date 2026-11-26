//! The terminal side of linking a computer (`ligus devices request|redeem`):
//! copying the request to the clipboard, reading the desktop's code from a
//! hidden paste or the clipboard, pulling the code out of whatever was
//! pasted, and asking the person at the terminal. A code read here is never
//! echoed, logged or passed to another program on its command line.

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

use anyhow::Result;
use ligus_runtime::computer_link::{GRANT_PREFIX, MAX_CODE_LEN, REQUEST_PREFIX};

fn b64url(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// The `prefix` code inside `text`: whatever was pasted around it (a
/// whole copied command, quotes, surrounding blank lines) is dropped. The
/// code may be wrapped across lines (a terminal copy); a line break
/// continues it only when the next line starts with code characters and not
/// with a `--flag`.
pub fn extract_code(text: &str, prefix: &str) -> Option<String> {
    let start = text.find(prefix)?;
    let chars: Vec<char> = text[start + prefix.len()..].chars().collect();
    let mut body = String::new();
    let mut i = 0;
    while i < chars.len() && body.len() <= MAX_CODE_LEN {
        let c = chars[i];
        if b64url(c) {
            body.push(c);
            i += 1;
            continue;
        }
        // A line break (and the spaces around it) inside a wrapped code.
        let mut j = i;
        while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
            j += 1;
        }
        if j >= chars.len() || (chars[j] != '\n' && chars[j] != '\r') {
            break;
        }
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let flag = chars.get(j) == Some(&'-') && chars.get(j + 1) == Some(&'-');
        if j < chars.len() && b64url(chars[j]) && !flag {
            i = j;
        } else {
            break;
        }
    }
    (!body.is_empty()).then(|| format!("{prefix}{body}"))
}

/// The desktop's code (`ligusgrant1:`) inside `text`.
pub fn extract_grant(text: &str) -> Option<String> {
    extract_code(text, GRANT_PREFIX)
}

/// Why `text` holds no desktop code, for a person to read (`where_from`:
/// "the clipboard", "the paste", "the input").
pub fn no_grant_message(text: &str, where_from: &str) -> String {
    if text.contains(REQUEST_PREFIX) {
        format!("{where_from} holds this computer's request (liguslink1:...), not the desktop's code: on your desktop add the computer, press Copy for lt, then run this again")
    } else {
        format!("{where_from} doesn't hold a desktop code (ligusgrant1:...): on your desktop press Copy for lt, then run this again")
    }
}

/// Whether a person is at this terminal: stderr is a terminal (the prompts
/// go there) and their answers can be read (stdin is a terminal, or the
/// controlling terminal opens).
pub fn interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal() && (std::io::stdin().is_terminal() || tty_in().is_some())
}

#[cfg(unix)]
fn tty_in() -> Option<std::fs::File> {
    std::fs::OpenOptions::new().read(true).open("/dev/tty").ok()
}

#[cfg(not(unix))]
fn tty_in() -> Option<std::fs::File> {
    None
}

/// One line typed by the person: from the controlling terminal when stdin
/// is not one (it carried the code), else from stdin.
fn read_answer_line() -> Result<String> {
    use std::io::IsTerminal;
    let mut line = String::new();
    if !std::io::stdin().is_terminal() {
        if let Some(t) = tty_in() {
            std::io::BufReader::new(t).read_line(&mut line)?;
            return Ok(line);
        }
    }
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// `prompt [y/N]` on stderr; only `y`/`yes` is a yes.
pub fn confirm(prompt: &str) -> Result<bool> {
    let mut err = std::io::stderr();
    write!(err, "{prompt} [y/N] ")?;
    err.flush()?;
    Ok(is_yes(&read_answer_line()?))
}

pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Turns the terminal's echo off until dropped (unix; elsewhere a no-op).
struct NoEcho {
    #[cfg(unix)]
    saved: Option<libc::termios>,
}

impl NoEcho {
    fn new() -> NoEcho {
        #[cfg(unix)]
        {
            // SAFETY: tcgetattr/tcsetattr on stdin with a zeroed termios we own.
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &mut t) != 0 {
                    return NoEcho { saved: None };
                }
                let saved = t;
                t.c_lflag &= !libc::ECHO;
                t.c_lflag |= libc::ECHONL;
                if libc::tcsetattr(0, libc::TCSANOW, &t) != 0 {
                    return NoEcho { saved: None };
                }
                NoEcho { saved: Some(saved) }
            }
        }
        #[cfg(not(unix))]
        NoEcho {}
    }
}

impl Drop for NoEcho {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(t) = self.saved {
            // SAFETY: restores the attributes read in `new`.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &t);
            }
        }
    }
}

/// `prompt` on stderr, then one line from the terminal with echo off (a
/// pasted code never lands in the scrollback). Off unix it reads nothing
/// (the clipboard is used).
pub fn read_hidden(prompt: &str) -> Result<String> {
    if cfg!(not(unix)) {
        return Ok(String::new());
    }
    let mut err = std::io::stderr();
    write!(err, "{prompt}")?;
    err.flush()?;
    let _guard = NoEcho::new();
    let mut lock = std::io::stdin().lock();
    let mut buf = Vec::new();
    std::io::Read::take(&mut lock, MAX_CODE_LEN as u64 + 1).read_until(b'\n', &mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// What `ligus devices redeem` asks with no code at a terminal.
pub const PASTE_PROMPT: &str =
    "Paste the desktop's code (hidden) and press Enter, or just press Enter to read it from the clipboard: ";

/// The desktop's code, from a hidden paste or, when the person just pressed
/// Enter, the clipboard. Errors say what was there instead (never the text).
pub fn read_pasted(hidden: &mut dyn FnMut(&str) -> Result<String>, clipboard: &mut dyn FnMut() -> Option<String>) -> Result<String> {
    let typed = hidden(PASTE_PROMPT)?;
    let (text, from) = if typed.trim().is_empty() {
        match clipboard() {
            Some(c) => (c, "the clipboard"),
            None => anyhow::bail!(
                "couldn't read the clipboard (pbpaste, wl-paste, xclip, xsel or PowerShell): paste the code at the prompt, or pipe it in (ligus devices redeem -)"
            ),
        }
    } else {
        (typed, "the paste")
    };
    extract_grant(&text).ok_or_else(|| anyhow::anyhow!(no_grant_message(&text, from)))
}

type Tool = (&'static str, &'static [&'static str]);

fn copy_tools() -> Vec<Tool> {
    let mut v: Vec<Tool> = vec![];
    if cfg!(target_os = "macos") {
        v.push(("pbcopy", &[]));
    }
    if cfg!(windows) {
        v.push(("clip.exe", &[]));
    }
    if cfg!(all(unix, not(target_os = "macos"))) {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            v.push(("wl-copy", &[]));
        }
        if std::env::var_os("DISPLAY").is_some() {
            v.push(("xclip", &["-selection", "clipboard"]));
            v.push(("xsel", &["--clipboard", "--input"]));
        }
        if std::env::var_os("WSL_DISTRO_NAME").is_some() {
            v.push(("clip.exe", &[]));
        }
    }
    v
}

fn paste_tools() -> Vec<Tool> {
    let mut v: Vec<Tool> = vec![];
    if cfg!(target_os = "macos") {
        v.push(("pbpaste", &[]));
    }
    if cfg!(windows) {
        v.push(("powershell.exe", &["-NoProfile", "-Command", "Get-Clipboard"]));
    }
    if cfg!(all(unix, not(target_os = "macos"))) {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            v.push(("wl-paste", &["--no-newline"]));
        }
        if std::env::var_os("DISPLAY").is_some() {
            v.push(("xclip", &["-selection", "clipboard", "-o"]));
            v.push(("xsel", &["--clipboard", "--output"]));
        }
        if std::env::var_os("WSL_DISTRO_NAME").is_some() {
            v.push(("powershell.exe", &["-NoProfile", "-Command", "Get-Clipboard"]));
        }
    }
    v
}

/// Copy `text` with the platform's tool (on its stdin, never its command
/// line): the tool's name, or `None` when none worked.
pub fn copy(text: &str) -> Option<&'static str> {
    for (tool, args) in copy_tools() {
        let Ok(mut child) = Command::new(tool).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
            continue;
        };
        let wrote = child.stdin.take().is_some_and(|mut i| i.write_all(text.as_bytes()).is_ok());
        if wrote && child.wait().is_ok_and(|s| s.success()) {
            return Some(tool);
        }
    }
    None
}

/// The clipboard's text, read with the platform's tool; `None` when no tool
/// worked.
pub fn paste() -> Option<String> {
    for (tool, args) in paste_tools() {
        let Ok(out) = Command::new(tool).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output() else { continue };
        if out.status.success() {
            let cut = out.stdout.len().min(MAX_CODE_LEN + 1);
            return Some(String::from_utf8_lossy(&out.stdout[..cut]).into_owned());
        }
    }
    None
}

/// `HH:MM` in this computer's time zone (UTC when it can't be read).
pub fn hh_mm(unix_ms: i64) -> String {
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    match time::OffsetDateTime::from_unix_timestamp(unix_ms.div_euclid(1000)) {
        Ok(t) => {
            let t = t.to_offset(offset);
            format!("{:02}:{:02}{}", t.hour(), t.minute(), if offset.is_utc() { " UTC" } else { "" })
        }
        Err(_) => "?".into(),
    }
}

/// The one line `ligus status` (and `ligus devices list`) shows for a link
/// waiting for the desktop's code.
pub fn pending_line(p: &ligus_runtime::computer_link::PendingStatus, now_ms: i64) -> String {
    if p.approvable(now_ms) {
        format!("Linking waiting for your desktop's code (started {}; expires {})", hh_mm(p.created_at), hh_mm(p.approve_by))
    } else {
        format!("Linking waiting: the code your desktop showed works until {}", hh_mm(p.expires_at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: &str = "ligusgrant1:eyJ2IjoxLCJhIjoiYi1jX2QifQ";

    #[test]
    fn the_grant_comes_out_of_whatever_was_pasted() {
        assert_eq!(extract_grant(G).as_deref(), Some(G));
        assert_eq!(extract_grant(&format!("\n  {G}\n\n")).as_deref(), Some(G));
        assert_eq!(extract_grant(&format!("lt --plain link-redeem '{G}' --approver-fingerprint 2C41-88E0")).as_deref(), Some(G));
        assert_eq!(extract_grant(&format!("ligus devices redeem {G} --approver-fingerprint X")).as_deref(), Some(G));
        assert_eq!(extract_grant(&format!("\"{G}\"")).as_deref(), Some(G));
        // Wrapped by a terminal.
        let (a, b) = G.split_at(20);
        assert_eq!(extract_grant(&format!("{a}\n{b}\n")).as_deref(), Some(G));
        assert_eq!(extract_grant(&format!("{a}\r\n   {b}")).as_deref(), Some(G));
        // A flag on the next line isn't part of it.
        assert_eq!(extract_grant(&format!("{G}\n--approver-fingerprint X")).as_deref(), Some(G));
        assert_eq!(extract_grant("liguslink1:abc"), None);
        assert_eq!(extract_grant("ligusgrant1:"), None);
        assert_eq!(extract_grant(""), None);
    }

    #[test]
    fn a_paste_without_a_grant_says_what_it_was_and_never_echoes_it() {
        let m = no_grant_message("liguslink1:SECRETISH", "the clipboard");
        assert!(m.contains("this computer's request") && !m.contains("SECRETISH"), "{m}");
        let m = no_grant_message("hello there", "the clipboard");
        assert!(m.contains("Copy for lt") && !m.contains("hello"), "{m}");
    }

    #[test]
    fn enter_reads_the_clipboard_and_a_paste_wins_over_it() {
        let mut asked = String::new();
        let got = read_pasted(
            &mut |p| {
                asked = p.to_string();
                Ok("\n".into())
            },
            &mut || Some(format!("{G}\n")),
        )
        .unwrap();
        assert_eq!(got, G);
        assert_eq!(asked, PASTE_PROMPT);
        let got = read_pasted(&mut |_| Ok(format!("  {G} \n")), &mut || panic!("the clipboard isn't read after a paste")).unwrap();
        assert_eq!(got, G);
        let e = read_pasted(&mut |_| Ok(String::new()), &mut || Some("liguslink1:abc".into())).unwrap_err();
        assert!(e.to_string().contains("this computer's request"), "{e}");
        let e = read_pasted(&mut |_| Ok(String::new()), &mut || None).unwrap_err();
        assert!(e.to_string().contains("couldn't read the clipboard"), "{e}");
    }

    #[test]
    fn a_fingerprint_matches_however_it_was_copied() {
        use crate::devices_cmd::same_fingerprint;
        assert!(same_fingerprint("2c41 88e0 1b9d 04af 6e73", "2C41-88E0-1B9D-04AF-6E73"));
        assert!(same_fingerprint(" 2C41-88E0-1B9D-04AF-6E73\n", "2C41-88E0-1B9D-04AF-6E73"));
        assert!(!same_fingerprint("2C41-88E0-1B9D-04AF-6E74", "2C41-88E0-1B9D-04AF-6E73"));
        assert!(!same_fingerprint("", ""));
    }

    #[test]
    fn only_yes_is_yes() {
        for y in ["y", "Y\n", " yes "] {
            assert!(is_yes(y));
        }
        for n in ["", "\n", "n", "no", "yep"] {
            assert!(!is_yes(n));
        }
    }

    #[test]
    fn a_pending_link_reads_as_one_line() {
        let p = ligus_runtime::computer_link::PendingStatus { created_at: 0, approve_by: 900_000, expires_at: 2_700_000, count: 1 };
        assert!(pending_line(&p, 1_000).starts_with("Linking waiting for your desktop's code (started "));
        assert!(pending_line(&p, 1_000_000).starts_with("Linking waiting: the code your desktop showed works until "));
    }
}
