//! A computer link that brings **all** the person's workspaces (wire note
//! 3.3, "more workspaces").
//!
//! A link request goes to one workspace's approval address, and its grant
//! covers that workspace. The approving device usually holds the person's
//! other workspaces too, so it offers one more grant for each workspace where
//! it can add the computer, in the same exchange:
//!
//! * the `offer` names each extra grant's workspace and digest (`more`);
//! * the computer's `ack` names the extra digests it takes (`more`): only for
//!   those does the approver publish a certificate (one passkey each);
//! * the `result` carries the extra grants whose certificate it published
//!   (`moreGrants`), each in its compact form ([`compact`]: the sealed
//!   bytes as one base64url string, a third of a `ligusgrant1:` text), which
//!   the computer turns back into the exact `ligusgrant1:` text the offer's
//!   digest names ([`expand`]).
//!
//! Every field is new, optional and left out when empty, so the bytes of a
//! one-workspace link are exactly what they were: an older computer ignores
//! the offer's `more` and acks only its own workspace; an older approver
//! sends no `more`, and the computer links one workspace as before.
//!
//! Each extra grant is an ordinary `ligusgrant1:` for the same request,
//! signed by the device that showed the code, and the computer checks it the
//! way it checks the first one ([`check_more_grant`]), then on that
//! workspace's own log (check 5, [`crate::ChatService::check_computer_link`]).
//! What it can't check against an address the person pasted (the workspace's
//! relay and founder) it takes from the approving device, the person's own
//! device whose code they just matched.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::envelope::{grant_digest, hint, AckBody, OfferBody, ResultBody, Verified, VerifiedOpen};
use super::hex_fixed;
use super::requester::{CheckedGrant, GrantError};
use crate::computer_link::{Grant, LinkRequest};
use ligus_core::e2ee::KeyAgreementKeypair;

/// The most extra workspaces one offer names.
pub const MAX_MORE: usize = 32;

/// One more workspace an offer names: its id, its name (a hint), and the
/// digest of the grant for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoreOffer {
    pub workspace_id: Uuid,
    pub name: String,
    pub grant_digest: String,
}

/// An extra grant the approver built: the workspace, its name, the grant.
/// The grant is a secret for the computer only: never log it.
#[derive(Clone)]
pub struct MoreGrant {
    pub workspace_id: Uuid,
    pub name: String,
    pub grant: String,
}

impl std::fmt::Debug for MoreGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MoreGrant({}, {:?})", self.workspace_id, self.name)
    }
}

// The wire shapes: the existing bodies, flattened, plus the new field.

#[derive(Serialize)]
pub(crate) struct OfferWire<'a> {
    #[serde(flatten)]
    pub base: &'a OfferBody,
    #[serde(skip_serializing_if = "<[MoreOffer]>::is_empty")]
    pub more: &'a [MoreOffer],
}

#[derive(Serialize)]
pub(crate) struct AckWire<'a> {
    #[serde(flatten)]
    pub base: &'a AckBody,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub more: &'a [String],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResultWire<'a> {
    #[serde(flatten)]
    pub base: &'a ResultBody,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub more_grants: &'a [String],
}

#[derive(Deserialize, Default)]
struct OfferMoreField {
    #[serde(default)]
    more: Vec<MoreOffer>,
}

#[derive(Deserialize, Default)]
struct AckMoreField {
    #[serde(default)]
    more: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ResultMoreField {
    #[serde(default)]
    more_grants: Vec<String>,
}

/// The extra workspaces a verified offer names: well-formed digests, one
/// entry per workspace, none for `own` (the request's own workspace, when
/// known), names as hints, at most [`MAX_MORE`]. Anything else is dropped.
pub(crate) fn offer_more(v: &Verified, own: Option<Uuid>) -> Vec<MoreOffer> {
    let parsed: OfferMoreField = serde_json::from_str(&v.body).unwrap_or_default();
    let mut out: Vec<MoreOffer> = Vec::new();
    for m in parsed.more {
        if out.len() >= MAX_MORE || hex_fixed::<32>(&m.grant_digest).is_none() || Some(m.workspace_id) == own {
            continue;
        }
        if out.iter().any(|o| o.workspace_id == m.workspace_id || o.grant_digest == m.grant_digest) {
            continue;
        }
        out.push(MoreOffer { name: hint(&m.name), ..m });
    }
    out
}

/// The extra digests a verified ack takes.
pub(crate) fn ack_more(v: &Verified) -> Vec<String> {
    let parsed: AckMoreField = serde_json::from_str(&v.body).unwrap_or_default();
    parsed.more.into_iter().filter(|d| hex_fixed::<32>(d).is_some()).take(MAX_MORE).collect()
}

/// The extra grants a verified result carries, back as `ligusgrant1:`
/// texts (an entry that doesn't expand is dropped).
pub(crate) fn result_more(v: &Verified) -> Vec<String> {
    let parsed: ResultMoreField = serde_json::from_str(&v.body).unwrap_or_default();
    parsed.more_grants.iter().take(MAX_MORE).filter_map(|c| expand(c)).collect()
}

/// A `ligusgrant1:` text's compact form: `b64u(ephemeralPublic(32) ‖
/// ciphertext)`. `None` for anything else.
pub(crate) fn compact(grant: &str) -> Option<String> {
    use base64::Engine as _;
    let body = grant.trim().strip_prefix(crate::computer_link::GRANT_PREFIX)?;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body.as_bytes()).ok()?;
    let blob: ligus_core::SealedBlob = serde_json::from_slice(&json).ok()?;
    if blob.ephemeral_public.len() != 32 {
        return None;
    }
    let mut out = blob.ephemeral_public;
    out.extend_from_slice(&blob.ciphertext);
    // Only a form that comes back as exactly this text.
    let c = super::b64::enc(&out);
    (expand(&c).as_deref() == Some(grant.trim())).then_some(c)
}

/// The `ligusgrant1:` text a compact form stands for (the same bytes
/// [`crate::computer_link::Grant::seal`] writes).
pub(crate) fn expand(compact: &str) -> Option<String> {
    use base64::Engine as _;
    let b = super::b64::dec(compact)?;
    if b.len() <= 32 || b.len() > crate::computer_link::MAX_CODE_LEN {
        return None;
    }
    let blob = ligus_core::SealedBlob { ephemeral_public: b[..32].to_vec(), ciphertext: b[32..].to_vec() };
    let json = serde_json::to_vec(&blob).ok()?;
    Some(format!("{}{}", crate::computer_link::GRANT_PREFIX, base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)))
}

/// What the computer made of one extra workspace.
#[derive(Debug)]
pub struct MoreGrantCheck {
    pub workspace_id: Uuid,
    /// The workspace's name, as the offer gave it (a hint).
    pub name: String,
    /// The grant, checked like the first one (unverified until check 5 on
    /// that workspace's log), or why not.
    pub outcome: Result<CheckedGrant, GrantError>,
}

/// Checks 1, 2 and 4 of design note 6.3 on one extra grant, plus: it is the
/// workspace the offer named, the same person as the first grant, and a
/// workspace whose founder the grant pins. `grant` is `None` when the
/// approver didn't send it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_more_grant(
    offered: &MoreOffer,
    grant: Option<&str>,
    open: &VerifiedOpen,
    request: &LinkRequest,
    ka: &KeyAgreementKeypair,
    device_key: &[u8],
    first: &CheckedGrant,
    now_ms: i64,
) -> Result<CheckedGrant, GrantError> {
    let CheckedGrant::Computer { grant: first_grant, person: first_person } = first else {
        return Err(GrantError::NotForThisRequest("the grant is for another kind of request".into()));
    };
    let Some(grant) = grant else { return Err(GrantError::NotSent) };
    if grant_digest(grant) != offered.grant_digest {
        return Err(GrantError::DigestMismatch);
    }
    let g = Grant::open(grant, ka).map_err(|e| GrantError::NotForThisRequest(e.to_string()))?;
    if g.request != request.digest() {
        return Err(GrantError::NotForThisRequest("it names another link request".into()));
    }
    if g.approver_key != open.approver_sig_key || g.approver_device_id != open.approver_device_id {
        return Err(GrantError::WrongApprover);
    }
    let person = g.check(device_key, now_ms).map_err(|e| GrantError::NotForThisRequest(e.to_string()))?;
    if g.workspace_id != offered.workspace_id || g.workspace_id == first_grant.workspace_id {
        return Err(GrantError::WrongWorkspace);
    }
    if person.account_id != first_person.account_id || person.id != first_person.id {
        return Err(GrantError::NotForThisRequest("it names another person than your first workspace".into()));
    }
    if g.conn.founder_pin().is_none() || g.conn.fk.as_deref().is_none_or(|f| hex_fixed::<16>(&f.trim().to_ascii_lowercase()).is_none()) {
        return Err(GrantError::WrongFounder);
    }
    Ok(CheckedGrant::Computer { grant: Box::new(g), person })
}
