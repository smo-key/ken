# Ken — development entry points.
# `make setup` once, then `make dev` to run the app.

# The repo standardizes on pnpm: src-tauri/tauri.conf.json shells out to
# `pnpm dev` / `pnpm build`, so pnpm must be on PATH even for `make dev`.
# `make setup` provisions it through corepack, which ships with Node.
PNPM := pnpm
VITE_PORT := 1420

.DEFAULT_GOAL := help
.PHONY: help setup dev build test check clean require-pnpm

help: ## Show available targets
	@grep -hE '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) | awk -F':.*?## ' '{printf "  \033[1m%-8s\033[0m %s\n", $$1, $$2}'

setup: ## Install pnpm, frontend deps, and Rust deps
	@command -v cargo >/dev/null 2>&1 || { echo "cargo not found — install Rust: https://rustup.rs"; exit 1; }
	@command -v $(PNPM) >/dev/null 2>&1 || { echo "pnpm not found — enabling it via corepack"; corepack enable pnpm; }
	$(PNPM) install
	cargo fetch

dev: require-pnpm ## Run the Tauri dev server (app window + hot reload)
	@lsof -ti :$(VITE_PORT) | xargs kill -9 2>/dev/null || true
	$(PNPM) tauri dev

build: require-pnpm ## Produce a release bundle
	$(PNPM) tauri build

test: require-pnpm ## Run Rust and frontend tests
	cargo test
	$(PNPM) test

check: require-pnpm ## Type-check the Svelte frontend
	$(PNPM) check

clean: ## Remove build artifacts
	cargo clean
	rm -rf dist node_modules

require-pnpm:
	@command -v $(PNPM) >/dev/null 2>&1 || { echo "pnpm not found — run 'make setup' first"; exit 1; }
