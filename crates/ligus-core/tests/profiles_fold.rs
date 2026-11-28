//! `ProfileUpdated` and `DeviceLabelSet` in the fold (identity model v2, 5
//! and 6; tests P1 and L1): who may write them, the content checks, and
//! convergence whatever order the events arrive in.

use ligus_core::crypto::{sign_envelope, DeviceCertificate, SigningKeypair};
use ligus_core::events::Verdict;
use ligus_core::profile::{Avatar, DeviceLabel, ProfileChange, MAX_AVATAR_BYTES};
use ligus_core::testkit::{Phone, SignedWorld};
use ligus_core::{ActorKind, SessionEvent, SessionEventEnvelope, Timestamp, WorkspaceRole};
use uuid::Uuid;

fn png() -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&[7u8; 40]);
    v
}

fn name(account: Uuid, n: &str) -> SessionEvent {
    SessionEvent::ProfileUpdated {
        profile: ProfileChange { account_id: account, display_name: Some(n.into()), avatar: None, clear_avatar: false, issued_at_ms: 1 },
    }
}

fn picture(account: Uuid, a: Avatar) -> SessionEvent {
    SessionEvent::ProfileUpdated {
        profile: ProfileChange { account_id: account, display_name: None, avatar: Some(a), clear_avatar: false, issued_at_ms: 1 },
    }
}

fn label(device: Uuid, account: Uuid, l: &str) -> SessionEvent {
    SessionEvent::DeviceLabelSet { label: DeviceLabel { device_id: device, account_id: account, label: l.into() } }
}

/// Ana (Owner), Ben (Admin) and Cy (Contributor), all members.
fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    w
}

/// A second device of `who` that `who`'s own desktop certified (a linked
/// computer: delegated), and an envelope drafter for it.
struct Extra {
    id: Uuid,
    kp: SigningKeypair,
}

fn linked_device(w: &mut SignedWorld, who: &str) -> Extra {
    let kp = SigningKeypair::generate().unwrap();
    let id = ligus_core::crypto::device_id_for(ligus_core::crypto::SigAlg::Ed25519, &kp.public_key_bytes());
    let p = &w.people[who];
    let cert = DeviceCertificate::issue(&p.account, p.account_id, &kp.public_key_bytes(), Timestamp::epoch());
    w.push(who, SessionEvent::DeviceCertificateAdded { certificate: cert });
    Extra { id, kp }
}

/// `payload` stamped as `who` but signed by `dev`.
fn from_device(w: &mut SignedWorld, who: &str, dev: &Extra, payload: SessionEvent) -> Uuid {
    let mut env: SessionEventEnvelope = w.draft(who, None, payload, None);
    if let Some(s) = env.actor_stamp.as_mut() {
        s.actor.device_id = Some(dev.id);
        s.prev = None;
    }
    env.clear_raw();
    sign_envelope(&mut env, dev.id, &dev.kp);
    w.push_env(None, env)
}

#[test]
fn a_person_sets_their_own_name_and_picture_from_their_desktop() {
    let mut w = world();
    let ana = w.people["ana"].account_id;
    let n = w.push_bare("ana", name(ana, "Ana Ruiz"));
    let a = Avatar::new("image/png", &png()).unwrap();
    let p = w.push_bare("ana", picture(ana, a.clone()));
    assert_eq!(w.verdict(n), Verdict::Allowed);
    assert_eq!(w.verdict(p), Verdict::Allowed);
    let c = w.config();
    let prof = &c.profiles[&ana];
    assert_eq!(prof.display_name.as_deref(), Some("Ana Ruiz"));
    assert_eq!(prof.avatar.as_ref(), Some(&a), "a picture-only change keeps the name");
    // Clearing the picture keeps the name.
    w.push_bare("ana", SessionEvent::ProfileUpdated {
        profile: ProfileChange { account_id: ana, display_name: None, avatar: None, clear_avatar: true, issued_at_ms: 2 },
    });
    let c = w.config();
    assert!(c.profiles[&ana].avatar.is_none());
    assert_eq!(c.profiles[&ana].display_name.as_deref(), Some("Ana Ruiz"));
    // A snapshot of the config never carries profiles (a snapshot can't plant one).
    let json = serde_json::to_string(&c).unwrap();
    assert!(!json.contains("Ana Ruiz") || !json.contains("profiles"), "{json}");
}

#[test]
fn nobody_sets_someone_elses_profile() {
    let mut w = world();
    let ana = w.people["ana"].account_id;
    let id = w.push_bare("ben", name(ana, "Not Ana"));
    assert_eq!(w.verdict(id), Verdict::Denied);
    assert!(!w.config().profiles.contains_key(&ana));
}

#[test]
fn a_linked_device_or_a_phone_cant_rename_its_person() {
    let mut w = world();
    let ana = w.people["ana"].account_id;
    let lt = linked_device(&mut w, "ana");
    let id = from_device(&mut w, "ana", &lt, name(ana, "Stolen Laptop"));
    assert_eq!(w.verdict(id), Verdict::Denied, "a delegated device renamed its person");
    // The linked device still names itself.
    let l = from_device(&mut w, "ana", &lt, label(lt.id, ana, "lt on mbp"));
    assert_eq!(w.verdict(l), Verdict::Allowed);
    // …but no other device of Ana's.
    let desk = w.people["ana"].device_id;
    let l2 = from_device(&mut w, "ana", &lt, label(desk, ana, "renamed by lt"));
    assert_eq!(w.verdict(l2), Verdict::Denied);
    // Ana's desktop renames the linked device.
    let l3 = w.push_bare("ana", label(lt.id, ana, "Ana's lt"));
    assert_eq!(w.verdict(l3), Verdict::Allowed);
    assert_eq!(w.config().device_labels[&lt.id][&ana], "Ana's lt");

    // A phone: neither its person's profile nor a label.
    let phone = Phone::new();
    let cert = w.phone_certificate("ana", &phone);
    w.push("ana", SessionEvent::DeviceCertificateAdded { certificate: cert });
    let p1 = w.phone_write("ana", &phone, None, name(ana, "From Phone"));
    let p2 = w.phone_write("ana", &phone, None, label(phone.device_id, ana, "Ana's iPhone"));
    assert_eq!(w.verdict(p1), Verdict::Denied);
    assert_eq!(w.verdict(p2), Verdict::Denied);
    // The desktop names the phone.
    let p3 = w.push_bare("ana", label(phone.device_id, ana, "Ana's iPhone"));
    assert_eq!(w.verdict(p3), Verdict::Allowed);
    assert_eq!(w.config().device_labels[&phone.device_id][&ana], "Ana's iPhone");
}

#[test]
fn content_checks_are_in_the_fold() {
    let mut w = world();
    let cy = w.people["cy"].account_id;
    let bad_names = ["", " Cy", "Cy ", "C  y", "C\u{202E}y", "C\u{200B}y", "Cy\u{0007}"];
    for n in bad_names {
        let id = w.push_bare("cy", name(cy, n));
        assert_eq!(w.verdict(id), Verdict::Denied, "{n:?}");
    }
    let id = w.push_bare("cy", name(cy, &"y".repeat(65)));
    assert_eq!(w.verdict(id), Verdict::Denied, "65 characters");
    let id = w.push_bare("cy", name(cy, &"\u{1F41D}".repeat(65)));
    assert_eq!(w.verdict(id), Verdict::Denied, "over 256 bytes");
    let good = Avatar::new("image/png", &png()).unwrap();
    let svg = Avatar { content_type: "image/svg+xml".into(), ..good.clone() };
    let mut big = png();
    big.resize(MAX_AVATAR_BYTES + 1024, 0);
    let big = Avatar { data: base64_of(&big), sha256: sha_of(&big), ..good.clone() };
    let jpeg_label = Avatar { content_type: "image/jpeg".into(), ..good.clone() };
    let wrong_sha = Avatar { sha256: "00".repeat(32), ..good.clone() };
    for a in [svg, big, jpeg_label, wrong_sha] {
        let id = w.push_bare("cy", picture(cy, a.clone()));
        assert_eq!(w.verdict(id), Verdict::Denied, "{}", a.content_type);
    }
    assert!(!w.config().profiles.contains_key(&cy), "nothing bad folded");
    let id = w.push_bare("cy", label(w.people["cy"].device_id, cy, &"d".repeat(49)));
    assert_eq!(w.verdict(id), Verdict::Denied, "49-character label");
}

fn base64_of(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn sha_of(b: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn a_box_sets_its_own_profile_and_a_non_member_nothing() {
    let mut w = SignedWorld::genesis(&["ana", "sff", "eve"], None);
    w.kinds.insert("sff".into(), ActorKind::Service);
    w.admit("ana", "sff", WorkspaceRole::Contributor);
    let sff = w.people["sff"].account_id;
    let id = w.push_bare("sff", name(sff, "Home box"));
    assert_eq!(w.verdict(id), Verdict::Allowed);
    w.publish_identity("eve");
    let eve = w.people["eve"].account_id;
    let id = w.push_bare("eve", name(eve, "Eve"));
    assert_eq!(w.verdict(id), Verdict::Denied, "not a member");
    let id = w.push_bare("eve", label(w.people["eve"].device_id, eve, "eve's"));
    assert_eq!(w.verdict(id), Verdict::Denied, "not a member");
}

/// L1: a member who certified someone else's device key for themselves can
/// label it, but only under their own account: their label never shows next
/// to the device's real person.
#[test]
fn a_foreign_certifier_labels_only_under_their_own_name() {
    let mut w = world();
    let (ana, ben) = (w.people["ana"].account_id, w.people["ben"].account_id);
    let desk = w.people["ana"].device_id;
    let cert = DeviceCertificate::issue(&w.people["ben"].account, ben, &w.people["ana"].device.public_key_bytes(), Timestamp::epoch());
    w.push("ben", SessionEvent::DeviceCertificateAdded { certificate: cert });
    let mine = w.push_bare("ana", label(desk, ana, "Ana's MacBook Air"));
    let his = w.push_bare("ben", label(desk, ben, "Ana's phone"));
    let forged = w.push_bare("ben", label(desk, ana, "Ana's phone"));
    assert_eq!(w.verdict(mine), Verdict::Allowed);
    assert_eq!(w.verdict(forged), Verdict::Denied, "under someone else's account");
    let c = w.config();
    assert_eq!(c.device_labels[&desk][&ana], "Ana's MacBook Air");
    if w.verdict(his) == Verdict::Allowed {
        assert_eq!(c.device_labels[&desk][&ben], "Ana's phone", "kept apart, under Ben");
    }
    // Clearing removes only that pair.
    w.push_bare("ana", label(desk, ana, ""));
    assert!(w.config().device_labels.get(&desk).is_none_or(|m| !m.contains_key(&ana)));
}

/// A revoked device writes neither.
#[test]
fn a_revoked_device_writes_no_profile() {
    let mut w = world();
    let ana = w.people["ana"].account_id;
    let lt = linked_device(&mut w, "ana");
    w.push("ana", SessionEvent::DeviceRevoked { device_id: lt.id, watermark: Vec::new() });
    let id = from_device(&mut w, "ana", &lt, label(lt.id, ana, "after revocation"));
    assert_eq!(w.verdict(id), Verdict::Denied);
}

/// P1: last writer wins per field in the fold's canonical order, whatever
/// order the events arrived in.
#[test]
fn every_arrival_order_folds_the_same_profile() {
    let mut w = world();
    let ana = w.people["ana"].account_id;
    w.push_bare("ana", name(ana, "Ana One"));
    w.push_bare("ana", picture(ana, Avatar::new("image/png", &png()).unwrap()));
    w.push_bare("ana", name(ana, "Ana Two"));
    w.push_bare("ana", label(w.people["ana"].device_id, ana, "Desk"));
    w.push_bare("ana", name(ana, "Ana Three"));
    let expect = w.config();
    assert_eq!(expect.profiles[&ana].display_name.as_deref(), Some("Ana Three"));
    let n = w.cfg.len();
    // Rotate and reverse the tail (the base seed stays first in canonical
    // order whatever the arrival order).
    for k in 1..6 {
        let mut cfg = w.cfg.clone();
        cfg[n - k..].reverse();
        cfg.rotate_left(k % n);
        let got = ligus_core::events::project_config(&cfg, None, None).0.unwrap();
        assert_eq!(got.profiles, expect.profiles, "order {k}");
        assert_eq!(got.device_labels, expect.device_labels, "order {k}");
    }
}

/// Workspace model v3, 17.6: a phone names itself from rules 4, and only
/// itself, under its own person. On rules 3 its label is denied, as a 0.4
/// fold denies it (no rules-3 verdict changes).
#[test]
fn a_phone_names_itself_only_from_rules_4() {
    let mut w = world();
    let (ana, ben) = (w.people["ana"].account_id, w.people["ben"].account_id);
    let phone = Phone::new();
    let cert = w.phone_certificate("ana", &phone);
    w.push("ana", SessionEvent::DeviceCertificateAdded { certificate: cert });
    w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    let on3 = w.phone_write("ana", &phone, None, label(phone.device_id, ana, "Ana's iPhone"));
    assert_eq!(w.verdict(on3), Verdict::Denied, "rules 3 folds a phone's own label as 0.4 does");
    assert!(!w.config().device_labels.contains_key(&phone.device_id));

    w.push("ana", SessionEvent::WorkspaceRulesSet { version: ligus_core::authz::profiles::PHONE_NAMES_ITSELF_FROM, presence: None });
    let own = w.phone_write("ana", &phone, None, label(phone.device_id, ana, "Ana's iPhone"));
    assert_eq!(w.verdict(own), Verdict::Allowed, "rules 4: the phone names itself");
    assert_eq!(w.config().device_labels[&phone.device_id][&ana], "Ana's iPhone");
    // Never another device, not even its person's desktop.
    let desk = w.people["ana"].device_id;
    let other = w.phone_write("ana", &phone, None, label(desk, ana, "renamed by the phone"));
    assert_eq!(w.verdict(other), Verdict::Denied);
    // Never under another account.
    let foreign = w.phone_write("ana", &phone, None, label(phone.device_id, ben, "Ben's now"));
    assert_eq!(w.verdict(foreign), Verdict::Denied);
    // The content checks still hold.
    let long = w.phone_write("ana", &phone, None, label(phone.device_id, ana, &"p".repeat(49)));
    assert_eq!(w.verdict(long), Verdict::Denied);
    // And the desktop still renames it; the newest wins.
    w.push_bare("ana", label(phone.device_id, ana, "Work phone"));
    assert_eq!(w.config().device_labels[&phone.device_id][&ana], "Work phone");
    // Still nothing about its person's profile.
    let p = w.phone_write("ana", &phone, None, name(ana, "From Phone"));
    assert_eq!(w.verdict(p), Verdict::Denied);
}
