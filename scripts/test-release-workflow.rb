#!/usr/bin/env ruby
# frozen_string_literal: true

# Checks the desktop and CLI release workflows against how the pinned
# release actions really behave, so a rerun can never overwrite the release
# notes the owner wrote, and the desktop assets and latest.json always land on
# the one release for the tag.
#
#   ruby scripts/test-release-workflow.rb
#
# Behaviour modelled (read from the pinned sources):
#
# tauri-apps/tauri-action v1.0.0 (src/create-release.ts, src/index.ts)
#   - releaseId set: upload to that release only. No lookup, no create, and
#     the release's name and body are never touched.
#   - tagName set, releaseDraft true: reuse a DRAFT release for the tag; a
#     PUBLISHED one is skipped ("NOT a draft") and the action tries to create
#     a second release for the tag, which fails (or leaves a stray draft), so
#     the published release gets no installers and no latest.json.
#   - tagName set, releaseDraft false: an existing published release has its
#     name and body overwritten with releaseName/releaseBody on every run.
#   - With no root package.json it runs "npm install -g @tauri-apps/cli@v2"
#     (latest 2.x, unpinned) inside the step that holds the signing keys,
#     unless tauriScript names a CLI that is already installed.
#   - Updater bundles are named <product>_<version>_<arch>.app.tar.gz (the
#     version is new in v1), and latest.json URLs are API asset URLs.
#
# softprops/action-gh-release v3.0.4 (release-cli.yml)
#   - Creates a published release (title = tag, empty body) if none exists.
#   - On an existing release keeps name, body and prerelease when those inputs
#     are unset, and publishes a draft at the end unless draft: true.

require "json"
require "minitest/autorun"
require "open3"
require "tmpdir"
require "fileutils"
require "yaml"

ROOT = File.expand_path("..", __dir__)
RELEASE = YAML.safe_load(File.read(File.join(ROOT, ".github/workflows/release.yml")))
RELEASE_CLI = YAML.safe_load(File.read(File.join(ROOT, ".github/workflows/release-cli.yml")))
TAURI_CONF = JSON.parse(File.read(File.join(ROOT, "app/tauri.conf.json")))
ENSURE = File.join(ROOT, "scripts/ensure-release.sh")

def steps(workflow, job)
  workflow.fetch("jobs").fetch(job).fetch("steps")
end

def tauri_step
  steps(RELEASE, "build").find { |s| s["uses"].to_s.start_with?("tauri-apps/tauri-action@") }
end

# What tauri-action v1 does to the release for the tag, given the step's
# inputs and the release that already exists (nil, :draft or :published).
def tauri_action_outcome(with, existing)
  return { assets_on: existing || :given, edits_notes: false } if with["releaseId"]
  return { assets_on: nil, edits_notes: false } unless with["tagName"]

  if with["releaseDraft"].to_s == "true"
    case existing
    when nil then { assets_on: :created, edits_notes: false }
    when :draft then { assets_on: :draft, edits_notes: false }
    else { assets_on: :failed_or_stray_draft, edits_notes: false }
    end
  else
    case existing
    when nil then { assets_on: :created, edits_notes: false }
    when :draft then { assets_on: :failed_or_stray_draft, edits_notes: false }
    else { assets_on: :published, edits_notes: true }
    end
  end
end

class TauriActionUse < Minitest::Test
  def with
    tauri_step.fetch("with")
  end

  def test_pinned_to_v1
    assert_match(/@1deb371b0cd8bd54025b384f1cd735e725c4060f\z/, tauri_step["uses"])
  end

  # release-cli (about 5 min) creates the release before the desktop build
  # (about 20 min) reaches the upload, so "published release exists" is the
  # normal case on a tag push, and the only case on a rerun after publishing.
  def test_installers_land_on_an_existing_published_release
    out = tauri_action_outcome(with, :published)
    assert_equal :published, out[:assets_on],
                 "tauri-action would not upload to the published release (#{out[:assets_on]})"
  end

  def test_rerun_never_rewrites_release_notes
    %i[published draft].each do |state|
      refute tauri_action_outcome(with, state)[:edits_notes],
             "a rerun against a #{state} release would overwrite its name and body"
    end
  end

  def test_release_comes_from_the_ensure_job
    assert_equal "${{ needs.release.outputs.id }}", with["releaseId"]
    %w[tagName releaseName releaseDraft prerelease].each do |k|
      refute with.key?(k), "#{k} makes tauri-action look up/create/edit the release itself"
    end
    assert_includes Array(RELEASE["jobs"]["build"]["needs"]), "release"
  end

  def test_tauri_cli_is_pinned_and_installed_without_secrets
    assert_equal "tauri", with["tauriScript"],
                 "without tauriScript the action installs an unpinned @tauri-apps/cli@v2 next to the signing keys"
    install = steps(RELEASE, "build").find { |s| s["run"].to_s.include?("@tauri-apps/cli@") }
    refute_nil install, "no step installs the Tauri CLI"
    assert_match(/@tauri-apps\/cli@\d+\.\d+\.\d+(\s|$)/, install["run"], "Tauri CLI version must be exact")
    assert_nil install["env"], "the CLI install step must not see any secrets"
    assert_operator steps(RELEASE, "build").index(install), :<, steps(RELEASE, "build").index(tauri_step)
  end

  def test_frontend_install_uses_the_lockfile
    install = steps(RELEASE, "build").find { |s| s["run"].to_s.start_with?("bun install") }
    assert_includes install["run"], "--frozen-lockfile"
  end
end

class ReleaseCliUse < Minitest::Test
  def gh_release_step
    steps(RELEASE_CLI, "build").find { |s| s["uses"].to_s.start_with?("softprops/action-gh-release@") }
  end

  # action-gh-release only keeps an existing release's notes and state when
  # it is not handed name/body/prerelease/draft/make_latest.
  def test_cli_upload_never_rewrites_the_release
    w = gh_release_step.fetch("with")
    %w[name body body_path prerelease draft make_latest append_body generate_release_notes].each do |k|
      refute w.key?(k), "release-cli would rewrite #{k} on a rerun"
    end
  end

  def test_cli_attaches_to_the_release_the_ensure_step_made
    ensure_step = steps(RELEASE_CLI, "build").find { |s| s["run"].to_s.include?("scripts/ensure-release.sh") }
    refute_nil ensure_step, "release-cli must create the release the same way release.yml does"
    assert_operator steps(RELEASE_CLI, "build").index(ensure_step), :<, steps(RELEASE_CLI, "build").index(gh_release_step)
  end
end

class AssetNames < Minitest::Test
  PRODUCT = TAURI_CONF.fetch("productName")

  # tauri-action v1 keeps the CLI's file name for .dmg and inserts the
  # version into .app.tar.gz(.sig).
  def v1_names(version)
    {
      dmg: "#{PRODUCT}_#{version}_aarch64.dmg",
      updater: "#{PRODUCT}_#{version}_aarch64.app.tar.gz",
    }
  end

  def test_cask_job_downloads_the_v1_dmg_name
    run = steps(RELEASE, "cask").map { |s| s["run"].to_s }.join("\n")
    pattern = run[/DMG="([^"]+)"/, 1]
    refute_nil pattern
    assert_equal v1_names("0.1.0")[:dmg], pattern.gsub("${VERSION}", "0.1.0")
  end

  def test_latest_json_is_verified_after_every_build
    verify = RELEASE["jobs"]["verify-updater"]
    refute_nil verify, "nothing checks latest.json after the matrix finishes"
    assert_includes Array(verify["needs"]), "build"
    run = verify["steps"].map { |s| s["run"].to_s }.join("\n")
    assert_includes run, "scripts/verify-latest-json.py"
  end
end

# scripts/ensure-release.sh against a fake gh.
class EnsureRelease < Minitest::Test
  FAKE_GH = <<~'SH'
    #!/usr/bin/env bash
    # Fake gh: state in $STATE (none|published|draft|race), calls logged to $LOG.
    echo "$*" >> "$LOG"
    case "$1 $2" in
      "api repos/o/r/releases/tags/v0.1.0")
        if [ "$STATE" = published ] || [ -f "$STATE_DIR/created" ]; then echo 41; exit 0; fi
        echo "gh: Not Found (HTTP 404)" >&2; exit 1 ;;
      "api --paginate")
        if [ "$STATE" = draft ]; then echo 42; fi; exit 0 ;;
      "release create")
        if [ "$STATE" = race ]; then touch "$STATE_DIR/created"; echo "already_exists" >&2; exit 1; fi
        touch "$STATE_DIR/created"; echo "https://github.com/o/r/releases/tag/v0.1.0"; exit 0 ;;
    esac
    echo "unexpected gh call: $*" >&2; exit 99
  SH

  def run_ensure(state, tag = "v0.1.0")
    Dir.mktmpdir do |dir|
      File.write(File.join(dir, "gh"), FAKE_GH)
      File.chmod(0o755, File.join(dir, "gh"))
      log = File.join(dir, "log")
      File.write(log, "")
      env = { "PATH" => "#{dir}:#{ENV['PATH']}", "STATE" => state, "STATE_DIR" => dir, "LOG" => log,
              "ENSURE_RELEASE_RETRY_DELAY" => "0" }
      out, err, status = Open3.capture3(env, "bash", ENSURE, "o/r", tag)
      [out.strip, err, status, File.read(log)]
    end
  end

  def test_reuses_a_published_release_without_editing_it
    out, _err, status, log = run_ensure("published")
    assert status.success?
    assert_equal "41", out
    refute_match(/release edit|PATCH|-X/, log)
    refute_match(/release create/, log)
  end

  def test_reuses_a_draft
    out, _err, status, log = run_ensure("draft")
    assert status.success?
    assert_equal "42", out
    refute_match(/release create|release edit/, log)
  end

  def test_creates_a_prerelease_when_missing
    out, _err, status, log = run_ensure("none")
    assert status.success?, log
    assert_equal "41", out
    create = log.lines.find { |l| l.start_with?("release create") }
    assert_includes create, "--prerelease"
    assert_includes create, "--verify-tag"
  end

  def test_loses_a_create_race_gracefully
    out, _err, status, _log = run_ensure("race")
    assert status.success?
    assert_equal "41", out
  end

  def test_rejects_a_malformed_tag
    _out, _err, status, log = run_ensure("published", "v0.1.0\"; rm -rf /")
    refute status.success?
    assert_equal "", log
  end
end

class TauriCliPin < Minitest::Test
  def test_release_cli_pin_matches_the_web_lockfile
    install = steps(RELEASE, "build").find { |s| s["run"].to_s.include?("@tauri-apps/cli@") }
    pinned = install["run"][/@tauri-apps\/cli@(\d+\.\d+\.\d+)/, 1]
    locked = File.read(File.join(ROOT, "web/bun.lock"))[/"@tauri-apps\/cli": \["@tauri-apps\/cli@(\d+\.\d+\.\d+)"/, 1]
    assert_equal locked, pinned, "release.yml installs a different Tauri CLI than web/bun.lock pins"
  end
end

# scripts/verify-latest-json.py on fixture releases.
class VerifyLatestJson < Minitest::Test
  VERIFY = File.join(ROOT, "scripts/verify-latest-json.py")
  API = "https://api.github.com/repos/o/r/releases/assets"

  def release(mac_name: "Ligus_0.1.0_aarch64.app.tar.gz", drop: nil, mac_sig: "SIG-MAC", version: "0.1.0")
    assets = [
      { "id" => 1, "name" => mac_name }, { "id" => 2, "name" => "#{mac_name}.sig" },
      { "id" => 3, "name" => "Ligus_0.1.0_x64_en-US.msi" }, { "id" => 4, "name" => "Ligus_0.1.0_x64_en-US.msi.sig" },
      { "id" => 5, "name" => "Ligus_0.1.0_amd64.AppImage" }, { "id" => 6, "name" => "Ligus_0.1.0_amd64.AppImage.sig" },
      { "id" => 7, "name" => "Ligus_0.1.0_aarch64.dmg" }, { "id" => 9, "name" => "latest.json" },
    ]
    platforms = {
      "darwin-aarch64" => { "url" => "#{API}/1", "signature" => mac_sig },
      "windows-x86_64" => { "url" => "#{API}/3", "signature" => "SIG-WIN" },
      "linux-x86_64" => { "url" => "#{API}/5", "signature" => "SIG-LIN" },
    }
    platforms.delete(drop) if drop
    sigs = { "#{mac_name}.sig" => "SIG-MAC", "Ligus_0.1.0_x64_en-US.msi.sig" => "SIG-WIN",
             "Ligus_0.1.0_amd64.AppImage.sig" => "SIG-LIN" }
    [{ "version" => version, "notes" => "", "platforms" => platforms }, assets, sigs]
  end

  def verify(latest, assets, sigs, platforms = "macos,windows,linux")
    Dir.mktmpdir do |dir|
      File.write(File.join(dir, "latest.json"), JSON.dump(latest))
      File.write(File.join(dir, "assets.json"), JSON.dump(assets))
      sigs.each { |n, v| File.write(File.join(dir, n), v) }
      out, _err, status = Open3.capture3("python3", "-I", VERIFY, "--from-dir", dir, "--tag", "v0.1.0",
                                         "--platforms", platforms)
      [status.success?, out]
    end
  end

  def test_good_release_passes
    ok, out = verify(*release)
    assert ok, out
  end

  def test_pre_v1_unversioned_updater_bundle_fails
    ok, out = verify(*release(mac_name: "Ligus_aarch64.app.tar.gz"))
    refute ok
    assert_includes out, "does not carry version"
  end

  def test_platform_dropped_by_a_concurrent_job_fails
    ok, out = verify(*release(drop: "windows-x86_64"))
    refute ok
    assert_includes out, "windows-x86_64: missing"
  end

  def test_skipped_platform_is_not_required
    ok, out = verify(*release(drop: "darwin-aarch64"), "windows,linux")
    assert ok, out
  end

  def test_stale_signature_after_a_rerun_fails
    ok, out = verify(*release(mac_sig: "OLD"))
    refute ok
    assert_includes out, "signature differs"
  end

  def test_url_to_another_release_fails
    latest, assets, sigs = release
    latest["platforms"]["linux-x86_64"]["url"] = "#{API}/999"
    ok, out = verify(latest, assets, sigs)
    refute ok
    assert_includes out, "not an asset of this release"
  end

  def test_wrong_version_fails
    ok, out = verify(*release(version: "1.18.0"))
    refute ok
    assert_includes out, "version"
  end
end

class ReleaseJob < Minitest::Test
  # An empty releaseId makes tauri-action log "skipping all uploads" and
  # succeed, so the release job must fail rather than output an empty id.
  def test_release_id_output_cannot_be_empty
    run = steps(RELEASE, "release").map { |s| s["run"].to_s }.join("\n")
    refute_match(/echo "id=\$\(/, run, "a failing ensure-release.sh inside echo would not fail the step")
    assert_match(/\^\[0-9\]\+\$/, run, "the id must be checked to be numeric")
  end
end

class ReleaseConfig < Minitest::Test
  SCRIPT = File.expand_path("check-release-config.sh", __dir__)
  CONF = File.expand_path("../app/tauri.conf.json", __dir__)

  def run_check(tag, conf = CONF)
    out, _err, status = Open3.capture3(SCRIPT, tag, conf)
    [status.success?, out]
  end

  def test_release_job_runs_the_check_before_creating_the_release
    run = steps(RELEASE, "release").map { |s| s["run"].to_s }.join("\n")
    assert_operator run.index("check-release-config.sh"), :<, run.index("ensure-release.sh")
  end

  def test_committed_config_passes_for_its_own_plain_tag
    version = JSON.parse(File.read(CONF))["version"]
    ok, out = run_check("v#{version}")
    assert ok, out
  end

  def test_placeholder_pubkey_fails
    Dir.mktmpdir do |dir|
      conf = JSON.parse(File.read(CONF))
      conf["plugins"]["updater"]["pubkey"] = "REPLACE_WITH_LIGUS_UPDATER_PUBKEY"
      path = File.join(dir, "tauri.conf.json")
      File.write(path, JSON.generate(conf))
      ok, out = run_check("v#{conf["version"]}", path)
      refute ok
      assert_includes out, "placeholder"
    end
  end

  def test_tag_must_match_the_app_version
    ok, out = run_check("v9.9.9")
    refute ok
    assert_includes out, "does not match"
    ok, _out = run_check("ligus-v0.1.0")
    refute ok
  end
end

# The native-passkeys signing checks (scripts/check-macos-signing.sh): the
# profile is checked before the build, the signed app's entitlements after.
class MacosSigning < Minitest::Test
  SCRIPT = File.expand_path("check-macos-signing.sh", __dir__)

  def build_steps
    steps(RELEASE, "build")
  end

  def index_of(&blk)
    build_steps.index(&blk)
  end

  def test_profile_is_checked_before_the_build_and_entitlements_after
    configure = index_of { |s| s["name"] == "Configure macOS signing" }
    build = index_of { |s| s["uses"].to_s.start_with?("tauri-apps/tauri-action@") }
    check = index_of { |s| s["run"].to_s.include?("check-macos-signing.sh entitlements") }
    run = build_steps[configure]["run"]
    assert_operator run.index("base64 --decode"), :<, run.index("check-macos-signing.sh profile")
    assert_operator run.index("check-macos-signing.sh profile"), :<, run.index("tauri.passkeys.conf.json"),
                    "the passkeys overlay is enabled only after the profile passed"
    assert_operator configure, :<, build
    refute_nil check, "no entitlement check after signing"
    assert_operator build, :<, check
    assert_includes build_steps[check]["if"], "LIGUS_MACOS_SIGNED", "an unsigned build has nothing to check"
  end

  def test_without_the_secret_there_is_no_overlay
    run = build_steps.find { |s| s["name"] == "Configure macOS signing" }["run"]
    guarded = run[/if \[ -n "\$APPLE_PROVISIONING_PROFILE" \]; then(.*?)\nfi/m, 1]
    refute_nil guarded
    assert_includes guarded, "tauri.passkeys.conf.json"
    refute_includes run.sub(guarded, ""), "tauri.passkeys.conf.json"
  end

  def profile(dir, app_id: "H5H3NNR456.ai.ligus.desktop", team: "H5H3NNR456", domains: true, expires: "2099-01-01T00:00:00Z")
    path = File.join(dir, "profile.plist")
    File.write(path, <<~PLIST)
      <?xml version="1.0" encoding="UTF-8"?>
      <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
      <plist version="1.0"><dict>
      <key>Entitlements</key><dict>
      <key>com.apple.application-identifier</key><string>#{app_id}</string>
      <key>com.apple.developer.team-identifier</key><string>#{team}</string>
      #{domains ? "<key>com.apple.developer.associated-domains</key><string>*</string>" : ""}
      </dict>
      <key>TeamIdentifier</key><array><string>#{team}</string></array>
      <key>ExpirationDate</key><date>#{expires}</date>
      </dict></plist>
    PLIST
    path
  end

  def check(path)
    out, err, status = Open3.capture3(SCRIPT, "profile", "--decoded", path)
    [status.success?, out + err]
  end

  def test_profile_checks
    skip "PlistBuddy and plutil are macOS tools" unless RUBY_PLATFORM.include?("darwin")
    Dir.mktmpdir do |dir|
      ok, out = check(profile(dir))
      assert ok, out
      [
        [{ app_id: "H5H3NNR456.ai.other.app" }, "app id"],
        [{ team: "AAAAAAAAAA" }, "team"],
        [{ domains: false }, "Associated Domains"],
        [{ expires: "2020-01-01T00:00:00Z" }, "expired"],
      ].each do |opts, why|
        ok, out = check(profile(dir, **opts))
        refute ok, "#{opts} passed"
        assert_includes out, why
      end
    end
  end
end

# Every job that builds or checks a release must build the tag itself, also
# on a manual dispatch (which otherwise checks out the dispatching branch).
class BuildsTheTag < Minitest::Test
  TAG_REF = "${{ github.event.inputs.tag && format('refs/tags/{0}', github.event.inputs.tag) || github.ref }}"

  def checkouts(workflow)
    workflow.fetch("jobs").flat_map do |name, job|
      job.fetch("steps").select { |s| s["uses"].to_s.start_with?("actions/checkout@") }.map { |s| [name, s] }
    end
  end

  def test_every_release_checkout_is_the_tag
    [RELEASE, RELEASE_CLI].each do |wf|
      checkouts(wf).each do |job, step|
        assert_equal TAG_REF, step.dig("with", "ref"), "#{job} would build the dispatching branch, not the tag"
      end
    end
  end

  def test_cli_release_checks_the_tag_against_the_version_first
    run = steps(RELEASE_CLI, "build").map { |s| s["run"].to_s }.join("\n")
    refute_nil run.index("check-release-config.sh"), "release-cli never checks the tag matches the version"
    assert_operator run.index("check-release-config.sh"), :<, run.index("cargo build")
  end
end

# The CLI binaries ship with a SHA-256 file each, and install.sh refuses a
# binary that does not match it.
class CliChecksums < Minitest::Test
  INSTALL = File.join(ROOT, "install.sh")

  def test_release_cli_attaches_a_checksum_per_binary
    step = steps(RELEASE_CLI, "build").find { |s| s["uses"].to_s.start_with?("softprops/action-gh-release@") }
    files = step.dig("with", "files").to_s
    assert_includes files, "ligus-${{ matrix.target }}.sha256"
    stage = steps(RELEASE_CLI, "build").find { |s| s["name"] == "Stage asset" }["run"]
    assert_match(/sha256/, stage)
  end

  # Run install.sh against a fake curl that serves files from a directory.
  def install(assets, env = {})
    Dir.mktmpdir do |dir|
      bin = File.join(dir, "bin")
      Dir.mkdir(bin)
      srv = File.join(dir, "srv")
      Dir.mkdir(srv)
      assets.each { |name, body| File.write(File.join(srv, name), body) }
      File.write(File.join(bin, "curl"), <<~SH)
        #!/bin/sh
        out=""; url=""
        while [ $# -gt 0 ]; do
          case "$1" in -o) out="$2"; shift ;; --proto|--tlsv1.2) [ "$1" = --proto ] && shift ;; -*) ;; *) url="$1" ;; esac
          shift
        done
        echo "$url" >> "#{dir}/urls"
        f="#{srv}/$(basename "$url")"
        [ -f "$f" ] || exit 22
        if [ -n "$out" ]; then cp "$f" "$out"; else cat "$f"; fi
      SH
      File.chmod(0o755, File.join(bin, "curl"))
      dest = File.join(dir, "dest")
      full_env = { "PATH" => "#{bin}:#{ENV.fetch("PATH")}", "HOME" => dir, "LIGUS_INSTALL_DIR" => dest }.merge(env)
      out, err, status = Open3.capture3(full_env, "sh", INSTALL)
      urls = File.exist?(File.join(dir, "urls")) ? File.read(File.join(dir, "urls")).split("\n") : []
      yield status.success?, out + err, File.exist?(File.join(dest, "ligus")), urls
    end
  end

  def target
    os = RbConfig::CONFIG["host_os"]
    arch = `uname -m`.strip
    if os.include?("darwin")
      arch == "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin"
    else
      arch == "x86_64" ? "x86_64-unknown-linux-musl" : "aarch64-unknown-linux-musl"
    end
  end

  def sha(body)
    require "digest"
    Digest::SHA256.hexdigest(body)
  end

  def test_good_checksum_installs
    body = "#!/bin/sh\necho ligus\n"
    install({ "ligus-#{target}" => body, "ligus-#{target}.sha256" => "#{sha(body)}  ligus-#{target}\n" }) do |ok, out, installed, _|
      assert ok, out
      assert installed
    end
  end

  def test_bad_checksum_is_refused
    install({ "ligus-#{target}" => "tampered", "ligus-#{target}.sha256" => "#{sha("original")}  ligus-#{target}\n" }) do |ok, out, installed, _|
      refute ok, out
      refute installed, "a binary that fails its checksum was installed"
      assert_includes out, "checksum"
    end
  end

  def test_missing_checksum_is_refused
    install({ "ligus-#{target}" => "x" }) do |ok, out, installed, _|
      refute ok, out
      refute installed
    end
  end

  def test_version_without_v_prefix_is_normalised
    body = "bin"
    assets = { "ligus-#{target}" => body, "ligus-#{target}.sha256" => "#{sha(body)}  ligus-#{target}\n" }
    install(assets, "LIGUS_VERSION" => "0.1.0") do |ok, out, _, urls|
      assert ok, out
      assert(urls.all? { |u| u.include?("/releases/download/v0.1.0/") }, urls.inspect)
    end
  end

  def test_malformed_version_is_refused
    install({}, "LIGUS_VERSION" => "../../evil") do |ok, _out, installed, urls|
      refute ok
      refute installed
      assert_empty urls
    end
  end
end

# Every version in the release must be the tag's version, not just tauri.conf.
class AllVersionsMatch < Minitest::Test
  SCRIPT = File.expand_path("check-release-config.sh", __dir__)

  def test_a_crate_or_package_version_mismatch_fails
    Dir.mktmpdir do |dir|
      %w[app web].each { |d| Dir.mkdir(File.join(dir, d)) }
      FileUtils.cp(File.join(ROOT, "app/tauri.conf.json"), File.join(dir, "app/tauri.conf.json"))
      version = TAURI_CONF.fetch("version")
      File.write(File.join(dir, "web/package.json"), JSON.generate({ "version" => version }))
      lock = File.read(File.join(ROOT, "Cargo.lock"))
      File.write(File.join(dir, "Cargo.lock"), lock)
      out, _e, st = Open3.capture3({ "LIGUS_RELEASE_ROOT" => dir }, SCRIPT, "v#{version}", File.join(dir, "app/tauri.conf.json"))
      assert st.success?, out
      File.write(File.join(dir, "Cargo.lock"), lock.sub(/(name = "ligus-cli"\nversion = ")[^"]+/, '\19.9.9'))
      out, _e, st = Open3.capture3({ "LIGUS_RELEASE_ROOT" => dir }, SCRIPT, "v#{version}", File.join(dir, "app/tauri.conf.json"))
      refute st.success?
      assert_includes out, "ligus-cli"
      File.write(File.join(dir, "Cargo.lock"), lock)
      File.write(File.join(dir, "web/package.json"), JSON.generate({ "version" => "9.9.9" }))
      out, _e, st = Open3.capture3({ "LIGUS_RELEASE_ROOT" => dir }, SCRIPT, "v#{version}", File.join(dir, "app/tauri.conf.json"))
      refute st.success?
      assert_includes out, "package.json"
    end
  end
end
