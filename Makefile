.DEFAULT_GOAL := help
.PHONY: help check test fmt lint hooks precommit clean

help: ## Show available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-12s %s\n", $$1, $$2}'

check: ## Type-check every target without running anything
	cargo check --all-targets --locked

test: ## Run the test suite
	cargo nextest run --locked --no-tests=pass

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
