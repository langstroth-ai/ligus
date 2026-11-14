//! Linked identities in the fold (IDENTITY-AND-FORGES §2.2, §7): only the
//! account itself publishes its links, from its own certified device, bound
//! to the workspace and the account key; agents can't; links are bounded;
//! and a link never grants membership, a role, or trust for sealing.
//!
//! The config log here is written by real signing devices: `owner`, `ada`
//! (Admin), `mara` (Viewer) and `eve` (published her identity, never let in).

use std::collections::BTreeMap;

use hive_core::authz::RosterState;
use hive_core::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
use hive_core::identity_link::{render_statement, IdentityLink, LinkMethod, MAX_LIVE_LINKS};
use hive_core::{
    fold_decisions, project_config, workspace_config_session_id, ActorIdentity, ActorKind, ActorStamp, ChatSession,
    SessionEvent, SessionEventEnvelope, Timestamp, Verdict, WorkspaceAuthz, WorkspaceMember, WorkspaceRole,
};
use proptest::prelude::*;
use uuid::Uuid;

struct Person {
    account_id: Uuid,
    account: SigningKeypair,
    device_id: Uuid,
    device: SigningKeypair,
}

#[derive(Clone)]
struct World {
    wid: Uuid,
    clock: u64,
    cfg: Vec<SessionEventEnvelope>,
    people: std::sync::Arc<BTreeMap<&'static str, Person>>,
}

const NAMES: [&str; 4] = ["owner", "ada", "mara", "eve"];

fn member(name: &str, p: &Person, role: WorkspaceRole) -> WorkspaceMember {
    let mut actor = ActorIdentity::new(name, name, ActorKind::Human);
    actor.account_id = Some(p.account_id);
    WorkspaceMember { id: name.into(), actor, role, title: String::new(), index: 0, joined_at: Timestamp::epoch() }
}

fn ts(s: &str) -> Timestamp {
    serde_json::from_value(serde_json::json!(s)).unwrap()
}

impl World {
    fn new() -> Self {
        let people: BTreeMap<&'static str, Person> = NAMES
            .iter()
            .map(|n| {
                (
                    *n,
                    Person {
                        account_id: Uuid::new_v4(),
                        account: SigningKeypair::generate().unwrap(),
                        device_id: Uuid::new_v4(),
                        device: SigningKeypair::generate().unwrap(),
                    },
                )
            })
            .collect();
        let mut w = Self { wid: Uuid::new_v4(), clock: 0, cfg: Vec::new(), people: std::sync::Arc::new(people) };
        let mut seed = ChatSession::new("", w.wid, "");
        seed.id = workspace_config_session_id(w.wid);
        seed.creator_actor_id = "owner".into();
        seed.members = vec![member("owner", &w.people["owner"], WorkspaceRole::Owner)];
        w.push("owner", SessionEvent::SessionSnapshot { session: Box::new(seed) });
        for name in NAMES {
            let p = &w.people[name];
            let (account_id, pk) = (p.account_id, p.account.public_key_bytes().to_vec());
            let cert = DeviceCertificate::issue(&p.account, p.account_id, p.device_id, &p.device.public_key_bytes(), Timestamp::epoch());
            w.push(name, SessionEvent::AccountKeyRegistered { account_id, signing_public_key: pk });
            w.push(name, SessionEvent::DeviceCertificateAdded { certificate: cert });
        }
        for (name, role) in [("ada", WorkspaceRole::Admin), ("mara", WorkspaceRole::Viewer)] {
            let m = member(name, &w.people[name], role);
            w.push("owner", SessionEvent::MemberAdded { member: m, invite: None });
        }
        for name in NAMES {
            let device_id = w.people[name].device_id;
            w.push(name, SessionEvent::DeviceKeyAgreementPublished { device_id, ka_public: vec![name.len() as u8; 32] });
        }
        w
    }

    fn draft(&mut self, by: &str, payload: SessionEvent) -> SessionEventEnvelope {
        self.clock += 1;
        let p = &self.people[by];
        let mut env = SessionEventEnvelope::new(workspace_config_session_id(self.wid), self.wid, self.clock as i64, payload);
        env.lamport = self.clock;
        let mut actor = ActorIdentity::new(by, by, ActorKind::Human);
        actor.account_id = Some(p.account_id);
        env.actor_stamp = Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false });
        sign_envelope(&mut env, p.device_id, &p.device);
        env
    }

    fn push(&mut self, by: &str, payload: SessionEvent) -> Uuid {
        let env = self.draft(by, payload);
        self.push_env(env)
    }

    fn push_env(&mut self, env: SessionEventEnvelope) -> Uuid {
        let id = env.event_id;
        self.cfg.push(env);
        id
    }

    fn verdict(&self, id: Uuid) -> Verdict {
        fold_decisions(&self.cfg, None, None, WorkspaceAuthz::Unchecked)[&id]
    }

    fn roster(&self) -> RosterState {
        project_config(&self.cfg, None, None).1.current().clone()
    }

    fn account(&self, name: &str) -> Uuid {
        self.people[name].account_id
    }

    /// A well-formed SSH-signed link for `name` in this workspace (the
    /// SSHSIG itself is checked device-locally, never in the fold).
    fn link(&self, name: &str, subject: &str) -> IdentityLink {
        self.link_in(name, subject, self.wid)
    }

    fn link_in(&self, name: &str, subject: &str, wid: Uuid) -> IdentityLink {
        let p = &self.people[name];
        let mut l = IdentityLink {
            link_id: Uuid::new_v4(),
            account_id: p.account_id,
            provider: "github".into(),
            host: "github.com".into(),
            subject_id: subject.into(),
            handle_at_link: format!("{name}-gh"),
            method: LinkMethod::SshSig,
            statement: String::new(),
            proof: "-----BEGIN SSH SIGNATURE-----\n...\n-----END SSH SIGNATURE-----".into(),
            key_fingerprint: "SHA256:test".into(),
            nonce: "AAECAwQFBgcICQoLDA0ODw".into(),
            issued_at: ts("2026-11-14T12:00:00Z"),
            expires_at: ts("2027-11-14T12:00:00Z"),
        };
        l.statement = render_statement(&l, wid, &p.account.public_key_bytes());
        l
    }
}

fn publish(l: IdentityLink) -> SessionEvent {
    SessionEvent::IdentityLinkPublished { link: l }
}

#[test]
fn own_link_folds_and_supersedes_by_subject() {
    let mut w = World::new();
    let l = w.link("mara", "100");
    let id = w.push("mara", publish(l.clone()));
    assert_eq!(w.verdict(id), Verdict::Allowed, "a Viewer links their own identity");
    assert_eq!(w.roster().links[&w.account("mara")].len(), 1);
    // The same (provider, host, subject) again: the newer one replaces it.
    let mut newer = w.link("mara", "100");
    newer.handle_at_link = "renamed".into();
    newer.statement = render_statement(&newer, w.wid, &w.people["mara"].account.public_key_bytes());
    let id = w.push("mara", publish(newer.clone()));
    assert_eq!(w.verdict(id), Verdict::Allowed);
    let links = &w.roster().links[&w.account("mara")];
    assert_eq!(links.len(), 1);
    assert_eq!(links.values().next().unwrap().handle_at_link, "renamed");
    // Removed by the account itself.
    let id = w.push("mara", SessionEvent::IdentityLinkRemoved { account_id: w.account("mara"), link_id: newer.link_id });
    assert_eq!(w.verdict(id), Verdict::Allowed);
    assert!(!w.roster().links.contains_key(&w.account("mara")));
}

/// S2 `link_replayed_by_other_account_invalid`: Ada can't publish Mara's
/// link, nor rebind it to her own account (the statement names Mara's
/// account and key).
#[test]
fn link_replayed_by_other_account_invalid() {
    let mut w = World::new();
    let mara = w.link("mara", "100");
    let id = w.push("ada", publish(mara.clone()));
    assert_eq!(w.verdict(id), Verdict::Denied, "only the account itself");
    let mut stolen = mara.clone();
    stolen.account_id = w.account("ada");
    let id = w.push("ada", publish(stolen));
    assert_eq!(w.verdict(id), Verdict::Denied, "the statement names Mara");
    assert!(w.roster().links.is_empty());
}

/// Cross-workspace replay: a link (and its statement) published in another
/// workspace is refused here.
#[test]
fn cross_workspace_replay_rejected() {
    let mut w = World::new();
    let elsewhere = w.link_in("mara", "100", Uuid::new_v4());
    let id = w.push("mara", publish(elsewhere));
    assert_eq!(w.verdict(id), Verdict::Denied);
}

/// After an account-key rotation a statement naming the old key no longer
/// folds (it binds the key in force at its position).
#[test]
fn statement_binds_the_account_key_at_its_position() {
    let mut w = World::new();
    let mut l = w.link("mara", "100");
    l.statement = render_statement(&l, w.wid, &SigningKeypair::generate().unwrap().public_key_bytes());
    let id = w.push("mara", publish(l));
    assert_eq!(w.verdict(id), Verdict::Denied);
}

/// "agent can't publish": an agent signing with its own certified key, and
/// a host-signed runtime effect, are both refused.
#[test]
fn agent_cannot_publish_a_link() {
    let mut w = World::new();
    let owner = &w.people["owner"];
    let (host, dev) = (owner.device_id, SigningKeypair::from_seed(&owner.device.seed_bytes()).unwrap());
    let mut agent = hive_core::WorkspaceAgent::new("coder", "gw");
    agent.owner_actor_id = "owner".into();
    agent.host_id = host.to_string();
    let agent_id = agent.id;
    w.push("owner", SessionEvent::AgentUpserted { agent });
    let akp = SigningKeypair::generate().unwrap();
    let cert = hive_core::AgentKeyCertificate::issue(&dev, host, agent_id, &akp.public_key_bytes(), Timestamp::epoch());
    let key_id = cert.key_id;
    w.push("owner", SessionEvent::AgentKeyCertified { cert });
    assert!(w.roster().valid_agent_key(key_id).is_some());

    // The agent, with its own key, for its owner's account.
    let l = w.link("owner", "200");
    let mut env = w.draft("owner", publish(l.clone()));
    let mut actor = ActorIdentity::new(agent_id.to_string(), "coder", ActorKind::Agent);
    actor.account_id = Some(w.account("owner"));
    env.actor_stamp = Some(ActorStamp { actor, recorded_at: Timestamp::epoch(), cfg_frontier: None, presence: None, runtime_effect: false });
    env.clear_raw();
    sign_envelope(&mut env, key_id, &akp);
    let id = w.push_env(env);
    assert_eq!(w.verdict(id), Verdict::Denied, "Agent tier");

    // A runtime effect signed by the owner's own device.
    let mut env = w.draft("owner", publish(l));
    env.actor_stamp.as_mut().unwrap().runtime_effect = true;
    env.clear_raw();
    sign_envelope(&mut env, host, &dev);
    let id = w.push_env(env);
    assert_eq!(w.verdict(id), Verdict::Denied, "runtime effect");
    assert!(w.roster().links.is_empty());
}

/// A non-member (Eve published her identity but was never let in) and a
/// forged or unsigned envelope (what a relay could inject) don't fold.
#[test]
fn outsider_forged_or_unsigned_links_are_refused() {
    let mut w = World::new();
    let id = w.push("eve", publish(w.link("eve", "300")));
    assert_eq!(w.verdict(id), Verdict::Denied, "not a member");
    // Signed by a device key the account never certified.
    let mut env = w.draft("mara", publish(w.link("mara", "100")));
    env.clear_raw();
    sign_envelope(&mut env, w.people["mara"].device_id, &SigningKeypair::generate().unwrap());
    let id = w.push_env(env);
    assert_eq!(w.verdict(id), Verdict::Denied, "bad signature");
    // Unsigned.
    let mut env = w.draft("mara", publish(w.link("mara", "101")));
    env.signer_device_id = None;
    env.signature = None;
    env.clear_raw();
    let id = w.push_env(env);
    assert_eq!(w.verdict(id), Verdict::Denied, "unsigned");
    assert!(w.roster().links.is_empty());
}

/// Bounded: at most 16 live links per account, and 8 KiB each.
#[test]
fn links_are_bounded_per_account() {
    let mut w = World::new();
    for i in 0..MAX_LIVE_LINKS {
        let id = w.push("mara", publish(w.link("mara", &format!("{i}"))));
        assert_eq!(w.verdict(id), Verdict::Allowed);
    }
    let id = w.push("mara", publish(w.link("mara", "999")));
    assert_eq!(w.verdict(id), Verdict::Denied, "the 17th");
    // Re-signing an existing subject still works (supersedes).
    let id = w.push("mara", publish(w.link("mara", "3")));
    assert_eq!(w.verdict(id), Verdict::Allowed);
    let mut big = w.link("ada", "1");
    big.proof = "x".repeat(9 * 1024);
    let id = w.push("ada", publish(big));
    assert_eq!(w.verdict(id), Verdict::Denied, "size cap");
}

#[test]
fn removal_is_the_account_or_an_admin_not_below() {
    let mut w = World::new();
    let ml = w.link("mara", "100");
    w.push("mara", publish(ml.clone()));
    let ol = w.link("owner", "1");
    w.push("owner", publish(ol.clone()));
    let id = w.push("eve", SessionEvent::IdentityLinkRemoved { account_id: w.account("mara"), link_id: ml.link_id });
    assert_eq!(w.verdict(id), Verdict::Denied, "an outsider");
    let id = w.push("mara", SessionEvent::IdentityLinkRemoved { account_id: w.account("owner"), link_id: ol.link_id });
    assert_eq!(w.verdict(id), Verdict::Denied, "a Viewer removing someone else's");
    let id = w.push("ada", SessionEvent::IdentityLinkRemoved { account_id: w.account("owner"), link_id: ol.link_id });
    assert_eq!(w.verdict(id), Verdict::Denied, "an Admin removing an Owner's");
    let id = w.push("ada", SessionEvent::IdentityLinkRemoved { account_id: w.account("mara"), link_id: ml.link_id });
    assert_eq!(w.verdict(id), Verdict::Allowed, "moderation");
}

/// S5 `link_never_grants_membership_or_seal_trust`: links change nothing
/// about members, roles, devices, agent keys or who a rotation seals to,
/// and a link naming an outsider's account doesn't let them in.
#[test]
fn link_never_grants_membership_or_seal_trust() {
    let mut w = World::new();
    let before = w.roster();
    for name in ["owner", "ada", "mara"] {
        let id = w.push(name, publish(w.link(name, &format!("{}", name.len()))));
        assert_eq!(w.verdict(id), Verdict::Allowed);
    }
    let after = w.roster();
    assert_eq!(after.members, before.members);
    assert_eq!(after.devices, before.devices);
    assert_eq!(after.agent_keys, before.agent_keys);
    assert_eq!(after.ka_keys, before.ka_keys);
    assert_eq!(after.rotation_recipients(), before.rotation_recipients());
    assert!(!after.rotation_recipients().contains_key(&w.people["eve"].device_id));
    assert!(after.role_of("eve").is_none());
    assert_eq!(after.links.len(), 3);
}

/// S10 `links_absent_from_unselected_workspaces`: a link published in one
/// workspace is not in another the same people share.
#[test]
fn links_absent_from_unselected_workspaces() {
    let mut a = World::new();
    let id = a.push("mara", publish(a.link("mara", "100")));
    assert_eq!(a.verdict(id), Verdict::Allowed);
    let b = World { wid: Uuid::new_v4(), clock: 0, cfg: Vec::new(), people: a.people.clone() };
    assert!(b.roster().links.is_empty());
    assert!(project_config(&b.cfg, None, None).1.current().links.is_empty());
}

#[test]
fn delegated_host_cannot_publish_its_owners_links() {
    let mut w = World::new();
    // Ada's desktop enrolls a box for her (a certificate the box didn't
    // publish itself: a delegated host).
    let ada = &w.people["ada"];
    let box_id = Uuid::new_v4();
    let box_key = SigningKeypair::generate().unwrap();
    let cert = DeviceCertificate::issue(&ada.account, ada.account_id, box_id, &box_key.public_key_bytes(), Timestamp::epoch());
    w.push("ada", SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert!(w.roster().devices[&box_id].delegated_for(w.account("ada")));
    let mut env = w.draft("ada", publish(w.link("ada", "7")));
    env.clear_raw();
    sign_envelope(&mut env, box_id, &box_key);
    let id = w.push_env(env);
    assert_eq!(w.verdict(id), Verdict::Denied);
}

/// A delegated host (a box acting for its owner without the account key)
/// can't remove its owner's identity links either: unlinking is as much the
/// account's own act as linking.
#[test]
fn delegated_host_cannot_remove_its_owners_links() {
    let mut w = World::new();
    let l = w.link("ada", "7");
    let id = w.push("ada", publish(l.clone()));
    assert_eq!(w.verdict(id), Verdict::Allowed);
    let ada = &w.people["ada"];
    let box_id = Uuid::new_v4();
    let box_key = SigningKeypair::generate().unwrap();
    let cert = DeviceCertificate::issue(&ada.account, ada.account_id, box_id, &box_key.public_key_bytes(), Timestamp::epoch());
    w.push("ada", SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert!(w.roster().devices[&box_id].delegated_for(w.account("ada")));
    let mut env = w.draft("ada", SessionEvent::IdentityLinkRemoved { account_id: w.account("ada"), link_id: l.link_id });
    env.clear_raw();
    sign_envelope(&mut env, box_id, &box_key);
    let id = w.push_env(env);
    assert_eq!(w.verdict(id), Verdict::Denied);
    assert!(w.roster().links[&w.account("ada")].contains_key(&l.link_id));
}

proptest! {
    /// S2 `prop_link_bound_to_account_and_key`: any change to the statement
    /// (one byte), or to a field it renders, is refused by the fold.
    #[test]
    fn prop_link_bound_to_account_and_key(pos in 0usize..400, byte in 0x20u8..0x7f, field in 0usize..5) {
        let mut w = World::new();
        let mut l = w.link("mara", "100");
        if field == 0 {
            let mut b = l.statement.clone().into_bytes();
            let i = pos % b.len();
            prop_assume!(b[i] != byte && b[i] != b'\n');
            b[i] = byte;
            l.statement = String::from_utf8(b).unwrap();
        } else {
            match field {
                1 => l.account_id = w.account("ada"),
                2 => l.subject_id = format!("{pos}x"),
                3 => l.host = "gitlab.com".into(),
                _ => l.issued_at = Timestamp(l.issued_at.0 + time::Duration::seconds(1 + pos as i64)),
            }
        }
        let id = w.push("mara", publish(l));
        prop_assert_eq!(w.verdict(id), Verdict::Denied);
    }
}
