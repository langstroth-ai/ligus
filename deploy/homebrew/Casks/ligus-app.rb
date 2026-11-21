cask "ligus-app" do
  # Apple Silicon only: the desktop app ships an aarch64 DMG (no Intel build).
  # Placeholder until Ligus v0.1.0 is released: the update-cask workflow
  # rewrites version and sha256.
  version "0.1.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/langstroth-ai/ligus/releases/download/v#{version}/Ligus_#{version}_aarch64.dmg"
  name "Ligus"
  desc "Shared LLM workspace for developers"
  homepage "https://github.com/langstroth-ai/ligus"

  livecheck do
    url :url
    strategy :github_latest
  end

  # The app updates itself (Tauri updater): brew upgrade skips it unless
  # --greedy. The release's cask job adds this line to the tap's copy.
  auto_updates true

  depends_on arch: :arm64
  depends_on :macos

  app "Ligus.app"

  # Remove Ligus's own data on `brew uninstall --zap ligus-app`: only the
  # ai.ligus.desktop paths.
  zap trash: [
    "~/Library/Application Support/ai.ligus.desktop",
    "~/Library/Caches/ai.ligus.desktop",
    "~/Library/Preferences/ai.ligus.desktop.plist",
    "~/Library/Saved Application State/ai.ligus.desktop.savedState",
  ]
end
