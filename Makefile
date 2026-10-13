# Kiln development tasks.
#
#   make install   build the CLI and link it onto your PATH as `kiln-dev`
#   make           rebuild (the installed link picks it up)
#   make vendor-lexxy VERSION=1.0.2   update the vendored rich text editor
#   make release VERSION=0.0.2        bump, check, commit, tag, then (asked) publish to crates.io

BIN_DIR ?= $(HOME)/.local/bin
BIN_NAME ?= kiln-dev
TARGET := $(CURDIR)/target/debug/kiln

.PHONY: build install uninstall test lint fmt vendor-lexxy release

build:
	cargo build -p kiln-framework-cli

# A symlink, so later `make` runs update the installed command without reinstalling.
install: build
	mkdir -p $(BIN_DIR)
	ln -sf $(TARGET) $(BIN_DIR)/$(BIN_NAME)
	@echo "installed $(BIN_DIR)/$(BIN_NAME) -> $(TARGET)"

uninstall:
	rm -f $(BIN_DIR)/$(BIN_NAME)

test:
	cargo test

lint:
	cargo fmt --all --check
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt --all

vendor-lexxy:
	@test -n "$(VERSION)" || (echo "usage: make vendor-lexxy VERSION=1.0.2" && exit 1)
	scripts/vendor-lexxy.sh $(VERSION)

release:
	@test -n "$(VERSION)" || (echo "usage: make release VERSION=0.0.2" && exit 1)
	scripts/release.sh $(VERSION)
