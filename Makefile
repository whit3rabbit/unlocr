# unlocr packaging orchestrator. Thin: each target shells out to a script.
NAME      := unlocr
CARGO_DIR := unlocr
VERSION   := $(shell sed -n 's/^version *= *"\(.*\)"/\1/p' $(CARGO_DIR)/Cargo.toml | head -1)
BIN       := $(CARGO_DIR)/target/release/$(NAME)

# DESTDIR + PREFIX so distro packagers and `make install` both work.
PREFIX ?= /usr/local
BINDIR := $(PREFIX)/bin

export NAME VERSION

.PHONY: all build test install uninstall deb rpm dist clean

all: build

build:
	cargo build --release --manifest-path $(CARGO_DIR)/Cargo.toml

test:
	cargo test --manifest-path $(CARGO_DIR)/Cargo.toml

# Bare install (used by `make install` and by distro %install/postinst alike).
install: build
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 0755 "$(BIN)" "$(DESTDIR)$(BINDIR)/$(NAME)"

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

clean:
	cargo clean --manifest-path $(CARGO_DIR)/Cargo.toml
	rm -rf dist
