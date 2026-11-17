//! Verify-on-read with quarantine — ported from `EnvelopeVerifier.swift`.
//!
//! Before projecting a workspace's event stream, each signed envelope is
//! checked against the writing device's public key (resolved from the device
//! roster). Envelopes that fail — bad signature, unknown/revoked device, or
//! (under the clean-replacement default) unsigned — are diverted to a
//! quarantine list and excluded from projection, rather than poisoning state.

use hive_core::crypto::verify_envelope;
use hive_core::{CryptoError, DeviceIdentity, SessionEvent, SessionEventEnvelope};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Why an envelope was quarantined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineReason {
    /// No signature present (rejected unless `allow_unsigned`).
    Unsigned,
    /// Signed by a device id not in the roster.
    UnknownDevice,
    /// Signed by a device whose key has been revoked.
    RevokedDevice,
    /// Signature did not verify against the device's public key.
    BadSignature,
    /// The stamped author's account is not the signing device's account — a
    /// member trying to attribute an event to someone else.
    Impersonation,
    /// Option C (github-signing-keys identity mode): GitHub was fetched and the
    /// account's registered signing key is **not** among the login's
    /// `ssh_signing_keys` — a provably-bad identity binding. See
    /// `identity_verifier` and docs/security-hardening-plan.md S1 Option C.
    GithubKeyNotListed,
    /// Two different signing keys were registered for the same account id — an
    /// ambiguous/contested identity binding. Fail closed: don't trust events
    /// stamped as a disputed account (a forged `AccountKeyRegistered` under a
    /// victim's account id can no longer silently win first-registration and
    /// hijack the victim). See `build_roster`.
    DisputedAccountKey,
    /// The device was certified while dearth mode is on and still waits for a
    /// second Admin's approval. Held (never accepted, never dropped) in every
    /// identity mode, so the approval binds in Relay mode too.
    PendingApproval,
    /// Signed by a device no certificate vouches for yet, while its stamp
    /// claims an account the workspace already knows. Held (retried), never
    /// accepted: otherwise anyone with the workspace key could stamp events as
    /// a known member from a fresh device id.
    UncertifiedDevice,
}

/// Resolves device public keys + revocation status for verification.
pub trait DeviceKeyResolver {
    fn public_key(&self, device_id: Uuid) -> Option<&[u8]>;
    fn is_revoked(&self, device_id: Uuid) -> bool;
}

/// A roster of known devices, the simplest [`DeviceKeyResolver`].
#[derive(Debug, Default, Clone)]
pub struct DeviceRoster {
    keys: HashMap<Uuid, Vec<u8>>,
    revoked: HashMap<Uuid, bool>,
}

impl DeviceRoster {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, device: &DeviceIdentity) {
        self.keys
            .insert(device.id, device.signing_public_key.clone());
        self.revoked.insert(device.id, device.is_revoked());
    }

    pub fn with_device(mut self, device: &DeviceIdentity) -> Self {
        self.insert(device);
        self
    }
}

impl DeviceKeyResolver for DeviceRoster {
    fn public_key(&self, device_id: Uuid) -> Option<&[u8]> {
        self.keys.get(&device_id).map(Vec::as_slice)
    }
    fn is_revoked(&self, device_id: Uuid) -> bool {
        self.revoked.get(&device_id).copied().unwrap_or(false)
    }
}

/// The split result of verifying an envelope stream.
#[derive(Debug, Default)]
pub struct VerificationOutcome {
    /// Envelopes that verified — safe to project.
    pub valid: Vec<SessionEventEnvelope>,
    /// Rejected envelopes paired with the reason.
    pub quarantined: Vec<(SessionEventEnvelope, QuarantineReason)>,
}

/// Verify each envelope against the resolver. `allow_unsigned` controls whether
/// envelopes without a signature pass through (Swift's back-compat behavior) or
/// are quarantined (the clean-replacement default, where everything is signed).
pub fn verify_stream<R: DeviceKeyResolver>(
    envelopes: impl IntoIterator<Item = SessionEventEnvelope>,
    resolver: &R,
    allow_unsigned: bool,
) -> VerificationOutcome {
    let mut outcome = VerificationOutcome::default();
    for env in envelopes {
        let Some(device_id) = env.signer_device_id else {
            if allow_unsigned {
                outcome.valid.push(env);
            } else {
                outcome.quarantined.push((env, QuarantineReason::Unsigned));
            }
            continue;
        };
        if resolver.is_revoked(device_id) {
            outcome
                .quarantined
                .push((env, QuarantineReason::RevokedDevice));
            continue;
        }
        let Some(public_key) = resolver.public_key(device_id) else {
            outcome
                .quarantined
                .push((env, QuarantineReason::UnknownDevice));
            continue;
        };
        match verify_envelope(&env, public_key) {
            Ok(()) => outcome.valid.push(env),
            Err(CryptoError::Unsigned) if allow_unsigned => outcome.valid.push(env),
            Err(_) => outcome
                .quarantined
                .push((env, QuarantineReason::BadSignature)),
        }
    }
    outcome
}

// ---------------------------------------------------------------------------
// Workspace roster + verdicts (S1) — a device-key resolver built by folding the
// trust events (`AccountKeyRegistered`, `DeviceCertificateAdded`, membership)
// out of a workspace's own event stream, so verification needs no network at
// ingest and every device derives the identical roster (it folds in canonical
// order). See docs/security-hardening-plan.md S1.
// ---------------------------------------------------------------------------

/// device_id → (signing public key, the set of accounts that have cert-verified
/// this device) for trusted, non-revoked devices, plus the set of devices whose
/// owning account was removed. A device can be certified to more than one
/// account — e.g. a local identity that later signs into GitHub is the same
/// person, and each cert is signed by its own account key — so an event is
/// authentic if its stamp matches ANY of the device's certified accounts.
#[derive(Debug, Default, Clone)]
pub struct WorkspaceRoster {
    devices: HashMap<Uuid, (Vec<u8>, HashSet<Uuid>)>,
    revoked: HashSet<Uuid>,
    /// account_id → the account's pinned signing public key, for member accounts
    /// (first-registration wins, canonical order). Retained so the Option-C
    /// identity gate can check this exact key against the account's GitHub
    /// `ssh_signing_keys`. Not used by Option A (relay-trusted) verification.
    account_keys: HashMap<Uuid, Vec<u8>>,
    /// Device certificates withheld because they were issued while their
    /// workspace was in dearth mode and lack a second approval:
    /// `(workspace, device, account)`.
    pending: Vec<(Uuid, Uuid, Uuid)>,
    /// Workspaces whose config log has dearth mode on (Admin-authorized).
    /// While on, an event no certified device vouches for is held, not
    /// accepted, whatever the identity mode.
    dearth_workspaces: HashSet<Uuid>,
    /// Accounts for which two *different* signing keys were registered — a
    /// contested binding. Events stamped as one of these are quarantined (fail
    /// closed), so a forged registration under a victim's account can't hijack it.
    disputed: HashSet<Uuid>,
    /// Every device whose certificate verifies against its account's pinned
    /// key, member or not (for the batch fixpoint's self-validating events).
    chained: HashMap<Uuid, (Vec<u8>, HashSet<Uuid>)>,
    /// Accounts an authorized removal took out (and no Admin re-added).
    removed_accounts: HashSet<Uuid>,
    /// Agent signing keys valid at the config head: key_id → (key, agent).
    agent_keys: HashMap<Uuid, (Vec<u8>, Uuid)>,
}

impl WorkspaceRoster {
    /// Whether `device_id` is cert-verified to sign as `account` (any of its
    /// certified accounts). False for an unknown device.
    pub fn device_signs_for(&self, device_id: Uuid, account: Uuid) -> bool {
        self.devices.get(&device_id).is_some_and(|(_, accts)| accts.contains(&account))
    }
    /// Whether `device_id` is a trusted (cert-verified, non-revoked) device.
    pub fn is_certified(&self, device_id: Uuid) -> bool {
        self.devices.contains_key(&device_id)
    }
    /// One account this device is certified to (deterministic: smallest by id),
    /// for callers needing a single account when an event carries no stamp. Use
    /// `device_signs_for` for authorization — a device may hold several.
    pub fn account_of(&self, device_id: Uuid) -> Option<Uuid> {
        self.devices.get(&device_id).and_then(|(_, accts)| accts.iter().min().copied())
    }
    /// The pinned account signing key for a member account, if known. Used by the
    /// Option-C identity gate to compare against GitHub `ssh_signing_keys`.
    pub fn account_signing_key(&self, account_id: Uuid) -> Option<&[u8]> {
        self.account_keys.get(&account_id).map(Vec::as_slice)
    }
    /// Whether `account_id` has a contested key binding (two different keys
    /// registered). Events stamped as it are quarantined — fail closed.
    pub fn is_disputed(&self, account_id: Uuid) -> bool {
        self.disputed.contains(&account_id)
    }
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty() && self.revoked.is_empty()
    }
    /// Certificates waiting for a dearth-mode second approval, as
    /// `(workspace, device, account)`.
    pub fn pending_devices(&self) -> &[(Uuid, Uuid, Uuid)] {
        &self.pending
    }
    /// Every trusted (cert-verified, non-revoked) device id.
    pub fn certified_devices(&self) -> Vec<Uuid> {
        let mut v: Vec<Uuid> = self.devices.keys().copied().collect();
        v.sort();
        v
    }
}

/// Dearth mode as the roster needs it: which devices are admitted without a
/// second approval, and the accounts of the Admins who approved each subject.
struct DearthPolicy {
    admitted: HashSet<Uuid>,
    approvers: HashMap<String, Vec<Uuid>>,
}

impl DearthPolicy {
    /// A certificate counts if its device was trusted when dearth mode began,
    /// or an Admin of a *different* account approved it (the owner's own
    /// certificate is the first approval).
    fn admits(&self, cert: &hive_core::crypto::DeviceCertificate) -> bool {
        self.admitted.contains(&cert.device_id)
            || self
                .approvers
                .get(&cert.device_id.to_string())
                .is_some_and(|accts| accts.iter().any(|a| *a != cert.account_id))
    }
}

/// Workspaces whose config log is in `ordered`.
fn config_workspaces(ordered: &[&SessionEventEnvelope]) -> HashSet<Uuid> {
    ordered
        .iter()
        .filter(|e| e.session_id == hive_core::workspace_config_session_id(e.workspace_id))
        .map(|e| e.workspace_id)
        .collect()
}

/// Per workspace, the active dearth policy from its config log, projected with
/// the usual ingest authorization (so only an Admin's switch or approval counts)
/// and the same founder pin as the roster, so a forged lower-lamport seed
/// can't pick the projection dearth mode is read from.
fn dearth_policies(ordered: &[&SessionEventEnvelope], pin: Option<&FounderPin>) -> HashMap<Uuid, DearthPolicy> {
    let founder = pin.map(|p| p.founder_account.to_string());
    let mut logs: HashMap<Uuid, Vec<SessionEventEnvelope>> = HashMap::new();
    for env in ordered {
        if env.session_id == hive_core::workspace_config_session_id(env.workspace_id) {
            logs.entry(env.workspace_id).or_default().push((*env).clone());
        }
    }
    let mut out = HashMap::new();
    for (ws, envs) in logs {
        if !envs.iter().any(|e| matches!(e.payload, SessionEvent::DearthModeChanged { .. })) {
            continue;
        }
        let Some(cfg) = hive_core::project_with_founder(&envs, founder.as_deref()) else { continue };
        let Some(d) = cfg.guards.dearth_on() else { continue };
        let approvers = cfg
            .guards
            .join_approvals
            .iter()
            .map(|(subject, ids)| {
                let accts = ids
                    .iter()
                    .filter_map(|id| cfg.members.iter().find(|m| &m.id == id))
                    .filter(|m| m.role.rank() >= hive_core::WorkspaceRole::Admin.rank())
                    .filter_map(|m| m.actor.account_id)
                    .collect();
                (subject.clone(), accts)
            })
            .collect();
        out.insert(ws, DearthPolicy { admitted: d.admitted_devices.iter().copied().collect(), approvers });
    }
    out
}

impl DeviceKeyResolver for WorkspaceRoster {
    fn public_key(&self, device_id: Uuid) -> Option<&[u8]> {
        self.devices.get(&device_id).map(|(k, _)| k.as_slice())
    }
    // (see below for is_revoked)
    fn is_revoked(&self, device_id: Uuid) -> bool {
        self.revoked.contains(&device_id)
    }
}

/// Fold a workspace's events into a [`WorkspaceRoster`]. Trust rooted in the
/// members (an account is trusted while it's in the roster) and the cryptographic
/// cert chain (a device is trusted only if its certificate verifies against its
/// account's registered signing key). Account keys are pinned first-registration
/// wins in canonical order (deterministic); the GitHub-anchored binding of that
/// key to a real identity is established where the key *enters* the log (the
/// directory / GitHub proof — Options A/B/C in the plan). A removed account's
/// devices become revoked.
pub fn build_roster(envelopes: &[SessionEventEnvelope]) -> WorkspaceRoster {
    build_roster_pinned(envelopes, None)
}

/// A workspace's founder, pinned out of band by its invite (B2): the founder's
/// account and the first 16 bytes of SHA-256 of its signing key (`fk`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FounderPin {
    pub founder_account: Uuid,
    pub fk: [u8; 16],
}

impl FounderPin {
    /// Parse a pin from the invite's founder id and hex `fk`.
    pub fn parse(founder: &str, fk_hex: &str) -> Option<Self> {
        let founder_account = Uuid::parse_str(founder.trim()).ok()?;
        let fk_hex = fk_hex.trim();
        if fk_hex.len() != 32 {
            return None;
        }
        let mut fk = [0u8; 16];
        for (i, b) in fk.iter_mut().enumerate() {
            *b = u8::from_str_radix(fk_hex.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some(Self { founder_account, fk })
    }

    /// The `fk` of an account signing key.
    pub fn fingerprint(signing_public_key: &[u8]) -> [u8; 16] {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(signing_public_key);
        let mut out = [0u8; 16];
        out.copy_from_slice(&d[..16]);
        out
    }

    /// Lowercase hex of [`FounderPin::fingerprint`], as the invite carries it.
    pub fn fingerprint_hex(signing_public_key: &[u8]) -> String {
        Self::fingerprint(signing_public_key).iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// The creation seed a pinned fold takes as its base (the earliest seed
/// naming the pinned founder as creator and author), when it is authentic:
/// signed by a device certified for the founder's account under a key whose
/// fingerprint is the pinned `fk`, and the account's key binding is not
/// disputed. `None` while no such seed is on the log: the founder named by an
/// invite has not (yet) founded it. A stamp claiming the founder's id is not
/// enough; a co-member who forged an invite "from" someone else can't sign as
/// that person's device.
pub fn founder_seed<'a>(envelopes: &'a [SessionEventEnvelope], pin: &FounderPin) -> Option<&'a SessionEventEnvelope> {
    let roster = build_roster_pinned(envelopes, Some(pin));
    let account = pin.founder_account;
    let key = roster.account_signing_key(account)?;
    if FounderPin::fingerprint(key) != pin.fk || roster.is_disputed(account) {
        return None;
    }
    let founder = account.to_string();
    let mut ordered: Vec<&SessionEventEnvelope> = envelopes.iter().collect();
    ordered.sort_by_key(|e| e.canonical_key());
    let base = ordered.into_iter().find(|e| match &e.payload {
        hive_core::SessionEvent::SessionSnapshot { session } => {
            session.creator_actor_id == founder && e.actor_stamp.as_ref().is_some_and(|s| s.actor.id == founder)
        }
        _ => false,
    })?;
    let device = base.signer_device_id?;
    let device_key = roster.public_key(device)?;
    (roster.device_signs_for(device, account)
        && !roster.is_revoked(device)
        && hive_core::crypto::verify_envelope(base, device_key).is_ok())
    .then_some(base)
}

/// One workspace's roster from that workspace's own trust events (P0.4). With
/// a founder pin, a registration of the founder's account under a key that
/// doesn't match the pin is rejected.
pub fn build_roster_pinned(envelopes: &[SessionEventEnvelope], pin: Option<&FounderPin>) -> WorkspaceRoster {
    let mut ordered: Vec<&SessionEventEnvelope> = envelopes.iter().collect();
    ordered.sort_by_key(|e| e.canonical_key());

    let mut account_keys: HashMap<Uuid, Vec<u8>> = HashMap::new();
    let mut disputed: HashSet<Uuid> = HashSet::new();
    let mut explicit_revoked: HashSet<Uuid> = HashSet::new();
    let mut raw_revocations: Vec<(Uuid, Uuid)> = Vec::new();
    let mut members: HashSet<Uuid> = HashSet::new();
    let mut member_account: HashMap<String, Uuid> = HashMap::new();
    let mut certs: Vec<hive_core::crypto::DeviceCertificate> = Vec::new();
    // The workspace each certificate was published in (parallel to `certs`).
    let mut cert_ws: Vec<Uuid> = Vec::new();

    let note_member = |members: &mut HashSet<Uuid>,
                           member_account: &mut HashMap<String, Uuid>,
                           m: &hive_core::WorkspaceMember| {
        if let Some(aid) = m.actor.account_id {
            members.insert(aid);
            member_account.insert(m.id.clone(), aid);
        }
    };

    // Each workspace's config log, folded once with the founder pin (a
    // lower-lamport seed from a keyholder who merely verifies, a
    // self-enrolled joiner, must not become a projection's founder).
    let founder = pin.map(|p| p.founder_account.to_string());
    let cfg_folds: Vec<CfgFold> = config_workspaces(&ordered)
        .into_iter()
        .map(|ws| {
            let envs: Vec<SessionEventEnvelope> = ordered
                .iter()
                .filter(|e| e.workspace_id == ws && e.session_id == hive_core::workspace_config_session_id(ws))
                .map(|e| (*e).clone())
                .collect();
            let (projected, timeline) = hive_core::project_config(&envs, founder.as_deref(), None);
            let decisions = hive_core::fold_decisions(&envs, founder.as_deref(), None, hive_core::WorkspaceAuthz::Unchecked);
            CfgFold { ws, envs, projected, timeline, decisions }
        })
        .collect();

    for env in &ordered {
        match &env.payload {
            // Rules v2: rotating the key of an account that holds a passkey
            // needs its presence, so only a rotation its config log allowed
            // counts (one without presence, or published elsewhere, would let
            // a thief holding the account key re-pin it and drop every device
            // the owner certified).
            SessionEvent::AccountKeyRotated { account_id, .. } if rotation_gated(&cfg_folds, env, *account_id) => {}
            SessionEvent::AccountKeyRegistered { account_id, signing_public_key } => {
                // With the founder pinned out of band we know which key is
                // genuine: a registration of the founder's account under any
                // other key is rejected (not merely contested, which would
                // lock the real founder out too).
                if let Some(pin) = pin.filter(|p| p.founder_account == *account_id) {
                    if FounderPin::fingerprint(signing_public_key) != pin.fk {
                        continue;
                    }
                }
                match account_keys.get(account_id) {
                    // A different key than the one already pinned for this account:
                    // a contested binding. Mark disputed (fail closed in verdict_for)
                    // instead of silently keeping first-wins — which let a forged
                    // registration under a victim's account id, sorted to lamport 0,
                    // deterministically win and hijack the victim's identity.
                    Some(existing) if existing != signing_public_key => {
                        disputed.insert(*account_id);
                    }
                    // Idempotent re-registration of the same key — not a conflict.
                    Some(_) => {}
                    None => {
                        account_keys.insert(*account_id, signing_public_key.clone());
                    }
                }
            }
            SessionEvent::AccountKeyRotated {
                account_id,
                new_signing_public_key,
                succession_signature,
            } => {
                // A legitimate rotation: the new key must be signed by the account's
                // CURRENT (pinned) key — proof the same owner rotated it (P2-2). Then
                // re-pin to the new key. A rotation with no prior key, or one whose
                // signature doesn't verify against the current key, is a forged/
                // contested binding → disputed (fail closed). Idempotent if it's
                // already the pinned key.
                match account_keys.get(account_id) {
                    Some(current) if current == new_signing_public_key => {}
                    Some(current) => {
                        if hive_core::crypto::verify(
                            current,
                            new_signing_public_key,
                            succession_signature,
                        )
                        .is_ok()
                        {
                            account_keys.insert(*account_id, new_signing_public_key.clone());
                        } else {
                            disputed.insert(*account_id);
                        }
                    }
                    None => {
                        // Rotation without an established key can't be authenticated.
                        disputed.insert(*account_id);
                    }
                }
            }
            SessionEvent::SessionSnapshot { session } => {
                for m in &session.members {
                    note_member(&mut members, &mut member_account, m);
                }
            }
            SessionEvent::MemberAdded { member, .. } => {
                note_member(&mut members, &mut member_account, member);
            }
            // On a config log, only a removal its projection accepted counts
            // (applied below, sticky); a raw one could come from anyone.
            SessionEvent::MemberRemoved { member_id, .. }
                if env.session_id != hive_core::workspace_config_session_id(env.workspace_id) =>
            {
                if let Some(aid) = member_account.get(member_id) {
                    members.remove(aid);
                }
            }
            SessionEvent::DeviceCertificateAdded { certificate } => {
                certs.push(certificate.clone());
                cert_ws.push(env.workspace_id);
            }
            SessionEvent::DeviceRevoked { device_id, .. } => {
                raw_revocations.push((env.workspace_id, *device_id));
            }
            _ => {}
        }
    }

    // Removal is sticky against the removed: an account taken out on a
    // workspace-config log stays out unless the authorization-checked
    // projection of that log has it as a member again (an Admin's re-add). A
    // raw `MemberAdded`, chat seed or unsigned event naming them afterwards,
    // their own included, doesn't count: a removed member still holds old
    // epoch keys and could otherwise re-enter with a freshly certified device.
    //
    // And the converse: a raw `MemberRemoved` that projection refuses (a
    // Contributor "removing" the owner, an unsigned one, one on a chat log)
    // must not revoke anyone. Every member of an authorization-checked config
    // projection stays a member, unless an authorized removal elsewhere took
    // the account out (removal wins, as the roster is shared by workspaces).
    let mut removed_by_ws: HashMap<Uuid, HashSet<String>> = HashMap::new();
    let mut agent_keys: HashMap<Uuid, (Vec<u8>, Uuid)> = HashMap::new();
    let mut current_accounts: HashSet<Uuid> = HashSet::new();
    let mut sticky_removed: HashSet<Uuid> = HashSet::new();
    // Each workspace's config roster at its head (rules, passkeys, the
    // devices its own log allowed).
    let mut heads: HashMap<Uuid, hive_core::authz::RosterState> = HashMap::new();
    // A device revocation counts when the config projection accepted it
    // (Admin+). One for a workspace whose config log we don't hold (legacy,
    // pre-hoist) keeps the raw behaviour.
    for (ws, device) in &raw_revocations {
        if !cfg_folds.iter().any(|c| c.ws == *ws) {
            explicit_revoked.insert(*device);
        }
    }
    for CfgFold { ws, envs: cfg_envs, projected, timeline, decisions } in cfg_folds {
        // Only removals this projection accepted are sticky; a raw one it
        // refused (anyone "removing" the founder) removes no one.
        for e in &cfg_envs {
            if let SessionEvent::MemberRemoved { member_id, .. } = &e.payload {
                if decisions.get(&e.event_id) == Some(&hive_core::Verdict::Allowed) {
                    removed_by_ws.entry(ws).or_default().insert(member_id.clone());
                }
            }
        }
        heads.insert(ws, timeline.current().clone());
        // A3: agent keys valid at the config head (certified, still bound).
        let head = timeline.current();
        for (key_id, k) in &head.agent_keys {
            if head.valid_agent_key(*key_id).is_some() {
                agent_keys.insert(*key_id, (k.signing_public_key.clone(), k.agent_id));
            } else {
                // Rebound since (or its host revoked): its events are refused.
                explicit_revoked.insert(*key_id);
            }
        }
        explicit_revoked.extend(projected.iter().flat_map(|c| c.guards.revoked_devices.iter().copied()));
        let current: Vec<hive_core::WorkspaceMember> = projected.map(|c| c.members).unwrap_or_default();
        current_accounts.extend(current.iter().filter_map(|m| m.actor.account_id));
        for id in removed_by_ws.get(&ws).into_iter().flatten() {
            if !current.iter().any(|m| &m.id == id) {
                if let Some(aid) = member_account.get(id) {
                    sticky_removed.insert(*aid);
                }
            }
        }
    }
    members.extend(current_accounts);
    for aid in &sticky_removed {
        members.remove(aid);
    }

    let mut roster = WorkspaceRoster::default();
    roster.agent_keys = agent_keys;
    roster.disputed = disputed;
    roster.removed_accounts = sticky_removed.clone();
    // Explicitly-revoked devices (a `DeviceRevoked` event) are never trusted, even
    // if they still hold a valid member cert — verdict_for checks is_revoked first.
    roster.revoked.extend(explicit_revoked);
    // Retain the pinned account signing keys for accounts that are members — the
    // Option-C gate checks these against GitHub. (Non-member account keys are
    // irrelevant; their devices are revoked below.)
    for (account_id, key) in &account_keys {
        if members.contains(account_id) {
            roster.account_keys.insert(*account_id, key.clone());
        }
    }
    let dearth = dearth_policies(&ordered, pin);
    roster.dearth_workspaces = dearth.keys().copied().collect();
    for (cert, ws) in certs.iter().zip(&cert_ws) {
        let Some(account_key) = account_keys.get(&cert.account_id) else { continue };
        if cert.verify(account_key).is_err() {
            continue; // cert must chain to its account's key
        }
        // Rules v2: a device of an account that holds a passkey or a recovery
        // key counts only once its config log allowed it (with that account's
        // presence). A certificate published anywhere else (a chat log, or
        // under a backdated frontier) certifies nothing then.
        if heads.get(ws).is_some_and(|h| {
            h.rules >= 2
                && h.passkeys.get(&cert.account_id).is_some_and(|m| !m.is_empty())
                && !h.account_certified(cert.account_id, cert.device_id)
        }) {
            continue;
        }
        if !roster.disputed.contains(&cert.account_id) {
            roster
                .chained
                .entry(cert.device_id)
                .or_insert_with(|| (cert.device_public_key.clone(), HashSet::new()))
                .1
                .insert(cert.account_id);
        }
        // Dearth mode: a device that wasn't trusted when the entrance closed
        // needs a second approval before its certificate counts.
        if let Some(policy) = dearth.get(ws) {
            if members.contains(&cert.account_id) && !policy.admits(cert) {
                roster.pending.push((*ws, cert.device_id, cert.account_id));
                continue;
            }
        }
        if members.contains(&cert.account_id) {
            // Accumulate every member account this device is certified to (a
            // device may transition identities — e.g. local → GitHub sign-in — and
            // hold a valid cert under each). Any of them is a valid signing account.
            let entry = roster
                .devices
                .entry(cert.device_id)
                .or_insert_with(|| (cert.device_public_key.clone(), HashSet::new()));
            entry.1.insert(cert.account_id);
        }
    }
    // A device whose *only* verified certs are to removed accounts is revoked —
    // but a device that also holds a current member cert (above) stays trusted.
    for cert in &certs {
        if roster.devices.contains_key(&cert.device_id) {
            continue;
        }
        if let Some(account_key) = account_keys.get(&cert.account_id) {
            if cert.verify(account_key).is_ok() && !members.contains(&cert.account_id) {
                roster.revoked.insert(cert.device_id);
            }
        }
    }
    roster
}

/// One workspace's config log, folded for the roster.
struct CfgFold {
    ws: Uuid,
    envs: Vec<SessionEventEnvelope>,
    projected: Option<hive_core::ChatSession>,
    timeline: hive_core::authz::ConfigTimeline,
    decisions: std::collections::BTreeMap<Uuid, hive_core::Verdict>,
}

/// Whether an `AccountKeyRotated` for `account` is left out of the roster:
/// its workspace is on rules v2, the account holds a passkey or a recovery
/// key, and the rotation isn't one its config log allowed.
fn rotation_gated(cfg: &[CfgFold], env: &SessionEventEnvelope, account: Uuid) -> bool {
    let Some(c) = cfg.iter().find(|c| c.ws == env.workspace_id) else { return false };
    let head = c.timeline.current();
    if head.rules < 2 || head.passkeys.get(&account).is_none_or(|m| m.is_empty()) {
        return false;
    }
    let on_config = env.session_id == hive_core::workspace_config_session_id(c.ws);
    !on_config || c.decisions.get(&env.event_id) != Some(&hive_core::Verdict::Allowed)
}

/// One roster per workspace, each folded from that workspace's events only
/// (P0.4): removing an account in one workspace never touches its devices in
/// another.
pub fn build_rosters(
    envelopes: &[SessionEventEnvelope],
    pins: &HashMap<Uuid, FounderPin>,
) -> HashMap<Uuid, WorkspaceRoster> {
    let mut by_ws: HashMap<Uuid, Vec<SessionEventEnvelope>> = HashMap::new();
    for e in envelopes {
        by_ws.entry(e.workspace_id).or_default().push(e.clone());
    }
    by_ws.into_iter().map(|(ws, envs)| (ws, build_roster_pinned(&envs, pins.get(&ws)))).collect()
}

/// Kinds that feed the trust roster (the same set `EventStore::trust_envelopes`
/// returns).
fn is_trust_kind(e: &SessionEvent) -> bool {
    matches!(
        e,
        SessionEvent::AccountKeyRegistered { .. }
            | SessionEvent::AccountKeyRotated { .. }
            | SessionEvent::DeviceCertificateAdded { .. }
            | SessionEvent::DeviceRevoked { .. }
            | SessionEvent::MemberAdded { .. }
            | SessionEvent::MemberRemoved { .. }
            | SessionEvent::SessionSnapshot { .. }
            | SessionEvent::MemberRoleChanged { .. }
            | SessionEvent::DearthModeChanged { .. }
            | SessionEvent::JoinApproved { .. }
            | SessionEvent::AgentKeyCertified { .. }
            | SessionEvent::AgentUpserted { .. }
            | SessionEvent::AgentRosterUpdated { .. }
            | SessionEvent::AgentRemoved { .. }
            | SessionEvent::WorkspaceRulesSet { .. }
            | SessionEvent::PasskeyRegistered { .. }
            | SessionEvent::PasskeyRemoved { .. }
    )
}

/// The roster for judging an incoming batch of one workspace (P0.4): the
/// stored trust events, plus the batch's own trust events **only once they
/// verify**. Account keys and device certificates are self-validating (a cert
/// is signed by its account key; registration is first-wins plus dispute), so
/// they join at once; membership and other trust events join in canonical
/// order once their verdict is `Valid` against the roster so far, repeated
/// until nothing changes (at most once per event). An unverified
/// `MemberAdded` in a batch therefore never certifies anyone.
pub fn roster_with_batch(
    mut stored: Vec<SessionEventEnvelope>,
    batch: &[&SessionEventEnvelope],
    pin: Option<&FounderPin>,
) -> WorkspaceRoster {
    let mut pending: Vec<&SessionEventEnvelope> = batch.iter().copied().filter(|e| is_trust_kind(&e.payload)).collect();
    pending.sort_by_key(|e| e.canonical_key());
    let (boot, mut pending): (Vec<_>, Vec<_>) = pending.into_iter().partition(|e| is_trust_bootstrap(e));
    // A key registration or rotation joins only when it speaks for its own
    // signer's account (see `key_publication_ok`): a forged one would make the
    // victim's account disputed for this very batch.
    let boot: Vec<_> = boot.into_iter().filter(|e| key_publication_ok(e)).collect();
    stored.extend(boot.into_iter().cloned());
    let mut roster = build_roster_pinned(&stored, pin);
    while !pending.is_empty() {
        let (ok, rest): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|e| verdict_for(&roster, e) == Verdict::Valid || self_validating(&roster, e, pin));
        pending = rest;
        if ok.is_empty() {
            break;
        }
        stored.extend(ok.into_iter().cloned());
        roster = build_roster_pinned(&stored, pin);
    }
    roster
}

/// Whether `env` is signed by a device whose certificate chains to `account`.
fn signed_as(roster: &WorkspaceRoster, env: &SessionEventEnvelope, account: Uuid) -> bool {
    let Some(device) = env.signer_device_id else { return false };
    let Some((key, accounts)) = roster.chained.get(&device) else { return false };
    // Not `is_revoked`: before its account is a member, the roster lists such
    // a device as revoked (non-member certs are). A removed member's own
    // re-enrollment is refused by the config projection, and their removal
    // stays sticky in the roster.
    accounts.contains(&account) && verify_envelope(env, key).is_ok()
}

/// Membership events that verify without already being a member: the config
/// log's founding seed (the self-consistent seed, signed by a device of the
/// founder's account; the pinned founder's, when pinned) and a self-enrollment
/// below Admin signed by a device of the joining account.
fn self_validating(roster: &WorkspaceRoster, env: &SessionEventEnvelope, pin: Option<&FounderPin>) -> bool {
    let Some(account) = env.actor_stamp.as_ref().and_then(|s| s.actor.account_id) else { return false };
    let author = env.actor_stamp.as_ref().map(|s| s.actor.id.as_str()).unwrap_or("");
    match &env.payload {
        SessionEvent::SessionSnapshot { session }
            if env.session_id == hive_core::workspace_config_session_id(env.workspace_id) =>
        {
            let owners: Vec<_> =
                session.members.iter().filter(|m| m.role == hive_core::WorkspaceRole::Owner).collect();
            session.creator_actor_id == author
                && owners.len() == 1
                && owners[0].id == author
                && pin.is_none_or(|p| p.founder_account == account)
                && signed_as(roster, env, account)
        }
        SessionEvent::MemberAdded { member, .. } => {
            member.id == author
                && member.actor.account_id == Some(account)
                && member.role.rank() < hive_core::WorkspaceRole::Admin.rank()
                && signed_as(roster, env, account)
        }
        _ => false,
    }
}

/// One roster per workspace in `batch`, each from that workspace's stored
/// trust events, its founder pin, and the batch's verified trust events.
pub fn rosters_for_batch(
    store: &crate::event_store::EventStore,
    batch: &[&SessionEventEnvelope],
) -> crate::event_store::Result<HashMap<Uuid, WorkspaceRoster>> {
    let mut by_ws: HashMap<Uuid, Vec<&SessionEventEnvelope>> = HashMap::new();
    for e in batch {
        by_ws.entry(e.workspace_id).or_default().push(e);
    }
    let mut out = HashMap::new();
    for (ws, envs) in by_ws {
        let pin = store.workspace_pin(ws)?;
        out.insert(ws, roster_with_batch(store.trust_envelopes(ws)?, &envs, pin.as_ref()));
    }
    Ok(out)
}

/// The verification outcome for a single envelope, split so a caller can apply a
/// non-bricking policy: **quarantine** the provably-bad, but only **hold** the
/// merely-unverifiable (retry as the roster grows) instead of dropping it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Signed by a known, non-revoked device; signature valid; author matches.
    Valid,
    /// Provably bad — reject: bad signature, revoked device, or impersonation.
    Quarantine(QuarantineReason),
    /// Can't verify yet (unsigned, or a device whose cert we haven't seen).
    /// Safe to accept for now and re-check later; never a reason to drop.
    Unverifiable(QuarantineReason),
}

/// Account-key and certificate events: inert in projection (only the roster
/// reads them, and the roster applies the dearth policy to certificates
/// itself), and Admins must be able to see a pending device to approve it.
fn is_trust_bootstrap(env: &SessionEventEnvelope) -> bool {
    matches!(
        env.payload,
        SessionEvent::AccountKeyRegistered { .. }
            | SessionEvent::AccountKeyRotated { .. }
            | SessionEvent::DeviceCertificateAdded { .. }
    )
}

/// Dearth mode is on for the event's workspace and the event is not a trust
/// bootstrap: an event no certified device vouches for waits.
fn entrance_closed(roster: &WorkspaceRoster, env: &SessionEventEnvelope) -> bool {
    roster.dearth_workspaces.contains(&env.workspace_id) && !is_trust_bootstrap(env)
}

/// An account key registration or rotation is published by a device of that
/// account: signed, and stamped as that account. Anyone else publishing one
/// for a victim's account id would make it disputed (every event the victim
/// signs is then quarantined).
fn key_publication_ok(env: &SessionEventEnvelope) -> bool {
    let account = match &env.payload {
        SessionEvent::AccountKeyRegistered { account_id, .. } | SessionEvent::AccountKeyRotated { account_id, .. } => {
            *account_id
        }
        _ => return true,
    };
    env.signer_device_id.is_some() && env.actor_stamp.as_ref().and_then(|s| s.actor.account_id) == Some(account)
}

/// Classify an envelope against the roster (see [`Verdict`]).
pub fn verdict_for(roster: &WorkspaceRoster, env: &SessionEventEnvelope) -> Verdict {
    if !key_publication_ok(env) {
        return Verdict::Quarantine(QuarantineReason::Impersonation);
    }
    let Some(device_id) = env.signer_device_id else {
        // A genuine event is ALWAYS signed — `append_signed` sets the signature and
        // the actor stamp together — so an unsigned envelope that nonetheless
        // carries an `actor_stamp` is provably forged: it claims an author with no
        // signature to prove it. It must be QUARANTINED, not merely held/accepted;
        // otherwise the default (Relay) ingest accepts it and projection-authz +
        // seed-selection trust the unverified stamped author — a workspace takeover
        // (a forged `MemberRoleChanged`/`MemberRemoved` or founder-stamped snapshot
        // stamped as the real Owner). An unsigned event with NO stamp claims no
        // author, so it stays the lenient `Unverifiable`.
        return if env.actor_stamp.is_some() {
            Verdict::Quarantine(QuarantineReason::Unsigned)
        } else if entrance_closed(roster, env) {
            Verdict::Unverifiable(QuarantineReason::PendingApproval)
        } else {
            Verdict::Unverifiable(QuarantineReason::Unsigned)
        };
    };
    // A3: an agent's own key (certified by its host, still bound to it).
    if let Some((agent_key, agent_id)) = roster.agent_keys.get(&device_id) {
        if verify_envelope(env, agent_key).is_err() {
            return Verdict::Quarantine(QuarantineReason::BadSignature);
        }
        let speaks_as_agent = env.actor_stamp.as_ref().is_some_and(|s| {
            s.actor.kind == hive_core::ActorKind::Agent && s.actor.id == agent_id.to_string() && s.actor.account_id.is_none()
        });
        return if speaks_as_agent { Verdict::Valid } else { Verdict::Quarantine(QuarantineReason::Impersonation) };
    }
    if roster.is_revoked(device_id) {
        return Verdict::Quarantine(QuarantineReason::RevokedDevice);
    }
    let Some(signing_key) = roster.public_key(device_id) else {
        if roster.pending.iter().any(|(_, d, _)| *d == device_id) && !is_trust_bootstrap(env) {
            return Verdict::Unverifiable(QuarantineReason::PendingApproval);
        }
        if entrance_closed(roster, env) {
            return Verdict::Unverifiable(QuarantineReason::PendingApproval);
        }
        // A fresh device id stamping itself as a member the workspace knows:
        // wait for that member's certificate of it, never accept on the stamp.
        let claims_known = env
            .actor_stamp
            .as_ref()
            .and_then(|s| s.actor.account_id.or_else(|| Uuid::parse_str(&s.actor.id).ok()))
            .is_some_and(|a| {
                roster.account_signing_key(a).is_some() || roster.is_disputed(a) || roster.removed_accounts.contains(&a)
            });
        if claims_known && !is_trust_bootstrap(env) {
            return Verdict::Unverifiable(QuarantineReason::UncertifiedDevice);
        }
        return Verdict::Unverifiable(QuarantineReason::UnknownDevice);
    };
    if verify_envelope(env, signing_key).is_err() {
        return Verdict::Quarantine(QuarantineReason::BadSignature);
    }
    // Impersonation: the stamped author's account must be one the signing device
    // is cert-verified to. A device legitimately certified to more than one
    // account (an identity transition) may sign as any of them — each cert is
    // signed by that account's own key, so this can't be forged. The device is
    // known here (public_key resolved above), so a stamp matching none of its
    // certified accounts is provably an impersonation.
    if let Some(stamp) = &env.actor_stamp {
        if let Some(claimed) = stamp.actor.account_id {
            // Contested identity: two different keys were registered for this
            // account, so we can't tell which is genuine. Fail closed — this is
            // what stops a forged `AccountKeyRegistered` under a victim's account
            // id from hijacking it (the victim's real registration now makes the
            // account disputed rather than silently losing first-registration).
            if roster.is_disputed(claimed) {
                return Verdict::Quarantine(QuarantineReason::DisputedAccountKey);
            }
            if !roster.device_signs_for(device_id, claimed) {
                return Verdict::Quarantine(QuarantineReason::Impersonation);
            }
        } else if let Ok(named) = Uuid::parse_str(&stamp.actor.id) {
            // No account on the stamp, but its actor id *is* a known account
            // (a member's actor id is their account id): the signer must sign
            // for it, or a member could leave the account off and act under
            // anyone's id (the fold and the author gate read the actor id).
            let known = roster.account_signing_key(named).is_some()
                || roster.is_disputed(named)
                || roster.removed_accounts.contains(&named);
            if known && !roster.device_signs_for(device_id, named) {
                return Verdict::Quarantine(QuarantineReason::Impersonation);
            }
        }
    }
    Verdict::Valid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity_store::{FileKeyVault, IdentityStore};
    use hive_core::{sign_envelope, ChatMessage, MessageRole, SessionEvent};

    fn signed_env(
        store: &IdentityStore<FileKeyVault>,
        device_id: Uuid,
        seq: i64,
    ) -> SessionEventEnvelope {
        let kp = store.device_keypair(device_id).unwrap();
        let mut env = SessionEventEnvelope::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            seq,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "Mara", "hi"),
            },
        );
        sign_envelope(&mut env, device_id, &kp);
        env
    }

    #[test]
    fn valid_signature_passes_unknown_and_tampered_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let store = IdentityStore::new(dir.path(), FileKeyVault::new(dir.path()));
        let id = store.bootstrap("Mara", "mara", "Mac").unwrap();
        let roster = DeviceRoster::new().with_device(&id.device);

        // 1) a properly signed envelope
        let good = signed_env(&store, id.device.id, 1);

        // 2) tampered after signing
        let mut tampered = signed_env(&store, id.device.id, 2);
        tampered.sequence = 999;

        // 3) signed by an unknown device
        let mut unknown = good.clone();
        unknown.signer_device_id = Some(Uuid::new_v4());

        // 4) unsigned
        let unsigned = SessionEventEnvelope::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            4,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "x", "y"),
            },
        );

        let outcome = verify_stream(
            vec![good, tampered, unknown, unsigned],
            &roster,
            false,
        );
        assert_eq!(outcome.valid.len(), 1);
        assert_eq!(outcome.quarantined.len(), 3);
        let reasons: Vec<_> = outcome.quarantined.iter().map(|(_, r)| *r).collect();
        assert!(reasons.contains(&QuarantineReason::BadSignature));
        assert!(reasons.contains(&QuarantineReason::UnknownDevice));
        assert!(reasons.contains(&QuarantineReason::Unsigned));
    }

    #[test]
    fn revoked_device_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let store = IdentityStore::new(dir.path(), FileKeyVault::new(dir.path()));
        let mut id = store.bootstrap("Mara", "mara", "Mac").unwrap();
        let env = signed_env(&store, id.device.id, 1);

        id.device.revoked_at = Some(hive_core::Timestamp::now());
        let roster = DeviceRoster::new().with_device(&id.device);

        let outcome = verify_stream(vec![env], &roster, false);
        assert_eq!(outcome.valid.len(), 0);
        assert_eq!(
            outcome.quarantined[0].1,
            QuarantineReason::RevokedDevice
        );
    }

    // --- S1 roster + verdicts ------------------------------------------------

    use hive_core::crypto::{DeviceCertificate, SigningKeypair};
    use hive_core::identity::{ActorIdentity, ActorKind, ActorStamp, WorkspaceMember, WorkspaceRole};
    use hive_core::{ChatSession, SessionEventEnvelope, Timestamp};

    /// A test principal: an account keypair + one device keypair, with the trust
    /// events (`AccountKeyRegistered`, `DeviceCertificateAdded`, `MemberAdded`)
    /// that put it in a roster.
    struct Principal {
        account_kp: SigningKeypair,
        account_id: Uuid,
        device_kp: SigningKeypair,
        device_id: Uuid,
    }

    fn principal() -> Principal {
        Principal {
            account_kp: SigningKeypair::generate().unwrap(),
            account_id: Uuid::new_v4(),
            device_kp: SigningKeypair::generate().unwrap(),
            device_id: Uuid::new_v4(),
        }
    }

    fn env(lamport: i64, payload: SessionEvent) -> SessionEventEnvelope {
        SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), lamport, payload)
    }

    fn trust_events(p: &Principal, lamport_base: i64) -> Vec<SessionEventEnvelope> {
        let cert = DeviceCertificate::issue(
            &p.account_kp,
            p.account_id,
            p.device_id,
            &p.device_kp.public_key_bytes(),
            Timestamp::epoch(),
        );
        let member = WorkspaceMember {
            id: p.account_id.to_string(),
            actor: ActorIdentity {
                id: p.account_id.to_string(),
                display_name: "P".into(),
                kind: ActorKind::Human,
                account_id: Some(p.account_id),
                device_id: Some(p.device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
                contact: None,
            },
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::epoch(),
        };
        vec![
            env(lamport_base, SessionEvent::MemberAdded { member, invite: None }),
            env(
                lamport_base + 1,
                SessionEvent::AccountKeyRegistered {
                    account_id: p.account_id,
                    signing_public_key: p.account_kp.public_key_bytes().to_vec(),
                },
            ),
            env(
                lamport_base + 2,
                SessionEvent::DeviceCertificateAdded { certificate: cert },
            ),
        ]
    }

    /// A content event signed by `signer`, stamping `claimed_account` as author.
    fn authored(
        signer_device: Uuid,
        signer_kp: &SigningKeypair,
        claimed_account: Uuid,
        lamport: i64,
    ) -> SessionEventEnvelope {
        let mut e = env(
            lamport,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "P", "hi"),
            },
        );
        e.actor_stamp = Some(ActorStamp {
            actor: ActorIdentity {
                id: claimed_account.to_string(),
                display_name: "P".into(),
                kind: ActorKind::Human,
                account_id: Some(claimed_account),
                device_id: Some(signer_device),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
                contact: None,
            },
            recorded_at: Timestamp::epoch(), cfg_frontier: None,
            presence: None, runtime_effect: false, prev: None });
        hive_core::sign_envelope(&mut e, signer_device, signer_kp);
        e
    }

    #[test]
    fn valid_account_key_rotation_re_pins_without_dispute() {
        // P2-2: an owner rotates K1 → K2 by signing K2 with K1 (succession proof),
        // and re-certs the device under K2. The roster re-pins to K2 without a
        // dispute, and the device stays trusted.
        let p = principal();
        let new_kp = SigningKeypair::generate().unwrap();
        let mut events = trust_events(&p, 1); // establishes K1 + device cert under K1

        let sig = p.account_kp.sign(&new_kp.public_key_bytes()).to_vec();
        events.push(env(
            10,
            SessionEvent::AccountKeyRotated {
                account_id: p.account_id,
                new_signing_public_key: new_kp.public_key_bytes().to_vec(),
                succession_signature: sig,
            },
        ));
        // Re-cert the device under K2 (its K1 cert no longer verifies vs pinned K2).
        let cert2 = DeviceCertificate::issue(
            &new_kp,
            p.account_id,
            p.device_id,
            &p.device_kp.public_key_bytes(),
            Timestamp::epoch(),
        );
        events.push(env(11, SessionEvent::DeviceCertificateAdded { certificate: cert2 }));

        let roster = build_roster(&events);
        assert!(!roster.is_disputed(p.account_id), "a valid succession must NOT dispute");
        let signed = authored(p.device_id, &p.device_kp, p.account_id, 20);
        assert_eq!(verdict_for(&roster, &signed), Verdict::Valid);
    }

    #[test]
    fn forged_account_key_rotation_is_disputed() {
        // A rotation whose succession signature is NOT by the current key (an
        // attacker trying to seize an established account) is disputed — fail closed.
        let p = principal();
        let attacker = SigningKeypair::generate().unwrap();
        let mut events = trust_events(&p, 1);
        let bad_sig = attacker.sign(&attacker.public_key_bytes()).to_vec(); // signed by attacker, not K1
        events.push(env(
            10,
            SessionEvent::AccountKeyRotated {
                account_id: p.account_id,
                new_signing_public_key: attacker.public_key_bytes().to_vec(),
                succession_signature: bad_sig,
            },
        ));
        assert!(
            build_roster(&events).is_disputed(p.account_id),
            "a rotation not signed by the current key must dispute"
        );
    }

    #[test]
    fn explicitly_revoked_device_is_quarantined_even_with_a_valid_cert() {
        // P1-3: a DeviceRevoked event revokes a single device without removing its
        // account — its signed events are quarantined even though its cert still
        // chains to a current member account.
        let p = principal();
        let mut events = trust_events(&p, 1);
        // Sanity: without revocation the device's event is Valid.
        let before = build_roster(&events);
        let signed = authored(p.device_id, &p.device_kp, p.account_id, 10);
        assert_eq!(verdict_for(&before, &signed), Verdict::Valid);

        // Revoke the device; now the same signed event is quarantined.
        events.push(env(20, SessionEvent::DeviceRevoked { device_id: p.device_id, watermark: Vec::new() }));
        let after = build_roster(&events);
        assert_eq!(
            verdict_for(&after, &signed),
            Verdict::Quarantine(QuarantineReason::RevokedDevice)
        );
    }

    #[test]
    fn conflicting_account_key_registration_disputes_the_account_and_fails_closed() {
        // P0-2 PoC: an attacker forges AccountKeyRegistered{victim_account, attacker_key}
        // (victim's account id is a guessable UUIDv5), racing the victim's genuine
        // registration. First-wins previously let the forgery pin the attacker's key
        // and impersonate the victim. Now the two different keys make the account
        // DISPUTED, and events stamped as it are quarantined — fail closed, no hijack.
        let victim = principal();
        let attacker_key = SigningKeypair::generate().unwrap();

        // Genuine trust events for the victim (registers victim's real key at
        // lamport 2), PLUS a forged registration of the SAME account id under the
        // attacker's key at a later lamport — a second, different key.
        let mut events = trust_events(&victim, 1);
        events.push(env(
            5,
            SessionEvent::AccountKeyRegistered {
                account_id: victim.account_id,
                signing_public_key: attacker_key.public_key_bytes().to_vec(),
            },
        ));
        let roster = build_roster(&events);
        assert!(roster.is_disputed(victim.account_id), "conflicting keys must dispute the account");

        // The victim's OWN genuinely-signed event (valid signature, certified
        // device) stamped as the victim is now quarantined because the account is
        // disputed — fail closed. Ambiguity is resolved as "trust neither," which
        // denies the attacker the takeover (their forged key can't be relied on).
        let genuine =
            authored(victim.device_id, &victim.device_kp, victim.account_id, 10);
        assert_eq!(
            verdict_for(&roster, &genuine),
            Verdict::Quarantine(QuarantineReason::DisputedAccountKey)
        );
    }

    #[test]
    fn unsigned_event_with_a_forged_stamp_is_quarantined_not_accepted() {
        // P0-1 PoC: a malicious member forges an UNSIGNED governance event stamped
        // as the real Owner. Genuine events are always signed (append_signed signs
        // + stamps together), so this is provably forged and MUST be quarantined —
        // not accepted (Relay mode), where projection-authz would trust the stamped
        // Owner and apply the takeover on every device.
        use crate::identity_verifier::{gate_ingest, IdentityCache, IdentityMode, IngestAction};

        let owner = principal();
        let roster = build_roster(&trust_events(&owner, 1));

        // Forged: unsigned (signer_device_id stays None) but stamped as the owner,
        // carrying a self-promotion to Owner.
        let mut forged = env(
            10,
            SessionEvent::MemberRoleChanged {
                change: hive_core::MemberRoleChange {
                    member_id: owner.account_id.to_string(),
                    old_role: WorkspaceRole::Contributor,
                    new_role: WorkspaceRole::Owner,
                }, watermark: Vec::new(),
            },
        );
        forged.actor_stamp = Some(ActorStamp {
            actor: ActorIdentity {
                id: owner.account_id.to_string(),
                display_name: "P".into(),
                kind: ActorKind::Human,
                account_id: Some(owner.account_id),
                device_id: Some(owner.device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
                contact: None,
            },
            recorded_at: Timestamp::epoch(), cfg_frontier: None,
            presence: None, runtime_effect: false, prev: None });
        assert!(forged.signer_device_id.is_none(), "PoC envelope must be unsigned");

        // The verdict must be Quarantine, and Relay-mode ingest must DROP it.
        assert_eq!(
            verdict_for(&roster, &forged),
            Verdict::Quarantine(QuarantineReason::Unsigned)
        );
        let cache = IdentityCache::new();
        assert!(
            matches!(
                gate_ingest(IdentityMode::Relay, verdict_for(&roster, &forged), None, None, &cache),
                IngestAction::Quarantine(_)
            ),
            "Relay-mode ingest must quarantine a forged unsigned stamped event, not Accept it",
        );

        // Sanity: an unsigned event with NO stamp claims no author, so it stays the
        // lenient Unverifiable — genuinely unauthored events aren't newly dropped.
        let unstamped = env(
            11,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "x", "y"),
            },
        );
        assert_eq!(
            verdict_for(&roster, &unstamped),
            Verdict::Unverifiable(QuarantineReason::Unsigned)
        );
    }

    #[test]
    fn roster_accepts_valid_rejects_tamper_and_impersonation() {
        let alice = principal();
        let roster = build_roster(&trust_events(&alice, 1));

        // 1) Alice's device signs, stamping Alice → Valid.
        let good = authored(alice.device_id, &alice.device_kp, alice.account_id, 100);
        assert_eq!(verdict_for(&roster, &good), Verdict::Valid);

        // 2) Tampered after signing → BadSignature (provably bad, quarantined).
        let mut tampered = good.clone();
        tampered.sequence = 9999;
        assert_eq!(
            verdict_for(&roster, &tampered),
            Verdict::Quarantine(QuarantineReason::BadSignature)
        );

        // 3) Alice's device signs but stamps a DIFFERENT account → Impersonation.
        let spoof = authored(alice.device_id, &alice.device_kp, Uuid::new_v4(), 101);
        assert_eq!(
            verdict_for(&roster, &spoof),
            Verdict::Quarantine(QuarantineReason::Impersonation)
        );
    }

    #[test]
    fn device_certified_to_multiple_accounts_may_sign_as_any() {
        // Identity transition: the SAME device is certified to an old account and
        // a new one (e.g. a local id, then a GitHub sign-in). Each cert is signed
        // by its own account key, so events stamped with EITHER account are
        // authentic — but a third, uncertified account is still impersonation.
        let p = principal();
        let old_account = p.account_id;
        let new_account = Uuid::new_v4();
        let new_account_kp = SigningKeypair::generate().unwrap();

        let old_cert = DeviceCertificate::issue(
            &p.account_kp, old_account, p.device_id, &p.device_kp.public_key_bytes(), Timestamp::epoch(),
        );
        let new_cert = DeviceCertificate::issue(
            &new_account_kp, new_account, p.device_id, &p.device_kp.public_key_bytes(), Timestamp::epoch(),
        );
        let mk_member = |acct: Uuid| WorkspaceMember {
            id: acct.to_string(),
            actor: ActorIdentity {
                id: acct.to_string(),
                display_name: "P".into(),
                kind: ActorKind::Human,
                account_id: Some(acct),
                device_id: Some(p.device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
                contact: None,
            },
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::epoch(),
        };
        let events = vec![
            env(1, SessionEvent::MemberAdded { invite: None, member: mk_member(old_account) }),
            env(2, SessionEvent::MemberAdded { invite: None, member: mk_member(new_account) }),
            env(3, SessionEvent::AccountKeyRegistered {
                account_id: old_account,
                signing_public_key: p.account_kp.public_key_bytes().to_vec(),
            }),
            env(4, SessionEvent::AccountKeyRegistered {
                account_id: new_account,
                signing_public_key: new_account_kp.public_key_bytes().to_vec(),
            }),
            env(5, SessionEvent::DeviceCertificateAdded { certificate: old_cert }),
            env(6, SessionEvent::DeviceCertificateAdded { certificate: new_cert }),
        ];
        let roster = build_roster(&events);

        // Signing as the new account → Valid (the device is certified to it).
        let as_new = authored(p.device_id, &p.device_kp, new_account, 100);
        assert_eq!(verdict_for(&roster, &as_new), Verdict::Valid);
        // Signing as the old account → still Valid.
        let as_old = authored(p.device_id, &p.device_kp, old_account, 101);
        assert_eq!(verdict_for(&roster, &as_old), Verdict::Valid);
        // Signing as a third, uncertified account → Impersonation.
        let as_other = authored(p.device_id, &p.device_kp, Uuid::new_v4(), 102);
        assert_eq!(
            verdict_for(&roster, &as_other),
            Verdict::Quarantine(QuarantineReason::Impersonation)
        );
    }

    #[test]
    fn unknown_and_unsigned_are_unverifiable_not_dropped() {
        let alice = principal();
        let roster = build_roster(&trust_events(&alice, 1));

        // A device with no cert in the roster → held, not rejected.
        let stranger = principal();
        let unknown = authored(stranger.device_id, &stranger.device_kp, stranger.account_id, 100);
        assert_eq!(
            verdict_for(&roster, &unknown),
            Verdict::Unverifiable(QuarantineReason::UnknownDevice)
        );

        // Unsigned → held, not rejected.
        let unsigned = env(101, SessionEvent::SessionTitleChanged { title: "x".into() });
        assert_eq!(
            verdict_for(&roster, &unsigned),
            Verdict::Unverifiable(QuarantineReason::Unsigned)
        );
    }

    #[test]
    fn cert_not_chaining_to_account_key_is_ignored() {
        let alice = principal();
        // Forge a cert for alice.device signed by a DIFFERENT (attacker) account key.
        let attacker = SigningKeypair::generate().unwrap();
        let forged = DeviceCertificate::issue(
            &attacker,
            alice.account_id, // claims Alice's account…
            alice.device_id,
            &alice.device_kp.public_key_bytes(),
            Timestamp::epoch(),
        );
        let events = vec![
            env(1, SessionEvent::AccountKeyRegistered {
                account_id: alice.account_id,
                signing_public_key: alice.account_kp.public_key_bytes().to_vec(), // real key
            }),
            env(2, SessionEvent::DeviceCertificateAdded { certificate: forged }),
        ];
        let roster = build_roster(&events);
        // The forged cert doesn't verify against Alice's real account key → device
        // never enters the roster.
        assert!(roster.public_key(alice.device_id).is_none());
    }

    #[test]
    fn removed_member_devices_are_revoked() {
        let alice = principal();
        let mut events = trust_events(&alice, 1);
        events.push(env(50, SessionEvent::MemberRemoved { member_id: alice.account_id.to_string(), watermark: Vec::new() }));
        let roster = build_roster(&events);

        assert!(roster.is_revoked(alice.device_id), "removed account's device is revoked");
        let e = authored(alice.device_id, &alice.device_kp, alice.account_id, 100);
        assert_eq!(
            verdict_for(&roster, &e),
            Verdict::Quarantine(QuarantineReason::RevokedDevice)
        );
    }

    #[test]
    fn roster_is_order_independent() {
        let alice = principal();
        let bob = principal();
        let mut events = trust_events(&alice, 1);
        events.extend(trust_events(&bob, 10));

        let forward = build_roster(&events);
        let mut reversed = events.clone();
        reversed.reverse();
        let backward = build_roster(&reversed);

        for p in [&alice, &bob] {
            assert_eq!(
                forward.public_key(p.device_id),
                backward.public_key(p.device_id),
                "roster must be identical regardless of event order",
            );
            assert!(forward.public_key(p.device_id).is_some());
        }
        let _ = ChatSession::new("x", Uuid::nil(), "r"); // silence unused import in some cfgs
    }
}
