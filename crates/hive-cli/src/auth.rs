//! `ligus auth`: sign this box in to an organization's relay through
//! ligus-auth (device code), and keep its relay token fresh.
//!
//! The session lives in `<data dir>/ligus-auth.json` (0600). While it exists
//! and `LIGUS_RELAY_ACCESS_TOKEN` is not set, every relay client this process
//! builds sends the ligus-auth access token, refreshed in the background.
//! Signing in gives relay access only: joining a workspace still needs an
//! Admin to add this identity as a member.

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use hive_runtime::ligus_auth::{self, DeviceKey, DeviceSigner, Poll, TokenCell};
use hive_runtime::secret_store::StoreKeyVault;
use hive_runtime::IdentityStore;

static CELL: OnceLock<TokenCell> = OnceLock::new();

/// The ligus-auth token cell, when this process has a session.
pub fn cell() -> Option<TokenCell> {
    CELL.get().cloned()
}

fn signer(data_dir: &Path) -> Result<Arc<dyn DeviceSigner>> {
    std::fs::create_dir_all(data_dir)?;
    let identity = IdentityStore::new(data_dir, StoreKeyVault::for_data_dir(data_dir));
    let stored = identity.bootstrap("hive-cli", "hive-cli", "hive-cli")?;
    let kp = identity.device_keypair(stored.device.id)?;
    Ok(Arc::new(DeviceKey::new(stored.device.id, kp)))
}

/// Start the background refresh when a session exists. An explicit
/// `LIGUS_RELAY_ACCESS_TOKEN` wins and disables it. Never fails the command:
/// problems are printed and the static token (if any) is used.
pub fn init(data_dir: &Path) {
    if hive_core::compat::env_opt("LIGUS_RELAY_ACCESS_TOKEN").is_some()
        || ligus_auth::load_session(data_dir).is_none()
    {
        return;
    }
    let cell = TokenCell::default();
    let started = signer(data_dir)
        .map_err(|e| anyhow!("{e:#}"))
        .and_then(|s| {
            ligus_auth::start_background_refresh(data_dir.to_path_buf(), s, cell.clone())
                .map_err(|e| anyhow!("{e}"))
        });
    match started {
        Ok(()) => {
            let _ = CELL.set(cell);
        }
        Err(e) => eprintln!("ligus-auth: {e} (run `ligus auth status`)"),
    }
}

/// `ligus auth login <url> | status | logout`.
pub fn cmd_auth(data_dir: &Path, args: &[&String], label: Option<String>) -> Result<()> {
    match args.first().map(|s| s.as_str()) {
        Some("login") => {
            let url = args.get(1).ok_or_else(|| {
                anyhow!(
                    "usage: ligus auth login https://auth.example.com/t/<tenant> [--label NAME]"
                )
            })?;
            login(data_dir, url, label)
        }
        Some("status") => status(data_dir),
        Some("logout") => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(ligus_auth::sign_out(&ligus_auth::http_client(), data_dir));
            println!("Signed out of ligus-auth on this box.");
            Ok(())
        }
        _ => bail!("usage: ligus auth login <url> [--label NAME] | status | logout"),
    }
}

fn login(data_dir: &Path, url: &str, label: Option<String>) -> Result<()> {
    let (base, tenant) = ligus_auth::parse_auth_url(url).map_err(|e| anyhow!("{e}"))?;
    let signer = signer(data_dir)?;
    let label = label.unwrap_or_else(|| {
        hive_core::compat::env_opt("HOSTNAME").unwrap_or_else(|| "ligus CLI".into())
    });
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let http = ligus_auth::http_client();
    let ds = rt
        .block_on(ligus_auth::start_device(&http, &base, "ligus-cli", &label))
        .map_err(|e| anyhow!("{e}"))?;
    println!(
        "To sign in this box, open:\n\n  {}\n\nand enter the code:  {}\n",
        ds.verification_uri, ds.user_code
    );
    if let Some(full) = &ds.verification_uri_complete {
        println!("(or open {full})\n");
    }
    println!("Waiting for approval…");
    let deadline = std::time::Instant::now() + Duration::from_secs(ds.expires_in.clamp(60, 1800));
    let mut interval = ds.interval.max(1);
    let session = loop {
        if std::time::Instant::now() > deadline {
            bail!("the code expired; run `ligus auth login` again");
        }
        std::thread::sleep(Duration::from_secs(interval));
        match rt
            .block_on(ligus_auth::poll_device(
                &http,
                &base,
                "ligus-cli",
                &ds.device_code,
                signer.as_ref(),
            ))
            .map_err(|e| anyhow!("{e}"))?
        {
            Poll::Pending => {}
            Poll::SlowDown => interval += 5,
            Poll::Denied => bail!("sign-in was denied"),
            Poll::Expired => bail!("the code expired; run `ligus auth login` again"),
            Poll::SignedIn(s) => break *s,
        }
    };
    // E-5: the token is for the relay this box syncs through, and only that.
    let mut session = session;
    session.relay_origin = hive_core::compat::env_opt("LIGUS_RELAY_URL")
        .or_else(|| crate::load_file_config(data_dir).relay_url)
        .and_then(|u| ligus_auth::origin_of(&u));
    if session.relay_origin.is_none() {
        eprintln!("No relay is configured: the relay access token won't be sent anywhere until you sign in again after `ligus enroll`.");
    }
    ligus_auth::save_session(data_dir, &session)
        .map_err(|e| anyhow!("{e}"))
        .context("saving the session")?;
    println!(
        "Signed in to {tenant} as {}. Relay access to: {}.",
        session.sub,
        session.workspaces.join(", ")
    );
    println!("This gives relay access only. A workspace Admin still has to add this identity as a member.");
    if hive_core::compat::env_opt("LIGUS_RELAY_ACCESS_TOKEN").is_some() {
        println!("Note: LIGUS_RELAY_ACCESS_TOKEN is set and takes precedence; unset it to use this sign-in.");
    }
    Ok(())
}

fn status(data_dir: &Path) -> Result<()> {
    let Some(s) = ligus_auth::load_session(data_dir) else {
        println!("Not signed in to ligus-auth. (`ligus auth login <url>`)");
        return Ok(());
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    println!("ligus-auth: {}", s.auth_url);
    println!("  tenant:     {}", s.tenant);
    println!("  subject:    {}", s.sub);
    println!("  workspaces: {}", s.workspaces.join(", "));
    if s.access_expires_at > now {
        println!(
            "  access:     valid for {}s (refreshed automatically)",
            s.access_expires_at - now
        );
    } else {
        println!("  access:     expired; refreshed on the next relay call");
    }
    if hive_core::compat::env_opt("LIGUS_RELAY_ACCESS_TOKEN").is_some() {
        println!("  note:       LIGUS_RELAY_ACCESS_TOKEN is set and takes precedence");
    }
    Ok(())
}
