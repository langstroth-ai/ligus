//! Bearer token + pairing for `ligus serve`.
//!
//! The token is 32 random bytes, hex-encoded, kept in a 0600 file
//! (`<LIGUS_DATA_DIR>/serve.token` by default) and generated on first run. The
//! phone gets it once, through the pairing link
//! `ligus://pair?url=<base64url(serverURL)>&token=<token>` shown as a terminal
//! QR code by `ligus serve --pair`, and keeps it in the iOS Keychain.

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use anyhow::{bail, Context, Result};
use base64::Engine;

/// Read the token at `path`, creating it (0600, 32 random bytes as hex) when
/// the file is missing or empty.
pub fn load_or_create_token(path: &Path) -> Result<String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let t = existing.trim().to_string();
        if !t.is_empty() {
            if t.len() < 32 {
                bail!("{} holds a token shorter than 32 characters; delete it to generate a new one", path.display());
            }
            ligus_runtime::secret_redact::register(&t);
            return Ok(t);
        }
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no OS randomness for the serve token: {e}"))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).with_context(|| format!("creating {}", path.display()))?;
    f.write_all(token.as_bytes())?;
    #[cfg(unix)]
    {
        // The mode above only applies on create; tighten a pre-existing empty file too.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    ligus_runtime::secret_redact::register(&token);
    Ok(token)
}

/// Replace the token at `path` with a fresh one (0600). `ligus phones revoke`
/// does this by default, because a stolen phone holds the token too; a
/// running `ligus serve` picks the new one up within seconds.
pub fn rotate_token(path: &Path) -> Result<String> {
    // Written beside it and renamed over it: a reader never sees it empty.
    let tmp = path.with_extension("rotating");
    let _ = std::fs::remove_file(&tmp);
    let token = load_or_create_token(&tmp)?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(token)
}

/// Read the token `ligus serve` already created at `path`. `ligus pair` must
/// not mint one: a fresh token in the wrong data dir pairs the phone with a
/// token the running server has never seen.
pub fn load_existing_token(path: &Path) -> Result<String> {
    let t = std::fs::read_to_string(path).map(|s| s.trim().to_string()).unwrap_or_default();
    if t.is_empty() {
        bail!(
            "no serve token at {}: start `ligus serve` first, or point LIGUS_DATA_DIR / --token-file at the server's data dir",
            path.display()
        );
    }
    Ok(t)
}

/// Constant-time token comparison (no early exit on the first differing byte).
pub fn token_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `ligus://pair?url=<base64url(serverURL), no padding>&token=<token>`. The
/// scheme comes from `ligus_core::names::DEEP_LINK_SCHEME`; phones accept
/// `ligus://` too.
pub fn pairing_url(server_url: &str, token: &str) -> String {
    let url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(server_url.trim_end_matches('/').as_bytes());
    ligus_core::names::deep_link(&format!("pair?url={url}&token={token}"))
}

/// Whether `ip` is an address the API may listen on without an explicit
/// override: loopback, a private LAN range, or the tailnet (Tailscale's
/// 100.64.0.0/10 and fd7a:115c:a1e0::/48). Never the unspecified address.
pub fn bind_allowed(ip: IpAddr) -> bool {
    !ip.is_unspecified() && ligus_core::endpoint::host_is_private(&ip.to_string())
}

/// The base URL the phone should use for a server listening on `addr`.
pub fn default_server_url(addr: SocketAddr) -> String {
    format!("http://{addr}")
}

/// Whether a phone could never reach `server_url` because it names the
/// loopback interface (on the phone that is the phone itself).
pub fn is_loopback_url(server_url: &str) -> bool {
    let rest = server_url.split_once("://").map_or(server_url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or("");
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Which pairing link to print.
#[derive(Debug, PartialEq, Eq)]
pub enum PairPlan {
    /// The direct link (with the relay's v2 fields too when remote access is on).
    Direct,
    /// A relay-only link: no direct URL or token in it.
    RelayOnly,
}

/// Decide the pairing link for a server at `server_url`. A loopback URL is
/// the phone itself on the phone, so it is never offered: with remote access
/// the link goes relay-only, and without it there is no link to print, only
/// the reason and the ways to get one (the `Err` text).
pub fn pair_plan(server_url: &str, remote: bool, relay_only: bool) -> Result<PairPlan, String> {
    const ENABLE: &str = "`ligus remote enable --relay <url>`";
    if relay_only && !remote {
        return Err(format!("--relay-only needs remote access: turn it on with {ENABLE} first"));
    }
    if relay_only || (remote && is_loopback_url(server_url)) {
        return Ok(PairPlan::RelayOnly);
    }
    if is_loopback_url(server_url) {
        return Err(format!(
            "no pairing link: this server is at {server_url}, which on a phone is the phone itself, and remote access is off. \
             Either turn on remote access with {ENABLE} and pair again, \
             or listen on a LAN or tailnet address (--listen <ip:port> or LIGUS_SERVE_LISTEN) \
             or set the address the phone should use (--public-url <url> or LIGUS_SERVE_PUBLIC_URL)."
        ));
    }
    Ok(PairPlan::Direct)
}

/// A one-line hint for pairing a phone to a box: the box is its own service
/// member (Phase B), so what the phone sends through it is the box's.
/// `None` for a revoked box.
pub fn member_hint(standing: &ligus_runtime::device_enroll::Standing) -> Option<&'static str> {
    matches!(standing, ligus_runtime::device_enroll::Standing::Own).then_some(
        "Note: this box is its own service member, so the phone's messages show under the box's name, \
         and its votes never count as a person's approval.",
    )
}

/// Render `text` as a QR code for a terminal: two module rows per text line
/// with half-block characters, explicit black-on-white ANSI colors (so it
/// scans on dark and light terminal themes alike) and a 2-module quiet zone.
pub fn render_qr(text: &str) -> Result<String> {
    use qrcodegen::{QrCode, QrCodeEcc};
    let qr = QrCode::encode_text(text, QrCodeEcc::Medium).map_err(|e| anyhow::anyhow!("QR encode: {e:?}"))?;
    const QUIET: i32 = 2;
    let size = qr.size();
    let dark = |x: i32, y: i32| x >= 0 && y >= 0 && x < size && y < size && qr.get_module(x, y);
    let mut out = String::new();
    let mut y = -QUIET;
    while y < size + QUIET {
        out.push_str("\x1b[30;47m");
        for x in -QUIET..size + QUIET {
            out.push(match (dark(x, y), dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push_str("\x1b[0m\n");
        y += 2;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_hint_only_for_a_separate_member() {
        use ligus_runtime::device_enroll::Standing;
        assert!(member_hint(&Standing::Own).is_some_and(|h| h.contains("own service member")));
        let owner = ligus_core::ActorIdentity::new("a", "Alex", ligus_core::ActorKind::Human);
        assert!(member_hint(&Standing::OwnerDevice(owner)).is_none());
        assert!(member_hint(&Standing::Revoked).is_none());
    }

    #[test]
    fn pairing_url_has_the_contract_shape() {
        let link = pairing_url("http://100.101.102.104:8787/", "abc123");
        assert!(link.starts_with("ligus://pair?"), "{link}");
        let rest = ligus_core::names::strip_deep_link_scheme(&link)
            .and_then(|r| r.strip_prefix("pair?"))
            .expect("pair link");
        let mut url = None;
        let mut token = None;
        for kv in rest.split('&') {
            let (k, v) = kv.split_once('=').unwrap();
            match k {
                "url" => url = Some(v.to_string()),
                "token" => token = Some(v.to_string()),
                other => panic!("unexpected key {other}"),
            }
        }
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(url.unwrap()).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "http://100.101.102.104:8787");
        assert_eq!(token.as_deref(), Some("abc123"));
        assert!(!link.contains('='.to_string().repeat(2).as_str()), "no base64 padding");
    }

    #[test]
    fn loopback_urls_are_unreachable_from_a_phone() {
        for u in ["http://127.0.0.1:8787", "http://localhost:8787/", "http://[::1]:8787", "http://127.1.2.3"] {
            assert!(is_loopback_url(u), "{u}");
        }
        for u in ["http://100.101.102.104:8787", "http://192.168.1.20:8787", "https://ligus.example.ts.net", "http://[fd7a:115c:a1e0::1]:8787"] {
            assert!(!is_loopback_url(u), "{u}");
        }
    }

    /// A loopback direct URL never reaches the phone: with no relay the
    /// link isn't printed (the reason and the ways out are); with one, the
    /// link goes relay-only.
    #[test]
    fn a_loopback_direct_url_is_never_offered_to_the_phone() {
        for u in ["http://127.0.0.1:8787", "http://localhost:8787", "http://[::1]:8787"] {
            match pair_plan(u, false, false) {
                Err(msg) => {
                    assert!(msg.contains("ligus remote enable --relay"), "{msg}");
                    assert!(msg.contains("LIGUS_SERVE_PUBLIC_URL"), "{msg}");
                    assert!(msg.contains(u), "{msg}");
                }
                Ok(p) => panic!("{u}: {p:?}"),
            }
            assert_eq!(pair_plan(u, true, false), Ok(PairPlan::RelayOnly), "{u}");
        }
        let lan = "http://192.168.1.20:8787";
        assert_eq!(pair_plan(lan, false, false), Ok(PairPlan::Direct));
        assert_eq!(pair_plan(lan, true, false), Ok(PairPlan::Direct));
        assert_eq!(pair_plan(lan, true, true), Ok(PairPlan::RelayOnly));
        assert!(pair_plan(lan, false, true).unwrap_err().contains("ligus remote enable --relay"));
    }

    #[test]
    fn pair_never_mints_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("serve.token");
        assert!(load_existing_token(&p).is_err());
        assert!(!p.exists());
        let t = load_or_create_token(&p).unwrap();
        assert_eq!(load_existing_token(&p).unwrap(), t);
    }

    #[test]
    fn token_file_is_created_0600_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("serve.token");
        let t1 = load_or_create_token(&p).unwrap();
        assert_eq!(t1.len(), 64);
        assert!(t1.chars().all(|c| c.is_ascii_hexdigit()));
        let t2 = load_or_create_token(&p).unwrap();
        assert_eq!(t1, t2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn token_compare() {
        assert!(token_matches("abcd", "abcd"));
        assert!(!token_matches("abcd", "abce"));
        assert!(!token_matches("abcd", "abc"));
        assert!(!token_matches("abcd", ""));
    }

    #[test]
    fn binds_only_loopback_lan_or_tailnet() {
        assert!(bind_allowed("127.0.0.1".parse().unwrap()));
        assert!(bind_allowed("100.101.102.104".parse().unwrap()));
        assert!(bind_allowed("192.168.1.5".parse().unwrap()));
        assert!(bind_allowed("::1".parse().unwrap()));
        assert!(!bind_allowed("0.0.0.0".parse().unwrap()));
        assert!(!bind_allowed("::".parse().unwrap()));
        assert!(!bind_allowed("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn qr_renders_square_block() {
        let q = render_qr("ligus://pair?url=aHR0cDovL3g&token=t").unwrap();
        let lines: Vec<&str> = q.lines().collect();
        assert!(lines.len() > 10);
        assert!(q.contains('█'));
    }
}
