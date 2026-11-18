//! Phase B service-member properties (run 256 cases by default,
//! `PROPTEST_CASES=4096` in release for the gate).
//!
//! A random history mixes honest governance by people with whatever a
//! service box tries: cuts, admissions, role raises, presence, votes, and
//! redemptions of box admissions with random kinds, roles, times and bound
//! accounts. Whatever the order:
//!
//! 1. no event a service member signs that cuts or admits anyone, carries
//!    presence, registers a passkey or decides a set is ever allowed;
//! 2. no service member ever holds a role above Contributor;
//! 3. a proposal has a human approval only if a human's own vote was allowed;
//! 4. every allowed redemption of a box admission admits a service at most
//!    Contributor, on time, by the account it names; each admits one member;
//! 5. once a box's device is revoked, nothing it signs later is allowed;
//! 6. (review) only a holder of the admission code redeems it, whatever its
//!    lamport: a member who reads the invite id off the log never does;
//! 7. (review) once an Admin closes an admission, no redemption but the one
//!    it names is allowed, wherever it sorts (a backdated one included);
//! 8. (review) a box never re-adds itself as a person.

use std::collections::{BTreeMap, BTreeSet};

use hive_core::admission::{admission_of, Admission, RedeemKey};
use hive_core::authorization::invite_subject;
use hive_core::testkit::{Proof, SignedWorld};
use hive_core::{
    ActionProposal, ActorKind, ChatMessage, MemberRoleChange, MessageRole, ProposalApproval, SessionEvent, SessionEventEnvelope,
    Timestamp, Verdict, WorkspaceRole,
};
use proptest::prelude::*;
use uuid::Uuid;

const HOUR_MS: i64 = 60 * 60 * 1000;
const PEOPLE: [&str; 3] = ["owner", "adm", "mara"];
const BOXES: [&str; 2] = ["box", "box2"];
const ALL: [&str; 6] = ["owner", "adm", "mara", "box", "box2", "eve"];

/// The redeem key of every admission issued here (the code's holder).
fn key() -> RedeemKey {
    RedeemKey::from_seed(&[7; 32]).unwrap()
}

/// A key someone without the code makes up.
fn forged() -> RedeemKey {
    RedeemKey::from_seed(&[9; 32]).unwrap()
}

fn now_ms() -> i64 {
    (Timestamp::now().inner().unix_timestamp_nanos() / 1_000_000) as i64
}

fn role(n: u8) -> WorkspaceRole {
    [WorkspaceRole::Owner, WorkspaceRole::Admin, WorkspaceRole::Contributor, WorkspaceRole::Viewer][n as usize % 4]
}

#[derive(Debug, Clone)]
enum Op {
    /// A person issues a box admission (slot, ttl in minutes, bound to a box?).
    Issue { by: u8, slot: u8, ttl_min: i16, bind: Option<u8>, presence: bool },
    /// A box (or a person) redeems slot's invite at a role, `late_min`
    /// minutes from now, as its own kind or claiming another; holding the
    /// code or not; backdated to sort right after the issue or not.
    Redeem { who: u8, slot: u8, role: u8, late_min: i16, holds: bool, back: bool },
    /// A person closes slot's admission, naming the redemption allowed so
    /// far (or none).
    Close { by: u8, slot: u8, name: bool },
    /// A box re-adds itself as a person.
    Flip { boxi: u8 },
    /// A box tries a cut or an admission against someone.
    BoxGoverns { boxi: u8, kind: u8, target: u8, presence: bool },
    /// The owner tries to raise a box's role.
    Raise { boxi: u8, to: u8 },
    /// A box or a person votes on the chat's proposal.
    Vote { who: u8 },
    /// A box posts in the chat, with or without stamped presence.
    Post { boxi: u8, presence: bool },
    /// An Admin revokes a box's device.
    Revoke { boxi: u8 },
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..3, 0u8..3, -30i16..600, prop::option::of(0u8..2), any::<bool>())
            .prop_map(|(by, slot, ttl_min, bind, presence)| Op::Issue { by, slot, ttl_min, bind, presence }),
        (0u8..6, 0u8..3, 0u8..4, -60i16..600, any::<bool>(), any::<bool>())
            .prop_map(|(who, slot, role, late_min, holds, back)| Op::Redeem { who, slot, role, late_min, holds, back }),
        (0u8..3, 0u8..3, any::<bool>()).prop_map(|(by, slot, name)| Op::Close { by, slot, name }),
        (0u8..2).prop_map(|boxi| Op::Flip { boxi }),
        (0u8..2, 0u8..6, 0u8..6, any::<bool>()).prop_map(|(boxi, kind, target, presence)| Op::BoxGoverns { boxi, kind, target, presence }),
        (0u8..2, 0u8..4).prop_map(|(boxi, to)| Op::Raise { boxi, to }),
        (0u8..6).prop_map(|who| Op::Vote { who }),
        (0u8..2, any::<bool>()).prop_map(|(boxi, presence)| Op::Post { boxi, presence }),
        (0u8..2).prop_map(|boxi| Op::Revoke { boxi }),
    ]
}

/// The honest watermark of `who`'s device on every log of `w`.
fn honest_watermark(w: &SignedWorld, who: &str) -> Vec<hive_core::watermark::Watermark> {
    let signers: std::collections::BTreeSet<Uuid> = [w.people[who].device_id].into_iter().collect();
    let mut out = hive_core::watermark::heads_of(&w.cfg, hive_core::workspace_config_session_id(w.wid), &signers);
    for (id, log) in &w.chats {
        out.extend(hive_core::watermark::heads_of(log, *id, &signers));
    }
    out
}

struct Run {
    w: SignedWorld,
    chat: Uuid,
    proposal: Uuid,
    slots: [Uuid; 3],
    /// Redemptions made by a holder of the code.
    held: BTreeSet<Uuid>,
}

fn setup() -> Run {
    let mut w = SignedWorld::genesis(&ALL, None);
    w.admit("owner", "adm", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    for b in BOXES {
        w.kinds.insert(b.into(), ActorKind::Service);
        w.publish_identity(b);
    }
    w.publish_identity("eve");
    let chat = w.chat("owner", "c");
    let p = ActionProposal::new("q", hive_core::ProposalKind::Decision, "owner");
    let proposal = p.id;
    w.say(chat, "owner", SessionEvent::ProposalUpserted { proposal: p });
    Run { w, chat, proposal, slots: [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()], held: BTreeSet::new() }
}

fn config_write(w: &mut SignedWorld, by: &str, e: SessionEvent, presence: bool, when: Option<i64>) -> Uuid {
    config_write_at(w, by, e, presence, when, None)
}

/// [`config_write`] at `lamport` (a backdated position) when given.
fn config_write_at(w: &mut SignedWorld, by: &str, e: SessionEvent, presence: bool, when: Option<i64>, lamport: Option<u64>) -> Uuid {
    let mut env = w.draft(by, None, e, None);
    if let Some(ms) = when {
        env.timestamp = Timestamp::from_unix_seconds(ms.div_euclid(1000));
    }
    if let Some(l) = lamport {
        env.lamport = l;
    }
    let p = presence.then(|| w.recovery_presence(by, &env));
    w.finish(&mut env, by, p);
    w.push_env(None, env)
}

fn play(r: &mut Run, ops: &[Op]) {
    let now = now_ms();
    for op in ops {
        match op.clone() {
            Op::Issue { by, slot, ttl_min, bind, presence } => {
                let by = PEOPLE[by as usize % 3];
                let account = bind.map(|b| r.w.people[BOXES[b as usize % 2]].account_id);
                let e = SessionEvent::JoinApproved {
                    subject: invite_subject(r.slots[slot as usize % 3]),
                    admission: Some(Admission::service(now + ttl_min as i64 * 60_000, account, key().public())),
                };
                config_write(&mut r.w, by, e, presence, None);
            }
            Op::Redeem { who, slot, role: n, late_min, holds, back } => {
                let who = ALL[who as usize % ALL.len()];
                let m = r.w.member_of(who, role(n));
                let id = r.slots[slot as usize % 3];
                let k = if holds { key() } else { forged() };
                let proof = k.prove(r.w.wid, id, who, r.w.people[who].device_id);
                let e = SessionEvent::MemberAdded { member: m, invite: Some(id), proof: Some(proof) };
                // Backdated: right after the newest issue of this invite.
                let lamport = back
                    .then(|| {
                        r.w.cfg
                            .iter()
                            .filter(|e| matches!(&e.payload, SessionEvent::JoinApproved { subject, .. } if *subject == invite_subject(id)))
                            .map(|e| e.lamport + 1)
                            .max()
                    })
                    .flatten();
                let ev = config_write_at(&mut r.w, who, e, false, Some(now + late_min as i64 * 60_000), lamport);
                if holds {
                    r.held.insert(ev);
                }
            }
            Op::Close { by, slot, name } => {
                let by = PEOPLE[by as usize % 3];
                let id = r.slots[slot as usize % 3];
                let v = r.w.verdicts();
                let redeemed = name
                    .then(|| {
                        r.w.cfg
                            .iter()
                            .find(|e| {
                                v.get(&e.event_id) == Some(&Verdict::Allowed)
                                    && matches!(&e.payload, SessionEvent::MemberAdded { invite: Some(i), .. } if *i == id)
                            })
                            .map(|e| e.event_id)
                    })
                    .flatten();
                config_write(&mut r.w, by, SessionEvent::AdmissionClosed { invite: id, redeemed }, false, None);
            }
            Op::Flip { boxi } => {
                let b = BOXES[boxi as usize % 2];
                let mut m = r.w.member_of(b, WorkspaceRole::Contributor);
                m.actor.kind = ActorKind::Human;
                config_write(&mut r.w, b, SessionEvent::MemberAdded { member: m, invite: None, proof: None }, false, None);
            }
            Op::BoxGoverns { boxi, kind, target, presence } => {
                let b = BOXES[boxi as usize % 2];
                let t = ALL[target as usize % ALL.len()];
                let e = match kind % 6 {
                    0 => SessionEvent::MemberRemoved { member_id: t.into(), watermark: vec![] },
                    1 => SessionEvent::MemberRoleChanged {
                        change: MemberRoleChange { member_id: t.into(), old_role: WorkspaceRole::Contributor, new_role: WorkspaceRole::Viewer },
                        watermark: vec![],
                    },
                    2 => SessionEvent::DeviceRevoked { device_id: r.w.people[t].device_id, watermark: vec![] },
                    3 => SessionEvent::MemberAdded { member: r.w.member_of(t, WorkspaceRole::Viewer), invite: None, proof: None },
                    4 => SessionEvent::JoinApproved { subject: invite_subject(Uuid::new_v4()), admission: None },
                    _ => SessionEvent::JoinApproved {
                        subject: invite_subject(Uuid::new_v4()),
                        admission: Some(Admission::service(now + HOUR_MS, None, key().public())),
                    },
                };
                config_write(&mut r.w, b, e, presence, None);
            }
            Op::Raise { boxi, to } => {
                let b = BOXES[boxi as usize % 2];
                let e = SessionEvent::MemberRoleChanged {
                    change: MemberRoleChange { member_id: b.into(), old_role: WorkspaceRole::Contributor, new_role: role(to) },
                    watermark: vec![],
                };
                config_write(&mut r.w, "owner", e, true, None);
            }
            Op::Vote { who } => {
                let who = ALL[who as usize % ALL.len()];
                let st = r.w.state(r.chat);
                let d = st.proposals.iter().find(|p| p.id == r.proposal).map(hive_core::authz::votes::diff_digest);
                let e = SessionEvent::ProposalVoteCast {
                    proposal_id: r.proposal,
                    approval: ProposalApproval { actor_id: who.into(), role: WorkspaceRole::Contributor, approved: true, created_at: Timestamp::epoch(), diff_digest: d },
                };
                r.w.say(r.chat, who, e);
            }
            Op::Post { boxi, presence } => {
                let b = BOXES[boxi as usize % 2];
                let e = SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, b, "hi") };
                r.w.write(b, Some(r.chat), e, None, if presence { Proof::Recovery } else { Proof::None });
            }
            Op::Revoke { boxi } => {
                let b = BOXES[boxi as usize % 2];
                let dev = r.w.people[b].device_id;
                let watermark = honest_watermark(&r.w, b);
                config_write(&mut r.w, "adm", SessionEvent::DeviceRevoked { device_id: dev, watermark }, true, None);
            }
        }
    }
}

fn all_envs(w: &SignedWorld) -> Vec<&SessionEventEnvelope> {
    w.cfg.iter().chain(w.chats.values().flatten()).collect()
}

fn signer_name(w: &SignedWorld, env: &SessionEventEnvelope) -> Option<&'static str> {
    ALL.into_iter().find(|n| env.signer_device_id == Some(w.people[*n].device_id))
}

fn check(r: &Run) -> Result<(), String> {
    let w = &r.w;
    let v = w.verdicts();
    let allowed = |e: &SessionEventEnvelope| v.get(&e.event_id) == Some(&Verdict::Allowed);
    let t = w.timeline();
    let roster = t.current();
    let config = w.config();

    // 1. A service member's allowed events never govern anyone.
    for env in all_envs(w).into_iter().filter(|e| allowed(e)) {
        let Some(name) = signer_name(w, env) else { continue };
        if !BOXES.contains(&name) {
            continue;
        }
        let own_account = w.people[name].account_id;
        let bad = match &env.payload {
            SessionEvent::MemberRemoved { member_id, .. } => member_id != name,
            SessionEvent::MemberRoleChanged { change, .. } => change.member_id != name,
            SessionEvent::DeviceRevoked { device_id, .. } => roster.devices.get(device_id).is_none_or(|d| !d.accounts.contains(&own_account)),
            SessionEvent::MemberAdded { member, .. } => member.id != name,
            SessionEvent::JoinApproved { .. } | SessionEvent::PasskeyRegistered { .. } | SessionEvent::ProposalSetDecided { .. } => true,
            _ => false,
        };
        if bad {
            return Err(format!("{name}'s {} was allowed", env.payload.kind_str()));
        }
        if env.actor_stamp.as_ref().is_some_and(|s| s.presence.is_some()) {
            return Err(format!("{name}'s {} carried presence and was allowed", env.payload.kind_str()));
        }
    }

    // 2. No service member above Contributor, ever.
    for env in &w.cfg {
        let Ok(at) = t.at(hive_core::CfgFrontier { lamport: env.lamport, event_id: env.event_id }) else { continue };
        for (id, m) in &at.members {
            if m.kind == ActorKind::Service && m.role.rank() > WorkspaceRole::Contributor.rank() {
                return Err(format!("service member {id} holds {:?}", m.role));
            }
        }
    }

    // 3. A human approval comes only from an allowed human vote.
    let state = w.state(r.chat);
    if hive_core::guard::has_human_approval(&state, r.proposal) {
        let human_vote = w.chats[&r.chat].iter().filter(|e| allowed(e)).any(|e| {
            matches!(&e.payload, SessionEvent::ProposalVoteCast { approval, .. } if approval.approved
                && PEOPLE.contains(&approval.actor_id.as_str()))
        });
        if !human_vote {
            return Err("a human gate was satisfied without a human's own vote".into());
        }
    }

    // 4. Allowed redemptions of box admissions follow their terms; one each.
    let mut redeemed: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
    for env in w.cfg.iter().filter(|e| allowed(e)) {
        let SessionEvent::MemberAdded { member, invite: Some(id), .. } = &env.payload else { continue };
        let Some(terms) = admission_of(&config, *id) else {
            if member.actor.kind == ActorKind::Service && member.id == member.actor.id {
                return Err(format!("{} joined as a service through a person's invite", member.id));
            }
            continue;
        };
        let at = hive_core::admission::envelope_ms(env);
        if member.actor.kind != ActorKind::Service
            || member.role.rank() > WorkspaceRole::Contributor.rank()
            || at > terms.expires_ms
            || terms.account_id.is_some_and(|a| member.actor.account_id != Some(a))
        {
            return Err(format!("{} redeemed {id} outside its terms", member.id));
        }
        redeemed.entry(*id).or_default().insert(member.id.clone());
    }
    if let Some((id, who)) = redeemed.iter().find(|(_, who)| who.len() > 1) {
        return Err(format!("admission {id} admitted {who:?}"));
    }

    // 6. Only the code's holder redeems an admission.
    // 7. A closed admission admits only the redemption every closure names.
    for env in w.cfg.iter().filter(|e| allowed(e)) {
        let SessionEvent::MemberAdded { member, invite: Some(id), .. } = &env.payload else { continue };
        if admission_of(&config, *id).is_none() || member.id != env.actor_stamp.as_ref().map(|s| s.actor.id.as_str()).unwrap_or("") {
            continue;
        }
        if !r.held.contains(&env.event_id) {
            return Err(format!("{} redeemed {id} without its code", member.id));
        }
        for c in w.cfg.iter().filter(|e| allowed(e)) {
            if let SessionEvent::AdmissionClosed { invite, redeemed } = &c.payload {
                if invite == id && *redeemed != Some(env.event_id) {
                    return Err(format!("{}'s redemption of {id} stands against a closure naming {redeemed:?}", member.id));
                }
            }
        }
    }

    // 8. A box that joined as a service stays one.
    for b in BOXES {
        if let Some(m) = roster.members.get(b) {
            if m.kind != ActorKind::Service {
                return Err(format!("{b} became a {:?}", m.kind));
            }
        }
    }

    // 5. Nothing a revoked box signs after its revocation is allowed.
    for b in BOXES {
        let dev = w.people[b].device_id;
        let revoked_at = w
            .cfg
            .iter()
            .filter(|e| allowed(e) && matches!(&e.payload, SessionEvent::DeviceRevoked { device_id, .. } if *device_id == dev))
            .map(|e| e.lamport)
            .min();
        if let Some(cut) = revoked_at {
            if let Some(e) = all_envs(w).into_iter().find(|e| allowed(e) && e.signer_device_id == Some(dev) && e.lamport > cut) {
                return Err(format!("{b} signed an allowed {} after its revocation", e.payload.kind_str()));
            }
        }
    }
    Ok(())
}

proptest! {
    #[test]
    fn prop_service_members_never_govern_and_admissions_keep_their_terms(ops in prop::collection::vec(arb_op(), 0..20)) {
        let mut r = setup();
        play(&mut r, &ops);
        if let Err(m) = check(&r) {
            prop_assert!(false, "{}", m);
        }
    }
}

/// The generator reaches the interesting states: a box admitted through an
/// admission, then cut off by a revocation.
#[test]
fn the_generator_admits_and_revokes_a_box() {
    let mut r = setup();
    play(
        &mut r,
        &[
            Op::Issue { by: 1, slot: 0, ttl_min: 60, bind: None, presence: true },
            Op::Redeem { who: 3, slot: 0, role: 2, late_min: 0, holds: true, back: false },
            Op::Post { boxi: 0, presence: false },
            Op::Revoke { boxi: 0 },
            Op::Post { boxi: 0, presence: false },
        ],
    );
    assert_eq!(r.w.roster().members["box"].kind, ActorKind::Service);
    assert!(r.w.roster().devices[&r.w.people["box"].device_id].revoked);
    check(&r).unwrap();
}

/// The generator reaches the review's states: a member without the code
/// tries first, a holder's redemption is closed in, and a backdated
/// redemption with the code after the close admits no one.
#[test]
fn the_generator_closes_an_admission_against_a_backdated_redemption() {
    let mut r = setup();
    play(
        &mut r,
        &[
            Op::Issue { by: 1, slot: 0, ttl_min: 60, bind: None, presence: true },
            Op::Redeem { who: 4, slot: 0, role: 2, late_min: 0, holds: false, back: true },
            Op::Redeem { who: 3, slot: 0, role: 2, late_min: 0, holds: true, back: false },
            Op::Close { by: 1, slot: 0, name: true },
            Op::Redeem { who: 4, slot: 0, role: 2, late_min: 0, holds: true, back: true },
        ],
    );
    assert!(r.w.roster().members.contains_key("box"));
    assert!(!r.w.roster().members.contains_key("box2"));
    check(&r).unwrap();
}
