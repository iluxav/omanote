# omanote — build and install
#
#   make install                       build, then copy to ~/.local/bin (no sudo needed)
#   make uninstall                     remove it again
#   make && sudo make install PREFIX=/usr/local     system-wide
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

.PHONY: build install uninstall test clean help release

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
	@sed -n '1,8p' Makefile
