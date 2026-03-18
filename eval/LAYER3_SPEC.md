# Layer 3: Claude Code A/B Eval — Specification

## Goal

Measure whether Claude Code finds the right files faster and with fewer tool calls when codemap is available, compared to using only Grep/Glob/Read.

## Design

### Two variants per task

| Variant | System prompt | Available tools |
|---------|--------------|-----------------|
| **Control** | Generic: "You are a coding assistant. Explore the codebase to find the files relevant to this task." | `grep`, `glob`, `read_file` |
| **Treatment** | Codemap map injected + instructions: "You have codemap available for structural queries." | `grep`, `glob`, `read_file`, `codemap_context`, `codemap_symbol`, `codemap_deps` |

Both variants receive the same user message: the task query from the eval dataset.

### Session flow

```
For each task:
  1. Send user message with task description
  2. Claude responds, possibly requesting tool calls
  3. Execute requested tools against the cloned fixture repo
  4. Return tool results to Claude
  5. Repeat until Claude stops calling tools or max_turns reached
  6. Record: which files Claude identified, tool call trace, token usage
```

### Termination conditions

- Claude sends a response with `stop_reason: "end_turn"` (it's done exploring)
- `max_turns` reached (default: 15 round-trips)
- Token budget exceeded (default: 50k input tokens per session)

## Task format

Extends the existing Layer 2 dataset format. Same files, same cases — Layer 3 reuses them.

```json
{
  "repo": "hono",
  "language": "js/ts",
  "commit": "latest",
  "index_db": "fixtures/hono/index.db",
  "cases": [
    {
      "id": "hono-001",
      "query": "add a new HTTP response helper to the context object",
      "expected_files": [
        { "path": "src/context.ts", "relevance": 3 },
        { "path": "src/types.ts", "relevance": 2 }
      ]
    }
  ]
}
```

No changes needed to the dataset schema. Layer 3 uses the same `expected_files` for evaluation.

## Tool definitions

### Control tools (both variants)

#### `grep`
```json
{
  "name": "grep",
  "description": "Search file contents for a regex pattern. Returns matching file paths and line content.",
  "input_schema": {
    "type": "object",
    "properties": {
      "pattern": { "type": "string", "description": "Regex pattern to search for" },
      "path": { "type": "string", "description": "Directory or file to search in. Defaults to repo root." },
      "glob": { "type": "string", "description": "File glob filter, e.g. '*.ts'" }
    },
    "required": ["pattern"]
  }
}
```

**Implementation**: Run `rg --json` against the fixture repo. Return up to 20 matches with file path, line number, and line content.

#### `glob`
```json
{
  "name": "glob",
  "description": "Find files matching a glob pattern. Returns file paths sorted by modification time.",
  "input_schema": {
    "type": "object",
    "properties": {
      "pattern": { "type": "string", "description": "Glob pattern, e.g. 'src/**/*.ts'" }
    },
    "required": ["pattern"]
  }
}
```

**Implementation**: Use the `glob` crate or shell out to `find`. Return matching file paths.

#### `read_file`
```json
{
  "name": "read_file",
  "description": "Read the contents of a file.",
  "input_schema": {
    "type": "object",
    "properties": {
      "path": { "type": "string", "description": "File path relative to repo root" },
      "limit": { "type": "integer", "description": "Max lines to read. Default: 200" }
    },
    "required": ["path"]
  }
}
```

**Implementation**: Read file from the fixture repo. Truncate to `limit` lines. Return content with line numbers.

### Treatment-only tools (codemap)

#### `codemap_context`
```json
{
  "name": "codemap_context",
  "description": "Find the most relevant files for a task description, ranked by structural importance.",
  "input_schema": {
    "type": "object",
    "properties": {
      "query": { "type": "string", "description": "Natural language task description" },
      "limit": { "type": "integer", "description": "Max results. Default: 10" }
    },
    "required": ["query"]
  }
}
```

**Implementation**: Run `codemap context --json "<query>" --limit <limit>` against the fixture repo.

#### `codemap_symbol`
```json
{
  "name": "codemap_symbol",
  "description": "Find where a symbol is defined and who references it.",
  "input_schema": {
    "type": "object",
    "properties": {
      "name": { "type": "string", "description": "Symbol name or pattern" },
      "exact": { "type": "boolean", "description": "Exact match only. Default: false" }
    },
    "required": ["name"]
  }
}
```

**Implementation**: Run `codemap symbol --json "<name>"` against the fixture repo.

#### `codemap_deps`
```json
{
  "name": "codemap_deps",
  "description": "Show imports and importers of a file.",
  "input_schema": {
    "type": "object",
    "properties": {
      "file": { "type": "string", "description": "File path to inspect" },
      "direction": { "type": "string", "enum": ["imports", "importers", "both"], "description": "Default: both" }
    },
    "required": ["file"]
  }
}
```

**Implementation**: Run `codemap deps --json "<file>"` against the fixture repo.

## Claude API integration

### Request format

Uses `POST https://api.anthropic.com/v1/messages` with the existing `ureq` HTTP client (blocking, no async runtime).

```rust
struct SessionRequest {
    model: String,           // "claude-sonnet-4-20250514"
    max_tokens: usize,       // 4096
    system: String,          // variant-specific system prompt
    messages: Vec<Message>,  // conversation history
    tools: Vec<Tool>,        // variant-specific tool list
    temperature: f64,        // 0.0 for determinism
}

struct Message {
    role: String,            // "user" or "assistant"
    content: Content,        // text or tool_use/tool_result blocks
}
```

### Response handling

Loop until `stop_reason == "end_turn"` or max turns:

```
1. POST /v1/messages with conversation history
2. Parse response content blocks:
   - text block → append to assistant message, extract mentioned files
   - tool_use block → execute tool, append tool_result to messages
3. If stop_reason == "tool_use", continue loop
4. If stop_reason == "end_turn", session complete
```

### Authentication

Read `ANTHROPIC_API_KEY` from environment (same as existing enrichment client).

## Metrics

### Per-session metrics (collected during execution)

```rust
struct SessionMetrics {
    variant: String,              // "control" or "treatment"
    case_id: String,
    tool_calls: usize,            // total tool invocations
    tool_calls_by_type: HashMap<String, usize>,  // grep: 5, read_file: 3, etc.
    files_read: HashSet<String>,  // unique files accessed via read_file
    files_mentioned: HashSet<String>,  // files Claude mentioned in text responses
    input_tokens: usize,
    output_tokens: usize,
    turns: usize,                 // conversation round-trips
    first_relevant_file_turn: Option<usize>,  // turn when first expected file appeared
}
```

### Per-task comparison metrics

For each task, compare control vs treatment:

```rust
struct TaskComparison {
    case_id: String,
    query: String,

    // Tool efficiency
    tool_call_reduction: f64,       // (control - treatment) / control
    file_read_reduction: f64,       // same for unique files read

    // Relevance
    control_recall_at_read: f64,    // of files Claude read, how many were relevant
    treatment_recall_at_read: f64,
    control_files_mentioned_recall: f64,  // of expected files, how many did Claude mention
    treatment_files_mentioned_recall: f64,

    // Cost
    control_tokens: usize,
    treatment_tokens: usize,
    token_reduction: f64,

    // Speed
    control_first_relevant_turn: Option<usize>,
    treatment_first_relevant_turn: Option<usize>,
}
```

### Aggregate metrics

```
Overall (N tasks):
  Tool calls:     control avg 12.3  →  treatment avg 5.1  (-58%)
  Files read:     control avg 8.2   →  treatment avg 3.4  (-59%)
  Recall (read):  control avg 0.35  →  treatment avg 0.72 (+106%)
  Recall (mentioned): control 0.41  →  treatment 0.68     (+66%)
  Tokens:         control avg 18.2k →  treatment avg 9.1k (-50%)
  First relevant: control turn 4.2  →  treatment turn 1.8 (-57%)
  Win/Loss/Tie:   Treatment wins 14, Control wins 3, Ties 3
```

## Extracting files Claude identified

Two methods to determine which files Claude found relevant:

1. **Files read**: Track every `read_file` tool call — these are files Claude chose to examine
2. **Files mentioned**: Parse Claude's text responses for file paths matching known repo files

For the "files mentioned" extraction:
```rust
fn extract_mentioned_files(text: &str, known_files: &HashSet<String>) -> Vec<String> {
    known_files.iter()
        .filter(|f| text.contains(f.as_str()))
        .cloned()
        .collect()
}
```

## System prompts

### Control prompt
```
You are a coding assistant. Your task is to explore a codebase and identify the files
most relevant to a given task.

Use the available tools (grep, glob, read_file) to explore the codebase and find the
files you would need to read or modify to complete the task.

When you are done exploring, list the files you identified as relevant and explain why.
```

### Treatment prompt
```
{codemap_map_output}

You are a coding assistant. Your task is to explore a codebase and identify the files
most relevant to a given task.

You have structural codebase tools available:
- codemap_context: find relevant files for a task (start here)
- codemap_symbol: find where a symbol is defined and referenced
- codemap_deps: show imports and importers of a file

You also have standard tools: grep, glob, read_file.

When you are done exploring, list the files you identified as relevant and explain why.
```

The `{codemap_map_output}` is generated by running `codemap map --tokens 1500 --no-instructions`
against the fixture repo.

## Implementation structure

### New files

```
eval/src/
  ab.rs              # A/B eval orchestration
  claude_client.rs   # Claude Messages API client with tool_use loop
  tools.rs           # Tool implementations (grep, glob, read_file, codemap_*)
```

### CLI integration

Add an `Ab` subcommand to `codemap-eval`:

```
codemap-eval ab --dataset eval/datasets/hono.json [OPTIONS]

Options:
  --model <MODEL>          Claude model to use [default: claude-sonnet-4-20250514]
  --max-turns <N>          Max conversation turns per session [default: 15]
  --max-tokens <N>         Max input tokens per session [default: 50000]
  --cases <IDS>            Run specific case IDs only (comma-separated)
  --variant <V>            Run only control or treatment [default: both]
  --concurrency <N>        Parallel sessions [default: 1]
  --no-archive             Skip archiving results
```

### Data flow

```
1. Load dataset (same as Layer 2)
2. Clone fixture repo to temp dir (or use existing clone)
3. Ensure codemap index exists in fixture
4. Generate codemap map output for treatment system prompt
5. For each case:
   a. Run CONTROL session (grep/glob/read_file only)
   b. Run TREATMENT session (all tools + codemap map in system prompt)
   c. Collect SessionMetrics for both
   d. Compute TaskComparison
6. Compute aggregate metrics
7. Print report
8. Archive to history.db with layer="ab_eval"
```

## Cost estimation

Per task: 2 sessions × ~15k tokens average = ~30k tokens
Per dataset (20 cases): ~600k tokens

With claude-sonnet-4-20250514 ($3/$15 per MTok):
- Input: 600k × $3/M = ~$1.80
- Output: ~100k × $15/M = ~$1.50
- **Total per dataset: ~$3.30**
- **Total for all 3 datasets: ~$10**

With claude-haiku-4-5-20251001 ($0.80/$4 per MTok):
- **Total for all 3 datasets: ~$3**

## Determinism and statistical significance

- Use `temperature: 0` for all sessions
- Run each task 3 times per variant (configurable via `--runs N`)
- Report mean ± std for each metric
- Paired t-test or Wilcoxon signed-rank for win/loss significance
- Seed the `system` fingerprint in the request for reproducibility

## History integration

Results archived using existing `history::save_run()`:

```rust
save_run(
    conn,
    git_commit,
    git_dirty,
    "ab_eval",              // layer
    "hono",                 // dataset
    "js/ts",                // language
    &metrics_json,          // aggregate + per-case metrics
    Some(&config_json),     // model, max_turns, variant info
)
```

The `compare` command will need to be extended to support `layer="ab_eval"` comparisons,
showing tool call reduction and recall changes between runs.

## Dependencies

No new crate dependencies needed. Layer 3 uses:
- `ureq` (already in deps) for Claude API calls
- `serde_json` (already in deps) for request/response serialization
- `rusqlite` (already in deps) for history archival
- `std::process::Command` for running `codemap` and `rg` subprocesses
- `tempfile` (already in dev-deps) for fixture repo clones

## Open questions

1. **Should we use real repos or synthetic ones?** Real repos (express, hono, tldraw) are more realistic but require cloning ~2GB. Could cache clones in `/tmp/`.

2. **How to handle codemap index for control variant?** The control variant shouldn't have codemap available. Use separate temp dirs — one with `.codemap/index.db`, one without.

3. **Should we test with enrichment on/off?** Running treatment with and without enrichment would show the lift from LLM-generated summaries. This triples the number of sessions but provides valuable signal.

4. **Model selection for eval vs production?** Using a cheaper model (Haiku) for rapid iteration, then confirm results with Sonnet for final numbers.
