//! `ligus forge …`: this box's code-host connections, and the git
//! credential helper that hands git a token from the OS secret store.
//!
//! - `ligus forge list`
//! - `ligus forge add <github|gitlab|forgejo|gitea|codeberg> <https://base> [--auth token|oauth|gh|none] [--client-id ID] [--internal]`
//!   (`--internal`: the server is on this device's network; a loopback,
//!   private or link-local address is refused without it)
//! - `ligus forge token <id> <read|write>`: reads the token from **stdin**
//!   (never argv, where `ps` and shell history would see it)
//! - `ligus forge login <id>`: device sign-in (GitHub App, GitLab app)
//! - `ligus forge test <id>`, `ligus forge remove <id>`
//! - `ligus forge git-credential <id> <get|store|erase>`: git's helper
//!   protocol. Git runs it (through `credential.<base>.helper`) for Ligus's
//!   own pushes on this box; it answers `get` with the write token, only
//!   for that connection's own https host.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use ligus_forge::tokens::{AuthSource, Purpose, Secret, TokenStore};
use ligus_forge::{Connection, Connections, ForgeId, ForgeKind};

/// The token store of this box.
pub fn token_store(data_dir: &Path) -> Arc<dyn TokenStore> {
    ligus_runtime::forge_secrets::ForgeSecrets::for_data_dir(data_dir)
}

/// This binary, for the `!ligus forge git-credential <id>` helper.
pub fn helper_program() -> Option<std::path::PathBuf> {
    std::env::current_exe().ok()
}

/// Parse `github|gitlab|forgejo|gitea|codeberg` into a kind and flavor.
pub fn parse_kind(s: &str) -> Result<(ForgeKind, Option<String>)> {
    let lower = s.trim().to_ascii_lowercase();
    let kind = ForgeKind::parse(&lower).with_context(|| {
        format!("unknown code host kind '{s}' (github, gitlab, forgejo, gitea, codeberg)")
    })?;
    let flavor = matches!(lower.as_str(), "forgejo" | "gitea" | "codeberg").then_some(lower);
    Ok((kind, flavor))
}

/// A token as pasted: trimmed, non-empty, at most 4 KiB, no whitespace or
/// control characters inside.
pub fn clean_token(raw: &str) -> Result<Secret> {
    let t = raw.trim();
    if t.is_empty() || t.len() > 4096 || t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("that doesn't look like a token (one line, no spaces, at most 4 KiB)");
    }
    Ok(Secret::new(t))
}

pub fn cmd(
    data_dir: &Path,
    args: &[&str],
    auth: Option<String>,
    client_id: Option<String>,
    internal: bool,
) -> Result<()> {
    match args.first().copied() {
        Some("git-credential") => {
            let id = ForgeId::parse(args.get(1).copied().unwrap_or(""))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let action = args.get(2).copied().unwrap_or("get");
            let mut input = String::new();
            std::io::stdin()
                .take(64 * 1024)
                .read_to_string(&mut input)?;
            git_credential(data_dir, &id, action, &input, &mut std::io::stdout())
        }
        Some("list") | None => {
            let store = token_store(data_dir);
            for c in Connections::load(data_dir).forges {
                let present: Vec<&str> = if c.auth == AuthSource::Gh {
                    vec!["gh"]
                } else {
                    store.present(&c.id).iter().map(|p| p.as_str()).collect()
                };
                println!(
                    "{:<34} {:<28} auth {:<6} tokens [{}]{}",
                    c.id.as_str(),
                    c.display(),
                    c.auth.as_str(),
                    present.join(","),
                    c.login
                        .as_deref()
                        .map(|l| format!("  as {l}"))
                        .unwrap_or_default()
                );
            }
            Ok(())
        }
        Some("add") => {
            let (Some(kind), Some(base)) = (args.get(1), args.get(2)) else {
                bail!("usage: ligus forge add <github|gitlab|forgejo|gitea|codeberg> <https://base-url> [--auth token|oauth|gh|none] [--client-id ID] [--internal]");
            };
            let (kind, flavor) = parse_kind(kind)?;
            let auth = match auth.as_deref() {
                None => {
                    if kind == ForgeKind::GitHub {
                        AuthSource::Gh
                    } else {
                        AuthSource::Token
                    }
                }
                Some("token") => AuthSource::Token,
                Some("oauth") => AuthSource::Oauth,
                Some("gh") => AuthSource::Gh,
                Some("none") => AuthSource::None,
                Some(o) => bail!("--auth is token, oauth, gh or none (got {o})"),
            };
            let mut c = Connection::new(kind, base, auth).map_err(|e| anyhow::anyhow!("{e}"))?;
            if flavor.is_some() && c.flavor.is_none() {
                c.flavor = flavor;
            }
            c.client_id = client_id.filter(|s| !s.trim().is_empty());
            c.internal = internal;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(ligus_forge::conn::check_address(&c))
                .map_err(|e| anyhow::anyhow!("{e} (--internal)"))?;
            ligus_forge::Connections::update(data_dir, |all| {
                all.upsert(c.clone());
                Ok(())
            })
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("added {} ({}).", c.id, c.display());
            if auth == AuthSource::Token {
                println!(
                    "now: ligus forge token {} write   (paste the token on stdin)",
                    c.id
                );
            }
            Ok(())
        }
        Some("token") => {
            let id = ForgeId::parse(args.get(1).copied().unwrap_or(""))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let purpose = args
                .get(2)
                .and_then(|p| Purpose::parse(p))
                .context("usage: ligus forge token <id> <read|write>  (token on stdin)")?;
            let mut all = Connections::load(data_dir);
            let c = all
                .get_mut(&id)
                .with_context(|| format!("no connection {id}: add it first"))?;
            if c.auth == AuthSource::Gh {
                bail!("{id} uses the gh login; add a token connection instead");
            }
            eprintln!(
                "paste the {} token, then press Enter (it is not echoed to any log):",
                purpose.as_str()
            );
            let mut line = String::new();
            std::io::stdin().take(8192).read_to_string(&mut line)?;
            let token = clean_token(line.lines().next().unwrap_or(""))?;
            token_store(data_dir)
                .put(&id, purpose, &token)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if c.auth == AuthSource::None {
                c.auth = AuthSource::Token;
            }
            all.save(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "stored the {} token for {id} in this device's secret store.",
                purpose.as_str()
            );
            Ok(())
        }
        Some("remove") => {
            let id = ForgeId::parse(args.get(1).copied().unwrap_or(""))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let mut all = Connections::load(data_dir);
            let removed = all.remove(&id);
            token_store(data_dir)
                .delete_all(&id)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            all.save(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "{}",
                if removed {
                    format!("removed {id} and its tokens.")
                } else {
                    format!("no connection {id}; its tokens (if any) were removed.")
                }
            );
            Ok(())
        }
        Some("test") => {
            let id = ForgeId::parse(args.get(1).copied().unwrap_or(""))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let all = Connections::load(data_dir);
            let c = all
                .get(&id)
                .with_context(|| format!("no connection {id}"))?
                .clone();
            let forge = c.build(c.credentials(
                token_store(data_dir),
                helper_program().as_deref(),
                Some(data_dir),
            ));
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            match rt.block_on(forge.whoami()) {
                Ok(Some(u)) => println!("{id}: signed in as {} (id {}).", u.login, u.id),
                Ok(None) => println!(
                    "{id}: no credentials ({})",
                    forge.signed_out_reason().unwrap_or_default()
                ),
                Err(e) => bail!("{id}: {e}"),
            }
            Ok(())
        }
        Some("login") => {
            let id = ForgeId::parse(args.get(1).copied().unwrap_or(""))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let all = Connections::load(data_dir);
            let c = all
                .get(&id)
                .with_context(|| format!("no connection {id}"))?
                .clone();
            let ep = ligus_forge::device::endpoints(&c).map_err(|e| anyhow::anyhow!("{e}"))?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let start = rt
                .block_on(ligus_forge::device::start(&ep))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "Open {} and enter the code {}  (check the app name before you approve)",
                start.verification_uri, start.user_code
            );
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(start.expires_in);
            let mut interval = start.interval;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(interval));
                if std::time::Instant::now() > deadline {
                    bail!("the code expired; run ligus forge login {id} again");
                }
                match rt
                    .block_on(ligus_forge::device::poll(&ep, &start.device_code))
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                {
                    ligus_forge::device::Poll::Pending => {}
                    ligus_forge::device::Poll::SlowDown(i) => interval = i,
                    ligus_forge::device::Poll::Denied => bail!("sign-in was denied"),
                    ligus_forge::device::Poll::Expired => {
                        bail!("the code expired; run ligus forge login {id} again")
                    }
                    ligus_forge::device::Poll::Done(t) => {
                        save_token_set(data_dir, &id, &t)?;
                        println!("signed in; the token is in this device's secret store.");
                        return Ok(());
                    }
                }
            }
        }
        Some(other) => bail!(
            "unknown `ligus forge {other}`: use list|add|token|login|test|remove|git-credential"
        ),
    }
}

/// Store a device sign-in's tokens and mark the connection as OAuth, only
/// while the connection still exists.
pub fn save_token_set(
    data_dir: &Path,
    id: &ForgeId,
    t: &ligus_forge::device::TokenSet,
) -> Result<()> {
    let saved = ligus_forge::conn::save_device_sign_in(
        data_dir,
        token_store(data_dir).as_ref(),
        id,
        t,
        || true,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    if !saved {
        bail!("{id} was removed during the sign-in; nothing was stored");
    }
    Ok(())
}

/// One git credential helper call: answer `get` from the store.
pub fn git_credential(
    data_dir: &Path,
    id: &ForgeId,
    action: &str,
    input: &str,
    out: &mut dyn Write,
) -> Result<()> {
    if action != "get" {
        return Ok(());
    }
    let all = Connections::load(data_dir);
    let Some(c) = all.get(id) else { return Ok(()) };
    if !matches!(c.auth, AuthSource::Token | AuthSource::Oauth) {
        return Ok(());
    }
    let creds = c.credentials(token_store(data_dir), None, Some(data_dir));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    // Through the credential source, so an expired device-flow token is
    // refreshed first.
    let token = rt.block_on(creds.token(Purpose::Write)).ok().flatten();
    let username = ligus_forge::tokens::git_username(c.kind, c.auth, c.login.as_deref());
    if let Some(answer) = ligus_forge::tokens::answer_git_credential(
        action,
        input,
        &c.host(),
        &username,
        token.as_ref(),
    ) {
        out.write_all(answer.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helper answers git with the write token for its own host only,
    /// and never with a read token.
    #[test]
    fn git_credential_helper_answers_from_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut all = Connections::default();
        let c =
            Connection::new(ForgeKind::Gitea, "https://codeberg.org", AuthSource::Token).unwrap();
        all.upsert(c.clone());
        all.save(dir.path()).unwrap();
        let store = token_store(dir.path());
        store
            .put(&c.id, Purpose::Read, &Secret::new("read-only-token-123456"))
            .unwrap();
        let mut out = Vec::new();
        git_credential(
            dir.path(),
            &c.id,
            "get",
            "protocol=https\nhost=codeberg.org\n",
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty(), "a read token is never handed to git");
        store
            .put(
                &c.id,
                Purpose::Write,
                &Secret::new("write-token-abcdef123456"),
            )
            .unwrap();
        git_credential(
            dir.path(),
            &c.id,
            "get",
            "protocol=https\nhost=codeberg.org\n",
            &mut out,
        )
        .unwrap();
        assert_eq!(String::from_utf8(out.clone()).unwrap(), "protocol=https\nhost=codeberg.org\nusername=ligus\npassword=write-token-abcdef123456\n");
        out.clear();
        git_credential(
            dir.path(),
            &c.id,
            "get",
            "protocol=https\nhost=github.com\n",
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty(), "another host gets nothing");
        git_credential(
            dir.path(),
            &c.id,
            "store",
            "protocol=https\nhost=codeberg.org\npassword=x\n",
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty());
        assert!(
            clean_token("ab cd").is_err()
                && clean_token("").is_err()
                && clean_token(" glpat-x \n").is_ok()
        );
    }
}
