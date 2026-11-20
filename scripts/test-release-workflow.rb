#!/usr/bin/env ruby
# frozen_string_literal: true

# Checks the desktop and CLI release workflows against how the pinned
# release actions really behave, so a rerun can never overwrite the release
# notes the owner wrote, the desktop assets, CLI binaries and latest.json
# always land on the tag's one DRAFT release, nothing is published until
# every build succeeded, the tap bumps follow publish, and ci.yml builds every
# CLI target the release ships.
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
# softprops/action-gh-release v3.0.4 (no longer used: release-cli.yml)
#   - Publishes a draft release at the end unless draft: true, so it would
#     publish the draft before the other builds finish. release-cli uploads
#     with `gh release upload` instead.
#
# gh release upload/view <tag> (cli/cli pkg/cmd/release/shared/fetch.go)
#   - Resolves the tag through REST releases/tags/<tag> (published only) and
#     GraphQL repository.release(tagName:) (finds a draft) in parallel, so it
#     uploads into a draft by tag. It never changes draft/prerelease state.

require "json"
require "minitest/autorun"
require "open3"
require "tmpdir"
require "fileutils"
require "yaml"

ROOT = File.expand_path("..", __dir__)
RELEASE = YAML.safe_load(File.read(File.join(ROOT, ".github/workflows/release.yml")))
RELEASE_CLI = YAML.safe_load(File.read(File.join(ROOT, ".github/workflows/release-cli.yml")))
CI = YAML.safe_load(File.read(File.join(ROOT, ".github/workflows/ci.yml")))
CUT = YAML.safe_load(File.read(File.join(ROOT, ".github/workflows/cut-release.yml")))
UPDATE_CASK = YAML.safe_load(File.read(File.join(ROOT, ".github/workflows/update-cask.yml")))
BUILD_CLI = YAML.safe_load(File.read(File.join(ROOT, ".github/actions/build-cli/action.yml")))
TAURI_CONF = JSON.parse(File.read(File.join(ROOT, "app/tauri.conf.json")))
ENSURE = File.join(ROOT, "scripts/ensure-release.sh")

def steps(workflow, job)
  workflow.fetch("jobs").fetch(job).fetch("steps")
end

def run_text(workflow, job)
  steps(workflow, job).map { |s| s["run"].to_s }.join("\n")
end

# YAML parses the bare key `on` as true.
def triggers(workflow)
  workflow["on"] || workflow[true]
end

def matrix_targets(workflow, job)
  workflow.fetch("jobs").fetch(job).dig("strategy", "matrix", "include").to_h { |e| [e["target"], e["runner"]] }
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

  # The release job creates the draft before any build starts, so "a draft
  # exists" is the case on every run, first or rerun.
  def test_installers_land_on_the_existing_draft
    out = tauri_action_outcome(with, :draft)
    assert_equal :draft, out[:assets_on],
                 "tauri-action would not upload to the draft (#{out[:assets_on]})"
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
  def upload_step
    steps(RELEASE_CLI, "build").find { |s| s["run"].to_s.include?("gh release upload") }
  end

  # action-gh-release publishes a draft when it finishes; nothing in the CLI
  # workflow may use it.
  def test_cli_never_uses_a_release_action_that_publishes
    RELEASE_CLI["jobs"].each_value do |job|
      job.fetch("steps", []).each do |st|
        refute st["uses"].to_s.start_with?("softprops/action-gh-release@"), "action-gh-release publishes the draft"
      end
    end
  end

  def test_cli_uploads_to_the_draft_the_release_job_made
    refute_nil upload_step, "release-cli never uploads its binaries"
    run = upload_step["run"]
    assert_includes run, "--clobber", "a rerun could not replace a target's binary"
    assert_operator run.index("databaseId,isDraft"), :<, run.index("gh release upload"),
                    "check gh resolves the tag to the release job's draft before uploading"
    assert_includes run, '"$RELEASE_ID true"'
    assert_equal "${{ inputs.release-id }}", upload_step.dig("env", "RELEASE_ID")
    cli = RELEASE.fetch("jobs").fetch("cli")
    assert_includes Array(cli["needs"]), "release"
    assert_equal "${{ needs.release.outputs.id }}", cli.dig("with", "release-id")
  end

  # Only release.yml's publish job may make the release public.
  def test_cli_never_publishes
    text = RELEASE_CLI["jobs"].keys.map { |j| run_text(RELEASE_CLI, j) }.join("\n")
    refute_match(/draft=false|release edit|-X PATCH|--latest/, text)
  end

  # Tag-triggered on its own, it would run outside the publish gate.
  def test_cli_runs_only_as_part_of_release_yml
    assert_equal ["workflow_call"], triggers(RELEASE_CLI).keys
    assert_equal "./.github/workflows/release-cli.yml", RELEASE.dig("jobs", "cli", "uses")
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
    # Fake gh: state in $STATE (none|published|draft|two-drafts|race|no-tag),
    # calls logged to $LOG.
    echo "$*" >> "$LOG"
    case "$1 $2" in
      "api repos/o/r/git/ref/tags/v0.1.0")
        [ "$STATE" = no-tag ] && { echo "gh: Not Found (HTTP 404)" >&2; exit 1; }
        exit 0 ;;
      "api repos/o/r/releases/tags/v0.1.0")
        if [ "$STATE" = published ]; then echo 41; exit 0; fi
        echo "gh: Not Found (HTTP 404)" >&2; exit 1 ;;
      "api --paginate")
        case "$STATE" in
          draft) echo 42 ;;
          two-drafts) echo 42; echo 44 ;;
          race) [ -f "$STATE_DIR/created" ] && echo 42 ;;
        esac
        exit 0 ;;
      "api -X")
        if [ "$STATE" = race ]; then touch "$STATE_DIR/created"; echo "HTTP 502" >&2; exit 1; fi
        echo 43; exit 0 ;;
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

  # Building into a published release would put new assets on a live one.
  def test_refuses_a_published_release
    out, err, status, log = run_ensure("published")
    refute status.success?
    assert_equal "", out
    assert_includes err, "already has a published release"
    refute_match(/-X POST|-X PATCH|release edit|release create/, log)
  end

  def test_reuses_the_draft_on_a_rerun
    out, _err, status, log = run_ensure("draft")
    assert status.success?
    assert_equal "42", out
    refute_match(/-X |release create|release edit/, log)
  end

  def test_refuses_two_drafts_for_one_tag
    _out, err, status, log = run_ensure("two-drafts")
    refute status.success?
    assert_includes err, "several drafts"
    refute_match(/-X /, log)
  end

  def test_creates_a_draft_prerelease_on_the_pushed_tag
    out, _err, status, log = run_ensure("none")
    assert status.success?, log
    assert_equal "43", out, "the id must come from the create itself, not a lagging list"
    create = log.lines.find { |l| l.start_with?("api -X POST") }
    refute_nil create
    assert_includes create, "draft=true"
    assert_includes create, "prerelease=true"
    assert_includes create, "tag_name=v0.1.0"
    assert_operator log.index("git/ref/tags/v0.1.0"), :<, log.index("api -X POST")
  end

  def test_an_unpushed_tag_gets_no_draft
    _out, err, status, log = run_ensure("no-tag")
    refute status.success?
    assert_includes err, "push it first"
    refute_match(/-X POST/, log)
  end

  def test_a_failed_create_looks_again
    out, _err, status, _log = run_ensure("race")
    assert status.success?
    assert_equal "42", out
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

  # On a case-insensitive filesystem target/release/Ligus is target/release/ligus:
  # a desktop build would overwrite the CLI.
  def test_desktop_binary_must_not_collide_with_the_cli
    Dir.mktmpdir do |dir|
      conf = JSON.parse(File.read(CONF))
      %w[Ligus LIGUS ligus].each do |name|
        conf["mainBinaryName"] = name
        path = File.join(dir, "tauri.conf.json")
        File.write(path, JSON.generate(conf))
        ok, out = run_check("v#{conf["version"]}", path)
        refute ok, name
        assert_includes out, "collides with the CLI binary"
      end
    end
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
  TAG_REF = "${{ inputs.tag && format('refs/tags/{0}', inputs.tag) || github.ref }}"

  def checkouts(workflow)
    workflow.fetch("jobs").flat_map do |name, job|
      job.fetch("steps", []).select { |s| s["uses"].to_s.start_with?("actions/checkout@") }.map { |s| [name, s] }
    end
  end

  def test_every_release_checkout_is_the_tag
    checkouts(RELEASE).each do |job, step|
      assert_equal TAG_REF, step.dig("with", "ref"), "#{job} would build the dispatching branch, not the tag"
    end
    # release-cli.yml is called with the tag release.yml resolved.
    assert_equal "${{ inputs.tag || github.ref_name }}", RELEASE.dig("jobs", "cli", "with", "tag")
    checkouts(RELEASE_CLI).each do |job, step|
      assert_equal "refs/tags/${{ inputs.tag }}", step.dig("with", "ref"), "release-cli #{job} would not build the tag"
    end
  end

  # Called by cut-release.yml, github.event is the caller's dispatch (its
  # inputs are `version`/`ref`), so the tag must only come from `inputs`.
  def test_tag_never_comes_from_the_event_payload
    [RELEASE, RELEASE_CLI].each do |wf|
      refute_includes JSON.dump(wf), "github.event.inputs", "github.event.inputs is the caller's when called"
    end
    assert_equal({ "type" => "string", "required" => true }, triggers(RELEASE)["workflow_call"]["inputs"]["tag"].slice("type", "required"))
  end

  def test_cli_release_checks_the_tag_against_the_version_first
    st = steps(RELEASE_CLI, "build")
    check = st.index { |s| s["run"].to_s.include?("check-release-config.sh") }
    build = st.index { |s| s["uses"] == "./.github/actions/build-cli" }
    refute_nil check, "release-cli never checks the tag matches the version"
    refute_nil build
    assert_operator check, :<, build
  end
end

# The CLI binaries ship with a SHA-256 file each, and install.sh refuses a
# binary that does not match it.
class CliChecksums < Minitest::Test
  INSTALL = File.join(ROOT, "install.sh")

  def test_release_cli_attaches_a_checksum_per_binary
    step = steps(RELEASE_CLI, "build").find { |s| s["run"].to_s.include?("gh release upload") }
    assert_includes step["run"], '"ligus-$TARGET" "ligus-$TARGET.sha256"'
    assert_equal "${{ matrix.target }}", step.dig("env", "TARGET")
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
      %w[app web crates crates/ligus-cli].each { |d| Dir.mkdir(File.join(dir, d)) }
      FileUtils.cp(File.join(ROOT, "app/tauri.conf.json"), File.join(dir, "app/tauri.conf.json"))
      FileUtils.cp(File.join(ROOT, "crates/ligus-cli/Cargo.toml"), File.join(dir, "crates/ligus-cli/Cargo.toml"))
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

# Release builds bake the code-host device-flow client ids the code reads
# (ligus-forge option_env!), not the removed 0.x sign-in id.
class ForgeClientIds < Minitest::Test
  def test_desktop_and_cli_builds_get_the_forge_client_ids
    tauri_env = tauri_step.fetch("env")
    cli_env = steps(RELEASE_CLI, "build").find { |s| s["name"] == "Build ligus CLI" }.fetch("env", {})
    [tauri_env, cli_env].each do |env|
      refute env.key?("LIGUS_GITHUB_CLIENT_ID"), "LIGUS_GITHUB_CLIENT_ID is read by nothing since 0.1"
      %w[LIGUS_GITHUB_APP_CLIENT_ID LIGUS_GITLAB_CLIENT_ID].each { |k| assert env.key?(k), "#{k} is not baked in" }
    end
  end
end

# The release stays a draft until every build succeeded: one job publishes,
# it waits on everything, and the tap follows it.
class DraftUntilComplete < Minitest::Test
  BUILD_JOBS = %w[release build cli verify-updater].freeze

  def publish
    RELEASE.fetch("jobs").fetch("publish")
  end

  def test_publish_needs_every_build
    assert_equal BUILD_JOBS.sort, Array(publish["needs"]).sort
  end

  # Without an explicit condition a skipped job would not block publish
  # under !cancelled(); require success from each one.
  def test_publish_requires_each_build_to_succeed
    cond = publish["if"].to_s
    BUILD_JOBS.each do |j|
      assert_includes cond, "needs.#{j}.result == 'success'", "publish does not wait for #{j} to succeed"
    end
    refute_match(/always\(\)|failure\(\)/, cond)
  end

  def test_verify_updater_waits_for_every_desktop_build
    assert_includes Array(RELEASE.dig("jobs", "verify-updater", "needs")), "build"
    assert_includes RELEASE.dig("jobs", "verify-updater", "if"), "needs.build.result == 'success'"
  end

  def test_publish_checks_the_draft_then_publishes_a_prerelease_by_id
    st = publish["steps"]
    assets = st.index { |s| s["run"].to_s.include?("scripts/check-release-assets.py") }
    latest = st.index { |s| s["run"].to_s.include?("scripts/verify-latest-json.py") }
    patch = st.index { |s| s["run"].to_s.include?("-X PATCH") }
    refute_nil assets, "publish does not check the draft's assets"
    refute_nil latest, "publish does not check latest.json"
    refute_nil patch, "publish never publishes"
    assert_operator assets, :<, patch
    assert_operator latest, :<, patch
    assert_includes st[latest]["run"], "--platforms macos,windows,linux",
                    "a published release must serve every platform, not just the ones this run built"
    run = st[patch]["run"]
    assert_includes run, "releases/$RELEASE_ID"
    assert_includes run, "draft=false"
    assert_includes run, "prerelease=true"
    assert_includes run, "make_latest=false", "promotion to Latest stays manual"
    refute_includes run, "--latest"
    assert_operator run.index('"true $TAG"'), :<, run.index("-X PATCH"), "publish must check it holds the tag's draft"
  end

  # No other job, in either workflow, flips draft or Latest.
  def test_only_publish_publishes
    [[RELEASE, "release.yml"], [RELEASE_CLI, "release-cli.yml"]].each do |wf, file|
      wf["jobs"].each do |name, job|
        next if wf.equal?(RELEASE) && name == "publish"

        text = job.fetch("steps", []).map { |s| s["run"].to_s }.join("\n")
        refute_match(/draft=false|-X PATCH|release edit|--latest/, text, "#{file} #{name} changes the release's state")
        job.fetch("steps", []).each do |s|
          refute s["uses"].to_s.start_with?("softprops/action-gh-release@"), "#{file} #{name} would publish the draft"
          w = s["with"] || {}
          %w[releaseDraft tagName prerelease].each { |k| refute w.key?(k), "#{file} #{name}: tauri-action #{k}" }
        end
      end
    end
  end

  def test_release_job_creates_a_draft
    assert_includes run_text(RELEASE, "release"), "scripts/ensure-release.sh"
    script = File.read(ENSURE)
    assert_match(/-F draft=true/, script)
    refute_match(/gh release create/, script)
  end

  def test_tap_bumps_run_after_publish
    %w[cask formula].each do |j|
      job = RELEASE.fetch("jobs").fetch(j)
      assert_equal ["publish"], Array(job["needs"]), "#{j} must wait for publish"
      assert_includes job["if"].to_s, "needs.publish.result == 'success'"
    end
    refute RELEASE_CLI.fetch("jobs").key?("formula"), "the formula bump must follow publish, in release.yml"
  end

  def test_manual_cask_bump_refuses_a_draft
    st = steps(UPDATE_CASK, "bump")
    guard = st.index { |s| s["run"].to_s.include?("releases/tags/$TAG") }
    bump = st.index { |s| s["run"].to_s.include?("homebrew-ligus.git") }
    refute_nil guard, "update-cask.yml would bump the tap to a draft"
    assert_operator guard, :<, bump
  end

  def test_one_release_run_per_tag_never_cancelled
    c = RELEASE.fetch("concurrency")
    assert_includes c["group"], "inputs.tag || github.ref_name"
    assert_equal false, c["cancel-in-progress"]
  end
end

# ci.yml builds the CLI for every target release-cli.yml ships, the same way.
class CiCoversReleaseTargets < Minitest::Test
  def ci_job
    CI.fetch("jobs").fetch("cli-targets")
  end

  def test_ci_builds_every_release_target_on_the_same_runner
    release = matrix_targets(RELEASE_CLI, "build")
    assert_equal 4, release.size
    assert_equal release, matrix_targets(CI, "cli-targets")
    assert_equal "ubuntu-24.04-arm", release["aarch64-unknown-linux-musl"]
  end

  def test_both_build_through_the_shared_action
    rel = steps(RELEASE_CLI, "build").find { |s| s["uses"] == "./.github/actions/build-cli" }
    ci = ci_job["steps"].find { |s| s["uses"] == "./.github/actions/build-cli" }
    refute_nil rel
    refute_nil ci
    assert_equal "release", rel.dig("with", "profile")
    assert_equal "debug", ci.dig("with", "profile")
    [rel, ci].each { |s| assert_equal "${{ matrix.target }}", s.dig("with", "target") }
    # Nobody builds the CLI beside the action, where it could drift.
    [run_text(RELEASE_CLI, "build"), ci_job["steps"].map { |s| s["run"].to_s }.join].each do |t|
      refute_match(/cargo build|rustup|musl-tools/, t)
    end
  end

  def test_shared_action_sets_up_the_target_like_the_release
    text = BUILD_CLI.dig("runs", "steps").map { |s| s["run"].to_s }.join("\n")
    assert_includes text, 'rustup target add "$TARGET"'
    assert_includes text, "musl-tools"
    assert_match(/cargo build .*--locked -p ligus-cli --target "\$TARGET"/, text)
    assert_includes text, "scripts/check-no-test-presence.sh"
    musl = BUILD_CLI.dig("runs", "steps").find { |s| s["run"].to_s.include?("musl-tools") }
    assert_equal "contains(inputs.target, 'linux-musl')", musl["if"]
  end

  def test_ci_ok_requires_the_job_and_it_runs_on_cli_changes
    assert_includes Array(CI.dig("jobs", "ci-ok", "needs")), "cli-targets"
    assert_includes ci_job["if"], "needs.changes.outputs.cli == 'true'"
    diff = steps(CI, "changes").map { |s| s["run"].to_s }.join("\n")
    cli_re = diff.lines.find { |l| l.include?("build-cli") }
    refute_nil cli_re, "the changes job never sets cli"
    re = Regexp.new(cli_re[/'(\^[^']+)'/, 1])
    %w[crates/ligus-cli/src/main.rs crates/ligus-runtime/src/lib.rs Cargo.toml Cargo.lock
       .github/actions/build-cli/action.yml .github/workflows/ci.yml].each { |f| assert_match re, f }
    %w[docs/packaging.md web/src/main.ts].each { |f| refute_match re, f }
    assert triggers(CI).key?("merge_group"), "the merge queue must run ci"
  end

  def test_asset_check_and_formula_use_the_same_targets
    release = matrix_targets(RELEASE_CLI, "build").keys.sort
    script = File.read(File.join(ROOT, "scripts/check-release-assets.py"))
    listed = script[/CLI_TARGETS = \((.*?)\)/m, 1].scan(/"([^"]+)"/).flatten
    assert_equal release, listed.sort
    formula = run_text(RELEASE, "formula")[/for t in ([^;]+);/, 1].split
    assert_equal release, formula.sort
  end
end

# scripts/check-release-assets.py on fixture drafts.
class CheckReleaseAssets < Minitest::Test
  SCRIPT = File.join(ROOT, "scripts/check-release-assets.py")
  TARGETS = %w[aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl].freeze
  HASH = "a" * 64

  def draft
    assets = [{ "name" => "Ligus_0.1.0_aarch64.dmg", "state" => "uploaded" },
              { "name" => "latest.json", "state" => "uploaded" }]
    sums = {}
    TARGETS.each do |t|
      assets << { "name" => "ligus-#{t}", "state" => "uploaded", "digest" => "sha256:#{HASH}" }
      assets << { "name" => "ligus-#{t}.sha256", "state" => "uploaded" }
      sums["ligus-#{t}.sha256"] = "#{HASH}  ligus-#{t}\n"
    end
    [assets, sums]
  end

  def check(assets, sums)
    Dir.mktmpdir do |dir|
      File.write(File.join(dir, "assets.json"), JSON.dump(assets))
      sums.each { |n, v| File.write(File.join(dir, n), v) }
      out, _err, status = Open3.capture3("python3", "-I", SCRIPT, "--from-dir", dir, "--tag", "v0.1.0")
      [status.success?, out]
    end
  end

  def test_complete_draft_passes
    ok, out = check(*draft)
    assert ok, out
  end

  def test_missing_cli_target_fails
    assets, sums = draft
    assets.reject! { |a| a["name"] == "ligus-aarch64-unknown-linux-musl" }
    ok, out = check(assets, sums)
    refute ok
    assert_includes out, "ligus-aarch64-unknown-linux-musl: missing"
  end

  def test_missing_checksum_or_dmg_fails
    assets, sums = draft
    assets.reject! { |a| a["name"] == "ligus-x86_64-apple-darwin.sha256" || a["name"].end_with?(".dmg") }
    ok, out = check(assets, sums)
    refute ok
    assert_includes out, "ligus-x86_64-apple-darwin.sha256: missing"
    assert_includes out, "Ligus_0.1.0_aarch64.dmg: missing"
  end

  def test_checksum_that_does_not_match_the_digest_fails
    assets, sums = draft
    sums["ligus-x86_64-unknown-linux-musl.sha256"] = "#{'b' * 64}  ligus-x86_64-unknown-linux-musl\n"
    ok, out = check(assets, sums)
    refute ok
    assert_includes out, "differs"
  end

  def test_checksum_for_another_file_fails
    assets, sums = draft
    sums["ligus-aarch64-apple-darwin.sha256"] = "#{HASH}  ligus-x86_64-apple-darwin\n"
    ok, out = check(assets, sums)
    refute ok
    assert_includes out, "not a sha256 line"
  end

  def test_unfinished_upload_fails
    assets, sums = draft
    assets.find { |a| a["name"] == "latest.json" }["state"] = "starter"
    ok, out = check(assets, sums)
    refute ok
    assert_includes out, "upload not finished"
  end

  def test_no_digest_from_github_still_checks_presence
    assets, sums = draft
    assets.each { |a| a.delete("digest") }
    ok, out = check(assets, sums)
    assert ok, out
  end
end

# cut-release.yml: the normal way to release, from GitHub, never a laptop.
class CutRelease < Minitest::Test
  def cut_steps
    steps(CUT, "cut")
  end

  def idx(text)
    i = cut_steps.index { |s| s["run"].to_s.include?(text) || s["uses"].to_s.include?(text) }
    refute_nil i, "cut-release has no step with #{text}"
    i
  end

  def test_dispatch_only_with_a_version
    assert_equal ["workflow_dispatch"], triggers(CUT).keys
    inputs = triggers(CUT)["workflow_dispatch"]["inputs"]
    assert inputs["version"]["required"]
    refute inputs["ref"]["required"]
  end

  def test_least_privilege
    assert_equal({}, CUT["permissions"], "nothing by default")
    assert_equal({ "contents" => "write", "checks" => "read" }, CUT.dig("jobs", "cut", "permissions"))
    assert_equal({ "contents" => "write" }, CUT.dig("jobs", "release", "permissions"))
  end

  def test_order_of_checks_before_the_tag
    admin = idx("collaborators/$who/permission")
    sha = idx("repos/$GITHUB_REPOSITORY/commits/$ref")
    ci = idx("check_name=ci-ok")
    config = idx("check-release-config.sh")
    tag = idx("git tag -a")
    assert_operator admin, :<, sha
    assert_operator sha, :<, ci
    assert_operator ci, :<, config
    assert_operator config, :<, tag
    tag_run = cut_steps[tag]["run"]
    assert_operator tag_run.index("git ls-remote --tags"), :<, tag_run.index("git tag -a"), "refuse an existing tag first"
    refute_match(/push.*(-f|--force)/, tag_run)
  end

  def test_admins_only_including_reruns
    run = cut_steps[idx("collaborators/$who/permission")]
    assert_includes run["run"], '[ "$perm" != admin ]'
    assert_equal "${{ github.actor }}", run.dig("env", "ACTOR")
    assert_equal "${{ github.triggering_actor }}", run.dig("env", "TRIGGERING_ACTOR")
  end

  def test_ci_ok_must_have_passed_on_that_exact_commit
    st = cut_steps[idx("check_name=ci-ok")]
    assert_equal "${{ steps.sha.outputs.sha }}", st.dig("env", "SHA")
    assert_includes st["run"], "commits/$SHA/check-runs"
    assert_includes st["run"], '.conclusion == "success"'
    assert_includes st["run"], '.app.slug == "github-actions"'
    checkout = cut_steps[idx("actions/checkout@")]
    assert_equal "${{ steps.sha.outputs.sha }}", checkout.dig("with", "ref"), "the checks and the tag must be on one commit"
    assert_includes cut_steps[idx("git tag -a")]["run"], '"$SHA"'
  end

  # The tag is pushed with GITHUB_TOKEN, which does not trigger release.yml's
  # push: tags, so the call below is the only pipeline run.
  def test_pipeline_is_called_once_with_the_tag
    rel = CUT.dig("jobs", "release")
    assert_equal "./.github/workflows/release.yml", rel["uses"]
    assert_equal "${{ needs.cut.outputs.tag }}", rel.dig("with", "tag")
    assert_equal ["cut"], Array(rel["needs"])
    assert_equal "inherit", rel["secrets"]
    checkout = cut_steps[idx("actions/checkout@")]
    refute checkout.fetch("with", {}).key?("token"), "a PAT/App token would make the tag push start release.yml too"
    refute_includes JSON.dump(CUT), "secrets.HOMEBREW", "cut-release itself needs no other secrets"
  end

  def test_tag_push_fallback_still_works
    assert_equal ["v*"], triggers(RELEASE)["push"]["tags"]
    assert triggers(RELEASE).key?("workflow_call")
  end
end
