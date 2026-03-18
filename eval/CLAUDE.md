# codemap-eval

E2e eval framework that runs real Claude Code sessions (control vs treatment) to measure codemap's impact on file discovery.

## Quick reference

```bash
make e2e                                    # all datasets, all 3 variants, Opus
make e2e DATASET=datasets/hono.json         # single dataset
make e2e CASES=hono-001,hono-002            # specific cases
make e2e-cheap                              # use Haiku
make e2e-control                            # control only
make e2e-treatment                          # treatment only
make e2e-enriched                           # enriched only (needs API key)
make e2e-verbose                            # print session debug info
make eval                                   # scorer quality eval (no Claude CLI)
make history                                # list archived runs
```

## Architecture

- `e2e.rs` — orchestrates control/treatment/enriched sessions, computes aggregate metrics
- `session.rs` — spawns `claude -p`, parses stream-json output into SessionMetrics
- `workspace.rs` — temp directory isolation, runs `codemap setup` for treatment/enriched, `codemap enrich --api` for enriched
- `metrics.rs` — precision, recall, MRR, NDCG computation
- `report.rs` — table/JSON output formatting
- `history.rs` — SQLite archival of eval runs

## Claude CLI gotchas

- `claude -p --output-format stream-json` requires `--verbose`, otherwise the CLI errors
- `--model` accepts aliases (`opus`, `sonnet`, `haiku`), not full IDs like `claude-opus-4-6-20250514`
- stream-json tool_use events are content blocks inside `assistant` events, NOT top-level events. Parse `event.message.content[]` where `block.type == "tool_use"`

## Treatment design

Treatment sessions run `codemap setup --no-post-hook` in each workspace to write the SessionStart hook to `.claude/settings.local.json`. Claude Code picks it up naturally — no `--append-system-prompt`.

Enriched sessions additionally run `codemap enrich --api` after setup, adding LLM-generated file summaries to the index. Requires `GEMINI_API_KEY` or `ANTHROPIC_API_KEY`.

## Eval analysis

- **"Name-obvious" cases don't differentiate variants.** When expected files rank highly in PageRank and file names directly match query keywords (e.g., "context object" → `context.ts`), all variants score equally. Prefer cases with a semantic gap between query and file names (e.g., "typed client" where `src/client/` is below the map cutoff).
- **`avg_codemap_calls: 0` means Claude never used interactive commands** (`codemap context`, `codemap symbol`, `codemap deps`). The only codemap interaction was the SessionStart hook. This is a key diagnostic — if codemap calls are zero across treatment/enriched, the case only tests the injected map, not tool usage.
- **`make smoke` vs `make smoke-deep`:** `smoke` (hono-001) tests the eval pipeline end-to-end with a trivial case. `smoke-deep` (hono-012) tests a case where expected files are below the PageRank cutoff, requiring Claude to actually invoke codemap commands to find them.
