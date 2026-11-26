//! A desktop stand-in for the iOS live end-to-end run (`ios-identity-e2e`
//! in the iOS repo): it founds a workspace and approves a `ligus serve` box
//! as its own device, so the phone can be paired with a box that acts for
//! its owner (where invites and code-host changes need presence). Test
//! harness only: it speaks for the identity in its own data dir, on the
//! relay it is given, and nothing else.
//!
//! ```text
//! cargo run -p ligus-cli --example ios_e2e_owner -- <data-dir> <relay> <room> <key> found <name>
//!   → prints the founder-pinned hivews1: invite for the box (`ligus enroll`)
//! cargo run -p ligus-cli --example ios_e2e_owner -- <data-dir> <relay> <room> <key> approve <hivedev1:…>
//!   → certifies the box's device under this account and syncs
//! ```
//!
//! Keep `LIGUS_SECRET_BACKEND=file` set so keys stay in the data dir.

use anyhow::{bail, Context, Result};
use base64::Engine;
use ligus_core::workspace_config_session_id;
use ligus_runtime::secret_store::StoreKeyVault;
use ligus_runtime::{ChatService, EventStore, IdentityStore, RelayClient, SyncEngine};

async fn sync(
    dir: &std::path::Path,
    relay: &str,
    room: &str,
    key: &str,
    device: uuid::Uuid,
) -> Result<()> {
    let identity = IdentityStore::new(dir, StoreKeyVault::for_data_dir(dir));
    let ka = identity.key_agreement(device)?;
    let src = ligus_runtime::keyring::KeySource::for_device(device, ka, Some(key), &[]);
    let mut engine =
        SyncEngine::new(RelayClient::new(relay), room.to_string()).with_key_source(src);
    let mut store = EventStore::open(dir.join("ligus.db"))?;
    for _ in 0..20 {
        let up = engine.take_unpushed(&store)?;
        engine.push_envelopes(&up).await?;
        engine.pull_round(&mut store).await?;
        if !engine.is_behind() {
            break;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, relay, room, key, cmd, arg] = args.as_slice() else {
        bail!("usage: ios_e2e_owner <data-dir> <relay> <room> <key> <found NAME | approve CODE>");
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    ligus_runtime::secret_store::init_for_data_dir(&dir, Some("file"));
    let ids = IdentityStore::new(&dir, StoreKeyVault::for_data_dir(&dir));
    let name = if cmd == "found" {
        arg.as_str()
    } else {
        "Owner"
    };
    let stored = ids.bootstrap(name, name, "e2e-desktop")?;
    let open = || -> Result<ChatService> {
        let store = EventStore::open(dir.join("ligus.db"))?;
        let device_kp = ids.device_keypair(stored.device.id)?;
        let account_kp = ids.account_keypair(stored.account.id)?;
        Ok(ChatService::new(
            store,
            device_kp,
            account_kp,
            stored.account.actor(),
        ))
    };
    let ws = ligus_core::room_workspace_id(room);
    sync(&dir, relay, room, key, stored.device.id).await?;
    match cmd.as_str() {
        "found" => {
            let mut svc = open()?;
            svc.set_author_display_name(arg.clone());
            let account = svc.author().account_id.context("no account")?;
            let fk = ligus_runtime::envelope_verifier::FounderPin::fingerprint_hex(
                &svc.account_public_key(),
            );
            svc.store()
                .set_workspace_pin(ws, &account.to_string(), &fk)?;
            svc.ensure_self_member(workspace_config_session_id(ws), ws)?;
            let _ = svc.ensure_default_channel(ws, "");
            let ka = ids.key_agreement(stored.device.id)?;
            let _ = svc.publish_key_agreement(ws, ka.public_key_bytes());
            let inv = svc.issue_invite(ws)?;
            let ak: String = svc
                .take_invite_redeem_seed(ws, inv)?
                .map(|s| s.iter().map(|b| format!("{b:02x}")).collect())
                .unwrap_or_default();
            drop(svc);
            sync(&dir, relay, room, key, stored.device.id).await?;
            let conn = serde_json::json!({
                "name": format!("{arg}'s workspace"), "relay_url": relay, "room": room, "key": key,
                "icon": null, "dm_account": null, "founder_actor_id": account.to_string(), "fk": fk, "inv": inv.to_string(),
                "ak": ak,
            });
            let code =
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&conn)?);
            println!("hivews1:{code}");
        }
        "approve" => {
            let req = ligus_runtime::device_enroll::DeviceEnrollment::decode(arg)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let mut svc = open()?;
            svc.certify_device(&req)?;
            drop(svc);
            sync(&dir, relay, room, key, stored.device.id).await?;
            println!("approved {}", req.label);
        }
        other => bail!("unknown command {other}"),
    }
    Ok(())
}
