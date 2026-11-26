//! Forged copies against cut watermarks (review of fix/forged-equivocation).
//! A watermark covers a signer's chain by `(device id, event id)`. A device
//! id is derived from its key (`ligus_core::crypto::device_id_for`), so a
//! second key for a device id (a member's own, or a squatter's for another
//! member's device id) is refused: a certificate binding one never verifies.
//! These are the review's attacks, which once worked through such a binding:
//! a removed member's backdated copy under a covered id, or a chain its copy
//! extends through `prev`, must never count as held by the remover.

use ligus_core::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
use ligus_core::events::{project_authorized, project_config, WorkspaceAuthz};
use ligus_core::testkit::SignedWorld;
use ligus_core::watermark::heads_of;
use ligus_core::{
    workspace_config_session_id, CfgFrontier, ChatMessage, MessageRole, SessionEvent,
    SessionEventEnvelope, Timestamp, Verdict, WorkspaceRole,
};
use std::collections::BTreeSet;
use uuid::Uuid;

fn msg(by: &str, body: &str) -> SessionEvent {
    SessionEvent::MessageAppended {
        message: ChatMessage::new(MessageRole::User, by, body),
    }
}

fn world() -> (SignedWorld, Uuid) {
    let mut w = SignedWorld::genesis(&["owner", "con", "mal"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.admit("owner", "mal", WorkspaceRole::Contributor);
    let chat = w.chat("owner", "c");
    (w, chat)
}

/// `mal` tries to bind `device` to a fresh key of mal's own account: the
/// certificate is refused (the id isn't the one the key derives), and the
/// key is returned for the attacks that follow.
fn bind(w: &mut SignedWorld, device: Uuid) -> SigningKeypair {
    let key = SigningKeypair::generate().unwrap();
    let mal = &w.people["mal"];
    let cert = DeviceCertificate::issue_for_id(
        &mal.account,
        mal.account_id,
        device,
        &key.public_key_bytes(),
        Timestamp::epoch(),
    );
    let id = w.push(
        "mal",
        SessionEvent::DeviceCertificateAdded { certificate: cert },
    );
    assert_eq!(
        w.verdict(id),
        Verdict::Denied,
        "a certificate under a device id its key doesn't derive was allowed"
    );
    let roster = w.roster();
    assert_ne!(
        roster.devices[&device].public_key,
        key.public_key_bytes().to_vec(),
        "the device id's key changed"
    );
    key
}

/// The production watermark for removing `mal` (`ChatService::cut_watermark`):
/// every device whose certificates include mal's account, every log.
fn removal_watermark(w: &SignedWorld, chat: Uuid) -> Vec<ligus_core::watermark::Watermark> {
    let roster = w.roster();
    let a = w.people["mal"].account_id;
    let signers: BTreeSet<Uuid> = roster
        .devices
        .iter()
        .filter(|(_, d)| d.accounts.contains(&a))
        .map(|(id, _)| *id)
        .collect();
    let mut wm = heads_of(&w.cfg, workspace_config_session_id(w.wid), &signers);
    wm.extend(heads_of(&w.chats[&chat], chat, &signers));
    wm
}

/// mal's message under `orig`'s id and position, judged at `frontier`,
/// naming `prev`, signed by `device` with `key`.
#[allow(clippy::too_many_arguments)]
fn copy_under(
    w: &mut SignedWorld,
    chat: Uuid,
    orig: &SessionEventEnvelope,
    body: &str,
    frontier: CfgFrontier,
    prev: Option<Uuid>,
    device: Uuid,
    key: &SigningKeypair,
) -> SessionEventEnvelope {
    let mut f = w.draft("mal", Some(chat), msg("mal", body), Some(frontier));
    f.event_id = orig.event_id;
    f.lamport = orig.lamport;
    if let Some(s) = f.actor_stamp.as_mut() {
        s.log_head = orig.actor_stamp.as_ref().unwrap().log_head;
        s.prev = prev;
    }
    f.clear_raw();
    sign_envelope(&mut f, device, key);
    f
}

fn bodies(w: &SignedWorld, log: &[SessionEventEnvelope]) -> Vec<String> {
    let t = project_config(&w.cfg, None, None).1;
    project_authorized(log, None, None, WorkspaceAuthz::Timeline(&t))
        .unwrap()
        .messages
        .into_iter()
        .map(|m| m.body)
        .collect()
}

/// A removed member's second key for their own device id: a copy under a
/// covered id of their own chain is not covered.
#[test]
fn a_second_key_copy_under_a_covered_id_is_not_covered() {
    let (mut w, chat) = world();
    let dev = w.people["mal"].device_id;
    let k2 = bind(&mut w, dev);
    let before = w.head().unwrap();
    let m1 = w.say(chat, "mal", msg("mal", "honest"));
    let orig = w.chats[&chat]
        .iter()
        .find(|e| e.event_id == m1)
        .unwrap()
        .clone();
    let wm = removal_watermark(&w, chat);
    let cut = w.push(
        "owner",
        SessionEvent::MemberRemoved {
            member_id: "mal".into(),
            watermark: wm,
        },
    );
    assert_eq!(w.verdict(cut), Verdict::Allowed);
    let f = copy_under(
        &mut w,
        chat,
        &orig,
        "backdated",
        before,
        orig.actor_stamp.as_ref().unwrap().prev,
        dev,
        &k2,
    );
    let mut log = w.chats[&chat].clone();
    log.push(f);
    let b = bodies(&w, &log);
    assert!(
        !b.contains(&"backdated".to_string()),
        "a backdated copy after the removal folded as covered: {b:?}"
    );
}

/// The copy names a new event as its `prev`: coverage must not walk into it.
#[test]
fn a_copy_under_a_covered_head_does_not_extend_coverage_through_prev() {
    let (mut w, chat) = world();
    let dev = w.people["mal"].device_id;
    let k2 = bind(&mut w, dev);
    let before = w.head().unwrap();
    let m1 = w.say(chat, "mal", msg("mal", "honest"));
    let orig = w.chats[&chat]
        .iter()
        .find(|e| e.event_id == m1)
        .unwrap()
        .clone();
    let wm = removal_watermark(&w, chat);
    w.push(
        "owner",
        SessionEvent::MemberRemoved {
            member_id: "mal".into(),
            watermark: wm,
        },
    );
    // A new backdated event, signed with mal's ordinary key, single copy.
    let mut y = w.draft("mal", Some(chat), msg("mal", "smuggled"), Some(before));
    y.lamport = orig.lamport;
    if let Some(s) = y.actor_stamp.as_mut() {
        s.log_head = orig.actor_stamp.as_ref().unwrap().log_head;
        s.prev = None;
    }
    w.finish(&mut y, "mal", None);
    let f = copy_under(&mut w, chat, &orig, "x", before, Some(y.event_id), dev, &k2);
    let mut log = w.chats[&chat].clone();
    log.push(y);
    log.push(f);
    let b = bodies(&w, &log);
    assert!(
        !b.contains(&"smuggled".to_string()),
        "coverage walked through a copy's prev: {b:?}"
    );
}

/// A squat attempt on con's device id (refused); the removal's watermark
/// doesn't name con's chain, and mal's copy under con's id must not count as
/// covered.
#[test]
fn a_squatters_copy_under_the_victims_covered_id_is_not_covered() {
    let (mut w, chat) = world();
    let con_dev = w.people["con"].device_id;
    let km = bind(&mut w, con_dev);
    let before = w.head().unwrap();
    let c1 = w.say(chat, "con", msg("con", "honest"));
    let orig = w.chats[&chat]
        .iter()
        .find(|e| e.event_id == c1)
        .unwrap()
        .clone();
    let wm = removal_watermark(&w, chat);
    assert!(
        !wm.iter().any(|x| x.signer == con_dev),
        "the squat attempt made con's device one of mal's"
    );
    w.push(
        "owner",
        SessionEvent::MemberRemoved {
            member_id: "mal".into(),
            watermark: wm,
        },
    );
    let f = copy_under(
        &mut w,
        chat,
        &orig,
        "backdated",
        before,
        orig.actor_stamp.as_ref().unwrap().prev,
        con_dev,
        &km,
    );
    let mut log = w.chats[&chat].clone();
    log.push(f);
    let b = bodies(&w, &log);
    assert!(b.contains(&"honest".to_string()), "{b:?}");
    assert!(
        !b.contains(&"backdated".to_string()),
        "a squatter's backdated copy folded as covered: {b:?}"
    );
}

/// A squatter's own events under the victim's device id must not decide the
/// victim's coverage: mal tries to bind con's device id (refused) and posts,
/// signed under it with mal's key, events naming ids that were never
/// published (gaps). None of them is con's device's, and ingest keeps none
/// (no key bound to the id verifies them). When an Owner later removes con
/// with the production watermark, con's earlier posts stay covered.
#[test]
fn a_squatters_events_under_the_victims_device_id_do_not_uncover_the_victim() {
    let (mut w, chat) = world();
    let con_dev = w.people["con"].device_id;
    let km = bind(&mut w, con_dev);
    let early = w.say(chat, "con", msg("con", "honest"));
    for i in 0..(ligus_core::watermark::MAX_HEADS + 1) {
        let mut e = w.draft("mal", Some(chat), msg("mal", &format!("noise {i}")), None);
        if let Some(s) = e.actor_stamp.as_mut() {
            s.prev = Some(Uuid::new_v4());
        }
        e.clear_raw();
        sign_envelope(&mut e, con_dev, &km);
        w.push_env(Some(chat), e);
    }
    // The production watermark for removing con.
    let roster = w.roster();
    let a = w.people["con"].account_id;
    let signers: BTreeSet<Uuid> = roster
        .devices
        .iter()
        .filter(|(_, d)| d.accounts.contains(&a))
        .map(|(id, _)| *id)
        .collect();
    // What a device holds: ingest keeps an event only when its device's key
    // (the one its id derives from) verifies it for an account that
    // certified the device.
    let held: Vec<SessionEventEnvelope> = w.chats[&chat]
        .iter()
        .filter(|e| {
            e.signer_device_id
                .and_then(|d| roster.devices.get(&d))
                .is_none_or(|d| d.verifying_key(e).is_some())
        })
        .cloned()
        .collect();
    assert_eq!(
        held.len() + ligus_core::watermark::MAX_HEADS + 1,
        w.chats[&chat].len(),
        "a squatter's event under con's device id verifies"
    );
    let mut wm = heads_of(&w.cfg, workspace_config_session_id(w.wid), &signers);
    wm.extend(heads_of(&held, chat, &signers));
    w.push(
        "owner",
        SessionEvent::MemberRemoved {
            member_id: "con".into(),
            watermark: wm,
        },
    );
    assert_eq!(
        w.verdict(early),
        Verdict::Allowed,
        "con's post before the removal lost its coverage"
    );
}
