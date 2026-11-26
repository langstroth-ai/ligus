//! Code-match approval: the short code both screens show when one device
//! approves another (`docs/design/code-match-approval.md`, section 6), and
//! the pieces of the approval mailbox every client derives the same way
//! (section 5.1).
//!
//! Everything here is pure: hashing, HKDF, Crockford base32 and the signed
//! message envelope. The transport (the relay mailbox) and the flows live in
//! `ligus_runtime::approval_mailbox`. The phone apps implement the same
//! functions and pass the same golden vectors
//! (`tests/golden/match_code_v1.jsonl`, copied byte for byte).
//!
//! ```text
//! commit     = SHA256("ligus-match-commit-v1\0" ‖ rid ‖ nR)
//! transcript = SHA256("ligus-match-transcript-v1\0"
//!                ‖ u8 kind ‖ mailbox ‖ rid ‖ SHA256(request code bytes)
//!                ‖ put(requesterKa) ‖ commit
//!                ‖ approverDeviceId ‖ put(approverSigKey) ‖ put(approverKa) ‖ nA)
//! code40     = first 40 bits of SHA256("ligus-match-code-v1\0" ‖ transcript ‖ nR)
//! shown      = Crockford base32 of code40, 8 symbols, "K7Q4 M9X2"
//! ```
//!
//! All the byte strings below are wire ids: never rename them with the
//! product. They are pinned as hex in the tests.

use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use uuid::Uuid;

// FROZEN contexts (new in 0.4).
pub const COMMIT_CTX: &[u8] = b"ligus-match-commit-v1\0";
pub const TRANSCRIPT_CTX: &[u8] = b"ligus-match-transcript-v1\0";
pub const CODE_CTX: &[u8] = b"ligus-match-code-v1\0";
pub const REQUEST_CTX: &[u8] = b"ligus-match-request-v1\0";
pub const CLAIM_CTX: &[u8] = b"ligus-match-claim-v1\0";
pub const OPEN_CTX: &[u8] = b"ligus-match-open-v1\0";
pub const REVEAL_CTX: &[u8] = b"ligus-match-reveal-v1\0";
pub const RESULT_CTX: &[u8] = b"ligus-match-result-v1\0";
pub const ABORT_CTX: &[u8] = b"ligus-match-abort-v1\0";
/// A computer link's offer: the sealed grant's digest, before it is sent.
pub const OFFER_CTX: &[u8] = b"ligus-match-offer-v1\0";
/// The requester's acknowledgement of an offer.
pub const ACK_CTX: &[u8] = b"ligus-match-ack-v1\0";
/// HKDF salt of every approval-mailbox key.
pub const MAILBOX_SALT: &[u8] = b"ligus-approval-mailbox-v1";
/// What a mailbox register or reset is signed over (by the mailbox auth key).
pub const MAILBOX_REGISTER_CTX: &[u8] = b"ligus-mailbox-register-v1\0";
/// What a member's mailbox read or write is signed over (by the mailbox auth key).
pub const MAILBOX_MEMBER_CTX: &[u8] = b"ligus-mailbox-member-v1\0";

/// Crockford base32, no I, L, O or U.
pub const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Bits in a shown code.
pub const CODE_BITS: u32 = 40;
/// How long a code is valid after the approver opened the request.
pub const CODE_TTL_MS: i64 = 5 * 60 * 1000;
/// How long a request lives in the mailbox.
pub const REQUEST_TTL_MS: i64 = 15 * 60 * 1000;
/// How long the desktop holds "Codes match" disabled after the code appears.
pub const HOLD_MS: i64 = 3_000;

/// What a request asks for. The number goes in the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchKind {
    /// A `ligus serve` or `ligus worker` box asking to join (`ligusjoin1:`).
    Box,
    /// A computer asking to be one of a person's devices (`liguslink1:`).
    Computer,
    /// A phone asking to be one of a person's devices (`ligusphone1:`).
    Phone,
}

impl MatchKind {
    pub fn byte(self) -> u8 {
        match self {
            Self::Box => 1,
            Self::Computer => 2,
            Self::Phone => 3,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Box => "box",
            Self::Computer => "computer",
            Self::Phone => "phone",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "box" => Some(Self::Box),
            "computer" => Some(Self::Computer),
            "phone" => Some(Self::Phone),
            _ => None,
        }
    }
}

fn put(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u32).to_le_bytes());
    h.update(bytes);
}

/// `SHA256("ligus-match-commit-v1\0" ‖ rid ‖ nR)`.
pub fn commit(rid: &[u8; 16], n_r: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(COMMIT_CTX);
    h.update(rid);
    h.update(n_r);
    h.finalize().into()
}

/// Everything the transcript covers.
#[derive(Debug, Clone, Copy)]
pub struct Transcript<'a> {
    pub kind: MatchKind,
    pub mailbox: &'a [u8; 16],
    pub rid: &'a [u8; 16],
    /// The request's code text (`ligusjoin1:…`, `liguslink1:…`, `ligusphone1:…`) as UTF-8 bytes.
    pub request_code: &'a [u8],
    pub requester_ka: &'a [u8],
    pub commit: &'a [u8; 32],
    pub approver_device_id: Uuid,
    /// Ed25519 (32 bytes) or P-256 (65 bytes SEC1 uncompressed).
    pub approver_sig_key: &'a [u8],
    pub approver_ka: &'a [u8],
    pub n_a: &'a [u8; 32],
}

impl Transcript<'_> {
    /// The transcript digest (section 6.1).
    pub fn digest(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(TRANSCRIPT_CTX);
        h.update([self.kind.byte()]);
        h.update(self.mailbox);
        h.update(self.rid);
        h.update(Sha256::digest(self.request_code));
        put(&mut h, self.requester_ka);
        h.update(self.commit);
        h.update(self.approver_device_id.as_bytes());
        put(&mut h, self.approver_sig_key);
        put(&mut h, self.approver_ka);
        h.update(self.n_a);
        h.finalize().into()
    }
}

/// The full code hash: `SHA256("ligus-match-code-v1\0" ‖ transcript ‖ nR)`.
fn code_hash(transcript: &[u8; 32], n_r: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(CODE_CTX);
    h.update(transcript);
    h.update(n_r);
    h.finalize().into()
}

/// The first `bits` bits (1..=64) of the code hash, big-endian. Shown codes
/// use [`CODE_BITS`]; tests truncate further to measure collision rates.
pub fn code_bits(transcript: &[u8; 32], n_r: &[u8; 32], bits: u32) -> u64 {
    assert!((1..=64).contains(&bits));
    let d = code_hash(transcript, n_r);
    let mut first = [0u8; 8];
    first.copy_from_slice(&d[..8]);
    u64::from_be_bytes(first) >> (64 - bits)
}

/// The 40-bit code.
pub fn code40(transcript: &[u8; 32], n_r: &[u8; 32]) -> u64 {
    code_bits(transcript, n_r, CODE_BITS)
}

/// The shown form: 8 Crockford symbols in two groups of four, `K7Q4 M9X2`.
pub fn encode(code40: u64) -> String {
    let compact = encode_compact(code40);
    format!("{} {}", &compact[..4], &compact[4..])
}

/// The 8 symbols with no space (`K7Q4M9X2`), as `--code` takes it.
pub fn encode_compact(code40: u64) -> String {
    (0..8)
        .rev()
        .map(|i| ALPHABET[((code40 >> (i * 5)) & 31) as usize] as char)
        .collect()
}

/// Read a typed code: case is ignored, `I` and `L` read as 1, `O` as 0, and
/// spaces and hyphens are skipped. Exactly 8 symbols, or `None`.
pub fn decode(typed: &str) -> Option<u64> {
    let mut v: u64 = 0;
    let mut n = 0;
    for c in typed.chars() {
        if c == ' ' || c == '-' {
            continue;
        }
        let c = match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            c => c,
        };
        let d = ALPHABET.iter().position(|&a| a as char == c)? as u64;
        n += 1;
        if n > 8 {
            return None;
        }
        v = (v << 5) | d;
    }
    (n == 8).then_some(v)
}

/// What a screen reader reads: `K, 7, Q, 4. M, 9, X, 2.` (the desktop's
/// `aria-label` is `Code: ` followed by this).
pub fn spell(code40: u64) -> String {
    let c = encode_compact(code40);
    let group = |s: &str| {
        s.chars()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("{}. {}.", group(&c[..4]), group(&c[4..]))
}

/// Two decoys for number matching on the phone (decision D2): distinct from
/// `real` and from each other, drawn from fresh randomness.
pub fn decoys(real: u64) -> [u64; 2] {
    let mut out = [real; 2];
    let mut i = 0;
    while i < 2 {
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).expect("randomness");
        let c = u64::from_be_bytes(b) >> (64 - CODE_BITS);
        if c != real && (i == 0 || c != out[0]) {
            out[i] = c;
            i += 1;
        }
    }
    out
}

/// The approval-mailbox keys every member derives from one epoch's
/// workspace key (section 5.1).
#[derive(Clone)]
pub struct MailboxKeys {
    /// Ed25519 seed of the mailbox auth key (`info = "auth"`).
    pub auth_seed: [u8; 32],
    /// X25519 seed of the mailbox seal key (`info = "seal"`).
    pub seal_seed: [u8; 32],
}

impl std::fmt::Debug for MailboxKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MailboxKeys(..)")
    }
}

fn hkdf32(ikm: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(MAILBOX_SALT), ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm).expect("hkdf expand 32");
    okm
}

impl MailboxKeys {
    /// Derive from the 32-byte workspace key of one epoch.
    pub fn derive(epoch_key: &[u8; 32]) -> Self {
        Self {
            auth_seed: hkdf32(epoch_key, b"auth"),
            seal_seed: hkdf32(epoch_key, b"seal"),
        }
    }

    pub fn auth(&self) -> crate::crypto::SigningKeypair {
        crate::crypto::SigningKeypair::from_seed(&self.auth_seed).expect("32-byte seed")
    }

    pub fn seal(&self) -> crate::e2ee::KeyAgreementKeypair {
        crate::e2ee::KeyAgreementKeypair::from_seed(&self.seal_seed).expect("32-byte seed")
    }

    /// The mailbox id: the first 16 bytes of `SHA256(authPub)`.
    pub fn mailbox(&self) -> [u8; 16] {
        mailbox_id(&self.auth().public_key_bytes())
    }
}

/// The mailbox id an auth key names (the relay checks it the same way).
pub fn mailbox_id(auth_pub: &[u8]) -> [u8; 16] {
    let d = Sha256::digest(auth_pub);
    let mut out = [0u8; 16];
    out.copy_from_slice(&d[..16]);
    out
}

/// The put capability for reset counter `counter`:
/// `HKDF(epoch key, salt, "put" ‖ u32le(counter))[..16]`.
pub fn put_cap(epoch_key: &[u8; 32], counter: u32) -> [u8; 16] {
    let mut info = b"put".to_vec();
    info.extend_from_slice(&counter.to_le_bytes());
    let full = hkdf32(epoch_key, &info);
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

/// What a mailbox register or reset signs:
/// `ctx ‖ mailbox ‖ SHA256(putCap) ‖ u32le(counter)`.
pub fn register_preimage(mailbox: &[u8; 16], put_cap_hash: &[u8; 32], counter: u32) -> Vec<u8> {
    let mut out = MAILBOX_REGISTER_CTX.to_vec();
    out.extend_from_slice(mailbox);
    out.extend_from_slice(put_cap_hash);
    out.extend_from_slice(&counter.to_le_bytes());
    out
}

/// What a member's mailbox call signs:
/// `ctx ‖ method ‖ 0 ‖ path ‖ 0 ‖ i64le(ts ms) ‖ SHA256(body)`, `path` without
/// the query string.
pub fn member_preimage(method: &str, path: &str, ts_ms: i64, body: &[u8]) -> Vec<u8> {
    let mut out = MAILBOX_MEMBER_CTX.to_vec();
    out.extend_from_slice(method.as_bytes());
    out.push(0);
    out.extend_from_slice(path.as_bytes());
    out.push(0);
    out.extend_from_slice(&ts_ms.to_le_bytes());
    out.extend_from_slice(&Sha256::digest(body));
    out
}

/// The context a message type is signed under.
pub fn message_ctx(t: &str) -> Option<&'static [u8]> {
    Some(match t {
        "request" => REQUEST_CTX,
        "claim" => CLAIM_CTX,
        "open" => OPEN_CTX,
        "reveal" => REVEAL_CTX,
        "result" => RESULT_CTX,
        "abort" => ABORT_CTX,
        "offer" => OFFER_CTX,
        "ack" => ACK_CTX,
        _ => return None,
    })
}

/// What a message's signature covers: its type's context, then the exact
/// UTF-8 bytes of its `body` string (section 5.2; the envelope keeps the body
/// as a string so no JSON canonicalisation is needed across languages).
pub fn message_preimage(t: &str, body: &str) -> Option<Vec<u8>> {
    let mut out = message_ctx(t)?.to_vec();
    out.extend_from_slice(body.as_bytes());
    Some(out)
}

/// Lowercase hex.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// Bytes from lowercase or uppercase hex.
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rand<const N: usize>() -> [u8; N] {
        let mut b = [0u8; N];
        getrandom::fill(&mut b).unwrap();
        b
    }

    #[test]
    fn match_constants_are_frozen() {
        assert_eq!(
            hex(COMMIT_CTX),
            "6c696775732d6d617463682d636f6d6d69742d763100"
        );
        assert_eq!(
            hex(TRANSCRIPT_CTX),
            "6c696775732d6d617463682d7472616e7363726970742d763100"
        );
        assert_eq!(hex(CODE_CTX), "6c696775732d6d617463682d636f64652d763100");
        assert_eq!(
            hex(REQUEST_CTX),
            "6c696775732d6d617463682d726571756573742d763100"
        );
        assert_eq!(hex(CLAIM_CTX), "6c696775732d6d617463682d636c61696d2d763100");
        assert_eq!(hex(OPEN_CTX), "6c696775732d6d617463682d6f70656e2d763100");
        assert_eq!(
            hex(REVEAL_CTX),
            "6c696775732d6d617463682d72657665616c2d763100"
        );
        assert_eq!(
            hex(RESULT_CTX),
            "6c696775732d6d617463682d726573756c742d763100"
        );
        assert_eq!(hex(ABORT_CTX), "6c696775732d6d617463682d61626f72742d763100");
        assert_eq!(hex(OFFER_CTX), "6c696775732d6d617463682d6f666665722d763100");
        assert_eq!(hex(ACK_CTX), "6c696775732d6d617463682d61636b2d763100");
        assert_eq!(MAILBOX_SALT, b"ligus-approval-mailbox-v1");
        assert_eq!(MAILBOX_REGISTER_CTX, b"ligus-mailbox-register-v1\0");
        assert_eq!(MAILBOX_MEMBER_CTX, b"ligus-mailbox-member-v1\0");
        assert_eq!(ALPHABET, b"0123456789ABCDEFGHJKMNPQRSTVWXYZ");
        assert_eq!(MatchKind::Box.byte(), 1);
        assert_eq!(MatchKind::Computer.byte(), 2);
        assert_eq!(MatchKind::Phone.byte(), 3);
    }

    #[test]
    fn crockford_round_trips_and_reads_lenient_input() {
        for v in [0u64, 1, 31, 32, (1 << 40) - 1, 0x12_3456_789a] {
            let s = encode(v);
            assert_eq!(s.len(), 9);
            assert_eq!(decode(&s), Some(v));
            assert_eq!(decode(&s.to_lowercase()), Some(v));
            assert_eq!(decode(&encode_compact(v)), Some(v));
        }
        assert_eq!(encode(0), "0000 0000");
        assert_eq!(encode((1 << 40) - 1), "ZZZZ ZZZZ");
        assert_eq!(decode("oooo-iiii"), decode("0000 1111"));
        assert_eq!(decode("LLLL 0000"), decode("1111 0000"));
        assert_eq!(decode("UUUU 0000"), None, "U is not a Crockford symbol");
        assert_eq!(decode("0000 000"), None);
        assert_eq!(decode("0000 00000"), None);
        assert_eq!(decode("0000 000!"), None);
        assert_eq!(
            spell(decode("K7Q4M9X2").unwrap()),
            "K, 7, Q, 4. M, 9, X, 2."
        );
    }

    fn transcript_for(n_a: &[u8; 32], commit: &[u8; 32], ka: &[u8; 32]) -> [u8; 32] {
        Transcript {
            kind: MatchKind::Box,
            mailbox: &[1; 16],
            rid: &[2; 16],
            request_code: b"ligusjoin1:abc",
            requester_ka: ka,
            commit,
            approver_device_id: Uuid::from_u128(7),
            approver_sig_key: &[8; 32],
            approver_ka: &[9; 32],
            n_a,
        }
        .digest()
    }

    /// M2: every input moves the code.
    #[test]
    fn changing_any_input_changes_the_code() {
        let base = Transcript {
            kind: MatchKind::Box,
            mailbox: &[1; 16],
            rid: &[2; 16],
            request_code: b"ligusjoin1:abc",
            requester_ka: &[3; 32],
            commit: &[4; 32],
            approver_device_id: Uuid::from_u128(7),
            approver_sig_key: &[8; 32],
            approver_ka: &[9; 32],
            n_a: &[10; 32],
        };
        let n_r = [11u8; 32];
        let c0 = code40(&base.digest(), &n_r);
        let variants: Vec<Transcript> = vec![
            Transcript {
                kind: MatchKind::Computer,
                ..base
            },
            Transcript {
                mailbox: &[0; 16],
                ..base
            },
            Transcript {
                rid: &[0; 16],
                ..base
            },
            Transcript {
                request_code: b"ligusjoin1:abd",
                ..base
            },
            Transcript {
                requester_ka: &[0; 32],
                ..base
            },
            Transcript {
                commit: &[0; 32],
                ..base
            },
            Transcript {
                approver_device_id: Uuid::from_u128(6),
                ..base
            },
            Transcript {
                approver_sig_key: &[0; 32],
                ..base
            },
            Transcript {
                approver_ka: &[0; 32],
                ..base
            },
            Transcript {
                n_a: &[0; 32],
                ..base
            },
        ];
        for (i, v) in variants.iter().enumerate() {
            assert_ne!(code40(&v.digest(), &n_r), c0, "variant {i}");
        }
        assert_ne!(code40(&base.digest(), &[12; 32]), c0, "nR");
        // Length prefixes: moving a byte between adjacent keys changes it too.
        let a = Transcript {
            requester_ka: &[3; 33],
            commit: &[4; 32],
            ..base
        };
        assert_ne!(a.digest(), base.digest());
    }

    /// M2 as a property over random inputs: any single-bit flip of nA or nR
    /// changes the 40-bit code (with overwhelming probability).
    #[test]
    fn random_flips_change_the_code() {
        for _ in 0..2_000 {
            let n_a: [u8; 32] = rand();
            let n_r: [u8; 32] = rand();
            let cm: [u8; 32] = rand();
            let ka: [u8; 32] = rand();
            let t = transcript_for(&n_a, &cm, &ka);
            let c = code40(&t, &n_r);
            let bit = (rand::<1>()[0] as usize) % 256;
            let mut n_a2 = n_a;
            n_a2[bit / 8] ^= 1 << (bit % 8);
            assert_ne!(code40(&transcript_for(&n_a2, &cm, &ka), &n_r), c);
            let mut n_r2 = n_r;
            n_r2[bit / 8] ^= 1 << (bit % 8);
            assert_ne!(code40(&t, &n_r2), c);
        }
    }

    /// M3: a relay in the middle runs two exchanges (one with the requester,
    /// one with the approver), choosing its own nonces only in the order the
    /// protocol allows: towards the approver it must commit before it sees
    /// nA, and towards the requester it must send nA before it sees nR. Its
    /// keys differ from the honest ones, so the transcripts differ. With the
    /// code cut to 16 bits the observed match rate is 2^-16; at 40 bits
    /// nothing matches.
    #[test]
    #[ignore = "10^6 runs: cargo test -p ligus-core match_code -- --ignored"]
    fn a_relay_in_the_middle_matches_at_the_chance_rate() {
        mitm_rate(1_000_000);
    }

    #[test]
    fn a_relay_in_the_middle_matches_at_the_chance_rate_quick() {
        mitm_rate(200_000);
    }

    fn mitm_rate(runs: u64) {
        let mut hits16 = 0u64;
        let mut hits40 = 0u64;
        for _ in 0..runs {
            // Requester side: honest requester ↔ attacker posing as approver.
            let rid: [u8; 16] = rand();
            let n_r: [u8; 32] = rand();
            let honest_commit = commit(&rid, &n_r);
            let attacker_n_a: [u8; 32] = rand();
            let t_req = Transcript {
                kind: MatchKind::Box,
                mailbox: &[1; 16],
                rid: &rid,
                request_code: b"ligusjoin1:honest",
                requester_ka: &[3; 32],
                commit: &honest_commit,
                approver_device_id: Uuid::from_u128(66),
                approver_sig_key: &[66; 32],
                approver_ka: &[67; 32],
                n_a: &attacker_n_a,
            }
            .digest();
            // Approver side: attacker posing as requester ↔ honest approver.
            let attacker_n_r: [u8; 32] = rand();
            let attacker_commit = commit(&rid, &attacker_n_r);
            let n_a: [u8; 32] = rand();
            let t_app = Transcript {
                kind: MatchKind::Box,
                mailbox: &[1; 16],
                rid: &rid,
                request_code: b"ligusjoin1:attacker",
                requester_ka: &[99; 32],
                commit: &attacker_commit,
                approver_device_id: Uuid::from_u128(7),
                approver_sig_key: &[8; 32],
                approver_ka: &[9; 32],
                n_a: &n_a,
            }
            .digest();
            if code_bits(&t_req, &n_r, 16) == code_bits(&t_app, &attacker_n_r, 16) {
                hits16 += 1;
            }
            if code40(&t_req, &n_r) == code40(&t_app, &attacker_n_r) {
                hits40 += 1;
            }
        }
        let expected = runs as f64 / 65_536.0;
        let sd = expected.sqrt();
        assert!(
            (hits16 as f64 - expected).abs() < 6.0 * sd + 2.0,
            "16-bit matches {hits16}, expected about {expected:.1}"
        );
        assert_eq!(hits40, 0, "no 40-bit match");
    }

    #[test]
    fn decoys_are_distinct_from_the_real_code_and_each_other() {
        for _ in 0..1_000 {
            let real = u64::from_be_bytes([0, 0, 0, 1, 2, 3, 4, 5]);
            let [a, b] = decoys(real);
            assert!(a != real && b != real && a != b);
            assert!(a < (1 << 40) && b < (1 << 40));
        }
    }

    #[test]
    fn mailbox_keys_are_deterministic_and_per_epoch() {
        let k1 = MailboxKeys::derive(&[1; 32]);
        let k1b = MailboxKeys::derive(&[1; 32]);
        let k2 = MailboxKeys::derive(&[2; 32]);
        assert_eq!(k1.mailbox(), k1b.mailbox());
        assert_ne!(k1.mailbox(), k2.mailbox());
        assert_ne!(k1.auth_seed, k1.seal_seed);
        assert_ne!(put_cap(&[1; 32], 0), put_cap(&[1; 32], 1));
        assert_eq!(mailbox_id(&k1.auth().public_key_bytes()), k1.mailbox());
    }

    #[test]
    fn message_contexts_are_distinct() {
        let ts = [
            "request", "claim", "open", "reveal", "result", "abort", "offer", "ack",
        ];
        for a in ts {
            for b in ts {
                if a != b {
                    assert_ne!(message_preimage(a, "{}"), message_preimage(b, "{}"));
                }
            }
        }
        assert!(message_preimage("grant", "{}").is_none());
    }
}
