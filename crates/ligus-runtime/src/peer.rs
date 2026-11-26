//! Direct peer-to-peer sync foundation (P1).
//!
//! This is the transport-agnostic seam that a real networked implementation
//! (iroh-backed: ed25519 node ids, hole-punching, relay fallback) plugs into
//! later. P1 ships the identity/contact model, a shareable "friend code", a
//! [`PeerLink`] trait, an in-memory [`LoopbackLink`] for tests, and a
//! [`PeerSync`] that exchanges signed envelopes over any link — mirroring the
//! relay [`crate::sync_engine::SyncEngine`] so it slots behind the same seam.
//!
//! No real networking here; that's P2.

use std::collections::HashSet;

use ligus_core::SessionEventEnvelope;

use crate::identity_verifier::{gate_ingest, IdentityCache, IdentityMode, IngestAction};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::envelope_verifier::verdict_for;
use crate::event_store::EventStore;

#[derive(Debug, Error)]
pub enum PeerError {
    #[error("peer link closed")]
    Closed,
    #[error("encode/decode error: {0}")]
    Codec(#[from] serde_json::Error),
    #[error("store error: {0}")]
    Store(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("invalid peer id: {0}")]
    BadPeerId(String),
}

/// A peer's stable identity — the encoded ed25519 public key. Maps directly to
/// an iroh `NodeId` in P2 (iroh node ids are ed25519 public keys too).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(pub String);

/// Current code prefix. Codes are `hive_` + base64url(public key) — ~48 chars
/// for a 32-byte key, vs ~74 for the old hex form.
const CODE_PREFIX: &str = "hive_";
/// Legacy prefix: `hivepeer1:` + lowercase hex. Still accepted on input so codes
/// shared before the shortening keep working.
const LEGACY_PREFIX: &str = "hivepeer1:";

impl PeerId {
    /// Build from raw public-key bytes (lowercase hex — the canonical internal
    /// form; iroh node-id parsing and contact storage both expect hex).
    pub fn from_public_key(bytes: &[u8]) -> Self {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        PeerId(s)
    }

    /// Raw public-key bytes (decoded from the hex inner form).
    fn key_bytes(&self) -> Option<Vec<u8>> {
        hex_to_bytes(&self.0)
    }

    /// A shareable "friend code" you hand to someone so they can add you. The
    /// public key is base64url-encoded (no padding) to keep it compact.
    pub fn to_code(&self) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        match self.key_bytes() {
            Some(bytes) => format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)),
            // Non-hex inner value (shouldn't happen): fall back to legacy form.
            None => format!("{LEGACY_PREFIX}{}", self.0),
        }
    }

    /// Parse a friend code back into a peer id. Accepts the compact `hive_`
    /// (base64url) form and the legacy `hivepeer1:` (hex) form. Tolerates
    /// surrounding whitespace; returns `None` for anything else.
    pub fn from_code(code: &str) -> Option<PeerId> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let code = code.trim();
        if let Some(b64) = code.strip_prefix(CODE_PREFIX) {
            let bytes = URL_SAFE_NO_PAD.decode(b64.trim()).ok()?;
            return (!bytes.is_empty()).then(|| PeerId::from_public_key(&bytes));
        }
        if let Some(hex) = code.strip_prefix(LEGACY_PREFIX) {
            let hex = hex.trim();
            return (!hex.is_empty()).then(|| PeerId(hex.to_string()));
        }
        None
    }
}

/// Decode a lowercase/uppercase hex string into bytes (even length only).
fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// A saved peer ("friend") you can connect to directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub peer_id: PeerId,
    #[serde(default)]
    pub label: String,
}

/// An authenticated, ordered, bidirectional byte channel to known peers. The
/// loopback impl is in-process; the iroh impl (P2) does real NAT traversal.
#[allow(async_fn_in_trait)] // used generically, never as `dyn`
pub trait PeerLink {
    /// Send bytes to a connected peer.
    async fn send(&self, to: &PeerId, data: Vec<u8>) -> Result<(), PeerError>;
    /// Next inbound `(sender, bytes)`, or `None` once the link closes. Takes
    /// `&self` (interior mutability) so a node can be shared between a sender
    /// and a receiver task concurrently.
    async fn recv(&self) -> Option<(PeerId, Vec<u8>)>;
    /// This end's own peer id.
    fn local_id(&self) -> &PeerId;
}

/// An in-memory link pair wiring two ends together — for tests and as the
/// reference behavior the networked impl must match.
pub struct LoopbackLink {
    local: PeerId,
    tx: mpsc::UnboundedSender<(PeerId, Vec<u8>)>,
    rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<(PeerId, Vec<u8>)>>,
}

impl LoopbackLink {
    /// Create two ends connected to each other.
    pub fn pair(a: PeerId, b: PeerId) -> (LoopbackLink, LoopbackLink) {
        let (to_a, a_inbox) = mpsc::unbounded_channel();
        let (to_b, b_inbox) = mpsc::unbounded_channel();
        let la = LoopbackLink {
            local: a.clone(),
            tx: to_b,
            rx: tokio::sync::Mutex::new(a_inbox),
        };
        let lb = LoopbackLink {
            local: b,
            tx: to_a,
            rx: tokio::sync::Mutex::new(b_inbox),
        };
        (la, lb)
    }
}

impl PeerLink for LoopbackLink {
    async fn send(&self, _to: &PeerId, data: Vec<u8>) -> Result<(), PeerError> {
        // The loopback has a single remote; tag the message with our own id.
        self.tx
            .send((self.local.clone(), data))
            .map_err(|_| PeerError::Closed)
    }
    async fn recv(&self) -> Option<(PeerId, Vec<u8>)> {
        self.rx.lock().await.recv().await
    }
    fn local_id(&self) -> &PeerId {
        &self.local
    }
}

/// Exchanges signed envelopes with peers over a [`PeerLink`]. Transport-
/// agnostic: it serializes/deserializes envelopes and tracks which it has
/// already seen, so the same instance can broadcast and ingest without loops.
pub struct PeerSync {
    /// Each copy seen, by its event id and signature: another signer's copy
    /// under an id is its own envelope to carry (`ligus_core::copies`).
    seen: HashSet<(Uuid, Option<Vec<u8>>)>,
    /// The workspaces this link carries. Only their events are sent, and an
    /// inbound event of any other workspace is dropped: a store holds every
    /// workspace on the device (its local one included), and a peer link must
    /// never become a way to read, or inject into, a workspace it isn't for.
    workspaces: HashSet<Uuid>,
    /// Same identity policy as the relay path, so P2P isn't a weaker door: under
    /// Option C a not-yet-GitHub-verified event is held, not silently accepted.
    #[cfg(test)]
    identity_mode: IdentityMode,
    identity_cache: IdentityCache,
}

impl PeerSync {
    /// A sync that carries exactly `workspaces` (empty = carries nothing).
    pub fn new(workspaces: impl IntoIterator<Item = Uuid>) -> Self {
        Self {
            seen: HashSet::new(),
            workspaces: workspaces.into_iter().collect(),
            #[cfg(test)]
            identity_mode: IdentityMode::Relay,
            identity_cache: IdentityCache::new(),
        }
    }

    /// Re-point the link at `workspaces` (the connected room changed). Events
    /// already sent stay sent; newly in-scope ones go out on the next push.
    pub fn set_workspaces(&mut self, workspaces: impl IntoIterator<Item = Uuid>) {
        self.workspaces = workspaces.into_iter().collect();
    }

    /// Override the identity-verification mode (tests only, mirrors `SyncEngine`).
    #[cfg(test)]
    pub fn with_identity_mode(mut self, mode: IdentityMode) -> Self {
        self.identity_mode = mode;
        self
    }

    /// Serialize every local envelope not yet shared, marking them seen.
    pub fn take_unpushed(&mut self, store: &EventStore) -> Result<Vec<Vec<u8>>, PeerError> {
        let mut out = Vec::new();
        let ids = store
            .list_session_ids()
            .map_err(|e| PeerError::Store(e.to_string()))?;
        for session_id in ids {
            // Every copy held, not only the first of an id to arrive here,
            // of each log (workspace, session) this peer shares.
            let claims = store
                .workspaces_claiming(session_id)
                .map_err(|e| PeerError::Store(e.to_string()))?;
            let mut envs = Vec::new();
            for ws in claims.into_iter().filter(|w| self.workspaces.contains(w)) {
                envs.extend(
                    store
                        .list_copies_in(ws, session_id)
                        .map_err(|e| PeerError::Store(e.to_string()))?,
                );
            }
            for env in envs {
                if self.workspaces.contains(&env.workspace_id)
                    && self.seen.insert((env.event_id, env.signature.clone()))
                {
                    out.push(serde_json::to_vec(&env)?);
                }
            }
        }
        Ok(out)
    }

    /// Send all unpushed envelopes to a peer over `link`.
    pub async fn push_to<L: PeerLink>(
        &mut self,
        link: &L,
        to: &PeerId,
        store: &EventStore,
    ) -> Result<usize, PeerError> {
        let batch = self.take_unpushed(store)?;
        let n = batch.len();
        for data in batch {
            link.send(to, data).await?;
        }
        Ok(n)
    }

    /// [`PeerSync::apply`] for an envelope `from` sent: only from one of
    /// `contacts` (the peers this device added by their code). The transport
    /// authenticates the sender's endpoint key, so anyone else who dials this
    /// endpoint is dropped before anything is parsed, held or kept as a
    /// lamport witness: a stranger can't fill the held table, or relay a
    /// removed member's copies here.
    pub fn apply_from(
        &mut self,
        store: &mut EventStore,
        from: &PeerId,
        contacts: &[Contact],
        data: &[u8],
    ) -> Result<bool, PeerError> {
        if !contacts
            .iter()
            .any(|c| c.peer_id.0.eq_ignore_ascii_case(&from.0))
        {
            tracing::warn!(peer = %from.0, "dropped a peer message from an endpoint that isn't a contact");
            return Ok(false);
        }
        self.apply(store, data)
    }

    /// Apply one received envelope into the store. Returns whether it was new.
    ///
    /// Verify-on-ingest: the P2P transport is untrusted, so — exactly like the
    /// relay path ([`crate::sync_engine::SyncEngine::apply_fetched`]) — build the
    /// trust roster from what we already hold plus this envelope's own trust
    /// events, then classify it. A **provably bad** envelope (bad signature,
    /// revoked device, impersonation) is rejected outright: it is *not* ingested,
    /// and `apply` returns `Ok(false)`. An envelope that can't be judged yet (a
    /// device whose certificate for the account it claims hasn't arrived, a
    /// disputed account key) is held in the device's held table and judged
    /// again as the roster grows, never dropped. This keeps the peer path at
    /// least as strict as the relay path.
    pub fn apply(&mut self, store: &mut EventStore, data: &[u8]) -> Result<bool, PeerError> {
        let env: SessionEventEnvelope = serde_json::from_slice(data)?;
        if !self.workspaces.contains(&env.workspace_id) {
            tracing::warn!(event_id = %env.event_id, "dropped a peer event of a workspace this link doesn't carry");
            return Ok(false);
        }
        self.seen.insert((env.event_id, env.signature.clone()));

        // P0.4: the workspace's own roster; this envelope's trust content
        // counts only once it verifies (see rosters_for_batch).
        let roster = crate::envelope_verifier::rosters_for_batch(store, &[&env])
            .map_err(|e| PeerError::Store(e.to_string()))?
            .remove(&env.workspace_id)
            .unwrap_or_default();
        let workspace = env.workspace_id;
        let roster_relevant = crate::envelope_verifier::is_trust_kind(&env.payload)
            || env.session_id == ligus_core::workspace_config_session_id(env.workspace_id);
        match self.gate(&roster, &env) {
            IngestAction::Accept => {
                let new = store
                    .ingest(&env)
                    .map_err(|e| PeerError::Store(e.to_string()))?;
                // It may be what a held event waits for (a certificate, an
                // admission): judge those again now.
                if roster_relevant {
                    self.release_held(store, workspace)?;
                }
                Ok(new)
            }
            IngestAction::Quarantine(reason) => {
                tracing::warn!(?reason, event_id = %env.event_id, "rejected a quarantined peer event");
                // A lamport witness, as on the relay path
                // (`SyncEngine::apply_fetched`): only for a refusal another
                // device's roster could judge otherwise.
                if !matches!(
                    reason,
                    crate::envelope_verifier::QuarantineReason::Unsigned
                        | crate::envelope_verifier::QuarantineReason::BadSignature
                ) {
                    store
                        .note_lamport_witness(&env)
                        .map_err(|e| PeerError::Store(e.to_string()))?;
                }
                Ok(false)
            }
            IngestAction::Hold(reason) => {
                // Can't judge yet (a certificate or admission not here yet, a
                // disputed account key): held aside in the device's held
                // table, never ingested now, and judged again when a peer
                // event (or the workspace's sync engine) changes the roster.
                tracing::debug!(reason, event_id = %env.event_id, "holding a peer event until the roster can judge it");
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                store
                    .hold_events(
                        env.workspace_id,
                        &[crate::event_store::HoldItem {
                            env: &env,
                            reason,
                            seq: None,
                        }],
                        now_ms,
                    )
                    .map_err(|e| PeerError::Store(e.to_string()))?;
                // A lamport witness meanwhile (as on the relay path).
                store
                    .note_lamport_witness(&env)
                    .map_err(|e| PeerError::Store(e.to_string()))?;
                Ok(false)
            }
        }
    }

    /// The ingest gate, the same as the relay path's (not just
    /// `verdict_for`): so the Option-C GitHub-identity check applies on P2P
    /// too, instead of silently accepting anything not provably-bad.
    fn gate(
        &self,
        roster: &crate::envelope_verifier::WorkspaceRoster,
        env: &SessionEventEnvelope,
    ) -> IngestAction {
        let base = verdict_for(roster, env);
        let signer_account = env
            .actor_stamp
            .as_ref()
            .and_then(|s| s.actor.account_id)
            .or_else(|| env.signer_device_id.and_then(|d| roster.account_of(d)));
        let account_key = signer_account.and_then(|a| roster.account_signing_key(a));
        gate_ingest(
            {
                #[cfg(test)]
                let mode = self.identity_mode;
                #[cfg(not(test))]
                let mode = IdentityMode::Relay;
                mode
            },
            base,
            signer_account,
            account_key,
            &self.identity_cache,
        )
    }

    /// Judge `workspace`'s held events again (a roster change arrived over
    /// P2P), a bounded number of passes while something is released.
    fn release_held(&mut self, store: &mut EventStore, workspace: Uuid) -> Result<(), PeerError> {
        let err = |e: crate::event_store::EventStoreError| PeerError::Store(e.to_string());
        for _ in 0..8 {
            let held = store.held_events(workspace).map_err(err)?;
            if held.is_empty() {
                break;
            }
            let refs: Vec<&SessionEventEnvelope> = held.iter().map(|h| &h.env).collect();
            let roster = crate::envelope_verifier::rosters_for_batch(store, &refs)
                .map_err(err)?
                .remove(&workspace)
                .unwrap_or_default();
            let mut release = Vec::new();
            for h in &held {
                match self.gate(&roster, &h.env) {
                    IngestAction::Accept => {
                        store.ingest(&h.env).map_err(err)?;
                        release.push(h.digest.clone());
                    }
                    IngestAction::Quarantine(_) => {
                        store.note_lamport_witness(&h.env).map_err(err)?;
                        release.push(h.digest.clone());
                    }
                    IngestAction::Hold(_) => {}
                }
            }
            store.release_held(workspace, &release).map_err(err)?;
            if release.is_empty() {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::{ChatMessage, MessageRole, SessionEvent};

    #[test]
    fn friend_code_round_trips() {
        // A realistic 32-byte key.
        let id = PeerId::from_public_key(&[0xab; 32]);
        assert_eq!(id.0.len(), 64); // hex inner form
        let code = id.to_code();
        assert!(code.starts_with("hive_"));
        // Compact: well under the ~74-char legacy hex code.
        assert!(code.len() < 50, "code too long: {} ({code})", code.len());
        assert_eq!(PeerId::from_code(&format!("  {code}  ")), Some(id.clone()));
        assert_eq!(PeerId::from_code("not-a-code"), None);
        assert_eq!(PeerId::from_code("hive_"), None);
    }

    #[test]
    fn legacy_hex_codes_still_parse() {
        let id = PeerId::from_public_key(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(id.0, "deadbeef");
        // Old codes shared before the shortening must still resolve.
        assert_eq!(PeerId::from_code("hivepeer1:deadbeef"), Some(id));
        assert_eq!(PeerId::from_code("hivepeer1:"), None);
    }

    fn sample_envelope(seq: i64) -> SessionEventEnvelope {
        crate::event_store::fixture_signed(&SessionEventEnvelope::new(
            Uuid::nil(),
            Uuid::nil(),
            seq,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "u", "hello"),
            },
        ))
    }

    #[tokio::test]
    async fn two_peers_converge_over_loopback() {
        let a = PeerId("alice".into());
        let b = PeerId("bob".into());
        let (la, lb) = LoopbackLink::pair(a.clone(), b.clone());

        let store_a = EventStore::open_in_memory().unwrap();
        let mut store_b = EventStore::open_in_memory().unwrap();
        let env = sample_envelope(1);
        store_a.append_envelope(&env).unwrap();

        let mut sync_a = PeerSync::new([Uuid::nil()]);
        let mut sync_b = PeerSync::new([Uuid::nil()]);

        // A pushes its unpushed envelopes to B.
        let pushed = sync_a.push_to(&la, &b, &store_a).await.unwrap();
        assert_eq!(pushed, 1);

        // B receives and applies them.
        let (_from, data) = lb.recv().await.unwrap();
        let applied = sync_b.apply(&mut store_b, &data).unwrap();
        assert!(applied);

        // B now has A's session; a re-apply is a no-op (dedup).
        assert_eq!(store_b.list(Uuid::nil()).unwrap().len(), 1);
        assert!(!sync_b.apply(&mut store_b, &data).unwrap());
        // A doesn't re-push the same envelope.
        assert_eq!(sync_a.push_to(&la, &b, &store_a).await.unwrap(), 0);
    }

    /// Only a contact's messages are applied: a stranger who dials this
    /// endpoint gets nothing ingested, held or witnessed.
    #[test]
    fn only_a_contacts_messages_are_applied() {
        let env = sample_envelope(1);
        let data = serde_json::to_vec(&env).unwrap();
        let friend = Contact {
            peer_id: PeerId("friend".into()),
            label: "F".into(),
        };
        let mut store = EventStore::open_in_memory().unwrap();
        let mut sync = PeerSync::new([Uuid::nil()]);
        assert!(!sync
            .apply_from(
                &mut store,
                &PeerId("stranger".into()),
                std::slice::from_ref(&friend),
                &data
            )
            .unwrap());
        assert!(
            !store.has_event(env.event_id).unwrap(),
            "a stranger's message was applied"
        );
        assert!(store.witness_ids(Uuid::nil()).unwrap().is_empty());
        assert!(sync
            .apply_from(
                &mut store,
                &friend.peer_id,
                std::slice::from_ref(&friend),
                &data
            )
            .unwrap());
        assert!(store.has_event(env.event_id).unwrap());
    }

    /// Regression: the link sends only its workspaces' events and drops an
    /// inbound event of any other workspace.
    #[tokio::test]
    async fn peer_link_carries_only_its_workspaces() {
        let a = PeerId("alice".into());
        let b = PeerId("bob".into());
        let (la, lb) = LoopbackLink::pair(a.clone(), b.clone());
        let room = Uuid::new_v4();
        let local = Uuid::new_v4();
        let msg = |ws: Uuid, body: &str| {
            crate::event_store::fixture_signed(&SessionEventEnvelope::new(
                Uuid::new_v4(),
                ws,
                1,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "Alex", body),
                },
            ))
        };
        let store_a = EventStore::open_in_memory().unwrap();
        let shared = msg(room, "team");
        let private = msg(local, "PRIVATE");
        store_a.append_envelope(&shared).unwrap();
        store_a.append_envelope(&private).unwrap();

        let mut sync_a = PeerSync::new([room]);
        assert_eq!(
            sync_a.push_to(&la, &b, &store_a).await.unwrap(),
            1,
            "only the room's event goes out"
        );
        let (_, data) = lb.recv().await.unwrap();
        assert!(!String::from_utf8_lossy(&data).contains("PRIVATE"));

        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = PeerSync::new([room]);
        assert!(sync_b.apply(&mut store_b, &data).unwrap());
        // An older peer that still sends everything: the foreign event is dropped.
        let leaked = serde_json::to_vec(&private).unwrap();
        assert!(!sync_b.apply(&mut store_b, &leaked).unwrap());
        assert!(!store_b.has_event(private.event_id).unwrap());
    }

    /// A peer event signed by a device whose certificate hasn't arrived waits
    /// in the held table (it isn't applied on its stamp), and is applied when
    /// the certificate arrives over the same link.
    #[test]
    fn a_peer_event_held_before_its_certificate_is_released_when_it_arrives() {
        use ligus_core::crypto::{DeviceCertificate, SigningKeypair};
        use ligus_core::identity::{
            ActorIdentity, ActorKind, ActorStamp, WorkspaceMember, WorkspaceRole,
        };
        use ligus_core::Timestamp;
        let account = SigningKeypair::generate().unwrap();
        let account_id = Uuid::new_v4();
        let device = SigningKeypair::generate().unwrap();
        let device_id = ligus_core::crypto::device_id_for(
            ligus_core::crypto::SigAlg::Ed25519,
            &device.public_key_bytes(),
        );
        let mut actor = ActorIdentity::new(account_id.to_string(), "A", ActorKind::Human);
        actor.account_id = Some(account_id);
        actor.device_id = Some(device_id);
        let own = |l: i64, payload: SessionEvent| {
            let mut e = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), l, payload);
            e.actor_stamp = Some(ActorStamp {
                actor: actor.clone(),
                recorded_at: Timestamp::epoch(),
                cfg_frontier: None,
                presence: None,
                runtime_effect: false,
                prev: None,
                sig_alg: Default::default(),
                log_head: None,
            });
            ligus_core::sign_envelope(&mut e, device_id, &device);
            e
        };
        let member = WorkspaceMember {
            id: account_id.to_string(),
            actor: actor.clone(),
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::epoch(),
        };
        let cert = DeviceCertificate::issue(
            &account,
            account_id,
            &device.public_key_bytes(),
            Timestamp::epoch(),
        );
        let content = own(10, SessionEvent::SessionTitleChanged { title: "t".into() });
        let mut store = EventStore::open_in_memory().unwrap();
        let mut sync = PeerSync::new([Uuid::nil()]);
        let bytes = |e: &SessionEventEnvelope| serde_json::to_vec(e).unwrap();
        assert!(
            !sync.apply(&mut store, &bytes(&content)).unwrap(),
            "held, not applied on its stamp"
        );
        assert_eq!(store.held_events(Uuid::nil()).unwrap().len(), 1);
        for e in [
            own(
                1,
                SessionEvent::MemberAdded {
                    member,
                    invite: None,
                    proof: None,
                },
            ),
            own(
                2,
                SessionEvent::AccountKeyRegistered {
                    account_id,
                    signing_public_key: account.public_key_bytes().to_vec(),
                },
            ),
            own(
                3,
                SessionEvent::DeviceCertificateAdded { certificate: cert },
            ),
        ] {
            sync.apply(&mut store, &bytes(&e)).unwrap();
        }
        assert!(
            store.has_event(content.event_id).unwrap(),
            "released once the certificate arrived"
        );
        assert!(store.held_events(Uuid::nil()).unwrap().is_empty());
    }

    /// Every copy a device holds goes out, not only the first to arrive:
    /// another signer's copy under an honest event's id that reached this
    /// device first must not keep the honest one from its peers.
    #[test]
    fn every_copy_under_an_id_is_forwarded() {
        let room = Uuid::new_v4();
        let honest = crate::event_store::fixture_signed(&SessionEventEnvelope::new(
            Uuid::new_v4(),
            room,
            1,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "Alex", "honest"),
            },
        ));
        let mut other = honest.clone();
        other.payload = SessionEvent::MessageAppended {
            message: ChatMessage::new(MessageRole::User, "Mal", "mine"),
        };
        other.clear_raw();
        ligus_core::sign_envelope(
            &mut other,
            Uuid::new_v4(),
            &ligus_core::SigningKeypair::generate().unwrap(),
        );
        let mut store = EventStore::open_in_memory().unwrap();
        store.ingest(&other).unwrap();
        store.ingest(&honest).unwrap();
        let mut sync = PeerSync::new([room]);
        let out = sync.take_unpushed(&store).unwrap();
        assert!(
            out.iter()
                .any(|d| String::from_utf8_lossy(d).contains("honest")),
            "the honest copy never left this device"
        );
        assert_eq!(out.len(), 2);
        assert!(
            sync.take_unpushed(&store).unwrap().is_empty(),
            "each copy goes out once"
        );
    }

    #[test]
    fn apply_verifies_before_ingest() {
        use ligus_core::crypto::{DeviceCertificate, SigningKeypair};
        use ligus_core::identity::{
            ActorIdentity, ActorKind, ActorStamp, WorkspaceMember, WorkspaceRole,
        };
        use ligus_core::Timestamp;

        let account = SigningKeypair::generate().unwrap();
        let account_id = Uuid::new_v4();
        let device = SigningKeypair::generate().unwrap();
        let device_id = ligus_core::crypto::device_id_for(
            ligus_core::crypto::SigAlg::Ed25519,
            &device.public_key_bytes(),
        );
        let cert = DeviceCertificate::issue(
            &account,
            account_id,
            &device.public_key_bytes(),
            Timestamp::epoch(),
        );
        let member = WorkspaceMember {
            id: account_id.to_string(),
            actor: ActorIdentity {
                id: account_id.to_string(),
                display_name: "A".into(),
                kind: ActorKind::Human,
                account_id: Some(account_id),
                device_id: Some(device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
                contact: None,
            },
            role: WorkspaceRole::Owner,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::epoch(),
        };

        // Seed the store with the trust events so the roster knows `device`.
        let plain = |lamport: i64, payload: SessionEvent| {
            SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), lamport, payload)
        };
        // Published by the device itself, stamped as its account (as
        // `publish_identity` does).
        let own = |mut e: SessionEventEnvelope| {
            e.actor_stamp = Some(ActorStamp {
                actor: member.actor.clone(),
                recorded_at: Timestamp::epoch(),
                cfg_frontier: None,
                presence: None,
                runtime_effect: false,
                prev: None,
                sig_alg: Default::default(),
                log_head: None,
            });
            ligus_core::sign_envelope(&mut e, device_id, &device);
            e
        };
        let mut store = EventStore::open_in_memory().unwrap();
        for e in [
            plain(
                1,
                SessionEvent::MemberAdded {
                    member: member.clone(),
                    invite: None,
                    proof: None,
                },
            ),
            own(plain(
                2,
                SessionEvent::AccountKeyRegistered {
                    account_id,
                    signing_public_key: account.public_key_bytes().to_vec(),
                },
            )),
            own(plain(
                3,
                SessionEvent::DeviceCertificateAdded { certificate: cert },
            )),
        ] {
            store.ingest(&e).unwrap();
        }

        // A content event signed by `device`, stamping `claim` as author.
        let content = |lamport: i64, claim: Uuid| -> SessionEventEnvelope {
            let mut e = SessionEventEnvelope::new(
                Uuid::nil(),
                Uuid::nil(),
                lamport,
                SessionEvent::SessionTitleChanged {
                    title: format!("t{lamport}"),
                },
            );
            e.actor_stamp = Some(ActorStamp {
                actor: ActorIdentity {
                    id: claim.to_string(),
                    display_name: "A".into(),
                    kind: ActorKind::Human,
                    account_id: Some(claim),
                    device_id: Some(device_id),
                    git_email: None,
                    key_agreement_public: None,
                    avatar_url: None,
                    contact: None,
                },
                recorded_at: Timestamp::epoch(),
                cfg_frontier: None,
                presence: None,
                runtime_effect: false,
                prev: None,
                sig_alg: Default::default(),
                log_head: None,
            });
            ligus_core::sign_envelope(&mut e, device_id, &device);
            e
        };

        let good = content(100, account_id);
        let mut tampered = content(101, account_id);
        tampered.sequence = 9999; // breaks the signature
        let spoof = content(102, Uuid::new_v4()); // stamps a different account

        let mut sync = PeerSync::new([Uuid::nil()]);
        let bytes = |e: &SessionEventEnvelope| serde_json::to_vec(e).unwrap();

        // A validly-signed envelope from a known-roster device is ingested.
        assert!(
            sync.apply(&mut store, &bytes(&good)).unwrap(),
            "valid signed event ingested"
        );
        assert!(store.has_event(good.event_id).unwrap());

        // A tampered (forged-signature) envelope is rejected, not ingested.
        assert!(
            !sync.apply(&mut store, &bytes(&tampered)).unwrap(),
            "tampered event rejected"
        );
        assert!(!store.has_event(tampered.event_id).unwrap());

        // An impersonating envelope is rejected, not ingested.
        assert!(
            !sync.apply(&mut store, &bytes(&spoof)).unwrap(),
            "impersonation rejected"
        );
        assert!(!store.has_event(spoof.event_id).unwrap());
    }
}
