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

    /// The envelope that posted `ask_id` in a chat: the first in the log's
    /// canonical order (an ask id belongs to its first signer).
    pub fn ask_envelope(&self, workspace_id: Uuid, session_id: Uuid, ask_id: Uuid) -> Result<Option<SessionEventEnvelope>> {
        let envs = self.store.list_in(workspace_id, session_id)?;
        Ok(envs
            .into_iter()
            .filter(|e| matches!(&e.payload, SessionEvent::FlowRecorded { record: FlowRecord::HumanAsk(a) } if a.ask_id == ask_id))
            .min_by_key(|e| e.canonical_key()))
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
