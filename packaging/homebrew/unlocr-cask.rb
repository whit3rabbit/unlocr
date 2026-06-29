# Homebrew CASK template (GUI app). Source of truth; the update-tap workflow renders
# the @@...@@ tokens from a release's .dmg assets and pushes the result to the tap
# repo's Casks/unlocr.rb. Install: brew install --cask whit3rabbit/tap/unlocr
#
# Style matches the rest of the tap (arch/sha256 keyed by arm:/intel:). The dmg arch
# tokens are tauri's: aarch64 (Apple silicon) and x64 (Intel).
cask "unlocr" do
  arch arm: "aarch64", intel: "x64"

  version "@@VERSION@@"
  sha256 arm:   "@@SHA_ARM_DMG@@",
         intel: "@@SHA_INTEL_DMG@@"

  url "https://github.com/whit3rabbit/unlocr/releases/download/v#{version}/unlocr_#{version}_#{arch}.dmg"
  name "unlocr"
  desc "Desktop OCR: PDFs to markdown via Unlimited-OCR (DeepSeek-OCR) + llama.cpp"
  homepage "https://github.com/whit3rabbit/unlocr"

  depends_on formula: "poppler"
  # pandoc is an OPTIONAL export-only dep (md -> docx/odt/rtf/html/txt). Casks have
  # no weak deps, so rather than force it on every install (and block install when
  # it is unavailable), it is documented as a caveats note below; missing pandoc
  # only disables export, never OCR.

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

    Optional: install pandoc to enable review-pane export (md -> docx/odt/rtf/html/txt):
      brew install pandoc
  EOS

  zap trash: [
    "~/Library/Caches/unlocr",
  ]
end
