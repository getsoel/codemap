# codemap-eval — common evaluation workflows
#
# Usage:
#   make eval              Run scorer eval on all datasets
#   make eval-hono         Run scorer eval on a single dataset
#   make e2e               Run e2e eval with real Claude Code CLI
#   make history           List archived eval runs

BIN       := ../target/release/codemap-eval
DATASETS  := datasets
FIXTURES  := fixtures
SCRIPTS   := scripts

# E2E defaults (override on command line)
MODEL     ?= opus
MAX_TURNS ?= 15
VARIANT   ?= all
CASES     ?=
TIMEOUT   ?= 300

# ---------- build ----------

.PHONY: build
build: ## Build codemap + codemap-eval in release mode
	cargo build --release

# ---------- scorer quality ----------

.PHONY: eval
eval: build ## Run scorer eval on all datasets
	$(BIN) run --dataset $(DATASETS)

.PHONY: eval-json
eval-json: build ## Run scorer eval on all datasets (JSON output)
	$(BIN) run --dataset $(DATASETS) --format json

.PHONY: eval-hono
eval-hono: build ## Run scorer eval on hono dataset only
	$(BIN) run --dataset $(DATASETS)/hono.json

.PHONY: eval-express
eval-express: build ## Run scorer eval on express dataset only
	$(BIN) run --dataset $(DATASETS)/express.json

.PHONY: eval-tldraw
eval-tldraw: build ## Run scorer eval on tldraw dataset only
	$(BIN) run --dataset $(DATASETS)/tldraw.json

.PHONY: compare
compare: build ## Compare current scorer results against last archived run
	$(BIN) compare --dataset $(DATASETS)

# ---------- End-to-end Claude Code eval ----------

# Extra flags (set by variant targets)
E2E_FLAGS ?=

.PHONY: e2e
e2e: build ## Run e2e eval (repos auto-cloned from dataset repo_url)
	$(BIN) e2e \
		--dataset $(or $(DATASET),$(DATASETS)) \
		--model $(MODEL) \
		--max-turns $(MAX_TURNS) \
		--timeout $(TIMEOUT) \
		--variant $(VARIANT) \
		$(E2E_FLAGS) \
		$(if $(CASES),--cases $(CASES))

.PHONY: e2e-control
e2e-control: VARIANT = control
e2e-control: e2e ## Run e2e eval — control variant only

.PHONY: e2e-treatment
e2e-treatment: VARIANT = treatment
e2e-treatment: e2e ## Run e2e eval — treatment variant only

.PHONY: e2e-enriched
e2e-enriched: VARIANT = enriched
e2e-enriched: e2e ## Run e2e eval — enriched variant only

.PHONY: e2e-cheap
e2e-cheap: MODEL = haiku
e2e-cheap: e2e ## Run e2e eval with Haiku (cheaper, faster iteration)

.PHONY: e2e-verbose
e2e-verbose: E2E_FLAGS = --verbose
e2e-verbose: e2e ## Run e2e eval with verbose output

.PHONY: report
report: DATASET =
report: e2e ## Run full e2e benchmark on all datasets → RESULTS.md

# ---------- history ----------

.PHONY: history
history: ## List archived eval runs
	$(BIN) history

# ---------- fixtures ----------

.PHONY: fixture
fixture: ## Build a fixture (set URL, COMMIT, NAME)
ifndef URL
	$(error URL is required. Usage: make fixture URL=https://github.com/honojs/hono COMMIT=HEAD NAME=hono)
endif
ifndef NAME
	$(error NAME is required. Usage: make fixture URL=https://github.com/honojs/hono COMMIT=HEAD NAME=hono)
endif
	$(SCRIPTS)/build_fixture.sh $(URL) $(or $(COMMIT),HEAD) $(NAME)

.PHONY: fixture-hono
fixture-hono: ## Rebuild hono fixture from upstream
	$(SCRIPTS)/build_fixture.sh https://github.com/honojs/hono HEAD hono

.PHONY: fixture-express
fixture-express: ## Rebuild express fixture from upstream
	$(SCRIPTS)/build_fixture.sh https://github.com/expressjs/express HEAD express

.PHONY: fixture-tldraw
fixture-tldraw: ## Rebuild tldraw fixture from upstream
	$(SCRIPTS)/build_fixture.sh https://github.com/tldraw/tldraw HEAD tldraw

# ---------- help ----------

.PHONY: help
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help
