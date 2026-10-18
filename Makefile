.DEFAULT_GOAL := help
.PHONY: help check test e2e module fmt lint hooks precommit clean

help: ## Show available targets
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-12s %s\n", $$1, $$2}'

check: ## Type-check every target without running anything
	cargo check --all-targets --locked

test: ## Run the test suite
	cargo nextest run --locked --no-tests=pass

e2e: ## Run the end-to-end test: louro and Ergo in a NixOS VM (slow)
	nix build -L --no-link .#checks.$$(nix config show system).e2e

module: ## Run the NixOS module's VM test: services.louro with Ergo (slow)
	nix build -L --no-link .#checks.$$(nix config show system).module

fmt: ## Format and lint (cargo fmt, clippy)
	cargo fmt --all
	cargo clippy --all-targets --locked -- -D warnings

lint: ## Read-only check (fmt --check, clippy)
	cargo fmt --all -- --check
	cargo clippy --all-targets --locked -- -D warnings

hooks: ## Install the prek git hooks (the dev shell does this on entry)
	prek install

precommit: ## Run all hooks against every file (same checks the commit hook runs)
	prek run --all-files

clean: ## Remove build artefacts
	rm -rf target
