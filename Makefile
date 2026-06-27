# unlocr packaging orchestrator. Thin: each target shells out to a script.
NAME      := unlocr
CARGO_DIR := .
VERSION   := $(shell sed -n 's/^version *= *"\(.*\)"/\1/p' $(CARGO_DIR)/Cargo.toml | head -1)
BIN       := $(CARGO_DIR)/target/release/$(NAME)

# DESTDIR + PREFIX so distro packagers and `make install` both work.
PREFIX ?= /usr/local
BINDIR := $(PREFIX)/bin

export NAME VERSION

.PHONY: all build test install uninstall deb rpm dist release clean

all: build

build:
	cargo build --release --locked --manifest-path $(CARGO_DIR)/Cargo.toml

test:
	cargo test --locked --manifest-path $(CARGO_DIR)/Cargo.toml

# Bare install (used by `make install` and by distro %install/postinst alike).
install: build
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 0755 "$(BIN)" "$(DESTDIR)$(BINDIR)/$(NAME)"

# Binary only: this is the distro/packager path; it must never delete the user's
# model cache. Full cleanup (binary + cache) is the top-level ./uninstall.sh.
uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/$(NAME)"

# .deb via dpkg-deb (needs: dpkg-deb).
deb: build
	BIN="$(abspath $(BIN))" packaging/deb/build-deb.sh

# .rpm via rpmbuild (needs: rpmbuild, on Fedora/RHEL or with rpm tools).
rpm: build
	BIN="$(abspath $(BIN))" packaging/rpm/build-rpm.sh

# Portable tarball for the install.sh path.
dist: build
	mkdir -p dist
	tar -C $(CARGO_DIR)/target/release -czf dist/$(NAME)-$(VERSION)-$$(uname -s)-$$(uname -m).tar.gz $(NAME)
	@echo "wrote dist/$(NAME)-$(VERSION)-$$(uname -s)-$$(uname -m).tar.gz"

# Tag + push; the GitHub Actions release workflow builds the per-OS binaries.
release:
	./release.sh

clean:
	cargo clean --manifest-path $(CARGO_DIR)/Cargo.toml
	rm -rf dist
