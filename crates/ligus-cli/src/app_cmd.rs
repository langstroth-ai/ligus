//! `ligus app`: open the Ligus desktop app (or bring it forward when it is
//! already running on macOS) and return at once. The app is started
//! detached from this terminal, so closing the terminal leaves it running.
//!
//! This command never reads the data dir, the config or the secret store:
//! `main` dispatches it before any of them is set up.
//!
//! How each platform finds the app:
//! - macOS: `LIGUS_APP`, then `/Applications/Ligus.app`, then
//!   `~/Applications/Ligus.app`, then, as a last resort, the copies Spotlight
//!   knows by bundle id (never one on a mounted disk image under `/Volumes/`
//!   or an App Translocation copy). The chosen bundle is opened by path:
//!   asking Launch Services by bundle id picks the highest `CFBundleVersion`
//!   it has seen, which can be an old test build on a mounted DMG.
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
    /// macOS: an app bundle's `CFBundleIdentifier` and
    /// `CFBundleShortVersionString`, from its Info.plist.
    fn bundle_info(&self, _app: &Path) -> Option<BundleInfo> {
        None
    }
    /// macOS: the app bundles Spotlight knows with this bundle id.
    fn spotlight_bundles(&self, _id: &str) -> Vec<PathBuf> {
        Vec::new()
    }
    /// macOS: the app bundles whose executables are running now.
    fn running_bundles(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// What an app bundle's Info.plist says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleInfo {
    pub id: String,
    pub version: Option<String>,
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

    fn bundle_info(&self, app: &Path) -> Option<BundleInfo> {
        let plist = app.join("Contents").join("Info.plist");
        let key = |k: &str| {
            let out = std::process::Command::new("/usr/bin/plutil")
                .args(["-extract", k, "raw", "-o", "-"])
                .arg(&plist)
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (out.status.success() && !v.is_empty()).then_some(v)
        };
        Some(BundleInfo {
            id: key("CFBundleIdentifier")?,
            version: key("CFBundleShortVersionString"),
        })
    }

    fn spotlight_bundles(&self, id: &str) -> Vec<PathBuf> {
        std::process::Command::new("/usr/bin/mdfind")
            .arg(format!("kMDItemCFBundleIdentifier == '{id}'"))
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn running_bundles(&self) -> Vec<PathBuf> {
        std::process::Command::new("/bin/ps")
            .args(["-axww", "-o", "comm="])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(bundle_of_executable)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The `.app` bundle an executable path (a `ps -o comm=` line) runs from.
fn bundle_of_executable(exe: &str) -> Option<PathBuf> {
    let (bundle, _) = exe.trim().split_once(".app/Contents/MacOS/")?;
    Some(PathBuf::from(format!("{bundle}.app")))
}

/// macOS: a copy `ligus app` never picks on its own, only when `LIGUS_APP`
/// names it: an app on a mounted disk image, or a copy Gatekeeper's App
/// Translocation runs from a random read-only path.
pub fn is_transient_app_path(p: &Path) -> bool {
    p.starts_with("/Volumes") || p.to_string_lossy().contains("/AppTranslocation/")
}

/// The numeric parts of a version (`0.2.1-rc.1` → `[0, 2, 1]`), for ordering.
fn version_key(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()
        .unwrap_or("")
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

/// The installed app on macOS, as a `.app` bundle path, in a fixed order:
/// `LIGUS_APP`, `/Applications`, `~/Applications`, then the copies Spotlight
/// knows by bundle id (not on a disk image or translocated; the one at
/// `cli_version`, else the newest).
pub fn find_macos(
    host: &dyn Host,
    cli_version: &str,
    searched: &mut Searched,
) -> Result<Option<PathBuf>> {
    let is_bundle = |p: &Path| host.is_file(&p.join("Contents").join("Info.plist"));
    if let Some(app) = host.var("LIGUS_APP") {
        let p = PathBuf::from(app);
        if is_bundle(&p) {
            return Ok(Some(p));
        }
        // Named on purpose: a wrong path is a mistake to report, not skip.
        bail!(
            "LIGUS_APP is set to {}, which is not an app bundle (no Contents/Info.plist)",
            p.display()
        );
    }
    searched.add("LIGUS_APP (unset)");
    // macOS paths, built as strings so the tests read the same on any OS.
    let mut dirs = vec!["/Applications".to_string()];
    match host.var("HOME") {
        Some(home) => dirs.push(format!(
            "{}/Applications",
            home.to_string_lossy().trim_end_matches('/')
        )),
        None => searched.add("~/Applications (HOME is unset)"),
    }
    for dir in dirs {
        let p = PathBuf::from(format!("{dir}/{PRODUCT_NAME}.app"));
        if is_bundle(&p) {
            return Ok(Some(p));
        }
        searched.add(p.display().to_string());
    }
    let mut found: Vec<(PathBuf, Option<String>)> = host
        .spotlight_bundles(BUNDLE_ID)
        .into_iter()
        .filter(|p| !is_transient_app_path(p) && is_bundle(p))
        .filter_map(|p| {
            let info = host.bundle_info(&p)?;
            (info.id == BUNDLE_ID).then_some((p, info.version))
        })
        .collect();
    searched.add(format!(
        "Spotlight, by bundle id {BUNDLE_ID} (not under /Volumes or App Translocation)"
    ));
    if let Some(i) = found
        .iter()
        .position(|(_, v)| v.as_deref() == Some(cli_version))
    {
        return Ok(Some(found.swap_remove(i).0));
    }
    found.sort_by(|a, b| {
        let key = |v: &Option<String>| v.as_deref().map(version_key);
        key(&a.1).cmp(&key(&b.1)).then_with(|| b.0.cmp(&a.0))
    });
    Ok(found.pop().map(|(p, _)| p))
}

/// macOS: a running copy of the app (by bundle id) at another path than
/// `chosen`.
pub fn running_elsewhere(host: &dyn Host, chosen: &Path) -> Option<PathBuf> {
    host.running_bundles()
        .into_iter()
        .find(|p| p != chosen && host.bundle_info(p).is_some_and(|i| i.id == BUNDLE_ID))
}

/// A one-line note when the app and this CLI are different versions: which
/// is older, and the command that updates it.
pub fn version_note(cli: &str, app: Option<&str>, path: &Path) -> Option<String> {
    let app = app?;
    if app == cli {
        return None;
    }
    let fix = match version_key(app).cmp(&version_key(cli)) {
        std::cmp::Ordering::Less => {
            "update the app: brew upgrade --cask --greedy ligus-app (or download it again)"
                .to_string()
        }
        std::cmp::Ordering::Greater => "update the CLI: brew upgrade ligus".to_string(),
        std::cmp::Ordering::Equal => "install the same release of both".to_string(),
    };
    Some(format!(
        "note: the desktop app at {} is {app} but this CLI is {cli}, so they may not agree on features; {fix}",
        path.display()
    ))
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
        searched.add(dir.join("Ligus*.AppImage").display().to_string());
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
        msg.push_str(&format!(
            ", or: {BREW_CASK} (for an app elsewhere, set LIGUS_APP to its .app path)"
        ));
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

/// What `ligus app` did, for its text and `--json` output.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// The app it opened (or would have opened).
    pub path: Option<PathBuf>,
    /// That app's `CFBundleShortVersionString` (macOS).
    pub version: Option<String>,
    /// The app was started or brought forward.
    pub launched: bool,
    /// Linux and Windows: the app was already running, so nothing started.
    pub already_running: bool,
    /// macOS: a copy is already running from another path, so nothing was
    /// opened.
    pub running_from: Option<PathBuf>,
    /// The version note, when the app and this CLI differ.
    pub note: Option<String>,
}

impl Outcome {
    fn json(&self) -> serde_json::Value {
        let cli = env!("CARGO_PKG_VERSION");
        serde_json::json!({
            "schema": "ligus.app.v1",
            "ok": true,
            "path": self.path.as_ref().map(|p| p.display().to_string()),
            "version": self.version,
            "cliVersion": cli,
            "versionMatches": self.version.as_deref().map(|v| v == cli),
            "launched": self.launched,
            "alreadyRunning": self.already_running || self.running_from.is_some(),
            "runningFrom": self.running_from.as_ref().map(|p| p.display().to_string()),
            "note": self.note,
        })
    }
}

/// `ligus app [--json]`.
pub fn cmd_app(args: &[String]) -> Result<()> {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            extra => bail!(
                "`ligus app` takes no arguments but --json (got {extra:?}): it only opens the desktop app"
            ),
        }
    }
    let out = open(&RealHost)?;
    if let Some(note) = &out.note {
        eprintln!("ligus: {note}");
    }
    if let (Some(other), Some(chosen)) = (&out.running_from, &out.path) {
        eprintln!(
            "ligus: {PRODUCT_NAME} is already running from {}; quit it to use {}",
            other.display(),
            chosen.display()
        );
    }
    if json {
        println!("{}", out.json());
    } else if out.already_running {
        // The app has no single-instance handoff, so a second launch would
        // open a second copy on the same data. Its window is already open.
        println!("{PRODUCT_NAME} is already running; switch to its window.");
    } else if out.launched {
        let path = out
            .path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        match &out.version {
            Some(v) => println!("Opened {PRODUCT_NAME} {v} ({path})."),
            None => println!("Opened {PRODUCT_NAME} ({path})."),
        }
    }
    Ok(())
}

/// macOS: what to do, from the lookups alone (no launch): the app to open,
/// its version, the version note, and another running copy if any.
pub fn plan_macos(host: &dyn Host, cli_version: &str) -> Result<Outcome> {
    let mut searched = Searched::default();
    let Some(app) = find_macos(host, cli_version, &mut searched)? else {
        bail!(not_found("macos", &searched));
    };
    let version = host.bundle_info(&app).and_then(|i| i.version);
    Ok(Outcome {
        note: version_note(cli_version, version.as_deref(), &app),
        running_from: running_elsewhere(host, &app),
        version,
        path: Some(app),
        ..Outcome::default()
    })
}

#[cfg(target_os = "macos")]
fn open(host: &dyn Host) -> Result<Outcome> {
    use std::process::{Command, Stdio};
    let mut out = plan_macos(host, env!("CARGO_PKG_VERSION"))?;
    let app = out.path.clone().expect("plan_macos always picks a path");
    if out.running_from.is_some() {
        // Opening by path would only focus the copy that is running (same
        // bundle id): say which one instead of switching to it silently.
        return Ok(out);
    }
    // By path, never by bundle id: Launch Services would pick the highest
    // CFBundleVersion it knows. It starts the app, or brings it forward when
    // it is already running; `open` returns once it has handed it off.
    let ok = Command::new("/usr/bin/open")
        .arg(&app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        bail!(
            "could not open {} (open exited with an error)",
            app.display()
        );
    }
    out.launched = true;
    Ok(out)
}

#[cfg(target_os = "linux")]
fn open(host: &dyn Host) -> Result<Outcome> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    if let Some(problem) = linux_display_problem(host) {
        bail!(problem);
    }
    if running_on_linux(Path::new("/proc")) {
        return Ok(Outcome {
            already_running: true,
            ..Outcome::default()
        });
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
    Ok(Outcome {
        path: Some(app),
        launched: true,
        ..Outcome::default()
    })
}

#[cfg(windows)]
fn open(host: &dyn Host) -> Result<Outcome> {
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
        return Ok(Outcome {
            already_running: true,
            ..Outcome::default()
        });
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
    Ok(Outcome {
        path: Some(app),
        launched: true,
        ..Outcome::default()
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn open(_host: &dyn Host) -> Result<Outcome> {
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
        bundles: HashMap<PathBuf, BundleInfo>,
        spotlight: Vec<PathBuf>,
        running: Vec<PathBuf>,
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
        /// An app bundle on disk (its Info.plist) with this id and version.
        fn bundle(mut self, app: &str, id: &str, version: &str) -> Self {
            let app = PathBuf::from(app);
            self.files.insert(app.join("Contents").join("Info.plist"));
            self.bundles.insert(
                app,
                BundleInfo {
                    id: id.into(),
                    version: Some(version.into()),
                },
            );
            self
        }
        fn spotlight(mut self, app: &str) -> Self {
            self.spotlight.push(app.into());
            self
        }
        fn running(mut self, app: &str) -> Self {
            self.running.push(app.into());
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
        fn bundle_info(&self, app: &Path) -> Option<BundleInfo> {
            self.bundles.get(app).cloned()
        }
        fn spotlight_bundles(&self, id: &str) -> Vec<PathBuf> {
            assert_eq!(id, BUNDLE_ID);
            self.spotlight.clone()
        }
        fn running_bundles(&self) -> Vec<PathBuf> {
            self.running.clone()
        }
    }

    fn mac(h: &FakeHost, cli: &str) -> Option<PathBuf> {
        find_macos(h, cli, &mut Searched::default()).unwrap()
    }

    #[test]
    fn macos_resolves_in_a_fixed_order() {
        let all = || {
            FakeHost::default()
                .var("HOME", "/Users/u")
                .var("LIGUS_APP", "/opt/Ligus.app")
                .bundle("/opt/Ligus.app", BUNDLE_ID, "0.2.1")
                .bundle("/Applications/Ligus.app", BUNDLE_ID, "0.2.1")
                .bundle("/Users/u/Applications/Ligus.app", BUNDLE_ID, "0.2.1")
                .bundle("/Users/u/src/Ligus.app", BUNDLE_ID, "0.2.1")
                .spotlight("/Users/u/src/Ligus.app")
        };
        assert_eq!(mac(&all(), "0.2.1"), Some("/opt/Ligus.app".into()));
        let mut h = all();
        h.vars.remove("LIGUS_APP");
        assert_eq!(mac(&h, "0.2.1"), Some("/Applications/Ligus.app".into()));
        h.files
            .remove(Path::new("/Applications/Ligus.app/Contents/Info.plist"));
        assert_eq!(
            mac(&h, "0.2.1"),
            Some("/Users/u/Applications/Ligus.app".into())
        );
        h.files.remove(Path::new(
            "/Users/u/Applications/Ligus.app/Contents/Info.plist",
        ));
        assert_eq!(mac(&h, "0.2.1"), Some("/Users/u/src/Ligus.app".into()));
    }

    #[test]
    fn macos_installed_app_beats_a_newer_test_build_on_a_dmg() {
        // The bug: Launch Services picked a mounted 2.0.0 test build over
        // the installed 0.2.1.
        let h = FakeHost::default()
            .var("HOME", "/Users/u")
            .bundle("/Volumes/Ligus 2.0.0 3/Ligus.app", BUNDLE_ID, "2.0.0")
            .bundle("/Applications/Ligus.app", BUNDLE_ID, "0.2.1")
            .spotlight("/Volumes/Ligus 2.0.0 3/Ligus.app")
            .spotlight("/Applications/Ligus.app");
        assert_eq!(mac(&h, "0.2.1"), Some("/Applications/Ligus.app".into()));
    }

    #[test]
    fn macos_spotlight_skips_disk_images_and_translocation() {
        let dmg = "/Volumes/Ligus 2.0.0/Ligus.app";
        let tl = "/private/var/folders/x/AppTranslocation/ABCD/d/Ligus.app";
        let h = FakeHost::default()
            .var("HOME", "/Users/u")
            .bundle(dmg, BUNDLE_ID, "2.0.0")
            .bundle(tl, BUNDLE_ID, "0.2.1")
            .spotlight(dmg)
            .spotlight(tl);
        let mut s = Searched::default();
        assert_eq!(find_macos(&h, "0.2.1", &mut s).unwrap(), None);
        let msg = not_found("macos", &s);
        for want in [
            "LIGUS_APP",
            "/Applications/Ligus.app",
            "/Users/u/Applications/Ligus.app",
            "Spotlight",
            BREW_CASK,
        ] {
            assert!(msg.contains(want), "missing {want:?} in {msg}");
        }
        // Named explicitly, a disk image's copy is used.
        let h = h.var("LIGUS_APP", dmg);
        assert_eq!(mac(&h, "0.2.1"), Some(dmg.into()));
        assert!(is_transient_app_path(Path::new(dmg)));
        assert!(is_transient_app_path(Path::new(tl)));
        assert!(!is_transient_app_path(Path::new("/Applications/Ligus.app")));
        assert!(!is_transient_app_path(Path::new("/VolumesX/Ligus.app")));
    }

    #[test]
    fn macos_spotlight_prefers_the_cli_version_then_the_newest() {
        let h = FakeHost::default()
            .bundle("/a/Ligus.app", BUNDLE_ID, "2.0.0")
            .bundle("/b/Ligus.app", BUNDLE_ID, "0.2.1")
            .bundle("/c/Ligus.app", BUNDLE_ID, "0.10.0")
            .bundle("/d/Other.app", "com.example.other", "9.0.0")
            .spotlight("/a/Ligus.app")
            .spotlight("/b/Ligus.app")
            .spotlight("/c/Ligus.app")
            .spotlight("/d/Other.app");
        assert_eq!(mac(&h, "0.2.1"), Some("/b/Ligus.app".into()));
        assert_eq!(mac(&h, "0.3.0"), Some("/a/Ligus.app".into()));
        assert!(version_key("0.10.0") > version_key("0.9.1"));
        assert_eq!(version_key("0.2.1-rc.1"), vec![0, 2, 1]);
    }

    #[test]
    fn macos_bad_ligus_app_is_an_error() {
        let h = FakeHost::default().var("LIGUS_APP", "/nope/Ligus.app");
        let err = find_macos(&h, "0.2.1", &mut Searched::default())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("LIGUS_APP") && err.contains("/nope/Ligus.app"),
            "{err}"
        );
    }

    #[test]
    fn version_note_names_the_older_side() {
        let p = Path::new("/Applications/Ligus.app");
        assert_eq!(version_note("0.2.1", Some("0.2.1"), p), None);
        assert_eq!(version_note("0.2.1", None, p), None);
        let n = version_note("0.2.1", Some("0.1.1"), p).unwrap();
        assert!(
            n.contains("0.1.1") && n.contains("0.2.1") && n.contains("/Applications/Ligus.app"),
            "{n}"
        );
        assert!(n.contains("brew upgrade --cask --greedy ligus-app"), "{n}");
        let n = version_note("0.2.1", Some("2.0.0"), p).unwrap();
        assert!(
            n.contains("brew upgrade ligus") && !n.contains("--cask"),
            "{n}"
        );
        assert!(!n.contains('\n'), "one line: {n}");
    }

    #[test]
    fn macos_plan_reports_a_copy_running_elsewhere() {
        let base = || {
            FakeHost::default()
                .var("HOME", "/Users/u")
                .bundle("/Applications/Ligus.app", BUNDLE_ID, "0.1.1")
                .bundle("/Volumes/Ligus 2.0.0/Ligus.app", BUNDLE_ID, "2.0.0")
                .bundle("/Applications/Safari.app", "com.apple.Safari", "18.0")
        };
        let plan = plan_macos(
            &base()
                .running("/Applications/Safari.app")
                .running("/Volumes/Ligus 2.0.0/Ligus.app"),
            "0.2.1",
        )
        .unwrap();
        assert_eq!(plan.path, Some("/Applications/Ligus.app".into()));
        assert_eq!(plan.version.as_deref(), Some("0.1.1"));
        assert_eq!(
            plan.running_from,
            Some("/Volumes/Ligus 2.0.0/Ligus.app".into())
        );
        assert!(plan.note.unwrap().contains("--cask"));
        assert!(!plan.launched);
        // Running from the chosen path (or not at all): nothing to report.
        let plan = plan_macos(&base().running("/Applications/Ligus.app"), "0.2.1").unwrap();
        assert_eq!(plan.running_from, None);
        let plan = plan_macos(&base().running("/Applications/Safari.app"), "0.2.1").unwrap();
        assert_eq!(plan.running_from, None);
    }

    #[test]
    fn finds_the_bundle_of_a_running_executable() {
        assert_eq!(
            bundle_of_executable("/Volumes/Ligus 2.0.0/Ligus.app/Contents/MacOS/ligus-desktop\n"),
            Some("/Volumes/Ligus 2.0.0/Ligus.app".into())
        );
        assert_eq!(bundle_of_executable("/usr/sbin/cfprefsd"), None);
    }

    #[test]
    fn outcome_json_carries_the_path_and_version() {
        let o = Outcome {
            path: Some("/Applications/Ligus.app".into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            launched: true,
            ..Outcome::default()
        };
        let j = o.json();
        assert_eq!(j["schema"], "ligus.app.v1");
        assert_eq!(j["path"], "/Applications/Ligus.app");
        assert_eq!(j["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(j["versionMatches"], true);
        assert_eq!(j["alreadyRunning"], false);
        assert!(j["runningFrom"].is_null());
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
        // Joined like the lookup joins them (`\` separators when the tests
        // run on Windows).
        let home = Path::new("/home/u");
        let apps = home.join("Applications").join("Ligus*.AppImage");
        let bin = home.join(".local").join("bin").join("Ligus*.AppImage");
        let (apps, bin) = (apps.display().to_string(), bin.display().to_string());
        for want in [
            "ligus-desktop on PATH",
            "LIGUS_APPIMAGE",
            &apps,
            &bin,
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
