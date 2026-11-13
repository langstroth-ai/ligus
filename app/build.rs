use std::path::Path;

/// Build-time env lookup of `LIGUS_<suffix>` (blank = unset).
fn build_env(suffix: &str) -> Option<String> {
    let name = format!("LIGUS_{suffix}");
    println!("cargo:rerun-if-env-changed={name}");
    std::env::var(&name).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn main() {
    bake_github_client_id();
    bake_release_tag();
    tauri_build::build();
}

/// Bake the release tag (e.g. `v0.1.0-beta.10`) so the app can tell whether a
/// newer published version exists (see `check_for_app_update`). CI sets
/// `LIGUS_RELEASE_TAG`; local/dev builds get `dev` and never show the update
/// banner (avoids false "update available" prompts during development).
fn bake_release_tag() {
    let tag = build_env("RELEASE_TAG").unwrap_or_else(|| "dev".to_string());
    println!("cargo:rustc-env=LIGUS_RELEASE_TAG={tag}");
}

/// Inject the GitHub OAuth App client id at compile time so `option_env!` /
/// `env!("LIGUS_GITHUB_CLIENT_ID")` resolves in the app, *without* committing the
/// id to source. Resolution order:
///   1. env `LIGUS_GITHUB_CLIENT_ID` (CI / one-off builds)
///   2. a gitignored `app/github_client_id` file (official local builds)
/// Forks without either build with no default (users paste their own in-app).
fn bake_github_client_id() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("github_client_id");
    println!("cargo:rerun-if-changed={}", file.display());

    let id = build_env("GITHUB_CLIENT_ID")
        .or_else(|| std::fs::read_to_string(&file).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();

    println!("cargo:rustc-env=LIGUS_GITHUB_CLIENT_ID={id}");
}
