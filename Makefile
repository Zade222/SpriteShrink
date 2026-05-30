.PHONY: help build release lint fmt test coverage coverage-html check clean

##@ General

help: ## Print this help message
	@awk 'BEGIN {FS = ":.*?## "; printf "\nUsage: \033[1mmake\033[0m \033[36m<target>\033[0m\n"} \
	     /^##@/ {printf "\n\033[1m%s\033[0m\n", substr($$0, 5); next} \
	     /^[a-zA-Z_-]+:.*?## / {printf "  \033[36m%-26s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@printf "\n"

##@ Build

build: ## Build all crates (dev profile)
	cargo build --workspace

release: ## Build all crates (release profile)
	cargo build --workspace --release

##@ Quality

lint: ## Check formatting and run Clippy (-D warnings)
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings

fmt: ## Format all source code
	cargo fmt --all

test: ## Run unit tests across the workspace
	cargo test --workspace

coverage: ## Print workspace code coverage summary
	cargo llvm-cov --workspace --all-targets --summary-only

coverage-html: ## Generate workspace HTML coverage report
	cargo llvm-cov --workspace --all-targets --html

check: lint test ## Run lint and test

##@ Misc

clean: ## Remove build artifacts
	cargo clean
