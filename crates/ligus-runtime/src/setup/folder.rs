//! Add a code folder: a git checkout on this device → every bind check →
//! match (or add, Admin) the workspace repository → bind.

use ligus_core::WorkspaceRole;

use super::agent::ops_code;
use super::validate::value;
use super::{exit, gate, Ctx, Field, FieldKind, Inputs, Progress, SetupOps, StepDef};

pub(crate) fn status(ops: &dyn SetupOps) -> (Progress, String) {
    let repos = ops.repos();
    let bound = repos.iter().filter(|r| r.bound_here.is_some()).count();
    match (repos.len(), bound) {
        (0, _) => (Progress::Todo, "no repositories yet".into()),
        (n, 0) => (Progress::Todo, format!("{n} in the workspace, none bound here")),
        (n, b) => (Progress::Done, format!("{b} of {n} bound here")),
    }
}

pub(crate) fn steps(_ops: &dyn SetupOps, _inputs: &Inputs) -> Vec<StepDef> {
    vec![
        StepDef::input(
            "folder",
            "Folder",
            vec![Field::new("path", "Folder", FieldKind::Path)
                .required()
                .help("A git checkout on this device (its main checkout, not a linked worktree).")],
        ),
        StepDef::input(
            "repo",
            "Repository",
            vec![
                Field::new("name", "Display name", FieldKind::Text).help("How the workspace names it. Defaults to the repository's name."),
                Field::new("branch", "Default branch", FieldKind::Text).help("Chats start from it. Detected from origin's HEAD, else main or master."),
            ],
        ),
        StepDef::verify("verify", "Bound"),
    ]
}

pub(crate) fn preflight(ctx: &mut Ctx, _ops: &dyn SetupOps) {
    if !crate::repos::git::git(std::path::Path::new("."), &["--version"]).is_ok() {
        ctx.fail("git", "git", "git isn't installed or isn't on PATH", "install git, then run this again", exit::FIXABLE);
    } else {
        ctx.ok("git", "git", "installed");
    }
}

pub(crate) fn run(ctx: &mut Ctx, ops: &mut dyn SetupOps, inputs: &Inputs) {
    let fields: Vec<Field> = steps(ops, inputs).into_iter().flat_map(|s| s.fields).collect();
    let path = value(inputs, &fields, "path").unwrap_or("").to_string();
    let f = ops.inspect_folder(&path);

    // Every check `repos::bind` makes, before any write.
    if !f.exists {
        return ctx.fail("folder", "folder", format!("{path}: no such folder"), "pass --path with an existing folder", exit::USAGE);
    }
    if !f.is_checkout {
        return ctx.fail("folder.git", "git", "that folder isn't a git checkout", "git clone the repository there, or pick its checkout", exit::USAGE);
    }
    if f.linked_worktree {
        return ctx.fail("folder.git", "git", "that folder is a linked worktree", "pick its main checkout (git worktree list shows it)", exit::USAGE);
    }
    if let Some(label) = &f.managed_elsewhere {
        return ctx.fail(
            "folder.managed",
            "managed",
            format!("that checkout is managed by {label} on this machine"),
            "clone the repository separately to work on it from here",
            exit::USAGE,
        );
    }
    let canonical = f.canonical.clone();
    let detail = canonical.clone().unwrap_or_else(|| "a local repository (no forge remote)".into());
    ctx.ok("folder.git", "git", detail);

    // The workspace repository it is a checkout of.
    let repos = ops.repos();
    let existing = canonical
        .as_ref()
        .and_then(|c| repos.iter().find(|r| &r.canonical == c))
        .or_else(|| f.root_commit.as_ref().and_then(|root| repos.iter().find(|r| r.canonical == format!("local:{root}"))))
        .cloned();
    if let Some(id) = &f.bound_to {
        if existing.as_ref().is_some_and(|r| &r.id == id) {
            ctx.ok("bind", "bound", format!("already bound to {}", existing.as_ref().map(|r| r.display_name.as_str()).unwrap_or("it")));
            ctx.result("repoId", id.clone());
            return ctx.done();
        }
        return ctx.fail("folder.taken", "folder", "that folder is already bound to another repository here", "unbind it in Settings > Repositories first", exit::USAGE);
    }
    let branch = value(inputs, &fields, "branch").map(str::to_string).or(f.default_branch.clone()).unwrap_or_default();
    let repo_id = match existing {
        Some(r) => {
            ctx.ok("repo", "repository", format!("{} is in the workspace", r.display_name));
            r.id
        }
        None => {
            if !gate::need(ctx, ops, WorkspaceRole::Admin, "adding a repository to the workspace") {
                return;
            }
            let name = value(inputs, &fields, "name").unwrap_or("").to_string();
            if ctx.cancelled() {
                return;
            }
            if ctx.dry_run() {
                ctx.skip("repo", "repository", format!("would add it to the workspace (default branch {})", if branch.is_empty() { "unknown" } else { &branch }));
                ctx.skip("bind", "bind", format!("would bind {path}"));
                return ctx.done();
            }
            match ops.add_repo(&f, &name, &branch) {
                Ok(id) => {
                    ctx.ok("repo", "repository", "added to the workspace");
                    id
                }
                Err(e) => return ctx.fail("repo", "repository", format!("couldn't add it: {e}"), "fix what it says and run this again", ops_code(&e)),
            }
        }
    };
    ctx.result("repoId", repo_id.clone());
    if ctx.cancelled() {
        return;
    }
    if ctx.dry_run() {
        ctx.skip("bind", "bind", format!("would bind {path}"));
        return ctx.done();
    }
    // The repo event (if any) is final: every bind check passed above.
    ctx.commit();
    match ops.bind_folder(&repo_id, &path) {
        Ok(()) => ctx.ok("bind", "bound", path.clone()),
        Err(e) => return ctx.fail("bind", "bind", e.to_string(), "fix what it says and run this again", ops_code(&e)),
    }
    if ops.repos().iter().any(|r| r.id == repo_id && r.bound_here.is_some()) {
        ctx.done();
    } else {
        ctx.fail("verify", "verify", "the binding isn't listed", "Settings > Repositories lists the bindings", exit::FAILED);
    }
}
