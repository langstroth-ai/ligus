//! The one-step agent end to end (workspace model v3, 17.4 and H4): on a
//! real rules-3 workspace on the in-process test relay, Alex's desk adds
//! @coder on a model gateway his laptop offers, placed on the laptop, in one
//! run of `ligus setup agent`. It writes one workspace runtime (whose client
//! follows the gateway's wire) and one agent record carrying its host, asks
//! for the passkey twice (Confirm 1 of 2), and the laptop's own fold accepts
//! both. A second agent on the same model reuses the runtime: one event, one
//! passkey.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ligus_core::agent_run::Placement;
use ligus_core::{workspace_config_session_id, HostKind, HostModelGateway, HostOffer, ModelProviderKind, OfferKind, SessionEvent, WorkspaceHost};
use ligus_runtime::chat_service::{PresenceProvider, PresenceRequest};
use ligus_runtime::setup::{self, FlowId, Inputs, RunOpts, Secrets};

use crate::approvals_cmd_tests::world;
use crate::handoff_tests::linked_laptop;
use crate::setup_cmd::CliOps;
use crate::{open_service, sync_once};

/// The test recovery key as Alex's passkey, counting the ceremonies.
struct Counting(ligus_core::presence::RecoveryKey, Arc<AtomicUsize>);

impl PresenceProvider for Counting {
    fn assert(&self, req: &PresenceRequest) -> Result<ligus_core::presence::PresenceAssertion, String> {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(self.0.assert(&req.challenge))
    }
}

fn config_events(cfg: &crate::Config, ws: uuid::Uuid) -> Vec<SessionEvent> {
    open_service(cfg).unwrap().store().list(workspace_config_session_id(ws)).unwrap().into_iter().map(|e| e.payload).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn coder_on_a_gateway_model_placed_on_a_host_in_one_step() {
    let w = world().await;
    let lap = linked_laptop(&w).await;
    let ws = w.ws;

    // The laptop lists a gateway (its own record, signed by its own device).
    let lap_id = {
        let mut svc = open_service(&lap).unwrap();
        let id = svc.device_id().to_string();
        let mut h = WorkspaceHost::new(id.clone(), HostKind::Device, "laptop");
        h.model_gateways = vec![HostModelGateway { name: "gw-home".into(), api: "openai".into() }];
        h.offers = Some(vec![HostOffer { name: "gw-home".into(), kind: OfferKind::Gateway, harness: String::new(), wire: "openai".into() }]);
        svc.register_host(ws, h).unwrap();
        id
    };
    sync_once(&lap).await.unwrap();
    sync_once(&w.desk).await.unwrap();

    let acct = open_service(&w.desk).unwrap().author().account_id.unwrap();
    let ceremonies = Arc::new(AtomicUsize::new(0));
    let run = |name: &str, ceremonies: &Arc<AtomicUsize>| {
        let mut ops = CliOps::open(&w.desk).unwrap();
        ops.set_presence(Arc::new(Counting(ligus_core::testkit::test_recovery_key(acct), ceremonies.clone())));
        let i = Inputs::new().with("name", name).with("source", "gateway:gw-home").with("model", "auto");
        // The form says where it runs and what it takes before anything runs.
        let form = setup::form(FlowId::Agent, &ops, &i);
        let runs_on = form.fields().find(|f| f.id == "runs-on").unwrap();
        assert_eq!(runs_on.default.as_deref(), Some(lap_id.as_str()), "the only host that offers it");
        assert_eq!(runs_on.note.as_deref(), Some(format!("Turns run on laptop. When it's offline, @{name} waits.").as_str()));
        let expected = form.confirmations;
        let out = setup::run(FlowId::Agent, &mut ops, &i, &Secrets::new(), &RunOpts::default());
        assert_eq!(out.exit, 0, "{}", out.to_json(false));
        expected
    };

    let before = config_events(&w.desk, ws).len();
    assert_eq!(run("coder", &ceremonies), 2, "the form said Confirm 1 of 2");
    let new: Vec<SessionEvent> = config_events(&w.desk, ws).split_off(before);
    assert_eq!(new.len(), 2, "one runtime, one agent: {new:?}");
    let SessionEvent::WorkspaceRuntimeUpserted { runtime } = &new[0] else { panic!("{:?}", new[0]) };
    assert_eq!((runtime.gateway.as_str(), runtime.model.as_str(), runtime.provider), ("gw-home", "auto", ModelProviderKind::Custom));
    assert!(runtime.endpoint.is_empty(), "no address on the log");
    let SessionEvent::AgentUpserted { agent } = &new[1] else { panic!("{:?}", new[1]) };
    assert_eq!((agent.name.as_str(), agent.runtime_id.as_str(), agent.host_id.as_str()), ("coder", runtime.id.as_str(), lap_id.as_str()));
    assert_eq!(agent.placement, Some(Placement::Host { host_id: lap_id.clone() }));
    assert_eq!(ceremonies.load(Ordering::SeqCst), 2, "two passkey confirmations, one per event");

    // The laptop's own fold accepts both.
    sync_once(&w.desk).await.unwrap();
    sync_once(&lap).await.unwrap();
    let svc = open_service(&lap).unwrap();
    let cfg = svc.workspace_config(ws).unwrap().unwrap();
    let a = cfg.agents().iter().find(|a| a.name == "coder").expect("@coder on the laptop's fold");
    assert_eq!(a.host_id, lap_id);
    assert!(svc.list_workspace_runtimes(ws).unwrap().iter().any(|r| r.id == a.runtime_id));

    // A second agent on the same model: the runtime is reused.
    let c2 = Arc::new(AtomicUsize::new(0));
    let before = config_events(&w.desk, ws).len();
    assert_eq!(run("reviewer", &c2), 1);
    let new = config_events(&w.desk, ws).split_off(before);
    assert!(matches!(new.as_slice(), [SessionEvent::AgentUpserted { .. }]), "{new:?}");
    assert_eq!(c2.load(Ordering::SeqCst), 1);
}
