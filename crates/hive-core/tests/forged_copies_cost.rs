//! What a fold costs when someone floods the signer directory (review of
//! fix/forged-equivocation). Account keys, rotations and certificates count
//! for telling copies apart whatever the fold decides about them, so anyone
//! able to deliver events (a self-certified stranger included) controls how
//! many there are. Telling copies apart must stay near-linear in them.

use hive_core::crypto::SigningKeypair;
use hive_core::events::project_config;
use hive_core::testkit::SignedWorld;
use hive_core::{SessionEvent, WorkspaceRole};

/// A stranger's chain of rotations of their own account, published newest
/// first (each later in the log than the one it succeeds), plus one copy
/// under a real id so the directory is built.
#[test]
fn a_reversed_rotation_chain_does_not_make_the_fold_cubic() {
    let mut w = SignedWorld::genesis(&["owner", "con", "eve"], None);
    w.admit("owner", "con", WorkspaceRole::Contributor);
    w.publish_identity("eve");
    let eve = w.people["eve"].account_id;
    let n = 160;
    let keys: Vec<SigningKeypair> = (0..n).map(|_| SigningKeypair::generate().unwrap()).collect();
    let signer = |i: usize| if i == 0 { &w.people["eve"].account } else { &keys[i - 1] };
    let rotations: Vec<SessionEvent> = (1..=n)
        .rev()
        .map(|i| {
            let new = keys[i - 1].public_key_bytes().to_vec();
            let succession_signature = signer(i - 1).sign(&new).to_vec();
            SessionEvent::AccountKeyRotated { account_id: eve, new_signing_public_key: new, succession_signature }
        })
        .collect();
    for r in rotations {
        w.push_bare("eve", r);
    }
    // A copy under a real id (eve's own event), so copies must be told apart.
    let target = w.cfg.last().unwrap().clone();
    let mut copy = w.draft("eve", None, SessionEvent::WorkspaceRulesSet { version: 2, presence: None }, None);
    copy.event_id = target.event_id;
    w.finish(&mut copy, "eve", None);
    w.push_env(None, copy);
    let t = std::time::Instant::now();
    let _ = project_config(&w.cfg, None, None);
    let took = t.elapsed();
    assert!(took < std::time::Duration::from_secs(5), "one config fold took {took:?}");
}
