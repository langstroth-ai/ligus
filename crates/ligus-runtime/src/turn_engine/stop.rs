//! Cancelling a turn: a per-turn, sticky stop signal, and a registry of the
//! turns running on this host so a Stop reaches the turn it names.
//!
//! The signal is a level, not an edge: a Stop pressed before the engine
//! awaits it (while MCP servers are listed, during the repo gate) is still
//! seen at the next await point. Each turn has its own signal, so stopping
//! one of two concurrent turns in a chat leaves the other running.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::watch;
use uuid::Uuid;

/// One turn's stop signal. Cloning shares it. Once stopped it stays stopped.
#[derive(Clone, Debug)]
pub struct StopSignal {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    tx: watch::Sender<bool>,
    /// The turn's reply placeholder, once the engine has begun it.
    message: OnceLock<Uuid>,
}

impl Default for StopSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl StopSignal {
    pub fn new() -> Self {
        Self { inner: Arc::new(Inner { tx: watch::channel(false).0, message: OnceLock::new() }) }
    }

    /// Stop the turn. Idempotent, and seen by a turn that hasn't started
    /// waiting yet.
    pub fn stop(&self) {
        self.inner.tx.send_replace(true);
    }

    pub fn is_stopped(&self) -> bool {
        *self.inner.tx.borrow()
    }

    /// Resolves once the turn is stopped (at once if it already is).
    pub async fn stopped(&self) {
        let mut rx = self.inner.tx.subscribe();
        // The sender lives as long as `self`, so this never errors.
        let _ = rx.wait_for(|s| *s).await;
    }

    /// Record the turn's reply placeholder (the engine does, right after it
    /// begins the message), so a Stop can name the turn by its message.
    pub fn bind_message(&self, message_id: Uuid) {
        let _ = self.inner.message.set(message_id);
    }

    pub fn message_id(&self) -> Option<Uuid> {
        self.inner.message.get().copied()
    }

    fn same(&self, other: &StopSignal) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

/// The turns (and dispatch waits) running on this host, per chat.
#[derive(Debug, Default)]
pub struct TurnStops {
    chats: Mutex<HashMap<Uuid, Vec<StopSignal>>>,
    /// Shutting down: every turn registered from now on is born stopped.
    closed: std::sync::atomic::AtomicBool,
}

impl TurnStops {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A fresh signal for a turn in `chat`, registered until the returned
    /// guard drops.
    pub fn register(self: &Arc<Self>, chat: Uuid) -> TurnStopGuard {
        let signal = StopSignal::new();
        let mut chats = self.lock();
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            signal.stop();
        }
        chats.entry(chat).or_default().push(signal.clone());
        drop(chats);
        TurnStopGuard { stops: self.clone(), chat, signal }
    }

    /// Stop `chat`'s turns: only the one whose reply is `message` when it is
    /// given, else every turn and dispatch wait in the chat. Returns how many
    /// were stopped.
    pub fn stop(&self, chat: Uuid, message: Option<Uuid>) -> usize {
        let chats = self.lock();
        let Some(list) = chats.get(&chat) else { return 0 };
        let mut n = 0;
        for s in list {
            if message.is_none() || s.message_id() == message {
                s.stop();
                n += 1;
            }
        }
        n
    }

    /// Shutdown: stop every turn, and every turn registered after this.
    pub fn close(&self) -> usize {
        let chats = self.lock();
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        chats.values().flatten().map(StopSignal::stop).count()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many turns are registered for `chat`.
    pub fn running(&self, chat: Uuid) -> usize {
        self.lock().get(&chat).map_or(0, Vec::len)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Vec<StopSignal>>> {
        self.chats.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Keeps a turn's signal registered; dropping it unregisters the turn.
#[derive(Debug)]
pub struct TurnStopGuard {
    stops: Arc<TurnStops>,
    chat: Uuid,
    signal: StopSignal,
}

impl TurnStopGuard {
    pub fn signal(&self) -> StopSignal {
        self.signal.clone()
    }
}

impl Drop for TurnStopGuard {
    fn drop(&mut self) {
        let mut chats = self.stops.lock();
        if let Some(list) = chats.get_mut(&self.chat) {
            list.retain(|s| !s.same(&self.signal));
            if list.is_empty() {
                chats.remove(&self.chat);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_stop_before_anyone_waits_is_not_lost() {
        let s = StopSignal::new();
        s.stop();
        tokio::time::timeout(Duration::from_secs(1), s.stopped()).await.expect("sticky");
        assert!(s.is_stopped());
    }

    #[test]
    fn stop_by_message_reaches_only_that_turn() {
        let stops = TurnStops::new();
        let chat = Uuid::new_v4();
        let (a, b) = (stops.register(chat), stops.register(chat));
        let (ma, mb) = (Uuid::new_v4(), Uuid::new_v4());
        a.signal().bind_message(ma);
        b.signal().bind_message(mb);
        assert_eq!(stops.stop(chat, Some(mb)), 1);
        assert!(!a.signal().is_stopped() && b.signal().is_stopped());
        assert_eq!(stops.stop(Uuid::new_v4(), None), 0, "another chat");
        assert_eq!(stops.stop(chat, None), 2);
        assert!(a.signal().is_stopped());
        drop(a);
        assert_eq!(stops.running(chat), 1);
        drop(b);
        assert_eq!(stops.running(chat), 0);
    }

    #[test]
    fn after_close_new_turns_are_born_stopped() {
        let stops = TurnStops::new();
        let chat = Uuid::new_v4();
        let running = stops.register(chat);
        assert_eq!(stops.close(), 1);
        assert!(running.signal().is_stopped());
        assert!(stops.register(Uuid::new_v4()).signal().is_stopped());
    }

    #[test]
    fn a_new_turn_starts_unstopped_after_a_stop() {
        let stops = TurnStops::new();
        let chat = Uuid::new_v4();
        {
            let _old = stops.register(chat);
            stops.stop(chat, None);
        }
        assert!(!stops.register(chat).signal().is_stopped());
    }
}
