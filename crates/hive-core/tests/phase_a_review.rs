//! Adversarial review of the Phase A protocol before the 2.0 wire freeze:
//! each test is a counterexample to a Phase A claim, written to fail on the
//! branch as reviewed and pass once the fix that follows it lands.

#![allow(dead_code, unused_imports)]

use hive_core::crypto::DeviceCertificate;
use hive_core::flow::{FlowRecord, HumanAnswered};
use hive_core::presence::PresencePolicy;
use hive_core::testkit::{Phone, SignedWorld};
use hive_core::watermark::heads_of;
use hive_core::{workspace_config_session_id, ActorKind, ChatMessage, MessageRole, SessionEvent, Timestamp, Verdict, WorkspaceRole};
use std::collections::BTreeSet;
use uuid::Uuid;

/// A config event by `by` at an exact lamport (a backdated write), with
/// their recovery-key presence.
fn cfg_at(w: &mut SignedWorld, by: &str, payload: SessionEvent, lamport: u64) -> Uuid {
    let mut env = w.draft(by, None, payload, None);
    env.lamport = lamport;
    env.clear_raw();
    let a = w.recovery_presence(by, &env);
    w.finish(&mut env, by, Some(a));
    w.push_env(None, env)
}

/// The honest watermark of `who`'s devices on every log of `w`.
fn honest_watermark(w: &SignedWorld, who: &str) -> Vec<hive_core::watermark::Watermark> {
    let signers: BTreeSet<Uuid> = [w.people[who].device_id].into_iter().collect();
    let mut out = heads_of(&w.cfg, workspace_config_session_id(w.wid), &signers);
    for (id, log) in &w.chats {
        out.extend(heads_of(log, *id, &signers));
    }
    out
}

fn lamport_of(w: &SignedWorld, id: Uuid) -> u64 {
    w.cfg.iter().find(|e| e.event_id == id).unwrap().lamport
}

/// Claim 2: a member names, as their event's `prev`, an id they never
/// publish. The remover's watermark reaches it through the chain, so after
/// the removal the member publishes an event under that id (any content,
/// any frontier) and it counts as covered. A watermark must cover only the
/// events its author held.
#[test]
fn a_reserved_prev_id_is_not_a_covered_slot_after_removal() {
    let mut w = SignedWorld::genesis(&["owner", "con"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    let chat = w.chat("owner", "c");
    let old_frontier = w.head();
    // Con's post names a reserved, never-published prev.
    let reserved = Uuid::new_v4();
    let mut env = w.draft("con", Some(chat), SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "con", "hi") }, None);
    env.actor_stamp.as_mut().unwrap().prev = Some(reserved);
    w.finish(&mut env, "con", None);
    let honest = w.push_env(Some(chat), env);
    let wm = honest_watermark(&w, "con");
    let removal = w.push("owner", SessionEvent::MemberRemoved { member_id: "con".into(), watermark: wm });
    assert_eq!(w.verdict(removal), Verdict::Allowed);
    // After the removal: an event under the reserved id, backdated.
    let mut late = w.draft("con", Some(chat), SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "con", "after removal") }, old_frontier);
    late.event_id = reserved;
    late.actor_stamp.as_mut().unwrap().prev = None;
    w.finish(&mut late, "con", None);
    let late = w.push_env(Some(chat), late);
    let v = w.verdicts();
    assert_eq!(v[&honest], Verdict::Allowed, "the post the remover held stands");
    assert_eq!(v[&late], Verdict::Denied, "an event the remover never held is not covered");
}

/// Claim 8: a service member never satisfies a human gate. Answering an
/// agent's question is one; the check reads only the stamp's self-declared
/// kind, which the service's device signs itself.
#[test]
fn a_service_member_cannot_answer_as_a_human_by_stamping_itself_human() {
    let mut w = SignedWorld::genesis(&["owner", "box"], None);
    w.kinds.insert("box".into(), ActorKind::Service);
    w.admit("owner", "box", WorkspaceRole::Contributor);
    let chat = w.chat("owner", "c");
    let answer = SessionEvent::FlowRecorded {
        record: FlowRecord::HumanAnswered(HumanAnswered { ask_id: Uuid::new_v4(), answer: "yes".into(), option_index: None, by: "box".into(), at_ms: 0 }),
    };
    // Honestly stamped (Service): refused already.
    let honest = w.say(chat, "box", answer.clone());
    // Stamped Human by the service's own device.
    let mut env = w.draft("box", Some(chat), answer, None);
    env.actor_stamp.as_mut().unwrap().actor.kind = ActorKind::Human;
    w.finish(&mut env, "box", None);
    let lying = w.push_env(Some(chat), env);
    let v = w.verdicts();
    assert_eq!(v[&honest], Verdict::Denied);
    assert_eq!(v[&lying], Verdict::Denied, "the member record (Service), not the stamp, decides who is human");
}

/// Claim 7: a P-256 phone key certified under the Ed25519 tag (no
/// `\0alg=p256` in the account's signature) verifies P-256 envelopes (the
/// stamp picks the algorithm) but is not marked a phone, so it escapes the
/// phone tier and the always-presence enrollment. An Ed25519 certificate
/// must carry a 32-byte key, and the stamp's algorithm must be the
/// certificate's.
#[test]
fn a_p256_key_under_an_ed25519_certificate_is_refused() {
    let mut w = SignedWorld::genesis(&["owner", "mara"], None);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    let phone = Phone::new();
    let p = &w.people["mara"];
    let cert = DeviceCertificate::issue(&p.account, p.account_id, phone.device_id, &phone.public_key(), Timestamp::epoch());
    assert!(cert.verify(&p.account.public_key_bytes()).is_err(), "an Ed25519 certificate names a 32-byte key");
    w.push("mara", SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert!(!w.roster().devices.contains_key(&phone.device_id), "the key is never certified");
    // So it can't sign at all: a device revocation of the owner's laptop
    // signed by the P-256 key is refused.
    let laptop = w.people["owner"].device_id;
    let gov = w.phone_write("mara", &phone, None, SessionEvent::DeviceRevoked { device_id: laptop, watermark: vec![] });
    assert_eq!(w.verdict(gov), Verdict::Denied);
}

/// Claim 5: the relying party decides which page every member's desktop
/// opens for each passkey ceremony (`PresencePolicy::page_url`), and what a
/// re-registration under it binds. Only the Owner may change it.
#[test]
fn only_the_owner_changes_the_relying_party() {
    let mut w = SignedWorld::genesis(&["owner", "ada"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    let evil = PresencePolicy::for_rp("evil.example");
    assert_eq!(evil.page_url(), "https://evil.example/presence");
    let by_admin = w.push("ada", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(evil.clone()) });
    assert_eq!(w.verdict(by_admin), Verdict::Denied, "an Admin can't point every member's passkey page elsewhere");
    assert_eq!(w.roster().presence_policy, PresencePolicy::ligus());
    let by_owner = w.push("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::for_rp("example.org")) });
    assert_eq!(w.verdict(by_owner), Verdict::Allowed);
}

/// Claim 1: the genesis `WorkspaceRulesSet` is the only presence-free rules
/// set. Before anyone holds a credential (a CLI-founded workspace), a second
/// presence-free one must not re-point the relying party.
#[test]
fn there_is_one_presence_free_genesis_rules_set() {
    let mut w = SignedWorld::genesis(&["owner"], None);
    // Keep the seed and the founder's identity only: no credential yet.
    w.cfg.truncate(3);
    let genesis = w.push_bare("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::ligus()) });
    let second = w.push_bare("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(PresencePolicy::for_rp("example.org")) });
    let v = w.verdicts();
    assert_eq!(v[&genesis], Verdict::Allowed);
    assert_eq!(v[&second], Verdict::Denied, "a second genesis");
    assert_eq!(w.roster().presence_policy, PresencePolicy::ligus());
}

/// Claims 2 and 5 rest on rank: "only the Owner" changes the version and
/// the relying party, and a cut must never reach above its author. An Admin
/// must not make anyone an Owner, nor revoke, demote or remove an Owner; and
/// a revocation must never erase the history of the device it revokes (here
/// the founder's, whose events are the workspace's genesis).
#[test]
fn an_admin_cannot_cut_above_them_or_erase_the_founders_history() {
    let mut w = SignedWorld::genesis(&["owner", "ada", "mara"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    let change = |m: &str, old, new| SessionEvent::MemberRoleChanged {
        change: hive_core::MemberRoleChange { member_id: m.into(), old_role: old, new_role: new },
        watermark: vec![],
    };
    let owners_device = w.people["owner"].device_id;
    let attempts = [
        w.push("ada", change("ada", WorkspaceRole::Admin, WorkspaceRole::Owner)),
        w.push("ada", change("mara", WorkspaceRole::Contributor, WorkspaceRole::Owner)),
        w.push("ada", change("owner", WorkspaceRole::Owner, WorkspaceRole::Viewer)),
        w.push("ada", SessionEvent::MemberRemoved { member_id: "owner".into(), watermark: vec![] }),
        // An empty watermark: "I held none of the founder's events".
        w.push("ada", SessionEvent::DeviceRevoked { device_id: owners_device, watermark: vec![] }),
    ];
    let v = w.verdicts();
    for id in attempts {
        assert_eq!(v[&id], Verdict::Denied);
    }
    // Not vacuous: the workspace is intact and Ada is still an Admin.
    let genesis_ids: Vec<Uuid> = w.cfg.iter().take(6).map(|e| e.event_id).collect();
    for id in genesis_ids.iter().skip(1) {
        assert_eq!(v[id], Verdict::Allowed, "the founder's genesis events stand");
    }
    let roster = w.roster();
    assert!(!roster.devices[&owners_device].revoked);
    assert_eq!(roster.role_of("owner"), Some(WorkspaceRole::Owner));
    assert_eq!(roster.role_of("ada"), Some(WorkspaceRole::Admin));
    assert_eq!(roster.role_of("mara"), Some(WorkspaceRole::Contributor));
}
