/// End-to-end eval orchestration: run real Claude Code sessions with and without codemap.
///
/// Unlike the simulation-based A/B eval (ab.rs), this spawns the actual `claude` CLI
/// and measures whether Claude Code completes file discovery tasks better when codemap
/// is available as a tool via `--append-system-prompt`.
use crate::history;
use crate::session::{self, SessionMetrics};
use crate::workspace;
use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use codemap::db;
use serde_json::json;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Variant {
    /// All variants: control + treatment + enriched
    All,
    Control,
    Treatment,
    /// Treatment with LLM-enriched summaries
    Enriched,
}

const TASK_PROMPT_TEMPLATE: &str = "\
Your task: {query}

Explore the codebase to find the files most relevant to this task.
When you are done, list all relevant file paths.

After exploring, respond with ONLY a JSON object in this exact format:
{\"relevant_files\": [\"path/to/file1.ts\", \"path/to/file2.ts\"]}";

struct TaskResult {
    case_id: String,
    expected: HashSet<String>,
    control: Option<SessionMetrics>,
    treatment: Option<SessionMetrics>,
    enriched: Option<SessionMetrics>,
}

impl TaskResult {
    /// Whether this result has at least two variants for comparison.
    fn has_multiple_variants(&self) -> bool {
        u8::from(self.control.is_some())
            + u8::from(self.treatment.is_some())
            + u8::from(self.enriched.is_some())
            >= 2
    }
}

struct DatasetReport {
    name: String,
    results: Vec<TaskResult>,
    aggregate: E2eAggregate,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
struct E2eAggregate {
    count: usize,

    // Recall & precision
    avg_control_recall: f64,
    avg_treatment_recall: f64,
    avg_enriched_recall: f64,
    avg_control_precision: f64,
    avg_treatment_precision: f64,
    avg_enriched_precision: f64,

    // Efficiency
    avg_control_tool_calls: f64,
    avg_treatment_tool_calls: f64,
    avg_enriched_tool_calls: f64,
    tool_call_reduction_pct: f64,
    avg_control_input_tokens: f64,
    avg_control_output_tokens: f64,
    avg_treatment_input_tokens: f64,
    avg_treatment_output_tokens: f64,
    avg_enriched_input_tokens: f64,
    avg_enriched_output_tokens: f64,
    token_reduction_pct: f64,
    avg_control_wall_time_ms: f64,
    avg_treatment_wall_time_ms: f64,
    avg_enriched_wall_time_ms: f64,
    time_reduction_pct: f64,

    // Codemap usage
    avg_control_codemap_calls: f64,
    avg_treatment_codemap_calls: f64,
    avg_enriched_codemap_calls: f64,

    // Speed
    avg_control_first_relevant: f64,
    avg_treatment_first_relevant: f64,
    avg_enriched_first_relevant: f64,

    // Wins (control vs treatment when no enriched; three-way when all present)
    treatment_wins: usize,
    control_wins: usize,
    enriched_wins: usize,
    ties: usize,
}

/// Run the full end-to-end eval pipeline.
#[allow(clippy::too_many_arguments)]
pub fn run_e2e_eval(
    dataset_path: &Path,
    model: &str,
    max_turns: usize,
    timeout_secs: u64,
    cases_filter: Option<&str>,
    variant: Variant,
    no_archive: bool,
    verbose: bool,
) -> Result<()> {
    check_prerequisites(variant)?;

    let enrichment_model = if matches!(variant, Variant::All | Variant::Enriched) {
        detect_enrichment_model()
    } else {
        None
    };

    let datasets = crate::load_datasets(dataset_path)?;
    let codemap_bin = workspace::find_codemap_bin()?;
    let eval_dir = crate::find_eval_dir()?;

    let case_ids: Option<HashSet<String>> =
        cases_filter.map(|s| s.split(',').map(|c| c.trim().to_string()).collect());

    let (git_commit, git_dirty) = history::get_git_info();
    let history_conn = if !no_archive {
        Some(history::open_history(&eval_dir)?)
    } else {
        None
    };

    let mut all_dataset_reports: Vec<DatasetReport> = Vec::new();

    for ds in &datasets {
        let repo_dir = workspace::ensure_repo(&eval_dir, &ds.repo, &ds.repo_url)?;
        let db_source = eval_dir.join(&ds.index_db);
        if !db_source.exists() {
            eprintln!(
                "Warning: fixture DB not found: {} (skipping {})",
                db_source.display(),
                ds.repo
            );
            continue;
        }

        // Load known files from fixture DB
        let conn = db::init_db(
            db_source
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path"))?,
        )?;
        let files = db::get_all_files_with_exports_and_enrichment(&conn)?;
        let known_files: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();

        eprintln!(
            "\nE2E Eval: {} ({} cases, model: {model})",
            ds.repo,
            ds.cases.len()
        );
        eprintln!("{}", "-".repeat(60));

        // Create workspaces once per dataset (not per case).
        // Sessions run in plan mode (read-only), so there's no cross-contamination.
        let control_ws = if matches!(variant, Variant::All | Variant::Control) {
            eprintln!("  Preparing control workspace...");
            Some(workspace::create_workspace(
                &repo_dir,
                workspace::WorkspaceKind::Control,
                &codemap_bin,
            )?)
        } else {
            None
        };
        let treatment_ws = if matches!(variant, Variant::All | Variant::Treatment) {
            eprintln!("  Preparing treatment workspace...");
            Some(workspace::create_workspace(
                &repo_dir,
                workspace::WorkspaceKind::Treatment,
                &codemap_bin,
            )?)
        } else {
            None
        };
        let enriched_ws = if matches!(variant, Variant::All | Variant::Enriched) {
            eprintln!("  Preparing enriched workspace (setup + enrich --api)...");
            Some(workspace::create_workspace(
                &repo_dir,
                workspace::WorkspaceKind::Enriched,
                &codemap_bin,
            )?)
        } else {
            None
        };

        let mut results = Vec::new();

        for case in &ds.cases {
            if let Some(ref ids) = case_ids
                && !ids.contains(&case.id)
            {
                continue;
            }

            let expected: HashSet<String> =
                case.expected_files.iter().map(|f| f.path.clone()).collect();
            let task_prompt = TASK_PROMPT_TEMPLATE.replace("{query}", &case.query);

            eprintln!("\n  {} - {}", case.id, case.query);

            // Run control
            let control = if let Some(ref ws) = control_ws {
                eprintln!("    Running control...");
                match run_session_variant(
                    ws.path(),
                    workspace::WorkspaceKind::Control,
                    &task_prompt,
                    model,
                    max_turns,
                    timeout_secs,
                    &known_files,
                    &expected,
                    &case.id,
                    verbose,
                ) {
                    Ok(m) => Some(m),
                    Err(e) => {
                        eprintln!("    Control session failed: {e}");
                        None
                    }
                }
            } else {
                None
            };

            // Run treatment
            let treatment = if let Some(ref ws) = treatment_ws {
                eprintln!("    Running treatment...");
                match run_session_variant(
                    ws.path(),
                    workspace::WorkspaceKind::Treatment,
                    &task_prompt,
                    model,
                    max_turns,
                    timeout_secs,
                    &known_files,
                    &expected,
                    &case.id,
                    verbose,
                ) {
                    Ok(m) => Some(m),
                    Err(e) => {
                        eprintln!("    Treatment session failed: {e}");
                        None
                    }
                }
            } else {
                None
            };

            // Run enriched
            let enriched = if let Some(ref ws) = enriched_ws {
                eprintln!("    Running enriched...");
                match run_session_variant(
                    ws.path(),
                    workspace::WorkspaceKind::Enriched,
                    &task_prompt,
                    model,
                    max_turns,
                    timeout_secs,
                    &known_files,
                    &expected,
                    &case.id,
                    verbose,
                ) {
                    Ok(m) => Some(m),
                    Err(e) => {
                        eprintln!("    Enriched session failed: {e}");
                        None
                    }
                }
            } else {
                None
            };

            // Print per-case winner
            if control.is_some() || treatment.is_some() || enriched.is_some() {
                let winner = multi_case_winner(
                    control.as_ref(),
                    treatment.as_ref(),
                    enriched.as_ref(),
                    &expected,
                );
                eprintln!("    Winner: {winner}");
            }

            results.push(TaskResult {
                case_id: case.id.clone(),
                expected,
                control,
                treatment,
                enriched,
            });
        }

        if !results.is_empty() {
            let agg = compute_aggregate(&results);
            print_e2e_report(&ds.repo, &results, &agg, model, enrichment_model.as_deref());

            // Archive results before moving into all_dataset_reports
            if let Some(ref hist_conn) = history_conn {
                let metrics_json = serde_json::to_value(&agg)?;
                let config_json = json!({
                    "model": model,
                    "max_turns": max_turns,
                    "timeout_secs": timeout_secs,
                    "variant": format!("{variant:?}"),
                    "enrichment_model": &enrichment_model,
                });
                history::save_run(
                    hist_conn,
                    &git_commit,
                    git_dirty,
                    "e2e_eval",
                    &ds.repo,
                    &ds.language,
                    &metrics_json,
                    Some(&config_json),
                )?;
                let dirty_marker = if git_dirty { " (dirty)" } else { "" };
                eprintln!("Archived: {} @ {}{dirty_marker}", ds.repo, git_commit);
            }

            all_dataset_reports.push(DatasetReport {
                name: ds.repo.clone(),
                results,
                aggregate: agg,
            });
        }
    }

    // Write combined markdown report
    if !all_dataset_reports.is_empty() {
        write_markdown_report(
            &eval_dir,
            &all_dataset_reports,
            model,
            &git_commit,
            enrichment_model.as_deref(),
        )?;
    }

    Ok(())
}

/// Run a single session variant in a pre-created workspace.
#[allow(clippy::too_many_arguments)]
fn run_session_variant(
    ws_path: &Path,
    kind: workspace::WorkspaceKind,
    task_prompt: &str,
    model: &str,
    max_turns: usize,
    timeout_secs: u64,
    known_files: &HashSet<String>,
    expected_files: &HashSet<String>,
    case_id: &str,
    verbose: bool,
) -> Result<SessionMetrics> {
    let label = kind.label();

    let raw = session::run_claude_session(ws_path, task_prompt, model, max_turns, timeout_secs)?;

    if verbose {
        eprintln!("    [{label} exit code]: {:?}", raw.exit_code);
        eprintln!("    [{label} stderr]: {}", truncate(&raw.stderr, 500));
        eprintln!("    [{label} stdout lines]: {}", raw.stdout.lines().count());
        if raw.exit_code != Some(0) {
            // Dump first few lines of stdout for debugging failed sessions
            for line in raw.stdout.lines().take(5) {
                eprintln!("    [{label} stdout]: {}", truncate(line, 200));
            }
        }
    }

    let workspace_prefix = format!("{}/", ws_path.display());
    let metrics = session::parse_stream_output(
        &raw,
        known_files,
        expected_files,
        label,
        case_id,
        &workspace_prefix,
    );

    // Warn if treatment/enriched session didn't receive hook injection
    if kind != workspace::WorkspaceKind::Control && !metrics.hook_injected {
        eprintln!("    WARNING: {label} session did not receive codemap hook injection");
    }

    print_session_summary(label, &metrics, expected_files);
    Ok(metrics)
}

/// Validate that required tools are available.
fn check_prerequisites(variant: Variant) -> Result<()> {
    // 1. claude CLI exists and responds
    let output = Command::new("claude")
        .arg("--version")
        .output()
        .context("claude CLI not found — install Claude Code first")?;
    ensure!(
        output.status.success(),
        "claude CLI not working (exit code {:?})",
        output.status.code()
    );
    let version = String::from_utf8_lossy(&output.stdout);
    eprintln!("Using claude CLI: {}", version.trim());

    // 2. codemap binary exists
    let output = Command::new("codemap")
        .arg("--version")
        .output()
        .context("codemap binary not found — run `cargo build --release`")?;
    ensure!(
        output.status.success(),
        "codemap binary not working (exit code {:?})",
        output.status.code()
    );

    // 3. enriched variant needs an LLM API key for `codemap enrich --api`
    if matches!(variant, Variant::Enriched | Variant::All) {
        let has_gemini = std::env::var("GEMINI_API_KEY").is_ok_and(|v| !v.is_empty());
        let has_anthropic = std::env::var("ANTHROPIC_API_KEY").is_ok_and(|v| !v.is_empty());
        ensure!(
            has_gemini || has_anthropic,
            "Enriched variant requires GEMINI_API_KEY or ANTHROPIC_API_KEY to be set.\n\
             `codemap enrich --api` uses these to generate file summaries."
        );
    }

    Ok(())
}

/// Detect which enrichment model `codemap enrich --api` would use based on env vars.
///
/// The codemap CLI defaults to `--provider gemini` and uses model `gemini-2.5-flash-lite`.
/// Falls back to Anthropic (`claude-haiku-4-5-20251001`) if only `ANTHROPIC_API_KEY` is set.
fn detect_enrichment_model() -> Option<String> {
    if std::env::var("GEMINI_API_KEY").is_ok_and(|v| !v.is_empty()) {
        Some("gemini (gemini-2.5-flash-lite)".to_string())
    } else if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|v| !v.is_empty()) {
        Some("anthropic (claude-haiku-4-5-20251001)".to_string())
    } else {
        None
    }
}

fn print_session_summary(label: &str, metrics: &SessionMetrics, expected: &HashSet<String>) {
    let file_set = best_file_set(metrics);
    let r = recall(&file_set, expected);
    let p = precision_from_identified(&metrics.files_identified, expected);
    eprintln!(
        "    {label:10} {} tools, {} files read, recall={:.0}% precision={:.0}%, \
         {:.1}k in + {:.1}k out tokens, {:.1}s",
        metrics.tool_calls,
        metrics.files_read.len(),
        r * 100.0,
        p * 100.0,
        metrics.input_tokens as f64 / 1000.0,
        metrics.output_tokens as f64 / 1000.0,
        metrics.wall_clock_ms as f64 / 1000.0,
    );
    if metrics.codemap_calls > 0 {
        eprintln!("               codemap calls: {}", metrics.codemap_calls);
    }
}

// --- Metrics helpers ---

/// Get the best available file set: prefer structured output, fall back to mentions.
fn best_file_set(m: &SessionMetrics) -> HashSet<String> {
    if !m.files_identified.is_empty() {
        m.files_identified.iter().cloned().collect()
    } else {
        m.files_mentioned.clone()
    }
}

fn recall(discovered: &HashSet<String>, expected: &HashSet<String>) -> f64 {
    if expected.is_empty() {
        return 0.0;
    }
    let hits = expected.iter().filter(|f| discovered.contains(*f)).count();
    hits as f64 / expected.len() as f64
}

fn precision_from_identified(identified: &[String], expected: &HashSet<String>) -> f64 {
    if identified.is_empty() {
        return 0.0;
    }
    let hits = identified
        .iter()
        .filter(|f| expected.contains(f.as_str()))
        .count();
    hits as f64 / identified.len() as f64
}

/// Determine the winner among available variants.
///
/// Best recall wins; on tie (within 1%), fewest tool calls wins.
fn multi_case_winner(
    control: Option<&SessionMetrics>,
    treatment: Option<&SessionMetrics>,
    enriched: Option<&SessionMetrics>,
    expected: &HashSet<String>,
) -> &'static str {
    let mut candidates: Vec<(&str, f64, usize)> = Vec::new();
    if let Some(c) = control {
        candidates.push(("control", recall(&best_file_set(c), expected), c.tool_calls));
    }
    if let Some(t) = treatment {
        candidates.push((
            "treatment",
            recall(&best_file_set(t), expected),
            t.tool_calls,
        ));
    }
    if let Some(e) = enriched {
        candidates.push((
            "enriched",
            recall(&best_file_set(e), expected),
            e.tool_calls,
        ));
    }

    if candidates.len() < 2 {
        return candidates.first().map_or("n/a", |(name, _, _)| *name);
    }

    // Sort by recall desc, then tool_calls asc
    candidates.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.2.cmp(&b.2))
    });

    let best = &candidates[0];
    let second = &candidates[1];

    // Clear recall advantage → winner; otherwise tie (tool_calls already sorted)
    if best.1 > second.1 + 0.01 {
        best.0
    } else if best.2 < second.2 {
        // Recalls within 1%, fewer tool calls wins
        best.0
    } else {
        "tie"
    }
}

fn reduction_pct(control: f64, treatment: f64) -> f64 {
    if control > 0.0 {
        (control - treatment) / control * 100.0
    } else {
        0.0
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let end = (0..=max)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        &s[..end]
    }
}

// --- Aggregate computation ---

impl E2eAggregate {
    /// Accumulate metrics from a single session into the running sums.
    fn accumulate(
        &mut self,
        m: &SessionMetrics,
        expected: &HashSet<String>,
        kind: workspace::WorkspaceKind,
    ) {
        let files = best_file_set(m);
        let r = recall(&files, expected);
        let p = precision_from_identified(&m.files_identified, expected);

        match kind {
            workspace::WorkspaceKind::Control => {
                self.avg_control_recall += r;
                self.avg_control_precision += p;
                self.avg_control_tool_calls += m.tool_calls as f64;
                self.avg_control_input_tokens += m.input_tokens as f64;
                self.avg_control_output_tokens += m.output_tokens as f64;
                self.avg_control_wall_time_ms += m.wall_clock_ms as f64;
                self.avg_control_codemap_calls += m.codemap_calls as f64;
            }
            workspace::WorkspaceKind::Treatment => {
                self.avg_treatment_recall += r;
                self.avg_treatment_precision += p;
                self.avg_treatment_tool_calls += m.tool_calls as f64;
                self.avg_treatment_input_tokens += m.input_tokens as f64;
                self.avg_treatment_output_tokens += m.output_tokens as f64;
                self.avg_treatment_wall_time_ms += m.wall_clock_ms as f64;
                self.avg_treatment_codemap_calls += m.codemap_calls as f64;
            }
            workspace::WorkspaceKind::Enriched => {
                self.avg_enriched_recall += r;
                self.avg_enriched_precision += p;
                self.avg_enriched_tool_calls += m.tool_calls as f64;
                self.avg_enriched_input_tokens += m.input_tokens as f64;
                self.avg_enriched_output_tokens += m.output_tokens as f64;
                self.avg_enriched_wall_time_ms += m.wall_clock_ms as f64;
                self.avg_enriched_codemap_calls += m.codemap_calls as f64;
            }
        }
    }

    /// Divide all running sums by n to produce averages, then compute reduction percentages.
    fn finalize(
        &mut self,
        n: usize,
        c_first_count: usize,
        t_first_count: usize,
        e_first_count: usize,
    ) {
        let nf = n as f64;
        self.avg_control_recall /= nf;
        self.avg_treatment_recall /= nf;
        self.avg_enriched_recall /= nf;
        self.avg_control_precision /= nf;
        self.avg_treatment_precision /= nf;
        self.avg_enriched_precision /= nf;
        self.avg_control_tool_calls /= nf;
        self.avg_treatment_tool_calls /= nf;
        self.avg_enriched_tool_calls /= nf;
        self.avg_control_input_tokens /= nf;
        self.avg_control_output_tokens /= nf;
        self.avg_treatment_input_tokens /= nf;
        self.avg_treatment_output_tokens /= nf;
        self.avg_enriched_input_tokens /= nf;
        self.avg_enriched_output_tokens /= nf;
        self.avg_control_wall_time_ms /= nf;
        self.avg_treatment_wall_time_ms /= nf;
        self.avg_enriched_wall_time_ms /= nf;
        self.avg_control_codemap_calls /= nf;
        self.avg_treatment_codemap_calls /= nf;
        self.avg_enriched_codemap_calls /= nf;

        if c_first_count > 0 {
            self.avg_control_first_relevant /= c_first_count as f64;
        }
        if t_first_count > 0 {
            self.avg_treatment_first_relevant /= t_first_count as f64;
        }
        if e_first_count > 0 {
            self.avg_enriched_first_relevant /= e_first_count as f64;
        }

        self.tool_call_reduction_pct =
            reduction_pct(self.avg_control_tool_calls, self.avg_treatment_tool_calls);
        let control_total_tokens = self.avg_control_input_tokens + self.avg_control_output_tokens;
        let treatment_total_tokens =
            self.avg_treatment_input_tokens + self.avg_treatment_output_tokens;
        self.token_reduction_pct = reduction_pct(control_total_tokens, treatment_total_tokens);
        self.time_reduction_pct = reduction_pct(
            self.avg_control_wall_time_ms,
            self.avg_treatment_wall_time_ms,
        );
    }
}

fn compute_aggregate(results: &[TaskResult]) -> E2eAggregate {
    // Require at least two variant types for comparison mode
    let has_comparison = results.iter().any(|r| r.has_multiple_variants());

    if !has_comparison {
        return compute_single_variant_aggregate(results);
    }

    let multi: Vec<_> = results
        .iter()
        .filter(|r| r.has_multiple_variants())
        .collect();

    let n = multi.len();
    let mut agg = E2eAggregate {
        count: n,
        ..Default::default()
    };
    let mut c_first_count = 0usize;
    let mut t_first_count = 0usize;
    let mut e_first_count = 0usize;

    for r in &multi {
        if let Some(c) = &r.control {
            agg.accumulate(c, &r.expected, workspace::WorkspaceKind::Control);
            if let Some(turn) = c.first_relevant_file_turn {
                agg.avg_control_first_relevant += turn as f64;
                c_first_count += 1;
            }
        }
        if let Some(t) = &r.treatment {
            agg.accumulate(t, &r.expected, workspace::WorkspaceKind::Treatment);
            if let Some(turn) = t.first_relevant_file_turn {
                agg.avg_treatment_first_relevant += turn as f64;
                t_first_count += 1;
            }
        }
        if let Some(e) = &r.enriched {
            agg.accumulate(e, &r.expected, workspace::WorkspaceKind::Enriched);
            if let Some(turn) = e.first_relevant_file_turn {
                agg.avg_enriched_first_relevant += turn as f64;
                e_first_count += 1;
            }
        }

        match multi_case_winner(
            r.control.as_ref(),
            r.treatment.as_ref(),
            r.enriched.as_ref(),
            &r.expected,
        ) {
            "treatment" => agg.treatment_wins += 1,
            "control" => agg.control_wins += 1,
            "enriched" => agg.enriched_wins += 1,
            _ => agg.ties += 1,
        }
    }

    agg.finalize(n, c_first_count, t_first_count, e_first_count);
    agg
}

fn compute_single_variant_aggregate(results: &[TaskResult]) -> E2eAggregate {
    let n = results.len();
    if n == 0 {
        return E2eAggregate::default();
    }

    let mut agg = E2eAggregate {
        count: n,
        ..Default::default()
    };

    for r in results {
        if let Some(c) = &r.control {
            agg.accumulate(c, &r.expected, workspace::WorkspaceKind::Control);
        }
        if let Some(t) = &r.treatment {
            agg.accumulate(t, &r.expected, workspace::WorkspaceKind::Treatment);
        }
        if let Some(e) = &r.enriched {
            agg.accumulate(e, &r.expected, workspace::WorkspaceKind::Enriched);
        }
    }

    agg.finalize(n, 0, 0, 0);
    agg
}

// --- Report printing ---

/// Whether any result in the set has enriched data.
fn has_enriched(results: &[TaskResult]) -> bool {
    results.iter().any(|r| r.enriched.is_some())
}

fn print_e2e_report(
    dataset: &str,
    results: &[TaskResult],
    agg: &E2eAggregate,
    model: &str,
    enrichment_model: Option<&str>,
) {
    let has_comparison = results.iter().any(|r| r.has_multiple_variants());
    let show_enriched = has_enriched(results);

    let width = if show_enriched { 78 } else { 66 };

    println!();
    println!("{}", "\u{2550}".repeat(width));
    if let Some(enrich_model) = enrichment_model {
        println!(
            "End-to-End Eval: {dataset} ({} tasks, model: {model}, enrichment: {enrich_model})",
            agg.count
        );
    } else {
        println!(
            "End-to-End Eval: {dataset} ({} tasks, model: {model})",
            agg.count
        );
    }
    println!("{}", "\u{2550}".repeat(width));

    if has_comparison {
        println!();
        if show_enriched {
            println!(
                "  {:20} {:>10} {:>10} {:>10} {:>10}",
                "Metric", "Control", "Treatment", "Enriched", "Delta"
            );
            println!("  {}", "\u{2500}".repeat(62));
        } else {
            println!(
                "  {:20} {:>10} {:>10} {:>10}",
                "Metric", "Control", "Treatment", "Delta"
            );
            println!("  {}", "\u{2500}".repeat(52));
        }

        // Recall
        print_metric_row(
            "Recall",
            agg.avg_control_recall,
            agg.avg_treatment_recall,
            if show_enriched {
                Some(agg.avg_enriched_recall)
            } else {
                None
            },
        );
        // Precision
        print_metric_row(
            "Precision",
            agg.avg_control_precision,
            agg.avg_treatment_precision,
            if show_enriched {
                Some(agg.avg_enriched_precision)
            } else {
                None
            },
        );
        // Tool calls
        if show_enriched {
            println!(
                "  {:20} {:>10.1} {:>10.1} {:>10.1} {:>+9.0}%",
                "Tool calls",
                agg.avg_control_tool_calls,
                agg.avg_treatment_tool_calls,
                agg.avg_enriched_tool_calls,
                -agg.tool_call_reduction_pct,
            );
        } else {
            println!(
                "  {:20} {:>10.1} {:>10.1} {:>+9.0}%",
                "Tool calls",
                agg.avg_control_tool_calls,
                agg.avg_treatment_tool_calls,
                -agg.tool_call_reduction_pct,
            );
        }
        // Tokens (input)
        if show_enriched {
            println!(
                "  {:20} {:>9.1}k {:>9.1}k {:>9.1}k {:>+9.0}%",
                "Input tokens",
                agg.avg_control_input_tokens / 1000.0,
                agg.avg_treatment_input_tokens / 1000.0,
                agg.avg_enriched_input_tokens / 1000.0,
                -reduction_pct(agg.avg_control_input_tokens, agg.avg_treatment_input_tokens),
            );
        } else {
            println!(
                "  {:20} {:>9.1}k {:>9.1}k {:>+9.0}%",
                "Input tokens",
                agg.avg_control_input_tokens / 1000.0,
                agg.avg_treatment_input_tokens / 1000.0,
                -reduction_pct(agg.avg_control_input_tokens, agg.avg_treatment_input_tokens),
            );
        }
        // Tokens (output)
        if show_enriched {
            println!(
                "  {:20} {:>9.1}k {:>9.1}k {:>9.1}k {:>+9.0}%",
                "Output tokens",
                agg.avg_control_output_tokens / 1000.0,
                agg.avg_treatment_output_tokens / 1000.0,
                agg.avg_enriched_output_tokens / 1000.0,
                -reduction_pct(
                    agg.avg_control_output_tokens,
                    agg.avg_treatment_output_tokens
                ),
            );
        } else {
            println!(
                "  {:20} {:>9.1}k {:>9.1}k {:>+9.0}%",
                "Output tokens",
                agg.avg_control_output_tokens / 1000.0,
                agg.avg_treatment_output_tokens / 1000.0,
                -reduction_pct(
                    agg.avg_control_output_tokens,
                    agg.avg_treatment_output_tokens
                ),
            );
        }
        // Wall time
        if show_enriched {
            println!(
                "  {:20} {:>9.1}s {:>9.1}s {:>9.1}s {:>+9.0}%",
                "Wall time",
                agg.avg_control_wall_time_ms / 1000.0,
                agg.avg_treatment_wall_time_ms / 1000.0,
                agg.avg_enriched_wall_time_ms / 1000.0,
                -agg.time_reduction_pct,
            );
        } else {
            println!(
                "  {:20} {:>9.1}s {:>9.1}s {:>+9.0}%",
                "Wall time",
                agg.avg_control_wall_time_ms / 1000.0,
                agg.avg_treatment_wall_time_ms / 1000.0,
                -agg.time_reduction_pct,
            );
        }
        // Codemap calls
        if show_enriched {
            println!(
                "  {:20} {:>10.1} {:>10.1} {:>10.1} {:>10}",
                "Codemap calls",
                agg.avg_control_codemap_calls,
                agg.avg_treatment_codemap_calls,
                agg.avg_enriched_codemap_calls,
                "n/a",
            );
        } else {
            println!(
                "  {:20} {:>10.1} {:>10.1} {:>10}",
                "Codemap calls",
                agg.avg_control_codemap_calls,
                agg.avg_treatment_codemap_calls,
                "n/a",
            );
        }

        println!();
        if show_enriched {
            println!(
                "  Wins: Treatment {} / Enriched {} / Control {} / Tie {}",
                agg.treatment_wins, agg.enriched_wins, agg.control_wins, agg.ties,
            );
        } else {
            println!(
                "  Win/Loss/Tie: Treatment {} / Control {} / Tie {}",
                agg.treatment_wins, agg.control_wins, agg.ties,
            );
        }
    } else {
        // Single variant mode
        println!();
        for r in results {
            let m = r
                .control
                .as_ref()
                .or(r.treatment.as_ref())
                .or(r.enriched.as_ref());
            if let Some(m) = m {
                let total = m.input_tokens + m.output_tokens;
                println!(
                    "  {:12} [{}] {} tools, {} files read, {:.1}k tokens, {:.1}s",
                    r.case_id,
                    m.variant,
                    m.tool_calls,
                    m.files_read.len(),
                    total as f64 / 1000.0,
                    m.wall_clock_ms as f64 / 1000.0,
                );
            }
        }
    }

    println!("{}", "\u{2550}".repeat(width));
    println!();
}

/// Write combined eval results to `eval/RESULTS.md`.
fn write_markdown_report(
    eval_dir: &Path,
    reports: &[DatasetReport],
    model: &str,
    git_commit: &str,
    enrichment_model: Option<&str>,
) -> Result<()> {
    let path = eval_dir.join("RESULTS.md");
    let mut md = String::new();

    writeln!(md, "# E2E Eval Results").unwrap();
    writeln!(md).unwrap();
    let enrichment_suffix = enrichment_model
        .map(|m| format!(" | Enrichment model: `{m}`"))
        .unwrap_or_default();
    writeln!(
        md,
        "> Auto-generated by `codemap-eval e2e`. Commit: `{git_commit}` | Model: `{model}`{enrichment_suffix}"
    )
    .unwrap();
    writeln!(md).unwrap();
    writeln!(md, "## How to read these results").unwrap();
    writeln!(md).unwrap();
    writeln!(
        md,
        "Each dataset runs tasks in multiple variants: **control** (Claude Code alone), \
         **treatment** (Claude Code + codemap), and optionally **enriched** \
         (Claude Code + codemap + LLM-generated summaries via `codemap enrich --api`). \
         All sessions explore the same repo copy in read-only mode \
         (`--permission-mode plan`) and report which files are relevant to the task."
    )
    .unwrap();
    writeln!(md).unwrap();
    writeln!(md, "| Metric | Meaning |").unwrap();
    writeln!(md, "|--------|---------|").unwrap();
    writeln!(
        md,
        "| Recall | Fraction of expected files that Claude found |"
    )
    .unwrap();
    writeln!(
        md,
        "| Precision | Fraction of files Claude reported that were actually relevant |"
    )
    .unwrap();
    writeln!(
        md,
        "| Tool calls | Average number of tool invocations (Read, Grep, Glob, Bash, etc.) |"
    )
    .unwrap();
    writeln!(
        md,
        "| Input tokens | Context tokens consumed (prompt + tool results) |"
    )
    .unwrap();
    writeln!(md, "| Output tokens | Tokens generated by the model |").unwrap();
    writeln!(md, "| Wall time | Clock time per session |").unwrap();
    writeln!(md).unwrap();
    writeln!(
        md,
        "Negative deltas on tool calls, tokens, and wall time mean the treatment was \
         more efficient. Positive deltas on recall and precision mean it found better results."
    )
    .unwrap();
    writeln!(md).unwrap();
    writeln!(
        md,
        "**Note:** Treatment sessions use `codemap setup` with heuristic-only metadata \
         (no LLM enrichment). Results may improve further with `codemap enrich --api`."
    )
    .unwrap();

    for report in reports {
        writeln!(md).unwrap();
        write_dataset_section(&mut md, &report.name, &report.results, &report.aggregate);
    }

    std::fs::write(&path, &md).with_context(|| format!("failed to write {}", path.display()))?;
    eprintln!("Wrote {}", path.display());
    Ok(())
}

fn write_dataset_section(
    md: &mut String,
    dataset: &str,
    results: &[TaskResult],
    agg: &E2eAggregate,
) {
    let has_comparison = results.iter().any(|r| r.has_multiple_variants());
    let show_enriched = has_enriched(results);

    writeln!(md, "## {dataset} ({} tasks)", agg.count).unwrap();
    writeln!(md).unwrap();

    if has_comparison {
        if show_enriched {
            writeln!(md, "| Metric | Control | Treatment | Enriched | Delta |").unwrap();
            writeln!(md, "|--------|---------|-----------|----------|-------|").unwrap();
            writeln!(
                md,
                "| Recall | {:.2} | {:.2} | {:.2} | {:+.0}% |",
                agg.avg_control_recall,
                agg.avg_treatment_recall,
                agg.avg_enriched_recall,
                -reduction_pct(agg.avg_control_recall, agg.avg_treatment_recall),
            )
            .unwrap();
            writeln!(
                md,
                "| Precision | {:.2} | {:.2} | {:.2} | {:+.0}% |",
                agg.avg_control_precision,
                agg.avg_treatment_precision,
                agg.avg_enriched_precision,
                -reduction_pct(agg.avg_control_precision, agg.avg_treatment_precision),
            )
            .unwrap();
            writeln!(
                md,
                "| Tool calls | {:.1} | {:.1} | {:.1} | {:+.0}% |",
                agg.avg_control_tool_calls,
                agg.avg_treatment_tool_calls,
                agg.avg_enriched_tool_calls,
                -agg.tool_call_reduction_pct,
            )
            .unwrap();
            writeln!(
                md,
                "| Input tokens | {:.1}k | {:.1}k | {:.1}k | {:+.0}% |",
                agg.avg_control_input_tokens / 1000.0,
                agg.avg_treatment_input_tokens / 1000.0,
                agg.avg_enriched_input_tokens / 1000.0,
                -reduction_pct(agg.avg_control_input_tokens, agg.avg_treatment_input_tokens),
            )
            .unwrap();
            writeln!(
                md,
                "| Output tokens | {:.1}k | {:.1}k | {:.1}k | {:+.0}% |",
                agg.avg_control_output_tokens / 1000.0,
                agg.avg_treatment_output_tokens / 1000.0,
                agg.avg_enriched_output_tokens / 1000.0,
                -reduction_pct(
                    agg.avg_control_output_tokens,
                    agg.avg_treatment_output_tokens
                ),
            )
            .unwrap();
            writeln!(
                md,
                "| Wall time | {:.1}s | {:.1}s | {:.1}s | {:+.0}% |",
                agg.avg_control_wall_time_ms / 1000.0,
                agg.avg_treatment_wall_time_ms / 1000.0,
                agg.avg_enriched_wall_time_ms / 1000.0,
                -agg.time_reduction_pct,
            )
            .unwrap();
        } else {
            writeln!(md, "| Metric | Control | Treatment | Delta |").unwrap();
            writeln!(md, "|--------|---------|-----------|-------|").unwrap();
            writeln!(
                md,
                "| Recall | {:.2} | {:.2} | {:+.0}% |",
                agg.avg_control_recall,
                agg.avg_treatment_recall,
                -reduction_pct(agg.avg_control_recall, agg.avg_treatment_recall),
            )
            .unwrap();
            writeln!(
                md,
                "| Precision | {:.2} | {:.2} | {:+.0}% |",
                agg.avg_control_precision,
                agg.avg_treatment_precision,
                -reduction_pct(agg.avg_control_precision, agg.avg_treatment_precision),
            )
            .unwrap();
            writeln!(
                md,
                "| Tool calls | {:.1} | {:.1} | {:+.0}% |",
                agg.avg_control_tool_calls,
                agg.avg_treatment_tool_calls,
                -agg.tool_call_reduction_pct,
            )
            .unwrap();
            writeln!(
                md,
                "| Input tokens | {:.1}k | {:.1}k | {:+.0}% |",
                agg.avg_control_input_tokens / 1000.0,
                agg.avg_treatment_input_tokens / 1000.0,
                -reduction_pct(agg.avg_control_input_tokens, agg.avg_treatment_input_tokens),
            )
            .unwrap();
            writeln!(
                md,
                "| Output tokens | {:.1}k | {:.1}k | {:+.0}% |",
                agg.avg_control_output_tokens / 1000.0,
                agg.avg_treatment_output_tokens / 1000.0,
                -reduction_pct(
                    agg.avg_control_output_tokens,
                    agg.avg_treatment_output_tokens
                ),
            )
            .unwrap();
            writeln!(
                md,
                "| Wall time | {:.1}s | {:.1}s | {:+.0}% |",
                agg.avg_control_wall_time_ms / 1000.0,
                agg.avg_treatment_wall_time_ms / 1000.0,
                -agg.time_reduction_pct,
            )
            .unwrap();
        }
        writeln!(md).unwrap();
        if show_enriched {
            writeln!(
                md,
                "**Wins:** Treatment {} / Enriched {} / Control {} / Tie {}",
                agg.treatment_wins, agg.enriched_wins, agg.control_wins, agg.ties,
            )
            .unwrap();
        } else {
            writeln!(
                md,
                "**Win/Loss/Tie:** Treatment {} / Control {} / Tie {}",
                agg.treatment_wins, agg.control_wins, agg.ties,
            )
            .unwrap();
        }
        writeln!(md).unwrap();

        // Per-case breakdown
        writeln!(md, "<details><summary>Per-case breakdown</summary>").unwrap();
        writeln!(md).unwrap();
        if show_enriched {
            writeln!(
                md,
                "| Case | Ctrl tools | Treat tools | Enrich tools | Ctrl recall | Treat recall | Enrich recall | Winner |"
            )
            .unwrap();
            writeln!(
                md,
                "|------|-----------|------------|-------------|------------|-------------|--------------|--------|"
            )
            .unwrap();
        } else {
            writeln!(
                md,
                "| Case | Control tools | Treatment tools | Control recall | Treatment recall | Winner |"
            )
            .unwrap();
            writeln!(
                md,
                "|------|--------------|----------------|---------------|-----------------|--------|"
            )
            .unwrap();
        }
        for r in results {
            let winner = multi_case_winner(
                r.control.as_ref(),
                r.treatment.as_ref(),
                r.enriched.as_ref(),
                &r.expected,
            );
            if show_enriched {
                let c_tools = r
                    .control
                    .as_ref()
                    .map_or("-".to_string(), |m| m.tool_calls.to_string());
                let t_tools = r
                    .treatment
                    .as_ref()
                    .map_or("-".to_string(), |m| m.tool_calls.to_string());
                let e_tools = r
                    .enriched
                    .as_ref()
                    .map_or("-".to_string(), |m| m.tool_calls.to_string());
                let c_recall = r.control.as_ref().map_or("-".to_string(), |m| {
                    format!("{:.0}%", recall(&best_file_set(m), &r.expected) * 100.0)
                });
                let t_recall = r.treatment.as_ref().map_or("-".to_string(), |m| {
                    format!("{:.0}%", recall(&best_file_set(m), &r.expected) * 100.0)
                });
                let e_recall = r.enriched.as_ref().map_or("-".to_string(), |m| {
                    format!("{:.0}%", recall(&best_file_set(m), &r.expected) * 100.0)
                });
                writeln!(
                    md,
                    "| {} | {c_tools} | {t_tools} | {e_tools} | {c_recall} | {t_recall} | {e_recall} | {winner} |",
                    r.case_id,
                )
                .unwrap();
            } else if let (Some(c), Some(t)) = (&r.control, &r.treatment) {
                let c_recall = recall(&best_file_set(c), &r.expected);
                let t_recall = recall(&best_file_set(t), &r.expected);
                writeln!(
                    md,
                    "| {} | {} | {} | {:.0}% | {:.0}% | {winner} |",
                    r.case_id,
                    c.tool_calls,
                    t.tool_calls,
                    c_recall * 100.0,
                    t_recall * 100.0,
                )
                .unwrap();
            }
        }
        writeln!(md).unwrap();
        writeln!(md, "</details>").unwrap();
    } else {
        // Single variant
        writeln!(
            md,
            "| Case | Variant | Tools | Files read | Tokens | Time |"
        )
        .unwrap();
        writeln!(md, "|------|---------|-------|-----------|--------|------|").unwrap();
        for r in results {
            if let Some(m) = r
                .control
                .as_ref()
                .or(r.treatment.as_ref())
                .or(r.enriched.as_ref())
            {
                let total = m.input_tokens + m.output_tokens;
                writeln!(
                    md,
                    "| {} | {} | {} | {} | {:.1}k | {:.1}s |",
                    r.case_id,
                    m.variant,
                    m.tool_calls,
                    m.files_read.len(),
                    total as f64 / 1000.0,
                    m.wall_clock_ms as f64 / 1000.0,
                )
                .unwrap();
            }
        }
    }
}

fn print_metric_row(name: &str, control: f64, treatment: f64, enriched: Option<f64>) {
    let delta_pct = if control > 0.0 {
        (treatment - control) / control * 100.0
    } else {
        0.0
    };
    if let Some(e) = enriched {
        println!(
            "  {:20} {:>10.2} {:>10.2} {:>10.2} {:>+9.0}%",
            name, control, treatment, e, delta_pct,
        );
    } else {
        println!(
            "  {:20} {:>10.2} {:>10.2} {:>+9.0}%",
            name, control, treatment, delta_pct,
        );
    }
}
