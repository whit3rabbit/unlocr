# Packages a prebuilt unlocr binary (built by `make build`/cargo). We do not
# compile inside rpmbuild to avoid a network-fetching cargo build in mock;
# the binary is shipped in as Source0. Pass: rpmbuild -bb --define "version X"
#   --define "_sourcedir <dir with unlocr binary>"
Name:           unlocr
Version:        %{version}
Release:        1%{?dist}
Summary:        OCR PDFs to markdown via Unlimited-OCR (DeepSeek-OCR) + llama.cpp

License:        MIT
URL:            https://huggingface.co/sahilchachra/Unlimited-OCR-GGUF
Source0:        unlocr

# pdftoppm. llama-server (llama.cpp) is also required at runtime but is not
# packaged in Fedora/EPEL, so it cannot be a Requires; see %post.
Requires:       poppler-utils

%description
Thin Rust wrapper that rasterizes PDF pages with pdftoppm (poppler-utils) and
runs a persistent llama.cpp llama-server to convert each page to markdown.

Requires llama.cpp's llama-server (build >= b8530) on PATH, installed
separately. See the project README.

%prep
# nothing: prebuilt binary

%build
# nothing: prebuilt binary

%install
install -Dm0755 %{_sourcedir}/unlocr %{buildroot}%{_bindir}/unlocr

%post
if ! command -v llama-server >/dev/null 2>&1; then
  echo "unlocr: llama-server not found on PATH."
  echo "      Install llama.cpp (build >= b8530): https://github.com/ggml-org/llama.cpp"
fi

%files
%{_bindir}/unlocr

%changelog
* Sat Jun 27 2026 unlocr maintainers <noreply@example.com> - 0.1.0-1
- Initial package.
