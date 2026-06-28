# Homebrew FORMULA template (CLI). Source of truth; the update-tap workflow renders
# the @@...@@ tokens from a release's assets and pushes the result to the tap repo's
# Formula/unlocr.rb. Install: brew install whit3rabbit/tap/unlocr
class Unlocr < Formula
  desc "OCR PDFs to markdown via Unlimited-OCR (DeepSeek-OCR) + llama.cpp"
  homepage "https://github.com/whit3rabbit/unlocr"
  version "@@VERSION@@"
  license "MIT"

  depends_on "poppler"

  on_macos do
    on_arm do
      url "https://github.com/whit3rabbit/unlocr/releases/download/v#{version}/unlocr-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "@@SHA_ARM_TGZ@@"
    end
    on_intel do
      url "https://github.com/whit3rabbit/unlocr/releases/download/v#{version}/unlocr-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "@@SHA_INTEL_TGZ@@"
    end
  end

  on_linux do
    url "https://github.com/whit3rabbit/unlocr/releases/download/v#{version}/unlocr-#{version}-x86_64-unknown-linux-musl.tar.gz"
    sha256 "@@SHA_LINUX_TGZ@@"
  end

  def install
    bin.install "unlocr"
  end

  # llama.cpp is a soft dep on purpose: the model needs build >= b8530 and
  # homebrew-core's llama.cpp can lag that, so we recommend rather than pin it.
  def caveats
    <<~EOS
      unlocr needs llama-server (llama.cpp build >= b8530) on PATH:
        brew install llama.cpp
      If the model fails to load, install a newer llama.cpp release manually.
      poppler (pdftoppm) is installed as a hard dependency.
    EOS
  end

  test do
    system bin/"unlocr", "--version"
  end
end
