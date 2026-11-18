//! Forged copies (review finding: erase-by-forgery). Event ids are chosen by
//! their author, so anyone holding the workspace key can publish another
//! envelope under a real event's id: unsigned, signed with a key that
//! doesn't verify, or signed by a device of their own. The equivocation rule
//! (owner decision on review P-A2c) is for one signer signing two different
//! events under one id; a copy that isn't the same signer's must never
//! displace, invalidate or shadow a verified event.

use hive_core::crypto::{sign_envelope, SigningKeypair};
use hive_core::events::{fold_copy_decisions, fold_decisions, project_authorized, project_config, WorkspaceAuthz};
use hive_core::testkit::{Phone, SignedWorld};
use hive_core::watermark::heads_of;
use hive_core::{workspace_config_session_id, ChatMessage, MessageRole, SessionEvent, SessionEventEnvelope, Verdict, WorkspaceRole};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn title(t: &str) -> SessionEvent {
    SessionEvent::SessionTitleChanged { title: t.into() }
}

fn msg(by: &str, body: &str) -> SessionEvent {
    SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, by, body) }
}

fn find(w: &SignedWorld, chat: Option<Uuid>, id: Uuid) -> SessionEventEnvelope {
    let log = match chat {
        None => &w.cfg,
        Some(c) => &w.chats[&c],
    };
    log.iter().find(|e| e.event_id == id).unwrap().clone()
}

/// How a forger copies a real event's id.
#[derive(Debug, Clone, Copy)]
enum Forgery {
    /// No signature and no stamp: claims no author.
    UnsignedStampless,
    /// The real stamp kept, the signature stripped.
    UnsignedStamped,
    /// The real signer's device id and stamp, signed with another key.
    WrongKey,
    /// Signed by a device of the forger's own, self-certified identity
    /// (no member: anyone with the workspace key can do this).
    Stranger,
    /// Signed by another member's own certified device.
    Insider,
}

const FORGERIES: [Forgery; 5] =
    [Forgery::UnsignedStampless, Forgery::UnsignedStamped, Forgery::WrongKey, Forgery::Stranger, Forgery::Insider];

/// A copy of `orig` (on `chat`) with `payload`, forged as `how`. It keeps
/// the original lamport when `same_lamport`; else a stranger's or insider's
/// copy counts from its signer's own view of the log (plausible), and any
/// other sits a little past the original.
fn forge(w: &mut SignedWorld, chat: Option<Uuid>, orig: &SessionEventEnvelope, payload: SessionEvent, how: Forgery, same_lamport: bool) -> SessionEventEnvelope {
    let mut f = orig.clone();
    f.payload = payload.clone();
    if !same_lamport {
        f.lamport = orig.lamport + 3;
    }
    f.clear_raw();
    match how {
        Forgery::UnsignedStampless => {
            f.actor_stamp = None;
            f.signature = None;
            f.signer_device_id = None;
        }
        Forgery::UnsignedStamped => {
            f.signature = None;
            f.signer_device_id = None;
        }
        Forgery::WrongKey => {
            let k = SigningKeypair::generate().unwrap();
            let device = orig.signer_device_id.unwrap();
            sign_envelope(&mut f, device, &k);
        }
        Forgery::Stranger | Forgery::Insider => {
            let by = if matches!(how, Forgery::Stranger) { "eve" } else { "mal" };
            f = w.draft(by, chat, payload, None);
            f.event_id = orig.event_id;
            if same_lamport {
                f.lamport = orig.lamport;
                if let (Some(s), Some(o)) = (f.actor_stamp.as_mut(), orig.actor_stamp.as_ref()) {
                    s.log_head = o.log_head;
                }
            }
            w.finish(&mut f, by, None);
        }
    }
    f
}

/// What a forger puts under a real chat event's id: a title change, except
/// for an insider, whose own title change would be theirs to make (it then
/// competes with the honest one as any later change does), so a message.
fn forged_payload(how: Forgery) -> SessionEvent {
    match how {
        Forgery::Insider => msg("mal", "insider"),
        _ => title("forged"),
    }
}

/// A world with an Owner, a Contributor, an insider (another Contributor)
/// and a stranger who published a self-certified identity but was never
/// admitted, plus one chat.
fn world() -> (SignedWorld, Uuid) {
    let mut w = SignedWorld::genesis(&["owner", "con", "mal", "eve", "dan"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.admit("owner", "mal", WorkspaceRole::Contributor);
    w.publish_identity("eve");
    let chat = w.chat("owner", "c");
    (w, chat)
}

fn chat_fold(cfg: &[SessionEventEnvelope], log: &[SessionEventEnvelope]) -> hive_core::ChatSession {
    let t = project_config(cfg, None, None).1;
    project_authorized(log, None, None, WorkspaceAuthz::Timeline(&t)).unwrap()
}

fn chat_verdicts(cfg: &[SessionEventEnvelope], log: &[SessionEventEnvelope]) -> BTreeMap<Uuid, Verdict> {
    let t = project_config(cfg, None, None).1;
    fold_decisions(log, None, None, WorkspaceAuthz::Timeline(&t))
}

/// The reviewer's store probe, as a fold: an honest title change survives
/// every kind of forged copy of its id, in either arrival order.
#[test]
fn a_forged_copy_never_erases_an_honest_title_change() {
    for how in FORGERIES {
        for same_lamport in [true, false] {
            let (mut w, chat) = world();
            w.say(chat, "con", title("before"));
            let id = w.say(chat, "con", title("honest"));
            let orig = find(&w, Some(chat), id);
            let forged = forge(&mut w, Some(chat), &orig, forged_payload(how), how, same_lamport);
            let mut log = w.chats[&chat].clone();
            log.push(forged.clone());
            let mut rev = w.chats[&chat].clone();
            rev.insert(1, forged);
            for l in [&log, &rev] {
                let s = chat_fold(&w.cfg, l);
                assert_eq!(s.title, "honest", "{how:?} same_lamport={same_lamport}: the honest change was erased or displaced");
                assert_eq!(chat_verdicts(&w.cfg, l)[&id], Verdict::Allowed, "{how:?}");
            }
        }
    }
}

/// The same on the config log: a forged copy of an Owner's removal of a
/// member doesn't bring the member back.
#[test]
fn a_forged_copy_never_erases_a_removal() {
    for how in FORGERIES {
        for same_lamport in [true, false] {
            let (mut w, _) = world();
            w.admit("owner", "dan", WorkspaceRole::Contributor);
            let signers: BTreeSet<Uuid> = [w.people["dan"].device_id].into_iter().collect();
            let wm = heads_of(&w.cfg, workspace_config_session_id(w.wid), &signers);
            let removal = w.push("owner", SessionEvent::MemberRemoved { member_id: "dan".into(), watermark: wm });
            assert_eq!(w.verdict(removal), Verdict::Allowed);
            let orig = find(&w, None, removal);
            let forged = forge(&mut w, None, &orig, SessionEvent::WorkspaceRulesSet { version: 2, presence: None }, how, same_lamport);
            let mut cfg = w.cfg.clone();
            cfg.push(forged.clone());
            let mut rev = w.cfg.clone();
            let at = rev.len() - 1;
            rev.insert(at, forged);
            for c in [&cfg, &rev] {
                let v = fold_decisions(c, None, None, WorkspaceAuthz::Unchecked);
                assert_eq!(v[&removal], Verdict::Allowed, "{how:?} same_lamport={same_lamport}: the removal was erased");
                let members: Vec<String> = project_config(c, None, None).0.unwrap().members.into_iter().map(|m| m.id).collect();
                assert!(!members.contains(&"dan".to_string()), "{how:?}: the removed member is back: {members:?}");
            }
        }
    }
}

/// A forged copy of the config seed's id doesn't take the workspace down.
#[test]
fn a_forged_copy_of_the_seed_leaves_the_workspace_standing() {
    for how in [Forgery::UnsignedStampless, Forgery::UnsignedStamped, Forgery::WrongKey] {
        let (w, chat) = world();
        let seed = w.cfg[0].clone();
        let mut forged = seed.clone();
        if let SessionEvent::SessionSnapshot { session } = &mut forged.payload {
            session.title = "taken".into();
        }
        forged.clear_raw();
        match how {
            Forgery::WrongKey => sign_envelope(&mut forged, seed.signer_device_id.unwrap(), &SigningKeypair::generate().unwrap()),
            _ => {
                forged.signature = None;
                forged.signer_device_id = None;
                if matches!(how, Forgery::UnsignedStampless) {
                    forged.actor_stamp = None;
                }
            }
        }
        let mut cfg = w.cfg.clone();
        cfg.push(forged);
        let (s, _) = project_config(&cfg, None, None);
        let members: Vec<String> = s.expect("the workspace still folds").members.into_iter().map(|m| m.id).collect();
        assert!(members.contains(&"owner".to_string()) && members.contains(&"con".to_string()), "{how:?}: {members:?}");
        // The chats still fold against it.
        assert!(project_authorized(&w.chats[&chat], None, None, WorkspaceAuthz::Timeline(&project_config(&cfg, None, None).1)).is_some());
    }
}

/// A forged copy of a config event under another lamport doesn't make the
/// chat events that name the real one as their frontier look forged.
#[test]
fn a_forged_copy_of_a_frontier_does_not_deny_the_events_that_name_it() {
    for how in [Forgery::Stranger, Forgery::Insider, Forgery::UnsignedStamped, Forgery::WrongKey] {
        let (mut w, chat) = world();
        let anchor = w.push("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: None });
        let orig = find(&w, None, anchor);
        let posts: Vec<Uuid> = (0..3).map(|i| w.say(chat, "con", msg("con", &format!("p{i}")))).collect();
        // The posts name the anchor as their frontier.
        assert!(posts.iter().all(|p| find(&w, Some(chat), *p).actor_stamp.unwrap().cfg_frontier.unwrap().event_id == anchor));
        let forged = forge(&mut w, None, &orig, SessionEvent::WorkspaceRulesSet { version: 2, presence: None }, how, false);
        assert_ne!(forged.lamport, orig.lamport, "{how:?}: a copy under another lamport");
        let mut cfg = w.cfg.clone();
        cfg.push(forged);
        let v = chat_verdicts(&cfg, &w.chats[&chat]);
        for p in &posts {
            assert_eq!(v[p], Verdict::Allowed, "{how:?}: a post naming the real frontier was refused");
        }
    }
}

/// An insider's copy under an honest id is their own event: it folds (or
/// not) as theirs, and the honest one folds as it would have.
#[test]
fn an_insiders_copy_under_an_honest_id_folds_beside_it() {
    let (mut w, chat) = world();
    let id = w.say(chat, "con", msg("con", "honest"));
    let orig = find(&w, Some(chat), id);
    let forged = forge(&mut w, Some(chat), &orig, msg("mal", "insider"), Forgery::Insider, true);
    let mut log = w.chats[&chat].clone();
    log.push(forged);
    let bodies: Vec<String> = chat_fold(&w.cfg, &log).messages.into_iter().map(|m| m.body).collect();
    assert!(bodies.contains(&"honest".to_string()), "{bodies:?}");
    assert!(bodies.contains(&"insider".to_string()), "the insider's own signed event is theirs: {bodies:?}");
}

/// The owner's decision stands for what it was meant for: the same signer
/// signing two different events under one id is denied on every device, and
/// a forged third copy doesn't rescue either.
#[test]
fn same_signer_equivocation_is_still_denied_with_forged_copies_around() {
    for how in FORGERIES {
        let (mut w, chat) = world();
        w.say(chat, "con", title("before"));
        let id = w.say(chat, "con", title("one story"));
        let orig = find(&w, Some(chat), id);
        let mut twin = w.draft("con", Some(chat), title("the other story"), None);
        twin.event_id = id;
        twin.lamport = orig.lamport;
        if let Some(s) = twin.actor_stamp.as_mut() {
            let o = orig.actor_stamp.as_ref().unwrap();
            s.prev = o.prev;
            s.log_head = o.log_head;
        }
        w.finish(&mut twin, "con", None);
        let forged = forge(&mut w, Some(chat), &orig, forged_payload(how), how, true);
        let mut log = w.chats[&chat].clone();
        log.push(twin.clone());
        log.push(forged.clone());
        let mut rev = w.chats[&chat].clone();
        rev.insert(1, forged);
        rev.insert(1, twin);
        for l in [&log, &rev] {
            let s = chat_fold(&w.cfg, l);
            assert_eq!(s.title, "before", "{how:?}: neither of the signer's copies folds, nor the forgery");
            let v = chat_verdicts(&w.cfg, l);
            if !matches!(how, Forgery::Insider | Forgery::Stranger) {
                assert_eq!(v[&id], Verdict::Denied, "{how:?}");
            }
        }
    }
}

/// A phone-signed event (P-256) survives a forged copy the same way.
#[test]
fn a_forged_copy_never_erases_a_phone_signed_event() {
    for how in FORGERIES {
        let (mut w, chat) = world();
        let phone = Phone::new();
        let cert = w.phone_certificate("con", &phone);
        w.push_bare("con", SessionEvent::DeviceCertificateAdded { certificate: cert });
        w.say(chat, "con", title("before"));
        let id = w.phone_write("con", &phone, Some(chat), title("from the phone"));
        assert_eq!(w.verdict(id), Verdict::Allowed, "control");
        let orig = find(&w, Some(chat), id);
        let forged = forge(&mut w, Some(chat), &orig, forged_payload(how), how, true);
        let mut log = w.chats[&chat].clone();
        log.push(forged);
        assert_eq!(chat_fold(&w.cfg, &log).title, "from the phone", "{how:?}");
    }
}

// ---------------------------------------------------------------------------
// Property: 2–4 devices, each holding every honest event (plus the same
// signer's equivocating twins) and its own subset of forged copies, in its
// own arrival order. Every honest event folds as it does with no forgery
// around, every same-signer equivocation is denied, and two devices holding
// the same events in different orders fold identically.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Plan {
    n_chat: usize,
    n_cfg: usize,
    twins: u16,
    forgeries: Vec<(u8, u8, bool, bool)>,
    devices: Vec<(u64, u64)>,
}

fn plan() -> impl Strategy<Value = Plan> {
    (
        2usize..7,
        0usize..3,
        any::<u16>(),
        proptest::collection::vec((any::<u8>(), 0u8..5, any::<bool>(), any::<bool>()), 0..10),
        proptest::collection::vec((any::<u64>(), any::<u64>()), 2..=4),
    )
        .prop_map(|(n_chat, n_cfg, twins, forgeries, devices)| Plan { n_chat, n_cfg, twins, forgeries, devices })
}

fn shuffle<T>(v: &mut [T], mut s: u64) {
    for i in (1..v.len()).rev() {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        v.swap(i, (s >> 33) as usize % (i + 1));
    }
}

struct Outcome {
    verdicts: BTreeMap<Uuid, Verdict>,
    /// Per copy: the config log's, and the chat's.
    copies: (hive_core::CopyVerdicts, hive_core::CopyVerdicts),
    title: String,
    bodies: BTreeMap<Uuid, String>,
    members: Vec<(String, u8)>,
    whole: (Option<hive_core::ChatSession>, hive_core::ChatSession),
}

impl Outcome {
    /// The verdict on this very envelope.
    fn of(&self, e: &SessionEventEnvelope) -> Option<Verdict> {
        if e.session_id == workspace_config_session_id(e.workspace_id) {
            self.copies.0.get(e)
        } else {
            self.copies.1.get(e)
        }
    }
}

fn outcome(cfg: &[SessionEventEnvelope], log: &[SessionEventEnvelope]) -> Outcome {
    let (config, t) = project_config(cfg, None, None);
    let mut verdicts = fold_decisions(cfg, None, None, WorkspaceAuthz::Unchecked);
    verdicts.extend(fold_decisions(log, None, None, WorkspaceAuthz::Timeline(&t)));
    let s = project_authorized(log, None, None, WorkspaceAuthz::Timeline(&t)).unwrap();
    let copies = (
        fold_copy_decisions(cfg, None, None, WorkspaceAuthz::Unchecked),
        fold_copy_decisions(log, None, None, WorkspaceAuthz::Timeline(&t)),
    );
    let mut members: Vec<(String, u8)> = config.as_ref().unwrap().members.iter().map(|m| (m.id.clone(), m.role.rank())).collect();
    members.sort();
    Outcome {
        verdicts,
        copies,
        title: s.title.clone(),
        bodies: s.messages.iter().map(|m| (m.id, m.body.clone())).collect(),
        members,
        whole: (config, s),
    }
}

proptest! {
    // 64 cases by default; `PROPTEST_CASES=4096` in release for the gate.
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64), ..ProptestConfig::default() })]

    #[test]
    fn prop_forged_copies_never_erase_honest_events(p in plan()) {
        let (mut w, chat) = world();
        w.admit("owner", "dan", WorkspaceRole::Contributor);
        // Honest history: chat posts and title changes, config events
        // (rule writes and, last, the removal of a member).
        let mut honest: Vec<(Option<Uuid>, Uuid)> = Vec::new();
        for i in 0..p.n_chat {
            let by = if i % 3 == 0 { "owner" } else { "con" };
            let e = if i % 2 == 0 { title(&format!("t{i}")) } else { msg(by, &format!("m{i}")) };
            honest.push((Some(chat), w.say(chat, by, e)));
        }
        for _ in 0..p.n_cfg {
            honest.push((None, w.push("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: None })));
        }
        let signers: BTreeSet<Uuid> = [w.people["dan"].device_id].into_iter().collect();
        let wm = heads_of(&w.cfg, workspace_config_session_id(w.wid), &signers);
        honest.push((None, w.push("owner", SessionEvent::MemberRemoved { member_id: "dan".into(), watermark: wm })));
        // The same signer's twins (chat events only, by their own author).
        let mut twins: Vec<(Uuid, SessionEventEnvelope)> = Vec::new();
        for (i, (log, id)) in honest.iter().enumerate() {
            if log.is_none() || p.twins & (1 << i) == 0 {
                continue;
            }
            let orig = find(&w, *log, *id);
            let by = if orig.signer_device_id == Some(w.people["owner"].device_id) { "owner" } else { "con" };
            let mut twin = w.draft(by, *log, title("twin"), None);
            twin.event_id = *id;
            twin.lamport = orig.lamport;
            if let Some(s) = twin.actor_stamp.as_mut() {
                let o = orig.actor_stamp.as_ref().unwrap();
                s.prev = o.prev;
                s.log_head = o.log_head;
            }
            w.finish(&mut twin, by, None);
            twins.push((*id, twin));
        }
        let twinned: BTreeSet<Uuid> = twins.iter().map(|(id, _)| *id).collect();
        // Forged copies of honest ids (and of twinned ones).
        let mut forged: Vec<(Option<Uuid>, SessionEventEnvelope)> = Vec::new();
        for (target, how, same_lamport, as_title) in &p.forgeries {
            let (log, id) = honest[*target as usize % honest.len()];
            let orig = find(&w, log, id);
            let how = FORGERIES[*how as usize];
            // An insider forges only new messages (so the comparison below
            // stays about the honest events); anyone else forges anything.
            let payload = match (how, log, as_title) {
                (Forgery::Insider, Some(_), _) => msg("mal", "insider"),
                (Forgery::Insider, None, _) => SessionEvent::WorkspaceRulesSet { version: 2, presence: None },
                (_, Some(_), true) => title("forged"),
                (_, Some(_), false) => msg("eve", "forged"),
                (_, None, true) => SessionEvent::MemberAdded { member: w.member_of("dan", WorkspaceRole::Owner), invite: None, proof: None },
                (_, None, false) => SessionEvent::WorkspaceRulesSet { version: 2, presence: None },
            };
            forged.push((log, forge(&mut w, log, &orig, payload, how, *same_lamport)));
        }
        let insider_chat_copies = forged.iter().any(|(l, f)| l.is_some() && f.signer_device_id == Some(w.people["mal"].device_id));
        // The reference: honest events and twins, nothing forged.
        let base_cfg = w.cfg.clone();
        let mut base_log = w.chats[&chat].clone();
        base_log.extend(twins.iter().map(|(_, t)| t.clone()));
        let reference = outcome(&base_cfg, &base_log);
        for (id, twin) in &twins {
            prop_assert_eq!(reference.verdicts[id], Verdict::Denied, "a same-signer equivocation folds");
            prop_assert_eq!(reference.of(twin), Some(Verdict::Denied));
        }
        let honest_envs: Vec<SessionEventEnvelope> = honest.iter().map(|(l, id)| find(&w, *l, *id)).collect();
        for (d, (subset, order)) in p.devices.iter().enumerate() {
            let mut cfg = base_cfg.clone();
            let mut log = base_log.clone();
            for (i, (l, f)) in forged.iter().enumerate() {
                if subset & (1 << (i % 64)) == 0 {
                    continue;
                }
                match l {
                    None => cfg.push(f.clone()),
                    Some(_) => log.push(f.clone()),
                }
            }
            shuffle(&mut cfg, *order);
            shuffle(&mut log, order.rotate_left(17));
            let got = outcome(&cfg, &log);
            for e in &honest_envs {
                if twinned.contains(&e.event_id) {
                    prop_assert_eq!(got.of(e), Some(Verdict::Denied), "device {}: a same-signer equivocation folds", d);
                } else {
                    prop_assert_eq!(got.of(e), reference.of(e), "device {}: an honest event's verdict moved", d);
                    prop_assert_eq!(got.verdicts[&e.event_id], reference.verdicts[&e.event_id], "device {}: an honest id's verdict moved", d);
                }
            }
            for (_, twin) in &twins {
                prop_assert_eq!(got.of(twin), Some(Verdict::Denied), "device {}: a same-signer twin folds", d);
            }
            prop_assert_eq!(&got.members, &reference.members, "device {}: the roster moved", d);
            prop_assert_eq!(&got.title, &reference.title, "device {}: the title moved", d);
            for (mid, body) in &reference.bodies {
                prop_assert_eq!(got.bodies.get(mid), Some(body), "device {}: an honest message moved", d);
            }
            if !insider_chat_copies {
                prop_assert_eq!(got.bodies.len(), reference.bodies.len(), "device {}: a forged message folded", d);
            }
            // The same events in another order: the identical fold.
            let (mut cfg2, mut log2) = (cfg.clone(), log.clone());
            shuffle(&mut cfg2, order ^ 0x9e37_79b9_7f4a_7c15);
            shuffle(&mut log2, order.rotate_left(29));
            let again = outcome(&cfg2, &log2);
            prop_assert_eq!(&again.verdicts, &got.verdicts, "device {}: arrival order decided a verdict", d);
            prop_assert_eq!(&again.whole, &got.whole, "device {}: arrival order decided the fold", d);
        }
    }
}

/// Re-encoding a genuine envelope (whitespace or key order inside a signed
/// field) needs no key: the bytes differ, the typed value doesn't, and the
/// signature still verifies (the typed-preimage fallback). That copy is the
/// same event, not the signer's second one: it must not deny the original.
#[test]
fn a_re_encoded_copy_is_the_same_event_not_an_equivocation() {
    for chat_side in [true, false] {
        let (mut w, chat) = world();
        let (log_of, id) = if chat_side {
            w.say(chat, "con", title("before"));
            (Some(chat), w.say(chat, "con", title("honest")))
        } else {
            (None, w.push("owner", SessionEvent::WorkspaceRulesSet { version: 2, presence: None }))
        };
        let orig = find(&w, log_of, id);
        let wire = serde_json::to_string(&orig).unwrap();
        let p = wire.find("\"payload\":{").expect("payload on the wire");
        let mut re = wire.clone();
        re.insert(p + "\"payload\":{".len(), ' ');
        let reencoded: SessionEventEnvelope = serde_json::from_str(&re).unwrap();
        assert_ne!(hive_core::crypto::envelope_preimage(&reencoded), hive_core::crypto::envelope_preimage(&orig), "control: other bytes");
        let key = &w.people[if chat_side { "con" } else { "owner" }].device.public_key_bytes();
        assert!(hive_core::crypto::verify_envelope(&reencoded, key).is_ok(), "control: it still verifies");
        let orig_wire: SessionEventEnvelope = serde_json::from_str(&wire).unwrap();
        assert!(!hive_core::events::equivocates(&orig_wire, &reencoded), "one event, two encodings");
        if chat_side {
            let mut log = w.chats[&chat].clone();
            let at = log.iter().position(|e| e.event_id == id).unwrap();
            log[at] = orig_wire;
            log.push(reencoded);
            assert_eq!(chat_fold(&w.cfg, &log).title, "honest", "a re-encoded copy erased the event");
            assert_eq!(chat_verdicts(&w.cfg, &log)[&id], Verdict::Allowed);
        } else {
            let mut cfg = w.cfg.clone();
            let at = cfg.iter().position(|e| e.event_id == id).unwrap();
            cfg[at] = orig_wire;
            cfg.push(reencoded);
            assert_eq!(fold_decisions(&cfg, None, None, WorkspaceAuthz::Unchecked)[&id], Verdict::Allowed, "a re-encoded copy erased the config event");
        }
    }
}
