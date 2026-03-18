# Layer 3: End-to-End Claude Code Eval — Specification

## Goal

Measure whether Claude Code completes real tasks better when codemap is installed, using the actual Claude Code agent — not a simulation.

## Why not simulate?

The previous Layer 3 design called the Claude API directly with custom tool schemas and a hand-written agent loop. This tests whether the model benefits from structural tools in theory, but does not test:

- Claude Code's real system prompt, agent loop, and reasoning
- The interaction between codemap and Claude Code's native tools (Bash, Read, Grep, Glob, Edit)
- The real hook integration where `codemap instructions` injects context at session start
- How codemap's `map` output competes for attention with Claude Code's own context management

The existing simulation code (`ab.rs`, `claude_client.rs`, `tools.rs`) remains available as a cheaper, faster approximation. This spec describes the real thing.

## Prerequisites

- `claude` CLI installed and authenticated (Claude Code)
- `codemap` binary built and in PATH
- Fixture repos cloned locally (the actual source trees, not just index.db)
- `ANTHROPIC_API_KEY` set (used by Claude Code internally)

## Design

### Two variants per task

| Variant | Repo state | Claude Code invocation |
|---------|-----------|----------------------|
| **Control** | No `.codemap/` directory | `claude -p "<task>" --output-format stream-json` |
| **Treatment** | `.codemap/index.db` present | `claude -p "<task>" --output-format stream-json --append-system-prompt "<map + instructions>"` |

Both variants run in an isolated copy of the fixture repo with identical source files. The only difference is whether codemap is set up.

### Why `--append-system-prompt` instead of hooks

In real usage, `codemap setup` installs a `SessionStart` hook that runs `codemap instructions` and injects the output as a system message. For the eval, we skip the hook machinery and directly append the same content via `--append-system-prompt`. This is equivalent but simpler to control programmatically.

The treatment system prompt is assembled as:

```
{codemap map --tokens 1500 --no-instructions}

## codemap — codebase intelligence
Use these commands in Bash for structural codebase queries:
- `codemap context "<task>"` — find the most relevant files for a task (start here)
- `codemap symbol <name>` — find where a symbol is defined and who uses it
- `codemap deps <file>` — imports and importers of a file
- `codemap map` — ranked overview of top files with signatures
```

### Session flow

```
For each task:
  1. Copy fixture repo to a temp directory
  2. Treatment only: copy index.db to .codemap/, generate system prompt
  3. Spawn: claude -p "<task_prompt>" \
       --output-format stream-json \
       --model <model> \
       --max-turns <N> \
       --permission-mode plan \        # read-only: no edits
       [--append-system-prompt <...>]  # treatment only
  4. Capture stream-json output, parse events
  5. Extract: tool calls, files read, files mentioned, tokens, wall time
  6. Clean up temp directory
```

### Permission mode

Use `--permission-mode plan` for file discovery tasks. Claude Code can read and explore but not modify files. This keeps both variants on equal footing and avoids side effects between runs.

For future end-to-end modification tasks (Phase 2), use `--permission-mode acceptEdits`.

### Task prompt

The user message sent to Claude Code asks it to explore and report relevant files:

```
Your task: {query}

Explore the codebase to find the files most relevant to this task.
When you are done, list all relevant file paths.
```

To get a clean, parseable file list from Claude's final response, append a structured output instruction:

```
After exploring, respond with ONLY a JSON object in this exact format:
{"relevant_files": ["path/to/file1.ts", "path/to/file2.ts"]}
```

### Termination conditions

- Claude Code sends final response (natural end of exploration)
- `--max-turns` reached (default: 20)
- Wall clock timeout (default: 5 minutes per session)

### Extracting metrics from stream-json

`claude -p --output-format stream-json` emits newline-delimited JSON events. Each event has a `type` field. Key events to capture:

```jsonl
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"..."}]}}
{"type":"tool_use","name":"Bash","input":{"command":"codemap context \"JWT\""}}
{"type":"tool_result","name":"Bash","content":"..."}
{"type":"result","result":"final text","session_id":"...","usage":{"input_tokens":N,"output_tokens":N}}
```

Parse the stream to extract:

1. **Tool calls**: Count each `tool_use` event by tool name (`Bash`, `Read`, `Grep`, `Glob`, etc.)
2. **Files read**: Extract `file_path` from `Read` tool inputs
3. **Codemap usage**: Check if any `Bash` tool call contains `codemap` in the command
4. **Tokens**: From the final `result` event's `usage` field
5. **Files mentioned**: Match known repo file paths in assistant text blocks
6. **Final file list**: Parse the structured JSON from Claude's final message

**Implementation note**: The exact event schema should be verified against the installed `claude` CLI version. Run `claude -p "hello" --output-format stream-json` and inspect the output to confirm field names. If `stream-json` is unavailable, fall back to `--output-format json` which returns a single JSON object after completion.

## Task format

Reuses the existing Layer 2 dataset format. Same files, same cases.

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

No schema changes needed. The `expected_files` ground truth is the same — we're changing how we test, not what we test.

## Metrics

### Per-session metrics

```rust
struct SessionMetrics {
    variant: String,              // "control" or "treatment"
    case_id: String,

    // Tool usage
    tool_calls: usize,            // total tool invocations
    tool_calls_by_name: HashMap<String, usize>,  // Bash: 5, Read: 3, Grep: 2
    codemap_calls: usize,         // Bash calls containing "codemap"

    // File discovery
    files_read: HashSet<String>,         // files accessed via Read tool
    files_identified: Vec<String>,       // files Claude reported in structured output
    files_mentioned: HashSet<String>,    // files mentioned in text (fallback)

    // Cost
    input_tokens: usize,
    output_tokens: usize,
    wall_clock_ms: u64,

    // Speed
    turns: usize,
    first_relevant_file_turn: Option<usize>,
}
```

### Per-task comparison

For each task, compare control vs treatment:

```rust
struct TaskComparison {
    case_id: String,

    // Primary: did Claude find the right files?
    control_recall: f64,        // |expected ∩ identified| / |expected|
    treatment_recall: f64,
    control_precision: f64,     // |expected ∩ identified| / |identified|
    treatment_precision: f64,

    // Efficiency
    tool_call_reduction: f64,   // (control - treatment) / control
    token_reduction: f64,
    time_reduction: f64,

    // Behavior
    control_codemap_calls: usize,   // should be 0
    treatment_codemap_calls: usize, // how much Claude used codemap

    // Winner (by recall, then efficiency)
    winner: String,             // "treatment", "control", "tie"
}
```

### Aggregate report

```
End-to-End Eval: hono (20 tasks, model: claude-sonnet-4-20250514)
══════════════════════════════════════════════════════════════════

  Metric              Control      Treatment     Delta
  ──────────────────────────────────────────────────────
  Recall              0.42         0.71          +69%
  Precision           0.31         0.58          +87%
  Tool calls          11.3         5.8           -49%
  Tokens (total)      18.2k        9.4k          -48%
  Wall time           42s          23s           -45%
  Codemap calls       0.0          2.4           n/a
  First relevant      turn 4.1     turn 1.6      -61%

  Win/Loss/Tie: Treatment 14 / Control 3 / Tie 3
══════════════════════════════════════════════════════════════════
```

## Implementation structure

### New/modified files

```
eval/src/
  e2e.rs           # End-to-end eval orchestration (replaces ab.rs role)
  session.rs       # Spawn claude CLI, parse stream-json output
  workspace.rs     # Temp directory setup, fixture copying
```

### CLI integration

Replaces the `Ab` subcommand (or adds a new one alongside it):

```
codemap-eval e2e --dataset eval/datasets/hono.json --repo-dir ~/hono [OPTIONS]

Options:
  --model <MODEL>          Claude model [default: claude-sonnet-4-20250514]
  --max-turns <N>          Max Claude Code turns [default: 20]
  --timeout <SECS>         Per-session wall clock timeout [default: 300]
  --cases <IDS>            Run specific cases only (comma-separated)
  --variant <V>            control, treatment, or both [default: both]
  --no-archive             Skip archiving results
  --verbose                Print full Claude Code output
```

### Data flow

```
1. Load dataset (reuse existing load_datasets)
2. Validate: claude CLI is in PATH and authenticated
3. Validate: repo-dir exists and contains expected source files
4. For treatment: copy index.db into repo, generate map output + instructions
5. For each case:
   a. Create temp directory, copy repo into it
   b. Run CONTROL session (no .codemap/, no extra prompt)
   c. Create fresh temp directory, copy repo + .codemap/ into it
   d. Run TREATMENT session (with codemap + appended prompt)
   e. Parse both outputs, compute comparison
6. Print aggregate report
7. Archive to history.db with layer="e2e_eval"
```

### Session runner (session.rs)

```rust
/// Spawn claude CLI and capture output.
fn run_claude_session(
    working_dir: &Path,
    task_prompt: &str,
    model: &str,
    max_turns: usize,
    timeout_secs: u64,
    append_system_prompt: Option<&str>,
) -> Result<RawSessionOutput> {
    let mut cmd = Command::new("claude");
    cmd.current_dir(working_dir)
        .arg("-p")
        .arg(task_prompt)
        .args(["--output-format", "stream-json"])
        .args(["--model", model])
        .args(["--max-turns", &max_turns.to_string()])
        .args(["--permission-mode", "plan"]);

    if let Some(prompt) = append_system_prompt {
        cmd.args(["--append-system-prompt", prompt]);
    }

    // Run with timeout
    let start = Instant::now();
    let output = cmd.output()?;  // or spawn + wait_timeout
    let elapsed = start.elapsed();

    Ok(RawSessionOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code(),
        wall_clock_ms: elapsed.as_millis() as u64,
    })
}
```

### Stream parser (session.rs)

```rust
/// Parse stream-json output into structured metrics.
fn parse_stream_output(
    raw: &str,
    known_files: &HashSet<String>,
) -> SessionMetrics {
    let mut metrics = SessionMetrics::default();

    for line in raw.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else { continue };
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "tool_use" => {
                let name = event.get("name").and_then(|v| v.as_str()).unwrap_or("");
                metrics.tool_calls += 1;
                *metrics.tool_calls_by_name.entry(name.to_string()).or_default() += 1;

                // Track file reads
                if name == "Read" {
                    if let Some(path) = event.pointer("/input/file_path").and_then(|v| v.as_str()) {
                        metrics.files_read.insert(normalize_path(path));
                    }
                }

                // Track codemap usage via Bash
                if name == "Bash" {
                    if let Some(cmd) = event.pointer("/input/command").and_then(|v| v.as_str()) {
                        if cmd.contains("codemap") {
                            metrics.codemap_calls += 1;
                        }
                    }
                }

                metrics.turns += 1;
            }
            "assistant" => {
                // Extract file mentions from text
                if let Some(text) = extract_text_from_message(&event) {
                    for file in known_files.iter().filter(|f| text.contains(f.as_str())) {
                        metrics.files_mentioned.insert(file.clone());
                    }
                }
            }
            "result" => {
                // Final result: extract tokens and structured output
                if let Some(usage) = event.get("usage") {
                    metrics.input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    metrics.output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                }

                // Try to parse structured file list from final response
                if let Some(result_text) = event.get("result").and_then(|v| v.as_str()) {
                    if let Some(files) = extract_json_file_list(result_text) {
                        metrics.files_identified = files;
                    }
                }
            }
            _ => {}
        }
    }

    metrics
}
```

### Workspace isolation (workspace.rs)

Each session runs in a fresh temp directory to prevent cross-contamination:

```rust
/// Create an isolated workspace for a single eval session.
fn create_workspace(
    repo_dir: &Path,
    variant: Variant,
    index_db: Option<&Path>,       // fixture index.db
    codemap_bin: Option<&Path>,    // for generating map output
) -> Result<TempWorkspace> {
    let tmp = tempfile::tempdir()?;

    // Copy repo contents (symlink would be faster but risks mutation)
    copy_dir_recursive(repo_dir, tmp.path())?;

    let mut system_prompt = None;

    if variant == Variant::Treatment {
        if let Some(db_path) = index_db {
            // Set up .codemap/index.db
            let codemap_dir = tmp.path().join(".codemap");
            std::fs::create_dir_all(&codemap_dir)?;
            std::fs::copy(db_path, codemap_dir.join("index.db"))?;
        }

        // Generate treatment system prompt
        let map_output = run_codemap_map(codemap_bin, tmp.path())?;
        let instructions = CODEMAP_INSTRUCTIONS;
        system_prompt = Some(format!("{map_output}\n\n{instructions}"));
    }

    Ok(TempWorkspace {
        dir: tmp,
        system_prompt,
    })
}
```

**Performance note**: Copying entire repos per session is expensive for large repos. Optimization: use `cp -al` (hardlinks) on Linux or a COW filesystem. Or create one read-only base copy and overlay `.codemap/` via a union mount. For initial implementation, a simple copy is fine — correctness first.

## Verifying `claude` CLI availability

Before running evals, validate the environment:

```rust
fn check_prerequisites() -> Result<()> {
    // 1. claude CLI exists and responds
    let output = Command::new("claude").arg("--version").output()?;
    anyhow::ensure!(output.status.success(), "claude CLI not found or not working");
    let version = String::from_utf8_lossy(&output.stdout);
    eprintln!("Using claude CLI: {}", version.trim());

    // 2. claude is authenticated
    let output = Command::new("claude")
        .args(["-p", "say hi", "--output-format", "json", "--max-turns", "1"])
        .output()?;
    anyhow::ensure!(output.status.success(), "claude CLI auth failed — run `claude` interactively first");

    // 3. codemap binary exists
    let output = Command::new("codemap").arg("--version").output()?;
    anyhow::ensure!(output.status.success(), "codemap binary not found — run `cargo build --release`");

    Ok(())
}
```

## Cost estimation

Claude Code uses more tokens per session than raw API calls due to its system prompt and agent overhead.

Per session (estimated):
- System prompt: ~5k tokens
- Tool definitions: ~3k tokens
- Per turn (tool call + result): ~2-4k tokens
- Average session (8 turns): ~25-35k input tokens, ~5k output tokens

Per task (2 sessions): ~70k input + 10k output tokens

With claude-sonnet-4-20250514 ($3/$15 per MTok):
- Per task: ~$0.36
- Per dataset (20 cases): ~$7
- **All 3 datasets: ~$21**

With claude-haiku-4-5-20251001 ($0.80/$4 per MTok):
- **All 3 datasets: ~$6**

About 2× the cost of the simulation approach, but testing the real agent.

## Determinism

- Use a fixed `--model` for all sessions
- Claude Code does not expose a `temperature` flag — it uses its own defaults
- Run each task multiple times (configurable via `--runs N`, default 1) and report mean ± std
- Track the `session_id` from each run for reproducibility auditing

Note: Claude Code's agent loop introduces more variance than direct API calls (tool choice, exploration strategy). Expect higher variance in metrics compared to the simulation. This is realistic — it reflects actual usage.

## History integration

Results archived using existing `history::save_run()`:

```rust
save_run(
    conn,
    git_commit,
    git_dirty,
    "e2e_eval",             // layer (distinct from "ab_eval" for simulation)
    "hono",                 // dataset
    "js/ts",                // language
    &metrics_json,          // aggregate + per-case metrics
    Some(&config_json),     // model, max_turns, variant info
)
```

## Phase 2: End-to-end task verification (future)

Phase 1 (above) measures file discovery: did Claude find the right files? This is the same signal as Layer 2/3 but through the real agent.

Phase 2 extends to verifiable tasks where Claude makes actual changes:

### Task types

1. **Code modification**: "Add a `timeout` parameter to the cookie helper"
   - Verification: diff touches expected files, project still compiles/lints
2. **Bug fix**: "Fix the JWT verification to check token expiry"
   - Verification: a pre-written test suite passes after Claude's changes
3. **Question answering**: "What function handles routing for parameterized URLs?"
   - Verification: structured answer matches known ground truth

### Dataset extension for Phase 2

```json
{
  "id": "hono-e2e-001",
  "type": "modify",
  "query": "Add a timeout parameter to the cookie helper",
  "expected_files": [
    { "path": "src/helper/cookie/index.ts", "relevance": 3 }
  ],
  "verification": {
    "must_touch": ["src/helper/cookie/index.ts"],
    "must_not_touch": ["src/index.ts"],
    "post_check": "npx tsc --noEmit"
  }
}
```

### Changes for Phase 2

- Use `--permission-mode acceptEdits` instead of `plan`
- After session: run verification commands (`tsc`, test suite, diff check)
- New metric: **task_success** (boolean) — did verification pass?
- Need repos with working build/test setups (not just source files)

Phase 2 is significantly more work (curating verifiable tasks, setting up build environments) but produces the metric that actually matters: does codemap help Claude succeed at real tasks?

## Dependencies

No new crate dependencies. Uses:
- `std::process::Command` for spawning `claude` and `codemap`
- `serde_json` for parsing stream-json output
- `tempfile` (already in dev-deps) for workspace isolation
- `std::time::Instant` for wall clock measurement
- Existing eval infrastructure: `history`, `load_datasets`, `find_eval_dir`

## Open questions

1. **`stream-json` event schema**: The exact event types and field names emitted by `claude -p --output-format stream-json` need to be verified against the installed CLI version. The schema in this spec is based on documentation — run a test session and adjust the parser accordingly.

2. **Workspace copying performance**: For tldraw (~2GB), copying the full repo per session is slow. Consider hardlinks, COW copies, or only copying the files that exist in the index.db (since that's all Claude could discover via codemap).

3. **Claude Code agent variance**: Without temperature control, results may vary between runs. How many repetitions are needed for statistical significance? Start with 1 run per task, increase to 3 if variance is high.

4. **`--max-turns` semantics**: Verify whether `--max-turns` in Claude Code counts the same way as conversation round-trips. It may count individual tool calls or assistant messages differently.

5. **Control fairness**: The control variant gets no extra system prompt. Should it get a generic "explore the codebase" instruction via `--append-system-prompt` to match the treatment's extra context? Or is the asymmetry the point (codemap = extra context)?
