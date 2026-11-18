# SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
# SPDX-License-Identifier: AGPL-3.0-only

.DEFAULT_GOAL := help
.PHONY: help check fmt clippy test bench bench-web bench-latency test-web web run release dist deb docker-build docker-up

# A program holds the paths of its sources (the places of its panics). The builds for a package
# name the project folder, the cargo folder and the rustup folder by fixed names, so a package
# does not show where it was built, and the same sources give the same program.
CARGO_DIR = $(or $(CARGO_HOME),$(HOME)/.cargo)
RUSTUP_DIR = $(or $(RUSTUP_HOME),$(HOME)/.rustup)
PATH_FLAGS = '--remap-path-prefix=$(CURDIR)=/xsrv-web', '--remap-path-prefix=$(CARGO_DIR)=/cargo', '--remap-path-prefix=$(RUSTUP_DIR)=/rustup'

help: ## List the targets
	@grep -E '^[a-zA-Z_-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "%-10s %s\n", $$1, $$2}'

check: fmt clippy test ## Run format, lint and tests

fmt: ## Check formatting
	cargo fmt --all -- --check

clippy: ## Lint with warnings as errors
	cargo clippy --workspace --all-targets -- -D warnings

test: ## Run all tests
	cargo test --workspace

bench: ## Measure messages, bytes and time of screen updates
	cargo bench -p xsrv-server --bench updates

bench-web: web ## Measure tile sizes, and how fast the page module applies them (needs node 22)
	cargo run --release -p xsrv-server --example tiles -- target/bench-tiles
	node web/test/bench.mjs target/bench-tiles

bench-latency: ## Measure the time from an X11 request to the message at a page
	cargo build --release -p xsrv-server --bin xsrv-web --example latency
	target/release/examples/latency target/release/xsrv-web

test-web: ## Test the page input script with node
	node web/test/input.test.mjs
	node web/test/wheel.test.mjs
	node web/test/settings.test.mjs
	node web/test/macros.test.mjs
	node web/test/macro-input.test.mjs
	node web/test/barpos.test.mjs
	node web/test/panel.test.mjs
	node web/test/screenshot.test.mjs
	node web/test/clipboard.test.mjs
	node web/test/video.test.mjs

web: ## Build the WASM module into web/pkg
	cargo build -p xsrv-wasm --target wasm32-unknown-unknown --release \
		--config "target.wasm32-unknown-unknown.rustflags=[$(PATH_FLAGS)]"
	wasm-bindgen --target web --out-dir web/pkg target/wasm32-unknown-unknown/release/xsrv_wasm.wasm

run: web ## Build the web part and start the server
	cargo run -p xsrv-server

release: web ## Build the server and the page for release
	cargo build --release -p xsrv-server --config "build.rustflags=[$(PATH_FLAGS)]"

dist: release ## Build the tarball dist/xsrv-web-<version>-linux-<machine>.tar.gz
	bash packaging/dist.sh

deb: release ## Build the Debian package dist/xsrv-web_<version>_<arch>.deb (needs dpkg-deb)
	bash packaging/deb.sh

docker-build: ## Build the xsrv-web image (needs docker)
	docker build -f docker/Dockerfile -t xsrv-web .

docker-up: ## Start xsrv-web and an xterm with docker compose (needs XSRV_TOKEN)
	docker compose -f docker/compose.yaml up --build
