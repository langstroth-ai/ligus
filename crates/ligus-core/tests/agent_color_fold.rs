//! An agent's accent color is synced in the roster and lands in every
//! viewer's CSS, so the fold keeps only a plain hex color: anything else
//! (a `url(...)` that would load a remote image, a gradient) folds to `None`
//! and the client falls back to its derived color.

use ligus_core::agent::is_valid_color_hex;
use ligus_core::testkit::{Proof, SignedWorld};
use ligus_core::{SessionEvent, WorkspaceAgent, WorkspaceRole};
use uuid::Uuid;

fn world() -> SignedWorld {
    let mut w = SignedWorld::genesis(&["owner", "mara"], None);
    w.admit("owner", "mara", WorkspaceRole::Contributor);
    w
}

fn agent(id: u128, color: &str) -> WorkspaceAgent {
    let mut a = WorkspaceAgent::new("scout", "rt");
    a.id = Uuid::from_u128(id);
    a.owner_actor_id = "mara".into();
    a.created_at = ligus_core::Timestamp::from_unix_seconds(0);
    a.avatar_color_hex = Some(color.into());
    a
}

fn color(w: &SignedWorld, id: u128) -> Option<String> {
    w.config()
        .workspace_agents
        .into_iter()
        .find(|a| a.id == Uuid::from_u128(id))
        .expect("agent folded")
        .avatar_color_hex
}

const BAD: &[&str] = &[
    "url(https://evil.example/p.gif)",
    "#fff url(https://evil.example/p.gif)",
    "red",
    "#12",
    "#123456789",
    "#ggg",
    "",
    " #fff",
];

#[test]
fn only_plain_hex_colors_are_valid() {
    for ok in ["#fff", "#FFF", "#3f72a8", "#3f72a8cc", "#abcd"] {
        assert!(is_valid_color_hex(ok), "{ok}");
    }
    for bad in BAD {
        assert!(!is_valid_color_hex(bad), "{bad}");
    }
}

#[test]
fn an_upserted_agent_with_a_non_hex_color_folds_without_it() {
    let mut w = world();
    w.write(
        "mara",
        None,
        SessionEvent::AgentUpserted {
            agent: agent(1, "#3f72a8"),
        },
        None,
        Proof::Recovery,
    );
    assert_eq!(color(&w, 1).as_deref(), Some("#3f72a8"));
    for (i, bad) in BAD.iter().enumerate() {
        let id = 10 + i as u128;
        w.write(
            "mara",
            None,
            SessionEvent::AgentUpserted {
                agent: agent(id, bad),
            },
            None,
            Proof::Recovery,
        );
        assert_eq!(color(&w, id), None, "{bad}");
    }
}

#[test]
fn a_roster_list_with_a_non_hex_color_folds_without_it() {
    let mut w = world();
    let list = vec![
        agent(1, "#abc"),
        agent(2, "url(https://evil.example/p.gif)"),
    ];
    w.write(
        "owner",
        None,
        SessionEvent::AgentRosterUpdated { agents: list },
        None,
        Proof::Recovery,
    );
    assert_eq!(color(&w, 1).as_deref(), Some("#abc"));
    assert_eq!(color(&w, 2), None);
}
