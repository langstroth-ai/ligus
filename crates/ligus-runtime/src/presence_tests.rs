//! Phase 0, B3: passkey presence in the fold, authored through ChatService
//! with a software authenticator (real ES256/EdDSA signatures over the real
//! event digest; no platform authenticator involved).

use std::sync::Arc;

use ligus_core::presence::soft::SoftAuthenticator;
use ligus_core::presence::{self, PresenceAssertion, RecoveryKey};
use ligus_core::{
    fold_decisions, workspace_config_session_id, ActionProposal, ActorIdentity, ActorKind, MemberRoleChange,
    SessionEvent, SessionEventEnvelope, SigningKeypair, Timestamp, Verdict, WorkspaceAuthz, WorkspaceMember,
    WorkspaceRole,
};
use proptest::prelude::*;
use uuid::Uuid;

use crate::chat_service::{ChatError, ChatService, PresenceProvider, PresenceRequest};
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
    let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), author);
    svc.disable_test_presence();
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
    svc.set_workspace_rules(wid, ligus_core::authz::RULES_NEW_WORKSPACE).unwrap();
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

/// Phase A, and rules 3 from 0.4: a workspace is on rules v3 from its seed.
/// The genesis `WorkspaceRulesSet` names the relying party without presence
/// (nobody has a credential yet); governance needs the founder's passkey
/// from the first event on; there is no rules v1 to switch to.
#[test]
fn a_new_workspace_is_on_rules_v2_from_its_seed_and_v3_from_its_first_passkey() {
    let (mut svc, wid) = founder();
    let cfg = workspace_config_session_id(wid);
    let log = config_log(&svc, wid);
    assert!(
        log.iter().any(|e| matches!(&e.payload, SessionEvent::WorkspaceRulesSet { version: 2, presence: Some(p) } if p.rp_id == "ligus.ai")),
        "genesis writes rules v2 with the deployment's relying party (a 0.3 build accepts it)"
    );
    assert_eq!(svc.load(cfg).unwrap().unwrap().authz.rules, 2);
    // Without a passkey the founder can't admit anyone.
    let err = svc.add_member(cfg, wid, member("bea", WorkspaceRole::Contributor)).unwrap_err().to_string();
    assert!(err.contains(ligus_core::authz::PRESENCE_REQUIRED), "{err}");
    // There is no rules v1.
    assert!(svc.set_workspace_rules(wid, 1).is_err());
    // The founder's first passkey registers with the seed's grant.
    let key = Arc::new(SoftAuthenticator::es256());
    register(&mut svc, wid, &key, None).unwrap();
    assert_eq!(svc.load(cfg).unwrap().unwrap().authz.rules, 3, "the founder's first passkey moves it to rules 3, with presence");
    assert!(!svc.workspace_secured(wid).unwrap(), "a passkey without a recovery key isn't secured yet");
    let rec = RecoveryKey::generate().unwrap();
    let k = key.clone();
    svc.register_passkey(wid, rec.credential(account(&svc), "Recovery"), &move |req| Ok(k.get(&req.challenge))).unwrap();
    assert!(svc.workspace_secured(wid).unwrap());
    svc.set_presence_provider(Some(Arc::new(Soft(key))));
    svc.add_member(cfg, wid, member("bea", WorkspaceRole::Contributor)).unwrap();
    let log = config_log(&svc, wid);
    let v = fold_decisions(&log, None, None, WorkspaceAuthz::Unchecked);
    assert!(log.iter().filter(|e| v.contains_key(&e.event_id)).all(|e| v[&e.event_id] == Verdict::Allowed));
}

/// A forged `WorkspaceRulesSet { version: 1 }` (signed, with the Owner's
/// presence) still doesn't fold: version 1 doesn't exist.
#[test]
fn rules_v1_is_refused_by_the_fold() {
    let (mut svc, wid, key, _rec) = on_rules_v2();
    let mut env = svc.draft_signed(workspace_config_session_id(wid), wid, SessionEvent::WorkspaceRulesSet { version: 1, presence: None }).unwrap();
    let a = key.get(&presence::challenge(&env));
    env.actor_stamp.as_mut().unwrap().presence = Some(a);
    env.clear_raw();
    svc.sign_for_test(&mut env);
    let mut log = config_log(&svc, wid);
    log.push(env.clone());
    assert_eq!(fold_decisions(&log, None, None, WorkspaceAuthz::Unchecked)[&env.event_id], Verdict::Denied);
    let (cfg, _) = ligus_core::project_config(&log, None, None);
    assert_eq!(cfg.unwrap().authz.rules, ligus_core::authz::RULES_NEW_WORKSPACE);
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
    assert!(ligus_core::verify_envelope(&stripped, &svc.device_public_key()).is_err(), "stripping breaks the device signature");
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
                s.cfg_frontier = s.cfg_frontier.map(|f| ligus_core::CfgFrontier { lamport: f.lamport, event_id: Uuid::new_v4() });
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
    use ligus_core::authz::rules::requires_presence;
    use ligus_core::authz::RosterState;
    let roster = RosterState::default();
    let fold = ligus_core::ChatSession::new("c", Uuid::nil(), "x");
    let yes = [
        SessionEvent::MemberRemoved { member_id: "x".into(), watermark: Vec::new() },
        SessionEvent::MemberRoleChanged {
            change: MemberRoleChange { member_id: "x".into(), old_role: WorkspaceRole::Viewer, new_role: WorkspaceRole::Admin }, watermark: Vec::new(),
        },
        SessionEvent::JoinApproved { subject: "x".into(), admission: None, redeem_key: None },
        SessionEvent::DeviceRevoked { device_id: Uuid::nil(), watermark: Vec::new() },
        SessionEvent::PasskeyRemoved { account_id: Uuid::nil(), credential_id: "c".into() },
        SessionEvent::ChatLockdownCleared { chat_id: Uuid::nil(), raises: vec![Uuid::nil()] },
        SessionEvent::WorkflowDefinitionsUpdated { definitions: vec![] },
        SessionEvent::AgentRemoved { agent_id: Uuid::nil() },
        SessionEvent::WorkspaceRuntimeRemoved { runtime_id: "r".into() },
        SessionEvent::WorkspaceRulesSet { version: 2, presence: None },
        SessionEvent::MemberAdded { member: member("someone-else", WorkspaceRole::Viewer), invite: None, proof: None },
        SessionEvent::WorkspaceRepoRemoved { repo_id: Uuid::nil() },
    ];
    for e in &yes {
        assert!(requires_presence(e, "me", &roster, &fold), "{} needs presence", e.kind_str());
    }
    let no = [
        SessionEvent::MemberAdded { member: member("me", WorkspaceRole::Viewer), invite: Some(Uuid::nil()), proof: None },
        SessionEvent::MessageRemoved { message_id: Uuid::nil() },
        SessionEvent::TurnClaimed { trigger_id: Uuid::nil(), device_id: Uuid::nil(), epoch: 0, supersedes: None, repo_id: None },
        SessionEvent::DeviceKeyAgreementPublished { device_id: Uuid::nil(), ka_public: vec![] },
        SessionEvent::WorkspaceRepoUpdated { repo_id: Uuid::nil(), display_name: "x".into(), default_branch: String::new(), scratch: None },
        SessionEvent::ChatRepoSet { repo_id: None, base_branch: String::new() },
        SessionEvent::ChatBasePinned { repo_id: Uuid::nil(), commit: String::new() },
        SessionEvent::ProposalUpserted { proposal: ActionProposal::new("p", ligus_core::ProposalKind::Decision, "a") },
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
    let mut p = ActionProposal::new("Ship", ligus_core::ProposalKind::Decision, "agent");
    p.author_actor_id = String::new();
    p.producing_agent_id = Some(agent);
    p.approvals.push(ligus_core::ProposalApproval {
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
    let db = dir.path().join("ligus.db");
    let account = Uuid::new_v4();
    let account_seed = SigningKeypair::generate().unwrap().seed_bytes();
    let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
    author.account_id = Some(account);
    let mut desk = ChatService::new(
        EventStore::open(&db).unwrap(),
        SigningKeypair::generate().unwrap(),
        SigningKeypair::from_seed(&account_seed).unwrap(),
        author.clone(),
    );
    desk.disable_test_presence();
    let wid = Uuid::new_v4();
    desk.ensure_workspace_config(wid).unwrap();
    let key = Arc::new(SoftAuthenticator::es256());
    register(&mut desk, wid, &key, None).unwrap();
    desk.set_presence_provider(Some(Arc::new(Soft(key.clone()))));
    let chat = desk.create_chat("c", wid, "anthropic").unwrap().id;
    let p = ActionProposal::new("Ship", ligus_core::ProposalKind::Decision, "agent");
    let pid = p.id;
    desk.upsert_proposal(chat, wid, p).unwrap();

    // The serve box: its own device key, enrolled by the desktop as Ana's.
    let box_key = SigningKeypair::generate().unwrap();
    let box_dev = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &box_key.public_key_bytes());
    desk.certify_device(&crate::device_enroll::DeviceEnrollment::new(wid, &box_key, "box")).unwrap();
    let mut serve = ChatService::new(
        EventStore::open(&db).unwrap(),
        box_key,
        SigningKeypair::generate().unwrap(),
        author,
    );
    serve.disable_test_presence();
    serve.act_as_owner_device(desk.author().clone());
    serve.accept_device_certificate(wid).unwrap();

    // Without presence: refused (the fold would not count it either).
    let err = serve.vote_on_proposal(chat, wid, pid, true).unwrap_err().to_string();
    assert!(err.contains(ligus_core::authz::PRESENCE_REQUIRED), "{err}");

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
    let p = ActionProposal::new("Ship", ligus_core::ProposalKind::Decision, "runtime");
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
    use ligus_core::authz::rules::requires_presence;
    use ligus_core::authz::RosterState;
    let roster = RosterState::default();
    let mut fold = ligus_core::ChatSession::new("c", Uuid::nil(), "x");
    let mut theirs = ligus_core::WorkspaceHost::new("box1", ligus_core::HostKind::Worker, "box1");
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
    let db = dir.path().join("ligus.db");
    let ana_account = Uuid::new_v4();
    let mut ana_author = ActorIdentity::new(ana_account.to_string(), "Ana", ActorKind::Human);
    ana_author.account_id = Some(ana_account);
    let mut ana = ChatService::new(EventStore::open(&db).unwrap(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), ana_author);
    ana.disable_test_presence();
    let wid = Uuid::new_v4();
    ana.ensure_workspace_config(wid).unwrap();
    let key = Arc::new(SoftAuthenticator::es256());
    register(&mut ana, wid, &key, None).unwrap();
    ana.set_presence_provider(Some(Arc::new(Soft(key.clone()))));
    let cfg = workspace_config_session_id(wid);
    let bea_account = Uuid::new_v4();
    let mut bea_author = ActorIdentity::new(bea_account.to_string(), "Bea", ActorKind::Human);
    bea_author.account_id = Some(bea_account);
    let mut m = member(&bea_account.to_string(), WorkspaceRole::Admin);
    m.actor = bea_author.clone();
    ana.add_member(cfg, wid, m).unwrap();
    let bea_dev = SigningKeypair::generate().unwrap();
    let bea_acct_key = SigningKeypair::generate().unwrap();
    let mut bea = ChatService::new(EventStore::open(&db).unwrap(), bea_dev, bea_acct_key, bea_author);
    bea.disable_test_presence();
    bea.ensure_workspace_config(wid).unwrap();
    let id = key.credential(ana_account, "").credential_id;
    let r = bea.remove_passkey(wid, ana_account, &id);
    assert!(r.is_err(), "an Admin removed the Owner's passkey: {r:?}");
    assert_eq!(ana.passkeys_of(wid, ana_account).unwrap().len(), 1);
}

/// The digest short id a person compares between the app and the passkey
/// page must not be grindable: 32 bits are (an attacker who can draft events
/// varies the event id until a different event shows the same short id).
/// 64 bits, and it is the prefix of the very challenge signed.
#[test]
fn digest_short_id_is_64_bits_of_the_challenge() {
    let (mut svc, wid, _key, _rec) = on_rules_v2();
    let cfg = workspace_config_session_id(wid);
    svc.capture_presence(true);
    assert!(svc.add_member(cfg, wid, member("gus", WorkspaceRole::Viewer)).is_err());
    let draft = svc.take_presence_draft().unwrap();
    assert_eq!(draft.request.digest_short, ligus_core::keys::hex(&draft.request.challenge[..8]));
}

/// Under rules v2, leaving v2 (back to v1, or on to a version this build
/// doesn't know) is itself presence-gated: a device key alone can't switch
/// presence off.
#[test]
fn rules_below_v2_are_refused_and_a_raise_needs_presence() {
    let (mut svc, wid, key, _rec) = on_rules_v2();
    for v in [1, 0] {
        let err = svc.set_workspace_rules(wid, v).unwrap_err().to_string();
        assert!(err.contains("doesn't exist"), "v{v}: {err}");
    }
    let err = svc.set_workspace_rules(wid, ligus_core::authz::RULES_SUPPORTED + 1).unwrap_err().to_string();
    assert!(err.contains(ligus_core::authz::PRESENCE_REQUIRED), "{err}");
    svc.set_presence_provider(Some(Arc::new(Soft(key))));
    svc.set_workspace_rules(wid, ligus_core::authz::RULES_SUPPORTED + 1).unwrap();
    assert!(svc.load(workspace_config_session_id(wid)).unwrap().unwrap().authz.read_only());
}

/// Ana (founder, actor id = account id) on rules v2 with a passkey and a
/// recovery key, and the account key her device holds (what a thief who
/// copied her device's secrets has too).
fn on_rules_v2_with_account_key() -> (ChatService, Uuid, Arc<SoftAuthenticator>, SigningKeypair) {
    let store = EventStore::open_in_memory().unwrap();
    let account = Uuid::new_v4();
    let seed = SigningKeypair::generate().unwrap().seed_bytes();
    let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
    author.account_id = Some(account);
    let mut svc = ChatService::new(store, SigningKeypair::generate().unwrap(), SigningKeypair::from_seed(&seed).unwrap(), author);
    svc.disable_test_presence();
    let wid = Uuid::new_v4();
    svc.ensure_workspace_config(wid).unwrap();
    let key = Arc::new(SoftAuthenticator::es256());
    register(&mut svc, wid, &key, None).unwrap();
    let rec = RecoveryKey::generate().unwrap();
    let k2 = key.clone();
    svc.register_passkey(wid, rec.credential(account, "Recovery key"), &move |req| Ok(k2.get(&req.challenge))).unwrap();
    svc.set_presence_provider(Some(Arc::new(Soft(key.clone()))));
    svc.set_workspace_rules(wid, ligus_core::authz::RULES_NEW_WORKSPACE).unwrap();
    svc.set_presence_provider(None);
    (svc, wid, key, SigningKeypair::from_seed(&seed).unwrap())
}

/// A copy of the newest config event re-dressed as `payload` on `session`,
/// signed by `svc`'s device without presence (what a device-key thief can
/// produce by hand).
fn forged(svc: &ChatService, wid: Uuid, session: Uuid, payload: SessionEvent) -> SessionEventEnvelope {
    let mut env = config_log(svc, wid).into_iter().max_by_key(|e| e.lamport).unwrap();
    env.clear_raw();
    env.event_id = Uuid::new_v4();
    env.session_id = session;
    env.lamport += 1000;
    env.payload = payload;
    env.actor_stamp.as_mut().unwrap().presence = None;
    env.signature = None;
    svc.sign_for_test(&mut env);
    env
}

/// Track B residual 1a: under rules v2 an account that holds a passkey adds
/// a device only with that passkey's presence. A thief holding the device and
/// account keys can't certify a device of their own, on the config log or by
/// publishing the certificate on a chat log.
#[test]
fn device_key_thief_cannot_add_a_device_under_v2() {
    let (mut svc, wid, key, account_kp) = on_rules_v2_with_account_key();
    let account = account(&svc);
    let cfg = workspace_config_session_id(wid);
    let thief_key = SigningKeypair::generate().unwrap();
    let thief_dev = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &thief_key.public_key_bytes());
    let req = crate::device_enroll::DeviceEnrollment::new(wid, &thief_key, "thief");
    let err = svc.certify_device(&req).unwrap_err().to_string();
    assert!(err.contains(ligus_core::authz::PRESENCE_REQUIRED), "{err}");

    // By hand: a certificate under the account key, without presence.
    let cert = ligus_core::crypto::DeviceCertificate::issue(&account_kp, account, &thief_key.public_key_bytes(), Timestamp::now());
    let on_cfg = forged(&svc, wid, cfg, SessionEvent::DeviceCertificateAdded { certificate: cert.clone() });
    let chat = svc.create_chat("c", wid, "anthropic").unwrap().id;
    let on_chat = forged(&svc, wid, chat, SessionEvent::DeviceCertificateAdded { certificate: cert });
    let mut log = config_log(&svc, wid);
    log.push(on_cfg.clone());
    assert_eq!(fold_decisions(&log, None, None, WorkspaceAuthz::Unchecked)[&on_cfg.event_id], Verdict::Denied);
    let (_, timeline) = ligus_core::project_config(&log, None, None);
    assert!(!timeline.current().device_trusted(thief_dev), "the config roster certified the thief's device");
    let mut trust = svc.store().trust_envelopes(wid).unwrap();
    trust.push(on_cfg);
    trust.push(on_chat);
    let roster = crate::envelope_verifier::build_roster(&trust);
    assert!(!roster.is_certified(thief_dev), "the ingest roster certified the thief's device");
    assert!(roster.is_certified(svc.device_id()), "Ana's own device stays certified");

    // With Ana's passkey it goes through, and re-publishing an already
    // certified device needs nothing.
    svc.set_presence_provider(Some(Arc::new(Soft(key))));
    svc.certify_device(&req).unwrap();
    svc.set_presence_provider(None);
    // Rules 3: allowed, and it counts once the device accepts it.
    let (_, timeline) = ligus_core::project_config(&config_log(&svc, wid), None, None);
    assert!(timeline.current().unaccepted.contains(&(thief_dev, account)));
    // Not in force until then, so publishing it again asks again.
    let err = svc.certify_device(&req).unwrap_err().to_string();
    assert!(err.contains(ligus_core::authz::PRESENCE_REQUIRED), "{err}");
}

/// Track B residual 1a (same gap, account keys): rotating an account key is
/// presence-gated under rules v2, so a rotation without presence must not
/// take effect anywhere. Otherwise a thief holding the account key re-pins
/// it and every device the owner certified drops out of the roster.
#[test]
fn device_key_thief_cannot_rotate_the_account_key_under_v2() {
    let (mut svc, wid, key, account_kp) = on_rules_v2_with_account_key();
    let account = account(&svc);
    let cfg = workspace_config_session_id(wid);
    let next = SigningKeypair::generate().unwrap();
    let err = svc.rotate_account_key(SigningKeypair::from_seed(&next.seed_bytes()).unwrap(), cfg, wid).unwrap_err().to_string();
    assert!(err.contains(ligus_core::authz::PRESENCE_REQUIRED), "{err}");
    let new_pub = next.public_key_bytes().to_vec();
    let rotation = SessionEvent::AccountKeyRotated {
        account_id: account,
        new_signing_public_key: new_pub.clone(),
        succession_signature: account_kp.sign(&new_pub).to_vec(),
    };
    let on_cfg = forged(&svc, wid, cfg, rotation.clone());
    let chat = svc.create_chat("c", wid, "anthropic").unwrap().id;
    let on_chat = forged(&svc, wid, chat, rotation);
    let mut log = config_log(&svc, wid);
    log.push(on_cfg.clone());
    assert_eq!(fold_decisions(&log, None, None, WorkspaceAuthz::Unchecked)[&on_cfg.event_id], Verdict::Denied);
    let (_, timeline) = ligus_core::project_config(&log, None, None);
    assert!(timeline.current().device_trusted(svc.device_id()), "the refused rotation dropped Ana's device from the config roster");
    let mut trust = svc.store().trust_envelopes(wid).unwrap();
    trust.push(on_cfg);
    trust.push(on_chat);
    let roster = crate::envelope_verifier::build_roster(&trust);
    assert_eq!(roster.account_signing_key(account), Some(&account_kp.public_key_bytes()[..]), "re-pinned without presence");
    assert!(roster.is_certified(svc.device_id()), "Ana's device dropped out of the ingest roster");

    // With her passkey the rotation takes effect (and re-certifies the device).
    svc.set_presence_provider(Some(Arc::new(Soft(key))));
    svc.rotate_account_key(next, cfg, wid).unwrap();
    let roster = crate::envelope_verifier::build_roster(&svc.store().trust_envelopes(wid).unwrap());
    assert_eq!(roster.account_signing_key(account), Some(&new_pub[..]));
    assert!(roster.is_certified(svc.device_id()));
}

/// Ana (founder, Owner) on rules v2 and a second person's service on the
/// same store: `who` becomes a member (by Ana, with her passkey) and
/// publishes her device identity.
fn second_person(dir: &std::path::Path, add: bool) -> (ChatService, ChatService, Uuid, Arc<SoftAuthenticator>, Uuid) {
    let db = dir.join("ligus.db");
    let ana_account = Uuid::new_v4();
    let mut ana_author = ActorIdentity::new(ana_account.to_string(), "Ana", ActorKind::Human);
    ana_author.account_id = Some(ana_account);
    let mut ana = ChatService::new(EventStore::open(&db).unwrap(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), ana_author);
    ana.disable_test_presence();
    let wid = Uuid::new_v4();
    ana.ensure_workspace_config(wid).unwrap();
    let key = Arc::new(SoftAuthenticator::es256());
    register(&mut ana, wid, &key, None).unwrap();
    let rec = RecoveryKey::generate().unwrap();
    let k2 = key.clone();
    ana.register_passkey(wid, rec.credential(ana_account, "Recovery key"), &move |req| Ok(k2.get(&req.challenge))).unwrap();
    let cfg = workspace_config_session_id(wid);
    let bea_account = Uuid::new_v4();
    let mut bea_author = ActorIdentity::new(bea_account.to_string(), "Bea", ActorKind::Human);
    bea_author.account_id = Some(bea_account);
    let mut m = member(&bea_account.to_string(), WorkspaceRole::Contributor);
    m.actor = bea_author.clone();
    // (`add == false`: Bea published her identity but was never let in.)
    ana.set_presence_provider(Some(Arc::new(Soft(key.clone()))));
    if add {
        ana.add_member(cfg, wid, m).unwrap();
    }
    ana.set_presence_provider(None);
    let mut bea = ChatService::new(EventStore::open(&db).unwrap(), SigningKeypair::generate().unwrap(), SigningKeypair::generate().unwrap(), bea_author);
    bea.disable_test_presence();
    if add {
        bea.publish_identity(cfg, wid).unwrap();
    }
    (ana, bea, wid, key, bea_account)
}

/// Track B residual 1b: after every credential of an account is removed, a
/// device-key thief can't register a new "first" passkey: that needs an
/// Admin's presence-confirmed re-grant first (B.4.6), and a re-grant is good
/// for one registration.
#[test]
fn first_passkey_after_all_credentials_removed_needs_a_regrant() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ana, mut bea, wid, ana_key, bea_account) = second_person(dir.path(), true);
    // Added by an Admin under v2: Bea's first passkey is allowed.
    let first = SoftAuthenticator::es256();
    register(&mut bea, wid, &first, None).unwrap();
    // Ana clears Bea's lost passkey (with her own passkey).
    ana.set_presence_provider(Some(Arc::new(Soft(ana_key.clone()))));
    ana.remove_passkey(wid, bea_account, &first.credential(bea_account, "").credential_id).unwrap();
    assert!(bea.passkeys_of(wid, bea_account).unwrap().is_empty());
    // Bea's device alone (a thief) can't register a new first passkey.
    let thief = SoftAuthenticator::es256();
    let err = register(&mut bea, wid, &thief, None).unwrap_err().to_string();
    assert!(err.contains("grants it"), "{err}");
    // A refused registration crafted by hand doesn't fold either.
    // Ana re-grants (her presence), then a new first passkey registers once.
    ana.regrant_passkey(wid, &bea_account.to_string()).unwrap();
    let fresh = SoftAuthenticator::ed25519();
    register(&mut bea, wid, &fresh, None).unwrap();
    assert_eq!(bea.passkeys_of(wid, bea_account).unwrap().len(), 1);
    // The re-grant is used up: cleared again, Bea needs another one.
    ana.remove_passkey(wid, bea_account, &fresh.credential(bea_account, "").credential_id).unwrap();
    assert!(register(&mut bea, wid, &SoftAuthenticator::es256(), None).is_err(), "a re-grant registers once");
    // Without presence a re-grant is refused.
    ana.set_presence_provider(None);
    assert!(ana.regrant_passkey(wid, &bea_account.to_string()).is_err());
}

/// The recovery key is the other way back: with every passkey gone but the
/// recovery key kept, a new passkey registers with the recovery key's
/// presence (and still not on the device key alone).
#[test]
fn recovery_key_re_registers_after_passkeys_are_removed() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ana, mut bea, wid, ana_key, bea_account) = second_person(dir.path(), true);
    let first = Arc::new(SoftAuthenticator::es256());
    register(&mut bea, wid, &first, None).unwrap();
    let rec = Arc::new(RecoveryKey::generate().unwrap());
    let f2 = first.clone();
    bea.register_passkey(wid, rec.credential(bea_account, "Recovery key"), &move |req| Ok(f2.get(&req.challenge))).unwrap();
    ana.set_presence_provider(Some(Arc::new(Soft(ana_key))));
    ana.remove_passkey(wid, bea_account, &first.credential(bea_account, "").credential_id).unwrap();
    let fresh = SoftAuthenticator::ed25519();
    assert!(register(&mut bea, wid, &fresh, None).is_err(), "the device key alone");
    let cred = fresh.credential(bea_account, "new");
    let r2 = rec.clone();
    bea.register_passkey(wid, cred, &move |req| Ok(r2.assert(&req.challenge))).unwrap();
    assert_eq!(bea.passkeys_of(wid, bea_account).unwrap().len(), 2);
}

/// Track B residual 1c: under rules v2, an account that never had a
/// credential (a member from before the switch) registers its first passkey
/// only after an Admin's re-grant; under rules v1 it stays trust on first use.
#[test]
fn an_outsider_cannot_register_a_passkey_without_a_grant() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ana, mut bea, wid, ana_key, bea_account) = second_person(dir.path(), false);
    let k = SoftAuthenticator::es256();
    assert!(register(&mut bea, wid, &k, None).is_err(), "an outsider registered a passkey");
    // Letting her in (Ana's presence) is her grant.
    ana.set_presence_provider(Some(Arc::new(Soft(ana_key))));
    let mut m = member(&bea_account.to_string(), WorkspaceRole::Contributor);
    m.actor = bea.author().clone();
    ana.add_member(workspace_config_session_id(wid), wid, m).unwrap();
    bea.publish_identity(workspace_config_session_id(wid), wid).unwrap();
    register(&mut bea, wid, &k, None).unwrap();
}

/// Track B residual 2: from rules v1 too, switching to rules v2 or any later
/// version (one this build doesn't know included) needs presence. A device
/// key alone can't make the workspace read-only on every device, or turn on
/// a future rules version; with presence the switch to an unknown version
/// still makes this build read-only.
#[test]
fn switching_to_any_later_rules_version_needs_presence() {
    let (mut svc, wid) = founder();
    let key = Arc::new(SoftAuthenticator::es256());
    register(&mut svc, wid, &key, None).unwrap();
    let rec = RecoveryKey::generate().unwrap();
    let k = key.clone();
    svc.register_passkey(wid, rec.credential(account(&svc), "Recovery"), &move |req| Ok(k.get(&req.challenge))).unwrap();
    let cfg = workspace_config_session_id(wid);
    let unknown = ligus_core::authz::RULES_SUPPORTED + 1;
    for v in [3, unknown, 7] {
        let err = svc.set_workspace_rules(wid, v).unwrap_err().to_string();
        assert!(err.contains(ligus_core::authz::PRESENCE_REQUIRED), "v{v}: {err}");
    }
    // A switch to an unknown version crafted without presence is denied, and
    // the workspace stays writable.
    let mut forged = config_log(&svc, wid).into_iter().max_by_key(|e| e.lamport).unwrap();
    forged.clear_raw();
    forged.event_id = Uuid::new_v4();
    forged.lamport += 100;
    forged.sequence += 100;
    forged.payload = SessionEvent::WorkspaceRulesSet { version: unknown, presence: None };
    forged.actor_stamp.as_mut().unwrap().presence = None;
    // A link of its own (a dangling one), so it shares none with the real
    // switch below: this device's chain stays unforked.
    forged.actor_stamp.as_mut().unwrap().prev = Some(Uuid::new_v4());
    forged.signature = None;
    svc.sign_for_test(&mut forged);
    let mut log = config_log(&svc, wid);
    log.push(forged.clone());
    assert_eq!(fold_decisions(&log, None, None, WorkspaceAuthz::Unchecked)[&forged.event_id], Verdict::Denied);
    svc.store().append_envelope(&forged).unwrap();
    assert!(!svc.load(cfg).unwrap().unwrap().authz.read_only(), "a forged switch made the workspace read-only");
    // With the passkey, the switch goes through and this build goes read-only.
    svc.set_presence_provider(Some(Arc::new(Soft(key))));
    svc.set_workspace_rules(wid, unknown).unwrap();
    assert!(svc.load(cfg).unwrap().unwrap().authz.read_only());
    let err = svc.add_member(cfg, wid, member("hal", WorkspaceRole::Viewer)).unwrap_err().to_string();
    assert!(err.contains("update Ligus"), "{err}");
}

struct NeverAsk;
impl PresenceProvider for NeverAsk {
    fn assert(&self, req: &PresenceRequest) -> Result<PresenceAssertion, String> {
        panic!("a background publication asked for a passkey ({})", req.kind);
    }
}

/// Follow-up to residual 1a: a new device of an account with a passkey
/// needs that passkey to be confirmed, but the identity publications Ligus
/// makes on its own (opening a workspace, the sync loop's key-agreement
/// key) never ask for one: the desktop's prompt would hold the service lock
/// for minutes and reopen on every retry. The person confirms the device
/// once, explicitly.
#[test]
fn a_new_device_is_confirmed_explicitly_never_from_a_background_publication() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ligus.db");
    let account = Uuid::new_v4();
    let account_seed = SigningKeypair::generate().unwrap().seed_bytes();
    let mut author = ActorIdentity::new(account.to_string(), "Ana", ActorKind::Human);
    author.account_id = Some(account);
    let mut desk = ChatService::new(EventStore::open(&db).unwrap(), SigningKeypair::generate().unwrap(), SigningKeypair::from_seed(&account_seed).unwrap(), author.clone());
    desk.disable_test_presence();
    let wid = Uuid::new_v4();
    desk.ensure_workspace_config(wid).unwrap();
    let key = Arc::new(SoftAuthenticator::es256());
    register(&mut desk, wid, &key, None).unwrap();
    let rec = RecoveryKey::generate().unwrap();
    let k2 = key.clone();
    desk.register_passkey(wid, rec.credential(account, "Recovery key"), &move |req| Ok(k2.get(&req.challenge))).unwrap();
    desk.set_presence_provider(Some(Arc::new(Soft(key.clone()))));
    desk.set_workspace_rules(wid, ligus_core::authz::RULES_NEW_WORKSPACE).unwrap();

    // Ana's second laptop: same account key, its own device key.
    let mut laptop = ChatService::new(EventStore::open(&db).unwrap(), SigningKeypair::generate().unwrap(), SigningKeypair::from_seed(&account_seed).unwrap(), author);
    laptop.disable_test_presence();
    laptop.set_presence_provider(Some(Arc::new(NeverAsk)));
    let cfg = workspace_config_session_id(wid);
    assert!(!laptop.publish_identity(cfg, wid).unwrap(), "published a device that needs a passkey");
    let chat = desk.create_chat("c", wid, "anthropic").unwrap().id;
    laptop.ensure_self_member(chat, wid).unwrap();
    let ka = ligus_core::e2ee::KeyAgreementKeypair::generate().unwrap();
    assert!(laptop.publish_key_agreement(wid, ka.public_key_bytes()).is_err(), "an unconfirmed device publishes no key");
    assert!(!laptop.device_confirmed(wid).unwrap());

    // Confirming it is an explicit act, with the passkey.
    laptop.set_presence_provider(Some(Arc::new(Soft(key))));
    assert!(laptop.confirm_device(wid).unwrap());
    assert!(laptop.device_confirmed(wid).unwrap());
    assert!(laptop.publish_key_agreement(wid, ka.public_key_bytes()).unwrap());
}

/// While the person confirms a passkey-gated action, this device writes
/// something else on that log (here a host record; in practice an agent
/// reply in the chat a vote is cast in). Committing the confirmation would
/// fork this device's chain, so it is refused (`StaleDraft`, nothing
/// stored; serve answers 409): the action is prepared again at the head and
/// the person confirms once more, and that one commits with one chain head.
#[test]
fn an_own_write_during_a_passkey_confirmation_asks_again() {
    let (mut svc, wid, key, _rec) = on_rules_v2();
    let cfg = workspace_config_session_id(wid);
    let host = |n: &str| ligus_core::WorkspaceHost::new(n.to_string(), ligus_core::HostKind::Worker, n.to_string());
    for i in 0..3 {
        let name = format!("m{i}");
        svc.capture_presence(true);
        assert!(svc.add_member(cfg, wid, member(&name, WorkspaceRole::Viewer)).is_err());
        let draft = svc.take_presence_draft().expect("captured");
        let a = key.get(&draft.request.challenge);
        assert!(svc.ensure_host(wid, host(&format!("h{i}"))).unwrap(), "an own write on the log meanwhile");
        let refused = svc.commit_presence(draft, a);
        assert!(matches!(refused, Err(ChatError::Store(crate::event_store::EventStoreError::StaleDraft))), "{refused:?}");
        assert!(!svc.load(cfg).unwrap().unwrap().members.iter().any(|m| m.id == name), "nothing stored");
        // Prepared again at the head, confirmed again.
        svc.capture_presence(true);
        assert!(svc.add_member(cfg, wid, member(&name, WorkspaceRole::Viewer)).is_err());
        let draft = svc.take_presence_draft().expect("captured again");
        let a = key.get(&draft.request.challenge);
        svc.commit_presence(draft, a).unwrap();
        assert!(svc.load(cfg).unwrap().unwrap().members.iter().any(|m| m.id == name));
    }
    let log = config_log(&svc, wid);
    let me = svc.device_id();
    let mine: Vec<&SessionEventEnvelope> = log.iter().filter(|e| e.signer_device_id == Some(me)).collect();
    let named: std::collections::BTreeSet<Uuid> = mine.iter().filter_map(|e| e.actor_stamp.as_ref().and_then(|s| s.prev)).collect();
    assert_eq!(mine.iter().filter(|e| !named.contains(&e.event_id)).count(), 1, "one chain head");
}
