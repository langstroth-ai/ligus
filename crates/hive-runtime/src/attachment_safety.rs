//! Receiving a shared file safely (issue #118): the device-side checks the
//! relay can't make (it sees only ciphertext).
//!
//! * [`archive_bomb`]: a zip's central directory is read without
//!   decompressing anything; a gzip stream is inflated into a counter (never
//!   to disk), stopping at a cap. Nothing is ever extracted.
//! * [`save_received`]: writes a verified file where the person chose, never
//!   over an existing file, never executable, with the OS quarantine marking
//!   ([`mark_quarantined`]: macOS `com.apple.quarantine`, Windows
//!   Mark-of-the-Web) so Gatekeeper and SmartScreen apply when it is opened.
//! * [`Scanner`]: an optional client-side scanning hook (ClamAV, or any
//!   command the person configures) run on the verified file before it is
//!   saved. Its result is shown; an infected verdict refuses the save.
//!   Nothing is uploaded anywhere.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hive_core::attachment::{self, ArchiveLimits, Assessment};

/// Why an archive looks like a decompression bomb, if it does.
pub fn archive_bomb(bytes: &[u8], a: &Assessment) -> Option<String> {
    archive_report(bytes, a).and_then(|r| r.bomb)
}

/// An archive's inspection (zip directories and gzip streams; other archive
/// formats are flagged risky by type but not opened).
pub fn archive_report(bytes: &[u8], a: &Assessment) -> Option<attachment::ArchiveReport> {
    let limits = ArchiveLimits::default();
    match a.detected.ext {
        "zip" | "jar" | "apk" | "ipa" | "docx" | "xlsx" | "pptx" | "docm" | "xlsm" | "pptm" | "odt" | "ods" | "epub" => {
            let mut budget = inflate_cap(bytes.len(), &limits);
            Some(zip_report(bytes, &limits, 0, &mut budget))
        }
        "gz" => Some(gzip_report(bytes, &limits)),
        _ => None,
    }
}

/// Archives inside archives this inspection opens (deeper is a bomb).
const MAX_ZIP_DEPTH: usize = 3;
/// The largest inner archive kept in memory to look inside.
const MAX_INNER_ARCHIVE: usize = 32 * 1024 * 1024;

/// How many bytes an archive of `len` bytes may unpack to, in all.
fn inflate_cap(len: usize, limits: &ArchiveLimits) -> u64 {
    limits.max_total.min(len as u64 * limits.max_ratio).max(1024 * 1024)
}

/// A zip's directory ([`attachment::inspect_zip`]) checked against what an
/// extractor would actually unpack (second review): every stored or deflated
/// entry is inflated into a counter (nothing is written anywhere), bounded by
/// `budget`; an entry that unpacks to more than its directory claims, or a
/// zip that unpacks past the cap, is a bomb; a zip or gzip inside (whatever
/// its name) is inspected the same way, at most [`MAX_ZIP_DEPTH`] deep, and
/// what it holds counts for the policy.
fn zip_report(bytes: &[u8], limits: &ArchiveLimits, depth: usize, budget: &mut u64) -> attachment::ArchiveReport {
    let mut r = attachment::inspect_zip(bytes, limits);
    if r.bomb.is_some() {
        return r;
    }
    let Ok(listing) = attachment::zip_entries(bytes) else { return r };
    for e in &listing.entries {
        if e.name.ends_with('/') && e.uncompressed == 0 {
            continue;
        }
        let Some(start) = attachment::zip_local_data(bytes, e) else {
            r.bomb = Some("its directory doesn't match its contents".into());
            return r;
        };
        if e.flags & 1 != 0 || !matches!(e.method, 0 | 8) {
            r.opaque_entries += 1;
            continue;
        }
        let data = &bytes[start..];
        let mut src: Box<dyn Read> = if e.method == 0 {
            let n = usize::try_from(e.compressed).unwrap_or(usize::MAX).min(data.len());
            Box::new(&data[..n])
        } else {
            Box::new(flate2::read::DeflateDecoder::new(data))
        };
        let mut src = (&mut src).take(*budget + 1);
        let mut buf = [0u8; 64 * 1024];
        let mut got: u64 = 0;
        let mut kept: Option<Vec<u8>> = None;
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if got == 0 {
                        let head = &buf[..n];
                        if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") || head.starts_with(&[0x1F, 0x8B]) {
                            kept = Some(Vec::new());
                        }
                    }
                    if let Some(k) = kept.as_mut() {
                        if k.len() + n > MAX_INNER_ARCHIVE {
                            kept = None;
                            r.opaque_entries += 1;
                        } else {
                            k.extend_from_slice(&buf[..n]);
                        }
                    }
                    got += n as u64;
                }
                Err(_) => {
                    r.bomb = Some("a corrupt compressed entry".into());
                    return r;
                }
            }
        }
        if got > *budget {
            r.bomb = Some(format!("it expands more than {}x", limits.max_ratio));
            return r;
        }
        *budget -= got;
        if got > e.uncompressed {
            r.bomb = Some("an entry unpacks to more than its directory says".into());
            return r;
        }
        let Some(inner) = kept else { continue };
        if depth + 1 > MAX_ZIP_DEPTH {
            r.bomb = Some(format!("archives nested more than {MAX_ZIP_DEPTH} deep"));
            return r;
        }
        let child = if inner.starts_with(&[0x1F, 0x8B]) {
            let g = gzip_report(&inner, limits);
            *budget = budget.saturating_sub(g.total_uncompressed);
            g
        } else {
            zip_report(&inner, limits, depth + 1, budget)
        };
        merge_inner(&mut r, &e.name, child);
        if r.bomb.is_some() {
            return r;
        }
    }
    r
}

/// Fold an inner archive's findings into its container's.
fn merge_inner(r: &mut attachment::ArchiveReport, name: &str, child: attachment::ArchiveReport) {
    r.nested_archives += 1 + child.nested_archives;
    r.risky_entries += child.risky_entries;
    r.opaque_entries += child.opaque_entries;
    r.hidden_entries |= child.hidden_entries;
    r.total_uncompressed = r.total_uncompressed.saturating_add(child.total_uncompressed);
    for t in child.entry_types {
        if r.entry_types.len() < 256 && !r.entry_types.contains(&t) {
            r.entry_types.push(t);
        }
    }
    let base = attachment::sanitize_filename(name);
    for u in child.unsafe_names {
        if r.unsafe_names.len() < 20 {
            r.unsafe_names.push(format!("{base}: {u}"));
        }
    }
    if let Some(b) = child.bomb {
        r.bomb = r.bomb.take().or(Some(format!("{b} (inside {base})")));
    }
}

/// Inflate a gzip stream into a counter, at most `max_total` bytes; a stream
/// that expands past the cap or the ratio is a bomb.
fn gzip_report(bytes: &[u8], limits: &ArchiveLimits) -> attachment::ArchiveReport {
    let mut r = attachment::ArchiveReport { entries: 1, ..Default::default() };
    let cap = limits.max_total.min(bytes.len() as u64 * limits.max_ratio).max(1024 * 1024);
    let mut d = flate2::read::MultiGzDecoder::new(bytes).take(cap + 1);
    let mut buf = [0u8; 64 * 1024];
    loop {
        match d.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => r.total_uncompressed += n as u64,
            Err(_) => {
                r.bomb = r.bomb.or(Some("a corrupt compressed stream".into()));
                break;
            }
        }
    }
    if r.total_uncompressed > cap {
        r.bomb = Some(format!("it expands more than {}x", limits.max_ratio));
    }
    r
}

/// The quarantine origin recorded on saved files.
pub const QUARANTINE_AGENT: &str = "Ligus";

/// Mark `path` as downloaded from elsewhere, so the OS checks it on open:
/// macOS `com.apple.quarantine` (Gatekeeper), Windows `Zone.Identifier`
/// (Mark-of-the-Web, SmartScreen, Office Protected View). Returns whether a
/// marking was written (other systems have no such marking).
pub fn mark_quarantined(path: &Path, sender: &str) -> std::io::Result<bool> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        // flags;hex timestamp;agent;event id. 0x0081 = downloaded + user
        // approval needed (what browsers set); 0x0002 marks it as coming from
        // another person.
        let value = format!("0083;{now:08x};{QUARANTINE_AGENT};{}", uuid::Uuid::new_v4().to_string().to_uppercase());
        let _ = sender;
        let p = CString::new(path.as_os_str().as_bytes()).map_err(|_| std::io::Error::other("path contains NUL"))?;
        let name = CString::new("com.apple.quarantine").expect("static");
        // SAFETY: valid NUL-terminated strings and a buffer of `value.len()` bytes.
        let rc = unsafe {
            libc::setxattr(p.as_ptr(), name.as_ptr(), value.as_ptr() as *const libc::c_void, value.len(), 0, libc::XATTR_NOFOLLOW)
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(true)
    }
    #[cfg(windows)]
    {
        let mut ads = path.as_os_str().to_owned();
        ads.push(":Zone.Identifier");
        let referrer: String = sender.chars().filter(|c| c.is_ascii_alphanumeric() || *c == ' ').take(64).collect();
        std::fs::write(PathBuf::from(ads), format!("[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=ligus:{referrer}\r\n"))?;
        Ok(true)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (path, sender);
        Ok(false)
    }
}

/// [`mark_quarantined`] on an open file (macOS: `fsetxattr` on its
/// descriptor).
fn mark_quarantined_file(f: &std::fs::File, path: &Path, sender: &str) -> std::io::Result<bool> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::io::AsRawFd;
        let _ = (path, sender);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let value = format!("0083;{now:08x};{QUARANTINE_AGENT};{}", uuid::Uuid::new_v4().to_string().to_uppercase());
        let name = CString::new("com.apple.quarantine").expect("static");
        // SAFETY: a valid descriptor, a NUL-terminated name and a buffer of `value.len()` bytes.
        let rc = unsafe { libc::fsetxattr(f.as_raw_fd(), name.as_ptr(), value.as_ptr() as *const libc::c_void, value.len(), 0, 0) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = f;
        mark_quarantined(path, sender)
    }
}

/// The quarantine marking on `path`, if any (macOS; tests and diagnostics).
pub fn quarantine_of(path: &Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let p = CString::new(path.as_os_str().as_bytes()).ok()?;
        let name = CString::new("com.apple.quarantine").ok()?;
        let mut buf = vec![0u8; 512];
        // SAFETY: valid NUL-terminated strings and a writable buffer of its length.
        let n = unsafe { libc::getxattr(p.as_ptr(), name.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0, libc::XATTR_NOFOLLOW) };
        if n < 0 {
            return None;
        }
        buf.truncate(n as usize);
        String::from_utf8(buf).ok()
    }
    #[cfg(windows)]
    {
        let mut ads = path.as_os_str().to_owned();
        ads.push(":Zone.Identifier");
        std::fs::read_to_string(PathBuf::from(ads)).ok()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = path;
        None
    }
}

/// Write a verified received file into `dir` under its cleaned `name`: a new
/// file only (never over an existing one or through a symlink; `name (2).ext`
/// when taken), not executable, and quarantine-marked. Returns the path.
pub fn save_received(dir: &Path, name: &str, bytes: &[u8], sender: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let clean = attachment::sanitize_filename(name);
    let ext = attachment::extension(&clean);
    let stem = if ext.is_empty() { clean.clone() } else { clean[..clean.len() - ext.len() - 1].to_string() };
    for n in 1..1000 {
        let candidate = if n == 1 { clean.clone() } else if ext.is_empty() { format!("{stem} ({n})") } else { format!("{stem} ({n}).{ext}") };
        let path = dir.join(&candidate);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o644);
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let mut f = match opts.open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        // Mark the open file before writing a byte (by its descriptor, so a
        // swapped path can't take the mark), so no window exists where the
        // file is complete but unmarked.
        if let Err(e) = mark_quarantined_file(&f, &path, sender) {
            drop(f);
            let _ = std::fs::remove_file(&path);
            return Err(std::io::Error::other(format!("couldn't mark the file as downloaded: {e}")));
        }
        let written = f.write_all(bytes).and_then(|_| f.sync_all());
        if let Err(e) = written {
            drop(f);
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        return Ok(path);
    }
    Err(std::io::Error::other("too many files with that name"))
}

/// Read `rel` inside `root` for an agent's file offer, safely against a
/// sandboxed agent racing symlinks: the file is opened without following a
/// symlink as its last component, then the path of the very file that was
/// opened (`F_GETPATH` on macOS, `/proc/self/fd` on Linux) must lie inside
/// `root`, and the bytes are read from that open file. Returns the bytes and
/// the file's name.
pub fn read_inside(root: &Path, rel: &str, max_bytes: u64) -> Result<(Vec<u8>, String), String> {
    let rel_path = Path::new(rel);
    if rel.is_empty() || rel_path.is_absolute() {
        return Err("give a path relative to your worktree".into());
    }
    let root = std::fs::canonicalize(root).map_err(|_| "this turn's worktree is gone".to_string())?;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = opts.open(root.join(rel_path)).map_err(|_| format!("no such regular file in your worktree: {rel}"))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("you can only offer a regular file inside your worktree".into());
    }
    let real = opened_path(&file).ok_or("this host can't confirm where the file is, so it can't be offered")?;
    if !real.starts_with(&root) {
        return Err("you can only offer a regular file inside your worktree".into());
    }
    if meta.len() > max_bytes {
        return Err("the file is larger than this workspace allows".into());
    }
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes).map_err(|e| format!("couldn't read the file: {e}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err("the file is larger than this workspace allows".into());
    }
    let name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok((bytes, name))
}

/// The path of an open file, from the kernel.
fn opened_path(f: &std::fs::File) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::AsRawFd;
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes at most PATH_MAX bytes into buf.
        let rc = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
        if rc == -1 {
            return None;
        }
        let end = buf.iter().position(|b| *b == 0)?;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..end])))
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        std::fs::read_link(format!("/proc/self/fd/{}", f.as_raw_fd())).ok()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = f;
        None
    }
}

/// Why a file must not leave this device, if it holds a credential: one of
/// this device's own secrets, a private key, or a well-known credential or
/// environment file.
pub fn credential_in(name: &str, bytes: &[u8]) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let ext = attachment::extension(&lower);
    let named = lower == ".env"
        || lower.starts_with(".env.")
        || lower.ends_with(".env")
        || matches!(lower.as_str(), "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519" | ".netrc" | ".npmrc" | ".pypirc" | "credentials" | ".git-credentials" | "known_hosts")
        || matches!(ext.as_str(), "pem" | "key" | "p12" | "pfx" | "jks" | "keystore" | "kdbx" | "ppk" | "asc" | "gpg");
    if named {
        return Some("this looks like a credential or key file; it can't be offered".into());
    }
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4 * 1024 * 1024)]);
    if text.contains("PRIVATE KEY-----") || text.contains("PRIVATE KEY BLOCK-----") {
        return Some("the file contains a private key; it can't be offered".into());
    }
    if crate::secret_redact::redacted(&text) != text {
        return Some("the file contains a credential of this device; it can't be offered".into());
    }
    static PATTERNS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = PATTERNS.get_or_init(|| {
        regex::Regex::new(r"(AKIA|ASIA)[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{40,}|xox[baprs]-[A-Za-z0-9-]{10,}|sk-[A-Za-z0-9_-]{32,}|(?i)aws_secret_access_key\s*=")
            .expect("static regex")
    });
    re.is_match(&text).then(|| "the file contains what looks like an access token; it can't be offered".into())
}

/// A scanner's verdict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "verdict", rename_all = "camelCase")]
pub enum ScanResult {
    /// No scanner configured (nothing ran).
    NotConfigured,
    Clean { scanner: String },
    /// The scanner found something; the save is refused.
    Infected { scanner: String, detail: String },
    /// The scanner failed to run or gave no verdict (shown; not a block).
    Failed { scanner: String, detail: String },
}

/// An optional client-side scanning hook: a program run on the verified
/// file with the path as its last argument, no shell. Exit 0 = clean, 1 =
/// infected (ClamAV's convention), anything else = failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scanner {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl Scanner {
    /// The configured scanner: `LIGUS_ATTACHMENT_SCANNER` names a program
    /// (with `LIGUS_ATTACHMENT_SCANNER_ARGS`, whitespace-separated), or
    /// `auto` for ClamAV's `clamdscan` or `clamscan` on the PATH. Unset =
    /// none.
    pub fn from_env() -> Option<Scanner> {
        let v = std::env::var("LIGUS_ATTACHMENT_SCANNER").ok().filter(|v| !v.trim().is_empty())?;
        let timeout = Duration::from_secs(120);
        if v.trim() == "auto" {
            for (bin, args) in [("clamdscan", vec!["--no-summary", "--fdpass"]), ("clamscan", vec!["--no-summary"])] {
                if let Some(p) = find_on_path(bin) {
                    return Some(Scanner { program: p, args: args.into_iter().map(String::from).collect(), timeout });
                }
            }
            return None;
        }
        let args = std::env::var("LIGUS_ATTACHMENT_SCANNER_ARGS").unwrap_or_default().split_whitespace().map(String::from).collect();
        Some(Scanner { program: PathBuf::from(v.trim()), args, timeout })
    }

    fn name(&self) -> String {
        self.program.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "scanner".into())
    }

    /// Run the scanner on `file` (blocking, bounded by the timeout).
    pub fn scan(&self, file: &Path) -> ScanResult {
        let scanner = self.name();
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(&self.args).arg(file).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ScanResult::Failed { scanner, detail: format!("couldn't start: {e}") },
        };
        let start = std::time::Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() > self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ScanResult::Failed { scanner, detail: "timed out".into() };
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return ScanResult::Failed { scanner, detail: e.to_string() },
            }
        };
        let mut out = String::new();
        if let Some(mut s) = child.stdout.take() {
            let _ = s.by_ref().take(4096).read_to_string(&mut out);
        }
        let detail: String = out.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().filter(|c| !c.is_control()).take(200).collect();
        match status.code() {
            Some(0) => ScanResult::Clean { scanner },
            Some(1) => ScanResult::Infected { scanner, detail },
            other => ScanResult::Failed { scanner, detail: format!("exit {other:?} {detail}") },
        }
    }
}

fn find_on_path(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(bin)).find(|c| c.is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gzip_bomb_is_caught_without_writing_anything() {
        use std::io::Write;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let zeros = vec![0u8; 1024 * 1024];
        for _ in 0..200 {
            e.write_all(&zeros).unwrap();
        }
        let bomb = e.finish().unwrap();
        assert!(bomb.len() < 1024 * 1024);
        let a = attachment::assess("logs.gz", &bomb);
        assert!(archive_bomb(&bomb, &a).is_some());
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b"just a little text\n").unwrap();
        let small = e.finish().unwrap();
        assert!(archive_bomb(&small, &attachment::assess("a.gz", &small)).is_none());
        // Corrupt stream: refused.
        let mut bad = small.clone();
        bad.truncate(12);
        bad.extend_from_slice(&[0xff; 32]);
        assert!(archive_bomb(&bad, &attachment::assess("a.gz", &bad)).is_some());
    }

    /// One zip entry for [`build_zip`]: its name, its real bytes, whether
    /// they are deflated, the uncompressed size the headers claim (`None` =
    /// the truth), and an Info-ZIP Unicode Path name (extra field 0x7075).
    struct Ent<'a> {
        name: &'a str,
        data: Vec<u8>,
        deflate: bool,
        claim: Option<u32>,
        upath: Option<&'a str>,
    }

    fn ent(name: &str, data: Vec<u8>) -> Ent<'_> {
        Ent { name, data, deflate: true, claim: None, upath: None }
    }

    /// A real zip (local headers with data, the central directory, the end
    /// record), written by hand so a test can lie in it.
    fn build_zip(entries: &[Ent]) -> Vec<u8> {
        use std::io::Write;
        let crc32 = |b: &[u8]| {
            let mut c = flate2::Crc::new();
            c.update(b);
            c.sum()
        };
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for e in entries {
            let packed = if e.deflate {
                let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
                z.write_all(&e.data).unwrap();
                z.finish().unwrap()
            } else {
                e.data.clone()
            };
            let crc = crc32(&e.data);
            let uncomp = e.claim.unwrap_or(e.data.len() as u32);
            let extra = match e.upath {
                Some(u) => {
                    let mut x = Vec::new();
                    x.extend_from_slice(&0x7075u16.to_le_bytes());
                    x.extend_from_slice(&((5 + u.len()) as u16).to_le_bytes());
                    x.push(1);
                    x.extend_from_slice(&crc32(e.name.as_bytes()).to_le_bytes());
                    x.extend_from_slice(u.as_bytes());
                    x
                }
                None => Vec::new(),
            };
            let method: u16 = if e.deflate { 8 } else { 0 };
            let off = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            out.extend_from_slice(&uncomp.to_le_bytes());
            out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            out.extend_from_slice(e.name.as_bytes());
            out.extend_from_slice(&extra);
            out.extend_from_slice(&packed);
            cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            cd.extend_from_slice(&20u16.to_le_bytes());
            cd.extend_from_slice(&20u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&method.to_le_bytes());
            cd.extend_from_slice(&[0; 4]);
            cd.extend_from_slice(&crc.to_le_bytes());
            cd.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            cd.extend_from_slice(&uncomp.to_le_bytes());
            cd.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            cd.extend_from_slice(&[0; 10]);
            cd.extend_from_slice(&off.to_le_bytes());
            cd.extend_from_slice(e.name.as_bytes());
            cd.extend_from_slice(&extra);
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&[0; 2]);
        out
    }

    fn report(bytes: &[u8]) -> attachment::ArchiveReport {
        archive_report(bytes, &attachment::assess("a.zip", bytes)).expect("a zip is inspected")
    }

    /// Second review: what an extractor would actually unpack is what the
    /// inspection judges.
    #[test]
    fn zip_inspection_sees_what_an_extractor_would() {
        // Honest: a small text file, nothing flagged.
        let ok = report(&build_zip(&[ent("notes.txt", b"hello".repeat(100))]));
        assert!(ok.bomb.is_none() && ok.risky_entries == 0, "{ok:?}");
        // The Unicode Path field (0x7075) renames the entry for Info-ZIP,
        // 7-Zip and libarchive: the policy must see that name too.
        let renamed = report(&build_zip(&[Ent { upath: Some("evil.exe"), ..ent("notes.txt", b"MZ".to_vec()) }]));
        assert!(renamed.entry_types.contains(&"@executable".to_string()) && renamed.risky_entries == 1, "{renamed:?}");
        // A directory that understates what an entry unpacks to: 64 MiB of
        // zeros claimed as 100 bytes.
        let lying = report(&build_zip(&[Ent { claim: Some(100), ..ent("zeros.bin", vec![0u8; 64 * 1024 * 1024]) }]));
        assert!(lying.bomb.is_some(), "{lying:?}");
        // A program inside a zip inside the zip reaches the policy.
        let inner = build_zip(&[ent("bin/tool.exe", b"MZ\x90\0".to_vec())]);
        let nested = report(&build_zip(&[Ent { deflate: false, ..ent("inner.zip", inner.clone()) }]));
        assert!(nested.entry_types.contains(&"@executable".to_string()), "{nested:?}");
        // ... also when the inner zip is named like a document.
        let disguised = report(&build_zip(&[ent("report.txt", inner)]));
        assert!(disguised.entry_types.contains(&"@executable".to_string()), "{disguised:?}");
        // Zips nested deeper than three levels are refused.
        let mut z = build_zip(&[ent("x.exe", b"MZ".to_vec())]);
        for i in 0..5 {
            z = build_zip(&[Ent { deflate: false, ..ent(["a.zip", "b.zip", "c.zip", "d.zip", "e.zip"][i], z) }]);
        }
        assert!(report(&z).bomb.is_some(), "deep nesting");
    }

    #[test]
    fn saved_files_are_new_not_executable_and_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let p = save_received(dir.path(), "../../report.pdf", b"%PDF-1.4", "Mara").unwrap();
        assert_eq!(p.parent(), Some(dir.path()));
        assert_eq!(p.file_name().unwrap(), "report.pdf");
        let p2 = save_received(dir.path(), "report.pdf", b"%PDF-1.4 two", "Mara").unwrap();
        assert_eq!(p2.file_name().unwrap(), "report (2).pdf");
        assert_eq!(std::fs::read(&p).unwrap(), b"%PDF-1.4", "never written over");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0, "a received file is never executable");
            // A symlink planted at the name is never followed.
            let target = dir.path().join("target.txt");
            std::fs::write(&target, b"keep").unwrap();
            std::os::unix::fs::symlink(&target, dir.path().join("link.txt")).unwrap();
            let p3 = save_received(dir.path(), "link.txt", b"evil", "Mara").unwrap();
            assert_eq!(p3.file_name().unwrap(), "link (2).txt");
            assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        }
        #[cfg(target_os = "macos")]
        {
            let q = quarantine_of(&p).expect("com.apple.quarantine is set");
            assert!(q.starts_with("0083;") && q.contains(";Ligus;"), "{q}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn offered_files_stay_inside_the_worktree_and_hold_no_credentials() {
        let wt = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(wt.path().join("report.md"), b"# ok").unwrap();
        std::fs::write(outside.path().join("id_ed25519"), b"-----BEGIN OPENSSH PRIVATE KEY-----").unwrap();
        std::fs::create_dir(wt.path().join("sub")).unwrap();
        std::fs::write(outside.path().join("notes.txt"), b"outside").unwrap();
        std::os::unix::fs::symlink(outside.path().join("notes.txt"), wt.path().join("link.txt")).unwrap();
        std::os::unix::fs::symlink(outside.path(), wt.path().join("dirlink")).unwrap();
        let (b, n) = read_inside(wt.path(), "report.md", 100).unwrap();
        assert_eq!((b.as_slice(), n.as_str()), (&b"# ok"[..], "report.md"));
        for bad in ["link.txt", "dirlink/notes.txt", "../x", "/etc/hosts", "sub", "nope", "sub/../../x"] {
            assert!(read_inside(wt.path(), bad, 100).is_err(), "{bad}");
        }
        assert!(read_inside(wt.path(), "report.md", 2).is_err(), "size cap");
        assert!(credential_in("id_ed25519", b"x").is_some());
        assert!(credential_in(".env.production", b"X=1").is_some());
        assert!(credential_in("notes.txt", b"-----BEGIN RSA PRIVATE KEY-----\nabc").is_some());
        assert!(credential_in("notes.txt", b"key AKIAABCDEFGHIJKLMNOP here").is_some());
        assert!(credential_in("notes.txt", b"plain notes").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_scanner_hook_reports_and_never_uses_a_shell() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
            p
        };
        let file = dir.path().join("x; touch pwned");
        std::fs::write(&file, b"data").unwrap();
        let clean = Scanner { program: script("clean.sh", "exit 0"), args: vec![], timeout: Duration::from_secs(10) };
        assert!(matches!(clean.scan(&file), ScanResult::Clean { .. }));
        let infected = Scanner { program: script("inf.sh", "echo \"$1: Eicar-Test-Signature FOUND\"; exit 1"), args: vec![], timeout: Duration::from_secs(10) };
        match infected.scan(&file) {
            ScanResult::Infected { detail, .. } => assert!(detail.contains("FOUND"), "{detail}"),
            other => panic!("{other:?}"),
        }
        let broken = Scanner { program: script("err.sh", "exit 2"), args: vec![], timeout: Duration::from_secs(10) };
        assert!(matches!(broken.scan(&file), ScanResult::Failed { .. }));
        let slow = Scanner { program: script("slow.sh", "sleep 5"), args: vec![], timeout: Duration::from_millis(200) };
        assert!(matches!(slow.scan(&file), ScanResult::Failed { .. }));
        let missing = Scanner { program: dir.path().join("nope"), args: vec![], timeout: Duration::from_secs(1) };
        assert!(matches!(missing.scan(&file), ScanResult::Failed { .. }));
        assert!(!dir.path().join("pwned").exists(), "the path never reached a shell");
    }
}
