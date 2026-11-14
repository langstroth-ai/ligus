//! The workspace roster over the config log's history ([`ConfigTimeline`]),
//! and the running roster a fold keeps while it walks one log
//! ([`RosterTracker`]).
//!
//! The config log is folded once (with its own authorization, in canonical
//! order). After every config event whose effect could change who may do
//! what, the tracker records a new [`RosterState`]; unchanged positions share
//! the previous one, so a log of heartbeats costs one `Arc` per event.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use uuid::Uuid;

use super::HoldReason;
use crate::events::{SessionEvent, SessionEventEnvelope};
use crate::identity::{ActorKind, CfgFrontier, WorkspaceRole};
use crate::session::ChatSession;

/// One member as authorization sees them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MemberState {
    pub role: WorkspaceRole,
    pub account_id: Option<Uuid>,
    pub kind: ActorKind,
}

/// One workspace agent's binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentState {
    pub host_id: String,
    pub runtime_id: String,
    pub owner: String,
}

/// One device, as the trust events on this log certify it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct DeviceState {
    /// Accounts whose (undisputed) key certified this device.
    pub accounts: BTreeSet<Uuid>,
    pub revoked: bool,
    /// The device's signing key, from its certificate.
    pub public_key: Vec<u8>,
    /// Accounts this device acts for without holding their key: each one's
    /// certificate for it was only ever published by another device (the
    /// owner's desktop approving `hive serve enroll`), or that a certified,
    /// non-delegated device of the account published for it (sticky: the
    /// device re-publishing the certificate it was handed doesn't undo it). A
    /// device that holds an account key publishes its own certificate for
    /// that account. A vote it
    /// signs for one of these accounts doesn't count toward a human gate (see
    /// `authz::authorize`).
    pub delegated_for: BTreeSet<Uuid>,
}

impl DeviceState {
    /// Whether this device signs for `account` as a delegated host.
    pub fn delegated_for(&self, account: Uuid) -> bool {
        self.delegated_for.contains(&account)
    }
}

/// An agent signing key certified by its host device (A3).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentKeyState {
    pub agent_id: Uuid,
    pub host_device_id: Uuid,
    pub signing_public_key: Vec<u8>,
}

/// Who may do what at one position of the config log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct RosterState {
    pub members: BTreeMap<String, MemberState>,
    pub agents: BTreeMap<Uuid, AgentState>,
    pub devices: BTreeMap<Uuid, DeviceState>,
    /// Agent keys by `key_id` (an agent-signed envelope's `signer_device_id`).
    pub agent_keys: BTreeMap<Uuid, AgentKeyState>,
    pub dearth: bool,
    /// The workspace rules version in force (1 when never set).
    pub rules: u32,
    /// The path zones (Admin-set review bars) in force.
    pub zones: Vec<crate::flow::PathZone>,
    /// The authorized workspace-key rotations so far (B1).
    pub key_epochs: crate::keys::KeyEpochs,
    /// Each device's own published X25519 key-agreement key (B1): the only
    /// keys a rotation seals to.
    pub ka_keys: BTreeMap<Uuid, Vec<u8>>,
    /// Each account's registered passkeys and recovery keys, by credential
    /// id (B3).
    pub passkeys: BTreeMap<Uuid, BTreeMap<String, crate::presence::PasskeyKey>>,
    /// Accounts without a credential that an Admin's presence-confirmed act
    /// under rules v2 lets register a first passkey, once (B.4.6): adding
    /// them as a new member, or re-granting their role after their
    /// credentials were removed.
    pub passkey_grants: BTreeSet<Uuid>,
}

impl RosterState {
    pub fn role_of(&self, actor_id: &str) -> Option<WorkspaceRole> {
        self.members.get(actor_id).map(|m| m.role)
    }

    /// Whether `account` certified `device` (and the device is not revoked).
    pub fn account_certified(&self, account: Uuid, device: Uuid) -> bool {
        self.devices.get(&device).is_some_and(|d| d.accounts.contains(&account))
    }

    /// Whether a device is certified (by some undisputed account) and not
    /// revoked.
    pub fn device_trusted(&self, device: Uuid) -> bool {
        self.devices.get(&device).is_some_and(|d| !d.revoked && !d.accounts.is_empty())
    }

    /// Whether `host` may hold keys for `agent_id` here: the agent is bound
    /// to that host, or, unbound, its owner's account certified the host.
    pub fn agent_bound_to(&self, agent_id: Uuid, host: Uuid) -> bool {
        let Some(agent) = self.agents.get(&agent_id) else { return false };
        if !self.device_trusted(host) {
            return false;
        }
        if agent.host_id.is_empty() {
            let owner_account = self
                .members
                .get(&agent.owner)
                .and_then(|m| m.account_id)
                .or_else(|| Uuid::parse_str(&agent.owner).ok());
            owner_account.is_some_and(|a| self.account_certified(a, host))
        } else {
            agent.host_id == host.to_string()
        }
    }

    /// The agent key `key_id`, if it is valid at this position: certified,
    /// and its agent still bound to the certifying host. A rebinding
    /// invalidates the old key from that position on.
    pub fn valid_agent_key(&self, key_id: Uuid) -> Option<&AgentKeyState> {
        let key = self.agent_keys.get(&key_id)?;
        self.agent_bound_to(key.agent_id, key.host_device_id).then_some(key)
    }

    /// Who a rotation at this position seals the new key to (B1): every
    /// device that published its own key-agreement key, is certified and not
    /// revoked, and acts for a current member's account (a member's own
    /// devices and the serve or worker boxes enrolled for them, which is how
    /// an agent's host gets the key). A device an agent is merely bound to
    /// is not enough: a removed member's box still hosting an agent would
    /// otherwise get every later key. Device id → X25519 public key.
    pub fn rotation_recipients(&self) -> BTreeMap<Uuid, Vec<u8>> {
        let accounts: BTreeSet<Uuid> = self.members.values().filter_map(|m| m.account_id).collect();
        self.ka_keys
            .iter()
            .filter(|(device, _)| {
                let Some(d) = self.devices.get(device) else { return false };
                !d.revoked && d.accounts.iter().any(|a| accounts.contains(a))
            })
            .map(|(d, k)| (*d, k.clone()))
            .collect()
    }

    fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }
}

/// The running roster while one log is folded (see the module docs).
#[derive(Debug, Clone, Default)]
pub struct RosterTracker {
    pub state: RosterState,
    account_keys: BTreeMap<Uuid, Vec<u8>>,
    disputed: BTreeSet<Uuid>,
    certs: Vec<crate::crypto::DeviceCertificate>,
    /// (device, account) certificates the device published itself (the
    /// envelope's signer is the certified device).
    self_published: BTreeSet<(Uuid, Uuid)>,
    /// (device, account) certificates that a *different*, certified and
    /// non-delegated device of that account published (an enrollment). Sticky:
    /// the enrolled device re-publishing its certificate later doesn't undo it
    /// (it only ever holds certificates it was handed).
    enrolled_by_other: BTreeSet<(Uuid, Uuid)>,
}

impl RosterTracker {
    /// Seed from the base snapshot.
    pub fn seeded(session: &ChatSession) -> Self {
        let mut t = Self::default();
        t.state.rules = 1;
        t.refresh(session);
        t
    }

    /// Fold the trust side of an event (account keys and device certificates
    /// are self-validating, not role-gated) and, for an allowed event, the
    /// roster side read back from the fold. Returns whether the state changed.
    pub fn observe(&mut self, env: &SessionEventEnvelope, allowed: bool, fold: &ChatSession) -> bool {
        let before = self.state.clone();
        if allowed && self.state.rules.min(super::RULES_SUPPORTED) >= 2 {
            self.note_grant(&env.payload);
        }
        match &env.payload {
            SessionEvent::AccountKeyRegistered { account_id, signing_public_key } => {
                match self.account_keys.get(account_id) {
                    Some(k) if k != signing_public_key => {
                        self.disputed.insert(*account_id);
                    }
                    Some(_) => {}
                    None => {
                        self.account_keys.insert(*account_id, signing_public_key.clone());
                    }
                }
                self.rebuild_devices(fold);
            }
            // A rotation the log refused (under rules v2, one without the
            // account's presence) re-pins nothing.
            SessionEvent::AccountKeyRotated { account_id, new_signing_public_key, succession_signature } if allowed => {
                match self.account_keys.get(account_id) {
                    Some(k) if k == new_signing_public_key => {}
                    Some(k) => {
                        if crate::crypto::verify(k, new_signing_public_key, succession_signature).is_ok() {
                            self.account_keys.insert(*account_id, new_signing_public_key.clone());
                        } else {
                            self.disputed.insert(*account_id);
                        }
                    }
                    None => {
                        self.disputed.insert(*account_id);
                    }
                }
                self.rebuild_devices(fold);
            }
            // Only a certificate the log allowed certifies (under rules v2 a
            // new device of an account with a passkey needs its presence).
            SessionEvent::DeviceCertificateAdded { certificate } if allowed => {
                self.certs.push(certificate.clone());
                let key = (certificate.device_id, certificate.account_id);
                match env.signer_device_id {
                    Some(s) if s == certificate.device_id => {
                        self.self_published.insert(key);
                    }
                    // Only a publisher that itself holds the account (certified,
                    // not revoked, not delegated, at this position) enrolls:
                    // anyone else re-publishing a cert can't mark it delegated.
                    Some(s)
                        if self.state.devices.get(&s).is_some_and(|d| {
                            !d.revoked && d.accounts.contains(&certificate.account_id) && !d.delegated_for(certificate.account_id)
                        }) =>
                    {
                        self.enrolled_by_other.insert(key);
                    }
                    _ => {}
                }
                self.rebuild_devices(fold);
            }
            SessionEvent::AgentKeyCertified { cert } if allowed => {
                self.state.agent_keys.insert(
                    cert.key_id,
                    AgentKeyState {
                        agent_id: cert.agent_id,
                        host_device_id: cert.host_device_id,
                        signing_public_key: cert.signing_public_key.clone(),
                    },
                );
            }
            SessionEvent::WorkspaceRulesSet { version } if allowed => {
                self.state.rules = *version;
            }
            SessionEvent::WorkspaceKeyRotated { rotation } if allowed => {
                self.state.key_epochs.push(rotation, env.canonical_key());
            }
            SessionEvent::DeviceKeyAgreementPublished { device_id, ka_public } if allowed => {
                self.state.ka_keys.insert(*device_id, ka_public.clone());
            }
            SessionEvent::PasskeyRegistered { credential } if allowed => {
                self.state.passkey_grants.remove(&credential.account_id);
                self.state
                    .passkeys
                    .entry(credential.account_id)
                    .or_default()
                    .insert(credential.credential_id.clone(), crate::presence::PasskeyKey::from(credential));
            }
            SessionEvent::PasskeyRemoved { account_id, credential_id } if allowed => {
                if let Some(m) = self.state.passkeys.get_mut(account_id) {
                    m.remove(credential_id);
                    if m.is_empty() {
                        self.state.passkeys.remove(account_id);
                    }
                }
            }
            e if allowed && touches_roster(e) => self.refresh(fold),
            _ => {}
        }
        self.state != before
    }

    /// Rules v2 (B.4.6): an allowed act that lets an account without a
    /// credential register a first passkey. Under rules v2 both need an
    /// Admin's presence, except a self-enrollment through an invite an Admin
    /// issued (which itself needed presence when issued under v2). An
    /// idempotent self re-add of an existing member grants nothing.
    fn note_grant(&mut self, e: &SessionEvent) {
        let account = match e {
            SessionEvent::MemberAdded { member, .. } if !self.state.members.contains_key(&member.id) => member.actor.account_id,
            SessionEvent::MemberRoleChanged { change } => self.state.members.get(&change.member_id).and_then(|m| m.account_id),
            _ => None,
        };
        if let Some(a) = account.filter(|a| self.state.passkeys.get(a).is_none_or(|m| m.is_empty())) {
            self.state.passkey_grants.insert(a);
        }
    }

    fn refresh(&mut self, fold: &ChatSession) {
        self.state.members = fold
            .members
            .iter()
            .map(|m| (m.id.clone(), MemberState { role: m.role, account_id: m.actor.account_id, kind: m.actor.kind }))
            .collect();
        self.state.agents = fold
            .workspace_agents
            .iter()
            .map(|a| (a.id, AgentState { host_id: a.host_id.clone(), runtime_id: a.runtime_id.clone(), owner: a.owner_actor_id.clone() }))
            .collect();
        self.state.dearth = fold.guards.dearth_on().is_some();
        self.state.zones = fold.flow.zones.clone();
        self.rebuild_devices(fold);
    }

    fn rebuild_devices(&mut self, fold: &ChatSession) {
        let mut devices: BTreeMap<Uuid, DeviceState> = BTreeMap::new();
        for cert in &self.certs {
            if self.disputed.contains(&cert.account_id) {
                continue;
            }
            let Some(key) = self.account_keys.get(&cert.account_id) else { continue };
            if cert.verify(key).is_ok() {
                let d = devices.entry(cert.device_id).or_default();
                d.accounts.insert(cert.account_id);
                d.public_key = cert.device_public_key.clone();
                let key = (cert.device_id, cert.account_id);
                if !self.self_published.contains(&key) || self.enrolled_by_other.contains(&key) {
                    d.delegated_for.insert(cert.account_id);
                }
            }
        }
        for d in &fold.guards.revoked_devices {
            devices.entry(*d).or_default().revoked = true;
        }
        self.state.devices = devices;
    }
}

/// Events whose effect can change the roster state.
fn touches_roster(e: &SessionEvent) -> bool {
    matches!(
        e,
        SessionEvent::MemberAdded { .. }
            | SessionEvent::MemberRemoved { .. }
            | SessionEvent::MemberRoleChanged { .. }
            | SessionEvent::AgentRosterUpdated { .. }
            | SessionEvent::AgentUpserted { .. }
            | SessionEvent::AgentRemoved { .. }
            | SessionEvent::DeviceRevoked { .. }
            | SessionEvent::DearthModeChanged { .. }
            | SessionEvent::JoinApproved { .. }
            | SessionEvent::SessionSnapshot { .. }
    ) || matches!(
        e,
        SessionEvent::FlowRecorded {
            record: crate::flow::FlowRecord::Zone(_) | crate::flow::FlowRecord::ZoneRemoved { .. }
        }
    )
}

/// How far past this device's newest config event a frontier may claim to be
/// and still be waited for (A-14).
pub const FRONTIER_SLACK: u64 = 4096;

/// The roster at every position of one workspace's config log.
#[derive(Debug, Clone, Default)]
pub struct ConfigTimeline {
    /// Canonical keys of the folded config events, ascending.
    keys: Vec<(u64, Uuid)>,
    /// For each key, the index in `states` in force after that event.
    at: Vec<u32>,
    /// `states[0]` is the base seed's roster.
    states: Vec<Arc<RosterState>>,
    /// Every config event this device holds (folded or not) → its lamport.
    held: HashMap<Uuid, u64>,
    /// Every rotation body on the log, authorized or not: keys a device can
    /// open from these are read-only candidates (B1; never sealed under
    /// unless authorized).
    rotations: Vec<crate::keys::SignedRotation>,
    fingerprint: u64,
}

impl ConfigTimeline {
    /// Start a timeline at the base seed.
    pub fn begin(seed: RosterState) -> Self {
        Self { states: vec![Arc::new(seed)], ..Default::default() }
    }

    /// Note an event of the config log this device holds (folded or not).
    pub fn note_held(&mut self, event_id: Uuid, lamport: u64) {
        self.held.insert(event_id, lamport);
    }

    /// Note a rotation body seen on the log (authorized or not).
    pub fn note_rotation(&mut self, rotation: &crate::keys::SignedRotation) {
        if !self.rotations.contains(rotation) {
            self.rotations.push(rotation.clone());
        }
    }

    /// Every rotation body seen on the log, authorized or not.
    pub fn rotation_candidates(&self) -> &[crate::keys::SignedRotation] {
        &self.rotations
    }

    /// Record the state after the folded event `key` (`changed` = a new state).
    pub fn push(&mut self, key: (u64, Uuid), state: &RosterState, changed: bool) {
        if changed {
            self.states.push(Arc::new(state.clone()));
        }
        self.keys.push(key);
        self.at.push((self.states.len() - 1) as u32);
    }

    /// [`finish`](Self::finish), or an empty timeline when the fold had no
    /// base (no snapshot): nothing then resolves, so every frontier holds.
    pub fn finish_if_begun(self) -> Self {
        if self.states.is_empty() {
            self
        } else {
            self.finish()
        }
    }

    /// Seal: compute the fingerprint of what the timeline decides.
    pub fn finish(mut self) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut last = u32::MAX;
        for (key, idx) in self.keys.iter().zip(&self.at) {
            if *idx != last {
                key.hash(&mut h);
                self.states[*idx as usize].fingerprint().hash(&mut h);
                last = *idx;
            }
        }
        self.states[0].fingerprint().hash(&mut h);
        self.fingerprint = h.finish().max(1);
        self
    }

    /// A stable fingerprint of the roster history (changes only when some
    /// position's roster changes), for keying cached chat projections.
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// How many config events this device holds.
    pub fn events_held(&self) -> usize {
        self.held.len()
    }

    /// Whether the config event `event_id` is held on this device.
    pub fn holds(&self, event_id: Uuid) -> bool {
        self.held.contains_key(&event_id)
    }

    /// The newest folded config event: the frontier a new event stamps.
    pub fn head(&self) -> Option<CfgFrontier> {
        self.keys.last().map(|&(lamport, event_id)| CfgFrontier { lamport, event_id })
    }

    /// The roster in force now (empty for a log without a base).
    pub fn current(&self) -> &RosterState {
        static EMPTY: std::sync::OnceLock<RosterState> = std::sync::OnceLock::new();
        self.states.last().map(|s| &**s).unwrap_or_else(|| EMPTY.get_or_init(RosterState::default))
    }

    /// The roster after every folded config event up to and including
    /// `frontier`. A frontier naming an event this device doesn't hold is
    /// held back ([`HoldReason::ConfigFrontierAhead`]); one that names a held
    /// event under the wrong lamport is forged (`Err(None)`).
    ///
    /// A-14: a hold is bounded. A frontier naming an event this device doesn't
    /// hold, at a lamport the config log has already passed, or more than
    /// [`FRONTIER_SLACK`] past its newest event, can't be waited for: it is
    /// treated like a forged one (the event is left out, not held, so it
    /// blocks no side effect). The fold is re-run as events arrive, so a
    /// genuinely late config event still validates it then.
    pub fn at(&self, frontier: CfgFrontier) -> Result<&RosterState, Option<HoldReason>> {
        match self.held.get(&frontier.event_id) {
            None => {
                let newest = self.held.values().copied().max().unwrap_or(0);
                if !self.held.is_empty() && (frontier.lamport < newest || frontier.lamport > newest.saturating_add(FRONTIER_SLACK)) {
                    return Err(None);
                }
                return Err(Some(HoldReason::ConfigFrontierAhead));
            }
            Some(l) if *l != frontier.lamport => return Err(None),
            Some(_) => {}
        }
        if self.states.is_empty() {
            return Err(Some(HoldReason::ConfigFrontierAhead));
        }
        let n = self.keys.partition_point(|k| *k <= frontier.key());
        let idx = if n == 0 { 0 } else { self.at[n - 1] as usize };
        Ok(&self.states[idx])
    }
}
