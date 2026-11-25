//! Agent questions with the 0.4 rules (`docs/design/agent-questions.md`,
//! sections 5 and 6): who may answer, quorum, "Not now", the asker's last
//! word, effects and presence. Every rule is fold-local: every record here
//! is accepted (the verdicts of builds with and without these rules agree),
//! and one that doesn't qualify is kept and counts for nothing.

use ligus_core::flow::asks::{ask_digest, signed_payload};
use ligus_core::flow::{
    AskAudience, AskEffect, AskOrigin, FlowRecord, HumanAnswered, HumanAsk, ASK_V2, EFFECT_PERMISSION, ORIGIN_OUTSIDE,
    PRESENCE_REQUIRED, PRIMARY,
};
use ligus_core::testkit::{Proof, SignedWorld};
use ligus_core::{ChatMessage, MessageRole, SessionEvent, Verdict, WorkspaceRole};
use uuid::Uuid;

/// Ana (Owner) hosts a turn of the chat's runtime on her device; Dee is an
/// Admin, Bo a Contributor and Cy a Viewer, each on their own device.
struct Room {
    w: SignedWorld,
    chat: Uuid,
    turn: Uuid,
}

fn room() -> Room {
    let mut w = SignedWorld::genesis(&["ana", "bo", "cy", "dee", "eve"], None);
    w.admit("ana", "bo", WorkspaceRole::Contributor);
    w.admit("ana", "cy", WorkspaceRole::Viewer);
    w.admit("ana", "dee", WorkspaceRole::Admin);
    let chat = w.chat("ana", "Fixtures");
    let mut ask = ChatMessage::new(MessageRole::User, "bo", "set up the test fixtures");
    ask.actor_identity = Some(w.people["bo"].actor());
    let trigger = ask.id;
    w.say(chat, "bo", SessionEvent::MessageAppended { message: ask });
    let mut reply = ChatMessage::new(MessageRole::Assistant, "Ligus", "");
    reply.reply_to_message_id = Some(trigger);
    reply.origin_device_id = Some(w.people["ana"].device_id);
    reply.is_streaming = true;
    let turn = reply.id;
    w.say(chat, "ana", SessionEvent::MessageAppended { message: reply });
    Room { w, chat, turn }
}

fn question(turn: Uuid, audience: AskAudience, quorum: u8) -> HumanAsk {
    HumanAsk {
        ask_id: Uuid::new_v4(),
        turn_id: turn,
        agent_key: PRIMARY.into(),
        question: "Use Postgres or SQLite for the test fixtures?".into(),
        options: vec!["Postgres".into(), "SQLite".into()],
        at_ms: 1,
        v: ASK_V2,
        audience: Some(audience),
        quorum: (quorum > 1).then_some(quorum),
        ..Default::default()
    }
}

impl Room {
    /// Record `rec` by `by`; every record is accepted.
    fn record(&mut self, by: &str, rec: FlowRecord) -> Uuid {
        let id = self.w.say(self.chat, by, SessionEvent::FlowRecorded { record: rec });
        assert_eq!(self.w.verdict(id), Verdict::Allowed, "every record is accepted, as before");
        id
    }

    /// Post `a` by `by`; returns its digest.
    fn post(&mut self, by: &str, a: &HumanAsk) -> String {
        let id = self.record(by, FlowRecord::HumanAsk(a.clone()));
        let env = self.w.chats[&self.chat].iter().find(|e| e.event_id == id).unwrap();
        ask_digest(a.ask_id, &signed_payload(env))
    }

    fn answer(&mut self, by: &str, ask: &HumanAsk, option: Option<u32>, digest: &str) -> Uuid {
        let rec = answer_rec(ask, by, option, digest);
        self.record(by, rec)
    }

    fn st(&self, ask: &HumanAsk) -> ligus_core::flow::AskState {
        self.w.state(self.chat).flow.asks[&ask.ask_id].clone()
    }
}

fn answer_rec(ask: &HumanAsk, by: &str, option: Option<u32>, digest: &str) -> FlowRecord {
    FlowRecord::HumanAnswered(HumanAnswered {
        ask_id: ask.ask_id,
        answer: option.map(|i| ask.options[i as usize].clone()).unwrap_or_default(),
        option_index: option,
        by: by.into(),
        at_ms: 2,
        ask_digest: digest.into(),
        ..Default::default()
    })
}

fn who(st: &ligus_core::flow::AskState) -> Vec<String> {
    st.answered_by.iter().map(|v| v.member_id.clone()).collect()
}

#[test]
fn a_person_audience_takes_only_that_persons_answer_over_the_asks_digest() {
    let mut r = room();
    let q = question(r.turn, AskAudience::people(vec!["bo".into()]), 1);
    let d = r.post("ana", &q);
    // Ana isn't asked; Bo without a digest answered on an older version;
    // Bo with another question's digest answers something else.
    r.answer("ana", &q, Some(0), &d);
    r.answer("bo", &q, Some(0), "");
    r.answer("bo", &q, Some(0), &"0".repeat(64));
    let st = r.st(&q);
    assert!(st.is_open(), "none of those counts");
    let whys: Vec<&str> = st.uncounted.iter().map(|v| v.why.as_str()).collect();
    assert_eq!(whys, vec!["not asked", "answered on an older version", "answers another question"]);
    r.answer("bo", &q, Some(1), &d);
    let st = r.st(&q);
    assert_eq!(st.answer.as_ref().and_then(|a| a.option_index), Some(1));
    assert_eq!(who(&st), vec!["bo"]);
}

#[test]
fn a_room_counts_its_role_floor_and_a_quorum_settles_once_frozen() {
    let mut r = room();
    let q = question(r.turn, AskAudience::room(WorkspaceRole::Admin), 2);
    let d = r.post("ana", &q);
    r.answer("bo", &q, Some(0), &d); // a Contributor: below the floor
    r.answer("ana", &q, Some(0), &d);
    r.answer("dee", &q, Some(1), &d);
    let st = r.st(&q);
    assert!(st.is_open(), "split votes: no option has two");
    assert_eq!(st.tally(), vec![1, 1]);
    assert_eq!(st.uncounted.len(), 1);
    // Dee changes her mind: her latest answer is her vote, and it settles.
    r.answer("dee", &q, Some(0), &d);
    let st = r.st(&q);
    assert_eq!(st.answer.as_ref().and_then(|a| a.option_index), Some(0));
    assert_eq!(who(&st), vec!["ana", "dee"]);
    // Settled at that position and frozen: a later change doesn't un-settle it.
    r.answer("dee", &q, Some(1), &d);
    r.answer("ana", &q, Some(1), &d);
    let st = r.st(&q);
    assert_eq!(st.answer.as_ref().and_then(|a| a.option_index), Some(0));
    assert!(!st.is_open());
}

#[test]
fn a_viewer_never_answers_and_the_room_floor_is_never_below_contributor() {
    let mut r = room();
    let mut q = question(r.turn, AskAudience::room(WorkspaceRole::Contributor), 1);
    q.audience.as_mut().unwrap().min_role = Some("viewer".into());
    let d = r.post("ana", &q);
    // A Viewer's FlowRecorded is refused by the role floor as before; the
    // fold would not count it anyway. Bo (Contributor) settles it.
    r.answer("bo", &q, Some(1), &d);
    assert!(r.st(&q).uncounted.iter().any(|v| v.why == "not asked"), "a floor this build can't read asks nobody");
    let q2 = question(r.turn, AskAudience::room(WorkspaceRole::Contributor), 1);
    let d2 = r.post("ana", &q2);
    r.answer("bo", &q2, Some(1), &d2);
    assert_eq!(who(&r.st(&q2)), vec!["bo"]);
}

#[test]
fn role_is_judged_at_the_answers_frontier_never_older_than_the_asks() {
    let mut r = room();
    let before_ask = r.w.head();
    // The config log moves on before the ask is posted.
    r.w.admit("ana", "eve", WorkspaceRole::Contributor);
    let q = question(r.turn, AskAudience::room(WorkspaceRole::Admin), 1);
    let d = r.post("ana", &q);
    // Dee is demoted to Contributor.
    let demote = r.w.write(
        "ana",
        None,
        SessionEvent::MemberRoleChanged {
            change: ligus_core::events::MemberRoleChange { member_id: "dee".into(), old_role: WorkspaceRole::Admin, new_role: WorkspaceRole::Contributor },
            watermark: Vec::new(),
        },
        None,
        Proof::Recovery,
    );
    assert_eq!(r.w.verdict(demote), Verdict::Allowed);
    // Her answer after the demotion doesn't count.
    r.answer("dee", &q, Some(0), &d);
    assert!(r.st(&q).is_open());
    assert_eq!(r.st(&q).uncounted[0].why, "below the role it asks");
    // Nor does one back-dated to a frontier older than the ask's own.
    let chat = r.chat;
    let id = r.w.write("dee", Some(chat), SessionEvent::FlowRecorded { record: answer_rec(&q, "dee", Some(0), &d) }, before_ask, Proof::None);
    assert_eq!(r.w.verdict(id), Verdict::Allowed);
    let st = r.st(&q);
    assert!(st.is_open());
    assert_eq!(st.uncounted[1].why, "stamped before the ask");
}

#[test]
fn not_now_closes_a_personal_ask_and_abstains_in_a_room() {
    let mut r = room();
    let mine = question(r.turn, AskAudience::people(vec!["bo".into()]), 1);
    let d = r.post("ana", &mine);
    let decline = |a: &HumanAsk, by: &str, d: &str| {
        FlowRecord::HumanAnswered(HumanAnswered { ask_id: a.ask_id, answer: "Not now".into(), by: by.into(), at_ms: 3, ask_digest: d.into(), decline: true, ..Default::default() })
    };
    r.record("bo", decline(&mine, "bo", &d));
    let st = r.st(&mine);
    assert!(st.declined && !st.is_open() && st.answer.is_none());
    // In a room (D6): only that person's abstention.
    let room_q = question(r.turn, AskAudience::room(WorkspaceRole::Contributor), 1);
    let d = r.post("ana", &room_q);
    r.record("bo", decline(&room_q, "bo", &d));
    let st = r.st(&room_q);
    assert!(st.is_open() && !st.declined, "\"Not now\" doesn't close a room ask for everyone");
    r.answer("dee", &room_q, Some(0), &d);
    assert_eq!(who(&r.st(&room_q)), vec!["dee"]);
}

#[test]
fn the_askers_settled_by_has_the_last_word() {
    let mut r = room();
    let q = question(r.turn, AskAudience::people(vec!["bo".into(), "dee".into()]), 1);
    let d = r.post("ana", &q);
    let bo_first = r.answer("bo", &q, Some(1), &d);
    r.answer("dee", &q, Some(0), &d);
    assert_eq!(who(&r.st(&q)), vec!["bo"]);
    // The host acted on Dee's answer (it had seen hers first): it says so.
    let dee_event = r.w.chats[&r.chat].last().unwrap().event_id;
    r.record("ana", FlowRecord::HumanAsk(HumanAsk { closed: true, settled_by: Some(dee_event), ..q.clone() }));
    let st = r.st(&q);
    assert_eq!(who(&st), vec!["dee"]);
    assert_eq!(st.answer.as_ref().and_then(|a| a.option_index), Some(0));
    assert!(st.ask.closed);
    // Someone else's settledBy changes nothing.
    let q2 = question(r.turn, AskAudience::people(vec!["bo".into(), "dee".into()]), 1);
    let d2 = r.post("ana", &q2);
    let first = r.answer("bo", &q2, Some(1), &d2);
    r.answer("dee", &q2, Some(0), &d2);
    let dee2 = r.w.chats[&r.chat].last().unwrap().event_id;
    r.record("bo", FlowRecord::HumanAsk(HumanAsk { closed: true, settled_by: Some(dee2), ..q2.clone() }));
    let st = r.st(&q2);
    assert_eq!(who(&st), vec!["bo"]);
    assert!(!st.ask.closed);
    let _ = (bo_first, first);
}

fn grant(r: &Room, presence: &str) -> HumanAsk {
    let host = r.w.people["ana"].device_id;
    HumanAsk {
        question: "Ligus asks to run cargo publish --dry-run".into(),
        options: vec!["Allow once".into(), "Deny".into()],
        permission: true,
        effect: Some(AskEffect {
            kind: EFFECT_PERMISSION.into(),
            tool: "execute".into(),
            title: "cargo publish --dry-run".into(),
            detail: "cargo publish --dry-run".into(),
            host_device_id: Some(host),
            presence: presence.into(),
            ..Default::default()
        }),
        ..question(r.turn, AskAudience::people(vec!["ana".into()]), 1)
    }
}

#[test]
fn an_effect_counts_only_from_the_turns_host_naming_itself() {
    let mut r = room();
    let real = grant(&r, "none");
    r.post("ana", &real);
    assert!(r.st(&real).grant().is_some_and(|e| e.detail == "cargo publish --dry-run"));
    // Bo signs the same shape: kept, a plain question, his.
    let forged = HumanAsk { ask_id: Uuid::new_v4(), ..real.clone() };
    r.post("bo", &forged);
    let st = r.st(&forged);
    assert!(st.grant().is_none() && st.posted_by.as_ref().is_some_and(|p| p.id == "bo"));
    // Ana's device naming another host is inert too.
    let mut other = HumanAsk { ask_id: Uuid::new_v4(), ..real.clone() };
    other.effect.as_mut().unwrap().host_device_id = Some(r.w.people["bo"].device_id);
    r.post("ana", &other);
    assert!(r.st(&other).grant().is_none());
    // And on a turn it doesn't run.
    let stray = HumanAsk { ask_id: Uuid::new_v4(), turn_id: Uuid::new_v4(), ..real.clone() };
    r.post("ana", &stray);
    assert!(r.st(&stray).grant().is_none());
    // An effect this build can't show (an unknown kind) is inert.
    let mut odd = HumanAsk { ask_id: Uuid::new_v4(), ..real.clone() };
    odd.effect.as_mut().unwrap().kind = "apply".into();
    r.post("ana", &odd);
    assert!(r.st(&odd).ask.effect.is_none());
}

#[test]
fn a_grant_needing_presence_counts_only_with_the_persons_passkey() {
    let mut r = room();
    let g = grant(&r, PRESENCE_REQUIRED);
    let d = r.post("ana", &g);
    r.answer("ana", &g, Some(0), &d);
    let st = r.st(&g);
    assert!(st.is_open());
    assert_eq!(st.uncounted[0].why, "no passkey");
    let chat = r.chat;
    let id = r.w.write("ana", Some(chat), SessionEvent::FlowRecorded { record: answer_rec(&g, "ana", Some(0), &d) }, None, Proof::Recovery);
    assert_eq!(r.w.verdict(id), Verdict::Allowed);
    let st = r.st(&g);
    assert_eq!(who(&st), vec!["ana"]);
}

#[test]
fn a_teammates_allow_on_your_grant_counts_for_nothing() {
    let mut r = room();
    let mut g = grant(&r, "none");
    g.audience = Some(AskAudience::people(vec!["ana".into(), "bo".into()]));
    let d = r.post("ana", &g);
    // Bo is named, but he is neither the host's person nor the agent's owner.
    r.answer("bo", &g, Some(0), &d);
    let st = r.st(&g);
    assert!(st.is_open());
    assert_eq!(st.uncounted[0].why, "not the host's person or the agent's owner");
}

fn outside(r: &Room, by: &str, audience: Vec<&str>) -> HumanAsk {
    let device = r.w.people[by].device_id;
    HumanAsk {
        ask_id: Uuid::new_v4(),
        turn_id: Uuid::nil(),
        agent_key: "outside:claude-code".into(),
        question: "Claude Code (outside Ligus) asks to run a command".into(),
        options: vec!["Allow once".into(), "Deny".into()],
        at_ms: 1,
        v: ASK_V2,
        audience: Some(AskAudience::people(audience.into_iter().map(String::from).collect())),
        effect: Some(AskEffect {
            kind: EFFECT_PERMISSION.into(),
            tool: "Bash".into(),
            detail: "rm -rf node_modules".into(),
            host_device_id: Some(device),
            presence: PRESENCE_REQUIRED.into(),
            ..Default::default()
        }),
        origin: Some(AskOrigin { via: ORIGIN_OUTSIDE.into(), runtime: "claude-code".into(), label: "shop".into(), ..Default::default() }),
        ..Default::default()
    }
}

#[test]
fn an_outside_prompt_counts_only_from_a_person_to_themselves() {
    let mut r = room();
    let mine = outside(&r, "bo", vec!["bo"]);
    let d = r.post("bo", &mine);
    assert!(r.st(&mine).grant().is_some(), "a person may raise a grant card to themselves");
    // To someone else, or to more than themselves: a plain question.
    let theirs = outside(&r, "bo", vec!["ana"]);
    r.post("bo", &theirs);
    assert!(r.st(&theirs).grant().is_none());
    let both = outside(&r, "bo", vec!["bo", "ana"]);
    r.post("bo", &both);
    assert!(r.st(&both).grant().is_none());
    // Only Bo, with his passkey, grants his own.
    r.answer("ana", &mine, Some(0), &d);
    r.answer("bo", &mine, Some(0), &d);
    assert!(r.st(&mine).is_open());
    let chat = r.chat;
    r.w.write("bo", Some(chat), SessionEvent::FlowRecorded { record: answer_rec(&mine, "bo", Some(0), &d) }, None, Proof::Recovery);
    assert_eq!(who(&r.st(&mine)), vec!["bo"]);
    // His device closes it.
    let q = outside(&r, "bo", vec!["bo"]);
    r.post("bo", &q);
    r.record("ana", FlowRecord::HumanAsk(HumanAsk { closed: true, ..q.clone() }));
    assert!(r.st(&q).is_open());
    r.record("bo", FlowRecord::HumanAsk(HumanAsk { closed: true, ..q.clone() }));
    assert!(!r.st(&q).is_open());
}

/// Q-T11: an answer from a 0.3 peer (no digest) to a v2 ask doesn't count;
/// an older ask (`ligus_ask_human`) folds as before.
#[test]
fn older_peers_answers_and_older_asks() {
    let mut r = room();
    let q = question(r.turn, AskAudience::room(WorkspaceRole::Contributor), 1);
    r.post("ana", &q);
    r.record("bo", FlowRecord::HumanAnswered(HumanAnswered { ask_id: q.ask_id, answer: "Postgres".into(), option_index: Some(0), by: "Bo".into(), at_ms: 2, ..Default::default() }));
    let st = r.st(&q);
    assert!(st.is_open());
    assert_eq!(st.uncounted[0].why, "answered on an older version");
    assert!(st.legacy_settled, "the v1 shadow sees it, as 0.3 does");
    // A v1 ask: the first answer wins, from anyone at Contributor or above.
    let old = HumanAsk { v: 0, audience: None, ask_id: Uuid::new_v4(), ..q.clone() };
    r.post("ana", &old);
    r.record("bo", FlowRecord::HumanAnswered(HumanAnswered { ask_id: old.ask_id, answer: "Postgres".into(), option_index: Some(0), by: "Bo".into(), at_ms: 2, ..Default::default() }));
    r.record("dee", FlowRecord::HumanAnswered(HumanAnswered { ask_id: old.ask_id, answer: "SQLite".into(), option_index: Some(1), by: "Dee".into(), at_ms: 3, ..Default::default() }));
    let st = r.st(&old);
    assert_eq!(st.answer.as_ref().map(|a| a.by.as_str()), Some("Bo"));
}

/// Fields this build can't read fold as absent; the record still decodes
/// and is accepted (an older build ignores them the same way).
#[test]
fn unreadable_new_fields_decode_as_absent() {
    let raw = serde_json::json!({
        "type": "humanAsk", "askId": Uuid::new_v4(), "turnId": Uuid::new_v4(), "agentKey": "primary",
        "question": "q", "atMs": 1, "v": "two", "quorum": 900, "audience": 5, "effect": [1], "origin": {"via": 3}, "settledBy": "nope", "allowText": "yes"
    });
    let rec: FlowRecord = serde_json::from_value(raw).unwrap();
    let FlowRecord::HumanAsk(a) = rec else { panic!("decodes as an ask") };
    assert_eq!((a.v, a.quorum, a.audience, a.effect, a.origin, a.settled_by, a.allow_text), (0, None, None, None, None, None, false));
    let raw = serde_json::json!({"type": "humanAnswered", "askId": Uuid::new_v4(), "answer": "x", "atMs": 1, "text": 7, "askDigest": [], "decline": "y"});
    let FlowRecord::HumanAnswered(a) = serde_json::from_value(raw).unwrap() else { panic!() };
    assert!(a.text.is_empty() && a.ask_digest.is_empty() && !a.decline);
}

/// The digest's golden vector (shared with the phone repositories):
/// `crates/ligus-core/tests/golden/ask_digest_v1.json`.
#[test]
fn ask_digest_golden_vector() {
    let v: serde_json::Value = serde_json::from_str(include_str!("golden/ask_digest_v1.json")).unwrap();
    for case in v["cases"].as_array().unwrap() {
        let id = Uuid::parse_str(case["askId"].as_str().unwrap()).unwrap();
        let payload = case["payload"].as_str().unwrap();
        assert_eq!(ask_digest(id, payload.as_bytes()), case["digest"].as_str().unwrap(), "{}", case["name"]);
    }
}
