//! Golden vectors for code-match approval (`ligus_core::match_code`,
//! `docs/design/code-match-approval.md` section 6.1), test M1.
//!
//! `golden/match_code_v1.jsonl`: one object per line, `t` says which kind:
//! `code` (commit, transcript, the 40-bit code and its shown form),
//! `crockford` (lenient decoding), `crockford-invalid`, `mailbox` (the HKDF
//! keys of one epoch), `preimage` (a message signature's bytes), `register`
//! and `member` (the relay auth preimages). The values were computed
//! independently (Python `hashlib` and `hmac`, standard library only). The
//! phone repositories carry a byte-for-byte copy. Existing lines are frozen.

use ligus_core::match_code::{self, MailboxKeys, MatchKind, Transcript};
use uuid::Uuid;

const GOLDEN: &str = include_str!("golden/match_code_v1.jsonl");

fn unhex(s: &str) -> Vec<u8> {
    match_code::unhex(s).expect("hex")
}
fn arr<const N: usize>(s: &str) -> [u8; N] {
    unhex(s).try_into().expect("length")
}

#[test]
fn match_code_vectors_hold() {
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    for line in GOLDEN.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let s = |k: &str| v[k].as_str().unwrap_or_else(|| panic!("{k}")).to_string();
        let label = s("label");
        let t = s("t");
        match t.as_str() {
            "code" => {
                let rid: [u8; 16] = arr(&s("rid"));
                let n_r: [u8; 32] = arr(&s("nR"));
                let commit = match_code::commit(&rid, &n_r);
                assert_eq!(match_code::hex(&commit), s("commit"), "{label}: commit");
                let mailbox: [u8; 16] = arr(&s("mailbox"));
                let n_a: [u8; 32] = arr(&s("nA"));
                let code = s("requestCode");
                let rka = unhex(&s("requesterKa"));
                let ask = unhex(&s("approverSigKey"));
                let aka = unhex(&s("approverKa"));
                let tr = Transcript {
                    kind: MatchKind::parse(&s("kind")).unwrap(),
                    mailbox: &mailbox,
                    rid: &rid,
                    request_code: code.as_bytes(),
                    requester_ka: &rka,
                    commit: &commit,
                    approver_device_id: Uuid::parse_str(&s("approverDeviceId")).unwrap(),
                    approver_sig_key: &ask,
                    approver_ka: &aka,
                    n_a: &n_a,
                }
                .digest();
                assert_eq!(match_code::hex(&tr), s("transcript"), "{label}: transcript");
                let c = match_code::code40(&tr, &n_r);
                assert_eq!(format!("{c:010x}"), s("code40"), "{label}: code40");
                assert_eq!(match_code::encode(c), s("shown"), "{label}: shown");
            }
            "crockford" => {
                let want = u64::from_str_radix(&s("code40"), 16).unwrap();
                assert_eq!(match_code::decode(&s("input")), Some(want), "{label}");
                assert_eq!(match_code::encode(want), s("shown"), "{label}");
            }
            "crockford-invalid" => assert_eq!(match_code::decode(&s("input")), None, "{label}"),
            "mailbox" => {
                let ek: [u8; 32] = arr(&s("epochKey"));
                let k = MailboxKeys::derive(&ek);
                assert_eq!(match_code::hex(&k.auth_seed), s("authSeed"), "{label}");
                assert_eq!(match_code::hex(&k.seal_seed), s("sealSeed"), "{label}");
                assert_eq!(match_code::hex(&match_code::put_cap(&ek, 0)), s("putCap0"), "{label}");
                assert_eq!(match_code::hex(&match_code::put_cap(&ek, 1)), s("putCap1"), "{label}");
            }
            "preimage" => {
                let p = match_code::message_preimage(&s("type"), &s("body")).unwrap();
                assert_eq!(match_code::hex(&p), s("preimage"), "{label}");
            }
            "register" => {
                let p = match_code::register_preimage(
                    &arr(&s("mailbox")),
                    &arr(&s("putCapHash")),
                    v["counter"].as_u64().unwrap() as u32,
                );
                assert_eq!(match_code::hex(&p), s("preimage"), "{label}");
            }
            "member" => {
                let p = match_code::member_preimage(&s("method"), &s("path"), v["ts"].as_i64().unwrap(), s("body").as_bytes());
                assert_eq!(match_code::hex(&p), s("preimage"), "{label}");
            }
            other => panic!("{label}: unknown t {other}"),
        }
        *seen.entry(t).or_default() += 1;
    }
    for t in ["code", "crockford", "crockford-invalid", "mailbox", "preimage", "register", "member"] {
        assert!(seen.get(t).copied().unwrap_or(0) > 0, "no {t} vector");
    }
}
