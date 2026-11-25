//! Proposal archiving (`ligus_core::proposal_archive`): who may archive and
//! restore, the settled and archived times the fold stamps (the events' own
//! recorded times, never the folding device's clock), the auto-archive view
//! rule, and the workspace setting an Admin sets.

use ligus_core::authz::votes::diff_digest;
use ligus_core::proposal_archive::{archived, auto_archive_due, Archived, ProposalArchivePolicy, DEFAULT_AFTER_DAYS};
use ligus_core::testkit::SignedWorld;
use ligus_core::{ActionProposal, ChatSession, ProposalApproval, ProposalStatus, SessionEvent, Timestamp, Verdict, WorkspaceRole};
use time::Duration;
use uuid::Uuid;

struct Fixture {
    w: SignedWorld,
    chat: Uuid,
    pid: Uuid,
}

const T0: i64 = 1_700_000_000;

fn at(secs: i64) -> Timestamp {
    Timestamp::from_unix_seconds(secs)
}

fn days(t: Timestamp, d: i64) -> Timestamp {
    Timestamp(t.inner() + Duration::days(d))
}

/// A workspace with an Admin (ada), a Contributor (mara) and a Viewer (vic),
/// and one open proposal by ada filed at `T0`.
fn fixture() -> Fixture {
    let mut w = SignedWorld::genesis(&["owner", "ada", "mara", "vic"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w.admit("owner", "vic", WorkspaceRole::Viewer);
    let chat = w.chat("owner", "c");
    let mut p = ActionProposal::file_diff("p", "ada", "diff --git a/f b/f\n+1\n".into(), vec!["f".into()]);
    p.created_at = at(T0);
    let pid = p.id;
    say_at(&mut w, chat, "ada", SessionEvent::ProposalUpserted { proposal: p }, at(T0));
    Fixture { w, chat, pid }
}

/// A chat event by `by` whose envelope records `t` (its author's clock).
fn say_at(w: &mut SignedWorld, chat: Uuid, by: &str, payload: SessionEvent, t: Timestamp) -> Uuid {
    let mut env = w.draft(by, Some(chat), payload, None);
    env.timestamp = t;
    w.finish(&mut env, by, None);
    w.push_env(Some(chat), env)
}

fn reject(f: &mut Fixture, by: &str, t: Timestamp) -> Uuid {
    let digest = diff_digest(&f.w.state(f.chat).proposals[0]);
    let approval = ProposalApproval { actor_id: f.w.people[by].actor().id, role: WorkspaceRole::Contributor, approved: false, created_at: t, diff_digest: Some(digest) };
    say_at(&mut f.w, f.chat, by, SessionEvent::ProposalVoteCast { proposal_id: f.pid, approval }, t)
}

fn archive(f: &mut Fixture, by: &str, archived: bool, t: Timestamp) -> Uuid {
    say_at(&mut f.w, f.chat, by, SessionEvent::ProposalDismissed { proposal_id: f.pid, dismissed: archived }, t)
}

fn proposal(s: &ChatSession) -> &ActionProposal {
    &s.proposals[0]
}

#[test]
fn settled_at_is_the_settling_events_own_time() {
    let mut f = fixture();
    let t = at(T0 + 3600);
    reject(&mut f, "mara", t);
    let s = f.w.state(f.chat);
    assert_eq!(proposal(&s).status, ProposalStatus::Rejected);
    assert_eq!(proposal(&s).settled_at, Some(t), "the vote's recorded time, not the folding device's clock");
    // Folding again later gives the same time: nothing reads the wall clock.
    assert_eq!(proposal(&f.w.state(f.chat)).settled_at, Some(t));
}

#[test]
fn auto_archive_is_a_view_rule_over_settled_at() {
    let mut f = fixture();
    let t = at(T0 + 3600);
    reject(&mut f, "mara", t);
    let s = f.w.state(f.chat);
    let p = proposal(&s);
    assert_eq!(archived(p, &s, Timestamp(days(t, DEFAULT_AFTER_DAYS as i64).inner() - Duration::seconds(1))), None, "a second short of the default");
    assert_eq!(archived(p, &s, days(t, DEFAULT_AFTER_DAYS as i64)), Some(Archived::Auto));
    assert_eq!(auto_archive_due(p, &s), Some(days(t, DEFAULT_AFTER_DAYS as i64)));
    // No event was written: the log holds only what people signed.
    assert!(!f.w.chats[&f.chat].iter().any(|e| matches!(e.payload, SessionEvent::ProposalDismissed { .. })));
}

#[test]
fn a_settling_event_from_a_clock_behind_never_ages_a_proposal_before_it_was_filed() {
    let mut f = fixture();
    // The voter's clock is a year behind the proposal's.
    let skewed = days(at(T0), -365);
    reject(&mut f, "mara", skewed);
    let s = f.w.state(f.chat);
    assert_eq!(proposal(&s).settled_at, Some(at(T0)), "clamped to when it was filed");
    assert_eq!(archived(proposal(&s), &s, days(at(T0), 6)), None);
    assert_eq!(archived(proposal(&s), &s, days(at(T0), 7)), Some(Archived::Auto));
}

#[test]
fn a_settling_event_from_a_clock_ahead_only_delays_auto_archive() {
    let mut f = fixture();
    let ahead = days(at(T0), 30);
    reject(&mut f, "mara", ahead);
    let s = f.w.state(f.chat);
    assert_eq!(proposal(&s).settled_at, Some(ahead));
    assert_eq!(archived(proposal(&s), &s, days(at(T0), 8)), None, "still in the inbox: it waits on the skewed clock");
    assert_eq!(archived(proposal(&s), &s, days(ahead, 7)), Some(Archived::Auto));
}

#[test]
fn reopening_clears_settled_at() {
    let mut f = fixture();
    reject(&mut f, "mara", at(T0 + 10));
    // mara changes her vote to an approval: open again (approved by quorum).
    let digest = diff_digest(&f.w.state(f.chat).proposals[0]);
    let approval = ProposalApproval { actor_id: f.w.people["mara"].actor().id, role: WorkspaceRole::Contributor, approved: true, created_at: at(T0 + 20), diff_digest: Some(digest) };
    say_at(&mut f.w, f.chat, "mara", SessionEvent::ProposalVoteCast { proposal_id: f.pid, approval }, at(T0 + 20));
    let s = f.w.state(f.chat);
    assert_ne!(proposal(&s).status, ProposalStatus::Rejected);
    assert_eq!(proposal(&s).settled_at, None);
    assert_eq!(archived(proposal(&s), &s, days(at(T0), 100)), None, "waiting on someone: never auto-archived");
}

#[test]
fn a_contributor_archives_for_everyone_and_the_stamp_says_who() {
    let mut f = fixture();
    reject(&mut f, "ada", at(T0 + 10));
    let t = at(T0 + 20);
    let id = archive(&mut f, "mara", true, t);
    assert_eq!(f.w.verdict(id), Verdict::Allowed);
    let s = f.w.state(f.chat);
    let p = proposal(&s);
    assert!(p.dismissed);
    assert_eq!(p.archived_at, Some(t));
    assert_eq!(p.archived_by, Some(f.w.people["mara"].account_id), "the account on the signed stamp");
    assert_eq!(archived(p, &s, at(T0 + 21)), Some(Archived::ByPerson));
}

/// A Viewer's archive or restore folds as on every build (`dismissed` flips,
/// so a 0.3 device and this one hold the same state), but the inbox ignores
/// it: only a Contributor+ archives for everyone.
#[test]
fn a_viewer_cant_archive_or_restore() {
    let mut f = fixture();
    reject(&mut f, "ada", at(T0 + 10));
    archive(&mut f, "vic", true, at(T0 + 20));
    let s = f.w.state(f.chat);
    assert!(proposal(&s).dismissed, "the shared fold is unchanged");
    assert_eq!(proposal(&s).archive_mark, Some(false), "filed in the inbox, and still there for the inbox");
    assert_eq!(archived(proposal(&s), &s, at(T0 + 21)), None, "the inbox ignores a Viewer's archive");
    archive(&mut f, "mara", true, at(T0 + 30));
    archive(&mut f, "vic", false, at(T0 + 40));
    let s = f.w.state(f.chat);
    assert!(!proposal(&s).dismissed);
    assert_eq!(archived(proposal(&s), &s, at(T0 + 41)), Some(Archived::ByPerson), "still archived: a Viewer can't restore");
    assert_eq!(proposal(&s).archived_by, Some(f.w.people["mara"].account_id));
}

#[test]
fn an_archive_never_hides_a_proposal_that_waits_on_someone() {
    let mut f = fixture();
    // The fold keeps the flag (an older build wrote it, or a hostile
    // client), but the view rule leaves it in the inbox.
    archive(&mut f, "mara", true, at(T0 + 10));
    let s = f.w.state(f.chat);
    assert!(proposal(&s).dismissed);
    assert_eq!(archived(proposal(&s), &s, days(at(T0), 100)), None);
    // Once the hygiene sweep marks it stale (its diff no longer applies),
    // the archive takes it out.
    say_at(&mut f.w, f.chat, "mara", SessionEvent::ProposalMarkedStale { proposal_id: f.pid }, at(T0 + 20));
    let s = f.w.state(f.chat);
    assert_eq!(archived(proposal(&s), &s, at(T0 + 21)), Some(Archived::ByPerson));
}

#[test]
fn restoring_an_auto_archived_proposal_keeps_it_in_the_inbox() {
    let mut f = fixture();
    reject(&mut f, "ada", at(T0 + 10));
    let later = days(at(T0), 30);
    assert_eq!(archived(proposal(&f.w.state(f.chat)), &f.w.state(f.chat), later), Some(Archived::Auto));
    archive(&mut f, "mara", false, later);
    let s = f.w.state(f.chat);
    assert!(proposal(&s).keep_unarchived);
    assert_eq!(archived(proposal(&s), &s, days(later, 365)), None);
    // Archiving it by hand still works.
    archive(&mut f, "mara", true, days(later, 1));
    let s = f.w.state(f.chat);
    assert_eq!(archived(proposal(&s), &s, days(later, 2)), Some(Archived::ByPerson));
}

#[test]
fn an_upsert_cant_assert_settled_or_archived_times() {
    let mut f = fixture();
    let mut p = f.w.state(f.chat).proposals[0].clone();
    p.title = "renamed".into();
    p.settled_at = Some(at(0));
    p.archived_at = Some(at(0));
    p.archived_by = Some(Uuid::new_v4());
    p.keep_unarchived = true;
    say_at(&mut f.w, f.chat, "ada", SessionEvent::ProposalUpserted { proposal: p }, at(T0 + 5));
    let s = f.w.state(f.chat);
    let q = proposal(&s);
    assert_eq!(q.title, "renamed");
    assert_eq!((q.settled_at, q.archived_at, q.archived_by, q.keep_unarchived, q.archive_mark), (None, None, None, false, Some(false)));
}

#[test]
fn superseded_counts_as_settled() {
    let mut f = fixture();
    let mut p = f.w.state(f.chat).proposals[0].clone();
    p.superseded_by = Some(Uuid::new_v4());
    let t = at(T0 + 50);
    say_at(&mut f.w, f.chat, "ada", SessionEvent::ProposalUpserted { proposal: p }, t);
    let s = f.w.state(f.chat);
    assert_eq!(proposal(&s).settled_at, Some(t));
    assert_eq!(archived(proposal(&s), &s, days(t, 7)), Some(Archived::Auto));
}

#[test]
fn only_an_admin_sets_the_auto_archive_days() {
    let mut w = SignedWorld::genesis(&["owner", "ada", "mara"], None);
    w.admit("owner", "ada", WorkspaceRole::Admin);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    let set = |d| SessionEvent::ProposalArchivePolicySet { policy: ProposalArchivePolicy { after_days: d } };
    let denied = w.push_bare("mara", set(1));
    assert_eq!(w.verdict(denied), Verdict::Denied);
    assert_eq!(w.config().guards.proposal_archive, None);
    let ok = w.push_bare("ada", set(14));
    assert_eq!(w.verdict(ok), Verdict::Allowed, "no presence needed");
    assert_eq!(ProposalArchivePolicy::days_in_force(w.config().guards.proposal_archive.as_ref()), Some(14));
    let too_long = w.push_bare("ada", set(366));
    assert_eq!(w.verdict(too_long), Verdict::Denied);
    let off = w.push_bare("ada", set(0));
    assert_eq!(w.verdict(off), Verdict::Allowed);
    assert_eq!(ProposalArchivePolicy::days_in_force(w.config().guards.proposal_archive.as_ref()), None, "off");
}

#[test]
fn the_setting_is_kept_on_the_config_log_only() {
    let mut w = SignedWorld::genesis(&["owner"], None);
    let chat = w.chat("owner", "c");
    let id = w.say(chat, "owner", SessionEvent::ProposalArchivePolicySet { policy: ProposalArchivePolicy { after_days: 3 } });
    assert_eq!(w.verdict(id), Verdict::Denied);
    assert_eq!(w.state(chat).guards.proposal_archive, None);
}

#[test]
fn turning_auto_archive_off_brings_settled_proposals_back() {
    let mut f = fixture();
    reject(&mut f, "ada", at(T0 + 10));
    let mut s = f.w.state(f.chat);
    let later = days(at(T0), 30);
    let p = proposal(&s).clone();
    assert_eq!(archived(&p, &s, later), Some(Archived::Auto));
    s.guards.proposal_archive = Some(ProposalArchivePolicy { after_days: 0 });
    assert_eq!(archived(&p, &s, later), None);
    s.guards.proposal_archive = Some(ProposalArchivePolicy { after_days: 60 });
    assert_eq!(archived(&p, &s, later), None);
}
