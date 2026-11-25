//! Phone-signed actions (Phase B): the box drafts, the phone checks the
//! preimage strictly and signs it with its own P-256 key, the fold judges it
//! at the phone tier. A phone never signs governance, never exceeds its
//! tier, and its vote counts as its person's only when signed by that
//! person's certified, unrevoked phone (with presence where required).

use ligus_core::authz::phone::phone_tier_allows;
use ligus_core::authz::votes::diff_digest;
use ligus_core::crypto::{envelope_preimage, sign_envelope_p256, SigAlg};
use ligus_core::flow::{FlowRecord, HumanAnswered};
use ligus_core::phone_action::{check_phone_action_preimage, Confirmed, PhoneAction};
use ligus_core::presence::PresenceAssertion;
use ligus_core::testkit::{Phone, SignedWorld};
use ligus_core::workspace_repo::ApplyRequest;
use ligus_core::{
    workspace_config_session_id, ActionProposal, ActorKind, ChatMessage, MemberRoleChange, MessageRole, ProposalApproval, ProposalStatus,
    SessionEvent, SessionEventEnvelope, Timestamp, Verdict, WorkspaceRole,
};
use proptest::prelude::*;
use uuid::Uuid;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

struct Fixture {
    w: SignedWorld,
    phone: Phone,
    chat: Uuid,
    pid: Uuid,
    digest: String,
    host: Uuid,
}

fn fixture() -> Fixture {
    let mut w = SignedWorld::genesis(&["owner", "ada", "mara", "box"], None);
    w.kinds.insert("box".into(), ActorKind::Service);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.admit("owner", "box", WorkspaceRole::Contributor);
    let phone = Phone::new();
    let cert = w.phone_certificate("mara", &phone);
    let enrolled = w.push("mara", SessionEvent::DeviceCertificateAdded { certificate: cert });
    assert_eq!(w.verdict(enrolled), Verdict::Allowed);
    let chat = w.chat("owner", "c");
    let mut p = ActionProposal::file_diff("p", "ada", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
    p.created_at = Timestamp::epoch();
    let pid = p.id;
    w.say(chat, "ada", SessionEvent::ProposalUpserted { proposal: p });
    let digest = diff_digest(&w.state(chat).proposals[0]);
    let host = w.people["box"].device_id;
    Fixture { w, phone, chat, pid, digest, host }
}

/// The event a box drafts for `action` (as `ChatService::draft_phone_action` does).
fn payload(f: &Fixture, action: &PhoneAction) -> SessionEvent {
    let mara = f.w.people["mara"].actor();
    match action {
        PhoneAction::Post { text, .. } => {
            let mut m = ChatMessage::new(MessageRole::User, mara.display_name.clone(), text.clone());
            let mut id = mara.clone();
            id.device_id = Some(f.phone.device_id);
            m.actor_identity = Some(id);
            SessionEvent::MessageAppended { message: m }
        }
        PhoneAction::Vote { proposal_id, diff_digest, approved, .. } => SessionEvent::ProposalVoteCast {
            proposal_id: *proposal_id,
            approval: ProposalApproval {
                actor_id: mara.id.clone(),
                role: WorkspaceRole::Contributor,
                approved: *approved,
                created_at: Timestamp::now(),
                diff_digest: Some(diff_digest.clone()),
            },
        },
        PhoneAction::Answer { ask_id, answer, option_index, .. } => SessionEvent::FlowRecorded {
            record: FlowRecord::HumanAnswered(HumanAnswered {
                ask_id: *ask_id,
                answer: answer.clone(),
                option_index: *option_index,
                by: mara.display_name.clone(),
                at_ms: 1,
            }),
        },
        PhoneAction::ClearLockdown { chat_id, raises } => SessionEvent::ChatLockdownCleared { chat_id: *chat_id, raises: raises.clone() },
        PhoneAction::RequestApply { proposal_id, diff_digest, host_device_id, expected_base_commit, repo_id, .. } => {
            SessionEvent::ProposalApplyRequested {
                request: ApplyRequest {
                    id: Uuid::new_v4(),
                    proposal_id: *proposal_id,
                    diff_digest: diff_digest.clone(),
                    host_device_id: *host_device_id,
                    expected_base_commit: expected_base_commit.clone(),
                    repo_id: *repo_id,
                },
            }
        }
        PhoneAction::WithdrawApply { proposal_id, request_id, .. } => {
            SessionEvent::ProposalApplyWithdrawn { request_id: *request_id, proposal_id: *proposal_id }
        }
        PhoneAction::Archive { proposal_id, archived, .. } => SessionEvent::ProposalDismissed { proposal_id: *proposal_id, dismissed: *archived },
        PhoneAction::ProposeMemory { chat_id, op, rationale } => {
            // A new entry is named by its proposal (the box's id).
            let id = Uuid::new_v4();
            let op = match op {
                ligus_core::memory::MemoryOp::Create { title, body, tags, .. } => {
                    ligus_core::memory::MemoryOp::Create { entry_id: id, title: title.clone(), body: body.clone(), tags: tags.clone() }
                }
                other => other.clone(),
            };
            SessionEvent::FlowRecorded {
                record: FlowRecord::MemoryProposed(ligus_core::memory::MemoryProposal {
                    proposal_id: id,
                    op,
                    proposer: String::new(),
                    source: ligus_core::memory::MemorySource { chat_id: Some(*chat_id), message_id: None, turn_id: None, egress: None },
                    rationale: rationale.clone(),
                }),
            }
        }
    }
}

/// A box's draft of `payload` on `session` (chat or config) as mara, for her
/// phone, unsigned.
fn draft(f: &mut Fixture, chat: Option<Uuid>, payload: SessionEvent) -> SessionEventEnvelope {
    let mut env = f.w.draft("mara", chat, payload, None);
    let s = env.actor_stamp.as_mut().unwrap();
    s.actor.device_id = Some(f.phone.device_id);
    s.sig_alg = SigAlg::P256;
    s.prev = None;
    env.signature = None;
    env.signer_device_id = None;
    env.clear_raw();
    env
}

fn chat_of(action: &PhoneAction) -> Option<Uuid> {
    match action {
        PhoneAction::ClearLockdown { .. } | PhoneAction::ProposeMemory { .. } => None,
        a => Some(a.chat_id()),
    }
}

/// Stamp mara's (recovery-key) presence, returning the challenge and assertion.
fn confirm(f: &Fixture, env: &mut SessionEventEnvelope) -> ([u8; 32], PresenceAssertion) {
    let c = ligus_core::presence::challenge(env);
    let a = f.w.people["mara"].recovery.assert(&c);
    env.actor_stamp.as_mut().unwrap().presence = Some(a.clone());
    env.clear_raw();
    (c, a)
}

fn actions(f: &Fixture) -> Vec<PhoneAction> {
    vec![
        PhoneAction::Post { chat_id: f.chat, text: "from my phone".into() },
        PhoneAction::Vote { chat_id: f.chat, proposal_id: f.pid, diff_digest: f.digest.clone(), approved: true },
        PhoneAction::Answer { chat_id: f.chat, ask_id: Uuid::from_u128(5), answer: "yes".into(), option_index: Some(0) },
        PhoneAction::ClearLockdown { chat_id: f.chat, raises: vec![Uuid::from_u128(0x77)] },
        PhoneAction::RequestApply {
            chat_id: f.chat,
            proposal_id: f.pid,
            diff_digest: f.digest.clone(),
            host_device_id: f.host,
            expected_base_commit: Some(SHA.into()),
            repo_id: None,
        },
        PhoneAction::WithdrawApply { chat_id: f.chat, proposal_id: f.pid, request_id: Uuid::from_u128(9) },
        PhoneAction::ProposeMemory {
            chat_id: f.chat,
            op: ligus_core::memory::MemoryOp::Create { entry_id: Uuid::from_u128(0x31), title: "Deploy day".into(), body: "Thursdays".into(), tags: vec![] },
            rationale: "from standup".into(),
        },
    ]
}

fn check(f: &Fixture, preimage: &[u8], confirmed: Option<Confirmed<'_>>, action: &PhoneAction) -> Result<ligus_core::phone_action::CheckedPhoneAction, &'static str> {
    check_phone_action_preimage(preimage, confirmed, f.w.wid, f.phone.device_id, f.w.people["mara"].account_id, action)
}

#[test]
fn every_action_round_trips_with_and_without_presence() {
    let mut f = fixture();
    for action in actions(&f) {
        let __a0 = chat_of(&action);
        let __a1 = payload(&f, &action);
        let mut env = draft(&mut f, __a0, __a1);
        let bare = envelope_preimage(&env);
        let ok = check(&f, &bare, None, &action).unwrap_or_else(|e| panic!("{}: {e}", action.kind()));
        assert_eq!((ok.event_id, ok.session_id, ok.actor_id.as_str()), (env.event_id, action.session_id(f.w.wid), "mara"));
        // The box said no presence, but stamped one anyway: refused.
        let (c, a) = confirm(&f, &mut env);
        let with = envelope_preimage(&env);
        assert!(check(&f, &with, None, &action).is_err(), "{}", action.kind());
        // Confirmed: passes; the bare preimage (no presence) doesn't.
        let conf = Confirmed { challenge: &c, assertion: Some(&a) };
        check(&f, &with, Some(conf), &action).unwrap_or_else(|e| panic!("{}: {e}", action.kind()));
        assert!(check(&f, &bare, Some(conf), &action).is_err());
        // Another challenge, or another assertion than the phone produced.
        let other = [7u8; 32];
        assert!(check(&f, &with, Some(Confirmed { challenge: &other, assertion: Some(&a) }), &action).is_err());
        let mut forged = a.clone();
        forged.signature.push('A');
        assert!(check(&f, &with, Some(Confirmed { challenge: &c, assertion: Some(&forged) }), &action).is_err());
    }
}

#[test]
fn a_preimage_for_anything_else_is_refused() {
    let mut f = fixture();
    let all = actions(&f);
    for action in &all {
        let __a0 = chat_of(action);
        let __a1 = payload(&f, action);
        let env = draft(&mut f, __a0, __a1);
        let good = envelope_preimage(&env);
        // Checked as any other action: refused.
        for other in all.iter().filter(|o| *o != action) {
            assert!(check(&f, &good, None, other).is_err(), "{} as {}", action.kind(), other.kind());
        }
        // Another workspace, phone, account; truncated; trailing bytes.
        let account = f.w.people["mara"].account_id;
        assert!(check_phone_action_preimage(&good, None, Uuid::new_v4(), f.phone.device_id, account, action).is_err());
        assert!(check_phone_action_preimage(&good, None, f.w.wid, Uuid::new_v4(), account, action).is_err());
        assert!(check_phone_action_preimage(&good, None, f.w.wid, f.phone.device_id, Uuid::new_v4(), action).is_err());
        assert!(check(&f, &good[..good.len() - 1], None, action).is_err());
        let mut longer = good.clone();
        longer.push(0);
        assert!(check(&f, &longer, None, action).is_err());
        // Ed25519-tagged, a runtime effect, an agent's stamp.
        for tweak in 0..3 {
            let mut e = env.clone();
            let s = e.actor_stamp.as_mut().unwrap();
            match tweak {
                0 => s.sig_alg = SigAlg::Ed25519,
                1 => s.runtime_effect = true,
                _ => s.actor.kind = ActorKind::Agent,
            }
            e.clear_raw();
            assert!(check(&f, &envelope_preimage(&e), None, action).is_err(), "tweak {tweak}");
        }
        // An unknown field in the payload or the stamp.
        for field in [2, 3] {
            assert!(check(&f, &with_extra_field(&good, field), None, action).is_err(), "field {field}");
        }
    }
    // The person's own choices: other text, other vote, other host, other
    // chat, another proposal.
    let a = PhoneAction::Post { chat_id: f.chat, text: "a".into() };
    let __a0 = Some(f.chat);
    let __a1 = payload(&f, &PhoneAction::Post { chat_id: f.chat, text: "b".into() });
    let env = draft(&mut f, __a0, __a1);
    assert!(check(&f, &envelope_preimage(&env), None, &a).is_err());
    let yes = PhoneAction::Vote { chat_id: f.chat, proposal_id: f.pid, diff_digest: f.digest.clone(), approved: true };
    let no = PhoneAction::Vote { chat_id: f.chat, proposal_id: f.pid, diff_digest: f.digest.clone(), approved: false };
    let __a0 = Some(f.chat);
    let __a1 = payload(&f, &no);
    let env = draft(&mut f, __a0, __a1);
    assert!(check(&f, &envelope_preimage(&env), None, &yes).is_err());
    let elsewhere = f.w.chat("owner", "other");
    let __a0 = Some(elsewhere);
    let __a1 = payload(&f, &yes);
    let env = draft(&mut f, __a0, __a1);
    assert!(check(&f, &envelope_preimage(&env), None, &yes).is_err());
    let apply = |host: Uuid| PhoneAction::RequestApply {
        chat_id: f.chat,
        proposal_id: f.pid,
        diff_digest: f.digest.clone(),
        host_device_id: host,
        expected_base_commit: None,
        repo_id: None,
    };
    let (a1, a2) = (apply(f.host), apply(Uuid::new_v4()));
    let __a0 = Some(f.chat);
    let __a1 = payload(&f, &a2);
    let env = draft(&mut f, __a0, __a1);
    assert!(check(&f, &envelope_preimage(&env), None, &a1).is_err());
    // A post whose message carries a field the person didn't set.
    let mut ev = payload(&f, &a);
    if let SessionEvent::MessageAppended { message } = &mut ev {
        message.related_proposal_id = Some(f.pid);
    }
    let __a0 = Some(f.chat);
    let __a1 = ev;
    let env = draft(&mut f, __a0, __a1);
    assert!(check(&f, &envelope_preimage(&env), None, &a).is_err());
    // A vote cast for someone else.
    let mut ev = payload(&f, &yes);
    if let SessionEvent::ProposalVoteCast { approval, .. } = &mut ev {
        approval.actor_id = "owner".into();
    }
    let __a0 = Some(f.chat);
    let __a1 = ev;
    let env = draft(&mut f, __a0, __a1);
    assert!(check(&f, &envelope_preimage(&env), None, &yes).is_err());
}

/// `preimage` with `"zz": 1` added to its JSON field `index` (0 scope, 1
/// timestamp, 2 stamp, 3 payload), lengths fixed up.
fn with_extra_field(preimage: &[u8], index: usize) -> Vec<u8> {
    let head = b"hive-envelope-v3\0".len() + 16 + 16 + 8 + 8 + 16;
    let mut out = preimage[..head].to_vec();
    let mut rest = &preimage[head..];
    for i in 0..4 {
        let len = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        let mut json = rest[4..4 + len].to_vec();
        rest = &rest[4 + len..];
        if i == index {
            let mut v: serde_json::Value = serde_json::from_slice(&json).unwrap();
            if let Some(o) = v.as_object_mut() {
                o.insert("zz".into(), serde_json::json!(1));
            }
            json = serde_json::to_vec(&v).unwrap();
        }
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&json);
    }
    out
}

/// The payload with what the box may choose (ids, clocks, the vote's
/// ignored role) normalized away.
fn normalized(e: &SessionEvent) -> SessionEvent {
    let mut e = e.clone();
    match &mut e {
        SessionEvent::MessageAppended { message } => {
            message.id = Uuid::nil();
            message.created_at = Timestamp::epoch();
        }
        SessionEvent::ProposalVoteCast { approval, .. } => {
            approval.created_at = Timestamp::epoch();
            approval.role = WorkspaceRole::Viewer;
        }
        SessionEvent::FlowRecorded { record: FlowRecord::HumanAnswered(a) } => a.at_ms = 0,
        SessionEvent::ProposalApplyRequested { request } => request.id = Uuid::nil(),
        _ => {}
    }
    e
}

fn parse_payload(preimage: &[u8], f: &Fixture) -> Option<SessionEvent> {
    ligus_core::phone_action::parse_phone_preimage(preimage, f.w.wid, f.phone.device_id, None, None).ok().map(|p| p.event)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64), ..ProptestConfig::default() })]

    /// Whatever bytes a box sends, an accepted preimage carries exactly the
    /// action the phone asked for (only box-chosen fields may differ).
    #[test]
    fn an_accepted_preimage_is_the_requested_action(which in 0usize..7, edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>(), 0u8..3), 1..4)) {
        let mut f = fixture();
        let action = actions(&f)[which].clone();
        let __a0 = chat_of(&action);
        let __a1 = payload(&f, &action);
        let env = draft(&mut f, __a0, __a1);
        let good = envelope_preimage(&env);
        let want = normalized(&env.payload);
        let mut bytes = good.clone();
        for (at, b, op) in edits {
            if bytes.is_empty() { break; }
            let i = at.index(bytes.len());
            match op {
                0 => bytes[i] = b,
                1 => bytes.insert(i, b),
                _ => { bytes.remove(i); }
            }
        }
        if check(&f, &bytes, None, &action).is_ok() {
            let got = parse_payload(&bytes, &f).expect("an accepted preimage parses");
            prop_assert_eq!(normalized(&got), want);
        }
    }
}

// ---- the phone tier: exactly these kinds ----

const GOLDEN: &str = include_str!("golden/envelopes_v3.jsonl");

/// One payload of every kind (the golden vectors cover every kind).
fn every_kind() -> Vec<SessionEvent> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for line in GOLDEN.lines().filter(|l| !l.trim().is_empty()) {
        let start = line.find("\"envelope\":").unwrap() + "\"envelope\":".len();
        let env: SessionEventEnvelope = serde_json::from_str(&line[start..line.len() - 1]).unwrap();
        let k = format!("{}{}", env.payload.kind_str(), matches!(&env.payload, SessionEvent::JoinApproved { admission: Some(_), .. }));
        if seen.insert(k) {
            out.push(env.payload);
        }
    }
    out
}

/// What the phone tier allows, by kind (`flowRecorded` only as an answer,
/// `messageAppended` only as a user message, `deviceRevoked` only of the
/// phone itself, `joinApproved` only as a box admission).
const PHONE_TIER: &[&str] = &[
    "messageAppended",
    "messageRemoved",
    "messageReactionAdded",
    "messageReactionRemoved",
    "proposalVoteCast",
    "proposalRefined",
    "proposalDismissed",
    "proposalApplyRequested",
    "proposalApplyWithdrawn",
    "chatLockdownRaised",
    "chatLockdownCleared",
];

#[test]
fn the_phone_tier_is_exactly_the_allow_list() {
    let me = Uuid::new_v4();
    let kinds = every_kind();
    assert!(kinds.len() >= 70, "every kind is covered ({} kinds)", kinds.len());
    for e in &kinds {
        let k = e.kind_str();
        let allowed = phone_tier_allows(e, me).is_ok();
        let expected = match e {
            SessionEvent::MessageAppended { message } => message.role == MessageRole::User,
            SessionEvent::FlowRecorded { record } => {
                matches!(record, FlowRecord::HumanAnswered(_)) || matches!(record, FlowRecord::MemoryProposed(p) if p.proposer.is_empty())
            }
            SessionEvent::JoinApproved { admission, .. } => admission.is_some(),
            SessionEvent::DeviceRevoked { device_id, .. } => *device_id == me,
            _ => PHONE_TIER.contains(&k),
        };
        assert_eq!(allowed, expected, "{k}");
    }
    // Every generic-path kind is in the tier.
    for k in ligus_core::phone_action::PHONE_ACTION_EVENT_KINDS {
        assert!(PHONE_TIER.contains(k) || *k == "flowRecorded", "{k}");
    }
    // Governance is never in it: roles, cuts, admissions beyond the box
    // admission, rules, closures, keys, agents, runtimes, hosts, repos.
    for k in [
        "memberAdded", "memberRemoved", "memberRoleChanged", "admissionClosed", "workspaceRulesSet", "workspaceKeyRotated",
        "passkeyRegistered", "passkeyRemoved", "agentUpserted", "agentRosterUpdated", "agentRemoved", "agentKeyCertified",
        "workspaceRuntimeUpserted", "workspaceHostUpserted", "workspaceRepoAdded", "dearthModeChanged", "workflowDefinitionsUpdated",
        "proposalApplied", "proposalApplyFailed", "proposalSetDecided", "proposalSetTuned", "proposalUpserted", "deviceCertificateAdded",
        "workspaceSandboxSet",
    ] {
        assert!(!PHONE_TIER.contains(&k), "{k}");
        for e in kinds.iter().filter(|e| e.kind_str() == k) {
            assert!(phone_tier_allows(e, me).is_err(), "{k}");
        }
    }
}

/// Governance a phone might try, as payloads valid in the fixture world.
fn governance(f: &Fixture, i: usize) -> (Option<Uuid>, SessionEvent) {
    let owner = f.w.people["owner"].clone_ids();
    match i % 8 {
        0 => (None, SessionEvent::MemberRoleChanged {
            change: MemberRoleChange { member_id: "box".into(), old_role: WorkspaceRole::Contributor, new_role: WorkspaceRole::Viewer },
            watermark: vec![],
        }),
        1 => (None, SessionEvent::MemberRemoved { member_id: "box".into(), watermark: vec![] }),
        2 => (None, SessionEvent::MemberAdded { member: f.w.member_of("ada", WorkspaceRole::Viewer), invite: None, proof: None }),
        3 => (None, SessionEvent::DeviceRevoked { device_id: owner.1, watermark: vec![] }),
        4 => (None, SessionEvent::WorkspaceRulesSet { version: 2, presence: None }),
        5 => (None, SessionEvent::JoinApproved { subject: format!("invite:{}", Uuid::new_v4()), admission: None, redeem_key: None }),
        6 => (None, SessionEvent::AdmissionClosed { invite: Uuid::new_v4(), redeemed: None }),
        _ => (Some(f.chat), SessionEvent::ProposalApplied {
            proposal_id: f.pid,
            diff_digest: f.digest.clone(),
            commit: None,
            at: Timestamp::epoch(),
            repo_id: None,
            applied_on: None,
            base_commit: None,
            applied_tree_hash: None,
            request_id: None,
        }),
    }
}

trait Ids {
    fn clone_ids(&self) -> (Uuid, Uuid);
}
impl Ids for ligus_core::testkit::Person {
    fn clone_ids(&self) -> (Uuid, Uuid) {
        (self.account_id, self.device_id)
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(32), ..ProptestConfig::default() })]

    /// Whatever its person's role (an Owner's phone included), with or
    /// without its person's presence, a phone never signs governance.
    #[test]
    fn a_phone_never_signs_governance_whatever_its_persons_role(role in 0usize..4, which in 0usize..8, present in any::<bool>()) {
        let mut w = SignedWorld::genesis(&["owner", "ada", "mara", "box"], None);
        w.kinds.insert("box".into(), ActorKind::Service);
        let r = [WorkspaceRole::Viewer, WorkspaceRole::Contributor, WorkspaceRole::Admin, WorkspaceRole::Owner][role];
        w.admit("owner", "ada", WorkspaceRole::Admin);
        w.admit("owner", "mara", r);
        w.admit("owner", "box", WorkspaceRole::Contributor);
        let phone = Phone::new();
        let cert = w.phone_certificate("mara", &phone);
        w.push("mara", SessionEvent::DeviceCertificateAdded { certificate: cert });
        let chat = w.chat("owner", "c");
        let mut p = ActionProposal::file_diff("p", "ada", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
        p.created_at = Timestamp::epoch();
        let pid = p.id;
        w.say(chat, "ada", SessionEvent::ProposalUpserted { proposal: p });
        let digest = diff_digest(&w.state(chat).proposals[0]);
        let mut f = Fixture { host: w.people["box"].device_id, w, phone, chat, pid, digest };
        let (log, ev) = governance(&f, which);
        let __a0 = log;
        let __a1 = ev;
        let mut env = draft(&mut f, __a0, __a1);
        if present {
            confirm(&f, &mut env);
        }
        sign_envelope_p256(&mut env, f.phone.device_id, &f.phone.key);
        let id = f.w.push_env(log, env);
        prop_assert_eq!(f.w.verdict(id), Verdict::Denied);
    }
}

// ---- votes count as the person's only from their certified phone ----

fn phone_signed(f: &mut Fixture, by: &str, chat: Option<Uuid>, payload: SessionEvent, present: bool) -> Uuid {
    let mut env = f.w.draft(by, chat, payload, None);
    let s = env.actor_stamp.as_mut().unwrap();
    s.actor.device_id = Some(f.phone.device_id);
    s.sig_alg = SigAlg::P256;
    s.prev = None;
    env.clear_raw();
    if present {
        let c = ligus_core::presence::challenge(&env);
        let a = f.w.people[by].recovery.assert(&c);
        env.actor_stamp.as_mut().unwrap().presence = Some(a);
        env.clear_raw();
    }
    sign_envelope_p256(&mut env, f.phone.device_id, &f.phone.key);
    f.w.push_env(chat, env)
}

fn vote(f: &Fixture, by: &str) -> SessionEvent {
    SessionEvent::ProposalVoteCast {
        proposal_id: f.pid,
        approval: ProposalApproval { actor_id: by.into(), role: WorkspaceRole::Admin, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(f.digest.clone()) },
    }
}

#[test]
fn a_phone_vote_counts_as_its_person_only() {
    let mut f = fixture();
    let chat = f.chat;
    // Mara's phone casting a vote stamped as the Owner: refused.
    let __a0 = "owner";
    let __a1 = Some(chat);
    let __a2 = vote(&f, "owner");
    let __a3 = false;
    let as_owner = phone_signed(&mut f, __a0, __a1, __a2, __a3);
    // Mara's phone voting as Mara: counted, at her role, as a human's.
    let __a0 = "mara";
    let __a1 = Some(chat);
    let __a2 = vote(&f, "mara");
    let __a3 = false;
    let ok = phone_signed(&mut f, __a0, __a1, __a2, __a3);
    let v = f.w.verdicts();
    assert_eq!((v[&as_owner], v[&ok]), (Verdict::Denied, Verdict::Allowed));
    let s = f.w.state(chat);
    let a = s.proposals[0].approvals.iter().find(|a| a.actor_id == "mara").unwrap();
    assert_eq!(a.role, WorkspaceRole::Contributor);
    assert!(s.guards.verified_approvals.get(&f.pid).is_some_and(|v| v.contains("mara")));
    assert!(!s.proposals[0].approvals.iter().any(|a| a.actor_id == "owner"));
}

#[test]
fn an_uncertified_or_revoked_phone_votes_for_no_one() {
    let mut f = fixture();
    let chat = f.chat;
    // A P-256 key nobody certified.
    let stranger = Phone::new();
    let real = std::mem::replace(&mut f.phone, stranger);
    let __a0 = "mara";
    let __a1 = Some(chat);
    let __a2 = vote(&f, "mara");
    let __a3 = false;
    let uncertified = phone_signed(&mut f, __a0, __a1, __a2, __a3);
    f.phone = real;
    assert_eq!(f.w.verdict(uncertified), Verdict::Denied);
    // Mara revokes her phone from her laptop; its later vote is refused.
    let dev = f.phone.device_id;
    let rev = f.w.push("mara", SessionEvent::DeviceRevoked { device_id: dev, watermark: vec![] });
    assert_eq!(f.w.verdict(rev), Verdict::Allowed);
    let __a0 = "mara";
    let __a1 = Some(chat);
    let __a2 = vote(&f, "mara");
    let __a3 = false;
    let after = phone_signed(&mut f, __a0, __a1, __a2, __a3);
    assert_eq!(f.w.verdict(after), Verdict::Denied);
    assert!(f.w.state(chat).proposals[0].approvals.is_empty());
}

#[test]
fn a_phone_vote_needs_presence_where_the_rules_do_and_clearing_a_lockdown_always() {
    let mut f = fixture();
    let chat = f.chat;
    // A workflow proposal's vote needs presence (the presence table).
    let mut wf = ActionProposal::new("wf", ligus_core::ProposalKind::Decision, "ada");
    wf.workflow = Some(Box::new(ligus_core::workflow::WorkflowDefinition {
        id: Uuid::from_u128(44),
        name: "wf".into(),
        description: String::new(),
        input_label: None,
        nodes: vec![],
        created_at: Timestamp::epoch(),
        disabled: false,
    }));
    wf.created_at = Timestamp::epoch();
    let wid = wf.id;
    f.w.say(chat, "ada", SessionEvent::ProposalUpserted { proposal: wf });
    let d = diff_digest(f.w.state(chat).proposals.iter().find(|p| p.id == wid).unwrap());
    let v = |d: &str| SessionEvent::ProposalVoteCast {
        proposal_id: wid,
        approval: ProposalApproval { actor_id: "mara".into(), role: WorkspaceRole::Contributor, approved: true, created_at: Timestamp::epoch(), diff_digest: Some(d.into()) },
    };
    let __a0 = "mara";
    let __a1 = Some(chat);
    let __a2 = v(&d);
    let __a3 = false;
    let bare = phone_signed(&mut f, __a0, __a1, __a2, __a3);
    let __a0 = "mara";
    let __a1 = Some(chat);
    let __a2 = v(&d);
    let __a3 = true;
    let present = phone_signed(&mut f, __a0, __a1, __a2, __a3);
    // Clearing a lockdown: config log, presence always.
    let __a0 = "mara";
    let __a1 = None;
    let __a2 = SessionEvent::ChatLockdownCleared { chat_id: chat, raises: vec![Uuid::from_u128(0x77)] };
    let __a3 = false;
    let clear_bare = phone_signed(&mut f, __a0, __a1, __a2, __a3);
    let __a0 = "mara";
    let __a1 = None;
    let __a2 = SessionEvent::ChatLockdownCleared { chat_id: chat, raises: vec![Uuid::from_u128(0x77)] };
    let __a3 = true;
    let clear = phone_signed(&mut f, __a0, __a1, __a2, __a3);
    let verdicts = f.w.verdicts();
    assert_eq!(
        [verdicts[&bare], verdicts[&present], verdicts[&clear_bare], verdicts[&clear]],
        [Verdict::Denied, Verdict::Allowed, Verdict::Denied, Verdict::Allowed]
    );
}

#[test]
fn a_phone_request_is_fulfilled_by_the_named_box_only() {
    let mut f = fixture();
    let chat = f.chat;
    let v = vote(&f, "owner");
    f.w.say(chat, "owner", v);
    assert_eq!(f.w.state(chat).proposals[0].status, ProposalStatus::Approved);
    let req = ApplyRequest { id: Uuid::from_u128(1), proposal_id: f.pid, diff_digest: f.digest.clone(), host_device_id: f.host, expected_base_commit: None, repo_id: None };
    let __a0 = "mara";
    let __a1 = Some(chat);
    let __a2 = SessionEvent::ProposalApplyRequested { request: req };
    let __a3 = false;
    let asked = phone_signed(&mut f, __a0, __a1, __a2, __a3);
    assert_eq!(f.w.verdict(asked), Verdict::Allowed);
    let host = f.host;
    let applied = f.w.say(chat, "box", SessionEvent::ProposalApplied {
        proposal_id: f.pid,
        diff_digest: f.digest.clone(),
        commit: None,
        at: Timestamp::epoch(),
        repo_id: None,
        applied_on: Some(host),
        base_commit: None,
        applied_tree_hash: None,
        request_id: Some(Uuid::from_u128(1)),
    });
    assert_eq!(f.w.verdict(applied), Verdict::Allowed);
    assert_eq!(f.w.state(chat).proposals[0].status, ProposalStatus::Applied);
    let _ = workspace_config_session_id(f.w.wid);
}
