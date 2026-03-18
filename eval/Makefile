# codemap-eval — common evaluation workflows
#
# Usage:
#   make eval              Run Layer 2 scorer eval on all datasets
#   make eval-hono         Run Layer 2 on a single dataset
#   make ab REPO=~/hono    Run Layer 3 A/B eval (requires ANTHROPIC_API_KEY)
#   make history           List archived eval runs

BIN       := ../target/release/codemap-eval
DATASETS  := datasets
FIXTURES  := fixtures
SCRIPTS   := scripts

# Layer 3 defaults (override on command line)
REPO      ?=
MODEL     ?= claude-sonnet-4-20250514
MAX_TURNS ?= 15
MAX_TOKENS?= 50000
VARIANT   ?= both
CASES     ?=

# ---------- build ----------

.PHONY: build
build: ## Build codemap + codemap-eval in release mode
	cargo build --release

# ---------- Layer 2: scorer quality ----------

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

# ---------- Layer 3a: Claude Code A/B (simulation) ----------

.PHONY: ab
ab: build ## Run A/B eval via API simulation (set REPO=/path/to/checkout)
ifndef REPO
	$(error REPO is required. Usage: make ab REPO=~/hono DATASET=datasets/hono.json)
endif
	$(BIN) ab \
		--dataset $(or $(DATASET),$(DATASETS)) \
		--repo-dir $(REPO) \
		--model $(MODEL) \
		--max-turns $(MAX_TURNS) \
		--max-tokens $(MAX_TOKENS) \
		--variant $(VARIANT) \
		$(if $(CASES),--cases $(CASES))

.PHONY: ab-control
ab-control: VARIANT = control
ab-control: ab ## Run A/B eval — control variant only

.PHONY: ab-treatment
ab-treatment: VARIANT = treatment
ab-treatment: ab ## Run A/B eval — treatment variant only

.PHONY: ab-cheap
ab-cheap: MODEL = claude-haiku-4-5-20251001
ab-cheap: ab ## Run A/B eval with Haiku (cheaper, faster iteration)

# ---------- Layer 3b: End-to-end Claude Code eval ----------

# E2E defaults (override on command line)
TIMEOUT   ?= 300

.PHONY: e2e
e2e: build ## Run e2e eval with real Claude Code CLI (set REPO=/path/to/checkout)
ifndef REPO
	$(error REPO is required. Usage: make e2e REPO=~/hono DATASET=datasets/hono.json)
endif
	$(BIN) e2e \
		--dataset $(or $(DATASET),$(DATASETS)) \
		--repo-dir $(REPO) \
		--model $(MODEL) \
		--max-turns $(MAX_TURNS) \
		--timeout $(TIMEOUT) \
		--variant $(VARIANT) \
		$(if $(CASES),--cases $(CASES))

.PHONY: e2e-control
e2e-control: VARIANT = control
e2e-control: e2e ## Run e2e eval — control variant only

.PHONY: e2e-treatment
e2e-treatment: VARIANT = treatment
e2e-treatment: e2e ## Run e2e eval — treatment variant only

.PHONY: e2e-cheap
e2e-cheap: MODEL = claude-haiku-4-5-20251001
e2e-cheap: e2e ## Run e2e eval with Haiku (cheaper, faster iteration)

.PHONY: e2e-verbose
e2e-verbose: build ## Run e2e eval with verbose output
ifndef REPO
	$(error REPO is required)
endif
	$(BIN) e2e \
		--dataset $(or $(DATASET),$(DATASETS)) \
		--repo-dir $(REPO) \
		--model $(MODEL) \
		--max-turns $(MAX_TURNS) \
		--timeout $(TIMEOUT) \
		--variant $(VARIANT) \
		--verbose \
		$(if $(CASES),--cases $(CASES))

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
