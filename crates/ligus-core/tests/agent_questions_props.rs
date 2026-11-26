//! Q-T3b: the eviction shadow. Under forged closes, answers that don't
//! count, quorum asks and "Not now", past the 256-ask cap, a fold with the
//! 0.4 rules keeps exactly the asks an older build keeps, and judges every
//! answer's `optionIndex` (the ingest bound) the same.

use ligus_core::authz::bounds::flow_within_bounds;
use ligus_core::flow::asks::AskProvenance;
use ligus_core::flow::{
    AskAudience, AskPoster, FlowRecord, FlowState, HumanAnswered, HumanAsk, ASK_V2,
    MAX_ASKS_PER_CHAT,
};
use ligus_core::{ChatSession, WorkspaceRole};
use proptest::prelude::*;
use uuid::Uuid;

#[derive(Debug, Clone)]
enum Op {
    Ask {
        v2: bool,
        quorum: u8,
        options: u8,
    },
    Close {
        pick: usize,
        by_signer: bool,
        settled: bool,
    },
    Answer {
        pick: usize,
        option: Option<u32>,
        counts: bool,
        decline: bool,
        who: u8,
    },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (any::<bool>(), 1u8..4, 0u8..6).prop_map(|(v2, quorum, options)| Op::Ask { v2, quorum, options }),
        1 => (any::<usize>(), any::<bool>(), any::<bool>()).prop_map(|(pick, by_signer, settled)| Op::Close { pick, by_signer, settled }),
        3 => (any::<usize>(), proptest::option::of(0u32..7), any::<bool>(), any::<bool>(), 0u8..4)
            .prop_map(|(pick, option, counts, decline, who)| Op::Answer { pick, option, counts, decline, who }),
    ]
}

fn prov(signer: u128, who: u8, why: &str) -> AskProvenance {
    let d = Uuid::from_u128(signer);
    AskProvenance {
        signer: Some(d),
        host: Some(d),
        by: AskPoster {
            id: format!("p{who}"),
            name: String::new(),
        },
        from_host: true,
        effect_ok: true,
        may_grant: true,
        why: why.into(),
        event_id: Some(Uuid::new_v4()),
        ..Default::default()
    }
}

fn session(flow: &FlowState) -> ChatSession {
    let mut s = ChatSession::new("", Uuid::nil(), "");
    s.flow = flow.clone();
    s
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]
    #[test]
    fn eviction_and_option_bounds_agree_with_older_builds(ops in proptest::collection::vec(op(), 200..420)) {
        let mut new = FlowState::default();
        let mut old = FlowState::default();
        let mut ids: Vec<HumanAsk> = Vec::new();
        for (n, o) in ops.into_iter().enumerate() {
            let rec = match o {
                Op::Ask { v2, quorum, options } => {
                    let a = HumanAsk {
                        ask_id: Uuid::new_v4(),
                        turn_id: Uuid::from_u128(7),
                        agent_key: "primary".into(),
                        question: "q".into(),
                        options: (0..options).map(|i| format!("o{i}")).collect(),
                        at_ms: n as i64,
                        v: if v2 { ASK_V2 } else { 0 },
                        audience: v2.then(|| AskAudience::room(WorkspaceRole::Contributor)),
                        quorum: (quorum > 1).then_some(quorum),
                        ..Default::default()
                    };
                    ids.push(a.clone());
                    new.apply_ask(&FlowRecord::HumanAsk(a.clone()), &prov(1, 0, ""));
                    FlowRecord::HumanAsk(a)
                }
                Op::Close { pick, by_signer, settled } => {
                    if ids.is_empty() { continue; }
                    let a = &ids[pick % ids.len()];
                    let c = HumanAsk { closed: true, settled_by: settled.then(Uuid::new_v4), ..a.clone() };
                    new.apply_ask(&FlowRecord::HumanAsk(c.clone()), &prov(if by_signer { 1 } else { 2 }, 0, ""));
                    FlowRecord::HumanAsk(c)
                }
                Op::Answer { pick, option, counts, decline, who } => {
                    if ids.is_empty() { continue; }
                    let a = &ids[pick % ids.len()];
                    let ans = FlowRecord::HumanAnswered(HumanAnswered { ask_id: a.ask_id, answer: "x".into(), option_index: option, at_ms: n as i64, decline, ..Default::default() });
                    // Both versions judge the ingest bound on their own fold.
                    let verdict_new = flow_within_bounds(&session(&new), &ans);
                    let verdict_old = flow_within_bounds(&session(&old), &ans);
                    prop_assert_eq!(verdict_new, verdict_old, "optionIndex ingest verdicts agree");
                    if !verdict_new { continue; }
                    new.apply_ask(&ans, &prov(3, who, if counts { "" } else { "not asked" }));
                    ans
                }
            };
            old.apply(&rec);
            let keys = |s: &FlowState| s.asks.keys().copied().collect::<Vec<_>>();
            prop_assert_eq!(keys(&new), keys(&old));
            prop_assert!(new.asks.len() <= MAX_ASKS_PER_CHAT);
        }
    }
}

/// Review M2: eviction over a snapshot reads only what 0.3 reads from it
/// (its answer and closed flags), never 0.4's `declined`.
#[test]
fn a_snapshot_with_a_declined_ask_evicts_as_older_builds_do() {
    let mut s = ChatSession::new("t", Uuid::nil(), "rt");
    let mut asks = serde_json::Map::new();
    for i in 0..MAX_ASKS_PER_CHAT as u128 {
        let id = Uuid::from_u128(i + 1);
        let a = HumanAsk {
            ask_id: id,
            question: "q".into(),
            options: vec!["a".into(), "b".into()],
            at_ms: i as i64,
            ..Default::default()
        };
        let mut st = serde_json::json!({"ask": serde_json::to_value(&a).unwrap()});
        if i == 0 {
            st["declined"] = serde_json::json!(true);
            st["ask"]["v"] = serde_json::json!(2);
        }
        asks.insert(id.to_string(), st);
    }
    let mut flow = serde_json::to_value(&s.flow).unwrap();
    flow["asks"] = serde_json::Value::Object(asks);
    s.flow = serde_json::from_value(flow).unwrap();
    s.flow.reseat_snapshot_asks();
    s.flow.apply(&FlowRecord::HumanAsk(HumanAsk {
        ask_id: Uuid::from_u128(9999),
        question: "q".into(),
        options: vec!["a".into()],
        at_ms: 10_000,
        ..Default::default()
    }));
    let ans = FlowRecord::HumanAnswered(HumanAnswered {
        ask_id: Uuid::from_u128(1),
        answer: "a".into(),
        option_index: Some(0),
        at_ms: 1,
        ..Default::default()
    });
    assert!(
        flow_within_bounds(&s, &ans),
        "the declined ask is kept, as on 0.3"
    );
}
