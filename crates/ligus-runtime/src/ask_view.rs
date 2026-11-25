//! What every surface draws for an agent question (`docs/design/agent-questions.md`
//! section 7): the desktop's `AskDto`, `ligus serve`'s `ask_dto`, `lt` and
//! the CLI build their card from [`views`], so they agree on who may answer,
//! what the answer does and how the card reads.
//!
//! The agent's words are cleaned here again (6.4: every reader cleans, so a
//! crafted record can't skip it). Grant labels come from Ligus's constants
//! by index, never from the record. Names come from the roster.

use ligus_core::flow::{AskState, AskVote, EFFECT_SECRET, ORIGIN_OUTSIDE};
use ligus_core::{ActorKind, ChatSession, WorkspaceRole};
use ligus_proto::ask_text;
use serde::Serialize;

/// Who an ask is put to, as a card says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudienceView {
    /// `people` or `room`.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub member_ids: Vec<String>,
    /// The named people's roster names, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    /// For a room: `contributor`, `admin` or `owner`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_role: Option<&'static str>,
}

/// An effect that counts (a grant card).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectView {
    /// `permission` or `secret`.
    pub kind: String,
    pub tool: String,
    /// The agent's own description, cleaned, one line (shown as "… describes
    /// it as: …").
    pub title: String,
    /// The call's own content, verbatim (shown in mono, scrolling).
    pub detail: String,
    pub cwd: String,
    pub host_device_id: Option<String>,
    /// The host's label (its workspace host name), when known.
    pub host_label: String,
    /// An answer needs the person's passkey.
    pub presence_required: bool,
    pub standing: bool,
    /// Too large to review on a card: Deny only.
    pub too_large: bool,
}

/// Where the ask came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginView {
    pub via: String,
    pub label: String,
    pub chat_id: Option<String>,
    pub runtime: String,
}

/// One person's answer, named from the roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoteView {
    pub member_id: String,
    pub name: String,
    pub device_id: Option<String>,
    pub at_ms: i64,
}

/// A card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AskView {
    pub ask_id: String,
    pub chat_id: String,
    /// The asking agent's name (or `Claude Code` for an outside prompt).
    pub agent_name: String,
    /// `question`, `permission` or `secret`.
    pub kind: &'static str,
    /// 2 for an ask with the 0.4 rules; 0 for an older one.
    pub v: u8,
    /// The agent's words, cleaned, plain text.
    pub question: String,
    /// The options to show, cleaned (a grant's are Ligus's labels).
    pub options: Vec<String>,
    pub allow_text: bool,
    pub audience: AudienceView,
    pub quorum: u8,
    /// This device's person may answer it.
    pub eligible: bool,
    pub effect: Option<EffectView>,
    pub origin: Option<OriginView>,
    /// Votes per option (a quorum card).
    pub tally: Vec<u32>,
    pub expires_at_ms: i64,
    /// Not settled, closed or declined.
    pub open: bool,
    /// Open and not expired: the buttons show.
    pub live: bool,
    pub declined: bool,
    /// The winning option and free text.
    pub answer: Option<String>,
    pub answer_text: Option<String>,
    pub answered_by: Vec<VoteView>,
    /// Someone answered from an older version, and it isn't counted.
    pub older_answer_uncounted: bool,
    /// Posted by someone other than the agent or its host (theirs; never a
    /// grant).
    pub posted_by: Option<String>,
    pub at_ms: i64,
}

/// The roster name of `id` (cleaned), or `fallback` cleaned, or the id.
fn name_of(session: &ChatSession, id: &str, fallback: &str) -> String {
    session
        .members
        .iter()
        .find(|m| m.id == id)
        .map(|m| ligus_proto::names::clean_display_name(&m.actor.display_name))
        .filter(|n| !n.is_empty())
        .or_else(|| Some(ligus_proto::names::clean_display_name(fallback)).filter(|n| !n.is_empty()))
        .unwrap_or_else(|| id.to_string())
}

fn role_str(r: WorkspaceRole) -> &'static str {
    match r {
        WorkspaceRole::Owner => "owner",
        WorkspaceRole::Admin => "admin",
        _ => "contributor",
    }
}

/// Whether `me` (a member id) may answer `st` here: what the fold will
/// count, judged at this device's roster (the fold has the last word).
pub fn eligible(session: &ChatSession, st: &AskState, me: &str) -> bool {
    let Some(m) = session.members.iter().find(|m| m.id == me) else { return false };
    if m.actor.kind != ActorKind::Human || m.role.rank() < WorkspaceRole::Contributor.rank() {
        return false;
    }
    let audience_ok = match st.ask.audience.as_ref() {
        Some(a) if a.kind == ligus_core::flow::AUDIENCE_PEOPLE => a.member_ids.iter().any(|x| x == me),
        Some(a) if a.kind == ligus_core::flow::AUDIENCE_ROOM => a.floor().is_some_and(|f| m.role.rank() >= f.rank()),
        Some(_) => false,
        None => true,
    };
    if !audience_ok {
        return false;
    }
    if st.ask.permission && !st.is_v2() {
        // The older grant rule: the host's person or the agent's owner.
        let owner = ligus_core::flow::agent_from_key(&st.ask.agent_key)
            .and_then(|a| session.workspace_agents.iter().find(|x| x.id == a))
            .map(|a| a.owner_actor_id.clone());
        let host_person = st.host.is_some_and(|h| session.members.iter().any(|x| x.id == me && x.actor.device_id == Some(h)));
        return owner.as_deref() == Some(me) || host_person || st.host.is_none();
    }
    true
}

fn vote_view(session: &ChatSession, v: &AskVote) -> VoteView {
    VoteView { member_id: v.member_id.clone(), name: name_of(session, &v.member_id, &v.name), device_id: v.device_id.map(|d| d.to_string()), at_ms: v.at_ms }
}

/// The card for one ask, for `me` (this device's person) at `now_ms`.
/// `host_label` names a device (its workspace host label), when known.
pub fn view(session: &ChatSession, st: &AskState, me: &str, now_ms: i64, host_label: &dyn Fn(uuid::Uuid) -> Option<String>) -> AskView {
    let outside = st.ask.origin.as_ref().is_some_and(|o| o.via == ORIGIN_OUTSIDE);
    let agent_name = if outside {
        match st.ask.origin.as_ref().map(|o| o.runtime.as_str()) {
            Some("claude-code") | None | Some("") => "Claude Code".to_string(),
            Some(other) => ask_text::cut(&ask_text::clean_option(other), 40),
        }
    } else {
        ligus_core::flow::agent_from_key(&st.ask.agent_key)
            .and_then(|id| session.workspace_agents.iter().find(|a| a.id == id).map(|a| ligus_proto::names::clean_display_name(&a.name)))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Ligus".into())
    };
    let grant = st.grant();
    let effect = grant.as_ref().map(|e| EffectView {
        kind: e.kind.clone(),
        tool: ask_text::cut(&ask_text::clean_option(&e.tool), 60),
        title: ask_text::cut(&ask_text::clean_option(&e.title), 200),
        detail: e.detail.chars().filter(|c| *c == '\n' || *c == '\t' || !ligus_proto::names::is_hidden_name_char(*c)).collect(),
        cwd: ask_text::clean_option(&e.cwd),
        host_device_id: e.host_device_id.or(st.host).map(|d| d.to_string()),
        host_label: e.host_device_id.or(st.host).and_then(host_label).unwrap_or_default(),
        presence_required: e.needs_presence(),
        standing: e.standing,
        too_large: e.too_large,
    });
    let kind = match &grant {
        Some(e) if e.kind == EFFECT_SECRET => "secret",
        Some(_) => "permission",
        None => "question",
    };
    // A grant's buttons are Ligus's labels by index, never the record's.
    let options: Vec<String> = match &grant {
        Some(e) if e.standing => ask_text::STANDING_GRANT_LABELS.iter().map(|s| s.to_string()).collect(),
        Some(_) => ask_text::GRANT_LABELS.iter().map(|s| s.to_string()).collect(),
        None => st.ask.options.iter().map(|o| ask_text::cut(&ask_text::clean_option(o), if st.is_v2() { ask_text::MAX_OPTION_CHARS } else { 200 })).collect(),
    };
    let audience = match st.ask.audience.as_ref() {
        Some(a) if a.kind == ligus_core::flow::AUDIENCE_PEOPLE => AudienceView {
            kind: "people",
            member_ids: a.member_ids.clone(),
            names: a.member_ids.iter().map(|id| name_of(session, id, "")).collect(),
            min_role: None,
        },
        Some(a) => AudienceView { kind: "room", member_ids: Vec::new(), names: Vec::new(), min_role: Some(a.floor().map(role_str).unwrap_or("owner")) },
        None => AudienceView { kind: "room", member_ids: Vec::new(), names: Vec::new(), min_role: Some("contributor") },
    };
    let answer = st.answer.as_ref();
    AskView {
        ask_id: st.ask.ask_id.to_string(),
        chat_id: session.id.to_string(),
        agent_name,
        kind,
        v: st.ask.v,
        question: ask_text::cut(&ask_text::clean_question(&st.ask.question), if st.is_v2() { ask_text::MAX_QUESTION_CHARS } else { 2000 }),
        options: options.clone(),
        allow_text: if st.is_v2() { st.ask.allow_text && grant.is_none() } else { grant.is_none() },
        audience,
        quorum: st.quorum(),
        eligible: eligible(session, st, me),
        effect,
        origin: st.ask.origin.as_ref().map(|o| OriginView {
            via: o.via.clone(),
            label: ask_text::cut(&ask_text::clean_option(&o.label), 60),
            chat_id: o.chat_id.map(|c| c.to_string()),
            runtime: ask_text::clean_option(&o.runtime),
        }),
        tally: st.tally(),
        expires_at_ms: st.ask.expires_at_ms,
        open: st.is_open(),
        live: st.is_live(now_ms),
        declined: st.declined,
        answer: answer.map(|a| match a.option_index.and_then(|i| options.get(i as usize)) {
            Some(o) => o.clone(),
            None => ask_text::cut(&ask_text::clean_option(&a.answer), 200),
        }),
        answer_text: answer.map(|a| ask_text::clean_question(&a.text)).filter(|t| !t.is_empty()),
        answered_by: st.answered_by.iter().map(|v| vote_view(session, v)).collect(),
        older_answer_uncounted: st.uncounted.iter().any(|v| v.why == "answered on an older version"),
        posted_by: st.poster_name(&session.members).map(|n| ligus_proto::names::clean_display_name(&n)),
        at_ms: st.ask.at_ms,
    }
}

/// Every card in a chat, newest first (at most `limit`).
pub fn views(session: &ChatSession, me: &str, now_ms: i64, limit: usize, host_label: &dyn Fn(uuid::Uuid) -> Option<String>) -> Vec<AskView> {
    let mut v: Vec<&AskState> = session.flow.asks.values().collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.ask.at_ms));
    v.into_iter().take(limit).map(|st| view(session, st, me, now_ms, host_label)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::flow::{AskAudience, AskEffect, HumanAsk, ASK_V2, EFFECT_PERMISSION};

    fn session() -> ChatSession {
        let mut s = ChatSession::new("t", uuid::Uuid::nil(), "rt");
        for (id, role) in [("ana", WorkspaceRole::Owner), ("bo", WorkspaceRole::Contributor), ("cy", WorkspaceRole::Viewer)] {
            s.members.push(ligus_core::identity::WorkspaceMember {
                id: id.into(),
                actor: ligus_core::ActorIdentity::new(id, &id.to_uppercase(), ActorKind::Human),
                role,
                title: String::new(),
                index: 0,
                joined_at: Default::default(),
            });
        }
        s
    }

    /// Q-T7 (readers clean again): a crafted record's bidi, zero-width and
    /// newlines in options don't reach a card; a grant's buttons are Ligus's.
    #[test]
    fn readers_clean_and_draw_grant_labels_by_index() {
        let s = session();
        let st = AskState {
            ask: HumanAsk {
                question: "Safe?\u{202E} yes\n\n\n\n\n\n\n\n\n\n\nmore".into(),
                options: vec!["De\u{200B}ny\nthis".into(), "Ok".into()],
                v: ASK_V2,
                audience: Some(AskAudience::people(vec!["bo".into()])),
                ..Default::default()
            },
            ..Default::default()
        };
        let v = view(&s, &st, "bo", 0, &|_| None);
        assert!(!v.question.contains('\u{202E}'));
        assert!(v.question.matches('\n').count() <= 8);
        assert_eq!(v.options[0], "Deny this");
        assert!(v.eligible && !view(&s, &st, "ana", 0, &|_| None).eligible);
        assert_eq!(v.audience.names, vec!["BO"]);
        let g = AskState {
            ask: HumanAsk {
                options: vec!["Deny".into(), "Allow once".into()],
                permission: true,
                v: ASK_V2,
                effect: Some(AskEffect { kind: EFFECT_PERMISSION.into(), detail: "rm -rf build".into(), presence: "required".into(), ..Default::default() }),
                audience: Some(AskAudience::people(vec!["ana".into()])),
                ..Default::default()
            },
            ..Default::default()
        };
        let v = view(&s, &g, "ana", 0, &|_| None);
        assert_eq!(v.kind, "permission");
        assert_eq!(v.options, vec!["Allow once", "Deny"], "labels by index, never the record's swapped ones");
        assert!(v.effect.as_ref().unwrap().presence_required && !v.allow_text);
    }

    #[test]
    fn viewers_and_rooms_below_the_floor_are_not_eligible() {
        let s = session();
        let st = AskState { ask: HumanAsk { v: ASK_V2, audience: Some(AskAudience::room(WorkspaceRole::Owner)), ..Default::default() }, ..Default::default() };
        assert!(eligible(&s, &st, "ana") && !eligible(&s, &st, "bo"));
        let open = AskState::default();
        assert!(eligible(&s, &open, "bo") && !eligible(&s, &open, "cy"), "a Viewer never answers");
    }
}
