//! `ligus app`: open the Ligus desktop app (or bring it forward when it is
//! already running on macOS) and return at once. The app is started
//! detached from this terminal, so closing the terminal leaves it running.
//!
//! This command never reads the data dir, the config or the secret store:
//! `main` dispatches it before any of them is set up.
//!
//! How each platform finds the app:
//! - macOS: Launch Services, `open -b <bundle id>`, then `open -a Ligus`.
//! - Windows: the installer's registry keys (NSIS: the uninstall key's
//!   `InstallLocation` and `Software\<publisher>\Ligus`; MSI:
//!   `Software\<publisher>\Ligus\InstallDir`), then the default install dirs
//!   under `%LOCALAPPDATA%` and `%ProgramFiles%`, then `PATH`.
//! - Linux: `ligus-desktop` on `PATH` (the deb and rpm packages), then an
//!   AppImage: `LIGUS_APPIMAGE`, else `Ligus*.AppImage` in `~/Applications`
//!   or `~/.local/bin`.
//!
//! The lookups take a [`Host`] so the tests run them against fake env,
//! files and registry; nothing here launches anything under test.

// Each platform's `open` uses only its own lookup; all three stay compiled
// everywhere so the tests cover them from any OS.
#![allow(dead_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

/// `identifier` in app/tauri.conf.json (a test keeps them equal).
pub const BUNDLE_ID: &str = "ai.ligus.desktop";
/// `productName` in app/tauri.conf.json: the app's name and install dir.
pub const PRODUCT_NAME: &str = "Ligus";
/// `mainBinaryName` in app/tauri.conf.json.
pub const BINARY: &str = "ligus-desktop";
/// The bundler's publisher (`Software\<publisher>\Ligus` on Windows): unset
/// in tauri.conf.json, so the identifier's second part.
const PUBLISHER: &str = "ligus";
const DOWNLOAD_URL: &str = "https://langstroth.ai/ligus/download";
const BREW_CASK: &str = "brew install --cask langstroth-ai/ligus/ligus-app";

/// What the lookups may ask of the machine.
pub trait Host {
    fn var(&self, name: &str) -> Option<OsString>;
    fn is_file(&self, path: &Path) -> bool;
    /// Files in a directory (empty when it can't be read).
    fn list_dir(&self, dir: &Path) -> Vec<PathBuf>;
    /// A registry string value (`None` for the default value), Windows only.
    fn reg_value(&self, key: &str, name: Option<&str>) -> Option<String>;
}

/// The real machine.
struct RealHost;

impl Host for RealHost {
    fn var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name).filter(|v| !v.is_empty())
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn list_dir(&self, dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default()
    }

    fn reg_value(&self, key: &str, name: Option<&str>) -> Option<String> {
        if !cfg!(windows) {
            return None;
        }
        let mut cmd = std::process::Command::new("reg");
        cmd.args(["query", key]);
        match name {
            Some(n) => cmd.args(["/v", n]),
            None => cmd.arg("/ve"),
        };
        let out = cmd.stderr(std::process::Stdio::null()).output().ok()?;
        if !out.status.success() {
            return None;
        }
        parse_reg_query(&String::from_utf8_lossy(&out.stdout))
    }
}

/// The value in `reg query` output: the first `REG_SZ` / `REG_EXPAND_SZ`
/// line's data, without the quotes NSIS writes around `InstallLocation`.
fn parse_reg_query(out: &str) -> Option<String> {
    out.lines().find_map(|line| {
        let (_, data) = ["REG_EXPAND_SZ", "REG_SZ"]
            .iter()
            .find_map(|t| line.split_once(t))?;
        let v = data.trim().trim_matches('"').trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// Executables named `name` on `PATH`.
fn on_path(host: &dyn Host, name: &str, sep: char) -> Option<PathBuf> {
    let path = host.var("PATH")?;
    let path = path.to_string_lossy();
    path.split(sep)
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|p| host.is_file(p))
}

/// Where the app was looked for when it isn't found.
#[derive(Debug, Default)]
pub struct Searched(Vec<String>);

impl Searched {
    fn add(&mut self, s: impl Into<String>) {
        self.0.push(s.into());
    }
}

/// The installed app on Windows: `ligus-desktop.exe`.
pub fn find_windows(host: &dyn Host, searched: &mut Searched) -> Option<PathBuf> {
    let exe = format!("{BINARY}.exe");
    // The registry records where the installer put it, wherever that was.
    let uninstall = format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{PRODUCT_NAME}");
    let vendor = format!(r"Software\{PUBLISHER}\{PRODUCT_NAME}");
    let keys: [(String, Option<&str>); 6] = [
        (format!(r"HKCU\{uninstall}"), Some("InstallLocation")),
        (format!(r"HKLM\{uninstall}"), Some("InstallLocation")),
        (format!(r"HKCU\{vendor}"), None),
        (format!(r"HKLM\{vendor}"), None),
        (format!(r"HKCU\{vendor}"), Some("InstallDir")),
        (format!(r"HKLM\{vendor}"), Some("InstallDir")),
    ];
    for (key, name) in &keys {
        if let Some(dir) = host.reg_value(key, *name) {
            let p = Path::new(&dir).join(&exe);
            if host.is_file(&p) {
                return Some(p);
            }
        }
    }
    searched.add(format!(r"the registry (HKCU/HKLM {uninstall}, {vendor})"));
    // The bundler's default install dirs: NSIS per-user, then per-machine/MSI.
    let dirs: [(&str, &[&str]); 4] = [
        ("LOCALAPPDATA", &[PRODUCT_NAME]),
        ("LOCALAPPDATA", &["Programs", PRODUCT_NAME]),
        ("ProgramFiles", &[PRODUCT_NAME]),
        ("ProgramFiles(x86)", &[PRODUCT_NAME]),
    ];
    for (var, parts) in dirs {
        let rel = parts
            .iter()
            .fold(PathBuf::new(), |p, s| p.join(s))
            .join(&exe);
        match host.var(var) {
            Some(base) => {
                let p = PathBuf::from(base).join(&rel);
                if host.is_file(&p) {
                    return Some(p);
                }
                searched.add(p.display().to_string());
            }
            None => searched.add(format!("%{var}%\\{}", rel.display())),
        }
    }
    if let Some(p) = on_path(host, &exe, ';') {
        return Some(p);
    }
    searched.add(format!("{exe} on PATH"));
    None
}

/// The installed app on Linux: the packaged binary, else an AppImage.
pub fn find_linux(host: &dyn Host, searched: &mut Searched) -> Result<Option<PathBuf>> {
    if let Some(p) = on_path(host, BINARY, ':') {
        return Ok(Some(p));
    }
    searched.add(format!("{BINARY} on PATH"));
    if let Some(img) = host.var("LIGUS_APPIMAGE") {
        let p = PathBuf::from(img);
        if host.is_file(&p) {
            return Ok(Some(p));
        }
        // Named on purpose: a wrong path is a mistake to report, not skip.
        bail!(
            "LIGUS_APPIMAGE is set to {}, which is not a file",
            p.display()
        );
    }
    searched.add("LIGUS_APPIMAGE (unset)");
    let Some(home) = host.var("HOME") else {
        searched.add("~/Applications and ~/.local/bin (HOME is unset)");
        return Ok(None);
    };
    for dir in [
        Path::new(&home).join("Applications"),
        Path::new(&home).join(".local").join("bin"),
    ] {
        // The newest-looking one when there are several (Ligus_0.2.0_… sorts
        // after Ligus_0.1.0_…).
        let mut images: Vec<PathBuf> = host
            .list_dir(&dir)
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(is_ligus_appimage)
            })
            .filter(|p| host.is_file(p))
            .collect();
        images.sort();
        if let Some(p) = images.pop() {
            return Ok(Some(p));
        }
        searched.add(format!("{}/Ligus*.AppImage", dir.display()));
    }
    Ok(None)
}

fn is_ligus_appimage(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("ligus") && n.ends_with(".appimage")
}

/// Linux: a desktop needs a graphical session. A serve box, a container or
/// an SSH login has neither variable set.
pub fn linux_display_problem(host: &dyn Host) -> Option<String> {
    if host.var("DISPLAY").is_some() || host.var("WAYLAND_DISPLAY").is_some() {
        return None;
    }
    Some(
        "no graphical session here (neither DISPLAY nor WAYLAND_DISPLAY is set), so the desktop app can't open. \
         Run `ligus app` from a terminal on the machine's own desktop; on a headless box (a `ligus serve` host, \
         a worker, a container) use the phone app or the other `ligus` commands instead"
            .into(),
    )
}

/// The not-found error: where it looked and how to install the app.
pub fn not_found(os: &str, searched: &Searched) -> String {
    let mut msg = String::from(
        "the Ligus desktop app is not installed (or not where `ligus app` looks).\nLooked in:\n",
    );
    for s in &searched.0 {
        msg.push_str(&format!("  - {s}\n"));
    }
    msg.push_str(&format!("Download it from {DOWNLOAD_URL}"));
    if os == "macos" {
        msg.push_str(&format!(", or: {BREW_CASK}"));
    }
    if os == "linux" {
        msg.push_str(" (for an AppImage elsewhere, set LIGUS_APPIMAGE to its path)");
    }
    msg
}

/// Linux: whether a `ligus-desktop` process is running, from `<proc>/*/comm`.
fn running_on_linux(proc_root: &Path) -> bool {
    std::fs::read_dir(proc_root)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
                && std::fs::read_to_string(e.path().join("comm"))
                    .is_ok_and(|c| c.trim_end() == BINARY)
        })
}

/// Windows: whether `tasklist` output lists `ligus-desktop.exe`.
fn tasklist_lists_app(out: &str) -> bool {
    let exe = format!("{BINARY}.exe").to_ascii_lowercase();
    out.lines()
        .any(|l| l.trim_start().to_ascii_lowercase().starts_with(&exe))
}

/// `ligus app`.
pub fn cmd_app(args: &[String]) -> Result<()> {
    if let Some(extra) = args.first() {
        bail!("`ligus app` takes no arguments (got {extra:?}): it only opens the desktop app");
    }
    open(&RealHost)
}

#[cfg(target_os = "macos")]
fn open(_host: &dyn Host) -> Result<()> {
    use std::process::{Command, Stdio};
    // Launch Services starts the app, or brings it forward when it is
    // already running; `open` returns once it has handed the app off.
    let try_open = |args: &[&str]| {
        Command::new("/usr/bin/open")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if try_open(&["-b", BUNDLE_ID]) || try_open(&["-a", PRODUCT_NAME]) {
        return Ok(());
    }
    let mut searched = Searched::default();
    searched.add(format!(
        "Launch Services, by bundle id {BUNDLE_ID} (open -b)"
    ));
    searched.add(format!(
        "Launch Services, by name {PRODUCT_NAME} (open -a: /Applications, ~/Applications)"
    ));
    bail!(not_found("macos", &searched))
}

#[cfg(target_os = "linux")]
fn open(host: &dyn Host) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    if let Some(problem) = linux_display_problem(host) {
        bail!(problem);
    }
    if running_on_linux(Path::new("/proc")) {
        // The app has no single-instance handoff, so a second launch would
        // open a second copy on the same data. Its window is already open.
        println!("Ligus is already running; switch to its window.");
        return Ok(());
    }
    let mut searched = Searched::default();
    let Some(app) = find_linux(host, &mut searched)? else {
        bail!(not_found("linux", &searched));
    };
    let mut cmd = Command::new(&app);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid() is async-signal-safe and touches no memory; it puts
    // the app in its own session, away from this terminal's hangup.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()
        .map_err(|e| anyhow::anyhow!("could not start {}: {e}", app.display()))?;
    println!("Opened Ligus ({}).", app.display());
    Ok(())
}

#[cfg(windows)]
fn open(host: &dyn Host) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let running = Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {BINARY}.exe"), "/NH"])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| tasklist_lists_app(&String::from_utf8_lossy(&o.stdout)));
    if running {
        // No single-instance handoff in the app: don't start a second copy.
        println!("Ligus is already running; switch to its window.");
        return Ok(());
    }
    let mut searched = Searched::default();
    let Some(app) = find_windows(host, &mut searched) else {
        bail!(not_found("windows", &searched));
    };
    Command::new(&app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not start {}: {e}", app.display()))?;
    println!("Opened Ligus ({}).", app.display());
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn open(_host: &dyn Host) -> Result<()> {
    bail!("the Ligus desktop app runs on macOS, Windows and Linux; see {DOWNLOAD_URL}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct FakeHost {
        vars: HashMap<String, String>,
        files: HashSet<PathBuf>,
        reg: HashMap<(String, Option<String>), String>,
    }

    impl FakeHost {
        fn var(mut self, k: &str, v: &str) -> Self {
            self.vars.insert(k.into(), v.into());
            self
        }
        fn file(mut self, p: impl Into<PathBuf>) -> Self {
            self.files.insert(p.into());
            self
        }
        fn reg(mut self, key: &str, name: Option<&str>, v: &str) -> Self {
            self.reg
                .insert((key.into(), name.map(Into::into)), v.into());
            self
        }
    }

    impl Host for FakeHost {
        fn var(&self, name: &str) -> Option<OsString> {
            self.vars.get(name).map(Into::into)
        }
        fn is_file(&self, path: &Path) -> bool {
            self.files.contains(path)
        }
        fn list_dir(&self, dir: &Path) -> Vec<PathBuf> {
            self.files
                .iter()
                .filter(|p| p.parent() == Some(dir))
                .cloned()
                .collect()
        }
        fn reg_value(&self, key: &str, name: Option<&str>) -> Option<String> {
            self.reg
                .get(&(key.to_string(), name.map(Into::into)))
                .cloned()
        }
    }

    #[test]
    fn names_match_tauri_conf() {
        let conf = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../app/tauri.conf.json"
        ))
        .unwrap();
        let conf: serde_json::Value = serde_json::from_str(&conf).unwrap();
        assert_eq!(conf["identifier"], BUNDLE_ID);
        assert_eq!(conf["productName"], PRODUCT_NAME);
        assert_eq!(conf["mainBinaryName"], BINARY);
        // No publisher set: the bundler uses the identifier's second part.
        assert!(conf["bundle"].get("publisher").is_none());
        assert_eq!(BUNDLE_ID.split('.').nth(1), Some(PUBLISHER));
    }

    #[test]
    fn linux_prefers_the_packaged_binary_on_path() {
        let h = FakeHost::default()
            .var("PATH", "/opt/x:/usr/bin")
            .var("HOME", "/home/u")
            .file("/usr/bin/ligus-desktop")
            .file("/home/u/Applications/Ligus_0.1.0_amd64.AppImage");
        let mut s = Searched::default();
        assert_eq!(
            find_linux(&h, &mut s).unwrap(),
            Some(PathBuf::from("/usr/bin/ligus-desktop"))
        );
    }

    #[test]
    fn linux_finds_an_appimage() {
        let h = FakeHost::default()
            .var("PATH", "/usr/bin")
            .var("HOME", "/home/u")
            .file("/home/u/Applications/notes.AppImage")
            .file("/home/u/Applications/Ligus_0.1.0_amd64.AppImage")
            .file("/home/u/Applications/Ligus_0.2.0_amd64.AppImage");
        let mut s = Searched::default();
        assert_eq!(
            find_linux(&h, &mut s).unwrap(),
            Some(PathBuf::from(
                "/home/u/Applications/Ligus_0.2.0_amd64.AppImage"
            ))
        );
        let h = FakeHost::default()
            .var("HOME", "/home/u")
            .file("/home/u/.local/bin/ligus.appimage");
        assert_eq!(
            find_linux(&h, &mut s).unwrap(),
            Some(PathBuf::from("/home/u/.local/bin/ligus.appimage"))
        );
    }

    #[test]
    fn linux_appimage_env_wins_and_a_bad_one_is_an_error() {
        let h = FakeHost::default()
            .var("HOME", "/home/u")
            .var("LIGUS_APPIMAGE", "/data/L.AppImage")
            .file("/data/L.AppImage")
            .file("/home/u/Applications/Ligus.AppImage");
        let mut s = Searched::default();
        assert_eq!(
            find_linux(&h, &mut s).unwrap(),
            Some(PathBuf::from("/data/L.AppImage"))
        );
        let h = FakeHost::default().var("LIGUS_APPIMAGE", "/nope");
        let err = find_linux(&h, &mut s).unwrap_err().to_string();
        assert!(
            err.contains("LIGUS_APPIMAGE") && err.contains("/nope"),
            "{err}"
        );
    }

    #[test]
    fn linux_not_found_names_where_it_looked() {
        let h = FakeHost::default()
            .var("PATH", "/usr/bin")
            .var("HOME", "/home/u");
        let mut s = Searched::default();
        assert_eq!(find_linux(&h, &mut s).unwrap(), None);
        let msg = not_found("linux", &s);
        for want in [
            "ligus-desktop on PATH",
            "LIGUS_APPIMAGE",
            "/home/u/Applications/Ligus*.AppImage",
            "/home/u/.local/bin/Ligus*.AppImage",
            DOWNLOAD_URL,
        ] {
            assert!(msg.contains(want), "missing {want:?} in {msg}");
        }
        assert!(!msg.contains("brew"), "{msg}");
    }

    #[test]
    fn linux_without_a_display_refuses() {
        assert!(linux_display_problem(&FakeHost::default())
            .unwrap()
            .contains("DISPLAY"));
        assert!(linux_display_problem(&FakeHost::default().var("DISPLAY", ":0")).is_none());
        assert!(
            linux_display_problem(&FakeHost::default().var("WAYLAND_DISPLAY", "wayland-0"))
                .is_none()
        );
    }

    #[test]
    fn windows_uses_the_registry_install_location() {
        let dir = r"D:\Apps\Ligus";
        let h = FakeHost::default()
            .var("LOCALAPPDATA", r"C:\Users\u\AppData\Local")
            .reg(
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Ligus",
                Some("InstallLocation"),
                dir,
            )
            .file(Path::new(dir).join("ligus-desktop.exe"))
            .file(
                Path::new(r"C:\Users\u\AppData\Local")
                    .join("Ligus")
                    .join("ligus-desktop.exe"),
            );
        let mut s = Searched::default();
        assert_eq!(
            find_windows(&h, &mut s),
            Some(Path::new(dir).join("ligus-desktop.exe"))
        );
        // The MSI's InstallDir under the publisher key.
        let h = FakeHost::default()
            .reg(
                r"HKCU\Software\ligus\Ligus",
                Some("InstallDir"),
                r"C:\Program Files\Ligus\",
            )
            .file(Path::new(r"C:\Program Files\Ligus\").join("ligus-desktop.exe"));
        assert_eq!(
            find_windows(&h, &mut s),
            Some(Path::new(r"C:\Program Files\Ligus\").join("ligus-desktop.exe"))
        );
    }

    #[test]
    fn windows_falls_back_to_default_dirs_then_path() {
        let local = r"C:\Users\u\AppData\Local";
        let per_user = Path::new(local).join("Ligus").join("ligus-desktop.exe");
        let h = FakeHost::default()
            .var("LOCALAPPDATA", local)
            .file(per_user.clone());
        let mut s = Searched::default();
        assert_eq!(find_windows(&h, &mut s), Some(per_user));
        let pf = Path::new(r"C:\Program Files")
            .join("Ligus")
            .join("ligus-desktop.exe");
        let h = FakeHost::default()
            .var("ProgramFiles", r"C:\Program Files")
            .file(pf.clone());
        assert_eq!(find_windows(&h, &mut s), Some(pf));
        let on_path = Path::new(r"C:\tools").join("ligus-desktop.exe");
        let h = FakeHost::default()
            .var("PATH", r"C:\Windows;C:\tools")
            .file(on_path.clone());
        assert_eq!(find_windows(&h, &mut s), Some(on_path));
    }

    #[test]
    fn windows_not_found_names_where_it_looked() {
        let h = FakeHost::default().var("LOCALAPPDATA", r"C:\L");
        let mut s = Searched::default();
        assert_eq!(find_windows(&h, &mut s), None);
        let msg = not_found("windows", &s);
        for want in [
            "registry",
            r"C:\L",
            "%ProgramFiles%",
            "ligus-desktop.exe on PATH",
            DOWNLOAD_URL,
        ] {
            assert!(msg.contains(want), "missing {want:?} in {msg}");
        }
    }

    #[test]
    fn macos_not_found_offers_the_cask() {
        let msg = not_found("macos", &Searched::default());
        assert!(
            msg.contains(BREW_CASK) && msg.contains(DOWNLOAD_URL),
            "{msg}"
        );
    }

    #[test]
    fn parses_reg_query_output() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Ligus\r\n    \
                   InstallLocation    REG_SZ    \"C:\\Users\\u\\AppData\\Local\\Ligus\"\r\n\r\n";
        assert_eq!(
            parse_reg_query(out).as_deref(),
            Some(r"C:\Users\u\AppData\Local\Ligus")
        );
        let out = "HKEY_CURRENT_USER\\Software\\ligus\\Ligus\r\n    (Default)    REG_SZ    C:\\x\\Ligus\r\n";
        assert_eq!(parse_reg_query(out).as_deref(), Some(r"C:\x\Ligus"));
        assert_eq!(
            parse_reg_query("ERROR: The system was unable to find the specified registry key"),
            None
        );
    }

    #[test]
    fn detects_a_running_app() {
        assert!(tasklist_lists_app(
            "\r\nligus-desktop.exe            4242 Console    1    120,000 K\r\n"
        ));
        assert!(!tasklist_lists_app(
            "INFO: No tasks are running which match the specified criteria.\r\n"
        ));
        let proc = tempfile::tempdir().unwrap();
        std::fs::create_dir(proc.path().join("12")).unwrap();
        std::fs::write(proc.path().join("12/comm"), "bash\n").unwrap();
        assert!(!running_on_linux(proc.path()));
        std::fs::create_dir(proc.path().join("34")).unwrap();
        std::fs::write(proc.path().join("34/comm"), "ligus-desktop\n").unwrap();
        assert!(running_on_linux(proc.path()));
    }

    #[test]
    fn extra_arguments_are_refused_before_anything_runs() {
        let err = cmd_app(&["ligus://x".into()]).unwrap_err().to_string();
        assert!(err.contains("takes no arguments"), "{err}");
    }
}
