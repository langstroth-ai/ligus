cask "ligus-app" do
  # Apple Silicon only: the desktop app ships an aarch64 DMG (no Intel build).
  # Placeholder until Ligus v2.0.0 is released: the update-cask workflow
  # rewrites version and sha256.
  version "2.0.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/ligus-ai/ligus/releases/download/v#{version}/Ligus_#{version}_aarch64.dmg"
  name "Ligus"
  desc "Shared LLM workspace for developers (formerly Hive)"
  homepage "https://github.com/ligus-ai/ligus"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on arch: :arm64
  depends_on :macos

  app "Ligus.app"

  # Remove user data on `brew uninstall --zap ligus-app`. Lists the pre-rename
  # identifier too: Ligus copies com.hive.desktop on first launch and never
  # deletes it, so a zap must clear both.
  zap trash: [
    "~/Library/Application Support/ai.ligus.desktop",
    "~/Library/Application Support/com.hive.desktop",
    "~/Library/Caches/ai.ligus.desktop",
    "~/Library/Caches/com.hive.desktop",
    "~/Library/Preferences/ai.ligus.desktop.plist",
    "~/Library/Preferences/com.hive.desktop.plist",
    "~/Library/Saved Application State/ai.ligus.desktop.savedState",
    "~/Library/Saved Application State/com.hive.desktop.savedState",
  ]
end
