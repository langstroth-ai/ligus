//! Change model… on the CLI's engine ops and `ligus workspace rules`, end
//! to end on the in-process test relay with real folded workspaces
//! (workspace model v3, 5.7, 15.2 and 17.4).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ligus_core::agent_run::{legacy_id, AgentRun};
use ligus_core::{workspace_config_session_id, SessionEvent, WorkspaceAgent};
use ligus_runtime::chat_service::{PresenceProvider, PresenceRequest};
use ligus_runtime::setup::{self, FlowId, Inputs, RunOpts, Secrets};

use crate::approvals_cmd_tests::world;
use crate::handoff_tests::linked_laptop;
use crate::setup_cmd::CliOps;
use crate::{open_service, sync_once};

struct Counting(ligus_core::presence::RecoveryKey, Arc<AtomicUsize>);

impl PresenceProvider for Counting {
    fn assert(&self, req: &PresenceRequest) -> Result<ligus_core::presence::PresenceAssertion, String> {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(self.0.assert(&req.challenge))
    }
}

fn upserts(cfg: &crate::Config, ws: uuid::Uuid) -> usize {
    let svc = open_service(cfg).unwrap();
    svc.store().list(workspace_config_session_id(ws)).unwrap().iter().filter(|e| matches!(e.payload, SessionEvent::AgentUpserted { .. })).count()
}

/// On rules 4 (a workspace this build founds), `ligus agents set scout
/// --model sonnet` is one event and no passkey, and Alex's laptop folds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn change_model_on_rules4_is_one_event_without_a_passkey() {
    let w = world().await;
    let lap = linked_laptop(&w).await;
    let ws = w.ws;
    let acct = {
        let mut svc = open_service(&w.desk).unwrap();
        assert_eq!(svc.workspace_config(ws).unwrap().unwrap().session().authz.rules, ligus_core::authz::RULES_V4);
        let run = AgentRun { harness: "claudeCode".into(), source: "claude-code".into(), model: "opus".into(), resolved: None };
        let mut a = WorkspaceAgent::new("scout", legacy_id(&run));
        a.owner_actor_id = svc.author().id.clone();
        a.run = Some(run);
        svc.add_agent(workspace_config_session_id(ws), ws, a).unwrap();
        svc.author().account_id.unwrap()
    };
    let asked = Arc::new(AtomicUsize::new(0));
    let mut ops = CliOps::open(&w.desk).unwrap();
    ops.set_presence(Arc::new(Counting(ligus_core::testkit::test_recovery_key(acct), asked.clone())));
    let i = Inputs::new().with("agent", "scout").with("model", "sonnet");
    let form = setup::form(FlowId::Agent, &ops, &i);
    assert_eq!((form.action.as_deref(), form.confirmations), (Some("Change model"), 0));
    let before = upserts(&w.desk, ws);
    let out = setup::run(FlowId::Agent, &mut ops, &i, &Secrets::new(), &RunOpts::default());
    assert_eq!(out.exit, 0, "{}", out.to_json(false));
    assert_eq!((upserts(&w.desk, ws) - before, asked.load(Ordering::SeqCst)), (1, 0), "one event, no passkey");
    drop(ops);
    sync_once(&w.desk).await.unwrap();
    sync_once(&lap).await.unwrap();
    let svc = open_service(&lap).unwrap();
    let a = svc.workspace_config(ws).unwrap().unwrap().agents().iter().find(|a| a.name == "scout").cloned().expect("on the laptop's fold");
    assert_eq!(a.run.map(|r| r.model), Some("sonnet".to_string()));

    // `ligus workspace rules` on rules 4: nothing to upgrade (it reads the
    // beats with a runtime of its own, so off this one).
    let desk = w.desk.clone();
    tokio::task::spawn_blocking(move || {
        crate::rules_cmd::cmd(&desk, &["rules".into()]).unwrap();
        crate::rules_cmd::cmd(&desk, &["rules".into(), "upgrade".into(), "--dry-run".into()]).unwrap();
        let svc = open_service(&desk).unwrap();
        assert_eq!(svc.rules4_report(ws).unwrap().rules, ligus_core::authz::RULES_V4);
    })
    .await
    .unwrap();
}
