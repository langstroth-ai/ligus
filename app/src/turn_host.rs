//! The desktop as the turn engine's host (`hive_runtime::turn_engine`): its
//! MCP catalogs and trust, the repo gate, worktree reuse, the flow and
//! proposal-set follow-ups, guard telemetry, and the Tauri events the window
//! renders. The turn itself runs in the engine, as on every host.

use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};

use hive_core::sandbox::EgressPolicy;
use hive_core::{ActionProposal, ChatSession, ModelProviderKind};
use hive_proto::{ChatActivityEvent, ChatStreamEvent};
use hive_runtime::git_worktree::Worktree;
use hive_runtime::ligus_mcp::LigusMcp;
use hive_runtime::provider::dispatch::StreamActivity;
use hive_runtime::turn_engine::{Checkout, FinalizeArgs, Phase, TurnHost};
use hive_runtime::{ChatService, McpServerSpec, McpTool};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::AppState;

pub(crate) struct DesktopHost<'a> {
    pub app: &'a AppHandle,
    pub state: &'a AppState,
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Option<Uuid>,
    /// The responder's name (the repo gate's refusal names it).
    pub author: String,
    pub provider: ModelProviderKind,
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
            crate::guards::filter_mcp_servers(self.state, self.state.combined_mcp_servers(), &egress)
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

    fn checkout(&self, session: &ChatSession, _message_id: Uuid) -> impl Future<Output = Result<Checkout, String>> + Send {
        // The host-side repo gate (repos-and-checkouts 5.3, review F8): a chat
        // with a repo runs only in this device's checkout of it, on its chat
        // branch brought up to date; never in other code.
        let gate = crate::repos_ui::turn_repo(self.state, session);
        let result = match gate {
            Err(reason) => Err(format!("{} did not run: {reason}", self.author)),
            // A repo chat reads and runs in its own chat worktree, never the
            // active chat's folder (review F14).
            Ok(Some(r)) => Ok(Checkout {
                context: r.context,
                workspace_root: Some(r.chat_worktree.to_string_lossy().into_owned()),
                repo_root: Some(r.checkout),
                base: Some((r.tip, r.base_branch)),
            }),
            Ok(None) => {
                let root = self.state.workspace_root.lock().unwrap().clone();
                // Worktrees are cut from the workspace's own checkout, not
                // from the root, which follows the active chat's worktree.
                let repo_root = crate::turn_repo_root(&self.state.workspace_repo_root(), &root);
                Ok(Checkout { context: String::new(), workspace_root: Some(root), repo_root: Some(repo_root.into()), base: None })
            }
        };
        async move { result }
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

    fn workflow_tool(&self, session: &ChatSession) -> Option<hive_runtime::workflow_tool::WorkflowTool> {
        let session = session.clone();
        Some(hive_runtime::workflow_tool::WorkflowTool::new(Box::new(move |json: &str| {
            crate::workflows::definition_from_directive(json, &session).map(|d| d.name)
        })))
    }

    fn finalize(&self, svc: &mut ChatService, a: FinalizeArgs<'_>) -> Result<String, String> {
        let root = self.state.workspace_root.lock().unwrap().clone();
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

    fn progress(&self, progress: &hive_runtime::patch_leaving::ProgressTracker) {
        crate::guards::record_progress(self.state, self.workspace_id, self.session_id, progress);
    }

    fn egress_denials(&self, denied: &[hive_runtime::egress::HostPort]) {
        crate::guards::record_sandbox_egress_denials(self.state, self.workspace_id, self.session_id, denied);
    }

    fn usage_error(&self, provider: ModelProviderKind, message: &str) {
        crate::guards::record_usage_error(self.state, provider, message);
    }
}
