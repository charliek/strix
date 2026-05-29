CARGO ?= cargo

.DEFAULT_GOAL := help

# ---- build / run -------------------------------------------------------

.PHONY: build
build:  ## Build the debug binary
	$(CARGO) build

.PHONY: release
release:  ## Build the optimized release binary
	$(CARGO) build --release

.PHONY: run
run:  ## Run strix; pass a repo path or flags via ARGS, e.g. make run ARGS="../roost --theme dark"
	$(CARGO) run -- $(ARGS)

.PHONY: dump
dump:  ## Render one frame to stdout (text) for quick visual inspection
	$(CARGO) run -q -- --dump-frame

# ---- quality -----------------------------------------------------------

.PHONY: fmt
fmt:  ## Format the code
	$(CARGO) fmt --all

.PHONY: check
check: fmt-check lint test  ## Run all checks (fmt, clippy, tests)

.PHONY: fmt-check
fmt-check:  ## Verify formatting
	$(CARGO) fmt --all -- --check

.PHONY: lint
lint:  ## Run clippy with warnings denied
	$(CARGO) clippy --all-targets -- -D warnings

.PHONY: test
test:  ## Run the test suite
	$(CARGO) test --all

# ---- docs --------------------------------------------------------------

.PHONY: docs docs-serve
docs:  ## Build the mkdocs site into site-build/
	uv sync --group docs && uv run mkdocs build

docs-serve:  ## Serve the docs locally with live reload
	uv sync --group docs && uv run mkdocs serve

# ---- misc --------------------------------------------------------------

.PHONY: clean
clean:  ## Remove build artifacts
	$(CARGO) clean
	rm -rf site-build

.PHONY: help
help:  ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'
