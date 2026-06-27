# Packaging unlocr

`unlocr` is a Rust binary. Two runtime dependencies, neither bundled:

| Dep | Binary | Debian/Ubuntu | Fedora/RHEL | macOS | Windows |
|-----|--------|---------------|-------------|-------|---------|
| poppler | `pdftoppm` | `poppler-utils` | `poppler-utils` | `brew install poppler` | scoop `poppler` |
| llama.cpp (>= b8530) | `llama-server` | not packaged | not packaged | `brew install llama.cpp` | scoop `llama-cpp` |

`poppler-utils` is a declared package dependency in the .deb and .rpm.
`llama.cpp` is **not** in apt/dnf, so it cannot be declared; the post-install
script warns if `llama-server` is missing. Install it from
<https://github.com/ggml-org/llama.cpp>.

## Build targets (Makefile, repo root)

```bash
make build      # cargo release build
make test       # cargo test
make install    # install to $PREFIX/bin (default /usr/local, honors DESTDIR)
make uninstall
make deb        # -> dist/unlocr_<version>_<arch>.deb   (needs dpkg-deb)
make rpm        # -> dist/unlocr-<version>-1.<arch>.rpm  (needs rpmbuild)
make dist       # -> dist/unlocr-<version>-<os>-<arch>.tar.gz
make clean
```

## Per-OS install

- macOS / Linux: `./install.sh` (builds, installs to `/usr/local/bin`, checks deps).
  Override target: `PREFIX=$HOME/.local ./install.sh`.
- Windows: `powershell -ExecutionPolicy Bypass -File packaging\windows\install.ps1`
  (builds, installs to `%LOCALAPPDATA%\Programs\unlocr`, adds to PATH, pulls deps via scoop).

## apt/yum repo distribution

The `.deb` and `.rpm` are standalone; users install with `apt install ./x.deb`
or `dnf install ./x.rpm`. To serve from a repo:

- deb: drop into a `reprepro`/`aptly` repo.
- rpm: `createrepo_c` over a dir, ship a `.repo` file pointing at it.

Building each native package needs its native toolchain (`dpkg-deb` on Debian,
`rpmbuild` on Fedora/RHEL). Cross-arch packages need the binary built for that
arch (cross-compile or build on that arch); the scripts just wrap whatever
binary `make build` produced.

## Windows MSI (not included)

The PowerShell installer covers Windows. A signed `.msi` would need WiX
Toolset + a code-signing cert: out of scope here. `scoop`/`winget` manifests are
the lighter path if you publish to those buckets.
