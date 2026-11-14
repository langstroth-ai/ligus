//! Phase 0, B3: passkey presence in the fold, authored through ChatService
//! with a software authenticator (real ES256/EdDSA signatures over the real
//! event digest; no platform authenticator involved).

use std::sync::Arc;

use hive_core::presence::soft::SoftAuthenticator;
use hive_core::presence::{self, PresenceAssertion, RecoveryKey};
use hive_core::{
    fold_decisions, workspace_config_session_id, ActionProposal, ActorIdentity, ActorKind, MemberRoleChange,
    SessionEvent, SessionEventEnvelope, SigningKeypair, Timestamp, Verdict, WorkspaceAuthz, WorkspaceMember,
    WorkspaceRole,
};
use proptest::prelude::*;
use uuid::Uuid;

use crate::chat_service::{ChatService, PresenceProvider, PresenceRequest};
use crate::EventStore;

struct Soft(Arc<SoftAuthenticator>);
impl PresenceProvider for Soft {
    fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
        Ok(self.0.get(&req.challenge))
    }
}

struct Rec(Arc<RecoveryKey>);
impl PresenceProvider for Rec {
    fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
        Ok(self.0.assert(&req.challenge))
    }
}

/// A founder (actor id = account id, so the founder pin names it) on a
/// fresh workspace.
fn founder() -> (ChatService, Uuid) {
    let store = EventStore::open_in_memory().unwrap();
    let account = Uuid::new_v4();
    let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
    author.account_id = Some(account);
    let mut svc = ChatService::new(store, Uuid::new_v4(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
    let wid = Uuid::new_v4();
    svc.ensure_workspace_config(wid).unwrap();
    (svc, wid)
}

fn account(svc: &ChatService) -> Uuid {
    svc.author().account_id.unwrap()
}

fn register(svc: &mut ChatService, wid: Uuid, auth: &SoftAuthenticator, existing: Option<&SoftAuthenticator>) -> crate::chat_service::Result<SessionEventEnvelope> {
    let cred = auth.credential(account(svc), "test key");
    svc.register_passkey(wid, cred, &|req| Ok(match existing {
        None => auth.get(&req.challenge),
        Some(e) => e.get(&req.challenge),
    }))
}

fn member(id: &str, role: WorkspaceRole) -> WorkspaceMember {
    WorkspaceMember {
        id: id.into(),
        actor: ActorIdentity::new(id, id, ActorKind::Human),
        role,
        title: String::new(),
        index: 0,
        joined_at: Timestamp::now(),
    }
}

fn config_log(svc: &ChatService, wid: Uuid) -> Vec<SessionEventEnvelope> {
    svc.store().list(workspace_config_session_id(wid)).unwrap()
}

fn verdict(svc: &ChatService, wid: Uuid, id: Uuid) -> Verdict {
    fold_decisions(&config_log(svc, wid), None, None, WorkspaceAuthz::Unchecked)[&id]
}

/// Ana with a passkey and a recovery key, on rules v2.
fn on_rules_v2() -> (ChatService, Uuid, Arc<SoftAuthenticator>, Arc<RecoveryKey>) {
    let (mut svc, wid) = founder();
    let key = Arc::new(SoftAuthenticator::es256());
    register(&mut svc, wid, &key, None).unwrap();
    let rec = Arc::new(RecoveryKey::generate().unwrap());
    let rc = rec.credential(account(&svc), "Recovery key");
    let k2 = key.clone();
    svc.register_passkey(wid, rc, &move |req| Ok(k2.get(&req.challenge))).unwrap();
    svc.set_presence_provider(Some(Arc::new(Soft(key.clone()))));
    svc.set_workspace_rules(wid, 2).unwrap();
    svc.set_presence_provider(None);
    (svc, wid, key, rec)
}

#[test]
fn first_passkey_proves_itself_both_algorithms() {
    for key in [SoftAuthenticator::es256(), SoftAuthenticator::ed25519()] {
        let (mut svc, wid) = founder();
        let env = register(&mut svc, wid, &key, None).unwrap();
        assert_eq!(verdict(&svc, wid, env.event_id), Verdict::Allowed);
        assert_eq!(svc.passkeys_of(wid, account(&svc)).unwrap().len(), 1);
    }
}

#[test]
fn first_passkey_with_a_forged_proof_is_refused() {
    let (mut svc, wid) = founder();
    let key = SoftAuthenticator::es256();
    let other = SoftAuthenticator::es256();
    let cred = key.credential(account(&svc), "k");
    // An assertion by another authenticator (no proof of the new key).
    assert!(svc.register_passkey(wid, cred.clone(), &|req| Ok(other.get(&req.challenge))).is_err());
    // A create isn't an assertion over the registration.
    assert!(svc.register_passkey(wid, cred.clone(), &|req| Ok(key.create(&req.challenge))).is_err());
    // An assertion over another challenge.
    assert!(svc.register_passkey(wid, cred, &|_| Ok(key.get(&[0u8; 32]))).is_err());
}

#[test]
fn second_passkey_needs_existing_presence() {
    let (mut svc, wid) = founder();
    let first = SoftAuthenticator::es256();
    register(&mut svc, wid, &first, None).unwrap();
    let second = SoftAuthenticator::ed25519();
    assert!(register(&mut svc, wid, &second, None).is_err(), "a second key can't prove itself alone");
    let stranger = SoftAuthenticator::es256();
    assert!(register(&mut svc, wid, &second, Some(&stranger)).is_err(), "presence from an unregistered key");
    register(&mut svc, wid, &second, Some(&first)).unwrap();
    assert_eq!(svc.passkeys_of(wid, account(&svc)).unwrap().len(), 2);
}

#[test]
fn rules_v1_needs_no_presence_v2_does() {
    let (mut svc, wid) = founder();
    let cfg = workspace_config_session_id(wid);
    svc.add_member(cfg, wid, member("bea", WorkspaceRole::Contributor)).unwrap();
    let before = svc.set_member_role(cfg, wid, "bea", WorkspaceRole::Viewer);
    assert!(before.is_ok(), "rules v1: no presence needed");
    // v2 can't be turned on without a passkey and a recovery key.
    assert!(svc.set_workspace_rules(wid, 2).is_err());
    let key = Arc::new(SoftAuthenticator::es256());
    register(&mut svc, wid, &key, None).unwrap();
    let rec = RecoveryKey::generate().unwrap();
    let k = key.clone();
    svc.register_passkey(wid, rec.credential(account(&svc), "Recovery"), &move |req| Ok(k.get(&req.challenge))).unwrap();
    let err = svc.set_workspace_rules(wid, 2).unwrap_err().to_string();
    assert!(err.contains(hive_core::authz::PRESENCE_REQUIRED), "the switch itself needs presence: {err}");
    svc.set_presence_provider(Some(Arc::new(Soft(key.clone()))));
    let switch = svc.set_workspace_rules(wid, 2).unwrap();
    svc.set_presence_provider(None);
    assert!(switch.actor_stamp.as_ref().unwrap().presence.is_some());
    // Under v2 a role change needs presence.
    let err = svc.set_member_role(cfg, wid, "bea", WorkspaceRole::Contributor).unwrap_err().to_string();
    assert!(err.contains(hive_core::authz::PRESENCE_REQUIRED), "{err}");
    svc.set_presence_provider(Some(Arc::new(Soft(key))));
    svc.set_member_role(cfg, wid, "bea", WorkspaceRole::Contributor).unwrap();
    // Prefix-stable: everything before the switch is still allowed.
    let log = config_log(&svc, wid);
    let v = fold_decisions(&log, None, None, WorkspaceAuthz::Unchecked);
    assert!(log.iter().filter(|e| v.contains_key(&e.event_id)).all(|e| v[&e.event_id] == Verdict::Allowed));
}

#[test]
fn recovery_key_assertion_accepted() {
    let (mut svc, wid, _key, rec) = on_rules_v2();
    let cfg = workspace_config_session_id(wid);
    svc.set_presence_provider(Some(Arc::new(Rec(rec))));
    let env = svc.add_member(cfg, wid, member("cal", WorkspaceRole::Contributor));
    assert!(env.is_ok(), "a recovery key stands in for a lost passkey: {env:?}");
}

#[test]
fn presence_rejects_unregistered_and_other_accounts_credentials() {
    let (mut svc, wid, _key, _rec) = on_rules_v2();
    let cfg = workspace_config_session_id(wid);
    let stranger = Arc::new(SoftAuthenticator::es256());
    svc.set_presence_provider(Some(Arc::new(Soft(stranger))));
    assert!(svc.add_member(cfg, wid, member("dee", WorkspaceRole::Viewer)).is_err(), "an unregistered credential");
    // Another account's registered credential can't confirm for Ana.
    let (mut bob, bwid) = founder();
    let bob_key = Arc::new(SoftAuthenticator::es256());
    register(&mut bob, bwid, &bob_key, None).unwrap();
    let _ = bwid;
    svc.set_presence_provider(Some(Arc::new(Soft(bob_key))));
    assert!(svc.add_member(cfg, wid, member("dee", WorkspaceRole::Viewer)).is_err());
}

/// A captured (serve) draft plus a valid assertion over it, and the same
/// assertion moved or the envelope changed afterwards.
fn signed_with_presence() -> (ChatService, Uuid, SessionEventEnvelope, Arc<SoftAuthenticator>) {
    let (mut svc, wid, key, _rec) = on_rules_v2();
    let cfg = workspace_config_session_id(wid);
    svc.capture_presence(true);
    assert!(svc.add_member(cfg, wid, member("eli", WorkspaceRole::Viewer)).is_err());
    let draft = svc.take_presence_draft().expect("captured");
    let a = key.get(&draft.request.challenge);
    let env = svc.commit_presence(draft, a).unwrap();
    assert_eq!(verdict(&svc, wid, env.event_id), Verdict::Allowed);
    (svc, wid, env, key)
}

#[test]
fn presence_tamper_strip_and_replay_are_rejected() {
    let (svc, wid, env, key) = signed_with_presence();
    let mut log = config_log(&svc, wid);
    // Strip: the device signature covers the stamp, so removing the
    // assertion breaks it; a re-signed copy without it lacks presence.
    let mut stripped = env.clone();
    stripped.clear_raw();
    stripped.actor_stamp.as_mut().unwrap().presence = None;
    stripped.event_id = Uuid::new_v4();
    assert!(hive_core::verify_envelope(&stripped, &svc.device_public_key()).is_err(), "stripping breaks the device signature");
    // Replay: the same assertion on another event (a different member).
    let mut replay = env.clone();
    replay.clear_raw();
    replay.event_id = Uuid::new_v4();
    replay.lamport += 1;
    if let SessionEvent::MemberAdded { member, .. } = &mut replay.payload {
        member.id = "mallory".into();
        member.actor.id = "mallory".into();
    }
    for e in [&mut stripped, &mut replay] {
        e.signature = None;
        svc.sign_for_test(e);
        log.push(e.clone());
    }
    let v = fold_decisions(&log, None, None, WorkspaceAuthz::Unchecked);
    assert_eq!(v[&stripped.event_id], Verdict::Denied, "no presence under v2");
    assert_eq!(v[&replay.event_id], Verdict::Denied, "an assertion signs one event's digest");
    let _ = key;
}

#[derive(Debug, Clone)]
enum Mutation {
    Lamport(u64),
    EventId,
    Session,
    Payload,
    Frontier,
    Account,
}

proptest! {
    #[test]
    fn prop_presence_bound_to_event(m in prop_oneof![
        (1u64..1000).prop_map(Mutation::Lamport),
        Just(Mutation::EventId),
        Just(Mutation::Session),
        Just(Mutation::Payload),
        Just(Mutation::Frontier),
        Just(Mutation::Account),
    ]) {
        let (svc, wid, env, _key) = signed_with_presence();
        let mut changed = env.clone();
        changed.clear_raw();
        match m {
            Mutation::Lamport(d) => changed.lamport += d,
            Mutation::EventId => changed.event_id = Uuid::new_v4(),
            Mutation::Session => changed.session_id = Uuid::new_v4(),
            Mutation::Payload => {
                if let SessionEvent::MemberAdded { member, .. } = &mut changed.payload {
                    member.role = WorkspaceRole::Owner;
                }
            }
            Mutation::Frontier => {
                let s = changed.actor_stamp.as_mut().unwrap();
                s.cfg_frontier = s.cfg_frontier.map(|f| hive_core::CfgFrontier { lamport: f.lamport, event_id: Uuid::new_v4() });
            }
            Mutation::Account => changed.actor_stamp.as_mut().unwrap().actor.account_id = Some(Uuid::new_v4()),
        }
        prop_assert_ne!(presence::challenge(&changed), presence::challenge(&env), "every digest input is bound");
        // (Moved to another log, the event isn't on this config log at all;
        // the digest binding above is what matters.)
        if matches!(m, Mutation::Session) {
            return Ok(());
        }
        changed.event_id = if matches!(m, Mutation::EventId) { changed.event_id } else { Uuid::new_v4() };
        changed.signature = None;
        svc.sign_for_test(&mut changed);
        let mut log = config_log(&svc, wid);
        log.retain(|e| e.event_id != env.event_id);
        log.push(changed.clone());
        let v = fold_decisions(&log, None, None, WorkspaceAuthz::Unchecked);
        prop_assert_ne!(v.get(&changed.event_id).copied(), Some(Verdict::Allowed));
    }
}

#[test]
fn requires_presence_exhaustive() {
    use hive_core::authz::rules::requires_presence;
    use hive_core::authz::RosterState;
    let roster = RosterState::default();
    let fold = hive_core::ChatSession::new("c", Uuid::nil(), "x");
    let yes = [
        SessionEvent::MemberRemoved { member_id: "x".into() },
        SessionEvent::MemberRoleChanged {
            change: MemberRoleChange { member_id: "x".into(), old_role: WorkspaceRole::Viewer, new_role: WorkspaceRole::Admin },
        },
        SessionEvent::JoinApproved { subject: "x".into() },
        SessionEvent::DeviceRevoked { device_id: Uuid::nil() },
        SessionEvent::PasskeyRemoved { account_id: Uuid::nil(), credential_id: "c".into() },
        SessionEvent::ChatLockdownCleared { chat_id: Uuid::nil() },
        SessionEvent::WorkflowDefinitionsUpdated { definitions: vec![] },
        SessionEvent::AgentRemoved { agent_id: Uuid::nil() },
        SessionEvent::WorkspaceRuntimeRemoved { runtime_id: "r".into() },
        SessionEvent::WorkspaceRulesSet { version: 2 },
        SessionEvent::MemberAdded { member: member("someone-else", WorkspaceRole::Viewer), invite: None },
    ];
    for e in &yes {
        assert!(requires_presence(e, "me", &roster, &fold), "{} needs presence", e.kind_str());
    }
    let no = [
        SessionEvent::MemberAdded { member: member("me", WorkspaceRole::Viewer), invite: Some(Uuid::nil()) },
        SessionEvent::MessageRemoved { message_id: Uuid::nil() },
        SessionEvent::TurnClaimed { trigger_id: Uuid::nil(), device_id: Uuid::nil() },
        SessionEvent::DeviceKeyAgreementPublished { device_id: Uuid::nil(), ka_public: vec![] },
        SessionEvent::ProposalUpserted { proposal: ActionProposal::new("p", hive_core::ProposalKind::Decision, "a") },
        SessionEvent::Unknown,
    ];
    for e in &no {
        assert!(!requires_presence(e, "me", &roster, &fold), "{} needs no presence", e.kind_str());
    }
}

#[test]
fn rules_v2_judges_host_signed_agent_output_with_the_agent_tier() {
    // A host that can't certify a key for an agent signs its output itself.
    // Under rules v2 that output is held to the Agent tier: an "agent"
    // proposal that already carries approvals is refused.
    let (mut svc, wid, _key, _rec) = on_rules_v2();
    let chat = svc.create_chat("c", wid, "anthropic").unwrap();
    let agent = Uuid::new_v4();
    let mut p = ActionProposal::new("Ship", hive_core::ProposalKind::Decision, "agent");
    p.author_actor_id = String::new();
    p.producing_agent_id = Some(agent);
    p.approvals.push(hive_core::ProposalApproval {
        actor_id: "x".into(),
        role: WorkspaceRole::Owner,
        approved: true,
        created_at: Timestamp::now(),
        diff_digest: None,
    });
    let err = svc.upsert_proposal(chat.id, wid, p.clone());
    assert!(err.is_err(), "the Agent tier refuses an agent proposal that asserts approvals");
    p.approvals.clear();
    assert!(svc.upsert_proposal(chat.id, wid, p).is_ok());
}


/// P0.6 final (B3): a box enrolled as its owner's device signs the phone's
/// vote; it counts only with the owner's passkey assertion over it, and then
/// as the owner's (the credential's account), not as the box's.
#[test]
fn phone_presence_vote_counts_as_credential_owner_not_serve() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("hive.db");
    let account = Uuid::new_v4();
    let account_seed = SigningKeypair::generate().unwrap().seed_bytes();
    let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
    author.account_id = Some(account);
    let mut desk = ChatService::new(
        EventStore::open(&db).unwrap(),
        Uuid::new_v4(),
        SigningKeypair::generate().unwrap(),
        SigningKeypair::from_seed(&account_seed).unwrap(),
        author.clone(),
    );
    let wid = Uuid::new_v4();
    desk.ensure_workspace_config(wid).unwrap();
    let key = SoftAuthenticator::es256();
    register(&mut desk, wid, &key, None).unwrap();
    let chat = desk.create_chat("c", wid, "anthropic").unwrap().id;
    let p = ActionProposal::new("Ship", hive_core::ProposalKind::Decision, "agent");
    let pid = p.id;
    desk.upsert_proposal(chat, wid, p).unwrap();

    // The serve box: its own device key, enrolled by the desktop as Ana's.
    let box_key = SigningKeypair::generate().unwrap();
    let box_dev = Uuid::new_v4();
    desk.certify_device(&crate::device_enroll::DeviceEnrollment::new(wid, box_dev, &box_key, "box")).unwrap();
    let mut serve = ChatService::new(
        EventStore::open(&db).unwrap(),
        box_dev,
        box_key,
        SigningKeypair::generate().unwrap(),
        author,
    );
    serve.act_as_owner_device(desk.author().clone());

    // Without presence: refused (the fold would not count it either).
    let err = serve.vote_on_proposal(chat, wid, pid, true).unwrap_err().to_string();
    assert!(err.contains(hive_core::authz::PRESENCE_REQUIRED), "{err}");

    // prepare → the phone's passkey (an unregistered one is refused) → commit.
    serve.capture_presence(true);
    assert!(serve.vote_on_proposal(chat, wid, pid, true).is_err());
    let draft = serve.take_presence_draft().unwrap();
    assert_eq!(draft.request.account_id, account, "the passkey asked for is the owner's");
    let stranger = SoftAuthenticator::es256();
    assert!(serve.commit_presence(draft.clone(), stranger.get(&draft.request.challenge)).is_err());
    let env = serve.commit_presence(draft.clone(), key.get(&draft.request.challenge)).unwrap();
    assert_eq!(env.signer_device_id, Some(box_dev), "the box signs the envelope");
    assert!(serve.commit_presence(draft, key.get(&[0; 32])).is_err(), "single use");

    let s = desk.load(chat).unwrap().unwrap();
    let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
    assert_eq!(p.approvals.len(), 1);
    assert_eq!(p.approvals[0].actor_id, account.to_string(), "counted as the credential's account");
}

/// Rules v2: the chat's primary runtime has no agent identity; its tool
/// effects are host-signed and stamped `runtime_effect`, and every fold
/// judges them at the Agent tier (no votes, no governance), while the same
/// host's own actions are unaffected.
#[test]
fn primary_runtime_effects_judged_at_agent_tier() {
    let (mut svc, wid, key, _rec) = on_rules_v2();
    let chat = svc.create_chat("c", wid, "anthropic").unwrap().id;
    let p = ActionProposal::new("Ship", hive_core::ProposalKind::Decision, "runtime");
    let pid = p.id;
    let filed = svc.as_agent(None, |s| s.upsert_proposal(chat, wid, p));
    assert!(filed.is_ok(), "the runtime files an open proposal: {filed:?}");
    let last = svc.store().list(chat).unwrap().into_iter().last().unwrap();
    assert!(last.actor_stamp.as_ref().unwrap().runtime_effect, "stamped as a runtime effect");
    // A vote from the runtime's tool loop is refused, even with a passkey at hand.
    svc.set_presence_provider(Some(Arc::new(Soft(key))));
    let vote = svc.as_agent(None, |s| s.vote_on_proposal(chat, wid, pid, true));
    assert!(vote.is_err(), "the runtime can't vote");
    let cfg = workspace_config_session_id(wid);
    let gov = svc.as_agent(None, |s| s.add_member(cfg, wid, member("zed", WorkspaceRole::Admin)));
    assert!(gov.unwrap_err().to_string().contains("runtime"), "the runtime can't govern");
    // The person's own vote, outside the runtime's effects, still counts.
    svc.vote_on_proposal(chat, wid, pid, true).unwrap();
    // And a runtime-effect vote crafted by hand is refused by the fold.
    let mut forged = svc.store().list(chat).unwrap().into_iter().last().unwrap();
    forged.clear_raw();
    forged.event_id = Uuid::new_v4();
    forged.actor_stamp.as_mut().unwrap().runtime_effect = true;
    forged.signature = None;
    svc.sign_for_test(&mut forged);
    let mut log = svc.store().list(chat).unwrap();
    log.push(forged.clone());
    let pin = svc.store().workspace_pin(wid).unwrap().unwrap().founder_account.to_string();
    let (_, t) = svc.store().load_config(wid, Some(&pin), None).unwrap();
    let v = fold_decisions(&log, None, None, WorkspaceAuthz::Timeline(&t));
    assert_eq!(v[&forged.event_id], Verdict::Denied);
}

#[test]
fn removing_a_passkey_needs_presence_under_v2_and_then_it_no_longer_counts() {
    let (mut svc, wid, key, rec) = on_rules_v2();
    let account = account(&svc);
    let id = key.credential(account, "").credential_id;
    assert!(svc.remove_passkey(wid, account, &id).is_err(), "needs a passkey confirmation");
    svc.set_presence_provider(Some(Arc::new(Rec(rec))));
    svc.remove_passkey(wid, account, &id).unwrap();
    // The removed passkey can't confirm anything any more.
    svc.set_presence_provider(Some(Arc::new(Soft(key))));
    let cfg = workspace_config_session_id(wid);
    assert!(svc.add_member(cfg, wid, member("fay", WorkspaceRole::Viewer)).is_err());
}

/// Rules v2: touching a host someone else owns needs presence, including
/// re-owning it under the same id (a takeover looks like a self-owned host).
#[test]
fn taking_over_a_foreign_host_needs_presence() {
    use hive_core::authz::rules::requires_presence;
    use hive_core::authz::RosterState;
    let roster = RosterState::default();
    let mut fold = hive_core::ChatSession::new("c", Uuid::nil(), "x");
    let mut theirs = hive_core::WorkspaceHost::new("box1", hive_core::HostKind::Worker, "box1");
    theirs.owner_actor_id = "ana".into();
    fold.workspace_hosts.push(theirs.clone());
    let mut mine = theirs.clone();
    mine.owner_actor_id = "me".into();
    assert!(requires_presence(&SessionEvent::WorkspaceHostUpserted { host: mine.clone() }, "me", &roster, &fold));
    assert!(requires_presence(&SessionEvent::WorkspaceHostsUpdated { hosts: vec![mine] }, "me", &roster, &fold));
}

/// An Admin may clear a member's lost credentials, but not strip an Owner's
/// (passkeys or recovery key): after that the Owner's account would take a
/// new "first" passkey on its device key alone (TOFU).
#[test]
fn an_admin_cannot_remove_an_owners_passkey() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("hive.db");
    let ana_account = Uuid::new_v4();
    let mut ana_author = ActorIdentity::new(ana_account.to_string(), "Ana", ActorKind::Human);
    ana_author.account_id = Some(ana_account);
    let mut ana = ChatService::new(EventStore::open(&db).unwrap(), Uuid::new_v4(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), ana_author);
    let wid = Uuid::new_v4();
    ana.ensure_workspace_config(wid).unwrap();
    let key = SoftAuthenticator::es256();
    register(&mut ana, wid, &key, None).unwrap();
    let cfg = workspace_config_session_id(wid);
    let bea_account = Uuid::new_v4();
    let mut bea_author = ActorIdentity::new(bea_account.to_string(), "Bea", ActorKind::Human);
    bea_author.account_id = Some(bea_account);
    let mut m = member(&bea_account.to_string(), WorkspaceRole::Admin);
    m.actor = bea_author.clone();
    ana.add_member(cfg, wid, m).unwrap();
    let bea_dev = SigningKeypair::generate().unwrap();
    let bea_dev_id = Uuid::new_v4();
    let bea_acct_key = SigningKeypair::generate().unwrap();
    let mut bea = ChatService::new(EventStore::open(&db).unwrap(), bea_dev_id, bea_dev, bea_acct_key, bea_author);
    bea.ensure_workspace_config(wid).unwrap();
    let id = key.credential(ana_account, "").credential_id;
    let r = bea.remove_passkey(wid, ana_account, &id);
    assert!(r.is_err(), "an Admin removed the Owner's passkey: {r:?}");
    assert_eq!(ana.passkeys_of(wid, ana_account).unwrap().len(), 1);
}
