//! Change subscription for [`ChatService`]: a broadcast of what changed.
//!
//! Every event this service appends is published to its [`ChangeFeed`] as a
//! [`ChatChange`] with [`ChangeOrigin::Local`]. A sync loop that ingests
//! through its own store connection publishes what it pulled into the same
//! feed ([`ChangeFeed::synced`]), so a client subscribes once and learns about
//! both, without diffing projections. Publishing never blocks or fails: with
//! no subscriber it is a no-op, and a subscriber that falls behind gets
//! `Lagged` and reloads.

use std::sync::Arc;

use ligus_core::SessionEventEnvelope;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::ChatService;

/// How many changes a subscriber may fall behind before it lags.
pub const CHANGE_FEED_CAPACITY: usize = 1024;

/// Where a change came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeOrigin {
    /// Appended by this device.
    Local,
    /// Pulled from a relay or a peer.
    Synced,
}

/// One change to a session's log.
#[derive(Debug, Clone)]
pub struct ChatChange {
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub origin: ChangeOrigin,
    /// The appended event (local changes). A synced change names only the
    /// session; reload it.
    pub event: Option<Arc<SessionEventEnvelope>>,
}

impl ChatChange {
    /// The event kind (`messageAppended`, `assistantChunk`, …), when known.
    pub fn kind(&self) -> Option<&'static str> {
        self.event.as_ref().map(|e| e.payload.kind_str())
    }
}

/// The broadcast a [`ChatService`] publishes to. Cheap to clone; clones
/// share subscribers.
#[derive(Debug, Clone)]
pub struct ChangeFeed(broadcast::Sender<ChatChange>);

impl Default for ChangeFeed {
    fn default() -> Self {
        Self(broadcast::channel(CHANGE_FEED_CAPACITY).0)
    }
}

impl ChangeFeed {
    pub fn subscribe(&self) -> broadcast::Receiver<ChatChange> {
        self.0.subscribe()
    }

    pub fn publish(&self, change: ChatChange) {
        if self.0.receiver_count() > 0 {
            let _ = self.0.send(change);
        }
    }

    /// An event this device appended.
    pub fn local(&self, env: &SessionEventEnvelope) {
        if self.0.receiver_count() > 0 {
            self.publish(ChatChange {
                session_id: env.session_id,
                workspace_id: env.workspace_id,
                origin: ChangeOrigin::Local,
                event: Some(Arc::new(env.clone())),
            });
        }
    }

    /// Session `session_id` of `workspace_id` gained synced events.
    pub fn synced(&self, session_id: Uuid, workspace_id: Uuid) {
        self.publish(ChatChange { session_id, workspace_id, origin: ChangeOrigin::Synced, event: None });
    }
}

impl ChatService {
    /// Subscribe to this service's changes.
    pub fn subscribe(&self) -> broadcast::Receiver<ChatChange> {
        self.changes.subscribe()
    }

    /// The feed this service publishes to (hand it to a sync loop).
    pub fn change_feed(&self) -> ChangeFeed {
        self.changes.clone()
    }

    /// Publish to `feed` instead (several services, one subscription).
    pub fn set_change_feed(&mut self, feed: ChangeFeed) {
        self.changes = feed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::EventStore;
    use ligus_core::{ActorIdentity, ActorKind, SigningKeypair};

    fn service() -> ChatService {
        let kp = SigningKeypair::generate().unwrap();
        let account = SigningKeypair::generate().unwrap();
        let author = ActorIdentity::new("me".to_string(), "Me".to_string(), ActorKind::Human);
        ChatService::new(EventStore::open_in_memory().unwrap(), kp, account, author)
    }

    #[test]
    fn appends_are_published_to_subscribers() {
        let mut svc = service();
        let mut rx = svc.subscribe();
        let ws = Uuid::new_v4();
        let chat = svc.create_chat("hello", ws, "").unwrap();
        svc.post_user_message(chat.id, ws, "hi there").unwrap();
        let mut kinds = Vec::new();
        while let Ok(c) = rx.try_recv() {
            assert_eq!(c.origin, ChangeOrigin::Local);
            if c.session_id == chat.id {
                kinds.push(c.kind().unwrap());
            }
        }
        assert!(kinds.contains(&"messageAppended"), "{kinds:?}");
    }

    #[test]
    fn a_shared_feed_carries_synced_changes_and_no_subscriber_is_fine() {
        let mut svc = service();
        let ws = Uuid::new_v4();
        // No subscriber: appends still work.
        svc.create_chat("a", ws, "").unwrap();
        let feed = ChangeFeed::default();
        svc.set_change_feed(feed.clone());
        let mut rx = svc.subscribe();
        let sid = Uuid::new_v4();
        feed.synced(sid, ws);
        let c = rx.try_recv().unwrap();
        assert_eq!((c.session_id, c.workspace_id, c.origin, c.kind()), (sid, ws, ChangeOrigin::Synced, None));
    }
}
