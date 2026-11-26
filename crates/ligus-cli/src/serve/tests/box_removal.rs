//! Removing a box from the relay roster (People > server-side members) also
//! removes it from the workspace before the key rotation (0.1.1).
//!
//! Before, the box's relay subject (`ligus-box:<id>`) mapped to no workspace
//! member, so nothing wrote a `MemberRemoved` for its service member and the
//! rotation, sealed to every current member's devices, sealed the new key to
//! the box: a removed box kept reading new traffic. Now the desktop links the
//! subject to the box admission it issued with it, and the admission's
//! redemption names the box's member.
//!
//! A real desktop (founder), a person (`ana`) and a box, synced through the
//! in-process test relay.

use ligus_runtime::chat_service::{
    PresenceProvider, PresenceRequest, RelayRemovalError, RelaySubject, BOX_SUBJECT_PREFIX,
};

use super::*;

struct World {
    ws: Uuid,
    relay: String,
    room: String,
    desktop: Config,
    ana: Config,
    boxc: Config,
    /// The box's relay subject, linked on the desktop to its admission.
    subject: String,
    invite: Uuid,
    box_member: String,
    box_device: Uuid,
    ana_member: String,
    ana_account: Uuid,
    ana_device: Uuid,
    _tmp: tempfile::TempDir,
}

fn new_subject() -> String {
    format!("{BOX_SUBJECT_PREFIX}{}", Uuid::new_v4())
}

async fn sync(cfg: &Config) {
    tokio::time::timeout(REQ, sync_once(cfg))
        .await
        .expect("a sync round in time")
        .unwrap();
}

/// The workspace, with ana joined and the box admitted (as the People pane's
/// Remote agents bootstrap does: a box admission linked to a fresh subject).
async fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let relay = spawn(test_relay::router()).await;
    let room = format!("box-remove-{}", Uuid::new_v4());
    let ws = crate::uuid_of_room(&room);
    let config_id = workspace_config_session_id(ws);
    let desktop = config(&tmp.path().join("desktop"), &relay, &room);
    let subject = new_subject();
    let (invite, box_code, ana_code) = {
        let mut svc = open_service(&desktop).unwrap();
        svc.ensure_self_member(config_id, ws).unwrap();
        crate::publish_key_agreement(&desktop, &mut svc);
        let (inv, seed) = svc.issue_box_admission(ws, 3_600_000, None).unwrap();
        svc.store()
            .set_box_relay_admission(ws, &subject, inv)
            .unwrap();
        let box_code = crate::serve::admission_code(&desktop, &svc, inv, &seed).unwrap();
        let person = svc.issue_invite(ws).unwrap();
        let pseed = svc
            .take_invite_redeem_seed(ws, person)
            .unwrap()
            .expect("the person invite's redeem key");
        let ana_code = crate::serve::admission_code(&desktop, &svc, person, &pseed).unwrap();
        (inv, box_code, ana_code)
    };
    sync(&desktop).await;

    // ana, a person, joins with her invite.
    let ana = config(&tmp.path().join("ana"), &relay, &room);
    crate::cmd_enroll(&ana, &ana_code, None).unwrap();
    sync(&ana).await;
    let (ana_member, ana_account, ana_device) = {
        let mut svc = open_service(&ana).unwrap();
        svc.ensure_self_member(config_id, ws).unwrap();
        crate::publish_key_agreement(&ana, &mut svc);
        (
            svc.author().id.clone(),
            svc.author().account_id.unwrap(),
            svc.device_id(),
        )
    };
    sync(&ana).await;

    // The box joins through its admission, as `ligus worker` does.
    let mut boxc = config(&tmp.path().join("box"), &relay, &room);
    let (box_member, box_device) = {
        let (mut svc, _) =
            crate::admit::prepare_box(&mut boxc, "build-1", None, Some(box_code), |c| async move {
                sync_once(&c).await
            })
            .await
            .unwrap();
        crate::publish_key_agreement(&boxc, &mut svc);
        assert_eq!(svc.author().kind, ligus_core::ActorKind::Service);
        (svc.author().id.clone(), svc.device_id())
    };
    sync(&boxc).await;
    sync(&desktop).await;
    World {
        ws,
        relay,
        room,
        desktop,
        ana,
        boxc,
        subject,
        invite,
        box_member,
        box_device,
        ana_member,
        ana_account,
        ana_device,
        _tmp: tmp,
    }
}

fn is_member(cfg: &Config, ws: Uuid, member: &str) -> bool {
    open_service(cfg)
        .unwrap()
        .load_in(ws, workspace_config_session_id(ws))
        .unwrap()
        .unwrap()
        .members
        .iter()
        .any(|m| m.id == member)
}

/// The box's own key-agreement keypair.
fn box_ka(w: &World) -> ligus_core::KeyAgreementKeypair {
    let ids = ligus_runtime::IdentityStore::new(
        &w.boxc.data_dir,
        ligus_runtime::secret_store::StoreKeyVault::for_data_dir(&w.boxc.data_dir),
    );
    ids.key_agreement(w.box_device).unwrap()
}

/// The fix: removing the box's relay subject removes its service member
/// first, so the rotation's recipients never include the box (by device or
/// by key-agreement key), and the box's later fetches can't open what was
/// said after. A person still on the roster reads it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_box_is_left_out_of_the_rotation_and_cant_read_what_follows() {
    let w = world().await;
    let box_dev = w.box_device.to_string();
    let mut svc = open_service(&w.desktop).unwrap();

    // Control (the gap before 0.1.1): with the subject mapped to no member,
    // the relay removal wrote nothing and the rotation sealed to the box.
    let (_, before) = svc
        .remove_for_relay_subject_and_rotate(w.ws, &RelaySubject::Unmapped { box_like: true })
        .unwrap();
    assert!(
        before.rotation.sealed.contains_key(&box_dev),
        "control: an unmapped removal leaves the box a recipient"
    );
    assert!(
        before
            .rotation
            .sealed
            .contains_key(&w.ana_device.to_string()),
        "control: ana is a recipient"
    );

    let target = svc.relay_subject_target(w.ws, &w.subject).unwrap();
    assert!(
        matches!(&target, RelaySubject::Box { invite, member: Some(m), redemption: Some(_) } if *invite == w.invite && *m == w.box_member),
        "the subject maps to the box's member through its admission: {target:?}"
    );
    let (removal, issued) = svc
        .remove_for_relay_subject_and_rotate(w.ws, &target)
        .unwrap();
    assert_eq!(removal.removed, vec![w.box_member.clone()]);
    assert!(!removal.unmapped());
    assert!(
        !issued.rotation.sealed.contains_key(&box_dev),
        "the rotation sealed the new key to the removed box"
    );
    let ka = box_ka(&w);
    assert!(
        issued
            .rotation
            .sealed
            .values()
            .all(|blob| ligus_core::e2ee::open(&ka, blob).is_err()),
        "a sealed copy opens with the box's key-agreement key"
    );
    assert!(
        issued
            .rotation
            .sealed
            .contains_key(&w.ana_device.to_string()),
        "ana still gets the key"
    );
    assert!(issued
        .rotation
        .sealed
        .contains_key(&svc.device_id().to_string()));
    let config_log = svc
        .load_in(w.ws, workspace_config_session_id(w.ws))
        .unwrap()
        .unwrap();
    assert!(
        !config_log.members.iter().any(|m| m.id == w.box_member),
        "the box is off the roster"
    );
    assert!(
        config_log.guards.closed_admissions.contains_key(&w.invite),
        "its admission is closed"
    );

    // Said after the removal.
    let chat = svc.create_chat("after", w.ws, "rt").unwrap();
    let said = svc
        .post_user_message(chat.id, w.ws, "said after the box was removed")
        .unwrap()
        .id;
    drop(svc);
    sync(&w.desktop).await;
    // Two rounds each: the first folds the rotation, the second opens what
    // was parked until the new key was there.
    for _ in 0..2 {
        let _ = tokio::time::timeout(REQ, sync_once(&w.boxc))
            .await
            .expect("the box's fetch in time");
        sync(&w.ana).await;
    }

    let ring = crate::box_ring(&w.boxc).unwrap();
    assert!(
        !ring.keys_for(before.rotation.version, None).is_empty(),
        "control: the box held the key from before its removal"
    );
    assert!(
        ring.keys_for(issued.rotation.version, None).is_empty(),
        "the box holds the post-removal key"
    );
    let on_box = open_service(&w.boxc)
        .unwrap()
        .load_resolving(chat.id)
        .unwrap();
    assert!(
        on_box.is_none_or(|s| !s.messages.iter().any(|m| m.id == said)),
        "the removed box read a message sent after its removal"
    );
    let on_ana = open_service(&w.ana)
        .unwrap()
        .load_resolving(chat.id)
        .unwrap()
        .expect("ana got the chat");
    assert!(on_ana.messages.iter().any(|m| m.id == said), "ana reads it");
}

/// A box whose admission was never redeemed: removing its subject closes the
/// admission (naming no redemption), so the box can't join through it later.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_a_box_that_never_joined_closes_its_admission() {
    let w = world().await;
    let config_id = workspace_config_session_id(w.ws);
    let subject = new_subject();
    let (invite, code) = {
        let mut svc = open_service(&w.desktop).unwrap();
        let (inv, seed) = svc.issue_box_admission(w.ws, 3_600_000, None).unwrap();
        svc.store()
            .set_box_relay_admission(w.ws, &subject, inv)
            .unwrap();
        (
            inv,
            crate::serve::admission_code(&w.desktop, &svc, inv, &seed).unwrap(),
        )
    };
    {
        let mut svc = open_service(&w.desktop).unwrap();
        let target = svc.relay_subject_target(w.ws, &subject).unwrap();
        assert_eq!(
            target,
            RelaySubject::Box {
                invite,
                member: None,
                redemption: None
            }
        );
        let (removal, issued) = svc
            .remove_for_relay_subject_and_rotate(w.ws, &target)
            .unwrap();
        assert!(removal.removed.is_empty());
        assert_eq!(removal.closed_admission, Some(invite));
        assert_eq!(
            svc.load_resolving(config_id)
                .unwrap()
                .unwrap()
                .guards
                .closed_admissions
                .get(&invite),
            Some(&vec![None])
        );
        // The first box, untouched, is still a recipient.
        assert!(issued
            .rotation
            .sealed
            .contains_key(&w.box_device.to_string()));
    }
    sync(&w.desktop).await;

    // The late box can't join with it.
    let mut late = config(&w._tmp.path().join("late-box"), &w.relay, &w.room);
    let joined = crate::admit::prepare_box(&mut late, "late", None, Some(code), |c| async move {
        sync_once(&c).await
    })
    .await;
    assert!(joined.is_err(), "a box joined through a closed admission");
    sync(&w.desktop).await;
    assert!(open_service(&w.desktop)
        .unwrap()
        .load_resolving(config_id)
        .unwrap()
        .unwrap()
        .members
        .iter()
        .all(|m| m.actor.kind != ligus_core::ActorKind::Service || m.id == w.box_member));
}

/// Removing a person works as before: every member of their account leaves
/// the roster before the rotation, which leaves their device out. Boxes are
/// their own service members, so removing a person (even the Admin who
/// admitted a box) leaves every box in place: remove a box on its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_a_person_still_works_and_keeps_boxes() {
    let w = world().await;
    let mut svc = open_service(&w.desktop).unwrap();
    let target = svc
        .relay_subject_target(w.ws, &format!("ligus:{}", w.ana_account))
        .unwrap();
    assert_eq!(
        target,
        RelaySubject::Person {
            members: vec![w.ana_member.clone()]
        }
    );
    let (removal, issued) = svc
        .remove_for_relay_subject_and_rotate(w.ws, &target)
        .unwrap();
    assert_eq!(removal.removed, vec![w.ana_member.clone()]);
    assert!(
        !issued
            .rotation
            .sealed
            .contains_key(&w.ana_device.to_string()),
        "the removed person still gets the key"
    );
    assert!(
        issued
            .rotation
            .sealed
            .contains_key(&w.box_device.to_string()),
        "a box stays a member"
    );
    drop(svc);
    assert!(!is_member(&w.desktop, w.ws, &w.ana_member));
    assert!(is_member(&w.desktop, w.ws, &w.box_member));

    // Subjects this device can't map are said to be unmapped, never guessed.
    let svc = open_service(&w.desktop).unwrap();
    assert_eq!(
        svc.relay_subject_target(w.ws, &new_subject()).unwrap(),
        RelaySubject::Unmapped { box_like: true }
    );
    assert_eq!(
        svc.relay_subject_target(w.ws, &format!("ligus:{}", Uuid::new_v4()))
            .unwrap(),
        RelaySubject::Unmapped { box_like: true }
    );
    assert_eq!(
        svc.relay_subject_target(w.ws, "acme:ana").unwrap(),
        RelaySubject::Unmapped { box_like: false }
    );
}

/// A passkey confirmation the Admin cancels.
struct Cancelled;

impl PresenceProvider for Cancelled {
    fn assert(
        &self,
        _: &PresenceRequest,
    ) -> std::result::Result<ligus_core::presence::PresenceAssertion, String> {
        Err("the passkey prompt was cancelled".into())
    }
}

/// A failure while writing the box's `MemberRemoved` stops before the
/// rotation: nothing is written, the box is still a member, and the error
/// says the key was not rotated.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_removal_never_rotates() {
    let w = world().await;
    let mut svc = open_service(&w.desktop).unwrap();
    let before = svc.store().config_envelopes(w.ws).unwrap().len();
    let target = svc.relay_subject_target(w.ws, &w.subject).unwrap();
    svc.set_presence_provider(Some(Arc::new(Cancelled)));
    let err = svc
        .remove_for_relay_subject_and_rotate(w.ws, &target)
        .err()
        .expect("the removal needs the Admin's passkey");
    assert!(matches!(err, RelayRemovalError::Removal(_)), "{err:?}");
    assert!(err.to_string().contains("the key was not rotated"), "{err}");
    let envs = svc.store().config_envelopes(w.ws).unwrap();
    assert_eq!(envs.len(), before, "a half-done removal was written");
    assert!(
        !envs.iter().any(|e| matches!(
            e.payload,
            ligus_core::SessionEvent::WorkspaceKeyRotated { .. }
        )),
        "rotated anyway"
    );
    drop(svc);
    assert!(
        is_member(&w.desktop, w.ws, &w.box_member),
        "the box was removed without its MemberRemoved"
    );
}

/// The common way a box joins: `ligus admit request` on the box, the Admin's
/// sealed admission for that request (desktop **Admit a box…** or `ligus
/// admit issue --request`), `ligus admit redeem`. Such a box has no relay
/// subject this desktop minted. It is removed from its member row in the
/// People pane (**Members → Remove & revoke**, `remove_and_revoke`): the box
/// is listed in every chat's member projection, and that path writes
/// `MemberRemoved` for its service member on the config log and then
/// rotates, so the box is left out and can't read what follows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_box_admitted_from_its_join_request_is_removed_from_its_member_row() {
    let w = world().await;
    let config_id = workspace_config_session_id(w.ws);
    let mut home = config(&w._tmp.path().join("home-server"), &w.relay, &w.room);
    let request = crate::admit::join_request(&home, "home-server").unwrap();
    let sealed = {
        let mut svc = open_service(&w.desktop).unwrap();
        let (inv, seed) = svc
            .issue_box_admission(w.ws, 3_600_000, Some(&request))
            .unwrap();
        request
            .seal(&crate::serve::admission_code(&w.desktop, &svc, inv, &seed).unwrap())
            .unwrap()
    };
    sync(&w.desktop).await;
    crate::admit::redeem(&mut home, &sealed).unwrap();
    let (member, device) = {
        let (mut svc, _) =
            crate::admit::prepare_box(&mut home, "home-server", None, None, |c| async move {
                sync_once(&c).await
            })
            .await
            .unwrap();
        crate::publish_key_agreement(&home, &mut svc);
        (svc.author().id.clone(), svc.device_id())
    };
    sync(&home).await;
    sync(&w.desktop).await;
    let dev = device.to_string();

    let mut svc = open_service(&w.desktop).unwrap();
    let control = svc.rotate_workspace_key(w.ws).unwrap();
    assert!(
        control.rotation.sealed.contains_key(&dev),
        "control: the admitted box gets the key"
    );
    // The People pane lists it on a chat's members (the config roster is
    // hoisted onto every chat), with the remove actions.
    let chat = svc.create_chat("ops", w.ws, "rt").unwrap();
    assert!(svc
        .load_resolving(chat.id)
        .unwrap()
        .unwrap()
        .members
        .iter()
        .any(|m| m.id == member && m.actor.kind == ligus_core::ActorKind::Service));

    // Remove & revoke on its row: remove_member (routed to the config log),
    // then the rotation.
    svc.remove_member(chat.id, w.ws, member.clone()).unwrap();
    assert!(
        !svc.load_resolving(config_id)
            .unwrap()
            .unwrap()
            .members
            .iter()
            .any(|m| m.id == member),
        "MemberRemoved on the config log"
    );
    let issued = svc.rotate_workspace_key(w.ws).unwrap();
    assert!(
        !issued.rotation.sealed.contains_key(&dev),
        "the rotation sealed to the removed box"
    );
    assert!(issued
        .rotation
        .sealed
        .contains_key(&w.ana_device.to_string()));
    let said = svc
        .post_user_message(chat.id, w.ws, "after the home server left")
        .unwrap()
        .id;
    drop(svc);
    sync(&w.desktop).await;
    for _ in 0..2 {
        let _ = tokio::time::timeout(REQ, sync_once(&home))
            .await
            .expect("the box's fetch in time");
        sync(&w.ana).await;
    }
    assert!(
        crate::box_ring(&home)
            .unwrap()
            .keys_for(issued.rotation.version, None)
            .is_empty(),
        "the box holds the new key"
    );
    let on_box = open_service(&home)
        .unwrap()
        .load_resolving(chat.id)
        .unwrap();
    assert!(
        on_box.is_none_or(|s| !s.messages.iter().any(|m| m.id == said)),
        "the removed box read what followed"
    );
    let on_ana = open_service(&w.ana)
        .unwrap()
        .load_resolving(chat.id)
        .unwrap()
        .expect("ana got the chat");
    assert!(on_ana.messages.iter().any(|m| m.id == said));
}
