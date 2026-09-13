BIN := ztop
DIST := dist
VERSION := $(shell awk -F'"' '/^version/{print $$2; exit}' Cargo.toml)

TARGETS := \
	x86_64-apple-darwin \
	aarch64-apple-darwin \
	x86_64-unknown-linux-gnu \
	aarch64-unknown-linux-gnu

.PHONY: all build release run clean fmt lint test \
	cross-mac cross-mac-intel cross-mac-arm \
	cross-linux cross-linux-amd64 cross-linux-arm64 \
	dist dist-mac dist-linux $(TARGETS)

all: release

build:
	cargo build

release:
	cargo build --release

run:
	cargo run

test:
	cargo test

fmt:
	cargo fmt

lint:
	cargo clippy --all-targets -- -D warnings

clean:
	cargo clean
	rm -rf $(DIST)

# ---- native macOS universal binary ----
cross-mac: cross-mac-intel cross-mac-arm
	mkdir -p $(DIST)
	lipo -create -output $(DIST)/$(BIN)-macos-universal \
		target/x86_64-apple-darwin/release/$(BIN) \
		target/aarch64-apple-darwin/release/$(BIN)

cross-mac-intel:
	rustup target add x86_64-apple-darwin
	cargo build --release --target x86_64-apple-darwin

cross-mac-arm:
	rustup target add aarch64-apple-darwin
	cargo build --release --target aarch64-apple-darwin

# ---- Linux via cross (docker-based) ----
cross-linux: cross-linux-amd64 cross-linux-arm64

cross-linux-amd64:
	cross build --release --target x86_64-unknown-linux-gnu

cross-linux-arm64:
	cross build --release --target aarch64-unknown-linux-gnu

# ---- generic per-target build ----
$(TARGETS):
	rustup target add $@ 2>/dev/null || true
	cargo build --release --target $@

# ---- packaging ----
dist: dist-mac dist-linux

dist-mac: cross-mac
	mkdir -p $(DIST)
	tar -C target/x86_64-apple-darwin/release -czf $(DIST)/$(BIN)-$(VERSION)-x86_64-apple-darwin.tar.gz $(BIN)
	tar -C target/aarch64-apple-darwin/release -czf $(DIST)/$(BIN)-$(VERSION)-aarch64-apple-darwin.tar.gz $(BIN)
	tar -C $(DIST) -czf $(DIST)/$(BIN)-$(VERSION)-macos-universal.tar.gz $(BIN)-macos-universal

dist-linux: cross-linux
	mkdir -p $(DIST)
	tar -C target/x86_64-unknown-linux-gnu/release -czf $(DIST)/$(BIN)-$(VERSION)-x86_64-unknown-linux-gnu.tar.gz $(BIN)
	tar -C target/aarch64-unknown-linux-gnu/release -czf $(DIST)/$(BIN)-$(VERSION)-aarch64-unknown-linux-gnu.tar.gz $(BIN)
