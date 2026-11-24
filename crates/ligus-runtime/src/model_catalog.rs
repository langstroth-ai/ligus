//! The models a runtime's harness offers, for the desktop's model picker
//! (the route pill's second step and the Settings runtime editors).
//!
//! Every listing here is read-only and spends no quota: no prompt is sent.
//! - Antigravity: `agy models` (the signed-in account's models; complete).
//! - Copilot CLI: `copilot help config`, the `model` setting's choices (the
//!   installed CLI's catalog; a plan may not include every one of them).
//! - Codex: `codex debug models` (the CLI's catalog as JSON, `list` entries).
//! - Ollama: `GET /api/tags` (complete).
//! - OpenAI-compatible servers: `GET …/v1/models` (complete for a local or
//!   private server; a hosted API may list models the key can't use).
//! - Anthropic: `GET /v1/models`.
//! - OpenRouter: `GET /api/v1/models` (public).
//!
//! Claude Code's list (its aliases plus the CLI's cached options) is read by
//! the desktop from the CLI's own files; other harnesses (pi, aider, Hermes,
//! a named model gateway) have no listing and offer "Other…" only.
//!
//! [`is_model_unavailable`] recognizes a turn that failed because the model
//! isn't available to the account, so the desktop can say so in one line and
//! offer another model instead of showing a failed reply.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ligus_core::ModelProviderKind;
use serde::Serialize;
use serde_json::Value;

use crate::cli_detect::{find_on_path, run_bounded_split_in, DetectEnv};

/// How long a CLI listing may take (`agy models` asks the network).
pub const CLI_LIST_TIMEOUT: Duration = Duration::from_secs(15);
/// `agy models` fetches the account's models; give it longer.
pub const AGY_LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// How long an HTTP listing may take.
pub const HTTP_LIST_TIMEOUT: Duration = Duration::from_secs(8);

/// One model a harness offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    /// What the harness takes as its model (`--model`, the request's `model`).
    pub id: String,
    /// The harness's display name, when it gives one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ModelOption {
    pub fn new(id: impl Into<String>, label: Option<String>) -> Self {
        Self { id: id.into(), label: label.filter(|l| !l.trim().is_empty()) }
    }
}

/// Where a list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelSource {
    /// Asked the account or server just now.
    Live,
    /// The installed harness's own catalog (not specific to the account).
    Catalog,
    /// Aliases the harness always understands.
    Aliases,
    /// The harness has no listing.
    None,
}

/// A runtime's models.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelList {
    pub models: Vec<ModelOption>,
    pub source: ModelSource,
    /// The list is everything the account or server can run: a model not on
    /// it won't work. Only a complete list is checked before sending.
    pub complete: bool,
    /// A short note for the picker (e.g. "your plan may not include all").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Why listing failed (Default and Other… still work).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ModelList {
    pub fn none(note: impl Into<String>) -> Self {
        Self { models: Vec::new(), source: ModelSource::None, complete: false, note: Some(note.into()), error: None }
    }

    pub fn failed(source: ModelSource, error: impl Into<String>) -> Self {
        Self { models: Vec::new(), source, complete: false, note: None, error: Some(error.into()) }
    }

    fn of(models: Vec<ModelOption>, source: ModelSource, complete: bool, note: Option<&str>) -> Self {
        Self { models: dedup(models), source, complete, note: note.map(str::to_string), error: None }
    }

    /// Whether `model` is on the list ([`same_model`]).
    pub fn contains(&self, model: &str) -> bool {
        self.models.iter().any(|m| same_model(&m.id, model))
    }
}

fn dedup(models: Vec<ModelOption>) -> Vec<ModelOption> {
    let mut out: Vec<ModelOption> = Vec::with_capacity(models.len());
    for m in models {
        if is_model_id(&m.id) && !out.iter().any(|o| o.id == m.id) {
            out.push(m);
        }
    }
    out
}

/// Two model names that run the same model: equal ignoring case and
/// surrounding space, with Ollama's implicit `:latest` tag.
pub fn same_model(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.trim().to_lowercase();
        s.strip_suffix(":latest").map(str::to_string).unwrap_or(s)
    };
    norm(a) == norm(b)
}

/// A turn error that says the model isn't available to the account or
/// server (Copilot's "Model is not available", Ollama's `model "x" not
/// found`, OpenAI's `model_not_found`, Codex's "model is not supported",
/// Anthropic's `not_found_error` on `model:`). Transient capacity errors
/// ("temporarily unavailable", "overloaded") are not.
pub fn is_model_unavailable(text: &str) -> bool {
    let t = text.to_lowercase();
    if t.contains("model_not_found") || (t.contains("not_found_error") && t.contains("model:")) {
        return true;
    }
    if t.contains("temporarily") || t.contains("overloaded") || t.contains("high demand") || t.contains("try again") {
        return false;
    }
    const AFTER_MODEL: &[&str] = &[
        "is not available",
        "not available to",
        "not available for",
        "is not supported",
        "not found",
        "does not exist",
        "is not enabled",
        "isn't available",
        "is unknown",
    ];
    const BEFORE_MODEL: &[&str] = &["unknown model", "invalid model", "unsupported model", "no such model", "not a valid model"];
    if BEFORE_MODEL.iter().any(|p| t.contains(p)) {
        return true;
    }
    // A phrase shortly after a mention of "model" ("Model \"x\" is not
    // available", "the model `x` does not exist").
    t.match_indices("model").any(|(i, _)| {
        let rest = &t[i..];
        let mut end = 120.min(rest.len());
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let window = &rest[..end];
        AFTER_MODEL.iter().any(|p| window.contains(p))
    })
}

// ---------------------------------------------------------------------------
// Parsers (pure; tested on recorded output).
// ---------------------------------------------------------------------------

/// Whether `id` can be offered as a model: non-empty, no spaces or control
/// characters, and not starting with `-` (a harness takes the model as an
/// argument, where a leading dash would read as a flag).
pub fn is_model_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty() && !id.starts_with('-') && !id.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// `agy models`: `<id>\t<display name>` lines (other lines, like
/// "Fetching available models...", are skipped).
pub fn parse_agy_models(out: &str) -> Vec<ModelOption> {
    out.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(id, label)| (id.trim(), label.trim()))
        .filter(|(id, _)| is_model_id(id))
        .map(|(id, label)| ModelOption::new(id, Some(label.to_string())))
        .collect()
}

/// `copilot help config`: the quoted choices listed under the `model`
/// setting (`  \`model\`: AI model to use …` then `    - "claude-sonnet-5.5"`
/// lines, up to the next blank line or setting).
pub fn parse_copilot_config_help(out: &str) -> Vec<ModelOption> {
    let mut models = Vec::new();
    let mut inside = false;
    for line in out.lines() {
        let t = line.trim();
        if !inside {
            inside = t.starts_with("`model`:");
            continue;
        }
        if t.is_empty() || t.starts_with('`') {
            break;
        }
        if let Some(rest) = t.strip_prefix("- ") {
            let id = rest.trim().trim_matches('"').trim();
            if is_model_id(id) {
                models.push(ModelOption::new(id, None));
            }
        }
    }
    models
}

/// `codex debug models`: `{"models": [{"slug", "display_name",
/// "visibility"}]}`; entries the CLI hides from its own picker are left out.
pub fn parse_codex_models(json: &str) -> Vec<ModelOption> {
    let Some(v) = first_json_object(json) else { return Vec::new() };
    v.get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|m| m.get("visibility").and_then(Value::as_str).is_none_or(|v| v == "list"))
                .filter_map(|m| {
                    let id = m.get("slug").and_then(Value::as_str).filter(|id| is_model_id(id))?;
                    Some(ModelOption::new(id, m.get("display_name").and_then(Value::as_str).map(str::to_string)))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The JSON object in a CLI's stdout (a log line before it is skipped).
fn first_json_object(text: &str) -> Option<Value> {
    serde_json::from_str(text.trim()).ok().or_else(|| {
        let start = text.find('{')?;
        serde_json::from_str(&text[start..]).ok()
    })
}

/// An OpenAI-style `GET /models` body (`{"data": [{"id", "name"?,
/// "display_name"?}]}`), as OpenAI, OpenRouter, Anthropic, LM Studio,
/// llama.cpp and gateways return it.
pub fn parse_openai_models(json: &Value) -> Vec<ModelOption> {
    json.get("data")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let id = m.get("id").and_then(Value::as_str).filter(|id| is_model_id(id))?;
                    let label = m.get("display_name").or_else(|| m.get("name")).and_then(Value::as_str).map(str::to_string);
                    Some(ModelOption::new(id, label.filter(|l| l != id)))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `GET /models` URL next to an OpenAI-compatible chat-completions URL
/// (`…/v1/chat/completions` → `…/v1/models`; a bare base → `base/v1/models`).
pub fn models_url(endpoint: &str) -> String {
    let e = endpoint.trim().trim_end_matches('/');
    let e = e.split('?').next().unwrap_or(e);
    match e.strip_suffix("/chat/completions") {
        Some(base) => format!("{base}/models"),
        None if e.ends_with("/v1") => format!("{e}/models"),
        None => format!("{e}/v1/models"),
    }
}

// ---------------------------------------------------------------------------
// Listing.
// ---------------------------------------------------------------------------

/// The program a CLI runtime runs: a path as configured, else looked up on
/// `PATH`.
fn resolve_program(program: &str, env: &DetectEnv) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    find_on_path(program, &env.path)
}

/// Whether `path`'s file name is the harness's own binary (`agy`,
/// `copilot`, `codex`; on Windows with `.exe`, `.cmd` or `.bat`). Listing
/// runs the program, and the add-runtime form lists as the endpoint is
/// typed, so any other file is never run.
fn is_harness_binary(path: &Path, expected: &str) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    if name == expected {
        return true;
    }
    cfg!(windows)
        && [".exe", ".cmd", ".bat"]
            .iter()
            .any(|ext| name.eq_ignore_ascii_case(&format!("{expected}{ext}")))
}

/// A fresh empty directory to run a listing in, so a harness never reads
/// project files (or config) from wherever the app was started. Removed on
/// drop.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Option<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "ligus-model-list-{}-{nanos}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).ok()?;
        Some(Self(dir))
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A subscription CLI's models (blocking; run it off the UI thread).
/// `program` is the runtime's binary (`agy`, `copilot`, `codex`, or a path
/// whose file name is one of those); any other program is refused.
pub fn cli_models(provider: ModelProviderKind, program: &str, env: &DetectEnv) -> ModelList {
    let (expected, args, timeout, source): (&str, &[&str], Duration, ModelSource) = match provider {
        ModelProviderKind::Antigravity => ("agy", &["models"], AGY_LIST_TIMEOUT, ModelSource::Live),
        ModelProviderKind::Copilot => ("copilot", &["help", "config"], CLI_LIST_TIMEOUT, ModelSource::Catalog),
        ModelProviderKind::Codex => ("codex", &["debug", "models"], CLI_LIST_TIMEOUT, ModelSource::Catalog),
        _ => return ModelList::none("This harness has no model list; type a model name."),
    };
    if !is_harness_binary(Path::new(program.trim()), expected) {
        return ModelList::failed(source, format!("models are listed only from {expected} itself; type a model name"));
    }
    let Some(path) = resolve_program(program.trim(), env) else {
        return ModelList::failed(source, format!("{program} isn't installed on this device"));
    };
    if !is_harness_binary(&path, expected) {
        return ModelList::failed(source, format!("models are listed only from {expected} itself; type a model name"));
    }
    let Some(scratch) = ScratchDir::new() else {
        return ModelList::failed(source, "couldn't make a temporary directory to list models in");
    };
    let run = |args: &[&str]| run_bounded_split_in(&path, args, env, timeout, Some(&scratch.0));
    let shown = args.iter().fold(program.to_string(), |a, s| format!("{a} {s}"));
    match run(args) {
        None => ModelList::failed(source, format!("`{shown}` didn't answer within {}s", timeout.as_secs())),
        Some((false, out, err)) => {
            let detail = if err.trim().is_empty() { out } else { err };
            let detail: String = detail.trim().chars().take(200).collect();
            ModelList::failed(source, format!("`{shown}` failed: {detail}"))
        }
        Some((true, out, err)) => match provider {
            ModelProviderKind::Antigravity => {
                let mut models = parse_agy_models(&out);
                if models.is_empty() {
                    models = parse_agy_models(&err);
                }
                if models.is_empty() {
                    ModelList::failed(source, "`agy models` listed no models")
                } else {
                    ModelList::of(models, source, true, None)
                }
            }
            ModelProviderKind::Copilot => ModelList::of(
                parse_copilot_config_help(&out),
                source,
                false,
                Some("Copilot lists every model it knows; your plan may not include all of them."),
            ),
            _ => {
                let models = parse_codex_models(&out);
                if models.is_empty() {
                    // An offline refresh can fail quietly: fall back to the
                    // catalog bundled with the CLI.
                    match run(&["debug", "models", "--bundled"]) {
                        Some((true, out, _)) => ModelList::of(parse_codex_models(&out), source, false, None),
                        _ => ModelList::failed(source, "`codex debug models` listed no models"),
                    }
                } else {
                    ModelList::of(models, source, false, None)
                }
            }
        },
    }
}

/// What an HTTP listing needs.
#[derive(Debug, Clone)]
pub struct HttpListing {
    pub provider: ModelProviderKind,
    /// The runtime's endpoint as a turn resolves it (the chat-completions
    /// URL for OpenAI-wire providers; empty for Anthropic).
    pub endpoint: String,
    pub api_key: Option<String>,
}

/// An HTTP runtime's models.
pub async fn http_models(req: HttpListing) -> ModelList {
    match tokio::time::timeout(HTTP_LIST_TIMEOUT, http_models_inner(&req)).await {
        Ok(list) => list,
        Err(_) => ModelList::failed(ModelSource::Live, format!("the server didn't answer within {}s", HTTP_LIST_TIMEOUT.as_secs())),
    }
}

async fn http_models_inner(req: &HttpListing) -> ModelList {
    let key = req.api_key.as_deref().map(str::trim).filter(|k| !k.is_empty());
    match req.provider {
        ModelProviderKind::Ollama => {
            let client = crate::provider::OllamaClient::new(&req.endpoint).with_connect_retries(0);
            match client.list_models().await {
                Ok(models) => ModelList::of(models.into_iter().map(|m| ModelOption::new(m.name, None)).collect(), ModelSource::Live, true, None),
                Err(e) => ModelList::failed(ModelSource::Live, e.to_string()),
            }
        }
        ModelProviderKind::Anthropic => {
            let Some(key) = key else { return ModelList::failed(ModelSource::Live, "add an Anthropic API key to list its models") };
            let http = crate::provider::http::client();
            let r = http
                .get("https://api.anthropic.com/v1/models?limit=100")
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .send()
                .await;
            from_response(r, false, None).await
        }
        ModelProviderKind::OpenAI | ModelProviderKind::OpenRouter | ModelProviderKind::Custom => {
            let url = models_url(&req.endpoint);
            let local = ligus_core::endpoint::endpoint_is_local_server(&req.endpoint);
            let mut r = crate::provider::http::client().get(&url);
            if let Some(k) = key {
                r = r.bearer_auth(k);
            }
            let note = (!local).then_some("A hosted API may list models your key can't use.");
            from_response(r.send().await, local, note).await
        }
        _ => ModelList::none("This runtime has no model list; type a model name."),
    }
}

async fn from_response(r: Result<reqwest::Response, reqwest::Error>, complete: bool, note: Option<&str>) -> ModelList {
    let resp = match r {
        Ok(r) => r,
        Err(e) => return ModelList::failed(ModelSource::Live, format!("couldn't reach the server: {e}")),
    };
    let status = resp.status();
    if !status.is_success() {
        return ModelList::failed(ModelSource::Live, format!("the server answered {status}"));
    }
    match resp.json::<Value>().await {
        Ok(v) => {
            let models = parse_openai_models(&v);
            if models.is_empty() {
                ModelList::failed(ModelSource::Live, "the server listed no models")
            } else {
                ModelList::of(models, ModelSource::Live, complete, note)
            }
        }
        Err(e) => ModelList::failed(ModelSource::Live, format!("couldn't read the model list: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGY: &str = "Fetching available models...\ngemini-4-neon-high\tGemini 4 Neon (High)\ngemini-3.8-flash-low\tGemini 3.8 Flash (Low)\nclaude-opus-5-5-high\tClaude Opus 5.5 (High)\n";

    const COPILOT_HELP: &str = r#"  `stayInAutopilot`: whether to stay in autopilot mode; defaults to `true`.

  `model`: AI model to use for Copilot CLI, including custom and local models; can be changed with /model command or --model flag option. Refer to `copilot help providers`.
    - "claude-sonnet-5.5"
    - "claude-opus-4.8"
    - "gpt-5.3-codex"
    - "gemini-3.8-flash"

  `contextTier`: context window tier for tiered-pricing models (e.g., "default" or "long_context").
    - When unset, a model starts on the tier its catalog entry prefers
"#;

    const CODEX: &str = r#"{"models":[
      {"slug":"gpt-reserve","display_name":"GPT-Reserve","visibility":"hide"},
      {"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","visibility":"list"},
      {"slug":"gpt-5.6-terra","display_name":"GPT-5.6-Terra","visibility":"list"},
      {"slug":"codex-auto-review","display_name":"Codex Auto Review","visibility":"hide"}
    ]}"#;

    fn ids(m: &[ModelOption]) -> Vec<&str> {
        m.iter().map(|m| m.id.as_str()).collect()
    }

    #[test]
    fn parses_agy_models_with_labels() {
        let m = parse_agy_models(AGY);
        assert_eq!(ids(&m), ["gemini-4-neon-high", "gemini-3.8-flash-low", "claude-opus-5-5-high"]);
        assert_eq!(m[0].label.as_deref(), Some("Gemini 4 Neon (High)"));
    }

    #[test]
    fn parses_only_the_model_setting_from_copilot_help() {
        let m = parse_copilot_config_help(COPILOT_HELP);
        assert_eq!(ids(&m), ["claude-sonnet-5.5", "claude-opus-4.8", "gpt-5.3-codex", "gemini-3.8-flash"]);
        assert!(parse_copilot_config_help("no settings here").is_empty());
    }

    #[test]
    fn parses_codex_catalog_without_hidden_models() {
        let m = parse_codex_models(CODEX);
        assert_eq!(ids(&m), ["gpt-5.6-sol", "gpt-5.6-terra"]);
        assert_eq!(m[0].label.as_deref(), Some("GPT-5.6-Sol"));
        // A log line before the JSON is skipped.
        assert_eq!(parse_codex_models(&format!("refreshing…\n{CODEX}")).len(), 2);
        assert!(parse_codex_models("not json").is_empty());
    }

    #[test]
    fn parses_openai_style_lists() {
        let v = serde_json::json!({"data": [
            {"id": "qwen3-coder", "object": "model"},
            {"id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5"},
            {"id": "openai/gpt-6", "name": "OpenAI: GPT-6"}
        ]});
        let m = parse_openai_models(&v);
        assert_eq!(ids(&m), ["qwen3-coder", "claude-sonnet-5-5", "openai/gpt-6"]);
        assert_eq!(m[0].label, None);
        assert_eq!(m[1].label.as_deref(), Some("Claude Sonnet 5.5"));
    }

    #[test]
    fn models_url_sits_next_to_chat_completions() {
        assert_eq!(models_url("http://box:8080/v1/chat/completions"), "http://box:8080/v1/models");
        assert_eq!(models_url("https://openrouter.ai/api/v1/chat/completions"), "https://openrouter.ai/api/v1/models");
        assert_eq!(models_url("http://localhost:1234/v1"), "http://localhost:1234/v1/models");
        assert_eq!(models_url("http://localhost:1234/"), "http://localhost:1234/v1/models");
    }

    #[test]
    fn same_model_ignores_case_and_ollama_latest() {
        assert!(same_model("llama3.2", "llama3.2:latest"));
        assert!(same_model(" GPT-5.3-codex ", "gpt-5.3-codex"));
        assert!(!same_model("llama3.2", "llama3.2:1b"));
    }

    #[test]
    fn recognizes_model_unavailable_errors() {
        for e in [
            "Copilot CLI: Model is not available",
            "Error: Model \"claude-opus-4.8\" is not available.",
            "ollama: model \"llama9\" not found, try pulling it first",
            "openai 404: {\"error\":{\"code\":\"model_not_found\",\"message\":\"The model `gpt-x` does not exist or you do not have access to it.\"}}",
            "The 'gpt-5.3-codex' model is not supported when using Codex with a ChatGPT account.",
            "anthropic 404: {\"type\":\"error\",\"error\":{\"type\":\"not_found_error\",\"message\":\"model: claude-nope\"}}",
            "Unknown model: gemini-9",
        ] {
            assert!(is_model_unavailable(e), "{e}");
        }
        for e in [
            "Copilot CLI isn't signed in. Run `copilot login`.",
            "rate limit reached for this model, try again later",
            "The model is temporarily unavailable",
            "Overloaded: model capacity exceeded",
            "file not found: src/main.rs",
            "",
        ] {
            assert!(!is_model_unavailable(e), "{e}");
        }
    }

    #[cfg(unix)]
    fn fake_bin(dir: &Path, name: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn env_for(bin: &Path) -> DetectEnv {
        let mut path = std::ffi::OsString::from(bin);
        path.push(":/usr/bin:/bin");
        DetectEnv { path, home: bin.to_path_buf(), vars: Default::default() }
    }

    #[cfg(unix)]
    #[test]
    fn lists_each_cli_with_its_read_only_command() {
        let bin = tempfile::tempdir().unwrap();
        // Each fake answers only its listing command; anything else (a
        // prompt) fails the test by exiting non-zero.
        fake_bin(bin.path(), "agy", &format!("[ \"$1\" = models ] || exit 9\nprintf '{}'", AGY.replace('\n', "\\n").replace('\t', "\\t")));
        fake_bin(bin.path(), "copilot", &format!("[ \"$1 $2\" = 'help config' ] || exit 9\ncat <<'EOF'\n{COPILOT_HELP}EOF"));
        fake_bin(bin.path(), "codex", &format!("[ \"$1 $2\" = 'debug models' ] || exit 9\ncat <<'EOF'\n{CODEX}\nEOF"));
        let env = env_for(bin.path());

        let agy = cli_models(ModelProviderKind::Antigravity, "agy", &env);
        assert_eq!((agy.source, agy.complete, agy.models.len()), (ModelSource::Live, true, 3), "{agy:?}");
        let cp = cli_models(ModelProviderKind::Copilot, "copilot", &env);
        assert_eq!((cp.source, cp.complete, cp.models.len()), (ModelSource::Catalog, false, 4), "{cp:?}");
        assert!(cp.note.is_some());
        let cx = cli_models(ModelProviderKind::Codex, "codex", &env);
        assert_eq!(ids(&cx.models), ["gpt-5.6-sol", "gpt-5.6-terra"]);

        let pi = cli_models(ModelProviderKind::Pi, "pi", &env);
        assert_eq!(pi.source, ModelSource::None);
        let missing = cli_models(ModelProviderKind::Antigravity, "/nowhere/agy", &env);
        assert!(missing.error.unwrap().contains("isn't installed"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_program_that_isnt_the_harness() {
        let bin = tempfile::tempdir().unwrap();
        let marker = bin.path().join("ran");
        let touch = format!("touch '{}'", marker.display());
        // Files a person might type into the endpoint field: a script, a
        // shell, a harness binary of another provider.
        fake_bin(bin.path(), "evil.sh", &touch);
        fake_bin(bin.path(), "codex", &touch);
        let env = env_for(bin.path());
        for (kind, program) in [
            (ModelProviderKind::Codex, bin.path().join("evil.sh").display().to_string()),
            (ModelProviderKind::Codex, "evil.sh".to_string()),
            (ModelProviderKind::Antigravity, bin.path().join("codex").display().to_string()),
            (ModelProviderKind::Copilot, "/bin/sh".to_string()),
            (ModelProviderKind::Copilot, format!("{}/", bin.path().display())),
        ] {
            let l = cli_models(kind, &program, &env);
            assert!(l.models.is_empty(), "{program}: {l:?}");
            assert!(l.error.as_deref().is_some_and(|e| e.contains("only from")), "{program}: {l:?}");
        }
        assert!(!marker.exists(), "a program that isn't the harness never runs");
    }

    #[cfg(unix)]
    #[test]
    fn lists_in_an_empty_temporary_directory() {
        let bin = tempfile::tempdir().unwrap();
        let out = bin.path().join("cwd");
        fake_bin(
            bin.path(),
            "agy",
            &format!("pwd > '{}'; ls -A >> '{}'; printf 'gemini-x\\tGemini X\\n'", out.display(), out.display()),
        );
        let l = cli_models(ModelProviderKind::Antigravity, "agy", &env_for(bin.path()));
        assert_eq!(ids(&l.models), ["gemini-x"], "{l:?}");
        let seen = std::fs::read_to_string(&out).unwrap();
        let mut lines = seen.lines();
        let cwd = std::path::PathBuf::from(lines.next().unwrap());
        assert!(cwd.file_name().unwrap().to_string_lossy().starts_with("ligus-model-list-"), "{seen}");
        assert_eq!(lines.next(), None, "the directory is empty");
        assert!(!cwd.exists(), "and removed afterwards");
        assert_ne!(cwd, std::env::current_dir().unwrap());
    }

    #[test]
    fn model_ids_starting_with_a_dash_are_never_listed() {
        assert!(!is_model_id("--config=/tmp/x") && !is_model_id("-m") && !is_model_id(" ") && !is_model_id("a b"));
        assert!(is_model_id("gpt-5.6-sol") && is_model_id("openai/gpt-6") && is_model_id("llama3.2:latest"));
        assert_eq!(ids(&parse_agy_models("--yolo\tYolo\ngemini-x\tGemini X\n")), ["gemini-x"]);
        let help = "  `model`: AI model\n    - \"--allow-all-tools\"\n    - \"gpt-5\"\n";
        assert_eq!(ids(&parse_copilot_config_help(help)), ["gpt-5"]);
        let codex = r#"{"models":[{"slug":"--dangerously-bypass","visibility":"list"},{"slug":"gpt-5.6-sol","visibility":"list"}]}"#;
        assert_eq!(ids(&parse_codex_models(codex)), ["gpt-5.6-sol"]);
        let v = serde_json::json!({"data": [{"id": "-x"}, {"id": "qwen3-coder"}]});
        assert_eq!(ids(&parse_openai_models(&v)), ["qwen3-coder"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_listing_reports_why() {
        let bin = tempfile::tempdir().unwrap();
        fake_bin(bin.path(), "agy", "echo 'Error: not signed in' >&2; exit 1");
        let l = cli_models(ModelProviderKind::Antigravity, "agy", &env_for(bin.path()));
        assert!(l.models.is_empty() && !l.complete);
        assert!(l.error.unwrap().contains("not signed in"));
    }

    #[tokio::test]
    async fn lists_an_openai_compatible_server() {
        use axum::{routing::get, Json, Router};
        let app = Router::new().route(
            "/v1/models",
            get(|| async { Json(serde_json::json!({"object": "list", "data": [{"id": "qwen3-coder"}, {"id": "gpt-oss:20b"}]})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let l = http_models(HttpListing {
            provider: ModelProviderKind::OpenAI,
            endpoint: format!("http://{addr}/v1/chat/completions"),
            api_key: None,
        })
        .await;
        assert_eq!(ids(&l.models), ["qwen3-coder", "gpt-oss:20b"]);
        assert!(l.complete, "a loopback server's list is complete");
        assert!(l.contains("gpt-oss:20b") && !l.contains("llama9"));
    }
}
