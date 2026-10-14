.DEFAULT_GOAL := help
.PHONY: help install run test lint fmt

help: ## Show this help
	@awk 'BEGIN {FS = ":.*## "} /^[a-z-]+:.*## / {printf "  %-8s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

install: ## Install cln into ~/.cargo/bin
	cargo install --path crates/cln

run: ## Run cln (pass args with ARGS="file.org")
	cargo run -p cln -- $(ARGS)

test: ## Run the test suite
	cargo test --workspace

lint: ## Clippy, warnings as errors
	cargo clippy --workspace --all-targets -- -D warnings

fmt: ## Format the workspace
	cargo fmt --all
