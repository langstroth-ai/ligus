//! Agent questions on this device (`docs/design/agent-questions.md`):
//! answering one over the digest of the record this device holds, with the
//! person's passkey when the card's effect needs it, and the restart sweep
//! that closes the asks a host stopped waiting on.

use ligus_core::flow::{asks as fold, AskState, FlowRecord, HumanAnswered, HumanAsk};
use ligus_core::{SessionEvent, SessionEventEnvelope};
use uuid::Uuid;

use super::{ChatError, ChatService, Result};

/// What a person picked on a card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AskChoice {
    /// The option (an index into the ask's options).
    pub option: Option<u32>,
    /// Free text (only where the ask allows it; up to 1000 characters).
    pub text: String,
    /// "Not now".
    pub decline: bool,
}

impl ChatService {
    /// Record a flow record and return the envelope it was signed in.
    pub fn record_flow_signed(&mut self, session_id: Uuid, workspace_id: Uuid, record: FlowRecord) -> Result<SessionEventEnvelope> {
        self.append_signed(session_id, workspace_id, SessionEvent::FlowRecorded { record })
    }

    /// The envelope that posted `ask_id` in a chat: the event the fold
    /// credits with it (an ask id belongs to its first signer that the fold
    /// accepted; a refused squat under the id is never it).
    pub fn ask_envelope(&self, workspace_id: Uuid, session_id: Uuid, ask_id: Uuid) -> Result<Option<SessionEventEnvelope>> {
        let Some(session) = self.load_in(workspace_id, session_id)? else { return Ok(None) };
        let Some(event_id) = session.flow.asks.get(&ask_id).and_then(|st| st.event_id) else { return Ok(None) };
        let envs = self.store.list_copies_in(workspace_id, session_id)?;
        Ok(envs.into_iter().find(|e| {
            e.event_id == event_id && matches!(&e.payload, SessionEvent::FlowRecorded { record: FlowRecord::HumanAsk(a) } if a.ask_id == ask_id)
        }))
    }

    /// The digest of the ask as this device holds it ([`fold::ask_digest`]).
    pub fn ask_digest_here(&self, workspace_id: Uuid, session_id: Uuid, ask_id: Uuid) -> Result<Option<String>> {
        Ok(self.ask_envelope(workspace_id, session_id, ask_id)?.as_ref().and_then(fold::envelope_ask_digest))
    }

    /// Answer `ask_id` as this device's person: the option (by index, its
    /// text taken from the ask), free text where allowed, or "Not now",
    /// bound to the digest of the signed ask this device holds. A grant
    /// whose effect needs presence is signed with the person's passkey.
    /// Returns the answer's envelope.
    pub fn answer_ask(&mut self, session_id: Uuid, workspace_id: Uuid, ask_id: Uuid, choice: AskChoice) -> Result<SessionEventEnvelope> {
        let session = self.load_in(workspace_id, session_id)?.ok_or_else(|| ChatError::NotFound("chat".into()))?;
        let st = session.flow.asks.get(&ask_id).cloned().ok_or_else(|| ChatError::NotFound("no such question".into()))?;
        let rec = answer_record(self, &session, &st, &choice)?;
        let presence = st.is_v2() && st.grant_needs_presence();
        self.force_presence = presence;
        let r = self.record_flow_signed(session_id, workspace_id, rec);
        self.force_presence = false;
        r
    }

    /// The restart sweep (section 5.3): close every open ask this device
    /// posted (as itself or through an agent key it certified). Its turns
    /// ended with the process, so nobody waits on them. Returns how many.
    pub fn sweep_open_asks(&mut self) -> Result<usize> {
        let mut closed = 0usize;
        let me = self.device_id;
        for session_id in self.store.list_session_ids()? {
            let Ok(Some(session)) = self.load(session_id) else { continue };
            // The device-local workspace folds without signers: every ask
            // in it is this device's.
            let mine: Vec<HumanAsk> = session
                .flow
                .asks
                .values()
                .filter(|s| s.is_open() && (s.host == Some(me) || (s.host.is_none() && self.local_workspace_id == Some(session.workspace_id))))
                .map(|s| HumanAsk {
                    ask_id: s.ask.ask_id,
                    turn_id: s.ask.turn_id,
                    agent_key: s.ask.agent_key.clone(),
                    closed: true,
                    at_ms: ligus_core::flow::now_ms(),
                    v: s.ask.v,
                    ..Default::default()
                })
                .collect();
            for close in mine {
                match self.record_flow_signed(session_id, session.workspace_id, FlowRecord::HumanAsk(close)) {
                    Ok(_) => closed += 1,
                    Err(e) => tracing::warn!(target: "asks", chat = %session_id, "restart sweep: {e}"),
                }
            }
        }
        Ok(closed)
    }
}

/// The answer record for `choice` on `st` (shared with the phone's draft).
pub fn answer_record(svc: &ChatService, session: &ligus_core::ChatSession, st: &AskState, choice: &AskChoice) -> Result<FlowRecord> {
    if !st.is_open() {
        return Err(ChatError::Unauthorized("that question is already answered or closed".into()));
    }
    if st.ask.expires_at_ms > 0 && ligus_core::flow::now_ms() >= st.ask.expires_at_ms {
        return Err(ChatError::Unauthorized("too late: the agent stopped waiting".into()));
    }
    let text: String = ligus_proto::ask_text::clean_question(&choice.text).chars().take(ligus_proto::ask_text::MAX_TEXT_ANSWER_CHARS).collect();
    let option_text = match choice.option {
        Some(i) => Some(st.ask.options.get(i as usize).cloned().ok_or_else(|| ChatError::Unauthorized("no such option".into()))?),
        None => None,
    };
    if st.is_v2() {
        if !choice.decline && choice.option.is_none() && !(st.ask.allow_text && !text.is_empty()) {
            return Err(ChatError::Unauthorized("pick an option".into()));
        }
        if !text.is_empty() && !st.ask.allow_text {
            return Err(ChatError::Unauthorized("this question takes no free text".into()));
        }
    } else if choice.decline {
        // An older ask settles on its first answer for everyone: "Not now"
        // there is only hiding it on this device, never a record.
        return Err(ChatError::Unauthorized("an older question can't be declined; leave it unanswered".into()));
    } else if option_text.is_none() && text.is_empty() {
        return Err(ChatError::Unauthorized("an answer is required".into()));
    }
    let digest = if st.is_v2() {
        svc.ask_digest_here(session.workspace_id, session.id, st.ask.ask_id)?.ok_or_else(|| ChatError::NotFound("the signed question isn't here yet".into()))?
    } else {
        String::new()
    };
    // The option's text stays in `answer` for older readers; an older ask
    // keeps its free text there too.
    let answer = match (&option_text, st.is_v2()) {
        _ if choice.decline => ligus_proto::ask_text::NOT_NOW.to_string(),
        (Some(o), _) => o.clone(),
        (None, true) => String::new(),
        (None, false) => text.clone(),
    };
    Ok(FlowRecord::HumanAnswered(HumanAnswered {
        ask_id: st.ask.ask_id,
        answer: answer.chars().take(ligus_core::flow::MAX_ANSWER_CHARS).collect(),
        option_index: if choice.decline { None } else { choice.option },
        by: svc.author().display_name.clone(),
        at_ms: ligus_core::flow::now_ms(),
        text: if st.is_v2() { text } else { String::new() },
        ask_digest: digest,
        decline: choice.decline,
    }))
}

/// What an outside prompt (`ligus hook permission`) asks the person to
/// allow, as [`crate::hook::outside_detail`] reads it from Claude Code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutsidePrompt {
    /// The tool (`Bash`, `Write`, `mcp__…`).
    pub tool: String,
    /// The call's own content.
    pub detail: String,
    pub cwd: String,
    /// The label the card shows (the project folder's name).
    pub label: String,
    /// Too large to review on a card: Deny only.
    pub too_large: bool,
    /// The Claude Code sub-agent that asked (`agent_type`), when one did.
    pub subagent: String,
}

/// The agent key an outside prompt is posted under.
pub const OUTSIDE_AGENT_KEY: &str = "outside:claude-code";

impl ChatService {
    /// Post an outside prompt as a grant card to this device's own person
    /// (section 8.2): signed by this device as its person, put to that
    /// person alone, its passkey always required. Returns the ask and the
    /// digest an answer must name. Refused when this device's identity
    /// isn't a person in the workspace (a box is its own member: its card
    /// couldn't be a grant).
    pub fn post_outside_ask(&mut self, workspace_id: Uuid, chat_id: Uuid, p: &OutsidePrompt, timeout: std::time::Duration) -> Result<(HumanAsk, String)> {
        if self.author.kind != ligus_core::ActorKind::Human || self.delegated {
            return Err(ChatError::Unauthorized("only a person's own device can raise a permission prompt to them".into()));
        }
        let session = self.load_in(workspace_id, chat_id)?.ok_or_else(|| ChatError::NotFound("chat".into()))?;
        if session.workspace_id != workspace_id {
            return Err(ChatError::NotFound("chat".into()));
        }
        let me = self.author.id.clone();
        if !session.members.iter().any(|m| m.id == me) {
            return Err(ChatError::Unauthorized("you aren't a member of that chat's workspace".into()));
        }
        let now = ligus_core::flow::now_ms();
        let ask = HumanAsk {
            ask_id: Uuid::new_v4(),
            turn_id: Uuid::nil(),
            agent_key: OUTSIDE_AGENT_KEY.into(),
            question: {
                let short = |s: &str| ligus_proto::ask_text::cut(&ligus_proto::ask_text::clean_option(s), 60);
                let via = if p.subagent.is_empty() { String::new() } else { format!(" (through its {} sub-agent)", short(&p.subagent)) };
                format!("Claude Code (outside Ligus) asks to run {} in {}{via}.", short(&p.tool), short(&p.label))
            },
            options: ligus_proto::ask_text::GRANT_LABELS.iter().map(|s| s.to_string()).collect(),
            expires_at_ms: now + timeout.as_millis() as i64,
            at_ms: now,
            v: ligus_core::flow::ASK_V2,
            audience: Some(ligus_core::flow::AskAudience::people(vec![me])),
            effect: Some(ligus_core::flow::AskEffect {
                kind: ligus_core::flow::EFFECT_PERMISSION.into(),
                tool: p.tool.clone(),
                detail: p.detail.clone(),
                cwd: p.cwd.clone(),
                host_device_id: Some(self.device_id),
                presence: ligus_core::flow::PRESENCE_REQUIRED.into(),
                too_large: p.too_large,
                ..Default::default()
            }),
            origin: Some(ligus_core::flow::AskOrigin {
                via: ligus_core::flow::ORIGIN_OUTSIDE.into(),
                label: p.label.clone(),
                runtime: "claude-code".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let prev = self.acting_agent.take();
        let r = self.record_flow_signed(chat_id, workspace_id, FlowRecord::HumanAsk(ask.clone()));
        self.acting_agent = prev;
        let env = r?;
        let digest = fold::envelope_ask_digest(&env).unwrap_or_default();
        Ok((ask, digest))
    }

    /// Close an outside prompt this device posted, naming the answer it
    /// acted on when it did.
    pub fn close_outside_ask(&mut self, workspace_id: Uuid, chat_id: Uuid, ask: &HumanAsk, settled_by: Option<Uuid>) -> Result<()> {
        let close = HumanAsk {
            ask_id: ask.ask_id,
            turn_id: ask.turn_id,
            agent_key: ask.agent_key.clone(),
            closed: true,
            at_ms: ligus_core::flow::now_ms(),
            v: ask.v,
            settled_by,
            ..Default::default()
        };
        let prev = self.acting_agent.take();
        let r = self.record_flow_signed(chat_id, workspace_id, FlowRecord::HumanAsk(close));
        self.acting_agent = prev;
        r.map(|_| ())
    }

    /// The other members of `workspace_id`'s chat `chat_id` who can read an
    /// outside prompt posted there (everyone but this device's person).
    pub fn others_in_chat(&self, workspace_id: Uuid, chat_id: Uuid) -> Result<usize> {
        let me = self.author.id.clone();
        Ok(self.load_in(workspace_id, chat_id)?.map(|s| s.members.iter().filter(|m| m.id != me).count()).unwrap_or(0))
    }
}
