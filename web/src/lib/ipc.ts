// Typed wrappers around the Tauri IPC commands/events. Each corresponds to a
// `#[tauri::command]` (or emitted event) in `app`, typed with the ts-rs
// bindings generated from `hive-proto`. This is the single seam the frontend
// uses to reach the Rust backend.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppInfo } from "@/bindings/AppInfo";
import type { ChatSummaryDto } from "@/bindings/ChatSummaryDto";
import type { ChannelDto } from "@/bindings/ChannelDto";
import type { ChatSessionDto } from "@/bindings/ChatSessionDto";
import type { ChatStreamEvent } from "@/bindings/ChatStreamEvent";
import type { ChatActivityEvent } from "@/bindings/ChatActivityEvent";
import type { GitFileDiffDto } from "@/bindings/GitFileDiffDto";
import type { AppSettingsDto } from "@/bindings/AppSettingsDto";
import type { WorkspaceAgentDto } from "@/bindings/WorkspaceAgentDto";
import type { SkillDto } from "@/bindings/SkillDto";
import type { McpServerDto } from "@/bindings/McpServerDto";
import type { ProposalDto } from "@/bindings/ProposalDto";
import type { ProposalSetDto } from "@/bindings/ProposalSetDto";
import type { WorkspaceMemberDto } from "@/bindings/WorkspaceMemberDto";
import type { VaultSourceDto } from "@/bindings/VaultSourceDto";
import type { RuntimeSummaryDto } from "@/bindings/RuntimeSummaryDto";
import type { ContextTelemetryDto } from "@/bindings/ContextTelemetryDto";
import type { WorkflowDefinitionDto } from "@/bindings/WorkflowDefinitionDto";
import type { WorkflowRunDto } from "@/bindings/WorkflowRunDto";
import type { WorkflowRunEvent } from "@/bindings/WorkflowRunEvent";
import type { QueuedWorkDto } from "@/bindings/QueuedWorkDto";
import type { WorkspaceHostDto } from "@/bindings/WorkspaceHostDto";
import type { MentionStateDto } from "@/bindings/MentionStateDto";

export type { ChatSummaryDto } from "@/bindings/ChatSummaryDto";
export type { ChannelDto } from "@/bindings/ChannelDto";
export type { ChatSessionDto } from "@/bindings/ChatSessionDto";
export type { ChatMessageDto } from "@/bindings/ChatMessageDto";
export type { GitFileDiffDto } from "@/bindings/GitFileDiffDto";
export type { AppSettingsDto } from "@/bindings/AppSettingsDto";
export type { WorkspaceAgentDto } from "@/bindings/WorkspaceAgentDto";
export type { SkillDto } from "@/bindings/SkillDto";
export type { McpServerDto } from "@/bindings/McpServerDto";
export type { ProposalDto } from "@/bindings/ProposalDto";
export type { ApplyRequestDto } from "@/bindings/ApplyRequestDto";
export type { ImplementPreviewDto } from "@/bindings/ImplementPreviewDto";
import type { ImplementPreviewDto } from "@/bindings/ImplementPreviewDto";
export type { ReactionDto } from "@/bindings/ReactionDto";
export type { WorkspaceMemberDto } from "@/bindings/WorkspaceMemberDto";
export type { VaultSourceDto } from "@/bindings/VaultSourceDto";
export type { RuntimeSummaryDto } from "@/bindings/RuntimeSummaryDto";
export type { ContextTelemetryDto } from "@/bindings/ContextTelemetryDto";

export const getAppInfo = () => invoke<AppInfo>("get_app_info");

export const listChats = () => invoke<ChatSummaryDto[]>("list_chats");

export const createChat = (title: string) =>
  invoke<ChatSessionDto>("create_chat", { title });

// ── Channels (spec §11) ──────────────────────────────────────────────────────
export const listChannels = () => invoke<ChannelDto[]>("list_channels");

export const createChannel = (name: string, purpose?: string) =>
  invoke<ChannelDto>("create_channel", { name, purpose: purpose ?? null });

export const renameChannel = (channelId: string, name: string, purpose?: string) =>
  invoke<void>("rename_channel", { channelId, name, purpose: purpose ?? null });

export const reorderChannels = (channelIds: string[]) =>
  invoke<void>("reorder_channels", { channelIds });

export const archiveChannel = (channelId: string, archived: boolean) =>
  invoke<void>("archive_channel", { channelId, archived });

export const createChatInChannel = (channelId: string, title: string) =>
  invoke<ChatSessionDto>("create_chat_in_channel", { channelId, title });

export const moveChat = (chatId: string, channelId: string) =>
  invoke<void>("move_chat", { chatId, channelId });

export const getChat = (sessionId: string) =>
  invoke<ChatSessionDto | null>("get_chat", { sessionId });

export const getContextTelemetry = (sessionId: string) =>
  invoke<ContextTelemetryDto | null>("get_context_telemetry", { sessionId });

export const sendMessage = (sessionId: string, body: string) =>
  invoke<void>("send_message", { sessionId, body });

/// Stop the in-flight turn for a session (the composer's Stop button). Aborts
/// generation and kills any CLI subprocess; safe no-op if nothing is running.
export const stopTurn = (sessionId: string) =>
  invoke<void>("stop_turn", { sessionId });

/// Force a chat out of a wedged "responding" state: aborts any in-flight turn,
/// frees the per-chat busy slot a stuck turn couldn't release, and clears a
/// lingering "generating" placeholder. Escape hatch for the stalled banner.
export const resetChatDispatch = (sessionId: string) =>
  invoke<void>("reset_chat_dispatch", { sessionId });

/// Replace the last assistant/agent turn with a fresh generation.
export const regenerate = (sessionId: string) =>
  invoke<void>("regenerate", { sessionId });

export const archiveChat = (sessionId: string, archived: boolean) =>
  invoke<void>("archive_chat", { sessionId, archived });

/// Rename a chat (the pencil affordance in the chat header).
export const renameChat = (sessionId: string, title: string) =>
  invoke<void>("rename_chat", { sessionId, title });

export const deleteChat = (sessionId: string) =>
  invoke<void>("delete_chat", { sessionId });

export const getWorkspaceDiffs = () =>
  invoke<GitFileDiffDto[]>("get_workspace_diffs");

export const getAppSettings = () => invoke<AppSettingsDto>("get_app_settings");
/// Record first-run progress in Ligus's data dir (settings.json), so it resets
/// with the data dir rather than living in the webview's storage.
export const markOnboarded = () => invoke<void>("mark_onboarded");
export const markTourSeen = () => invoke<void>("mark_tour_seen");

/// Git branch + dirty count for the active workspace. Separate (lazy) command —
/// it shells out to git, so it's queried only where the pill is shown.
export interface GitStatus {
  branch: string | null;
  dirtyCount: number;
}
export const getGitStatus = () => invoke<GitStatus>("get_git_status");

export const listRuntimes = () => invoke<RuntimeSummaryDto[]>("list_runtimes");

export const addRuntime = (
  id: string,
  name: string,
  provider: string,
  location: string,
  endpoint: string,
  model: string,
  supportsTools: boolean,
  supportsEmbeddings: boolean,
  modelBaseUrl?: string | null,
  modelProviderId?: string | null,
  contextWindow?: number | null,
  keepAlive?: string | null,
  think?: boolean | null,
  maxToolRounds?: number | null,
  allowRemoteFileTools?: boolean | null,
  gateway?: string | null,
) =>
  invoke<void>("add_runtime", {
    id,
    name,
    provider,
    location,
    endpoint,
    model,
    supportsTools,
    supportsEmbeddings,
    modelBaseUrl: modelBaseUrl ?? null,
    modelProviderId: modelProviderId ?? null,
    contextWindow: contextWindow ?? null,
    keepAlive: keepAlive ?? null,
    think: think ?? null,
    maxToolRounds: maxToolRounds ?? null,
    allowRemoteFileTools: allowRemoteFileTools ?? null,
    gateway: gateway ?? null,
  });

/// Whether an endpoint's host is local or private (loopback, LAN, Tailscale,
/// link-local, *.local; not *.ts.net names, which Funnel can make public) —
/// Hive's built-in file tools are only offered
/// to other endpoints with the runtime's explicit opt-in.
export const endpointIsPrivate = (endpoint: string) => invoke<boolean>("endpoint_is_private", { endpoint });

/// Hive's built-in file tools for HTTP model runtimes (Settings → Permissions).
export type AgentToolSettings = {
  /// Offer read_file / list_dir / search (and edits where allowed).
  enabled: boolean;
  /// Allow edits when a turn runs in place (no isolated worktree).
  writeInPlace: boolean;
  /// Offer run_command (shell) where edits are allowed.
  runCommand: boolean;
};
export const getAgentToolSettings = () => invoke<AgentToolSettings>("get_agent_tool_settings");
export const setAgentToolSettings = (settings: AgentToolSettings) =>
  invoke<void>("set_agent_tool_settings", { settings });

/// Vault retrieval (Settings → Models). Empty embeddings URL/model = off, and
/// vaults are pasted into the system prompt as before. URLs take a base
/// (http://host:8080/v1) or the full /embeddings, /rerank endpoint.
export type RetrievalSettings = {
  embedUrl: string;
  embedModel: string;
  /// Optional; empty = rank by embedding similarity alone.
  rerankUrl: string;
  rerankModel: string;
  /// Passages added to each message.
  topK: number;
  /// Nearest chunks handed to the reranker.
  candidates: number;
};
export const getRetrievalSettings = () => invoke<RetrievalSettings>("get_retrieval_settings");
export const setRetrievalSettings = (settings: RetrievalSettings) =>
  invoke<void>("set_retrieval_settings", { settings });
/// Resolves with a short success line; rejects with what went wrong.
export const testRetrievalSettings = (settings: RetrievalSettings) =>
  invoke<string>("test_retrieval_settings", { settings });

/// Custom /summarize + /compact instructions ("" = using the built-in default,
/// which is returned so the UI can show it).
export interface ContextCommandsDto {
  summarizePrompt: string;
  compactPrompt: string;
  defaultPrompt: string;
}
export const getContextCommands = () =>
  invoke<ContextCommandsDto>("get_context_commands");
export const setContextCommands = (summarizePrompt: string, compactPrompt: string) =>
  invoke<void>("set_context_commands", { summarizePrompt, compactPrompt });

/// The "Quick tasks model": runtime id used for chat titles (null = same as
/// the chat's runtime).
export const getUtilityRuntime = () => invoke<string | null>("get_utility_runtime");
export const setUtilityRuntime = (runtimeId: string | null) =>
  invoke<void>("set_utility_runtime", { runtimeId });

export const removeRuntime = (runtimeId: string) =>
  invoke<void>("remove_runtime", { runtimeId });

export type RuntimeTestResult = {
  ok: boolean;
  latency_ms: number;
  reply: string;
  error: string | null;
  /// Native Ollama only: "tools · thinking · 40k max ctx" as reported by the server.
  capabilities?: string | null;
};

/// Preflight a runtime: sends a trivial prompt through the real dispatch path
/// and reports pass/fail + latency, so a CLI that hangs or isn't logged in shows
/// up here instead of as a silent "thinking forever" in chat.
export const testRuntime = (runtimeId: string) =>
  invoke<RuntimeTestResult>("test_runtime", { runtimeId });

// --- Ollama model management (Settings → Models → "Models on this server") ---

/// A model installed on an Ollama server, plus whether it's loaded right now.
export type OllamaModel = {
  name: string;
  sizeBytes: number;
  modifiedAt: string;
  family: string;
  parameterSize: string;
  quantization: string;
  loaded: boolean;
  loadedContextLength: number | null;
  expiresAt: string | null;
};
export type OllamaModels = {
  /// Normalized server base URL; `ollama://pull` events carry the same string.
  base: string;
  host: string;
  models: OllamaModel[];
  /// Models with a pull already in flight on this server.
  pulling: string[];
};
/// Which server: an existing Ollama runtime's id, or a bare endpoint (the
/// add-runtime form, before a runtime exists). Both omitted = the Ollama
/// provider's saved base URL, else localhost.
export type OllamaServerRef = { runtimeId?: string | null; endpoint?: string | null };
const serverArgs = (s: OllamaServerRef) => ({
  runtimeId: s.runtimeId ?? null,
  endpoint: s.endpoint ?? null,
});

/// Installed + loaded models on the server (`/api/tags` + `/api/ps`).
export const listOllamaModels = (server: OllamaServerRef) =>
  invoke<OllamaModels>("ollama_list_models", serverArgs(server));
/// Start pulling (or updating) a model in the background; progress arrives on
/// `onOllamaPull`. Rejects if that model is already being pulled there.
export const pullOllamaModel = (server: OllamaServerRef, model: string) =>
  invoke<void>("ollama_pull_model", { ...serverArgs(server), model });
/// Cancel an in-flight pull (already-downloaded layers stay; re-pull resumes).
export const cancelOllamaPull = (server: OllamaServerRef, model: string) =>
  invoke<void>("ollama_cancel_pull", { ...serverArgs(server), model });
/// Remove an installed model from the server.
export const deleteOllamaModel = (server: OllamaServerRef, model: string) =>
  invoke<void>("ollama_delete_model", { ...serverArgs(server), model });

/// Pull progress. `done` / `error` / `canceled` are terminal; before that
/// `status` is the server's phase and `completed`/`total` are bytes of the
/// layer currently downloading.
export type OllamaPullEvent = {
  base: string;
  model: string;
  status: string;
  total?: number;
  completed?: number;
  done: boolean;
  canceled: boolean;
  error?: string;
};
export const onOllamaPull = (cb: (e: OllamaPullEvent) => void): Promise<UnlistenFn> =>
  listen<OllamaPullEvent>("ollama://pull", (evt) => cb(evt.payload));

/// Validate a provider's key at entry (no runtime needed): pings the provider
/// with a chosen/default model and reports pass/fail + latency + error. `model`
/// is optional — omit to use a safe default just to check the key.
export const probeProvider = (kind: string, model?: string, key?: string, baseUrl?: string) =>
  invoke<RuntimeTestResult>("probe_provider", {
    kind,
    model: model ?? null,
    key: key ?? null,
    baseUrl: baseUrl ?? null,
  });

export const setChatRuntime = (sessionId: string, runtimeId: string) =>
  invoke<void>("set_chat_runtime", { sessionId, runtimeId });

/// Add the local user to a chat's roster on open (so People shows who's here).
export const ensureSelfMember = (sessionId: string) =>
  invoke<void>("ensure_self_member", { sessionId });

/// Cross-device dispatch: ask this device to answer a synced, unanswered user
/// message if it owns the responder (no-op otherwise). Safe to call on sync.
export const maybeRespond = (sessionId: string) =>
  invoke<void>("maybe_respond", { sessionId });

/// Raise a local notification if the newest synced message @-mentions you.
/// Deduped server-side; safe to call on every sync.
export const notifyMentions = (sessionId: string) =>
  invoke<void>("notify_mentions", { sessionId });

export interface PresenceDto {
  actorId: string;
  name: string;
  typing: boolean;
  sessionId: string;
}

/// Heartbeat the local user's presence (online + typing in `sessionId`) to the
/// relay, sealed under the workspace key. No-op when relay-less or keyless, and
/// only "not typing" is sent while "Share typing status" is off.
export const presencePing = (sessionId: string, typing: boolean) =>
  invoke<void>("presence_ping", { sessionId, typing });

/// Live presence for the workspace (excludes self + stale entries).
export const presenceList = () => invoke<PresenceDto[]>("presence_list");

/// Whether this device tells others when you're typing (Settings → Profile →
/// Privacy). Only that you're typing, sealed under the workspace key; never what.
export const getShareTyping = () => invoke<boolean>("get_share_typing");
export const setShareTyping = (enabled: boolean) => invoke<void>("set_share_typing", { enabled });

export const setWorkspaceRoot = (path: string) =>
  invoke<void>("set_workspace_root", { path });

export const addWorkspaceToList = (path: string) =>
  invoke<string[]>("add_workspace_to_list", { path });

export const pickWorkspaceFolder = () =>
  invoke<string | null>("pick_workspace_folder");

/// Read a workspace file (relative to the workspace root) to reference into a
/// chat. Path-safe + size-capped on the backend.
export const readWorkspaceFile = (path: string) =>
  invoke<string>("read_workspace_file", { path });

/// Repo-relative file paths in the workspace (git-tracked + untracked-not-ignored,
/// capped) for the @file picker's fuzzy search.
export const listWorkspaceFiles = () => invoke<string[]>("list_workspace_files");

/// Export a chat transcript to a Markdown file via a save dialog. Resolves to
/// the written path, or null if the user cancelled.
export const exportChat = (sessionId: string) =>
  invoke<string | null>("export_chat", { sessionId });

/// Persist a composer attachment (base64 bytes) to disk; resolves to its
/// absolute path, embedded in the message as a `[Attached: ...]` marker.
export const saveAttachment = (name: string, dataBase64: string) =>
  invoke<string>("save_attachment", { name, dataBase64 });

/// Turn a composer attachment into the marker to embed. Relay upload is off in
/// this release (a code-only guard in the backend, no setting), so this returns
/// a device-local `[Attached: <path>]` marker (only this device can open it).
export const shareAttachment = (sessionId: string, path: string) =>
  invoke<string>("share_attachment", { sessionId, path });

/// Resolve a received blob reference to a local file path (cache hit, or download
/// + decrypt). Rejects when it can't be fetched (no relay, aged out, wrong key).
export const resolveAttachmentBlob = (id: string, name: string) =>
  invoke<string>("resolve_attachment_blob", { id, name });

/// Read a local image attachment as a data: URL for inline preview. Rejects when
/// the file isn't present on this device (e.g. an attachment synced from another
/// device) so the UI can fall back to a filename chip.
export const readImageDataUrl = (path: string) =>
  invoke<string>("read_image_data_url", { path });

/// Tail of today's app log (internal errors/warnings/panics) for the Diagnostics
/// view.
export const readRecentLogs = (maxLines?: number) =>
  invoke<string>("read_recent_logs", { maxLines: maxLines ?? null });

/// Absolute path of the log directory (for "where are my logs" when sharing).
export const logDirPath = () => invoke<string | null>("log_dir_path");

export type MembershipAuditEntry = {
  action: "added" | "removed";
  member: string;
  role: string | null;
  by: string;
  at: string;
};

/// Workspace-join audit: who added/removed whom and when, from the signed config
/// log (newest first).
export const listMembershipAudit = () =>
  invoke<MembershipAuditEntry[]>("list_membership_audit");

export const removeWorkspaceFromList = (path: string) =>
  invoke<string[]>("remove_workspace_from_list", { path });

export const setDisplayName = (name: string) =>
  invoke<void>("set_display_name", { name });

/// Set (or clear, with null) the local user's avatar — a small `data:image` URL.
/// Rides the synced roster so every member sees it.
export const setAvatar = (avatar: string | null) =>
  invoke<void>("set_avatar", { avatar });

/// Set (or clear) an owned agent's avatar image and/or accent color.
export const setAgentAvatar = (
  sessionId: string,
  agentId: string,
  avatar: string | null,
  colorHex: string | null,
) => invoke<void>("set_agent_avatar", { sessionId, agentId, avatar, colorHex });

/// Git email used to attribute commits agents make on this user's behalf.
export const setGitEmail = (email: string) =>
  invoke<void>("set_git_email", { email });

/// Direct peer-to-peer (iroh): connect to friends by their peer code.
export interface ContactDto {
  peerId: string;
  label: string;
}
/// This device's shareable friend code (its P2P public key).
export const p2pMyCode = () => invoke<string>("p2p_my_code");
export const p2pListContacts = () => invoke<ContactDto[]>("p2p_list_contacts");
export const p2pAddContact = (code: string, label: string) =>
  invoke<void>("p2p_add_contact", { code, label });
export const p2pRemoveContact = (peerId: string) =>
  invoke<void>("p2p_remove_contact", { peerId });

/// Short, speakable pairing codes brokered through the relay.
export interface ShortCodeDto {
  code: string;
  expiresIn: number;
}
export interface RedeemResultDto {
  /** "peer" | "workspace" */
  kind: string;
  label: string;
}
/// Publish this device's friend code as a short code (e.g. "K7P2QX").
export const p2pShareCode = () => invoke<ShortCodeDto>("p2p_share_code");
/// Publish a workspace invite as a short code.
export const workspaceShareCode = (workspaceId: string) =>
  invoke<ShortCodeDto>("workspace_share_code", { workspaceId });
/// Resolve a short code and act on it (add peer / join workspace).
export const redeemShortCode = (code: string) =>
  invoke<RedeemResultDto>("redeem_short_code", { code });

export const openInEditor = () => invoke<void>("open_in_editor");

/// Both sides of one file's diff (HEAD blob vs working tree) for the
/// side-by-side editor. Binary files come back flagged and empty.
export interface FileDiffSidesDto {
  original: string;
  modified: string;
  isBinary: boolean;
}
export const getFileDiffSides = (path: string) =>
  invoke<FileDiffSidesDto>("get_file_diff_sides", { path });

/// Code editors installed on this machine (VS Code, Cursor, Zed, …).
export interface EditorDto {
  id: string;
  label: string;
}
export const detectEditors = () => invoke<EditorDto[]>("detect_editors");

/// Open a workspace file (or the root, when path is "") in a detected editor.
export const openPathInEditor = (editorId: string, path: string) =>
  invoke<void>("open_path_in_editor", { editorId, path });

/// Open an http(s) URL in the system browser (window.open doesn't reach the OS
/// browser from the Tauri webview).
export const openExternal = (url: string) =>
  invoke<void>("open_external", { url });

export type LaunchHealth = {
  macos: boolean;
  translocated: boolean;
  quarantined: boolean;
};

/// macOS signals that predict silent agent stalls (a quarantined/translocated
/// bundle can't persist the file-access grant Hive's spawned CLIs need).
export const launchHealth = () => invoke<LaunchHealth>("launch_health");

/// Open the macOS Full Disk Access pane so the user can grant Hive persistent
/// file access.
export const openFileAccessSettings = () =>
  invoke<void>("open_file_access_settings");

/// Your profile: the random account id made on first launch (never a
/// provider id), name, picture, safety code (eight groups of four hex
/// characters people compare out of band) and commit email.
export interface ProfileDto {
  accountId: string;
  displayName: string;
  avatarUrl: string | null;
  safetyCode: string | null;
  gitEmail: string;
}
export const myProfile = () => invoke<ProfileDto>("my_profile");

/// Organization sign-in through langstroth-auth (device code). The session's
/// short-lived relay token replaces the static relay access token while it is
/// valid. It grants relay access only, never workspace membership.
export interface LigusAuthStatusDto {
  authUrl: string;
  tenant: string;
  sub: string;
  workspaces: string[];
  accessExpiresAt: number;
  active: boolean;
}
export interface LigusAuthStartDto {
  userCode: string;
  verificationUri: string;
  verificationUriComplete: string | null;
  interval: number;
  expiresIn: number;
}
export const ligusAuthStatus = () => invoke<LigusAuthStatusDto | null>("ligus_auth_status");
export const ligusAuthStart = (url: string) => invoke<LigusAuthStartDto>("ligus_auth_start", { url });
/** "pending" | "slowDown" | "denied" | "expired" | "success" | "none" */
export const ligusAuthPoll = () => invoke<string>("ligus_auth_poll");
export const ligusAuthSignOut = () => invoke<void>("ligus_auth_sign_out");

/// What's installed/available on this machine, to drive first-run defaults.
export interface EnvDetectDto {
  claudeCode: boolean;
  codex: boolean;
  ollama: boolean;
  anthropicEnv: boolean;
  openaiEnv: boolean;
  gitName?: string | null;
  gitEmail?: string | null;
}
export const detectEnvironment = () => invoke<EnvDetectDto>("detect_environment");

/// A provider Hive can reach right now (creds present or none needed), for the
/// one-click "detect & configure" setup flow.
export interface DetectedProviderDto {
  kind: string;
  label: string;
  source: string; // "saved" | "env" | "local" | "path"
  needsKey: boolean;
  addable: boolean; // can be turned into a runtime directly (hosted + ollama)
}
export const detectProviders = () => invoke<DetectedProviderDto[]>("detect_providers");

/// A subscription CLI agent on this machine (Google `agy`, GitHub `copilot`,
/// or a legacy `gemini` explained): installed, version, sign-in state, and
/// the exact login / install command. Detection never changes auth state;
/// `probe` also runs `agy models` (read-only) to confirm the Google sign-in.
export interface DetectedCliDto {
  id: string; // "antigravity" | "copilot" | "gemini"
  provider: string | null; // add_runtime provider; null = can't be driven
  label: string;
  binary: string;
  installed: boolean;
  path: string | null;
  version: string | null;
  login: "loggedIn" | "notLoggedIn" | "unknown";
  loginDetail: string;
  loginCommand: string;
  installCommand: string;
  supported: boolean;
  note: string;
  models: string[];
}
export const detectCliRuntimes = (probe = false) =>
  invoke<DetectedCliDto[]>("detect_cli_runtimes", { probe });

/// Portable config (hive.toml): export the current runtime/agent/MCP setup +
/// defaults as TOML (no secrets), or import one to stamp a setup. import returns
/// a human summary.
export const exportConfig = () => invoke<string>("export_config");
export const importConfig = (tomlText: string) => invoke<string>("import_config", { tomlText });

/// LLM providers (connections): a backend kind + optional API key + base URL.
/// Runtimes (models) reference a provider for credentials/endpoint.
export interface ProviderDto {
  kind: string;
  name: string;
  needsKey: boolean;
  hasKey: boolean;
  supportsBaseUrl: boolean;
  baseUrl: string;
  note: string;
}
export const listProviders = () => invoke<ProviderDto[]>("list_providers");

/// A known OpenAI-compatible backend preset (Gemini, LM Studio, Groq, Azure, …)
/// for one-click runtime setup.
export interface ProviderPresetDto {
  label: string;
  provider: string; // the value `addRuntime` expects
  endpoint: string; // prefills the chat-completions URL
  needsKey: boolean;
  tools: boolean; // start the runtime with "Supports tools" on
}
export const listProviderPresets = () =>
  invoke<ProviderPresetDto[]>("list_provider_presets");

/// Hive's defaults for a model family (role hint in the add-runtime form).
export interface ModelProfileDto {
  family: string;
  /// "agent" | "chat" | "utility" | "quality" | "embedding" | "rerank"; null
  /// when the family isn't a good default for any role.
  role: string | null;
  roleLabel: string | null;
  reasoning: "none" | "toggle" | "always-effort" | "always";
  tools: boolean;
  contextWindow: number;
  note: string;
}
/// The profile for a model id, or null when Hive has none for its family.
export const modelProfileHint = (model: string) =>
  invoke<ModelProfileDto | null>("model_profile_hint", { model });
export const setProviderKey = (kind: string, key: string) =>
  invoke<void>("set_provider_key", { kind, key });
export const setProviderBaseUrl = (kind: string, baseUrl: string) =>
  invoke<void>("set_provider_base_url", { kind, baseUrl });

/// Reusable agent definitions (persona = name + model/runtime + role +
/// instructions) you can attach to any chat.
export interface AgentTemplateDto {
  id: string;
  name: string;
  runtimeId: string;
  role: string;
  instructions: string;
}
export const listAgentTemplates = () => invoke<AgentTemplateDto[]>("list_agent_templates");
export const addAgentTemplate = (
  name: string,
  runtimeId: string,
  role: string,
  instructions: string,
) => invoke<void>("add_agent_template", { name, runtimeId, role, instructions });
export const removeAgentTemplate = (id: string) =>
  invoke<void>("remove_agent_template", { id });

/// A server-side workspace member (relays with membership turned on only).
export interface MemberEntry {
  account: string; // the relay subject: "ligus:<account id>" or "<tenant>:<user>"
  login: string;
  role: "owner" | "admin" | "contributor" | "viewer";
  addedBy: string;
  addedAt: number;
}
/// Claim the active workspace's room on a membership-enforcing relay (caller →
/// Owner). Returns false on an open relay or if already claimed.
export const workspaceClaimMembership = () =>
  invoke<boolean>("workspace_claim_membership");
/// Server-side members of the active workspace (empty on open relays).
export const workspaceMembers = () => invoke<MemberEntry[]>("workspace_members");
/// Remove a member by relay subject; caller must be Admin+.
export const workspaceRemoveMember = (account: string) =>
  invoke<void>("workspace_remove_member", { account });

/// A relay invite (metadata only; never the code).
export interface InviteEntry {
  id: string;
  role: string;
  createdBy: string;
  expiresAt: number; // unix secs; 0 = never
  maxUses: number; // 0 = unlimited
  uses: number;
  revoked: boolean;
}
/// A freshly-issued invite — `code` shown once.
export interface IssuedInvite {
  id: string;
  code: string;
  role: string;
  expiresAt: number;
  maxUses: number;
}
/// List the active workspace's relay invites (Admin+; empty otherwise).
export const workspaceListInvites = () => invoke<InviteEntry[]>("workspace_list_invites");
/// Issue an invite (Admin+); `code` is returned once. 0 = never/unlimited.
export const workspaceCreateInvite = (role: string, ttlSecs: number, maxUses: number) =>
  invoke<IssuedInvite | null>("workspace_create_invite", { role, ttlSecs, maxUses });
/// Revoke an invite by id (Admin+).
export const workspaceRevokeInvite = (inviteId: string) =>
  invoke<void>("workspace_revoke_invite", { inviteId });
/// Redeem an invite code to self-enroll in the active workspace; returns the
/// granted role, or null if refused.
export const workspaceJoinViaInvite = (code: string) =>
  invoke<string | null>("workspace_join_via_invite", { code });

/// Remote-agent bootstrap (relay admins with an issuer key). `hasIssuerKey`
/// gates the UI; `setRelayIssuerKey` stores the local Ed25519 seed;
/// `generateAgentBootstrap` mints the box a random relay identity
/// (`ligus:<id>`), enrolls it, and returns the copy-paste command.
export const hasIssuerKey = () => invoke<boolean>("has_issuer_key");
export const setRelayIssuerKey = (key: string) =>
  invoke<void>("set_relay_issuer_key", { key });
export const generateAgentBootstrap = (role: string, label: string, expDays: number) =>
  invoke<string>("generate_agent_bootstrap", { role, label, expDays });

// ── People: co-members, DMs, invites (no directory, no handles) ───────────

/// Someone you share a workspace with, keyed by their random account id.
/// Names aren't unique: the safety code tells two people apart.
export interface CoMemberDto {
  accountId: string;
  displayName: string;
  avatarUrl: string | null;
  safetyCode: string | null;
  /// The shared workspaces disagree about their account key: nothing is
  /// sealed to them.
  keyConflict: boolean;
  /// Another co-member's name looks like this one (case, look-alike letters
  /// from other scripts): tell them apart by safety code.
  lookAlike?: boolean;
  /// Devices whose keys are verified on a shared workspace.
  devices: number;
  workspaces: { id: string; name: string }[];
  dmWorkspaceId: string | null;
  /// Their linked accounts in the workspaces you share (opt-in claims).
  /// The status is this device's cached forge check; absent when none.
  links?: LinkChipDto[];
}
export const listCoMembers = () => invoke<CoMemberDto[]>("list_co_members");

// ── Linked accounts (identity links; IDENTITY-AND-FORGES §2) ───────────────

/// A link's status on this device. Never synced: every device asks the forge
/// itself (no token, never through the relay), at most once a day per link.
export type LinkStatus =
  | { state: "verified"; checkedAt: string; handle: string | null }
  | { state: "signatureOnly" }
  | { state: "stale"; checkedAt: string; reason: string }
  | { state: "unreachable"; lastVerifiedAt: string | null; reason: string }
  | { state: "expired" }
  | { state: "invalid"; reason: string }
  | { state: "selfAsserted" }
  | { state: "notChecked"; reason: string };

export type LinkMethod = "sshSig" | "keyPublished";

export interface LinkChipDto {
  provider: string;
  host: string;
  handle: string;
  subjectId: string;
  status: LinkStatus | null;
}

export interface LinkDto extends LinkChipDto {
  linkId: string;
  method: LinkMethod | string;
  issuedAt: string;
  expiresAt: string;
  workspaces: { id: string; name: string }[];
  /// Also on this device's contact card ("discoverable by this identity").
  discoverable: boolean;
}

export interface LinkDraftDto {
  linkId: string;
  provider: string;
  host: string;
  subjectId: string;
  handle: string;
  method: LinkMethod;
  /// One statement per workspace (and one for the contact card if chosen).
  statements: { workspaceId: string; workspaceName: string; statement: string }[];
  signCommand: string;
  namespace: string;
  /// Keys the forge lists for you that this machine's ssh-agent holds.
  agentKeys: string[];
  /// The `ssh-ed25519 …` line to add as a signing key (published-key method).
  accountKeyOpenssh: string | null;
}

export interface SelfHostedForge {
  provider: string;
  host: string;
  baseUrl: string;
}
export interface LinkSettingsDto {
  checkForges: boolean;
  forges: SelfHostedForge[];
}

export const identityLinkPrepare = (args: {
  provider: string;
  host: string;
  handle: string;
  workspaceIds: string[];
  method: LinkMethod;
  discoverable: boolean;
}) => invoke<LinkDraftDto>("identity_link_prepare", args);
export const identityLinkSignWithAgent = (linkId: string, publicKey: string) =>
  invoke<Record<string, string>>("identity_link_sign_with_agent", { linkId, publicKey });
export const identityLinkPublish = (linkId: string, signatures: Record<string, string>) =>
  invoke<{ published: string[]; status: LinkStatus }>("identity_link_publish", { linkId, signatures });
export const myIdentityLinks = () => invoke<LinkDto[]>("my_identity_links");
export const identityLinkStatuses = (accountId: string, force: boolean) =>
  invoke<LinkDto[]>("identity_link_statuses", { accountId, force });
export const identityLinkRemove = (linkId: string) => invoke<number>("identity_link_remove", { linkId });
export const identityLinkSettings = () => invoke<LinkSettingsDto>("identity_link_settings");
export const setIdentityLinkSettings = (settings: LinkSettingsDto) =>
  invoke<void>("set_identity_link_settings", { settings });

export interface DmDto {
  workspaceId: string;
  /// The other person's account id.
  account: string;
  name: string;
  avatarUrl: string | null;
  safetyCode: string | null;
  /// Earlier DMs with the same person, archived (read-only) because both of
  /// you opened one at the same time.
  archived: string[];
  /// The DM's invite while this device holds it for delivery.
  inviteDelivery: InviteDelivery | null;
}
export const listDms = () => invoke<DmDto[]>("list_dms");

/// A DM invite this device holds until the other person's contact id is
/// known, then delivers once. `waiting` (no contact id yet), `warning` (their
/// keys changed since: held until you confirm), `sending`, `delivered`,
/// `joined` (they used the link you shared), `expired`, `dismissed`, `failed`.
export interface InviteDelivery {
  state: "waiting" | "warning" | "sending" | "delivered" | "joined" | "expired" | "dismissed" | "failed";
  pending: boolean;
  delivered: boolean;
  deliveredAt: number | null;
  expiresAt: number;
  /// For `warning`: why, starting with "unverified recipient".
  warning: string | null;
}

export interface OpenDmDto {
  workspaceId: string;
  /// The DM invite, on creation. Hand it over yourself unless `delivered`.
  invite: string | null;
  delivered: boolean;
  /// Not delivered yet, but held: delivered once their device is reachable.
  pending: boolean;
  inviteDelivery: InviteDelivery | null;
  safetyCode: string | null;
}
/// Open (creating on first use) the DM with a co-member and switch to it.
export const openDm = (accountId: string) => invoke<OpenDmDto>("open_dm", { accountId });
/// The link of a DM invite held for delivery (to send it yourself meanwhile).
export const heldDmInvite = (workspaceId: string) => invoke<string>("held_dm_invite", { workspaceId });
/// After the unverified-recipient warning: deliver to their current keys.
/// `safetyCode` is the code shown and compared; refused if theirs changed
/// since.
export const confirmDmInvite = (workspaceId: string, safetyCode: string) =>
  invoke<InviteDelivery>("confirm_dm_invite", { workspaceId, safetyCode });
/// Stop waiting to deliver (the link still works by hand).
export const dismissDmInvite = (workspaceId: string) => invoke<InviteDelivery | null>("dismiss_dm_invite", { workspaceId });
/// A held DM invite changed (delivered, expired, warning…).
export const onDmInviteChanged = (cb: (e: { workspaceId: string; inviteDelivery: InviteDelivery }) => void): Promise<UnlistenFn> =>
  listen<{ workspaceId: string; inviteDelivery: InviteDelivery }>("dm-invite-changed", (evt) => cb(evt.payload));

export interface CoMemberInviteDto {
  invite: string;
  name: string;
  safetyCode: string | null;
  devices: number;
  /// Delivered to their drop box, sealed to their verified devices.
  delivered: boolean;
}
/// Invite a co-member to another workspace you administer.
export const coMemberInvite = (workspaceId: string, accountId: string) =>
  invoke<CoMemberInviteDto>("co_member_invite", { workspaceId, accountId });

/// `known`: a co-member and the key matches. `mismatch`: a co-member with a
/// different key (not from them). `unknown`: compare the safety code out of
/// band. `none`: the invite carries no safety code.
export type InviteTrust = "known" | "claimed" | "mismatch" | "unknown" | "none";

export interface InviteInfoDto {
  workspaceName: string;
  isDm: boolean;
  addressedToMe: boolean;
  founderAccount: string | null;
  founderName: string | null;
  safetyCode: string | null;
  trust: InviteTrust;
  alreadyJoined: boolean;
}
/// What an invite says about who sent it, before joining.
export const inspectInvite = (code: string) => invoke<InviteInfoDto>("inspect_invite", { code });
/// Join a DM from its invite; `confirmSafetyCode` after comparing codes with
/// someone you share no workspace with.
export const joinDm = (code: string, confirmSafetyCode: boolean) =>
  invoke<WorkspaceInfoDto>("join_dm", { code, confirmSafetyCode });

export const listAgents = (sessionId: string) =>
  invoke<WorkspaceAgentDto[]>("list_agents", { sessionId });

export const addAgent = (
  sessionId: string,
  name: string,
  runtimeId: string,
  role: string,
) => invoke<void>("add_agent", { sessionId, name, runtimeId, role });

export const removeAgent = (sessionId: string, agentId: string) =>
  invoke<void>("remove_agent", { sessionId, agentId });

// ── Queued work (spec §12.5): unanswered agent mentions + host status ────────
export type { QueuedWorkDto } from "@/bindings/QueuedWorkDto";
export type { WorkspaceHostDto } from "@/bindings/WorkspaceHostDto";

/// Every unanswered agent mention in the active workspace, each tagged with its
/// agent's host status ("online" | "offline" | "device" | "unknown").
export const listQueuedWork = () => invoke<QueuedWorkDto[]>("list_queued_work");

/// The active workspace's registered hosts (devices + workers) with live state.
export const listWorkspaceHosts = () =>
  invoke<WorkspaceHostDto[]>("list_workspace_hosts");

/// Reassign an agent to a host — "run on worker instead". Empty host = owner's
/// device.
export const setAgentHost = (agentId: string, hostId: string) =>
  invoke<void>("set_agent_host", { agentId, hostId });

// ── Mention highlights: chats where the local user was @-mentioned ───────────
export type { MentionStateDto } from "@/bindings/MentionStateDto";

/// Every chat in the active workspace where the local user has been mentioned,
/// with the position of the last mention. The sidebar compares it against a
/// per-chat read cursor (localStorage) to highlight unread mentions.
export const listMentionStates = () =>
  invoke<MentionStateDto[]>("list_mention_states");

/// Self-mention state across every known workspace (each DTO tagged with its
/// `workspaceId`), so the workspace switcher can flag a background workspace
/// holding an unread self-mention. Grouped client-side by `workspaceId`.
export const listAllMentionStates = () =>
  invoke<MentionStateDto[]>("list_all_mention_states");

export const listSkills = (sessionId: string) =>
  invoke<SkillDto[]>("list_skills", { sessionId });

export const addSkillInline = (
  sessionId: string,
  name: string,
  instructions: string,
  agentIds: string[] = [],
) => invoke<void>("add_skill_inline", { sessionId, name, instructions, agentIds });

export const installSkill = (
  sessionId: string,
  name: string,
  source: string,
  agentIds: string[] = [],
) => invoke<void>("install_skill", { sessionId, name, source, agentIds });

export const removeSkill = (sessionId: string, skillId: string) =>
  invoke<void>("remove_skill", { sessionId, skillId });

export const listMcpServers = () => invoke<McpServerDto[]>("list_mcp_servers");

export const installMcpServer = (source: string) =>
  invoke<void>("install_mcp_server", { source });

export const removeMcpServer = (serverId: string) =>
  invoke<void>("remove_mcp_server", { serverId });

export const setMcpEnabled = (serverId: string, enabled: boolean) =>
  invoke<void>("set_mcp_enabled", { serverId, enabled });

export const listProposals = (sessionId: string) =>
  invoke<ProposalDto[]>("list_proposals", { sessionId });

export const createProposal = (
  sessionId: string,
  title: string,
  body: string,
  kind: string,
  requiredApprovals: number,
) => invoke<void>("create_proposal", { sessionId, title, body, kind, requiredApprovals });

export const voteProposal = (sessionId: string, proposalId: string, approved: boolean) =>
  invoke<ProposalDto | null>("vote_proposal", { sessionId, proposalId, approved });

/// Append a refinement note (comment or change request) to a proposal's thread.
export const refineProposal = (
  sessionId: string,
  proposalId: string,
  text: string,
  requestChanges: boolean,
) => invoke<ProposalDto | null>("refine_proposal", { sessionId, proposalId, text, requestChanges });

/// Send a proposal back to the agent to revise per the thread's feedback; the
/// agent produces a linked new version and this one is marked superseded.
export const requestProposalChanges = (sessionId: string, proposalId: string) =>
  invoke<void>("request_proposal_changes", { sessionId, proposalId });

/// Hide (or restore) proposals in the Review inbox. Hides only — the proposals
/// stay in the event log as a paper trail. Resolves to the number changed.
export const dismissProposals = (
  sessionId: string,
  proposalIds: string[],
  dismissed: boolean,
) => invoke<number>("dismiss_proposals", { sessionId, proposalIds, dismissed });

export const toggleReaction = (sessionId: string, messageId: string, emoji: string) =>
  invoke<void>("toggle_reaction", { sessionId, messageId, emoji });

export const listMembers = (sessionId: string) =>
  invoke<WorkspaceMemberDto[]>("list_members", { sessionId });

export const addMember = (sessionId: string, displayName: string, role: string, title: string) =>
  invoke<void>("add_member", { sessionId, displayName, role, title });

export const removeMember = (sessionId: string, memberId: string) =>
  invoke<void>("remove_member", { sessionId, memberId });

export interface RevokeResultDto {
  /** Whether the workspace key was rotated (false if no relay / no recipients). */
  rotated: boolean;
  recipients: number;
}
/// Remove a member AND rotate the workspace key so they lose read access to new
/// messages (owner/admin only). Rotation is sealed to the remaining members.
export const removeAndRevoke = (sessionId: string, memberId: string) =>
  invoke<RevokeResultDto>("remove_and_revoke", { sessionId, memberId });

// ── My devices (a `ligus serve` box enrolled as one of your devices) ─────────

export interface DeviceCodeDto {
  deviceId: string;
  label: string;
  /** Compare with the box's terminal (`ligus serve enroll` prints it). */
  fingerprint: string;
  knownWorkspace: boolean;
  alreadyMine: boolean;
  revoked: boolean;
  /** The box's own pre-enrollment member, retired on approval if asked. */
  replacesMember: { memberId: string; displayName: string } | null;
}
export interface MyDeviceDto {
  deviceId: string;
  label: string;
  fingerprint: string;
  thisDevice: boolean;
  revoked: boolean;
  lastSeen: string | null;
}
export interface DeviceRequestDto {
  hostId: string;
  label: string;
}
export interface ApproveDeviceDto {
  retired: string | null;
  retireError: string | null;
}
/// Parse an enrollment code (`hivedev1:…`) to show before approving it.
export const inspectDeviceCode = (code: string) => invoke<DeviceCodeDto>("inspect_device_code", { code });
/// Approve a box as one of your devices (signs its certificate with your
/// account key); `retireMember` also removes its separate member.
export const approveDevice = (code: string, retireMember: boolean) =>
  invoke<ApproveDeviceDto>("approve_device", { code, retireMember });
export const listMyDevices = () => invoke<MyDeviceDto[]>("list_my_devices");
/// Hosts naming you as their owner that you haven't approved yet.
export const listDeviceRequests = () => invoke<DeviceRequestDto[]>("list_device_requests");
/// Revoke one of your devices: every member stops accepting its events.
export const revokeMyDevice = (deviceId: string) => invoke<void>("revoke_my_device", { deviceId });

export type RevocationStepState = "pending" | "done" | "failed" | "not_applicable";
export interface RevocationStep {
  state: RevocationStepState;
  detail?: string;
  at: number;
  attempts: number;
}
/// What one serve box did about a revoked phone device (its sealed report).
export interface DeviceRevocationDto {
  hostId: string;
  hostLabel: string;
  reportedAt: number;
  /** "revoked: tunnel cut, push removed, session ended" (or what's left). */
  summary: string;
  complete: boolean;
  deviceId: string;
  name: string;
  revokedAt: number;
  tunnel: RevocationStep;
  push: RevocationStep;
  session: RevocationStep;
}
/// Phone revocation status reported by the boxes the phone was paired with.
export const listDeviceRevocations = () => invoke<DeviceRevocationDto[]>("list_device_revocations");
/// Ask those boxes to retry the unfinished steps now.
export const retryDeviceRevocation = (deviceId: string) => invoke<void>("retry_device_revocation", { deviceId });

export const setMemberRole = (sessionId: string, memberId: string, role: string) =>
  invoke<void>("set_member_role", { sessionId, memberId, role });

export const listVaults = (sessionId: string) =>
  invoke<VaultSourceDto[]>("list_vaults", { sessionId });

export const addVault = (
  sessionId: string,
  kind: string,
  reference: string,
  agentIds: string[] = [],
) => invoke<void>("add_vault", { sessionId, kind, reference, agentIds });

export const removeVault = (sessionId: string, url: string) =>
  invoke<void>("remove_vault", { sessionId, url });

export const previewVault = (url: string) => invoke<string>("preview_vault", { url });

/// Subscribe to streaming chat updates. Returns an unlisten function.
export const onChatStream = (
  cb: (e: ChatStreamEvent) => void,
): Promise<UnlistenFn> =>
  listen<ChatStreamEvent>("chat://stream", (evt) => cb(evt.payload));

/// Subscribe to live background-activity (tool calls, results, thinking) for a
/// running subprocess-agent turn. Ephemeral — shown under the generating bubble.
export const onChatActivity = (
  cb: (e: ChatActivityEvent) => void,
): Promise<UnlistenFn> =>
  listen<ChatActivityEvent>("chat://activity", (evt) => cb(evt.payload));

export type { ChatActivityEvent };

/// Real connection health for the relay sync loop, updated on every sync
/// attempt: `live` (last sync ok), `limited` (syncing, but the relay refused
/// this workspace's writes, e.g. "workspace full", or the room is truncated;
/// see `lastError`), `error` (last sync failed — see `lastError`), or
/// `offline` (not syncing / no relay).
export type ConnectionState = "live" | "limited" | "error" | "offline" | "needs_key";

export interface SyncStatusDto {
  relayConfigured: boolean;
  relayUrl: string;
  room: string;
  encrypted: boolean;
  /// Live health, so the UI reflects a dead relay / bad key instead of just
  /// "a URL is set".
  connectionState: ConnectionState;
  /// Actionable message from the last failed sync (null when healthy).
  lastError: string | null;
}

export const syncStatus = () => invoke<SyncStatusDto>("sync_status");

/// Whether sending in a chat waits for its workspace's founder (their signed
/// founding hasn't synced to this device yet). A state, not a failure.
export interface FounderStatusDto {
  waiting: boolean;
  founderName: string | null;
}
export const founderStatus = (sessionId: string) => invoke<FounderStatusDto>("founder_status", { sessionId });

export type RelayProbeStatus =
  | "ok"
  | "unauthorized"
  | "httpError"
  | "unreachable"
  | "unconfigured";

export interface RelayProbeDto {
  status: RelayProbeStatus;
  detail: string;
}

/// Actually hit the configured relay and report reachability + auth, as opposed
/// to `sync_status` which only reflects whether a URL is set.
export const probeRelay = () => invoke<RelayProbeDto>("probe_relay");

/// Probe a URL + token being entered (onboarding/settings), before saving it.
export const probeRelayAt = (url: string, accessToken: string | null) =>
  invoke<RelayProbeDto>("probe_relay_at", { url, accessToken });

export interface WorkspaceInfoDto {
  id: string;
  name: string;
  /** "local" | "room" */
  kind: string;
  active: boolean;
  /** Optional workspace icon as a `data:` URL; rail renders it over initials. */
  iconUrl?: string | null;
}

/// The selectable workspaces: "My workspace" (local) + any joined relay room.
export const listWorkspaces = () => invoke<WorkspaceInfoDto[]>("list_workspaces");

/// Scope the chat list to a workspace (local id or a joined room id).
export const setActiveWorkspace = (workspaceId: string) =>
  invoke<void>("set_active_workspace", { workspaceId });

/// Point the effective code dir at a chat's isolated worktree (or `null` to
/// revert to the active workspace's folder). No-op fallback on a non-git root.
export const setActiveChat = (sessionId: string | null) =>
  invoke<void>("set_active_chat", { sessionId });

/// Set a workspace icon (a `data:image/…` URL), or clear it with `null`.
export const setWorkspaceIcon = (workspaceId: string, icon: string | null) =>
  invoke<void>("set_workspace_icon", { workspaceId, icon });

/// Create a new team workspace (generates a room + E2EE key), switch to it.
/// `relayUrl`: the relay the creator picked ("" = none); omitted keeps the
/// currently configured relay.
export const createWorkspace = (name: string, relayUrl?: string) =>
  invoke<WorkspaceInfoDto>("create_workspace", { name, relayUrl: relayUrl ?? null });

/// Join a team workspace from an invite code (`hivews1:…`).
export const joinWorkspace = (invite: string) =>
  invoke<WorkspaceInfoDto>("join_workspace", { invite });

/// Shareable invite code for a workspace (bundles relay + room + key).
export const workspaceInvite = (workspaceId: string) =>
  invoke<string>("workspace_invite", { workspaceId });

/// A box's join request (`ligusjoin1:…` from `ligus admit request`), read
/// so the Admin can compare its fingerprint with the box's own.
export type BoxRequest = { fingerprint: string; label: string; accountId: string };
export const inspectBoxRequest = (request: string) => invoke<BoxRequest>("inspect_box_request", { request });

/// A box admission: `ligusadmit1:…` sealed to the box that asked, or a bearer
/// `hivews1:…` code for a Kubernetes Secret. Single use, expiring.
export type BoxAdmission = { code: string; sealed: boolean; expiresInMinutes: number };
/// Admit a `ligus serve` / `ligus worker` box as its own service member
/// (Admins; confirmed with a passkey).
export const admitBox = (workspaceId: string, request: string | null, ttlMinutes: number | null) =>
  invoke<BoxAdmission>("admit_box", { workspaceId, request, ttlMinutes });

/// An invite delivered to this device's contact drop box (sealed to it,
/// opened locally).
export type PendingInvite = {
  relay: string;
  seq: number;
  isDm: boolean;
  workspaceName: string;
  founderAccount: string | null;
  founderName: string | null;
  safetyCode: string | null;
  trust: InviteTrust;
  code: string;
};
export type PendingInvites = {
  /// "on" | "unsupported" (the relay has no contact routes: invites come as
  /// links and QR codes only) | "noRelay".
  contacts: string;
  invites: PendingInvite[];
};

/// Refresh this device's contact and read its drop boxes.
export const listPendingInvites = () => invoke<PendingInvites>("list_pending_invites");

/// Accept a delivered invite (a DM from someone you share no workspace with
/// needs `confirmSafetyCode` after comparing codes).
export const acceptPendingInvite = (invite: PendingInvite, confirmSafetyCode = false) =>
  invoke<WorkspaceInfoDto>("accept_pending_invite", {
    relay: invite.relay,
    seq: invite.seq,
    code: invite.code,
    confirmSafetyCode,
  });

/// Dismiss a delivered invite (deleted on the relay at the next read).
export const dismissPendingInvite = (invite: PendingInvite) =>
  invoke<void>("dismiss_pending_invite", { relay: invite.relay, seq: invite.seq });

/// Leave a team workspace (removes it from the rail).
export const removeWorkspace = (workspaceId: string) =>
  invoke<void>("remove_workspace", { workspaceId });

export type ClaudePermissionMode =
  | "default"
  | "acceptEdits"
  | "runCommands"
  | "bypassPermissions";

export interface ConnectionSettingsDto {
  relayUrl: string;
  room: string;
  hasWorkspaceKey: boolean;
  hasApiKey: boolean;
  /// Whether a relay access token (for a token-gated relay) is set.
  hasRelayAccessToken: boolean;
  permissionMode: ClaudePermissionMode;
}

export const getConnectionSettings = () =>
  invoke<ConnectionSettingsDto>("get_connection_settings");

/// Update connection settings. For `workspaceKey`/`apiKey`/`relayAccessToken`:
/// pass `null` to leave unchanged, "" to clear, or a value to set (secrets
/// aren't echoed back).
export const updateConnectionSettings = (args: {
  relayUrl: string;
  room: string;
  workspaceKey: string | null;
  apiKey: string | null;
  relayAccessToken: string | null;
  permissionMode: ClaudePermissionMode;
}) => invoke<ConnectionSettingsDto>("update_connection_settings", args);

/// Fires when the background relay sync applies remote events.
export const onWorkspaceSynced = (cb: () => void): Promise<UnlistenFn> =>
  listen("workspace://synced", () => cb());

/// Fires when the background relay sync fails. The payload is an actionable
/// error message (e.g. "unauthorized — check your relay token"), mirrored into
/// `sync_status`'s `lastError` / `connectionState: "error"`.
export const onSyncError = (cb: (message: string) => void): Promise<UnlistenFn> =>
  listen<string>("workspace://sync-error", (evt) => cb(evt.payload));

/// Fires once when this device syncs a removal of the local user from the active
/// workspace (an owner/admin removed them). Payload is the workspace id.
export const onWorkspaceRemoved = (cb: (workspaceId: string) => void): Promise<UnlistenFn> =>
  listen<string>("workspace://removed", (evt) => cb(evt.payload));

/// Fires when a system-tray menu item asks the UI to navigate. The payload is a
/// route string: `"friends"`, `"settings"`, or `"settings:<Tab>"`.
export const onTrayNavigate = (cb: (route: string) => void): Promise<UnlistenFn> =>
  listen<string>("tray://navigate", (evt) => cb(evt.payload));

/// Tint the native title bar to match the app background (Windows 11; no-op
/// elsewhere). Called by `applyTheme` on launch and every theme change.
export const setTitlebarColor = (r: number, g: number, b: number, dark: boolean) =>
  invoke<void>("set_titlebar_color", { r, g, b, dark });

/// Mirror the theme to the app data dir so `ligus serve` on this machine can
/// tell a paired phone which theme to follow ("Match host").
export const saveAppearance = (palette: string, mode: string) =>
  invoke<void>("save_appearance", { palette, mode });

/// Factory reset: wipe all local data (chats, identity, keys, settings,
/// workspaces) and relaunch the app. The backend defers the actual deletion to
/// the next launch (the DB file is open/locked) and restarts immediately.
export const resetLocalData = () => invoke<void>("reset_local_data");

/// Scheduled / triggered agents (Settings → Schedules).
export type ScheduleTrigger =
  | { kind: "interval"; every_secs: number }
  | { kind: "daily_at"; hour: number; minute: number };

export interface ScheduledAgent {
  id: string;
  enabled: boolean;
  label: string;
  workspaceId: string | null;
  runtimeId: string;
  prompt: string;
  trigger: ScheduleTrigger;
  lastRun: string | null;
}

export const listSchedules = () => invoke<ScheduledAgent[]>("list_schedules");
export const addSchedule = (args: {
  label: string;
  prompt: string;
  runtimeId?: string;
  workspaceId?: string;
  trigger: ScheduleTrigger;
}) => invoke<ScheduledAgent>("add_schedule", args);
export const removeSchedule = (id: string) => invoke<void>("remove_schedule", { id });
export const setScheduleEnabled = (id: string, enabled: boolean) =>
  invoke<void>("set_schedule_enabled", { id, enabled });

/// `/summarize` — post a model summary of the chat (transcript kept intact).
export const summarizeChat = (sessionId: string) => invoke<void>("summarize_chat", { sessionId });
/// `/compact` — collapse the conversation into a single summary checkpoint.
export const compactChat = (sessionId: string) => invoke<void>("compact_chat", { sessionId });

/// Add a remote HTTP MCP server by URL (e.g. a hosted server like Linear's).
/// `bearerToken` (optional) is a static token stored on this device only;
/// omit it to leave any saved token alone.
export const addRemoteMcpServer = (id: string, url: string, bearerToken?: string) =>
  invoke<void>("add_remote_mcp_server", { id, url, bearerToken });
/// Save (or clear, with "") a static bearer token for an HTTP MCP server. The
/// token stays on this device and is never read back — `McpServerDto` only
/// reports `hasBearerToken`. Resolves to whether a token is now set.
export const setMcpBearerToken = (serverId: string, token: string) =>
  invoke<boolean>("set_mcp_bearer_token", { serverId, token });
/// Connect a remote MCP server via OAuth 2.1 + PKCE (opens the browser).
export const authorizeMcpServer = (serverId: string, scope?: string) =>
  invoke<void>("authorize_mcp_server", { serverId, scope });

/// Carry out an approved proposal (agreement-gated): a structured command
/// runs its exact argv; anything else goes to the producing agent (or
/// `targetAgentId`) as the exact instruction the preview shows.
export const implementProposal = (sessionId: string, proposalId: string, targetAgentId?: string | null) =>
  invoke<void>("implement_proposal", { sessionId, proposalId, targetAgentId: targetAgentId ?? null });

/// "Apply on ‹host›": ask one host to apply an approved proposal. That host
/// applies it once and reports back; nobody else applies it meanwhile.
/// Returns the request id.
export const requestApplyOnHost = (sessionId: string, proposalId: string, hostDeviceId: string) =>
  invoke<string>("request_apply_on_host", { sessionId, proposalId, hostDeviceId });

/// Withdraw an open apply request (yours, or any as an Admin).
export const withdrawApplyRequest = (sessionId: string, requestId: string) =>
  invoke<void>("withdraw_apply_request", { sessionId, requestId });

/// What Implement will do, exactly (built by the same function that runs it).
export const getImplementPlan = (sessionId: string, proposalId: string, targetAgentId?: string | null) =>
  invoke<ImplementPreviewDto>("get_implement_plan", { sessionId, proposalId, targetAgentId: targetAgentId ?? null });

/// An agent's question to the chat's people (ligus_ask_human).
export type AskDto = {
  askId: string;
  agentName: string;
  question: string;
  options: string[];
  expiresAtMs: number;
  answer?: string | null;
  answeredBy?: string | null;
  open: boolean;
};

/// The agents' questions in a chat, newest first.
export const listAsks = (sessionId: string) => invoke<AskDto[]>("list_asks", { sessionId });

/// Answer an agent's question (the first answer wins; it authorizes nothing).
export const answerAsk = (sessionId: string, askId: string, answer: string, optionIndex?: number | null) =>
  invoke<void>("answer_ask", { sessionId, askId, answer, optionIndex: optionIndex ?? null });

/// Check for an app update. Resolves to the new version if one is available,
/// null if up to date; rejects with a friendly message if the updater isn't
/// configured yet (#144).
export const checkForUpdate = () => invoke<string | null>("check_for_update");

/// Download + install the latest signed update in place and relaunch (VSCode
/// style). Resolves by relaunching on success; rejects if the updater isn't
/// configured / no signed release yet — so the caller can fall back to the
/// release page.
export const installUpdate = () => invoke<void>("install_update");

/// Download progress for `installUpdate`, streamed from the updater.
export interface UpdateProgress {
  downloaded?: number;
  total?: number | null;
  done?: boolean;
}
export const onUpdateProgress = (cb: (p: UpdateProgress) => void): Promise<UnlistenFn> =>
  listen<UpdateProgress>("update://progress", (e) => cb(e.payload));

/// Linear context source (#145 L3): pull issues from the connected Linear MCP
/// server as a block to insert into the composer.
export const linearIssuesContext = () => invoke<string>("linear_issues_context");

/// Store a remote MCP server's OAuth client credentials before connecting
/// (e.g. a Linear OAuth app's Client ID + secret). Secret stays local.
export const setMcpOauthClient = (serverId: string, clientId: string, clientSecret?: string) =>
  invoke<void>("set_mcp_oauth_client", { serverId, clientId, clientSecret });

/// Lightweight "is a newer published version available?" check (independent of
/// the auto-updater). Returns the update info, or null when current / dev / offline.
export interface AppUpdateInfo { tag: string; name: string; url: string; notes: string }
export const checkForAppUpdate = () => invoke<AppUpdateInfo | null>("check_for_app_update");

/// Selected Claude Code CLI model alias (`--model`); empty = the CLI default.
export const getClaudeCodeModel = () => invoke<string>("get_claude_code_model");
export const setClaudeCodeModel = (model: string) =>
  invoke<void>("set_claude_code_model", { model });

/// A model to offer for the local Claude Code CLI. `value` is passed to
/// `--model` verbatim (empty = the CLI's own default).
export interface ClaudeModelOption {
  value: string;
  label: string;
  description: string | null;
}
/// Base aliases merged with whatever Claude Code has cached as available for
/// this account (Fable, …), so the picker mirrors the CLI's own `/model` list.
/// Falls back to the base aliases if the CLI cache isn't present.
export const listClaudeCodeModels = () =>
  invoke<ClaudeModelOption[]>("list_claude_code_models");

/// Set the Primary Runtime's model (the default Anthropic model shown in
/// Settings → Models). Persisted; applies immediately.
export const setDefaultModel = (model: string) =>
  invoke<void>("set_default_model", { model });

/// Set which runtime new chats default to (the per-row "Set default" in
/// Settings → Models). Persisted; drives `isDefault` in listRuntimes.
export const setDefaultRuntime = (runtimeId: string) =>
  invoke<void>("set_default_runtime", { runtimeId });

// --- Agentic workflows -------------------------------------------------------

export type { WorkflowDefinitionDto } from "@/bindings/WorkflowDefinitionDto";
export type { WorkflowNodeDto } from "@/bindings/WorkflowNodeDto";
export type { WorkflowRunDto } from "@/bindings/WorkflowRunDto";
export type { WorkflowNodeRunDto } from "@/bindings/WorkflowNodeRunDto";

export const listWorkflows = (sessionId: string) =>
  invoke<WorkflowDefinitionDto[]>("list_workflows", { sessionId });

export const removeWorkflow = (sessionId: string, workflowId: string) =>
  invoke<void>("remove_workflow", { sessionId, workflowId });

/// Pause (`enabled: false`) or resume a workflow. A paused workflow stays
/// listed but can't start a run.
export const setWorkflowEnabled = (sessionId: string, workflowId: string, enabled: boolean) =>
  invoke<void>("set_workflow_enabled", { sessionId, workflowId, enabled });

/// The chat message for "Ask an agent to change this": the request plus the
/// current definition, marked so the agent's answer is filed as a revision.
/// Send it with `sendMessage`.
export const workflowChangeRequest = (sessionId: string, workflowId: string, request: string) =>
  invoke<string>("workflow_change_request", { sessionId, workflowId, request });

export const listWorkflowRuns = (sessionId: string) =>
  invoke<WorkflowRunDto[]>("list_workflow_runs", { sessionId });

/// Start a run; resolves to the run id. Stage turns stream into the chat.
export const startWorkflowRun = (sessionId: string, workflowId: string, input: string) =>
  invoke<string>("start_workflow_run", { sessionId, workflowId, input });

export const cancelWorkflowRun = (sessionId: string, runId: string) =>
  invoke<void>("cancel_workflow_run", { sessionId, runId });

/// Restart an interrupted run (e.g. after an app restart mid-run).
export const resumeWorkflowRun = (sessionId: string, runId: string) =>
  invoke<void>("resume_workflow_run", { sessionId, runId });

/// Hand a run this device drives to another online device (planned handover).
/// Resolves to the successor's device id.
export const handOverWorkflowRun = (sessionId: string, runId: string, successor?: string) =>
  invoke<string>("hand_over_workflow_run", { sessionId, runId, successor: successor ?? null });

export type { GuardSignalDto } from "@/bindings/GuardSignalDto";
export type { RunLeaseDto } from "@/bindings/RunLeaseDto";

/// Device-local guard signals (never synced; content-free), oldest first.
export const listGuardSignals = (afterRow?: number, kind?: string, limit?: number) =>
  invoke<import("@/bindings/GuardSignalDto").GuardSignalDto[]>("list_guard_signals", {
    afterRow: afterRow ?? null,
    kind: kind ?? null,
    limit: limit ?? null,
  });

// ── Workspace guards: dearth mode ──
export type { DearthStateDto } from "@/bindings/DearthStateDto";
export type { PendingJoinDto } from "@/bindings/PendingJoinDto";
export type { EntranceDto } from "@/bindings/EntranceDto";

export const getDearthState = () =>
  invoke<import("@/bindings/DearthStateDto").DearthStateDto>("get_dearth_state");

/// Switch dearth mode (Admin+). Lists are trimmed; empty entries dropped.
export const setDearthMode = (
  enabled: boolean,
  reason: string,
  broodPaths: string[],
  allowedMcpServers: string[],
) => invoke<void>("set_dearth_mode", { enabled, reason, broodPaths, allowedMcpServers });

/// Approve a member or device waiting on dearth mode's second approval.
export const approveJoin = (subject: string) => invoke<void>("approve_join", { subject });

// ── Egress presets (device-local, off by default) ──
export type { EgressPresetsDto } from "@/bindings/EgressPresetsDto";
export type { EgressPresetDto } from "@/bindings/EgressPresetDto";

/// Presets that exist, what the active workspace (and `sessionId`'s chat)
/// has on, and whether a guard overrides them for that chat.
export const getEgressPresets = (sessionId?: string | null) =>
  invoke<import("@/bindings/EgressPresetsDto").EgressPresetsDto>("get_egress_presets", { sessionId: sessionId ?? null });

/// Whether the active workspace's CLI agents use the user's own CLI setup
/// (Claude Code: user settings, hooks, plugins, skills) on this device.
export const getWorkspaceCliUserConfig = () => invoke<boolean>("get_workspace_cli_user_config");

/// Opt the active workspace's CLI agents into (or out of) the user's own
/// CLI setup on this device. Off by default.
export const setWorkspaceCliUserConfig = (enabled: boolean) => invoke<void>("set_workspace_cli_user_config", { enabled });

/// Turn presets on for the active workspace on this device (`[]` = off).
export const setWorkspaceEgressPresets = (presets: string[]) =>
  invoke<void>("set_workspace_egress_presets", { presets });

/// Override the workspace for one chat: `null` = follow the workspace,
/// `[]` = off for this chat.
export const setChatEgressPresets = (sessionId: string, presets: string[] | null) =>
  invoke<void>("set_chat_egress_presets", { sessionId, presets });

// ── Workspace guards: chat lockdown (self-medication) ──
export type { ChatLockdownDto } from "@/bindings/ChatLockdownDto";
export type { LockdownSignalDto } from "@/bindings/LockdownSignalDto";

export const getChatLockdown = (sessionId: string) =>
  invoke<import("@/bindings/ChatLockdownDto").ChatLockdownDto>("get_chat_lockdown", { sessionId });
/// Clear a chat's lockdown (a human member; re-checked on every device).
/// Clear exactly the lockdown signals the person was shown, each named by
/// its `raiseEventId`: one raised since stays in force.
export const clearChatLockdown = (sessionId: string, raiseIds: string[]) =>
  invoke<void>("clear_chat_lockdown", { sessionId, raiseIds });
/// Lock a chat down by hand.
export const lockChat = (sessionId: string) => invoke<void>("lock_chat", { sessionId });

// ── Workspace guards: hygiene ──
export type { HygieneReportDto } from "@/bindings/HygieneReportDto";
export const getHygieneReport = () =>
  invoke<import("@/bindings/HygieneReportDto").HygieneReportDto>("get_hygiene_report");
/// Sweep leftover worktrees and stale proposals now.
export const runHygieneNow = () =>
  invoke<import("@/bindings/HygieneReportDto").HygieneReportDto>("run_hygiene_now");

// ── Workspace guards: staggered throttles ──
export type { ThrottleStateDto } from "@/bindings/ThrottleStateDto";
export const getThrottleState = () =>
  invoke<import("@/bindings/ThrottleStateDto").ThrottleStateDto>("get_throttle_state");

// ── Nuc workspaces: growth suggestions from occupancy ──
export type { GrowthSuggestionDto } from "@/bindings/GrowthSuggestionDto";
export const growthSuggestions = () =>
  invoke<import("@/bindings/GrowthSuggestionDto").GrowthSuggestionDto[]>("growth_suggestions");

/// Fires whenever a workflow run's persisted state changes.
export const onWorkflowRun = (
  cb: (e: WorkflowRunEvent) => void,
): Promise<UnlistenFn> =>
  listen<WorkflowRunEvent>("workflow://run", (evt) => cb(evt.payload));

// ── Code editor + embedded terminal (feat/editor-view) ───────────────────────
// DTOs are declared inline here (not imported from @/bindings) to avoid a race
// with the backend package that generates those bindings; shapes match the
// IPC contract exactly.

/// One directory entry in the lazy workspace tree (`path` is repo-relative).
export interface FsEntryDto {
  name: string;
  path: string;
  isDir: boolean;
}
/// A chunk of PTY output for terminal `id` (UTF-8, lossy-decoded on the backend).
export interface TerminalOutput {
  id: string;
  data: string;
}
/// A terminal's child process exited; `code` is null when killed by a signal.
export interface TerminalExit {
  id: string;
  code: number | null;
}
/// Debounced batch of repo-relative paths the workspace watcher saw change.
export interface FsChanged {
  paths: string[];
}

/// One directory level of the workspace tree (lazy). `relDir` null/"" = root;
/// returns dirs-first, name-sorted, with `.git` skipped.
export const listWorkspaceTree = (relDir?: string | null) =>
  invoke<FsEntryDto[]>("list_workspace_tree", { relDir: relDir ?? null });

/// Overwrite or create a file (within root; parent dirs created as needed).
export const writeWorkspaceFile = (path: string, contents: string) =>
  invoke<void>("write_workspace_file", { path, contents });

/// Create a new file (`isDir=false`) or directory (`isDir=true`) within root.
export const createWorkspaceEntry = (path: string, isDir: boolean) =>
  invoke<void>("create_workspace_entry", { path, isDir });

/// Rename/move an entry within root.
export const renameWorkspaceEntry = (from: string, to: string) =>
  invoke<void>("rename_workspace_entry", { from, to });

/// Remove a file, or a directory recursively (within root only).
export const deleteWorkspaceEntry = (path: string) =>
  invoke<void>("delete_workspace_entry", { path });

/// Spawn a shell in a PTY (default cwd = workspace root); resolves to its id.
export const terminalOpen = (cwd: string | null, cols: number, rows: number) =>
  invoke<string>("terminal_open", { cwd: cwd ?? null, cols, rows });

/// Write bytes (as a string) to the PTY master.
export const terminalWrite = (id: string, data: string) =>
  invoke<void>("terminal_write", { id, data });

/// Resize the PTY viewport.
export const terminalResize = (id: string, cols: number, rows: number) =>
  invoke<void>("terminal_resize", { id, cols, rows });

/// Kill the child and drop the terminal.
export const terminalClose = (id: string) =>
  invoke<void>("terminal_close", { id });

/// Fires (debounced) when files under the workspace root change on disk.
export const onFsChanged = (cb: (e: FsChanged) => void): Promise<UnlistenFn> =>
  listen<FsChanged>("workspace://fs-changed", (evt) => cb(evt.payload));

/// Streaming PTY output for any open terminal (filter by `id` on the frontend).
export const onTerminalOutput = (
  cb: (e: TerminalOutput) => void,
): Promise<UnlistenFn> =>
  listen<TerminalOutput>("terminal://output", (evt) => cb(evt.payload));

/// Fires when a terminal's child process exits.
export const onTerminalExit = (cb: (e: TerminalExit) => void): Promise<UnlistenFn> =>
  listen<TerminalExit>("terminal://exit", (evt) => cb(evt.payload));

// ── Language servers (LSP) ───────────────────────────────────────────────────
// A thin bridge to the backend's LSP process manager. The backend owns the
// child processes + Content-Length framing; the frontend speaks JSON-RPC over
// these wrappers (see `@/lib/lsp`). DTOs are declared inline (not imported from
// `@/bindings`) to avoid a race with the backend package that generates them;
// the shapes match the IPC contract exactly.

/// One entry in the language-server registry. `available` is whether the
/// server's command was resolvable on PATH at discovery time.
export interface LspServerDto {
  id: string;
  language: string;
  command: string;
  available: boolean;
}

/// The registry of known language servers with their PATH availability.
export const lspServers = () => invoke<LspServerDto[]>("lsp_servers");

/// Start (or reuse a running) session for a server id; resolves to its session
/// id. Rejects if the server isn't available.
export const lspStart = (serverId: string) =>
  invoke<string>("lsp_start", { serverId });

/// Send one complete JSON-RPC message string to a session's stdin (the backend
/// prepends the Content-Length framing).
export const lspSend = (sessionId: string, body: string) =>
  invoke<void>("lsp_send", { sessionId, body });

/// Kill a session's child process and drop it.
export const lspStop = (sessionId: string) =>
  invoke<void>("lsp_stop", { sessionId });

/// One inbound JSON-RPC message (raw JSON string) from a language server.
export interface LspMessage {
  sessionId: string;
  body: string;
}
/// A language-server process exited; `code` is null when killed by a signal.
export interface LspExit {
  sessionId: string;
  code: number | null;
}

/// Subscribe to inbound JSON-RPC messages from any language server (filter by
/// `sessionId` on the frontend). Returns an unlisten function.
export const onLspMessage = (cb: (e: LspMessage) => void): Promise<UnlistenFn> =>
  listen<LspMessage>("lsp://message", (evt) => cb(evt.payload));

/// Subscribe to language-server process exits. Returns an unlisten function.
export const onLspExit = (cb: (e: LspExit) => void): Promise<UnlistenFn> =>
  listen<LspExit>("lsp://exit", (evt) => cb(evt.payload));

// ── Proposal sets ───────────────────────────────────────────────────────────
// Competing alternatives scored blind by agent scouts; a person decides after
// a pre-flight (see the Review pane's set view).

export type { ProposalSetDto } from "@/bindings/ProposalSetDto";
export type { SetAlternativeDto } from "@/bindings/SetAlternativeDto";

export const listProposalSets = (sessionId: string) =>
  invoke<ProposalSetDto[]>("list_proposal_sets", { sessionId });

/// Send one scout turn per alternative that still needs assessments this round.
/// Resolves to the number of scout turns sent.
export const sendScouts = (sessionId: string, setId: string) =>
  invoke<number>("send_scouts", { sessionId, setId });

/// The "fast ↔ careful" dial, or explicit knobs.
export const tuneProposalSet = (
  sessionId: string,
  setId: string,
  knobs: { dial?: string; agentQuorum?: number; requiredApprovals?: number; qualityBar?: number },
) =>
  invoke<ProposalSetDto>("tune_proposal_set", {
    sessionId,
    setId,
    dial: knobs.dial ?? null,
    agentQuorum: knobs.agentQuorum ?? null,
    requiredApprovals: knobs.requiredApprovals ?? null,
    qualityBar: knobs.qualityBar ?? null,
  });

/// New evidence: advance the round so scout support decays and scouts re-evaluate.
export const advanceProposalSetRound = (sessionId: string, setId: string) =>
  invoke<ProposalSetDto>("advance_proposal_set_round", { sessionId, setId });

export const withdrawSetAlternative = (sessionId: string, setId: string, proposalId: string) =>
  invoke<ProposalSetDto>("withdraw_set_alternative", { sessionId, setId, proposalId });

export const closeProposalSet = (sessionId: string, setId: string) =>
  invoke<ProposalSetDto>("close_proposal_set", { sessionId, setId });

/// Phase one of the two-phase commit: run and record the pre-flight.
export const preflightProposalSet = (sessionId: string, setId: string, proposalId: string) =>
  invoke<ProposalSetDto>("preflight_proposal_set", { sessionId, setId, proposalId });

/// Phase two: apply the alternative whose pre-flight passed and decide the set.
export const applyProposalSet = (sessionId: string, setId: string, proposalId: string) =>
  invoke<ProposalSetDto>("apply_proposal_set", { sessionId, setId, proposalId });

/// Revert an applied file change (reverse patch + commit in the chat's worktree).
export const revertProposal = (sessionId: string, proposalId: string) =>
  invoke<void>("revert_proposal", { sessionId, proposalId });

// ── S1: trust this workspace ──
/// A runtime or MCP server the open repository ships. Inert until this
/// device enables its exact digest (a changed command is a new digest).
export type WorkspaceTrustItem = {
  kind: "mcpServer" | "runtime";
  id: string;
  commandLine: string;
  source: string;
  digest: string;
  enabled: boolean;
};
export type WorkspaceTrust = { root: string; items: WorkspaceTrustItem[] };

export const getWorkspaceTrust = () => invoke<WorkspaceTrust>("get_workspace_trust");
export const setWorkspaceTrust = (digest: string, enabled: boolean) =>
  invoke<WorkspaceTrust>("set_workspace_trust", { digest, enabled });
export const trustWorkspace = () => invoke<WorkspaceTrust>("trust_workspace");

// ── Passkeys and workspace keys (Phase 0, Track B) ─────────────────────────

export type PasskeyDto = { credentialId: string; recovery: boolean };
export type SecurityStatusDto = {
  passkeys: PasskeyDto[];
  /** Workspace rules version (2 = passkey presence enforced). */
  rules: number;
  /** The newest authorized workspace key version (0 = the passphrase key). */
  keyVersion: number;
  /** Where this device keeps Ligus's secrets ("keychain", "file", …). */
  secretBackend: string;
  /// This device counts on the workspace (rules v2: a new device of an
  /// account with a passkey needs Confirm this device).
  deviceConfirmed: boolean;
};
/** What the "Confirm with passkey" sheet shows while the browser is open. */
export type PresencePromptDto = { kind: string; summary: string; digestShort: string; op: "get" | "create" };

export const securityStatus = () => invoke<SecurityStatusDto>("security_status");
export const passkeyRegister = (label: string) => invoke<void>("passkey_register", { label });
export const recoveryKeyCreate = () => invoke<string>("recovery_key_create");
export const passkeyRemove = (credentialId: string) => invoke<void>("passkey_remove", { credentialId });
export const workspaceRulesSet = (version: number) => invoke<void>("workspace_rules_set", { version });
/** Confirm this device on the active workspace with your passkey (rules v2). */
export const deviceConfirm = () => invoke<boolean>("device_confirm");
/** Let a member register a first passkey again (an Admin's re-grant, confirmed with your passkey). */
export const passkeyRegrant = (memberId: string) => invoke<void>("passkey_regrant", { memberId });
export const rotateWorkspaceKey = () => invoke<{ rotated: boolean; recipients: number }>("rotate_workspace_key");
export const presenceCancel = () => invoke<void>("presence_cancel");
/** Confirm the pending request with the recovery key's 24 words (signed on this device, never stored). */
export const presenceUseRecovery = (words: string) => invoke<void>("presence_use_recovery", { words });
export const onPresenceRequested = (cb: (p: PresencePromptDto) => void): Promise<UnlistenFn> =>
  listen<PresencePromptDto>("presence://requested", (e) => cb(e.payload));
export const onPresenceDone = (cb: (ok: boolean) => void): Promise<UnlistenFn> =>
  listen<boolean>("presence://done", (e) => cb(e.payload));

// ── Code hosts (forges): Settings → Code hosts and the repo picker ─────────
// Tokens go from this window to this device's backend and its OS secret
// store, and nowhere else: never synced, never sent to the relay.

export type ForgeConnection = {
  id: string;
  kind: "github" | "gitlab" | "gitea";
  flavor?: string | null;
  displayName: string;
  baseUrl: string;
  authSource: "gh" | "token" | "oauth" | "none";
  hasRead: boolean;
  hasWrite: boolean;
  user?: string | null;
  deviceFlow: boolean;
  openLabel: string;
  holder: string;
  available?: boolean;
  reason?: string | null;
  /** Marked internal on this device: a loopback, private or link-local address is allowed. */
  internal?: boolean;
};

export type ForgeRepo = {
  repo: { owner: string; name: string };
  description: string;
  private: boolean;
  defaultBranch: string;
  updatedAt: string;
  webUrl: string;
  fork: boolean;
};

export type ForgeDeviceStart = { userCode: string; verificationUri: string; expiresIn: number };
export type ForgeDeviceStatus = { id: string; state: "pending" | "done" | "denied" | "expired" | "error"; error?: string | null };

export const forgesList = (check = false) => invoke<ForgeConnection[]>("forges_list", { check });
export const forgesAdd = (kind: string, baseUrl: string, auth?: string, clientId?: string, internal?: boolean) =>
  invoke<ForgeConnection>("forges_add", { kind, baseUrl, auth, clientId, internal });
export const forgesRemove = (id: string) => invoke<void>("forges_remove", { id });
export const forgesSetToken = (id: string, purpose: "read" | "write", token: string) =>
  invoke<ForgeConnection>("forges_set_token", { id, purpose, token });
export const forgesTest = (id: string) => invoke<ForgeConnection>("forges_test", { id });
export const forgesDeviceStart = (id: string) => invoke<ForgeDeviceStart>("forges_device_start", { id });
export const forgesDeviceStatus = () => invoke<ForgeDeviceStatus>("forges_device_status");
export const forgesRepos = (id: string, q?: string, owner?: string, page?: number) =>
  invoke<{ login: string; owners: string[]; repos: ForgeRepo[]; hasMore: boolean }>("forges_repos", { id, q, owner, page });
export const forgesClone = (id: string, repo: string, parent: string) => invoke<string>("forges_clone", { id, repo, parent });
