//! The desktop as the turn engine's host (`ligus_runtime::turn_engine`): its
//! MCP catalogs and trust, the repo gate, worktree reuse, the flow and
//! proposal-set follow-ups, guard telemetry, and the Tauri events the window
//! renders. The turn itself runs in the engine, as on every host.

use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ligus_core::sandbox::EgressPolicy;
use ligus_core::{ActionProposal, ChatSession, ModelProviderKind};
use ligus_proto::{ChatActivityEvent, ChatStreamEvent};
use ligus_runtime::git_worktree::Worktree;
use ligus_runtime::ligus_mcp::LigusMcp;
use ligus_runtime::provider::dispatch::StreamActivity;
use ligus_runtime::turn_engine::{Checkout, FinalizeArgs, Phase, TurnHost};
use ligus_runtime::{ChatService, McpServerSpec, McpTool};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::AppState;

pub(crate) struct DesktopHost<'a> {
    pub app: &'a AppHandle,
    pub state: &'a AppState,
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub provider: ModelProviderKind,
    /// The turn's own MCP catalog (its chat's folder, review F14).
    pub mcp: Vec<McpServerSpec>,
    /// Where the turn runs: its chat's directory and checkout, after the
    /// repo gate (`crate::turn_scope`, run before the engine).
    pub checkout: Checkout,
}

impl DesktopHost<'_> {
    fn stream_event(&self, message_id: Uuid, phase: &str, text: String) {
        let _ = self.app.emit(
            ChatStreamEvent::EVENT,
            ChatStreamEvent { session_id: self.session_id.to_string(), message_id: message_id.to_string(), phase: phase.into(), text },
        );
    }
}

impl TurnHost for DesktopHost<'_> {
    fn service(&self) -> &Mutex<ChatService> {
        &self.state.service
    }

    fn mcp_servers(&self, egress: &EgressPolicy, _shell: bool) -> impl Future<Output = Vec<McpServerSpec>> + Send {
        let egress = egress.clone();
        async move {
            // Renew any near-expiry remote-server tokens first. The turn's
            // sandbox envelope decides which servers it may reach at all.
            self.state.ensure_fresh_mcp_tokens().await;
            // The turn's own catalog, never the viewed one's; tokens refreshed
            // above are re-read from the settings.
            let mut servers = self.mcp.clone();
            crate::apply_mcp_auth(&mut servers, &self.state.settings.lock().unwrap());
            crate::guards::filter_mcp_servers(self.state, servers, &egress)
        }
    }

    fn mcp_listed(&self, tagged: &[(String, McpTool)]) {
        crate::guards::record_mcp_tools(self.state, tagged);
    }

    fn ligus(&self) -> Option<Arc<LigusMcp>> {
        Some(crate::agent_iface::server(self.app))
    }

    fn directive_guidance(&self) -> Vec<&'static str> {
        crate::flow::directive_guidance().to_vec()
    }

    fn checkout(&self, _session: &ChatSession, _message_id: Uuid) -> impl Future<Output = Result<Checkout, String>> + Send {
        // Resolved with the host-side repo gate (repos-and-checkouts 5.3,
        // review F8) before the engine ran (`crate::turn_scope`): a repo chat
        // runs only in this device's checkout of it, in its own chat
        // worktree, never the active chat's folder (review F14).
        let checkout = self.checkout.clone();
        async move { Ok(checkout) }
    }

    fn cut_worktree(&self, repo_root: &Path, chat_id: &str, turn_id: &str) -> std::io::Result<Worktree> {
        crate::flow::cut_turn_worktree(self.state, repo_root, chat_id, turn_id)
    }

    fn reuse_worktrees(&self) -> bool {
        crate::flow::settings(self.state).reuse_worktrees
    }

    fn release_worktree(&self, wt: Worktree, chat_id: Uuid) {
        crate::flow::release_worktree(self.state, wt, chat_id);
    }

    fn workflow_tool(&self, session: &ChatSession) -> Option<ligus_runtime::workflow_tool::WorkflowTool> {
        let session = session.clone();
        Some(ligus_runtime::workflow_tool::WorkflowTool::new(Box::new(move |json: &str| {
            crate::workflows::definition_from_directive(json, &session).map(|d| d.name)
        })))
    }

    fn finalize(&self, svc: &mut ChatService, a: FinalizeArgs<'_>) -> Result<String, String> {
        // The turn's own chat directory (review F14), not the window's.
        let root = self.checkout.workspace_root.clone().unwrap_or_default();
        let Some(mid) = a.ctx.message_id else { return Err("no reply to finalize".into()) };
        crate::finalize_reply_with(svc, a.ctx.session_id, a.ctx.workspace_id, mid, &a.ctx.author, a.ctx.agent_id, &root, a.full, a.edits_in_review, a.mcp)
    }

    fn before_file_proposal(&self, proposal: &mut ActionProposal) {
        crate::flow::before_file_proposal(self.state, self.session_id, self.workspace_id, self.agent_id, proposal);
    }

    fn proposal_filed(&self, svc: &mut ChatService, proposal: Uuid) {
        crate::proposal_sets::group_same_trigger(svc, self.session_id, self.workspace_id, proposal);
        // Nudge the Review pane to refetch so the change surfaces.
        let _ = self.app.emit("workspace://synced", 1);
    }

    fn delta(&self, message_id: Uuid, text: &str) {
        self.stream_event(message_id, "delta", text.to_string());
    }

    // Live "background processing": the tool calls the agent makes, their
    // results, and thinking. Ephemeral (never persisted to the log).
    fn activity(&self, message_id: Uuid, act: &StreamActivity) {
        crate::flow::observe_activity(message_id, act);
        let base = ChatActivityEvent {
            session_id: self.session_id.to_string(),
            message_id: message_id.to_string(),
            kind: String::new(),
            id: String::new(),
            name: String::new(),
            input_json: String::new(),
            content: String::new(),
            is_error: false,
        };
        let ev = match act.clone() {
            StreamActivity::Tool { id, name, input_json } => ChatActivityEvent { kind: "tool".into(), id, name, input_json, ..base },
            StreamActivity::ToolResult { call_id, is_error, content } => ChatActivityEvent { kind: "result".into(), id: call_id, content, is_error, ..base },
            StreamActivity::Thinking { text } => ChatActivityEvent { kind: "thinking".into(), content: text, ..base },
            // Plan-window telemetry: feeds the staggered throttles, not the UI.
            StreamActivity::RateLimit { window } => {
                crate::guards::record_plan_window(self.state, self.provider, window);
                return;
            }
        };
        let _ = self.app.emit(ChatActivityEvent::EVENT, ev);
    }

    fn phase(&self, message_id: Uuid, phase: Phase, text: &str) {
        let p = match phase {
            Phase::Completed => "completed",
            Phase::Error => "error",
        };
        self.stream_event(message_id, p, text.to_string());
    }

    fn locked_down(&self) {
        let _ = self.app.emit("workspace://synced", 1);
    }

    fn tools_selected(&self, per_server: &[(String, u32, u32)]) {
        crate::guards::record_tools_selected(self.state, self.workspace_id, self.session_id, per_server);
    }

    fn progress(&self, progress: &ligus_runtime::patch_leaving::ProgressTracker) {
        crate::guards::record_progress(self.state, self.workspace_id, self.session_id, progress);
    }

    fn egress_denials(&self, denied: &[ligus_runtime::egress::HostPort]) {
        crate::guards::record_sandbox_egress_denials(self.state, self.workspace_id, self.session_id, denied);
    }

    fn agent_used(&self, agent: &str, use_: &ligus_runtime::agent_access::AgentUse) {
        // "Notify me when someone else uses this agent": this device runs
        // its owner's agent for another member.
        if !use_.notify {
            return;
        }
        let who = {
            let svc = self.state.service.lock().unwrap_or_else(|e| e.into_inner());
            let Some(s) = svc.load(self.session_id).ok().flatten() else { return };
            // Only on the owner's own device (an agent sent to another
            // member's machine as its host doesn't notify that member).
            let links = svc.device_links(self.workspace_id).unwrap_or_default();
            let mine = s
                .workspace_agents
                .iter()
                .find(|a| Some(a.id) == self.agent_id)
                .is_some_and(|a| links.person(&a.owner_actor_id) == links.person(&svc.author().id));
            if !mine {
                return;
            }
            s.members.into_iter().find(|m| links.person(&m.id) == use_.requester).map(|m| m.actor.display_name).unwrap_or_else(|| "Someone".into())
        };
        use tauri_plugin_notification::NotificationExt;
        let _ = self
            .app
            .notification()
            .builder()
            .title("Ligus")
            .body(format!("{} is using {}", ligus_runtime::display::shown(&who), ligus_runtime::display::shown(agent)))
            .show();
    }

    fn usage_error(&self, provider: ModelProviderKind, message: &str) {
        crate::guards::record_usage_error(self.state, provider, message);
    }
}
