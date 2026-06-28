# Homebrew CASK template (GUI app). Source of truth; the update-tap workflow renders
# the @@...@@ tokens from a release's .dmg assets and pushes the result to the tap
# repo's Casks/unlocr.rb. Install: brew install --cask whit3rabbit/tap/unlocr
cask "unlocr" do
  version "@@VERSION@@"

  on_arm do
    url "https://github.com/whit3rabbit/unlocr/releases/download/v#{version}/unlocr_#{version}_aarch64.dmg"
    sha256 "@@SHA_ARM_DMG@@"
  end
  on_intel do
    url "https://github.com/whit3rabbit/unlocr/releases/download/v#{version}/unlocr_#{version}_x64.dmg"
    sha256 "@@SHA_INTEL_DMG@@"
  end

  name "unlocr"
  desc "Desktop OCR: PDFs to markdown via Unlimited-OCR (DeepSeek-OCR) + llama.cpp"
  homepage "https://github.com/whit3rabbit/unlocr"

  depends_on formula: "poppler"

  app "unlocr.app"

  # Unsigned/un-notarized: Gatekeeper will quarantine it. Homebrew strips the
  # quarantine bit on install for casks, but document the manual fix just in case.
  caveats <<~EOS
    unlocr.app is not signed or notarized. If macOS blocks it on first launch
    ("cannot verify developer"), clear the quarantine bit:
      xattr -dr com.apple.quarantine "/Applications/unlocr.app"
    or right-click the app and choose Open.

    The OCR engine also needs llama-server (llama.cpp build >= b8530):
      brew install llama.cpp
  EOS

  zap trash: [
    "~/Library/Caches/unlocr",
  ]
end
