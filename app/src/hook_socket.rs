//! The desktop's side of `ligus hook permission` (`ligus_runtime::hook`):
//! an owner-only socket in this app's data dir where Claude Code's
//! permission prompts, from sessions Ligus didn't start, arrive and become
//! grant cards put to this device's person (their passkey always needed).

use ligus_core::flow::HumanAsk;
use ligus_runtime::ligus_mcp::LigusHost;
use ligus_runtime::ChatService;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

use crate::AppState;

struct HookHost {
    app: AppHandle,
}

impl LigusHost for HookHost {
    fn with_service(&self, f: &mut dyn FnMut(&mut ChatService)) {
        let state = self.app.state::<AppState>();
        let mut svc = state.service.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut svc);
    }

    fn ask_posted(&self, chat_id: Uuid, ask: &HumanAsk) {
        let _ = self.app.emit("workspace://synced", 1);
        crate::notices::ask_posted(&self.app, &self.app.state::<AppState>(), chat_id, ask.turn_id, ask.ask_id);
    }
}

/// Listen for hook prompts for as long as the app runs.
pub(crate) fn start(app: &AppHandle) {
    let data_dir = app.state::<AppState>().data_dir.clone();
    let host = std::sync::Arc::new(HookHost { app: app.clone() });
    tauri::async_runtime::spawn(async move {
        match ligus_runtime::hook::listen(&data_dir, ligus_runtime::hook::Answerer::Host(host)) {
            Ok(server) => {
                tracing::info!(target: "hook", path = %server.path.display(), "listening for Claude Code permission prompts");
                // Held for the life of the app.
                std::future::pending::<()>().await;
                drop(server);
            }
            Err(e) => tracing::warn!(target: "hook", "hook socket: {e}"),
        }
    });
}
