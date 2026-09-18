# omanote — build and install
#
#   make install                       build, then copy to ~/.local/bin (no sudo needed)
#   make uninstall                     remove it again
#   make && sudo make install PREFIX=/usr/local     system-wide
#   make bump VERSION=0.1.0            set the version in Cargo.toml and Cargo.lock (then commit both)
#   make release                       tag the version in Cargo.toml and push the tag;
#                                      GitHub Actions then builds and publishes it

PREFIX  ?= $(HOME)/.local
BINDIR  ?= $(PREFIX)/bin
NAME    := omanote
BIN     := target/release/$(NAME)
VERSION := $(shell sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
TAG     := v$(VERSION)
REMOTE  ?= origin
SOURCES := Cargo.toml Cargo.lock demo.md $(wildcard src/*.rs)

.PHONY: build install uninstall test clean help bump release

build: $(BIN)

# Only calls cargo when a source changed, so `sudo make install` after a plain
# `make` just copies and never runs cargo as root.
$(BIN): $(SOURCES)
	cargo build --release
	@touch $(BIN)

install: $(BIN)
	install -Dm755 $(BIN) $(DESTDIR)$(BINDIR)/$(NAME)
	@echo "Installed $(DESTDIR)$(BINDIR)/$(NAME)"
	@case ":$$PATH:" in \
		*":$(BINDIR):"*) echo "Run it from anywhere:  $(NAME) --help" ;; \
		*) echo "Note: $(BINDIR) is not on your PATH. Add this to your shell profile:"; \
		   echo "  export PATH=\"$(BINDIR):\$$PATH\"" ;; \
	esac

# Removes the program only. Notes and vaults in ~/.omanote are never touched.
uninstall:
	rm -f $(DESTDIR)$(BINDIR)/$(NAME)
	@echo "Removed $(DESTDIR)$(BINDIR)/$(NAME) (your notes in ~/.omanote are untouched)"

test:
	cargo test

# Cargo.lock records the package version too, and the release build uses
# --locked, so the two have to change together.
bump:
	@test "$(origin VERSION)" = "command line" || { echo "usage: make bump VERSION=0.1.0"; exit 1; }
	@echo "$(VERSION)" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$$' || { echo "bump: '$(VERSION)' is not a version like 0.1.0"; exit 1; }
	@sed -i.bak -e '1,/^version = /s/^version = ".*"/version = "$(VERSION)"/' Cargo.toml && rm -f Cargo.toml.bak
	@cargo update --offline -p $(NAME) >/dev/null 2>&1 || cargo update -p $(NAME) >/dev/null
	@echo "Version is now $(VERSION). Commit Cargo.toml and Cargo.lock, then: make release"

# Tags what is committed; it never commits for you. Bump `version` in
# Cargo.toml and commit first, then run this.
release:
	@test -n "$(VERSION)" || { echo "release: no version found in Cargo.toml"; exit 1; }
	@git remote get-url $(REMOTE) >/dev/null 2>&1 || { echo "release: no git remote '$(REMOTE)' — add one with: git remote add $(REMOTE) git@github.com:<you>/omanote.git"; exit 1; }
	@test -z "$$(git status --porcelain)" || { echo "release: uncommitted changes — commit them first:"; git status --short; exit 1; }
	@grep -q '^version = "$(VERSION)"' Cargo.toml && grep -A1 '^name = "omanote"' Cargo.lock | grep -q '"$(VERSION)"' || { echo "release: Cargo.lock does not say $(VERSION) — run 'cargo build' and commit Cargo.lock"; exit 1; }
	@! git rev-parse -q --verify "refs/tags/$(TAG)" >/dev/null || { echo "release: tag $(TAG) already exists — bump the version in Cargo.toml"; exit 1; }
	@! git ls-remote --exit-code --tags $(REMOTE) "refs/tags/$(TAG)" >/dev/null 2>&1 || { echo "release: $(TAG) is already on $(REMOTE) — bump the version in Cargo.toml"; exit 1; }
	cargo test --locked
	git tag -a $(TAG) -m "omanote $(TAG)"
	git push $(REMOTE) $(TAG)
	@echo "Pushed $(TAG). GitHub Actions is building the release now."

clean:
	cargo clean

help:
	@sed -n '1,9p' Makefile
