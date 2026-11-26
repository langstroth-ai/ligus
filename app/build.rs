
/// Build-time env lookup of `LIGUS_<suffix>` (blank = unset).
fn build_env(suffix: &str) -> Option<String> {
    let name = format!("LIGUS_{suffix}");
    println!("cargo:rerun-if-env-changed={name}");
    std::env::var(&name).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn main() {
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

