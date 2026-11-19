//! Typing indicators for the phone: `POST /v1/chats/{id}/typing {typing}`,
//! `GET /v1/chats/{id}/typing`, and the live `typing` SSE event.
//!
//! The phone tells its box it is typing in a chat; the box publishes a human
//! presence beat for that chat to the chat's own relay room (the workspace's,
//! or for a DM, only that DM's room), sealed under that room's keyring
//! ([`hive_runtime::human_presence`]). The beat names the person the box
//! writes as (`svc.author()`): its owner when it is the owner's enrolled
//! device, else its own identity, exactly as its messages are attributed. It
//! carries no message text.
//!
//! - **Throttle.** A `typing: true` within [`REPUBLISH_EVERY`] of the last
//!   publish only refreshes the expiry; "stopped" publishes at once. More than
//!   [`MAX_REQUESTS`] requests in [`WINDOW`] answer 429.
//! - **Expiry.** Without a ping for [`EXPIRE_AFTER`], the box publishes
//!   "stopped" itself (a phone that went away mid-word); readers also drop a
//!   beat older than [`hive_runtime::human_presence::PRESENCE_TTL_MS`].
//! - **Others.** While a phone holds the event stream open, the box reads the
//!   presence of its workspace and DM rooms every [`POLL_EVERY`] and emits a
//!   `typing` event (`{chatId, typing: [{actorId, name}]}`, the full list for
//!   that chat) whenever it changes. Live only: not kept for replay, never a
//!   push notification.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use hive_core::ChatSession;
use hive_runtime::human_presence::{self, HumanBeat};
use hive_runtime::keyring::KeyringEngine;
use hive_runtime::RelayClient;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::watch;
use uuid::Uuid;

use super::api::{load_phone_chat, ApiError, ApiResult};
use super::Shared;

/// A typing beat is republished at most this often while the phone keeps typing.
pub const REPUBLISH_EVERY: Duration = Duration::from_secs(3);
/// With no ping for this long, the box publishes "stopped".
pub const EXPIRE_AFTER: Duration = Duration::from_secs(6);
/// Request budget per window (a phone pings about every 2.5 s).
pub const MAX_REQUESTS: u32 = 30;
pub const WINDOW: Duration = Duration::from_secs(10);
/// How often others' presence is read while a phone is listening.
pub const POLL_EVERY: Duration = Duration::from_secs(2);

/// The phone's typing in one chat, as this box last published it.
struct Mine {
    workspace: Uuid,
    last_ping: Instant,
    last_publish: Option<Instant>,
    typing: bool,
}

/// Typing state kept by the box (in memory).
#[derive(Default)]
pub struct Typing {
    mine: Mutex<HashMap<Uuid, Mine>>,
    window: Mutex<Option<(Instant, u32)>>,
    /// chat → who else is typing there, as last emitted.
    others: Mutex<HashMap<Uuid, Vec<(String, String)>>>,
    /// Wakes [`typing_loop`] early (it otherwise looks every 500 ms).
    kick: tokio::sync::Notify,
    /// Tests: a clock they set and advance instead of waiting on real time.
    #[cfg(test)]
    clock: Mutex<Option<Instant>>,
}

/// What a ping asks the box to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Publish this typing state now.
    Publish(bool),
    /// Nothing to publish (throttled, or already stopped).
    Skip,
}

impl Typing {
    /// The time the throttle, budget and expiry go by: the real clock, or
    /// in tests the one they set.
    pub fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(t) = *self.clock.lock().unwrap_or_else(|e| e.into_inner()) {
            return t;
        }
        Instant::now()
    }

    /// Tests: stop the clock at `at` (it moves only by [`Self::advance`]).
    #[cfg(test)]
    pub fn freeze_clock(&self, at: Instant) {
        *self.clock.lock().unwrap_or_else(|e| e.into_inner()) = Some(at);
    }

    /// Tests: move the frozen clock on by `by` and have [`typing_loop`]
    /// look at once.
    #[cfg(test)]
    pub fn advance(&self, by: Duration) {
        {
            let mut c = self.clock.lock().unwrap_or_else(|e| e.into_inner());
            *c = Some(c.unwrap_or_else(Instant::now) + by);
        }
        self.kick.notify_one();
    }

    /// Tests: whether the box still holds `chat` as typing (not expired).
    #[cfg(test)]
    pub fn is_typing(&self, chat: Uuid) -> bool {
        self.mine.lock().unwrap_or_else(|e| e.into_inner()).get(&chat).is_some_and(|e| e.typing)
    }

    /// Count one request against the window; false when over budget.
    pub fn admit(&self, now: Instant) -> bool {
        let mut w = self.window.lock().unwrap_or_else(|e| e.into_inner());
        match &mut *w {
            Some((start, n)) if now.duration_since(*start) < WINDOW => {
                if *n >= MAX_REQUESTS {
                    return false;
                }
                *n += 1;
            }
            _ => *w = Some((now, 1)),
        }
        true
    }

    /// Record a ping from the phone and decide whether to publish.
    pub fn ping(&self, chat: Uuid, workspace: Uuid, typing: bool, now: Instant) -> Decision {
        let mut m = self.mine.lock().unwrap_or_else(|e| e.into_inner());
        let e = m.entry(chat).or_insert(Mine { workspace, last_ping: now, last_publish: None, typing: false });
        e.last_ping = now;
        let due = !e.typing || e.last_publish.is_none_or(|t| now.duration_since(t) >= REPUBLISH_EVERY);
        let d = match (typing, e.typing) {
            (true, _) if due => Decision::Publish(true),
            (true, _) => Decision::Skip,
            (false, true) => Decision::Publish(false),
            (false, false) => Decision::Skip,
        };
        if let Decision::Publish(t) = d {
            e.typing = t;
            e.last_publish = Some(now);
        }
        if !typing && !e.typing {
            m.remove(&chat);
        }
        d
    }

    /// Chats whose typing went quiet for [`EXPIRE_AFTER`]: marked stopped and
    /// returned (chat, workspace) so the caller publishes "stopped".
    pub fn expire(&self, now: Instant) -> Vec<(Uuid, Uuid)> {
        let mut m = self.mine.lock().unwrap_or_else(|e| e.into_inner());
        let gone: Vec<(Uuid, Uuid)> = m
            .iter()
            .filter(|(_, e)| e.typing && now.duration_since(e.last_ping) >= EXPIRE_AFTER)
            .map(|(c, e)| (*c, e.workspace))
            .collect();
        for (c, _) in &gone {
            m.remove(c);
        }
        gone
    }

    /// Replace one chat's list of others typing; true when it changed.
    fn set_others(&self, chat: Uuid, list: Vec<(String, String)>) -> bool {
        let mut o = self.others.lock().unwrap_or_else(|e| e.into_inner());
        let prev = o.get(&chat).cloned().unwrap_or_default();
        if prev == list {
            return false;
        }
        if list.is_empty() {
            o.remove(&chat);
        } else {
            o.insert(chat, list);
        }
        true
    }

    fn others_in(&self, chat: Uuid) -> Option<Vec<(String, String)>> {
        self.others.lock().unwrap_or_else(|e| e.into_inner()).get(&chat).cloned()
    }

    fn chats_with_others(&self) -> Vec<Uuid> {
        self.others.lock().unwrap_or_else(|e| e.into_inner()).keys().copied().collect()
    }
}

/// The relay, room and keyring a chat's presence lives on: the box's own
/// workspace room, or the DM's. `None` when there's no relay, no seal key or
/// (for a DM) its relay isn't allowed.
pub(super) async fn room_of(s: &Shared, workspace: Uuid) -> Option<(RelayClient, String, KeyringEngine)> {
    if workspace == s.ws {
        let relay = crate::relay_client(&s.cfg).ok()?;
        let ring = match crate::key_source(&s.cfg) {
            Ok(src) => KeyringEngine::from_store(s.lock().store(), s.ws, &src),
            Err(_) => KeyringEngine::passphrase(s.ws, hive_core::derive_workspace_key(s.cfg.key.as_deref()?)),
        };
        ring.seal_key()?;
        return Some((relay, s.cfg.room.clone(), ring));
    }
    let conn = s.dms.list().into_iter().find(|d| d.workspace_id() == workspace)?;
    super::dms::foreign_relay_check(&s.cfg, &conn.relay_url).await.ok()?;
    let cfg = s.dms.config_for(&s.cfg, &conn);
    let src = crate::key_source(&cfg).ok()?;
    let ring = KeyringEngine::from_store(s.lock().store(), workspace, &src);
    ring.seal_key()?;
    Some((super::dms::relay_client_for(&s.cfg, &conn.relay_url), conn.room, ring))
}

/// Publish the box's typing beat for `chat` (best effort; false when nothing
/// could be published).
async fn publish(s: &Shared, chat: Uuid, workspace: Uuid, typing: bool) -> bool {
    let Some((relay, room, ring)) = room_of(s, workspace).await else { return false };
    let beat = {
        let svc = s.lock();
        let a = svc.author();
        HumanBeat {
            actor_id: a.id.clone(),
            name: a.display_name.clone(),
            device_id: svc.device_id().to_string(),
            session_id: chat.to_string(),
            typing,
            ts: human_presence::now_ms(),
        }
    };
    human_presence::publish_human_beat(&relay, &room, &ring, &beat).await.is_ok()
}

#[derive(Deserialize)]
pub struct TypingReq {
    typing: bool,
}

/// `POST /v1/chats/{id}/typing {typing}` → `{ok, published}`.
pub async fn post_typing(
    State(s): State<Arc<Shared>>,
    Path(id): Path<String>,
    Json(req): Json<TypingReq>,
) -> ApiResult<Json<Value>> {
    let chat = load_phone_chat(&s, &id)?;
    if !s.typing.admit(s.typing.now()) {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "typing pings too often"));
    }
    let in_dm = chat.workspace_id != s.ws;
    // Nothing can be written there yet: nor typing.
    let (waiting, founder) = super::identity_api::founder_wait(&s, chat.workspace_id);
    if waiting {
        return Err(ApiError::waiting_for_founder(founder));
    }
    let blocked = s.lock().write_block().is_some() || (in_dm && s.dms.is_archived(chat.workspace_id));
    let typing = req.typing && !blocked;
    let published = match s.typing.ping(chat.id, chat.workspace_id, typing, s.typing.now()) {
        Decision::Publish(t) => publish(&s, chat.id, chat.workspace_id, t).await,
        Decision::Skip => false,
    };
    Ok(Json(json!({ "ok": true, "published": published })))
}

/// `GET /v1/chats/{id}/typing` → `{chatId, typing: [{actorId, name}]}`.
pub async fn get_typing(State(s): State<Arc<Shared>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let chat = load_phone_chat(&s, &id)?;
    let list = match s.typing.others_in(chat.id) {
        Some(l) => l,
        None => match room_of(&s, chat.workspace_id).await {
            Some((relay, room, ring)) => match human_presence::fetch_human_beats(&relay, &room, &ring, human_presence::now_ms()).await {
                Ok(beats) => others_typing(&s, &chat, &beats),
                Err(_) => Vec::new(),
            },
            None => Vec::new(),
        },
    };
    Ok(Json(typing_dto(chat.id, &list)))
}

fn typing_dto(chat: Uuid, list: &[(String, String)]) -> Value {
    json!({
        "chatId": chat,
        "typing": list.iter().map(|(a, n)| json!({ "actorId": a, "name": n })).collect::<Vec<_>>(),
    })
}

/// Who else is typing in `chat`: never the person this box writes as, named
/// from the chat's roster when they're on it, else by their beat (cleaned).
fn others_typing(s: &Shared, chat: &ChatSession, beats: &[HumanBeat]) -> Vec<(String, String)> {
    let me = s.lock().author().id.clone();
    let mut list = human_presence::typing_in(beats, &chat.id.to_string(), &me);
    for (actor, name) in &mut list {
        if let Some(m) = chat.members.iter().find(|m| &m.actor.id == actor) {
            let n = human_presence::clean_display_name(&m.actor.display_name);
            if !n.is_empty() {
                *name = n;
            }
        }
        if name.is_empty() {
            *name = "Someone".into();
        }
    }
    list
}

/// Expire the phone's quiet typing, and while a phone listens, read everyone
/// else's typing and emit `typing` events on change.
pub async fn typing_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut last_poll: Option<Instant> = None;
    loop {
        for (chat, ws) in shared.typing.expire(shared.typing.now()) {
            publish(&shared, chat, ws, false).await;
        }
        let listening = shared.hub.listeners() > 0;
        if listening && last_poll.is_none_or(|t| t.elapsed() >= POLL_EVERY) {
            last_poll = Some(Instant::now());
            poll_others(&shared).await;
        } else if !listening {
            // Nobody to tell: forget, so a returning phone gets a fresh list.
            for chat in shared.typing.chats_with_others() {
                shared.typing.set_others(chat, Vec::new());
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            _ = shared.typing.kick.notified() => {}
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

/// One read of every room's presence; emits `typing` for chats whose list changed.
async fn poll_others(shared: &Shared) {
    let Ok(chats) = shared.phone_chats() else { return };
    let mut by_ws: HashMap<Uuid, Vec<ChatSession>> = HashMap::new();
    for c in chats {
        by_ws.entry(c.workspace_id).or_default().push(c);
    }
    let now = human_presence::now_ms();
    for (ws, chats) in by_ws {
        let Some((relay, room, ring)) = room_of(shared, ws).await else { continue };
        let Ok(beats) = human_presence::fetch_human_beats(&relay, &room, &ring, now).await else { continue };
        for chat in &chats {
            let list = others_typing(shared, chat, &beats);
            if shared.typing.set_others(chat.id, list.clone()) {
                shared.hub.emit_live("typing", typing_dto(chat.id, &list));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttles_republishes_and_stops_at_once() {
        let t = Typing::default();
        let (c, w) = (Uuid::new_v4(), Uuid::new_v4());
        let t0 = Instant::now();
        assert_eq!(t.ping(c, w, true, t0), Decision::Publish(true));
        assert_eq!(t.ping(c, w, true, t0 + Duration::from_secs(1)), Decision::Skip);
        assert_eq!(t.ping(c, w, true, t0 + REPUBLISH_EVERY), Decision::Publish(true));
        assert_eq!(t.ping(c, w, false, t0 + REPUBLISH_EVERY + Duration::from_millis(10)), Decision::Publish(false));
        assert_eq!(t.ping(c, w, false, t0 + REPUBLISH_EVERY + Duration::from_millis(20)), Decision::Skip);
        assert_eq!(t.ping(c, w, true, t0 + REPUBLISH_EVERY + Duration::from_millis(30)), Decision::Publish(true));
    }

    #[test]
    fn quiet_typing_expires_once() {
        let t = Typing::default();
        let (c, w) = (Uuid::new_v4(), Uuid::new_v4());
        let t0 = Instant::now();
        t.ping(c, w, true, t0);
        assert!(t.expire(t0 + Duration::from_secs(1)).is_empty());
        assert_eq!(t.expire(t0 + EXPIRE_AFTER), vec![(c, w)]);
        assert!(t.expire(t0 + EXPIRE_AFTER * 2).is_empty());
    }

    #[test]
    fn over_budget_is_refused_until_the_window_turns() {
        let t = Typing::default();
        let t0 = Instant::now();
        for _ in 0..MAX_REQUESTS {
            assert!(t.admit(t0));
        }
        assert!(!t.admit(t0 + Duration::from_secs(1)));
        assert!(t.admit(t0 + WINDOW));
    }
}
