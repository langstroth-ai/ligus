//! Host offers (workspace model v3, 5.1): what a host lists on its record,
//! the readiness report in its sealed beat, and which host records readers
//! use. Invariants H6, H7 and H11 of the design note.

use std::collections::BTreeSet;

use ligus_core::authz::timeline::{DeviceState, MemberState};
use ligus_core::authz::RosterState;
use ligus_core::events::Verdict;
use ligus_core::testkit::SignedWorld;
use ligus_core::workspace_host::{bound_hosts, looks_sensitive, MAX_STATUS_MODELS};
use ligus_core::workspace_repo::{status_message, HostBeat};
use ligus_core::{
    ActorKind, HostKind, HostModelGateway, HostOffer, HostStatus, OfferKind, OfferStatus, SessionEvent, SigningKeypair, WorkspaceHost,
    WorkspaceRole,
};
use proptest::prelude::*;
use uuid::Uuid;

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["ana", "ben", "cy"], None);
    w.admit("ana", "ben", WorkspaceRole::Admin);
    w.admit("ana", "cy", WorkspaceRole::Contributor);
    let id = w.push("ana", SessionEvent::WorkspaceRulesSet { version: 3, presence: None });
    assert_eq!(w.verdict(id), Verdict::Allowed);
    w
}

fn offer(name: &str, kind: OfferKind, harness: &str, wire: &str) -> HostOffer {
    HostOffer { name: name.into(), kind, harness: harness.into(), wire: wire.into() }
}

fn host_of(w: &SignedWorld, device_of: &str, owner: &str, offers: Option<Vec<HostOffer>>) -> WorkspaceHost {
    let mut h = WorkspaceHost::new(w.people[device_of].device_id.to_string(), HostKind::Device, format!("{device_of}-mbp"));
    h.owner_actor_id = owner.into();
    h.offers = offers;
    h
}

fn readers(w: &SignedWorld) -> Vec<WorkspaceHost> {
    let c = w.config();
    bound_hosts(&c.workspace_hosts, &c.authz.host_signers)
}

/// H11: a Contributor who registers a host under another person's device id
/// gets a record the fold allows (no one held the id yet) but no reader or
/// picker uses: it wasn't signed by the device it names.
#[test]
fn a_host_record_its_device_didnt_sign_is_ignored_by_readers() {
    let mut w = world();
    let squat = host_of(&w, "ana", "cy", Some(vec![offer("claude-code", OfferKind::Cli, "claudeCode", "")]));
    let id = w.push_bare("cy", SessionEvent::WorkspaceHostUpserted { host: squat });
    assert_eq!(w.verdict(id), Verdict::Allowed, "control: the fold allows the squat under rules 3");
    assert_eq!(w.config().workspace_hosts.len(), 1, "the fold keeps it for authorization");
    assert!(readers(&w).is_empty(), "a reader used a host record its device never signed");

    // Cy's own computer, signed by itself, counts.
    let own = host_of(&w, "cy", "cy", Some(vec![]));
    w.push_bare("cy", SessionEvent::WorkspaceHostUpserted { host: own.clone() });
    let r = readers(&w);
    assert_eq!(r.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), [own.id.as_str()]);

    // An Admin rewriting Cy's record from another device: the fold takes it,
    // readers don't until Cy's device writes again.
    let mut over = own.clone();
    over.label = "renamed".into();
    let id = w.push("ben", SessionEvent::WorkspaceHostUpserted { host: over });
    assert_eq!(w.verdict(id), Verdict::Allowed);
    assert!(readers(&w).is_empty(), "a record another device wrote under Cy's id counted");
    w.push_bare("cy", SessionEvent::WorkspaceHostUpserted { host: own.clone() });
    assert_eq!(readers(&w).len(), 1);

    // A removal by the owner removes it from both views.
    w.push_bare("cy", SessionEvent::WorkspaceHostRemoved { host_id: own.id.clone() });
    assert!(readers(&w).is_empty());
}

/// A legacy whole-list event counts only for the signer's own host in it.
#[test]
fn a_whole_list_event_binds_only_the_signers_own_host() {
    let mut w = world();
    let mine = host_of(&w, "ana", "ana", None);
    let mut theirs = WorkspaceHost::new(Uuid::new_v4().to_string(), HostKind::Worker, "box");
    theirs.owner_actor_id = "ana".into();
    w.push("ana", SessionEvent::WorkspaceHostsUpdated { hosts: vec![mine.clone(), theirs] });
    let r = readers(&w);
    assert_eq!(r.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), [mine.id.as_str()]);
}

/// Offers are sticky: an older writer's upsert (no field) keeps them; an
/// explicit empty list ("Don't list this computer's tools") clears them.
#[test]
fn offers_are_sticky_across_older_writers() {
    let mut w = world();
    let listed = vec![offer("claude-code", OfferKind::Cli, "claudeCode", ""), offer("gw-home", OfferKind::Gateway, "", "openai")];
    w.push_bare("cy", SessionEvent::WorkspaceHostUpserted { host: host_of(&w, "cy", "cy", Some(listed.clone())) });
    // A 0.4 build on the same device rewrites its record without the field.
    let old = host_of(&w, "cy", "cy", None);
    let wire = serde_json::to_value(&old).unwrap();
    assert!(wire.get("offers").is_none(), "an unset field isn't written (0.4 shape)");
    w.push_bare("cy", SessionEvent::WorkspaceHostUpserted { host: old });
    let r = readers(&w);
    assert_eq!(r[0].offers.as_deref(), Some(listed.as_slice()), "an older writer erased the offers");
    w.push_bare("cy", SessionEvent::WorkspaceHostUpserted { host: host_of(&w, "cy", "cy", Some(vec![])) });
    assert_eq!(readers(&w)[0].offers.as_deref(), Some(&[][..]));
    assert!(readers(&w)[0].listed_offers().is_empty());
}

/// A 0.4 reader decodes a 0.5 host record (the new field is ignored) and a
/// 0.5 reader decodes a 0.4 one: offers are additive.
#[test]
fn offers_are_additive_on_the_wire() {
    let mut h = WorkspaceHost::new("h", HostKind::Worker, "box");
    h.model_gateways = vec![HostModelGateway { name: "gw-home".into(), api: "anthropic".into() }];
    h.offers = Some(vec![offer("codex", OfferKind::Cli, "codex", "")]);
    let j = serde_json::to_value(&h).unwrap();
    assert_eq!(j["offers"][0]["kind"], "cli");
    // A newer build's unknown kind decodes and is never offered.
    let newer: WorkspaceHost = serde_json::from_value(serde_json::json!({
        "id": "h", "kind": "worker", "label": "box",
        "offers": [{"name": "x-new", "kind": "somethingNew"}, {"name": "codex", "kind": "cli", "harness": "codex"}]
    }))
    .unwrap();
    assert_eq!(newer.listed_offers().iter().map(|o| o.name.as_str()).collect::<Vec<_>>(), ["codex"]);
    // A record from before offers lists its gateways, on their own wire.
    h.offers = None;
    let legacy = h.listed_offers();
    assert_eq!(legacy, [offer("gw-home", OfferKind::Gateway, "", "anthropic")]);
}

/// H6: a gateway offer's harness comes from the host's own wire; nothing
/// but `openai` or `anthropic` is a gateway wire.
#[test]
fn a_gateway_offer_carries_its_own_wire() {
    assert!(offer("gw", OfferKind::Gateway, "", "anthropic").well_formed());
    assert!(offer("gw", OfferKind::Gateway, "", "openai").well_formed());
    assert!(!offer("gw", OfferKind::Gateway, "", "").well_formed());
    assert!(!offer("gw", OfferKind::Gateway, "", "openAI-but-anthropic").well_formed());
}

fn key() -> SigningKeypair {
    SigningKeypair::from_seed(&[7u8; 32]).unwrap()
}

const WS: Uuid = Uuid::from_u128(0x5eed);
const DEVICE: Uuid = Uuid::from_u128(0xd0d0);

fn roster_with(k: &SigningKeypair) -> RosterState {
    let mut r = RosterState::default();
    r.devices.insert(DEVICE, DeviceState { accounts: BTreeSet::from([Uuid::from_u128(1)]), public_key: k.public_key_bytes().to_vec(), ..Default::default() });
    r.members.insert("ada".into(), MemberState { role: WorkspaceRole::Contributor, account_id: Some(Uuid::from_u128(1)), kind: ActorKind::Human });
    r
}

fn status() -> HostStatus {
    HostStatus {
        build: "0.5.0".into(),
        offers: vec![
            OfferStatus { name: "claude-code".into(), ready: true, models: vec!["sonnet".into(), "opus".into()], ..Default::default() },
            OfferStatus { name: "codex".into(), ready: false, why: "not signed in".into(), ..Default::default() },
        ],
        refuses: vec![],
    }
}

/// The status rides the beat signed by its host's device; anything else
/// (unsigned, another key, another workspace, another host's slot,
/// tampered) is ignored.
#[test]
fn a_status_counts_only_signed_by_its_own_host() {
    let k = key();
    let r = roster_with(&k);
    let mut beat = HostBeat { host_id: DEVICE.to_string(), at: 1, seq: 42, ..Default::default() };
    beat.sign_status(WS, DEVICE, status(), &k);
    assert_eq!(beat.status_verified(WS, &r), Some(status()));
    assert!(beat.status_verified(Uuid::from_u128(9), &r).is_none(), "another workspace");
    let unsigned = HostBeat { status_sig: String::new(), ..beat.clone() };
    assert!(unsigned.status_verified(WS, &r).is_none());
    let mut forged = beat.clone();
    forged.sign_status(WS, DEVICE, HostStatus::default(), &SigningKeypair::from_seed(&[1u8; 32]).unwrap());
    assert!(forged.status_verified(WS, &r).is_none(), "another member's key");
    let mut tampered = beat.clone();
    tampered.status = tampered.status.replace("0.5.0", "9.9.9");
    assert!(tampered.status_verified(WS, &r).is_none());
    let replayed = HostBeat { seq: 43, ..beat.clone() };
    assert!(replayed.status_verified(WS, &r).is_none(), "the seq is signed");
    let other = HostBeat { host_id: Uuid::from_u128(3).to_string(), ..beat.clone() };
    assert!(other.status_verified(WS, &r).is_none());
    // A newer build's extra fields still verify: the signed bytes are kept.
    let mut newer = HostBeat { host_id: DEVICE.to_string(), at: 1, seq: 50, ..Default::default() };
    newer.status = r#"{"build":"0.6.0","offers":[],"future":{"x":1}}"#.into();
    newer.status_sig = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(k.sign(&status_message(WS, DEVICE, 50, &newer.status)))
    };
    assert_eq!(newer.status_verified(WS, &r).map(|s| s.build), Some("0.6.0".into()));
    // An older reader ignores both fields (HostBeat with no status).
    let wire = serde_json::to_value(&HostBeat { host_id: "h".into(), at: 1, ..Default::default() }).unwrap();
    assert!(wire.get("status").is_none() && wire.get("statusSig").is_none(), "a status-less beat is byte-identical to before");
}

/// A status over the bound drops its model lists first.
#[test]
fn an_oversized_status_drops_its_model_lists() {
    let big = HostStatus {
        build: "0.5.0".into(),
        offers: (0..32)
            .map(|i| OfferStatus { name: format!("src-{i}"), ready: true, models: (0..200).map(|m| format!("model-{m:03}-long-name")).collect(), complete: true, ..Default::default() })
            .collect(),
        refuses: vec![],
    }
    .sanitized();
    assert!(big.offers.iter().all(|o| o.models.is_empty() && !o.complete));
    assert_eq!(big.offers.len(), 32, "readiness survives");
    let one = HostStatus { offers: vec![OfferStatus { name: "a".into(), models: (0..100).map(|m| m.to_string()).collect(), complete: true, ..Default::default() }], ..Default::default() }.sanitized();
    assert_eq!(one.offers[0].models.len(), MAX_STATUS_MODELS);
    assert!(!one.offers[0].complete, "a cut list isn't complete");
}

/// The strings H7 forbids: addresses, userinfo, key prefixes.
fn sensitive() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z]{2,8}://[a-z0-9.]{1,20}(:[0-9]{2,5})?(/[a-z]{0,8})?",
        "[a-z]{1,8}:[a-zA-Z0-9]{1,12}@[a-z0-9.]{1,20}",
        "[a-z]{1,8}@[a-z]{1,10}\\.[a-z]{2,4}",
        "(sk-|sk-ant-|ghp_|github_pat_|xoxb-|AKIA|glpat-)[A-Za-z0-9]{8,30}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// H7: whatever a host's configuration holds, no address, userinfo or
    /// key survives into an offer a reader uses or a status it opens.
    #[test]
    fn no_address_or_key_survives_into_offers_or_status(bad in sensitive(), name in "[a-z][a-z0-9-]{0,20}[a-z0-9]") {
        prop_assert!(looks_sensitive(&bad), "{bad}");
        for o in [
            offer(&bad, OfferKind::Cli, "claudeCode", ""),
            offer(&name, OfferKind::Local, &bad, ""),
            offer(&name, OfferKind::Gateway, "", &bad),
        ] {
            prop_assert!(!o.well_formed(), "{o:?}");
        }
        let st = HostStatus {
            build: bad.clone(),
            offers: vec![OfferStatus { name: name.clone(), ready: false, why: bad.clone(), models: vec![bad.clone(), "sonnet".into()], complete: true }],
            refuses: vec![bad.clone()],
        }
        .sanitized();
        let text = serde_json::to_string(&st).unwrap();
        prop_assert!(!text.contains(&bad), "{text}");
        prop_assert!(!text.contains("://"));
        prop_assert_eq!(&st.offers[0].models, &vec!["sonnet".to_string()]);
        prop_assert!(!st.offers[0].complete);
        // A host record listing only well-formed offers carries none either.
        let mut h = WorkspaceHost::new(Uuid::new_v4().to_string(), HostKind::Worker, "box");
        h.offers = Some(vec![offer(&bad, OfferKind::Cli, "codex", ""), offer(&name, OfferKind::Cli, "codex", "")]);
        let listed = serde_json::to_string(&h.listed_offers()).unwrap();
        prop_assert!(!listed.contains(&bad));
    }
}

/// Ordinary model and source names aren't mistaken for secrets.
#[test]
fn ordinary_names_pass() {
    for s in ["sonnet", "llama3:8b", "org/model", "gpt-5.3-codex", "claude-sonnet-4@20250514", "qwen2.5-coder:7b", "gw-home", "claude-code"] {
        assert!(!looks_sensitive(s), "{s}");
    }
}
