//! The sync loop and the apply loop as services a client runs.
//!
//! - [`sync_round`]: one push + pull round of a [`SyncEngine`] (the CLI's
//!   `ligus sync`, a worker tick).
//! - [`sync_round_notify`]: the same, then what the pull brought in, published
//!   to a [`ChangeFeed`] so a subscribed UI reloads only what changed.
//! - [`SyncLoop`]: rounds on a timer, sooner when this device appended
//!   something (a local change on the feed) or when the relay's event stream
//!   says the room moved, and again at once while the engine is behind.
//! - [`apply_tick`]: one pass of the apply-on-host loop: every open request
//!   naming this host, fulfilled once each in the checkout `repo_for` names.
//!
//! The loop owns its own [`EventStore`] connection (the WAL is shared with
//! the service's), so a round never waits on the service mutex.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use uuid::Uuid;

use crate::chat_service::{ChangeFeed, ChangeOrigin, ChatService};
use crate::event_store::EventStore;
use crate::sync_engine::{SyncEngine, SyncError};

/// One push + pull round: (pushed, pulled).
pub async fn sync_round(engine: &mut SyncEngine, store: &mut EventStore) -> Result<(usize, usize), SyncError> {
    let to_push = engine.take_unpushed(store)?;
    let pushed = to_push.len();
    engine.push_envelopes(&to_push).await?;
    // One bounded round of relay pages; `engine.is_behind()` says another is due.
    let pulled = engine.pull_round(store).await?;
    Ok((pushed, pulled))
}

/// What one notifying round did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoundReport {
    pub pushed: usize,
    pub pulled: usize,
    /// Sessions that gained events in the pull, as (session, workspace), in
    /// first-changed order.
    pub changed: Vec<(Uuid, Uuid)>,
}

/// [`sync_round`], then publish every session the pull changed to `feed` as
/// [`ChangeOrigin::Synced`].
pub async fn sync_round_notify(engine: &mut SyncEngine, store: &mut EventStore, feed: &ChangeFeed) -> Result<RoundReport, SyncError> {
    let before = store.max_row_id()?;
    let (pushed, pulled) = sync_round(engine, store).await?;
    let mut changed = Vec::new();
    if pulled > 0 {
        for sid in store.sessions_changed_after(before)? {
            let ws = store.workspace_of(sid)?.unwrap_or_default();
            feed.synced(sid, ws);
            changed.push((sid, ws));
        }
    }
    Ok(RoundReport { pushed, pulled, changed })
}

/// What the loop reports after each round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStatus {
    Synced(RoundReport),
    Failed(String),
}

/// A background sync loop for one room.
pub struct SyncLoop {
    pub engine: SyncEngine,
    pub store: EventStore,
    pub feed: ChangeFeed,
    /// The room's workspace: only its local changes wake the loop early.
    pub workspace_id: Uuid,
    /// The longest wait between rounds.
    pub every: Duration,
    /// The relay and room whose event stream (SSE) wakes the loop when the
    /// room moves; `None` polls only. A relay without the stream (404) is
    /// polled.
    pub live: Option<(crate::relay_client::RelayClient, String)>,
}

impl SyncLoop {
    /// Run until `stop` fires, calling `status` after each round.
    pub async fn run(mut self, mut stop: tokio::sync::watch::Receiver<bool>, mut status: impl FnMut(SyncStatus) + Send) {
        let mut local = self.feed.subscribe();
        let mut sse = None;
        loop {
            if *stop.borrow() {
                return;
            }
            match sync_round_notify(&mut self.engine, &mut self.store, &self.feed).await {
                Ok(r) => status(SyncStatus::Synced(r)),
                Err(e) => status(SyncStatus::Failed(e.to_string())),
            }
            if self.engine.is_behind() {
                continue;
            }
            if let (Some((relay, room)), None) = (&self.live, &sse) {
                sse = relay.open_event_stream(room).await.ok();
            }
            let ws = self.workspace_id;
            let woke_by_relay = async {
                match sse.as_mut() {
                    Some(s) => s.recv().await.is_some(),
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = stop.changed() => {}
                _ = tokio::time::sleep(self.every) => {}
                alive = woke_by_relay => if !alive { sse = None },
                _ = async {
                    loop {
                        match local.recv().await {
                            Ok(c) if c.origin == ChangeOrigin::Local && c.workspace_id == ws => return,
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
                        }
                    }
                } => {
                    // Let a burst of appends (a streamed reply) settle.
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    while local.try_recv().is_ok() {}
                }
            }
        }
    }
}

/// What one [`apply_tick`] did: per request `(request, proposal, outcome)`,
/// or why the requests couldn't be listed.
#[derive(Debug, Default)]
pub struct ApplyTick {
    pub listing_error: Option<String>,
    pub results: Vec<(Uuid, Uuid, Result<crate::apply_host::Outcome, String>)>,
}

impl ApplyTick {
    /// The requests that got an outcome, as `(request, outcome)`.
    pub fn outcomes(&self) -> Vec<(Uuid, crate::apply_host::Outcome)> {
        self.results.iter().filter_map(|(r, _, o)| o.as_ref().ok().map(|o| (*r, o.clone()))).collect()
    }
}

/// One pass of the apply-on-host loop for `workspace_id`: every open
/// `ProposalApplyRequested` naming this host, fulfilled once, in the checkout
/// `repo_for(chat)` names (`None`: refused as having no checkout). Never on
/// this host's own approval: only a person's request names a host. A device
/// that can't write (revoked, awaiting approval) does nothing.
///
/// `founder` is `workspace_id`'s trusted founder. The requests are listed,
/// and each is judged, under that pin, set under the same lock as each
/// decision: a client that shares one service between several workspaces
/// (re-pinning it for each) can't have another workspace's pin in force
/// while this one's requests are authorized. The service's own pin is put
/// back after.
pub fn apply_tick(
    svc: &Mutex<ChatService>,
    workspace_id: Uuid,
    founder: Option<&str>,
    repo_for: &dyn Fn(Uuid) -> Option<PathBuf>,
) -> ApplyTick {
    let mut tick = ApplyTick::default();
    let jobs = {
        let s = crate::apply_host::Pinned::new(svc, founder);
        if s.write_block().is_some() {
            return tick;
        }
        match crate::apply_host::pending(&s, workspace_id) {
            Ok(j) => j,
            Err(e) => {
                tick.listing_error = Some(e.to_string());
                return tick;
            }
        }
    };
    for job in jobs {
        let repo = repo_for(job.chat_id);
        let outcome = crate::apply_host::fulfil(svc, &job, repo.as_deref()).map_err(|e| e.to_string());
        tick.results.push((job.request.id, job.proposal.id, outcome));
    }
    tick
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay_client::RelayClient;
    use hive_core::{ActorIdentity, ActorKind, ChatMessage, ChatSession, MessageRole, SessionEvent, SessionEventEnvelope, SigningKeypair};

    async fn spawn_relay() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, crate::test_relay::router()).await.unwrap() });
        format!("http://{addr}")
    }

    fn seed(store: &mut EventStore, wid: Uuid) -> Uuid {
        let session = ChatSession::new("Chat", wid, "rt");
        let sid = session.id;
        store.ingest_next(&SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(session) })).unwrap();
        let message = ChatMessage::new(MessageRole::User, "Alex", "hello");
        store.ingest_next(&SessionEventEnvelope::new(sid, wid, 2, SessionEvent::MessageAppended { message })).unwrap();
        sid
    }

    /// One device pushes; the other's round publishes the session it pulled,
    /// as a synced change, and a round with nothing new publishes nothing.
    #[tokio::test]
    async fn a_round_publishes_the_sessions_it_pulled() {
        let base = spawn_relay().await;
        let room = "room-sync-service";
        let wid = hive_core::room_workspace_id(room);
        let key = hive_core::derive_workspace_key("sync service");

        let mut a = EventStore::open_in_memory().unwrap();
        let sid = seed(&mut a, wid);
        let mut ea = SyncEngine::new(RelayClient::new(&base), room).with_key(key);
        let feed_a = ChangeFeed::default();
        let mut rx_a = feed_a.subscribe();
        let r = sync_round_notify(&mut ea, &mut a, &feed_a).await.unwrap();
        assert_eq!((r.pushed, r.pulled, r.changed.len()), (2, 0, 0));
        assert!(rx_a.try_recv().is_err(), "a push changes nothing here");

        let mut b = EventStore::open_in_memory().unwrap();
        let mut eb = SyncEngine::new(RelayClient::new(&base), room).with_key(key);
        let feed_b = ChangeFeed::default();
        let mut rx_b = feed_b.subscribe();
        let r = sync_round_notify(&mut eb, &mut b, &feed_b).await.unwrap();
        assert_eq!((r.pushed, r.pulled), (0, 2));
        assert_eq!(r.changed, vec![(sid, wid)]);
        let c = rx_b.try_recv().unwrap();
        assert_eq!((c.session_id, c.workspace_id, c.origin, c.event.is_none()), (sid, wid, ChangeOrigin::Synced, true));
        assert_eq!(sync_round_notify(&mut eb, &mut b, &feed_b).await.unwrap(), RoundReport::default());
        assert!(rx_b.try_recv().is_err());
        // The plain round is the same push + pull.
        assert_eq!(sync_round(&mut eb, &mut b).await.unwrap(), (0, 0));
    }

    /// The loop runs a round at once, reports it, and stops when told.
    #[tokio::test]
    async fn the_loop_reports_rounds_and_stops() {
        let base = spawn_relay().await;
        let room = "room-sync-loop";
        let key = hive_core::derive_workspace_key("sync loop");
        let lp = SyncLoop {
            engine: SyncEngine::new(RelayClient::new(&base), room).with_key(key),
            store: EventStore::open_in_memory().unwrap(),
            feed: ChangeFeed::default(),
            workspace_id: hive_core::room_workspace_id(room),
            every: Duration::from_millis(20),
            live: None,
        };
        let (stop, rx) = tokio::sync::watch::channel(false);
        let (tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(lp.run(rx, move |s| {
            let _ = tx.send(s);
        }));
        assert!(matches!(seen.recv().await, Some(SyncStatus::Synced(_))));
        assert!(matches!(seen.recv().await, Some(SyncStatus::Synced(_))), "and again on the timer");
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
    }

    /// A device that can't write applies nothing; with no request naming it,
    /// a tick does nothing.
    #[test]
    fn apply_tick_without_requests_or_writes_does_nothing() {
        let author = ActorIdentity::new("ana".to_string(), "Ana".to_string(), ActorKind::Human);
        let svc = ChatService::new(
            EventStore::open_in_memory().unwrap(),
            Uuid::new_v4(),
            SigningKeypair::generate().unwrap(),
            SigningKeypair::generate().unwrap(),
            author,
        );
        let svc = Mutex::new(svc);
        let ws = Uuid::new_v4();
        let tick = apply_tick(&svc, ws, None, &|_| None);
        assert!(tick.listing_error.is_none() && tick.results.is_empty() && tick.outcomes().is_empty());
        // The tick works under the pin it was given and puts the service's back.
        svc.lock().unwrap().set_trusted_founder(Some("other-workspace-founder".into()));
        assert!(apply_tick(&svc, ws, Some("this-workspace-founder"), &|_| None).results.is_empty());
        assert_eq!(svc.lock().unwrap().trusted_founder(), Some("other-workspace-founder"));
        svc.lock().unwrap().set_write_block(Some("revoked".into()));
        assert!(apply_tick(&svc, ws, None, &|_| None).results.is_empty());
    }
}
