//! Native notifications, kept quiet.
//!
//! The desktop notifies only for things that need *you*: a run you asked for
//! finished, failed, or stopped to ask you something; you were mentioned; a
//! proposal waits on your vote; and, when you turned it on for an agent,
//! someone else used it. Everything else stays in the window (chat-row
//! states, the inbox).
//!
//! Every notice goes through [`post`]: nothing shows while the window has
//! focus (the window already shows it), and notices that arrive close
//! together are batched into one ("3 chats finished") instead of a burst.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use ligus_core::{ChatMessage, ChatSession, MessageRole};
use ligus_runtime::ChatService;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;
use uuid::Uuid;

use crate::{shown, AppState, NOTIFIED_CAP};

/// How long to wait for more notices before showing one.
const BATCH_WINDOW: Duration = Duration::from_millis(1500);

/// A synced reply older than this isn't news (a first sync, a long offline
/// stretch): it shows in the chat, without a notification.
const SYNCED_FRESH_SECS: i64 = 10 * 60;

/// Why a notice is shown. Ordered by urgency: a chat that both finished and
/// asked you something shows as asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NoticeKind {
    /// An agent asked you something, or mentioned you.
    NeedsYou,
    /// A proposal waits on your vote.
    Vote,
    /// A run you asked for failed.
    Failed,
    /// A run you asked for finished.
    Done,
    /// Someone else used an agent of yours (opt-in per agent).
    Used,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Notice {
    pub kind: NoticeKind,
    /// The chat it's about, so a chat that changes twice in one batch counts once.
    pub chat: Option<Uuid>,
    /// What a notice on its own says.
    pub line: String,
}

impl Notice {
    pub fn new(kind: NoticeKind, chat: Option<Uuid>, line: impl Into<String>) -> Self {
        Self { kind, chat, line: line.into() }
    }
}

/// One notice per chat (the most urgent), in arrival order. Notices without
/// a chat are kept as they are.
pub(crate) fn coalesce(notices: Vec<Notice>) -> Vec<Notice> {
    let mut out: Vec<Notice> = Vec::new();
    for n in notices {
        match n.chat.and_then(|c| out.iter().position(|o| o.chat == Some(c))) {
            Some(i) if n.kind <= out[i].kind => out[i] = n,
            Some(_) => {}
            None => out.push(n),
        }
    }
    out
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The text of one notification for a batch: the notice itself when there's
/// one, else a count per kind ("2 need you · 1 failed · 3 finished").
pub(crate) fn summarize(notices: &[Notice]) -> Option<String> {
    match notices {
        [] => None,
        [one] => Some(one.line.clone()),
        _ => {
            let of = |k: NoticeKind| notices.iter().filter(|n| n.kind == k).count();
            let kinds = [NoticeKind::NeedsYou, NoticeKind::Vote, NoticeKind::Failed, NoticeKind::Done, NoticeKind::Used];
            let present: Vec<NoticeKind> = kinds.into_iter().filter(|k| of(*k) > 0).collect();
            let phrase = |k: NoticeKind, alone: bool| {
                let n = of(k);
                match k {
                    NoticeKind::NeedsYou => format!("{n} need{} you", if n == 1 { "s" } else { "" }),
                    NoticeKind::Vote => count(n, "vote needs you", "votes need you"),
                    NoticeKind::Failed if alone => format!("{} failed", count(n, "run", "runs")),
                    NoticeKind::Failed => format!("{n} failed"),
                    NoticeKind::Done if alone => format!("{} finished", count(n, "chat", "chats")),
                    NoticeKind::Done => format!("{n} finished"),
                    NoticeKind::Used => format!("your agents were used {}", count(n, "time", "times")),
                }
            };
            let alone = present.len() == 1;
            let parts: Vec<String> = present.iter().map(|k| phrase(*k, alone)).collect();
            let mut s = parts.join(" · ");
            if let Some(first) = s.get(0..1) {
                s = first.to_uppercase() + &s[1..];
            }
            Some(s)
        }
    }
}

struct Batch {
    pending: Mutex<Vec<Notice>>,
    armed: AtomicBool,
}

fn batch() -> &'static Batch {
    static B: OnceLock<Batch> = OnceLock::new();
    B.get_or_init(|| Batch { pending: Mutex::new(Vec::new()), armed: AtomicBool::new(false) })
}

/// The window is in front: it already shows the change (chat-row states,
/// the inbox), so a native notification would only repeat it.
fn window_focused(app: &AppHandle) -> bool {
    app.get_webview_window("main").and_then(|w| w.is_focused().ok()).unwrap_or(false)
}

/// Queue a notice. It shows after a short quiet window, batched with any
/// that arrive meanwhile, unless the window has focus by then.
pub(crate) fn post(app: &AppHandle, notice: Notice) {
    let b = batch();
    b.pending.lock().unwrap_or_else(|e| e.into_inner()).push(notice);
    if b.armed.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(BATCH_WINDOW).await;
        let b = batch();
        b.armed.store(false, Ordering::SeqCst);
        let drained = std::mem::take(&mut *b.pending.lock().unwrap_or_else(|e| e.into_inner()));
        if window_focused(&app) {
            return;
        }
        if let Some(body) = summarize(&coalesce(drained)) {
            let _ = app.notification().builder().title("Ligus").body(body).show();
        }
    });
}

/// The tray tooltip for a needs-you count.
pub(crate) fn tray_tooltip(count: u32) -> String {
    match count {
        0 => "Ligus".into(),
        1 => "Ligus · 1 needs you".into(),
        n => format!("Ligus · {n} need you"),
    }
}

/// How many things need this person (the window's inbox count): the dock
/// badge on macOS and the tray tooltip. Zero clears the badge. Nothing else
/// (unread chats, activity, suggestions) is ever counted here.
#[tauri::command]
pub(crate) fn set_needs_you_count(app: AppHandle, count: u32) {
    if let Some(w) = app.get_webview_window("main") {
        // Not every platform has a badge (Windows); the tooltip still says it.
        let _ = w.set_badge_count((count > 0).then_some(i64::from(count)));
    }
    if let Some(tray) = app.tray_by_id("ligus-tray") {
        let _ = tray.set_tooltip(Some(tray_tooltip(count)));
    }
}

/// "in “Title”", or nothing for an untitled chat.
fn in_chat(s: &ChatSession) -> String {
    let t = shown(&s.title);
    if t.trim().is_empty() {
        String::new()
    } else {
        format!(" in “{t}”")
    }
}

fn agent_name(s: &ChatSession, m: &ChatMessage) -> String {
    m.responder_id
        .and_then(|a| s.workspace_agents.iter().find(|x| x.id == a))
        .map(|a| shown(&a.name))
        .unwrap_or_else(|| shown(&m.author))
}

/// Whether this device's person asked for the turn `reply` answers (the
/// nearest human behind its trigger). Unknown is no.
fn asked_by_me(svc: &ChatService, s: &ChatSession, reply: &ChatMessage) -> bool {
    let Some(trigger) = reply.reply_to_message_id else { return false };
    let links = svc.device_links(s.workspace_id).unwrap_or_default();
    let me = links.person(&svc.author().id).to_string();
    svc.trigger_people(s.id, trigger)
        .ok()
        .flatten()
        .and_then(|p| p.first().cloned())
        .is_some_and(|p| links.person(&p) == me)
}

/// The notice for a finished or failed reply in a run this person asked for,
/// if it warrants one. A stopped or empty reply doesn't.
fn reply_notice(svc: &ChatService, s: &ChatSession, reply: &ChatMessage, failed: bool) -> Option<Notice> {
    reply_notice_with(s, reply, failed, |m| asked_by_me(svc, s, m))
}

fn reply_notice_with(s: &ChatSession, reply: &ChatMessage, failed: bool, mine: impl Fn(&ChatMessage) -> bool) -> Option<Notice> {
    if !matches!(reply.role, MessageRole::Assistant | MessageRole::Agent) || reply.superseded {
        return None;
    }
    let failed = failed || reply.body.starts_with("[error]");
    if !failed && reply.body.trim().is_empty() {
        return None;
    }
    if !mine(reply) {
        return None;
    }
    let who = agent_name(s, reply);
    Some(if failed {
        Notice::new(NoticeKind::Failed, Some(s.id), format!("{who}'s run failed{}", in_chat(s)))
    } else {
        Notice::new(NoticeKind::Done, Some(s.id), format!("{who} finished{}", in_chat(s)))
    })
}

/// A turn this device ran ended (the turn engine's terminal phase).
pub(crate) fn turn_ended(app: &AppHandle, state: &AppState, chat: Uuid, message_id: Uuid, failed: bool) {
    let notice = {
        let svc = state.service.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = svc.load(chat).ok().flatten() else { return };
        let Some(m) = s.messages.iter().find(|m| m.id == message_id) else { return };
        reply_notice(&svc, &s, m, failed)
    };
    if let Some(n) = notice {
        if state.notified.lock().unwrap_or_else(|e| e.into_inner()).insert(message_id, NOTIFIED_CAP) {
            post(app, n);
        }
    }
}

/// Replies a sync brought in from other devices (a worker, `ligus serve`,
/// another of your devices) that end a run you asked for.
pub(crate) fn synced_replies(app: &AppHandle, state: &AppState, chat: Uuid, ids: &[Uuid]) {
    let now = ligus_core::Timestamp::now().0;
    let notices: Vec<(Uuid, Notice)> = {
        let svc = state.service.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = svc.load(chat).ok().flatten() else { return };
        s.messages
            .iter()
            .filter(|m| ids.contains(&m.id) && !m.is_streaming && (now - m.created_at.0).whole_seconds() < SYNCED_FRESH_SECS)
            .filter_map(|m| reply_notice(&svc, &s, m, false).map(|n| (m.id, n)))
            .collect()
    };
    for (id, n) in notices {
        if state.notified.lock().unwrap_or_else(|e| e.into_inner()).insert(id, NOTIFIED_CAP) {
            post(app, n);
        }
    }
}

/// An agent turn on this device asked a question (`ligus_ask_human`) in a
/// run this person asked for.
pub(crate) fn ask_posted(app: &AppHandle, state: &AppState, chat: Uuid, turn_message: Uuid, ask_id: Uuid) {
    let notice = {
        let svc = state.service.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = svc.load(chat).ok().flatten() else { return };
        let Some(m) = s.messages.iter().find(|m| m.id == turn_message) else { return };
        if !asked_by_me(&svc, &s, m) {
            return;
        }
        Notice::new(NoticeKind::NeedsYou, Some(chat), format!("{} asked you something{}", agent_name(&s, m), in_chat(&s)))
    };
    if state.notified.lock().unwrap_or_else(|e| e.into_inner()).insert(ask_id, NOTIFIED_CAP) {
        post(app, notice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(kind: NoticeKind, chat: u128, line: &str) -> Notice {
        Notice::new(kind, (chat != 0).then(|| Uuid::from_u128(chat)), line)
    }

    fn reply(s: &mut ChatSession, body: &str) -> ChatMessage {
        let ask = ChatMessage::new(MessageRole::User, "Ana", "@builder fix it");
        let mut r = ChatMessage::new(MessageRole::Assistant, "builder", body);
        r.reply_to_message_id = Some(ask.id);
        s.messages.push(ask);
        s.messages.push(r.clone());
        r
    }

    #[test]
    fn only_runs_you_asked_for_notify() {
        let mut s = ChatSession::new("Relay backoff", Uuid::new_v4(), "rt");
        let r = reply(&mut s, "Done: retry cap is 30s.");
        let done = reply_notice_with(&s, &r, false, |_| true).unwrap();
        assert_eq!((done.kind, done.line.as_str()), (NoticeKind::Done, "builder finished in “Relay backoff”"));
        assert_eq!(reply_notice_with(&s, &r, false, |_| false), None, "someone else's run");
        let failed = reply_notice_with(&s, &r, true, |_| true).unwrap();
        assert_eq!((failed.kind, failed.line.as_str()), (NoticeKind::Failed, "builder's run failed in “Relay backoff”"));
    }

    #[test]
    fn a_stopped_or_human_message_is_not_news() {
        let mut s = ChatSession::new("", Uuid::new_v4(), "rt");
        let stopped = reply(&mut s, "  ");
        assert_eq!(reply_notice_with(&s, &stopped, false, |_| true), None);
        let errored = reply(&mut s, "[error] the model refused");
        let n = reply_notice_with(&s, &errored, false, |_| true).unwrap();
        assert_eq!((n.kind, n.line.as_str()), (NoticeKind::Failed, "builder's run failed"));
        let human = ChatMessage::new(MessageRole::User, "Ana", "thanks");
        assert_eq!(reply_notice_with(&s, &human, false, |_| true), None);
    }

    #[test]
    fn the_tray_says_how_many_need_you() {
        assert_eq!(tray_tooltip(0), "Ligus");
        assert_eq!(tray_tooltip(1), "Ligus · 1 needs you");
        assert_eq!(tray_tooltip(4), "Ligus · 4 need you");
    }

    #[test]
    fn one_notice_says_itself() {
        assert_eq!(summarize(&[n(NoticeKind::Done, 1, "builder finished in “Relay backoff”")]).as_deref(), Some("builder finished in “Relay backoff”"));
        assert_eq!(summarize(&[]), None);
    }

    #[test]
    fn a_burst_of_one_kind_is_counted() {
        let done = [n(NoticeKind::Done, 1, "a"), n(NoticeKind::Done, 2, "b"), n(NoticeKind::Done, 3, "c")];
        assert_eq!(summarize(&done).as_deref(), Some("3 chats finished"));
        let failed = [n(NoticeKind::Failed, 1, "a"), n(NoticeKind::Failed, 2, "b")];
        assert_eq!(summarize(&failed).as_deref(), Some("2 runs failed"));
        let votes = [n(NoticeKind::Vote, 0, "a"), n(NoticeKind::Vote, 0, "b")];
        assert_eq!(summarize(&votes).as_deref(), Some("2 votes need you"));
    }

    #[test]
    fn a_mixed_burst_lists_the_most_urgent_first() {
        let mixed = [n(NoticeKind::Done, 1, "a"), n(NoticeKind::Failed, 2, "b"), n(NoticeKind::NeedsYou, 3, "c"), n(NoticeKind::Done, 4, "d")];
        assert_eq!(summarize(&mixed).as_deref(), Some("1 needs you · 1 failed · 2 finished"));
    }

    #[test]
    fn a_chat_counts_once_with_its_most_urgent_notice() {
        let got = coalesce(vec![n(NoticeKind::Done, 1, "done"), n(NoticeKind::NeedsYou, 1, "asks"), n(NoticeKind::Done, 1, "done again"), n(NoticeKind::Done, 2, "other")]);
        assert_eq!(got, vec![n(NoticeKind::NeedsYou, 1, "asks"), n(NoticeKind::Done, 2, "other")]);
        // Notices without a chat are never merged.
        assert_eq!(coalesce(vec![n(NoticeKind::Vote, 0, "a"), n(NoticeKind::Vote, 0, "b")]).len(), 2);
    }
}
