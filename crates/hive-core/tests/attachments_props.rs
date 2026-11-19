//! File-attachment properties (issue #118). 256 cases by default;
//! `PROPTEST_CASES=4096` in release for the gate.
//!
//! A random history mixes policy changes (by anyone, with or without
//! presence), blocks and unblocks, shares by members of every role and by a
//! non-member, deletions, device revocations and removals. Whatever happens:
//!
//! 1. the fold is a function of the event set: any arrival order folds to the
//!    same attachments;
//! 2. nobody outside the workspace ever shares, and a Viewer only when the
//!    policy in force lets Viewers;
//! 3. an allowed share was allowed by the policy and the blocklist at its
//!    author's position (for an author no cut reaches);
//! 4. one attachment id is one file; a deletion is its sharer's or an
//!    Admin's;
//! 5. only an Admin with presence changes the policy or lifts a block;
//! 6. a revoked device's later shares never fold;
//! 7. an allowed share's id is bound to the device that signed it.
//!
//! Plus the pure checks: a cleaned name is always one safe component and
//! cleaning is idempotent; type detection, assessment and zip inspection
//! never panic on arbitrary bytes.

use std::collections::{BTreeMap, BTreeSet};

use hive_core::attachment::{self, AttachmentPolicy, AttachmentRef};
use hive_core::testkit::{Proof, SignedWorld};
use hive_core::watermark::heads_of;
use hive_core::{workspace_config_session_id, SessionEvent, SessionEventEnvelope, Verdict, WorkspaceRole};
use proptest::prelude::*;
use uuid::Uuid;

const WHO: [&str; 5] = ["owner", "adm", "mara", "vic", "eve"];
const NAMES: [&str; 6] = ["a.pdf", "b.png", "tool.exe", "run.sh", "notes.txt", "x.zip"];
const HASHES: [&str; 3] = [
    "1111111111111111111111111111111111111111111111111111111111111111",
    "2222222222222222222222222222222222222222222222222222222222222222",
    "3333333333333333333333333333333333333333333333333333333333333333",
];

fn role(n: u8) -> WorkspaceRole {
    [WorkspaceRole::Viewer, WorkspaceRole::Contributor, WorkspaceRole::Admin][n as usize % 3]
}

#[derive(Debug, Clone)]
enum Op {
    Policy { by: u8, enabled: bool, min_role: u8, max_kb: u16, block_exe: bool, presence: bool },
    Block { by: u8, hash: u8 },
    Unblock { by: u8, hash: u8, presence: bool },
    Share { by: u8, chat: u8, name: u8, hash: u8, kb: u16, slot: u8 },
    /// `by` deletes the file `of` shared in `slot`.
    Delete { by: u8, chat: u8, slot: u8, of: u8 },
    Revoke { who: u8 },
    Remove { who: u8 },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..5, any::<bool>(), 0u8..3, 1u16..2048, any::<bool>(), any::<bool>())
            .prop_map(|(by, enabled, min_role, max_kb, block_exe, presence)| Op::Policy { by, enabled, min_role, max_kb, block_exe, presence }),
        (0u8..5, 0u8..3).prop_map(|(by, hash)| Op::Block { by, hash }),
        (0u8..5, 0u8..3, any::<bool>()).prop_map(|(by, hash, presence)| Op::Unblock { by, hash, presence }),
        (0u8..5, 0u8..2, 0u8..6, 0u8..3, 1u16..2048, 0u8..6).prop_map(|(by, chat, name, hash, kb, slot)| Op::Share { by, chat, name, hash, kb, slot }),
        (0u8..5, 0u8..2, 0u8..6, 0u8..5).prop_map(|(by, chat, slot, of)| Op::Delete { by, chat, slot, of }),
        (2u8..4).prop_map(|who| Op::Revoke { who }),
        (2u8..4).prop_map(|who| Op::Remove { who }),
    ]
}

fn honest_watermark(w: &SignedWorld, who: &str) -> Vec<hive_core::watermark::Watermark> {
    let signers: BTreeSet<Uuid> = [w.people[who].device_id].into_iter().collect();
    let mut out = heads_of(&w.cfg, workspace_config_session_id(w.wid), &signers);
    for (id, log) in &w.chats {
        out.extend(heads_of(log, *id, &signers));
    }
    out
}

struct Run {
    w: SignedWorld,
    chats: [Uuid; 2],
    /// (event id, chat, author, attachment) of every share written.
    shares: Vec<(Uuid, Uuid, String, AttachmentRef)>,
    /// (event id, chat, author, attachment id) of every deletion written.
    deletes: Vec<(Uuid, Uuid, String, Uuid)>,
    /// (event id, author, presence) of every policy change and unblock.
    governance: Vec<(Uuid, String, bool)>,
    /// How many shares were written when each device was revoked.
    revoked: BTreeMap<Uuid, usize>,
    /// Members a cut reached (removed or revoked).
    cut: BTreeSet<String>,
}

fn run(ops: &[Op]) -> Run {
    let mut w = SignedWorld::genesis(&WHO, None);
    w.admit("owner", "adm", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.admit("owner", "vic", WorkspaceRole::Viewer);
    let chats = [w.chat("owner", "one"), w.chat("adm", "two")];
    let mut r = Run { w, chats, shares: vec![], deletes: vec![], governance: vec![], revoked: BTreeMap::new(), cut: BTreeSet::new() };
    for o in ops {
        match *o {
            Op::Policy { by, enabled, min_role, max_kb, block_exe, presence } => {
                let policy = AttachmentPolicy {
                    enabled,
                    min_role: role(min_role),
                    max_bytes: max_kb as u64 * 1024,
                    blocked_types: if block_exe { vec!["@executable".into()] } else { vec![] },
                    ..AttachmentPolicy::default()
                };
                let by = WHO[by as usize];
                let proof = if presence { Proof::Recovery } else { Proof::None };
                let id = r.w.write(by, None, SessionEvent::AttachmentPolicySet { policy }, None, proof);
                r.governance.push((id, by.into(), presence));
            }
            Op::Block { by, hash } => {
                r.w.push_bare(WHO[by as usize], SessionEvent::AttachmentHashBlocked { sha256: HASHES[hash as usize].into() });
            }
            Op::Unblock { by, hash, presence } => {
                let by = WHO[by as usize];
                let proof = if presence { Proof::Recovery } else { Proof::None };
                let id = r.w.write(by, None, SessionEvent::AttachmentHashUnblocked { sha256: HASHES[hash as usize].into() }, None, proof);
                r.governance.push((id, by.into(), presence));
            }
            Op::Share { by, chat, name, hash, kb, slot } => {
                let by = WHO[by as usize];
                let blob_id = format!("{:0>43}", slot);
                let a = AttachmentRef {
                    // Bound to the sharer's device (a second share of a
                    // slot by one device reuses the id and is refused).
                    id: attachment::share_id(r.w.people[by].device_id, &blob_id),
                    blob_id,
                    sha256: HASHES[hash as usize].into(),
                    size: kb as u64 * 1024,
                    content_type: "application/octet-stream".into(),
                    name: NAMES[name as usize].into(),
                    message_id: None,
                    proposal_id: None,
                };
                let c = r.chats[chat as usize];
                let id = r.w.say(c, by, SessionEvent::AttachmentShared { attachment: a.clone() });
                r.shares.push((id, c, by.into(), a));
            }
            Op::Delete { by, chat, slot, of } => {
                let by = WHO[by as usize];
                let c = r.chats[chat as usize];
                let target = attachment::share_id(r.w.people[WHO[of as usize]].device_id, &format!("{:0>43}", slot));
                let id = r.w.say(c, by, SessionEvent::AttachmentDeleted { attachment_id: target });
                r.deletes.push((id, c, by.into(), target));
            }
            Op::Revoke { who } => {
                let who = WHO[who as usize];
                let d = r.w.people[who].device_id;
                if r.revoked.contains_key(&d) {
                    continue;
                }
                let wm = honest_watermark(&r.w, who);
                r.w.push("owner", SessionEvent::DeviceRevoked { device_id: d, watermark: wm });
                r.revoked.insert(d, r.shares.len());
                r.cut.insert(who.into());
            }
            Op::Remove { who } => {
                let who = WHO[who as usize];
                let wm = honest_watermark(&r.w, who);
                r.w.push("owner", SessionEvent::MemberRemoved { member_id: who.into(), watermark: wm });
                r.cut.insert(who.into());
            }
        }
    }
    r
}

fn attachments_of(w: &SignedWorld, chat: Uuid) -> BTreeMap<Uuid, (String, String, bool)> {
    w.state(chat).attachments.into_iter().map(|(id, a)| (id, (a.author, a.attachment.sha256, a.deleted))).collect()
}

fn frontier_of(env: &SessionEventEnvelope) -> hive_core::CfgFrontier {
    env.actor_stamp.as_ref().and_then(|s| s.cfg_frontier).unwrap()
}

proptest! {
    #[test]
    fn attachment_fold_rules_hold(ops in prop::collection::vec(op(), 1..28), seed in any::<u64>()) {
        let r = run(&ops);
        let w = &r.w;
        let v = w.verdicts();
        let t = w.timeline();

        // 1. Any arrival order folds to the same attachments.
        let mut shuffled = w.clone();
        let mut s = seed | 1;
        let mut shuffle = |log: &mut Vec<SessionEventEnvelope>| {
            for i in (1..log.len()).rev() {
                s ^= s << 13; s ^= s >> 7; s ^= s << 17;
                log.swap(i, (s % (i as u64 + 1)) as usize);
            }
        };
        shuffle(&mut shuffled.cfg);
        for log in shuffled.chats.values_mut() { shuffle(log); }
        for c in r.chats {
            prop_assert_eq!(attachments_of(w, c), attachments_of(&shuffled, c));
        }

        for (i, (id, chat, author, a)) in r.shares.iter().enumerate() {
            if v[id] != Verdict::Allowed { continue; }
            // 2. Never an outsider.
            prop_assert_ne!(author.as_str(), "eve");
            let env = w.chats[chat].iter().find(|e| e.event_id == *id).unwrap();
            // 7. Its id is bound to the device that signed it (no squatting).
            prop_assert_eq!(a.id, attachment::share_id(env.signer_device_id.unwrap(), &a.blob_id));
            // 6. A revoked device's shares after its revocation never fold.
            if let Some(n) = r.revoked.get(&env.signer_device_id.unwrap()) {
                prop_assert!(i < *n, "a revoked device's later share folded");
            }
            // 3. The policy and blocklist at its position allowed it.
            if !r.cut.contains(author) {
                let at = t.at(frontier_of(env)).unwrap();
                let p = &at.attachments.policy;
                prop_assert!(p.enabled);
                prop_assert!(!at.attachments.blocked.contains(&a.sha256));
                prop_assert!(a.size <= p.max_bytes);
                let role = at.role_of(author).unwrap();
                prop_assert!(role.rank() >= p.min_role.rank());
                prop_assert!(p.file_refusal(&attachment::type_tokens(&a.name, None), a.size).is_none());
            }
        }
        // 4. One id, one file: the folded file is the first allowed share of its id.
        for c in r.chats {
            let state = w.state(c);
            let mut first: BTreeMap<Uuid, &AttachmentRef> = BTreeMap::new();
            let mut allowed: Vec<&(Uuid, Uuid, String, AttachmentRef)> = r.shares.iter().filter(|s| s.1 == c && v[&s.0] == Verdict::Allowed).collect();
            allowed.sort_by_key(|s| w.chats[&c].iter().find(|e| e.event_id == s.0).unwrap().canonical_key());
            for s in &allowed {
                prop_assert!(first.insert(s.3.id, &s.3).is_none(), "two allowed shares of one id");
            }
            prop_assert_eq!(first.len(), state.attachments.len());
            for (id, a) in &state.attachments {
                prop_assert_eq!(&a.attachment, first[id]);
            }
        }
        for (id, chat, author, aid) in &r.deletes {
            if v[id] != Verdict::Allowed { continue; }
            let sharer = r.shares.iter().find(|s| s.1 == *chat && s.3.id == *aid && v[&s.0] == Verdict::Allowed).map(|s| s.2.clone());
            prop_assert!(sharer.is_some());
            prop_assert!(sharer.as_deref() == Some(author.as_str()) || author == "owner" || author == "adm");
        }
        // 5. Only an Admin (or the Owner) with presence changes the policy or lifts a block.
        for (id, author, presence) in &r.governance {
            if v[id] == Verdict::Allowed {
                prop_assert!(*presence && (author == "owner" || author == "adm"));
            }
        }
    }

    #[test]
    fn cleaned_names_are_one_safe_component(raw in "\\PC{0,300}", noise in prop::collection::vec(prop::sample::select(vec!['/', '\\', '.', '\u{202E}', '\u{200B}', '\0', '\n', ':', ' ', '\u{2066}']), 0..12)) {
        let mut s = raw.clone();
        for (i, c) in noise.iter().enumerate() {
            let at = (i * 7) % (s.chars().count() + 1);
            let byte = s.char_indices().nth(at).map(|(b, _)| b).unwrap_or(s.len());
            s.insert(byte, *c);
        }
        let clean = attachment::sanitize_filename(&s);
        prop_assert!(!clean.is_empty() && clean.len() <= attachment::MAX_NAME_BYTES);
        prop_assert!(!clean.contains('/') && !clean.contains('\\') && !clean.contains(':'));
        prop_assert!(!clean.chars().any(|c| c.is_control()));
        let hidden = clean.chars().any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200F}'));
        prop_assert!(!hidden, "a bidi or zero-width character survived");
        prop_assert!(!clean.starts_with('.') && !clean.ends_with('.') && !clean.ends_with(' '));
        prop_assert_eq!(attachment::sanitize_filename(&clean), clean.clone(), "cleaning is idempotent");
        let stem = clean.split('.').next().unwrap().to_lowercase();
        prop_assert!(!["con", "prn", "aux", "nul", "com1", "lpt1"].contains(&stem.as_str()));
    }

    #[test]
    fn detection_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..4096), prefix in prop::sample::select(vec![&b""[..], b"PK\x03\x04", b"PK\x05\x06", b"MZ", b"\x7fELF", b"\xca\xfe\xba\xbe", b"<svg", b"#!", b"\x1f\x8b"])) {
        let mut b = prefix.to_vec();
        b.extend_from_slice(&bytes);
        let a = attachment::assess("f.bin", &b);
        let _ = attachment::inspect_zip(&b, &attachment::ArchiveLimits::default());
        // An executable is always risky whatever it is named.
        if a.detected.class == attachment::TypeClass::Executable {
            prop_assert!(a.risky);
        }
        // Only inert raster images and plain text are ever previewable.
        if a.preview.is_some() {
            prop_assert!(matches!(a.detected.ext, "png" | "jpeg" | "gif" | "webp" | "bmp" | "txt"));
        }
    }
}
