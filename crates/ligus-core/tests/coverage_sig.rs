//! Signed coverage (repos review residual 1; design repos-and-checkouts
//! 2.8): a host signs its own coverage report with its device key, and a
//! reader uses a report only when it verifies against the roster. Golden
//! vectors pin the preimage (`ligus-coverage-v1\0`) and the signature; the
//! properties run at `PROPTEST_CASES=4096` in release on the CI box.

use std::collections::BTreeSet;

use ligus_core::authz::timeline::{DeviceState, MemberState};
use ligus_core::authz::RosterState;
use ligus_core::workspace_repo::{coverage_message, CoverageEntry, HostBeat};
use ligus_core::{ActorKind, SigningKeypair, WorkspaceRole};
use proptest::prelude::*;
use uuid::Uuid;

const WS: Uuid = Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888);
const DEVICE: Uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
const ACCOUNT: Uuid = Uuid::from_u128(0xacc0);

fn key() -> SigningKeypair {
    SigningKeypair::from_seed(&[9u8; 32]).unwrap()
}

fn golden_entries() -> Vec<CoverageEntry> {
    vec![
        CoverageEntry {
            repo_id: Uuid::from_u128(0xa),
            state: "present".into(),
            code: String::new(),
            via_fork: false,
        },
        CoverageEntry {
            repo_id: Uuid::from_u128(0xb),
            state: "error".into(),
            code: "clone_failed".into(),
            via_fork: true,
        },
    ]
}

fn roster_with(key: &SigningKeypair) -> RosterState {
    let mut r = RosterState::default();
    r.devices.insert(
        DEVICE,
        DeviceState {
            accounts: BTreeSet::from([ACCOUNT]),
            public_key: key.public_key_bytes().to_vec(),
            ..Default::default()
        },
    );
    r.members.insert(
        "ada".into(),
        MemberState {
            role: WorkspaceRole::Contributor,
            account_id: Some(ACCOUNT),
            kind: ActorKind::Human,
        },
    );
    r
}

fn signed(seq: u64, coverage: Vec<CoverageEntry>) -> HostBeat {
    let mut b = HostBeat {
        host_id: DEVICE.to_string(),
        at: 1,
        seq,
        coverage,
        ..Default::default()
    };
    b.sign_coverage(WS, DEVICE, &key());
    b
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The frozen preimage and signature (Ed25519 is deterministic).
#[test]
fn coverage_signature_golden_vector() {
    let msg = coverage_message(WS, DEVICE, 1_700_000_000_000, &golden_entries());
    assert_eq!(hex(&msg), GOLDEN_MESSAGE_HEX);
    let beat = signed(1_700_000_000_000, golden_entries());
    assert_eq!(beat.coverage_sig, GOLDEN_SIG_B64);
    assert_eq!(
        serde_json::to_value(&beat).unwrap()["coverageSig"],
        GOLDEN_SIG_B64
    );
    assert!(beat.coverage_verified(WS, &roster_with(&key())));
}

const GOLDEN_MESSAGE_HEX: &str = "6c696775732d636f7665726167652d763100111122223333444455556666777788880123456789abcdef0123456789abcdef0068e5cf8b010000020000000000000000000000000000000000000a0700000070726573656e7400000000000000000000000000000000000000000b050000006572726f720c000000636c6f6e655f6661696c656401";
const GOLDEN_SIG_B64: &str =
    "vM81a4v9uO7jx11SrIrTRlpEt/jXLbk6PgHCc2PaNHlkmZ1Q7s+go+ERvIxgDUoadGwVW9m4OsB9Z5ku87VGCg==";

/// Every way a report fails: unsigned, another member's key, another
/// workspace, another host's slot, tampered, and a device the roster no
/// longer vouches for (revoked, a phone, unknown, or acting for no member).
#[test]
fn unsigned_forged_or_unvouched_coverage_is_ignored() {
    let k = key();
    let r = roster_with(&k);
    let good = signed(5, golden_entries());
    assert!(good.coverage_verified(WS, &r));
    let unsigned = HostBeat {
        coverage_sig: String::new(),
        ..good.clone()
    };
    assert!(!unsigned.coverage_verified(WS, &r));
    // A member forging this host's "missing" with its own key.
    let mut forged = HostBeat {
        coverage: vec![CoverageEntry {
            repo_id: Uuid::from_u128(0xa),
            state: "missing".into(),
            code: String::new(),
            via_fork: false,
        }],
        ..good.clone()
    };
    forged.sign_coverage(WS, DEVICE, &SigningKeypair::from_seed(&[1u8; 32]).unwrap());
    assert!(!forged.coverage_verified(WS, &r));
    assert!(
        !good.coverage_verified(Uuid::from_u128(7), &r),
        "another workspace"
    );
    let other_slot = HostBeat {
        host_id: Uuid::from_u128(3).to_string(),
        ..good.clone()
    };
    assert!(
        !other_slot.coverage_verified(WS, &r),
        "another host's report"
    );
    let mut tampered = good.clone();
    tampered.coverage[0].state = "missing".into();
    assert!(!tampered.coverage_verified(WS, &r));
    let replay_seq = HostBeat {
        seq: 6,
        ..good.clone()
    };
    assert!(!replay_seq.coverage_verified(WS, &r), "the seq is signed");
    let garbage = HostBeat {
        coverage_sig: "!!".into(),
        ..good.clone()
    };
    assert!(!garbage.coverage_verified(WS, &r));
    for f in [
        |r: &mut RosterState| r.devices.get_mut(&DEVICE).unwrap().revoked = true,
        |r: &mut RosterState| r.devices.get_mut(&DEVICE).unwrap().phone = true,
        |r: &mut RosterState| {
            r.devices.remove(&DEVICE);
        },
        |r: &mut RosterState| r.members.clear(),
        |r: &mut RosterState| r.devices.get_mut(&DEVICE).unwrap().accounts.clear(),
    ] {
        let mut r2 = r.clone();
        f(&mut r2);
        assert!(!good.coverage_verified(WS, &r2));
    }
    // The bounds still hold for a signed report.
    let mut big = signed(
        5,
        (0..65)
            .map(|i| CoverageEntry {
                repo_id: Uuid::from_u128(i),
                state: "present".into(),
                code: String::new(),
                via_fork: false,
            })
            .collect(),
    );
    assert!(!big.coverage_verified(WS, &r));
    big.coverage.truncate(64);
    big.sign_coverage(WS, DEVICE, &k);
    assert!(big.coverage_verified(WS, &r));
}

fn entry() -> impl Strategy<Value = CoverageEntry> {
    (
        any::<u128>(),
        prop::sample::select(vec!["present", "cloning", "missing", "error", "x"]),
        "[a-z_]{0,12}",
        any::<bool>(),
    )
        .prop_map(|(id, st, code, via_fork)| CoverageEntry {
            repo_id: Uuid::from_u128(id),
            state: st.into(),
            code,
            via_fork,
        })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256), ..ProptestConfig::default() })]

    /// A report verifies iff it is exactly what the host signed: any change
    /// to an entry, the order, the seq, the workspace or the host, or a
    /// signature by any other key, is ignored.
    #[test]
    fn prop_coverage_verifies_only_as_signed(
        cov in prop::collection::vec(entry(), 1..8),
        seq in any::<u64>(),
        tamper in 0usize..7,
        pick in any::<prop::sample::Index>(),
        other_seed in any::<[u8; 32]>(),
    ) {
        let k = key();
        let r = roster_with(&k);
        let good = signed(seq, cov.clone());
        prop_assert!(good.coverage_verified(WS, &r));
        let i = pick.index(cov.len());
        let mut b = good.clone();
        let mut ws = WS;
        let changed = match tamper {
            0 => { b.coverage[i].via_fork = !b.coverage[i].via_fork; true }
            1 => { b.coverage[i].code.push('x'); true }
            2 => { b.seq = b.seq.wrapping_add(1); true }
            3 => { ws = Uuid::from_u128(ws.as_u128() ^ 1); true }
            4 => { b.coverage[i].repo_id = Uuid::from_u128(b.coverage[i].repo_id.as_u128() ^ 1); true }
            5 => {
                let other = SigningKeypair::from_seed(&other_seed).unwrap();
                b.sign_coverage(WS, DEVICE, &other);
                other.public_key_bytes() != k.public_key_bytes()
            }
            _ => {
                // Moving one entry's bytes into the next: the lengths are prefixed.
                if b.coverage.len() > 1 { b.coverage.swap(0, 1); b.coverage[0] != b.coverage[1] } else { false }
            }
        };
        prop_assert_eq!(b.coverage_verified(ws, &r), !changed);
    }
}
