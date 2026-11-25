//! "Add a computer" across real devices: a person's desktop, the computer
//! they link, and a teammate, synced through the in-process relay with every
//! pull verified on ingest as in production (`docs/design/link-a-computer.md`,
//! tests T3 to T7).
//!
//! Mixed versions (T6): this feature changes no `ligus-core` code (no event,
//! field or rule), so every fold below, the teammate's included, is the fold
//! that shipped in 0.2.1; what they accept, a 0.2.1 peer accepts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ligus_core::presence::PresenceAssertion;
use ligus_core::{workspace_config_session_id, ActionProposal, MessageRole, ProposalKind};
use uuid::Uuid;

use crate::chat_service::{PresenceProvider, PresenceRequest};
use crate::computer_link::{Grant, LinkError, LinkRequest, LinkState};
use crate::workspace_invite::{encode_workspace_invite, WorkspaceConn};
use crate::{ChatService, EventStore, FileKeyVault, IdentityStore, RelayClient, SyncEngine};

const PASSPHRASE: &str = "link a computer passphrase";

async fn spawn_relay() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, crate::test_relay::router()).await.unwrap() });
    format!("http://{addr}")
}

/// A person's passkey: their test recovery key.
struct TestPasskey(Uuid);

impl PresenceProvider for TestPasskey {
    fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
        Ok(ligus_core::testkit::test_recovery_key(self.0).assert(&req.challenge))
    }
}

struct Dev {
    svc: ChatService,
    dir: PathBuf,
    engine: SyncEngine,
}

fn dearth(enabled: bool) -> ligus_core::guard::DearthMode {
    ligus_core::guard::DearthMode {
        enabled,
        reason: String::new(),
        brood_paths: Vec::new(),
        allowed_mcp_servers: Vec::new(),
        admitted_devices: Vec::new(),
        set_by: String::new(),
        set_at: ligus_core::Timestamp::now(),
    }
}

fn now_ms() -> i64 {
    ligus_core::flow::now_ms()
}

impl Dev {
    fn open(dir: &Path, name: &str, relay: &str, ws: Uuid) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let ids = IdentityStore::new(dir, FileKeyVault::new(dir));
        let me = ids.bootstrap(name, &name.to_lowercase(), name).unwrap();
        let mut svc = ChatService::new(
            EventStore::open(dir.join("ligus.db")).unwrap(),
            ids.device_keypair(me.device.id).unwrap(),
            ids.account_keypair(me.account.id).unwrap(),
            me.account.actor(),
        );
        svc.enable_test_presence();
        let key = ligus_core::derive_workspace_key(PASSPHRASE);
        let engine = SyncEngine::new(RelayClient::new(relay), ws.to_string()).with_room_workspace(ws).with_key(key);
        Self { svc, dir: dir.to_path_buf(), engine }
    }

    fn ids(&self) -> IdentityStore<FileKeyVault> {
        IdentityStore::new(&self.dir, FileKeyVault::new(&self.dir))
    }

    async fn sync(&mut self) {
        let mut store = EventStore::open(self.dir.join("ligus.db")).unwrap();
        self.engine.push_new(&store).await.unwrap();
        self.engine.pull(&mut store).await.unwrap();
    }

    fn id(&self) -> String {
        self.svc.author().id.clone()
    }

    fn account(&self) -> Uuid {
        self.svc.author().account_id.unwrap()
    }

    /// Found `ws` with a registered credential (the test recovery key) and
    /// this person's passkey provider.
    fn found(&mut self, ws: Uuid) {
        let cfg = workspace_config_session_id(ws);
        self.svc.ensure_self_member(cfg, ws).unwrap();
        let acct = self.account();
        let key = ligus_core::testkit::test_recovery_key(acct);
        self.svc.register_passkey(ws, key.credential(acct, "Test recovery key"), &|req| Ok(key.assert(&req.challenge))).unwrap();
        self.svc.set_presence_provider(Some(Arc::new(TestPasskey(acct))));
    }

    /// The connection a desktop seals into a grant (no invite).
    fn conn(&self, room: &str) -> String {
        let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&self.svc.account_public_key());
        encode_workspace_invite(&WorkspaceConn {
            name: "Acme".into(),
            relay_url: "http://relay.invalid".into(),
            room: room.into(),
            key: Some(PASSPHRASE.into()),
            icon: None,
            dm_account: None,
            founder_actor_id: Some(self.id()),
            fk: Some(fk),
            inv: None,
            ek: Vec::new(),
            sig: None,
        })
    }

    async fn join_via(&mut self, admin: &mut Dev, ws: Uuid) {
        let invite = admin.svc.issue_invite(ws).unwrap();
        admin.sync().await;
        self.sync().await;
        self.svc.store().set_join_invite(ws, invite).unwrap();
        self.svc.store().set_join_redeem(ws, admin.svc.take_invite_redeem_seed(ws, invite).unwrap().as_ref()).unwrap();
        self.svc.ensure_self_member(workspace_config_session_id(ws), ws).unwrap();
    }

    fn bodies(&self, chat: Uuid) -> Vec<(String, String)> {
        self.svc
            .load(chat)
            .unwrap()
            .unwrap()
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .map(|m| (m.body.clone(), m.actor_identity.as_ref().map(|a| a.id.clone()).unwrap_or_default()))
            .collect()
    }

    /// This computer's link request, remembered as pending.
    fn request(&self, label: &str) -> LinkRequest {
        let ids = self.ids();
        let me = ids.load().unwrap().unwrap();
        let ka = ids.key_agreement(me.device.id).unwrap();
        let r = LinkRequest::new(&ids.device_keypair(me.device.id).unwrap(), &ka.public_key_bytes(), label, now_ms(), None);
        let mut state = LinkState::load(&self.dir).unwrap();
        state.add_pending(&r, now_ms());
        state.save(&self.dir).unwrap();
        r
    }

    /// Redeem `grant` the way the CLI and lt do: open, check, pin the
    /// founder, pull, adopt, record, act as the person.
    async fn redeem(&mut self, grant: &str) -> Result<ligus_core::ActorIdentity, String> {
        let ids = self.ids();
        let me = ids.load().unwrap().unwrap();
        let ka = ids.key_agreement(me.device.id).unwrap();
        let g = Grant::open(grant, &ka).map_err(|e| e.to_string())?;
        let mut state = LinkState::load(&self.dir).unwrap();
        if state.pending_for(&g.request, now_ms()).is_none() {
            return Err(LinkError::NotPending.to_string());
        }
        g.check(&me.device.signing_public_key, now_ms()).map_err(|e| e.to_string())?;
        let (f, fk) = g.conn.founder_pin().ok_or("no founder pin")?;
        self.svc.store().set_workspace_pin(g.workspace_id, &f, &fk).unwrap();
        self.svc.set_trusted_founder(Some(f));
        self.sync().await;
        let person = self.svc.check_computer_link(&g, now_ms()).map_err(|e| e.to_string())?;
        state.record_link(&person, g.workspace_id).map_err(|e| e.to_string())?;
        state.save(&self.dir).unwrap();
        self.svc.act_as_owner_device(person.clone());
        self.sync().await;
        Ok(person)
    }
}

/// T3, T5: the desktop adds a computer with one passkey ceremony; the
/// computer authors as the person everywhere, as a device that acts for them
/// without their account key (a vote from it needs their passkey), and stops
/// counting once revoked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_linked_computer_is_the_persons_own_device_until_revoked() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let room = "acme";
    let ws = ligus_core::room_workspace_id(room);
    let cfg = workspace_config_session_id(ws);

    let mut mac = Dev::open(&tmp.path().join("mac"), "Alex", &relay, ws);
    mac.found(ws);
    let chat = mac.svc.create_chat("plans", ws, "").unwrap().id;
    let proposal = ActionProposal::new("ship it", ProposalKind::Decision, mac.id());
    let pid = proposal.id;
    mac.svc.upsert_proposal(chat, ws, proposal).unwrap();
    mac.sync().await;
    let mut taylor = Dev::open(&tmp.path().join("taylor"), "Taylor", &relay, ws);
    taylor.join_via(&mut mac, ws).await;
    taylor.sync().await;

    // The computer: a fresh install that never joined anything.
    let mut lt = Dev::open(&tmp.path().join("lt"), "lt on mbp", &relay, ws);
    let request = lt.request("lt on mbp");
    let decoded = LinkRequest::decode(&request.encode()).unwrap();
    let preview = mac.svc.preview_computer_link(ws, &decoded).unwrap();
    assert_eq!(preview.fingerprint, crate::device_enroll::fingerprint(&lt.svc.device_public_key()));
    assert!(!preview.already_mine && !preview.revoked && preview.someone_elses.is_none() && preview.previous_member.is_none());
    let grant = mac.svc.issue_computer_link(ws, &decoded, &mac.conn(room), now_ms()).unwrap();
    // Before the desktop syncs, the computer can't find its certificate.
    let ka = lt.ids().key_agreement(lt.svc.device_id()).unwrap();
    let opened = Grant::open(&grant, &ka).unwrap();
    assert_eq!(opened.approver_fingerprint(), crate::device_enroll::fingerprint(&mac.svc.device_public_key()));
    mac.sync().await;

    // A grant isn't a bearer code: another computer can't open it.
    let other = Dev::open(&tmp.path().join("other"), "Other", &relay, ws);
    let other_ka = other.ids().key_agreement(other.svc.device_id()).unwrap();
    assert_eq!(Grant::open(&grant, &other_ka).unwrap_err(), LinkError::NotForThisDevice);

    let person = lt.redeem(&grant).await.unwrap();
    assert_eq!(person.id, mac.id());
    // Single use: the pending request is gone.
    assert!(lt.redeem(&grant).await.unwrap_err().contains("doesn't answer a request"));
    // Its key-agreement key, so later rotations reach it.
    let lt_ka = lt.ids().key_agreement(lt.svc.device_id()).unwrap();
    assert!(lt.svc.publish_key_agreement(ws, lt_ka.public_key_bytes()).unwrap());
    // Never an account key of its own under Alex's id (M3).
    assert!(!lt.svc.publish_identity(cfg, ws).unwrap());

    lt.svc.post_user_message(chat, ws, "from lt").unwrap();
    // A vote from a device without the account key needs Alex's passkey.
    let refused = lt.svc.vote_on_proposal(chat, ws, pid, true).unwrap_err().to_string();
    assert!(refused.contains(ligus_core::authz::PRESENCE_REQUIRED), "{refused}");
    lt.sync().await;
    mac.sync().await;
    taylor.sync().await;
    for dev in [&mac, &taylor] {
        let bodies = dev.bodies(chat);
        assert!(bodies.contains(&("from lt".into(), mac.id())), "authored as Alex: {bodies:?}");
        let (_, t) = dev.svc.store().load_config(ws, Some(&mac.id()), None).unwrap();
        let d = t.current().devices.get(&lt.svc.device_id()).unwrap().clone();
        assert!(d.accounts.contains(&mac.account()) && d.delegated_for(mac.account()) && !d.phone, "{d:?}");
        assert!(t.current().ka_keys.contains_key(&lt.svc.device_id()));
        // One member, not two.
        let config = dev.svc.load(cfg).unwrap().unwrap();
        assert_eq!(config.members.iter().filter(|m| m.actor.display_name == "Alex").count(), 1);
        // No account key from the computer's device.
        let trust = dev.svc.store().trust_envelopes(ws).unwrap();
        assert!(!trust.iter().any(|e| e.signer_device_id == Some(lt.svc.device_id())
            && matches!(e.payload, ligus_core::SessionEvent::AccountKeyRegistered { .. } | ligus_core::SessionEvent::AccountKeyRotated { .. })));
    }

    // Account-key acts stay on the desktop.
    let acct = mac.account();
    let k = ligus_core::testkit::test_recovery_key(Uuid::new_v4());
    assert!(lt.svc.register_passkey(ws, k.credential(acct, "x"), &|req| Ok(k.assert(&req.challenge))).is_err());
    let phone = p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
    let phone_pk = phone.verifying_key().to_sec1_point(false).as_bytes().to_vec();
    let phone_id = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::P256, &phone_pk);
    assert!(lt.svc.certify_phone(ws, phone_id, &phone_pk).is_err());

    // The computer re-publishing the desktop's own certificate
    // doesn't turn the desktop into a delegated device.
    let mac_cert = mac
        .svc
        .store()
        .trust_envelopes(ws)
        .unwrap()
        .into_iter()
        .find_map(|e| match e.payload {
            ligus_core::SessionEvent::DeviceCertificateAdded { certificate } if certificate.device_id == mac.svc.device_id() => Some(certificate),
            _ => None,
        })
        .unwrap();
    let env = lt.svc.draft_signed(cfg, ws, ligus_core::SessionEvent::DeviceCertificateAdded { certificate: mac_cert }).unwrap();
    lt.svc.store_authored(&env).unwrap();
    lt.sync().await;
    mac.sync().await;
    let (_, t) = mac.svc.store().load_config(ws, Some(&mac.id()), None).unwrap();
    assert!(!t.current().devices[&mac.svc.device_id()].delegated_for(mac.account()), "the desktop keeps its account key's standing");

    // T8b: a second copy of the computer's own certificate, under the
    // desktop's event id and signed by the computer, leaves it delegated.
    let own = mac
        .svc
        .store()
        .trust_envelopes(ws)
        .unwrap()
        .into_iter()
        .find(|e| matches!(&e.payload, ligus_core::SessionEvent::DeviceCertificateAdded { certificate } if certificate.device_id == lt.svc.device_id()))
        .unwrap();
    let mut copy = own.clone();
    copy.clear_raw();
    copy.sequence = lt.svc.store().max_sequence(ws, cfg).unwrap().unwrap_or(0) + 1;
    if let Some(st) = copy.actor_stamp.as_mut() {
        st.prev = None;
    }
    ligus_core::crypto::sign_envelope(&mut copy, lt.svc.device_id(), &lt.ids().device_keypair(lt.svc.device_id()).unwrap());
    let _ = lt.svc.store().append_envelope(&copy);
    lt.sync().await;
    mac.sync().await;
    let (_, t) = mac.svc.store().load_config(ws, Some(&mac.id()), None).unwrap();
    assert!(t.current().devices[&lt.svc.device_id()].delegated_for(mac.account()), "still delegated");
    // Listed under My devices on the desktop.
    assert!(mac.svc.own_devices(ws).unwrap().iter().any(|d| d.device_id == lt.svc.device_id() && !d.revoked));
    // T7: approving it again says so.
    let again = lt.request("lt on mbp");
    assert!(mac.svc.preview_computer_link(ws, &again).unwrap().already_mine);
    assert!(mac.svc.issue_computer_link(ws, &again, &mac.conn(room), now_ms()).unwrap_err().to_string().contains("already one of your devices"));

    // T5: revoked from the desktop.
    mac.svc.revoke_device(cfg, ws, lt.svc.device_id()).unwrap();
    mac.sync().await;
    taylor.sync().await;
    lt.sync().await;
    assert_eq!(lt.svc.device_standing(ws).unwrap(), crate::device_enroll::Standing::Revoked);
    // Written anyway (a stale build), it is dropped by everyone.
    let _ = lt.svc.post_user_message(chat, ws, "after revocation");
    lt.sync().await;
    mac.sync().await;
    taylor.sync().await;
    for dev in [&mac, &taylor] {
        assert!(!dev.bodies(chat).iter().any(|(b, _)| b == "after revocation"));
    }
}

/// T4: nobody can get a computer certified as someone else. A teammate's
/// grant makes the computer the teammate's; a grant whose certificate names
/// another person than the key that signed it is refused; a computer already
/// in the workspace as someone else is refused by the desktop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_computer_is_never_certified_as_someone_else() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let room = "acme";
    let ws = ligus_core::room_workspace_id(room);
    let mut mac = Dev::open(&tmp.path().join("mac"), "Alex", &relay, ws);
    mac.found(ws);
    mac.sync().await;
    let mut taylor = Dev::open(&tmp.path().join("taylor"), "Taylor", &relay, ws);
    taylor.join_via(&mut mac, ws).await;
    let tacct = taylor.account();
    let key = ligus_core::testkit::test_recovery_key(tacct);
    taylor.sync().await;
    mac.sync().await;
    // Taylor's credential needs Alex's grant (a new member's first passkey).
    taylor.svc.register_passkey(ws, key.credential(tacct, "Test recovery key"), &|req| Ok(key.assert(&req.challenge))).unwrap();
    taylor.svc.set_presence_provider(Some(Arc::new(TestPasskey(tacct))));
    taylor.sync().await;

    // Taylor answers a request Alex's computer made (she saw it pasted in a
    // chat): the grant certifies it as *Taylor's* device, never Alex's.
    let mut lt = Dev::open(&tmp.path().join("lt"), "lt", &relay, ws);
    let req = lt.request("lt");
    let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&mac.svc.account_public_key());
    let conn = encode_workspace_invite(&WorkspaceConn {
        name: "Acme".into(),
        relay_url: "http://relay.invalid".into(),
        room: room.into(),
        key: Some(PASSPHRASE.into()),
        icon: None,
        dm_account: None,
        founder_actor_id: Some(mac.id()),
        fk: Some(fk),
        inv: None,
        ek: Vec::new(),
        sig: None,
    });
    let (f, fk) = crate::workspace_invite::decode_workspace_invite(&conn).unwrap().founder_pin().unwrap();
    taylor.svc.store().set_workspace_pin(ws, &f, &fk).unwrap();
    let grant = taylor.svc.issue_computer_link(ws, &req, &conn, now_ms()).unwrap();
    let ka = lt.ids().key_agreement(lt.svc.device_id()).unwrap();
    let g = Grant::open(&grant, &ka).unwrap();
    assert_eq!(g.check(&lt.svc.device_public_key(), now_ms()).unwrap().account_id, Some(tacct), "Taylor's, as it says");

    taylor.sync().await;
    lt.svc.store().set_workspace_pin(ws, &mac.id(), &crate::envelope_verifier::FounderPin::fingerprint_hex(&mac.svc.account_public_key())).unwrap();
    lt.svc.set_trusted_founder(Some(mac.id()));
    lt.sync().await;
    // A forged grant: Taylor's grant names Alex as the person. The
    // workspace disagrees (the approver isn't Alex's device, and the
    // computer is certified for Taylor): refused, it never acts as Alex.
    let mut forged = g.clone();
    forged.person = mac.svc.author().clone();
    let err = lt.svc.check_computer_link(&forged, now_ms()).unwrap_err().to_string();
    assert!(err.contains("wasn't issued by a device of that person"), "{err}");
    // The honest grant names Taylor: the computer is Taylor's, not Alex's.
    let person = lt.svc.check_computer_link(&g, now_ms()).unwrap();
    assert_eq!(person.account_id, Some(tacct));
    // It acts as Taylor (its own signature as Taylor, what the redeem's
    // key-agreement key publishes): from then on it chose Taylor.
    lt.svc.act_as_owner_device(person);
    let chat = lt.svc.create_chat("lt", ws, "").unwrap().id;
    lt.svc.post_user_message(chat, ws, "as Taylor").unwrap();
    lt.sync().await;
    mac.sync().await;
    // Now it is Taylor's: Alex's desktop refuses to make it his.
    let again = lt.request("lt");
    let preview = mac.svc.preview_computer_link(ws, &again).unwrap();
    assert_eq!(preview.someone_elses.as_deref(), Some("Taylor"));
    let err = mac.svc.issue_computer_link(ws, &again, &mac.conn(room), now_ms()).unwrap_err().to_string();
    assert!(err.contains("as someone else"), "{err}");
    // Expired requests are refused.
    let mut stale = lt.request("lt");
    stale.created_at -= crate::computer_link::REQUEST_TTL_MS + 1;
    assert!(mac.svc.issue_computer_link(ws, &stale, &mac.conn(room), now_ms()).is_err());
}

/// B3 (identity model v2, 7.2): a certificate another member puts on a
/// computer's public key before the person approves it doesn't block the
/// link: the desktop's approval and the computer's redeem both go through,
/// and the computer acts as the person, never as the certifier. Once the
/// computer has acted as the person, a second person's approval is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_certificate_doesnt_block_or_redirect_a_link() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let room = "acme";
    let ws = ligus_core::room_workspace_id(room);
    let mut mac = Dev::open(&tmp.path().join("mac"), "Alex", &relay, ws);
    mac.found(ws);
    let chat = mac.svc.create_chat("plans", ws, "").unwrap().id;
    mac.sync().await;
    let mut taylor = Dev::open(&tmp.path().join("taylor"), "Taylor", &relay, ws);
    taylor.join_via(&mut mac, ws).await;
    taylor.sync().await;
    mac.sync().await;

    // The computer asks; its request (and so its public key) is seen by
    // Taylor, who certifies the key for herself before Alex approves.
    let mut lt = Dev::open(&tmp.path().join("lt"), "lt", &relay, ws);
    let req = lt.request("lt");
    taylor.svc.publish_foreign_certificate(ws, &req.device_public_key).unwrap();
    taylor.sync().await;
    mac.sync().await;
    let preview = mac.svc.preview_computer_link(ws, &req).unwrap();
    assert_eq!(preview.someone_elses, None, "a certificate alone isn't the computer choosing Taylor");
    assert_eq!(preview.also_certified_by, vec!["Taylor".to_string()], "shown, quietly");
    let grant = mac.svc.issue_computer_link(ws, &req, &mac.conn(room), now_ms()).unwrap();
    mac.sync().await;
    lt.svc.store().set_workspace_pin(ws, &mac.id(), &crate::envelope_verifier::FounderPin::fingerprint_hex(&mac.svc.account_public_key())).unwrap();
    let person = lt.redeem(&grant).await.unwrap();
    assert_eq!(person.id, mac.id(), "the computer is Alex's");
    lt.svc.post_user_message(chat, ws, "from lt").unwrap();
    lt.sync().await;
    mac.sync().await;
    assert!(mac.bodies(chat).contains(&("from lt".into(), mac.id())), "{:?}", mac.bodies(chat));
    let (standing, foreign) = lt.svc.device_standing_and_foreign(ws).unwrap();
    assert!(matches!(standing, crate::device_enroll::Standing::OwnerDevice(ref a) if a.id == mac.id()), "{standing:?}");
    assert_eq!(foreign.iter().map(|a| a.id.clone()).collect::<Vec<_>>(), vec![taylor.id()], "the certifier is only shown");

    // Having acted as Alex, the computer is Alex's: someone else can't link
    // it now (Sam never certified it, so this is the "chose" check).
    let mut sam = Dev::open(&tmp.path().join("sam"), "Sam", &relay, ws);
    sam.join_via(&mut mac, ws).await;
    sam.sync().await;
    mac.sync().await;
    let sacct = sam.account();
    let key = ligus_core::testkit::test_recovery_key(sacct);
    sam.svc.register_passkey(ws, key.credential(sacct, "Test recovery key"), &|r| Ok(key.assert(&r.challenge))).unwrap();
    sam.svc.set_presence_provider(Some(Arc::new(TestPasskey(sacct))));
    sam.sync().await;
    let fk = crate::envelope_verifier::FounderPin::fingerprint_hex(&mac.svc.account_public_key());
    sam.svc.store().set_workspace_pin(ws, &mac.id(), &fk).unwrap();
    sam.svc.set_trusted_founder(Some(mac.id()));
    let again = lt.request("lt");
    let err = sam.svc.issue_computer_link(ws, &again, &mac.conn(room), now_ms()).unwrap_err().to_string();
    assert!(err.contains("as someone else"), "{err}");
}

/// T7: lockdown refuses; the previous-member proof is shown when it verifies,
/// and the old member's history keeps its author after removal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_duplicate_member_moves_to_a_linked_device() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let room = "acme";
    let ws = ligus_core::room_workspace_id(room);
    let cfg = workspace_config_session_id(ws);
    let mut mac = Dev::open(&tmp.path().join("mac"), "Alex", &relay, ws);
    mac.found(ws);
    let chat = mac.svc.create_chat("plans", ws, "").unwrap().id;
    mac.sync().await;
    // Today's state: Alex invited himself into lt, a second member.
    let mut lt = Dev::open(&tmp.path().join("lt"), "Alex (lt)", &relay, ws);
    lt.join_via(&mut mac, ws).await;
    let old_member = lt.id();
    lt.svc.post_user_message(chat, ws, "as the second Alex").unwrap();
    lt.sync().await;
    mac.sync().await;

    // Link the existing install: a new device key, prepared next to the
    // current one, with the old member's proof.
    let ids = lt.ids();
    let stored = ids.load().unwrap().unwrap();
    let (new_id, new_key) = ids.prepare_device().unwrap();
    let ka = ids.key_agreement(new_id).unwrap();
    let account_kp = ids.account_keypair(stored.account.id).unwrap();
    let req = LinkRequest::new(&new_key, &ka.public_key_bytes(), "lt on mbp", now_ms(), Some((stored.account.id, &account_kp)));
    let mut state = LinkState::default();
    state.add_pending(&req, now_ms());
    state.save(&lt.dir).unwrap();
    let preview = mac.svc.preview_computer_link(ws, &req).unwrap();
    assert_eq!(preview.previous_member.as_ref().map(|m| m.id.clone()), Some(old_member.clone()));
    // A proof by a key that isn't that member's shows nothing.
    let forged = LinkRequest::new(&new_key, &ka.public_key_bytes(), "lt", now_ms(), Some((stored.account.id, &ligus_core::crypto::SigningKeypair::generate().unwrap())));
    assert!(mac.svc.preview_computer_link(ws, &forged).unwrap().previous_member.is_none());

    // Lockdown refuses.
    mac.svc.set_dearth_mode(ws, dearth(true)).unwrap();
    let err = mac.svc.issue_computer_link(ws, &req, &mac.conn(room), now_ms()).unwrap_err().to_string();
    assert!(err.contains("lockdown"), "{err}");
    mac.svc.set_dearth_mode(ws, dearth(false)).unwrap();

    let grant = mac.svc.issue_computer_link(ws, &req, &mac.conn(room), now_ms()).unwrap();
    mac.sync().await;
    // The install switches to the prepared device, then redeems.
    ids.adopt_device(new_id).unwrap();
    drop(lt);
    let mut lt = Dev::open(&tmp.path().join("lt"), "Alex (lt)", &relay, ws);
    assert_eq!(lt.svc.device_id(), new_id);
    lt.redeem(&grant).await.unwrap();
    lt.svc.post_user_message(chat, ws, "as Alex").unwrap();
    lt.sync().await;
    // The desktop removes the old member.
    mac.svc.remove_member(cfg, ws, old_member.clone()).unwrap();
    mac.sync().await;
    lt.sync().await;
    mac.sync().await;
    let bodies = mac.bodies(chat);
    assert!(bodies.contains(&("as the second Alex".into(), old_member.clone())), "history keeps its author: {bodies:?}");
    assert!(bodies.contains(&("as Alex".into(), mac.id())), "{bodies:?}");
    assert_eq!(mac.svc.device_links(ws).unwrap().person(&old_member), old_member, "no alias: nothing is re-attributed");
    let config = mac.svc.load(cfg).unwrap().unwrap();
    assert!(!config.members.iter().any(|m| m.id == old_member));

    // Removing the old member doesn't touch the linked device: it still
    // posts as Alex (the owner's 0.3.0 report).
    lt.svc.post_user_message(chat, ws, "after the removal").unwrap();
    lt.sync().await;
    mac.sync().await;
    assert!(mac.bodies(chat).contains(&("after the removal".into(), mac.id())), "{:?}", mac.bodies(chat));

    // A second process on the same data dir that still signs as the old
    // member (an lt left open across the link) is refused, in words that
    // say why and how to fix it, never "role Viewer cannot perform".
    let mut stale = ChatService::new(
        EventStore::open(lt.dir.join("ligus.db")).unwrap(),
        ids.device_keypair(stored.device.id).unwrap(),
        ids.account_keypair(stored.account.id).unwrap(),
        stored.account.actor(),
    );
    stale.set_trusted_founder(Some(mac.id()));
    let err = stale.post_user_message(chat, ws, "as the removed member").unwrap_err().to_string();
    assert!(err.contains("isn't a member of this workspace") && err.contains("Link this computer"), "{err}");
    assert!(!err.contains("cannot perform"), "{err}");
}

/// Review R1: a device can sign a trust-bootstrap event stamped as anyone
/// (ingest stores it inert); that is never choosing that account. Choice is
/// `chosen ∩ certified` (design 7.2), so approving a box code never offers
/// to remove a member the box merely signed as.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_never_chose_an_account_that_never_certified_it() {
    let relay = spawn_relay().await;
    let tmp = tempfile::tempdir().unwrap();
    let room = "acme";
    let ws = ligus_core::room_workspace_id(room);
    let cfg = workspace_config_session_id(ws);
    let mut mac = Dev::open(&tmp.path().join("mac"), "Alex", &relay, ws);
    mac.found(ws);
    mac.sync().await;
    let mut victor = Dev::open(&tmp.path().join("victor"), "Victor", &relay, ws);
    victor.join_via(&mut mac, ws).await;
    victor.sync().await;
    let mut mallory = Dev::open(&tmp.path().join("mallory"), "Mallory", &relay, ws);
    mallory.join_via(&mut mac, ws).await;
    mallory.sync().await;
    mac.sync().await;
    let vacct = victor.account();
    let (mdev, mkey) = (mallory.svc.device_id(), mallory.svc.device_public_key());

    // Mallory's own device signs a trust-bootstrap event stamped as Victor
    // (it re-publishes her own, valid certificate). Victor never certified it.
    let ids = mallory.ids();
    let stored = ids.load().unwrap().unwrap();
    let akp = ids.account_keypair(stored.account.id).unwrap();
    let cert = ligus_core::crypto::DeviceCertificate::issue(&akp, mallory.account(), &mkey, ligus_core::Timestamp::now());
    let victor_actor = victor.svc.author().clone();
    mallory.svc.act_as_owner_device(victor_actor);
    let env = mallory.svc.draft_signed(cfg, ws, ligus_core::SessionEvent::DeviceCertificateAdded { certificate: cert }).unwrap();
    mallory.engine.push_envelopes(&[env.clone()]).await.unwrap();
    mac.sync().await;

    assert!(mac.svc.store().trust_envelopes(ws).unwrap().iter().any(|e| e.event_id == env.event_id), "precondition: stored inert");
    assert!(!mac.svc.accounts_certifying(ws, mdev).unwrap().contains(&vacct), "precondition: Victor never certified it");
    let chosen = mac.svc.device_chosen_accounts(ws, mdev, &mkey).unwrap();
    assert!(!chosen.contains(&vacct), "the device 'chose' Victor without Victor's certificate: {chosen:?}");
    assert!(chosen.contains(&stored.account.id), "its own account it did choose: {chosen:?}");
}
