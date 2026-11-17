cask "ligus-app" do
  # Apple Silicon only: the desktop app ships an aarch64 DMG (no Intel build).
  # Placeholder until Ligus v0.1.0 is released: the update-cask workflow
  # rewrites version and sha256.
  version "0.1.0"
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

  # Remove Ligus's own data on `brew uninstall --zap ligus-app`. Only the
  # ai.ligus.desktop paths: Ligus 0.1 starts fresh and never touches Hive 1.x
  # data (com.hive.desktop), so neither does its zap.
  zap trash: [
    "~/Library/Application Support/ai.ligus.desktop",
    "~/Library/Caches/ai.ligus.desktop",
    "~/Library/Preferences/ai.ligus.desktop.plist",
    "~/Library/Saved Application State/ai.ligus.desktop.savedState",
  ]
end
