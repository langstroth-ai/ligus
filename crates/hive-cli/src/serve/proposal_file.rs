//! `GET /v1/proposals/{id}/file?path=…&side=old|new`: one file of a proposal's
//! diff, whole, before or after the change — so the phone can render a changed
//! Markdown document instead of only its hunks.
//!
//! The base is read from the chat's bound checkout on this host: the chat's
//! branch `hive/chat/<id>` when it exists (that is what turns are cut from),
//! else the checkout's `HEAD`. The proposal's patch for that one file is
//! applied in a scratch directory outside the repository — never in the
//! checkout or a worktree. When the base already carries the change (the
//! proposal was applied), the patch is reversed instead to recover the old
//! side.
//!
//! Errors: 400 for a bad `side`, a path that isn't a plain relative path, or
//! one the diff doesn't touch; 404 when the proposal has no diff, the chat has
//! no checkout here, or the requested side doesn't exist (a new file has no
//! old side, a deleted one no new side); 409 when the diff applies to neither
//! base; 413 over the size cap; 415 for a file that isn't UTF-8 text.

use std::io::Write;
use std::path::{Path as FsPath, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use hive_core::ProposalKind;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::api::{find_proposal, ApiError, ApiResult};
use super::Shared;
use crate::repos::{self, RepoBindings};

/// Largest file (either side) the endpoint returns.
pub const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_PATH_LEN: usize = 1024;

#[derive(Deserialize, Default)]
pub struct FileQuery {
    path: Option<String>,
    side: Option<String>,
}

/// One file's section of a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    /// `None` for a file the diff creates.
    pub old: Option<String>,
    /// `None` for a file the diff deletes.
    pub new: Option<String>,
    pub text: String,
}

/// A plain relative path inside the repository: no absolute paths, no `.`/`..`
/// or empty components, no backslashes or NULs, nothing under `.git`.
pub fn valid_rel_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= MAX_PATH_LEN
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains('\0')
        && !p.chars().any(char::is_control)
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && !c.eq_ignore_ascii_case(".git"))
}

fn header_path(rest: &str) -> Option<String> {
    let p = rest.split('\t').next().unwrap_or("").trim_end();
    if p == "/dev/null" {
        return None;
    }
    let p = p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")).unwrap_or(p);
    Some(p.to_string())
}

/// `@@ -a,b +c,d @@` → (b, d); a missing count is 1.
fn hunk_counts(line: &str) -> (usize, usize) {
    let mut out = (1, 1);
    let ranges = line.trim_start_matches('@').split("@@").next().unwrap_or("");
    for tok in ranges.split_whitespace() {
        let count = |rest: &str| rest.split(',').nth(1).and_then(|n| n.parse().ok()).unwrap_or(1);
        if let Some(rest) = tok.strip_prefix('-') {
            out.0 = count(rest);
        } else if let Some(rest) = tok.strip_prefix('+') {
            out.1 = count(rest);
        }
    }
    out
}

/// Split a unified diff into per-file sections. Sections start at
/// `diff --git` (or, for a plain diff, at a `--- ` header outside a hunk);
/// hunk bodies are consumed by their header counts, so a removed line that
/// reads `-- x` is never taken for a header.
pub fn split_patch(diff: &str) -> Vec<FilePatch> {
    let mut out: Vec<FilePatch> = Vec::new();
    let mut cur: Option<FilePatch> = None;
    let (mut old_left, mut new_left) = (0usize, 0usize);
    let mut saw_hunk = false;
    for line in diff.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if old_left > 0 || new_left > 0 {
            if let Some(c) = cur.as_mut() {
                c.text.push_str(line);
            }
            match bare.chars().next() {
                Some('+') => new_left = new_left.saturating_sub(1),
                Some('-') => old_left = old_left.saturating_sub(1),
                Some('\\') => {}
                _ => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
            }
            continue;
        }
        let starts_git = bare.starts_with("diff --git ");
        let starts_plain = bare.starts_with("--- ") && (cur.is_none() || saw_hunk);
        if starts_git || starts_plain {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            saw_hunk = false;
            let (mut old, mut new) = (None, None);
            if starts_git {
                // `diff --git a/x b/x`: the split at the last " b/" is right
                // for the usual same-path case; ---/+++ refine it below.
                let rest = &bare["diff --git ".len()..];
                if let Some(i) = rest.rfind(" b/") {
                    old = header_path(&rest[..i]);
                    new = header_path(&rest[i + 1..]);
                }
            }
            cur = Some(FilePatch { old, new, text: String::new() });
        }
        let Some(c) = cur.as_mut() else { continue };
        c.text.push_str(line);
        if let Some(rest) = bare.strip_prefix("--- ") {
            c.old = header_path(rest);
        } else if let Some(rest) = bare.strip_prefix("+++ ") {
            c.new = header_path(rest);
        } else if bare.starts_with("new file mode") {
            c.old = None;
        } else if bare.starts_with("deleted file mode") {
            c.new = None;
        } else if let Some(p) = bare.strip_prefix("rename from ") {
            c.old = Some(p.to_string());
        } else if let Some(p) = bare.strip_prefix("rename to ") {
            c.new = Some(p.to_string());
        } else if bare.starts_with("@@") {
            saw_hunk = true;
            (old_left, new_left) = hunk_counts(bare);
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Self> {
        let p = std::env::temp_dir().join(format!("hive-propfile-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(p.join("work"))?;
        Ok(Self(p))
    }
    fn work(&self) -> PathBuf {
        self.0.join("work")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git_bytes(dir: &FsPath, args: &[&str]) -> Option<Vec<u8>> {
    let out = hive_core::process_util::command("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// The file at `rev:path`, `Ok(None)` when it doesn't exist there.
fn blob_at(repo: &FsPath, rev: &str, path: &str) -> Result<Option<Vec<u8>>, ApiError> {
    let spec = format!("{rev}:{path}");
    let Some(size) = repos::git_out(repo, &["cat-file", "-s", &spec]).and_then(|s| s.parse::<u64>().ok()) else {
        return Ok(None);
    };
    if size > MAX_FILE_BYTES {
        return Err(too_big());
    }
    git_bytes(repo, &["cat-file", "blob", &spec])
        .map(Some)
        .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "couldn't read the file from the checkout"))
}

fn too_big() -> ApiError {
    ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, format!("the file is larger than {} KB", MAX_FILE_BYTES / 1024))
}

/// Apply `patch` (one file's section) in a scratch dir holding `content` at
/// `at`, and read back `want`. `None` content/result means "no such file".
/// `Ok(None)` when the patch doesn't apply.
fn apply_in_scratch(
    patch: &str,
    reverse: bool,
    at: Option<&str>,
    content: Option<&[u8]>,
    want: Option<&str>,
) -> Result<Option<Option<Vec<u8>>>, ApiError> {
    let io = |e: std::io::Error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("scratch dir: {e}"));
    let scratch = Scratch::new().map_err(io)?;
    let work = scratch.work();
    if let (Some(at), Some(content)) = (at, content) {
        let f = work.join(at);
        if let Some(parent) = f.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        std::fs::write(&f, content).map_err(io)?;
    }
    let mut cmd = hive_core::process_util::command("git");
    cmd.current_dir(&work)
        // Never find a repository above the scratch dir: a plain patch tool.
        .env("GIT_CEILING_DIRECTORIES", &scratch.0)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(["apply", "--whitespace=nowarn"]);
    if reverse {
        cmd.arg("--reverse");
    }
    let mut child = cmd
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("git apply: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(patch.as_bytes());
    }
    let status = child.wait().map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("git apply: {e}")))?;
    if !status.success() {
        return Ok(None);
    }
    let Some(want) = want else { return Ok(Some(None)) };
    let f = work.join(want);
    if !f.is_file() {
        return Ok(Some(None));
    }
    if std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
        return Err(too_big());
    }
    Ok(Some(Some(std::fs::read(&f).map_err(io)?)))
}

/// Both sides of one file, from the first base the patch fits.
pub struct Sides {
    pub old: Option<Vec<u8>>,
    pub new: Option<Vec<u8>>,
    pub base: String,
    /// The base already carries the change.
    pub applied: bool,
}

pub fn resolve_sides(repo: &FsPath, chat: Uuid, fp: &FilePatch) -> Result<Sides, ApiError> {
    let branch = repos::chat_branch(chat);
    let mut bases: Vec<(String, String)> = Vec::new();
    if repos::git_out(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_some() {
        bases.push((format!("refs/heads/{branch}"), branch));
    }
    if let Some(sha) = repos::git_out(repo, &["rev-parse", "--verify", "--quiet", "HEAD"]) {
        let label = repos::inspect(repo).branch.unwrap_or_else(|| sha.chars().take(12).collect());
        if !bases.iter().any(|(r, _)| repos::git_out(repo, &["rev-parse", r]).as_deref() == Some(sha.as_str())) {
            bases.push((sha, label));
        }
    }
    for (rev, label) in bases {
        // Forward: the base is the old side.
        let old = match fp.old.as_deref() {
            Some(p) => blob_at(repo, &rev, p)?,
            None => None,
        };
        if fp.old.is_none() || old.is_some() {
            if let Some(new) = apply_in_scratch(&fp.text, false, fp.old.as_deref(), old.as_deref(), fp.new.as_deref())? {
                return Ok(Sides { old, new, base: label, applied: false });
            }
        }
        // Reverse: the base already has the change (an applied proposal).
        let new = match fp.new.as_deref() {
            Some(p) => blob_at(repo, &rev, p)?,
            None => None,
        };
        if fp.new.is_none() || new.is_some() {
            if let Some(old) = apply_in_scratch(&fp.text, true, fp.new.as_deref(), new.as_deref(), fp.old.as_deref())? {
                return Ok(Sides { old, new, base: label, applied: true });
            }
        }
    }
    Err(ApiError::new(
        StatusCode::CONFLICT,
        "the proposal's diff doesn't apply to this chat's checkout on this host (the file has moved on since it was proposed)",
    ))
}

pub async fn proposal_file(
    State(s): State<Arc<Shared>>,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> ApiResult<Json<Value>> {
    let side = match q.side.as_deref().map(str::trim) {
        None | Some("") | Some("new") => "new",
        Some("old") => "old",
        Some(_) => return Err(ApiError::bad_request("side must be old or new")),
    };
    let path = q.path.unwrap_or_default();
    let path = path.trim().trim_start_matches("./").to_string();
    if !valid_rel_path(&path) {
        return Err(ApiError::bad_request("path must be a relative path inside the repository"));
    }
    let (chat, p) = find_proposal(&s, &id)?;
    let diff = match (p.kind, p.diff.as_deref()) {
        (ProposalKind::FileDiff, Some(d)) if !d.trim().is_empty() => d.to_string(),
        _ => return Err(ApiError::new(StatusCode::NOT_FOUND, "this proposal has no file changes")),
    };
    let fp = split_patch(&diff)
        .into_iter()
        .find(|f| f.new.as_deref() == Some(path.as_str()) || f.old.as_deref() == Some(path.as_str()))
        .ok_or_else(|| ApiError::bad_request("that file isn't part of this proposal's diff"))?;
    if [fp.old.as_deref(), fp.new.as_deref()].into_iter().flatten().any(|p| !valid_rel_path(p)) {
        return Err(ApiError::bad_request("the diff names a path outside the repository"));
    }
    let repo = RepoBindings::load(&s.cfg.data_dir)
        .resolve(&repos::repos_dir(&s.cfg.data_dir), chat.id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "this chat has no repository on this host"))?;
    let chat_id = chat.id;
    let sides = tokio::task::spawn_blocking(move || {
        if !hive_runtime::git_worktree::is_git_repo(&repo) {
            return Err(ApiError::new(StatusCode::NOT_FOUND, "this chat's checkout is missing on this host"));
        }
        resolve_sides(&repo, chat_id, &fp)
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    let bytes = match side {
        "old" => sides.old.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "the file is new in this proposal: it has no old side"))?,
        _ => sides.new.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "the proposal deletes this file: it has no new side"))?,
    };
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(too_big());
    }
    let size = bytes.len();
    let content = String::from_utf8(bytes)
        .map_err(|_| ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "the file isn't UTF-8 text"))?;
    Ok(Json(json!({
        "path": path,
        "side": side,
        "content": content,
        "size": size,
        "base": sides.base,
        "applied": sides.applied,
    })))
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn paths_must_be_plain_and_relative() {
        for ok in ["README.md", "docs/a b.md", "a/.github/x.md", ".hive/notes.md"] {
            assert!(valid_rel_path(ok), "{ok}");
        }
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a//b", "./a", "a/.", ".git/config", "a\\b", "a\0b", "a\nb"] {
            assert!(!valid_rel_path(bad), "{bad:?}");
        }
    }

    #[test]
    fn split_follows_hunk_counts_and_headers() {
        let diff = "diff --git a/README.md b/README.md\nindex 1..2 100644\n--- a/README.md\n+++ b/README.md\n@@ -1,2 +1,2 @@\n-- old\n--- not a header\n+- new\n+--- still body\n\
diff --git a/new.md b/new.md\nnew file mode 100644\n--- /dev/null\n+++ b/new.md\n@@ -0,0 +1 @@\n+hi\n\
diff --git a/gone.md b/gone.md\ndeleted file mode 100644\n--- a/gone.md\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n";
        let files = split_patch(diff);
        assert_eq!(files.len(), 3);
        assert_eq!((files[0].old.as_deref(), files[0].new.as_deref()), (Some("README.md"), Some("README.md")));
        assert!(files[0].text.contains("--- not a header"));
        assert_eq!((files[1].old.as_deref(), files[1].new.as_deref()), (None, Some("new.md")));
        assert_eq!((files[2].old.as_deref(), files[2].new.as_deref()), (Some("gone.md"), None));
        assert!(files[2].text.ends_with("-bye\n"));
    }

    #[test]
    fn hunk_headers_never_panic() {
        assert_eq!(hunk_counts("@@ -3,4 +3,5 @@ ctx"), (4, 5));
        assert_eq!(hunk_counts("@@ -1 +1 @@"), (1, 1));
        assert_eq!(hunk_counts("@@ é -0,0 +1,2 @@"), (0, 2));
    }

    #[test]
    fn plain_diffs_without_git_headers_split_too() {
        let diff = "--- a/x.md\n+++ b/x.md\n@@ -1 +1 @@\n-a\n+b\n--- a/y.md\n+++ b/y.md\n@@ -1 +1 @@\n-c\n+d\n";
        let files = split_patch(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].new.as_deref(), Some("y.md"));
        assert!(files[1].text.starts_with("--- a/y.md"));
    }
}
