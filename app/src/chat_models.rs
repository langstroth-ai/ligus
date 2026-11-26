//! The model behind a chat, picked while chatting (the route pill's second
//! step) — `docs/design/chat-model-picker.md`.
//!
//! - The choice is per chat and per runtime, kept in this device's
//!   `settings.json` (`chat_models`), never synced. It applies only to the
//!   chat's untagged (`@ligus`) turns that this device builds
//!   (`responder_for`), so it can't change another member's agent or spend
//!   their quota. `@agents` keep their own runtime and model.
//! - The lists come from `ligus_runtime::model_catalog` (read-only, no
//!   quota), cached here for [`LIST_TTL`].
//! - A turn that fails because the model isn't available marks that runtime
//!   and model for this session and tells the chat (`chat://model-unavailable`),
//!   which shows one line with "Pick another model" instead of a failed reply;
//!   the next send is checked first ([`chat_model_problem`]).

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ligus_core::ModelProviderKind;
use ligus_runtime::model_catalog::{self, ModelList, ModelOption, ModelSource};
use ligus_runtime::provider::dispatch::ResolvedRuntime;
use tauri::{Emitter, State};
use uuid::Uuid;

use crate::AppState;

/// How long a model list is reused before the harness is asked again.
pub(crate) const LIST_TTL: Duration = Duration::from_secs(5 * 60);

/// The event a chat listens for to show the one-line "isn't available" note.
pub(crate) const MODEL_UNAVAILABLE_EVENT: &str = "chat://model-unavailable";

fn lists() -> &'static Mutex<HashMap<String, (Instant, ModelList)>> {
    static L: OnceLock<Mutex<HashMap<String, (Instant, ModelList)>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// (runtime id, model, lowercased) this device saw fail as unavailable.
fn unavailable() -> &'static Mutex<HashSet<(String, String)>> {
    static U: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    U.get_or_init(Default::default)
}

fn unavailable_key(runtime_id: &str, model: &str) -> (String, String) {
    (runtime_id.to_string(), model.trim().to_lowercase())
}

pub(crate) fn mark_unavailable(runtime_id: &str, model: &str) {
    if !model.trim().is_empty() {
        unavailable()
            .lock()
            .unwrap()
            .insert(unavailable_key(runtime_id, model));
    }
}

fn clear_unavailable(runtime_id: &str, model: &str) {
    unavailable()
        .lock()
        .unwrap()
        .remove(&unavailable_key(runtime_id, model));
}

pub(crate) fn is_marked_unavailable(runtime_id: &str, model: &str) -> bool {
    unavailable()
        .lock()
        .unwrap()
        .contains(&unavailable_key(runtime_id, model))
}

/// The chat's model choice on this device for `runtime_id`.
pub(crate) fn override_for(state: &AppState, session_id: Uuid, runtime_id: &str) -> Option<String> {
    stored(
        &state.settings.lock().unwrap().chat_models,
        session_id,
        runtime_id,
    )
}

fn stored(
    map: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    session_id: Uuid,
    runtime_id: &str,
) -> Option<String> {
    map.get(&session_id.to_string())
        .and_then(|m| m.get(runtime_id))
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
}

/// Store (or with `None`/empty, clear) the chat's choice for a runtime.
/// Returns whether anything changed.
fn store(
    map: &mut std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    session_id: Uuid,
    runtime_id: &str,
    model: Option<&str>,
) -> bool {
    let key = session_id.to_string();
    match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => {
            let entry = map.entry(key).or_default();
            entry.insert(runtime_id.to_string(), m.to_string()) != Some(m.to_string())
        }
        None => {
            let Some(entry) = map.get_mut(&key) else {
                return false;
            };
            let changed = entry.remove(runtime_id).is_some();
            if entry.is_empty() {
                map.remove(&key);
            }
            changed
        }
    }
}

/// Run `model` on this turn's runtime.
pub(crate) fn apply_model(rt: &mut ResolvedRuntime, model: &str) {
    rt.set_model(model);
}

/// A deleted chat's choices go with it.
pub(crate) fn forget_chat(state: &AppState, session_id: Uuid) {
    let mut s = state.settings.lock().unwrap();
    if s.chat_models.remove(&session_id.to_string()).is_some() {
        crate::save_settings(&state.data_dir, &s);
    }
}

/// A failed turn: when the model wasn't available, remember it and tell an
/// open chat (untagged turns only; an agent's model is its owner's to change).
pub(crate) fn note_turn_error(
    app: &tauri::AppHandle,
    session_id: Uuid,
    responder: &crate::Responder,
    error: &str,
) {
    if !model_catalog::is_model_unavailable(error) {
        return;
    }
    let model = responder.runtime.effective_model();
    mark_unavailable(&responder.runtime_id, &model);
    if responder.agent_id.is_none() {
        let _ = app.emit(
            MODEL_UNAVAILABLE_EVENT,
            serde_json::json!({
                "sessionId": session_id.to_string(),
                "runtimeId": responder.runtime_id,
                "model": model,
                "message": error.chars().take(300).collect::<String>(),
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Listing.
// ---------------------------------------------------------------------------

/// A runtime's models as the picker shows them.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeModelsDto {
    models: Vec<ModelOption>,
    /// `live`, `catalog`, `aliases` or `none`.
    source: ModelSource,
    /// A model not on a complete list won't run; only those are checked
    /// before sending.
    complete: bool,
    note: Option<String>,
    error: Option<String>,
    /// Models on (or chosen for) this runtime that failed as unavailable
    /// on this device this session.
    unavailable: Vec<String>,
}

/// What to list: a runtime by id, or (Settings' add-runtime form, before a
/// runtime exists) a provider kind and endpoint.
enum Target {
    Runtime(ResolvedRuntime),
    ClaudeCode,
}

fn cli_program(kind: ModelProviderKind) -> &'static str {
    match kind {
        ModelProviderKind::Antigravity => "agy",
        ModelProviderKind::Copilot => "copilot",
        ModelProviderKind::Codex => "codex",
        ModelProviderKind::ClaudeCode => "claude",
        _ => "",
    }
}

fn form_target(state: &AppState, provider: &str, endpoint: &str) -> Result<Target, String> {
    let kind = crate::parse_provider_kind(provider)?;
    if kind == ModelProviderKind::ClaudeCode {
        return Ok(Target::ClaudeCode);
    }
    let (key, base) = {
        let s = state.settings.lock().unwrap();
        let name = crate::provider_config_name(kind);
        (
            form_key(
                kind,
                s.provider_keys.get(name).map(String::as_str),
                s.api_key.as_deref(),
            ),
            s.provider_base_urls.get(name).cloned().unwrap_or_default(),
        )
    };
    let endpoint = endpoint.trim();
    let endpoint = if kind.is_cli_agent() {
        if endpoint.is_empty() {
            cli_program(kind).to_string()
        } else {
            endpoint.to_string()
        }
    } else if kind == ModelProviderKind::Anthropic {
        String::new()
    } else {
        let base = if endpoint.is_empty() {
            base
        } else {
            endpoint.to_string()
        };
        let base = if base.trim().is_empty() {
            ligus_runtime::provider::dispatch::default_endpoint(kind).to_string()
        } else {
            base
        };
        if base.contains("/chat/completions") || kind == ModelProviderKind::Ollama {
            base
        } else {
            format!("{}/v1/chat/completions", base.trim_end_matches('/'))
        }
    };
    let mut rt = state.resolve_runtime("");
    rt.provider = kind;
    rt.endpoint = endpoint;
    rt.api_key = if kind.is_cli_agent() { None } else { key };
    rt.gateway = None;
    Ok(Target::Runtime(rt))
}

/// The key a form listing sends: the provider's own stored key, else the
/// legacy global key, except for `custom`, whose endpoint is whatever is
/// being typed (the legacy key belongs to some other provider and is never
/// sent there).
fn form_key(
    kind: ModelProviderKind,
    provider_key: Option<&str>,
    legacy: Option<&str>,
) -> Option<String> {
    let own = provider_key.map(str::trim).filter(|k| !k.is_empty());
    let fallback = (kind != ModelProviderKind::Custom)
        .then_some(legacy)
        .flatten()
        .map(str::trim)
        .filter(|k| !k.is_empty());
    own.or(fallback).map(str::to_string)
}

fn runtime_target(state: &AppState, runtime_id: &str) -> Target {
    let known = state.combined_runtimes().iter().any(|r| r.id == runtime_id);
    if runtime_id == "claude-code" && !known {
        return Target::ClaudeCode;
    }
    let rt = state.resolve_runtime(runtime_id);
    if rt.provider == ModelProviderKind::ClaudeCode {
        Target::ClaudeCode
    } else {
        Target::Runtime(rt)
    }
}

fn claude_code_list() -> ModelList {
    let models = crate::claude_code_model_options()
        .into_iter()
        .filter(|(value, _)| model_catalog::is_model_id(value) && value != "default")
        .map(|(value, label)| ModelOption::new(value, Some(label)))
        .collect();
    ModelList {
        models,
        source: ModelSource::Aliases,
        complete: false,
        note: Some("Claude Code's aliases and the models it lists for your account.".into()),
        error: None,
    }
}

async fn fetch(target: Target) -> ModelList {
    match target {
        Target::ClaudeCode => claude_code_list(),
        Target::Runtime(rt) if rt.gateway.is_some() => {
            ModelList::none("This runtime goes through a model gateway; type a model name.")
        }
        Target::Runtime(rt) if rt.provider.is_cli_agent() => {
            let (kind, program) = (rt.provider, rt.endpoint.clone());
            let program = if program.trim().is_empty() {
                cli_program(kind).to_string()
            } else {
                program
            };
            tokio::task::spawn_blocking(move || {
                model_catalog::cli_models(
                    kind,
                    &program,
                    &ligus_runtime::cli_detect::DetectEnv::from_process(),
                )
            })
            .await
            .unwrap_or_else(|e| ModelList::failed(ModelSource::None, e.to_string()))
        }
        Target::Runtime(rt) => {
            model_catalog::http_models(model_catalog::HttpListing {
                provider: rt.provider,
                endpoint: rt.endpoint,
                api_key: rt.api_key,
            })
            .await
        }
    }
}

/// A cached complete list for `runtime_id`, if one is fresh.
fn cached(key: &str) -> Option<ModelList> {
    let l = lists().lock().unwrap();
    l.get(key)
        .filter(|(at, _)| at.elapsed() < LIST_TTL)
        .map(|(_, list)| list.clone())
}

/// The models a runtime offers (read-only, no quota; cached for five
/// minutes, `refresh` asks again). `runtime_id` names a runtime; without
/// one, `provider` and `endpoint` describe the one being added in Settings.
#[tauri::command]
pub(crate) async fn list_runtime_models(
    state: State<'_, AppState>,
    runtime_id: Option<String>,
    provider: Option<String>,
    endpoint: Option<String>,
    refresh: Option<bool>,
) -> Result<RuntimeModelsDto, String> {
    let runtime_id = runtime_id
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());
    let (key, target) = match &runtime_id {
        Some(id) => (id.clone(), runtime_target(&state, id)),
        None => {
            let p = provider.unwrap_or_default();
            let e = endpoint.unwrap_or_default();
            (format!("form|{p}|{e}"), form_target(&state, &p, &e)?)
        }
    };
    let list = match cached(&key).filter(|_| refresh != Some(true)) {
        Some(l) => l,
        None => {
            let l = fetch(target).await;
            // Failures aren't cached: the next open asks again.
            if l.error.is_none() {
                lists()
                    .lock()
                    .unwrap()
                    .insert(key.clone(), (Instant::now(), l.clone()));
            }
            l
        }
    };
    let rid = runtime_id.unwrap_or(key);
    let mut marked: Vec<String> = unavailable()
        .lock()
        .unwrap()
        .iter()
        .filter(|(r, _)| *r == rid)
        .map(|(_, m)| {
            list.models
                .iter()
                .find(|o| o.id.to_lowercase() == *m)
                .map(|o| o.id.clone())
                .unwrap_or_else(|| m.clone())
        })
        .collect();
    marked.sort();
    Ok(RuntimeModelsDto {
        models: list.models,
        source: list.source,
        complete: list.complete,
        note: list.note,
        error: list.error,
        unavailable: marked,
    })
}

// ---------------------------------------------------------------------------
// The per-chat choice.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatModelDto {
    /// The chat's runtime (synced, the route pill's choice).
    runtime_id: String,
    /// This device's model choice for that runtime; `None` = the runtime's
    /// own model.
    model: Option<String>,
    /// The runtime's own model ("" = the harness default).
    runtime_model: String,
    /// Every runtime's choice for this chat on this device (runtime id →
    /// model), so the picker shows each runtime's.
    choices: std::collections::BTreeMap<String, String>,
    /// Whether this device answers the chat's untagged messages (else they
    /// run on another member's device, with its own settings).
    applies_here: bool,
}

fn chat_model(state: &AppState, session_id: Uuid) -> Result<ChatModelDto, String> {
    let me = state.local_actor_id();
    let (session, links) = {
        let svc = state.service.lock().unwrap();
        let s = svc
            .load_resolving(session_id)
            .map_err(crate::map_err)?
            .ok_or("unknown chat")?;
        let links = crate::device_links_of(&svc, s.workspace_id);
        (s, links)
    };
    let local_only = state.is_local_only_ws(session.workspace_id);
    let applies_here = ligus_runtime::turn_dispatch::runs_here_linked(
        local_only,
        &links,
        &session,
        &me,
        &session.creator_actor_id,
        ligus_runtime::turn_dispatch::HostBinding::Unbound,
    );
    let model = override_for(state, session_id, &session.runtime_id);
    let choices = state
        .settings
        .lock()
        .unwrap()
        .chat_models
        .get(&session_id.to_string())
        .cloned()
        .unwrap_or_default();
    Ok(ChatModelDto {
        choices,
        runtime_model: state.resolve_runtime(&session.runtime_id).effective_model(),
        runtime_id: session.runtime_id,
        model,
        applies_here,
    })
}

/// The chat's model choice on this device.
#[tauri::command]
pub(crate) async fn get_chat_model(
    ligus_app: tauri::AppHandle,
    session_id: String,
) -> Result<ChatModelDto, String> {
    let id = Uuid::parse_str(&session_id).map_err(crate::map_err)?;
    crate::off_main(ligus_app, move |app| {
        chat_model(&tauri::Manager::state::<AppState>(app), id)
    })
    .await
}

/// Pick the model for a chat's runtime on this device (`None` or empty =
/// back to the runtime's own). Picking a model clears its "unavailable"
/// mark: the person is trying it again.
#[tauri::command]
pub(crate) async fn set_chat_model(
    ligus_app: tauri::AppHandle,
    session_id: String,
    runtime_id: String,
    model: Option<String>,
) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(crate::map_err)?;
    crate::off_main(ligus_app, move |app| {
        set_chat_model_sync(
            &tauri::Manager::state::<AppState>(app),
            id,
            &runtime_id,
            model.as_deref(),
        )
    })
    .await
}

/// A model name a harness can take as its `--model` argument: short, no
/// control characters, and not starting with `-` (which would read as a
/// flag).
fn check_model_name(m: &str) -> Result<(), String> {
    if m.len() > 200 || m.chars().any(char::is_control) || m.starts_with('-') {
        return Err("that isn't a model name".to_string());
    }
    Ok(())
}

pub(crate) fn set_chat_model_sync(
    state: &AppState,
    id: Uuid,
    runtime_id: &str,
    model: Option<&str>,
) -> Result<(), String> {
    let runtime_id = runtime_id.trim();
    if runtime_id.is_empty() {
        return Err("a runtime id is required".to_string());
    }
    if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
        check_model_name(m)?;
        clear_unavailable(runtime_id, m);
    }
    let mut s = state.settings.lock().unwrap();
    if store(&mut s.chat_models, id, runtime_id, model) {
        crate::save_settings(&state.data_dir, &s);
    }
    Ok(())
}

/// Why the chat's next untagged turn on this device would fail: its model
/// failed as unavailable here, or isn't on a complete list from the cache.
/// `None` = fine (or unknown, or the turn runs on another device).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelProblemDto {
    runtime_id: String,
    model: String,
    /// `unavailable` (it failed here) or `notListed` (not on the harness's
    /// complete list).
    reason: String,
}

pub(crate) fn problem_for(
    runtime_id: &str,
    model: &str,
    list: Option<&ModelList>,
) -> Option<ModelProblemDto> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    let reason = if is_marked_unavailable(runtime_id, model) {
        "unavailable"
    } else if list.is_some_and(|l| l.complete && !l.models.is_empty() && !l.contains(model)) {
        "notListed"
    } else {
        return None;
    };
    Some(ModelProblemDto {
        runtime_id: runtime_id.to_string(),
        model: model.to_string(),
        reason: reason.to_string(),
    })
}

#[tauri::command]
pub(crate) async fn chat_model_problem(
    ligus_app: tauri::AppHandle,
    session_id: String,
) -> Result<Option<ModelProblemDto>, String> {
    let id = Uuid::parse_str(&session_id).map_err(crate::map_err)?;
    crate::off_main(ligus_app, move |app| {
        let state = tauri::Manager::state::<AppState>(app);
        let c = chat_model(&state, id)?;
        if !c.applies_here {
            return Ok(None);
        }
        let model = c.model.clone().unwrap_or(c.runtime_model.clone());
        Ok(problem_for(
            &c.runtime_id,
            &model,
            cached(&c.runtime_id).as_ref(),
        ))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn choices_are_kept_per_chat_and_per_runtime() {
        let mut map = BTreeMap::new();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(store(&mut map, a, "copilot", Some(" gpt-5.3-codex ")));
        assert!(
            !store(&mut map, a, "copilot", Some("gpt-5.3-codex")),
            "same choice: nothing to save"
        );
        assert!(store(&mut map, a, "agy", Some("gemini-3.8-flash-low")));
        assert_eq!(stored(&map, a, "copilot").as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(
            stored(&map, a, "agy").as_deref(),
            Some("gemini-3.8-flash-low")
        );
        assert_eq!(
            stored(&map, b, "copilot"),
            None,
            "another chat keeps the runtime's model"
        );
        assert_eq!(
            stored(&map, a, "codex"),
            None,
            "a choice for one runtime never applies to another"
        );

        assert!(store(&mut map, a, "copilot", None));
        assert!(
            store(&mut map, a, "agy", Some("  ")),
            "empty = back to Default"
        );
        assert!(map.is_empty(), "a chat with no choices leaves no entry");
        assert!(!store(&mut map, b, "copilot", None));
    }

    #[test]
    fn a_model_starting_with_a_dash_is_refused() {
        for bad in ["--dangerously-skip-permissions", "-m", "-", "a\nb"] {
            assert!(check_model_name(bad).is_err(), "{bad}");
        }
        for ok in [
            "gpt-5.3-codex",
            "openai/gpt-6",
            "llama3.2:latest",
            "claude-opus-5-5[1m]",
        ] {
            assert!(check_model_name(ok).is_ok(), "{ok}");
        }
        // set_chat_model_sync trims first, so " -m" is caught as "-m".
        assert!(check_model_name(" -m".trim()).is_err());
    }

    #[test]
    fn a_custom_endpoint_never_gets_the_legacy_key() {
        assert_eq!(
            form_key(ModelProviderKind::Custom, None, Some("sk-legacy")),
            None
        );
        assert_eq!(
            form_key(ModelProviderKind::Custom, Some(""), Some("sk-legacy")),
            None
        );
        assert_eq!(
            form_key(
                ModelProviderKind::Custom,
                Some("sk-custom"),
                Some("sk-legacy")
            )
            .as_deref(),
            Some("sk-custom")
        );
        assert_eq!(
            form_key(ModelProviderKind::OpenAI, None, Some("sk-legacy")).as_deref(),
            Some("sk-legacy")
        );
        assert_eq!(
            form_key(
                ModelProviderKind::OpenAI,
                Some("sk-openai"),
                Some("sk-legacy")
            )
            .as_deref(),
            Some("sk-openai")
        );
    }

    #[test]
    fn a_problem_is_a_failed_model_or_one_missing_from_a_complete_list() {
        let complete = ModelList {
            models: vec![
                ModelOption::new("gemini-3.8-flash-low", None),
                ModelOption::new("llama3.2:latest", None),
            ],
            source: ModelSource::Live,
            complete: true,
            note: None,
            error: None,
        };
        assert_eq!(
            problem_for("agy-t", "gemini-3.8-flash-low", Some(&complete)),
            None
        );
        assert_eq!(
            problem_for("agy-t", "llama3.2", Some(&complete)),
            None,
            "Ollama's :latest is implied"
        );
        assert_eq!(
            problem_for("agy-t", "gemini-9", Some(&complete))
                .unwrap()
                .reason,
            "notListed"
        );

        // An incomplete list (Copilot's catalog) never blocks a send.
        let catalog = ModelList {
            complete: false,
            source: ModelSource::Catalog,
            ..complete.clone()
        };
        assert_eq!(
            problem_for("copilot-t", "claude-opus-4.8", Some(&catalog)),
            None
        );
        assert_eq!(problem_for("copilot-t", "claude-opus-4.8", None), None);

        // Once a turn here failed with it, it is caught before the next send,
        // until the person picks it again.
        mark_unavailable("copilot-t", "Claude-Opus-4.8");
        assert_eq!(
            problem_for("copilot-t", "claude-opus-4.8", Some(&catalog))
                .unwrap()
                .reason,
            "unavailable"
        );
        assert_eq!(
            problem_for("copilot-other", "claude-opus-4.8", None),
            None,
            "marked for that runtime only"
        );
        clear_unavailable("copilot-t", "claude-opus-4.8");
        assert_eq!(problem_for("copilot-t", "claude-opus-4.8", None), None);
        assert_eq!(
            problem_for("copilot-t", "", None),
            None,
            "the harness default isn't checked"
        );
    }
}
