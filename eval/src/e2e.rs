/// End-to-end eval orchestration: run real Claude Code sessions with and without codemap.
///
/// Unlike the simulation-based A/B eval (ab.rs), this spawns the actual `claude` CLI
/// and measures whether Claude Code completes file discovery tasks better when codemap
/// is available as a tool via `--append-system-prompt`.
use crate::history;
use crate::session::{self, SessionMetrics};
use crate::workspace;
use anyhow::{Context, Result, ensure};
use codemap::db;
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

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
}

#[derive(Debug, Clone, Default, serde::Serialize)]
struct E2eAggregate {
    count: usize,

    // Recall & precision
    avg_control_recall: f64,
    avg_treatment_recall: f64,
    avg_control_precision: f64,
    avg_treatment_precision: f64,

    // Efficiency
    avg_control_tool_calls: f64,
    avg_treatment_tool_calls: f64,
    tool_call_reduction_pct: f64,
    avg_control_tokens: f64,
    avg_treatment_tokens: f64,
    token_reduction_pct: f64,
    avg_control_wall_time_ms: f64,
    avg_treatment_wall_time_ms: f64,
    time_reduction_pct: f64,

    // Codemap usage
    avg_control_codemap_calls: f64,
    avg_treatment_codemap_calls: f64,

    // Speed
    avg_control_first_relevant: f64,
    avg_treatment_first_relevant: f64,

    // Wins
    treatment_wins: usize,
    control_wins: usize,
    ties: usize,
}

/// Run the full end-to-end eval pipeline.
#[allow(clippy::too_many_arguments)]
pub fn run_e2e_eval(
    dataset_path: &Path,
    repo_dir: &Path,
    model: &str,
    max_turns: usize,
    timeout_secs: u64,
    cases_filter: Option<&str>,
    variant: crate::ab::Variant,
    no_archive: bool,
    verbose: bool,
) -> Result<()> {
    check_prerequisites()?;

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

    let repo_dir = std::fs::canonicalize(repo_dir)
        .with_context(|| format!("repo dir not found: {}", repo_dir.display()))?;

    for ds in &datasets {
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
            let control = if matches!(
                variant,
                crate::ab::Variant::Both | crate::ab::Variant::Control
            ) {
                eprintln!("    Running control...");
                match run_session_variant(
                    &repo_dir,
                    false,
                    None,
                    &codemap_bin,
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
            let treatment = if matches!(
                variant,
                crate::ab::Variant::Both | crate::ab::Variant::Treatment
            ) {
                eprintln!("    Running treatment...");
                match run_session_variant(
                    &repo_dir,
                    true,
                    Some(db_source.as_path()),
                    &codemap_bin,
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

            // Print per-case summary
            if let (Some(c), Some(t)) = (&control, &treatment) {
                let winner = case_winner(c, t, &expected);
                eprintln!("    Winner: {winner}");
            }

            results.push(TaskResult {
                case_id: case.id.clone(),
                expected,
                control,
                treatment,
            });
        }

        if !results.is_empty() {
            let agg = compute_aggregate(&results);
            print_e2e_report(&ds.repo, &results, &agg, model);

            // Archive results
            if let Some(ref hist_conn) = history_conn {
                let metrics_json = serde_json::to_value(&agg)?;
                let config_json = json!({
                    "model": model,
                    "max_turns": max_turns,
                    "timeout_secs": timeout_secs,
                    "variant": format!("{variant:?}"),
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
        }
    }

    Ok(())
}

/// Run a single session variant (control or treatment) in an isolated workspace.
#[allow(clippy::too_many_arguments)]
fn run_session_variant(
    repo_dir: &Path,
    is_treatment: bool,
    index_db: Option<&Path>,
    codemap_bin: &Path,
    task_prompt: &str,
    model: &str,
    max_turns: usize,
    timeout_secs: u64,
    known_files: &HashSet<String>,
    expected_files: &HashSet<String>,
    case_id: &str,
    verbose: bool,
) -> Result<SessionMetrics> {
    let label = if is_treatment { "treatment" } else { "control" };
    let ws = workspace::create_workspace(repo_dir, is_treatment, index_db, codemap_bin)?;

    let raw = session::run_claude_session(
        ws.path(),
        task_prompt,
        model,
        max_turns,
        timeout_secs,
        ws.system_prompt.as_deref(),
    )?;

    if verbose {
        eprintln!("    [{label} exit code]: {:?}", raw.exit_code);
        eprintln!("    [{label} stderr]: {}", truncate(&raw.stderr, 500));
        eprintln!("    [{label} stdout lines]: {}", raw.stdout.lines().count());
    }

    let workspace_prefix = format!("{}/", ws.path().display());
    let metrics = session::parse_stream_output(
        &raw,
        known_files,
        expected_files,
        label,
        case_id,
        &workspace_prefix,
    );

    print_session_summary(label, &metrics, expected_files);
    Ok(metrics)
}

/// Validate that required tools are available.
fn check_prerequisites() -> Result<()> {
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

    Ok(())
}

fn print_session_summary(label: &str, metrics: &SessionMetrics, expected: &HashSet<String>) {
    let file_set = best_file_set(metrics);
    let r = recall(&file_set, expected);
    let p = precision_from_identified(&metrics.files_identified, expected);
    eprintln!(
        "    {label:10} {} tools, {} files read, recall={:.0}% precision={:.0}%, \
         {}+{} tokens, {:.1}s",
        metrics.tool_calls,
        metrics.files_read.len(),
        r * 100.0,
        p * 100.0,
        metrics.input_tokens,
        metrics.output_tokens,
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

fn case_winner(
    control: &SessionMetrics,
    treatment: &SessionMetrics,
    expected: &HashSet<String>,
) -> &'static str {
    let c_recall = recall(&best_file_set(control), expected);
    let t_recall = recall(&best_file_set(treatment), expected);
    if t_recall > c_recall + 0.01 {
        "treatment"
    } else if c_recall > t_recall + 0.01 {
        "control"
    } else if treatment.tool_calls < control.tool_calls {
        "treatment"
    } else if control.tool_calls < treatment.tool_calls {
        "control"
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
    fn accumulate(&mut self, m: &SessionMetrics, expected: &HashSet<String>, is_treatment: bool) {
        let files = best_file_set(m);
        let r = recall(&files, expected);
        let p = precision_from_identified(&m.files_identified, expected);

        if is_treatment {
            self.avg_treatment_recall += r;
            self.avg_treatment_precision += p;
            self.avg_treatment_tool_calls += m.tool_calls as f64;
            self.avg_treatment_tokens += (m.input_tokens + m.output_tokens) as f64;
            self.avg_treatment_wall_time_ms += m.wall_clock_ms as f64;
            self.avg_treatment_codemap_calls += m.codemap_calls as f64;
        } else {
            self.avg_control_recall += r;
            self.avg_control_precision += p;
            self.avg_control_tool_calls += m.tool_calls as f64;
            self.avg_control_tokens += (m.input_tokens + m.output_tokens) as f64;
            self.avg_control_wall_time_ms += m.wall_clock_ms as f64;
            self.avg_control_codemap_calls += m.codemap_calls as f64;
        }
    }

    /// Divide all running sums by n to produce averages, then compute reduction percentages.
    fn finalize(&mut self, n: usize, c_first_count: usize, t_first_count: usize) {
        let nf = n as f64;
        self.avg_control_recall /= nf;
        self.avg_treatment_recall /= nf;
        self.avg_control_precision /= nf;
        self.avg_treatment_precision /= nf;
        self.avg_control_tool_calls /= nf;
        self.avg_treatment_tool_calls /= nf;
        self.avg_control_tokens /= nf;
        self.avg_treatment_tokens /= nf;
        self.avg_control_wall_time_ms /= nf;
        self.avg_treatment_wall_time_ms /= nf;
        self.avg_control_codemap_calls /= nf;
        self.avg_treatment_codemap_calls /= nf;

        if c_first_count > 0 {
            self.avg_control_first_relevant /= c_first_count as f64;
        }
        if t_first_count > 0 {
            self.avg_treatment_first_relevant /= t_first_count as f64;
        }

        self.tool_call_reduction_pct =
            reduction_pct(self.avg_control_tool_calls, self.avg_treatment_tool_calls);
        self.token_reduction_pct =
            reduction_pct(self.avg_control_tokens, self.avg_treatment_tokens);
        self.time_reduction_pct = reduction_pct(
            self.avg_control_wall_time_ms,
            self.avg_treatment_wall_time_ms,
        );
    }
}

fn compute_aggregate(results: &[TaskResult]) -> E2eAggregate {
    let paired: Vec<_> = results
        .iter()
        .filter(|r| r.control.is_some() && r.treatment.is_some())
        .collect();

    if paired.is_empty() {
        return compute_single_variant_aggregate(results);
    }

    let n = paired.len();
    let mut agg = E2eAggregate {
        count: n,
        ..Default::default()
    };
    let mut c_first_count = 0usize;
    let mut t_first_count = 0usize;

    for r in &paired {
        let c = r.control.as_ref().unwrap();
        let t = r.treatment.as_ref().unwrap();

        agg.accumulate(c, &r.expected, false);
        agg.accumulate(t, &r.expected, true);

        if let Some(turn) = c.first_relevant_file_turn {
            agg.avg_control_first_relevant += turn as f64;
            c_first_count += 1;
        }
        if let Some(turn) = t.first_relevant_file_turn {
            agg.avg_treatment_first_relevant += turn as f64;
            t_first_count += 1;
        }

        match case_winner(c, t, &r.expected) {
            "treatment" => agg.treatment_wins += 1,
            "control" => agg.control_wins += 1,
            _ => agg.ties += 1,
        }
    }

    agg.finalize(n, c_first_count, t_first_count);
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
            agg.accumulate(c, &r.expected, false);
        }
        if let Some(t) = &r.treatment {
            agg.accumulate(t, &r.expected, true);
        }
    }

    agg.finalize(n, 0, 0);
    agg
}

// --- Report printing ---

fn print_e2e_report(dataset: &str, results: &[TaskResult], agg: &E2eAggregate, model: &str) {
    let paired: Vec<_> = results
        .iter()
        .filter(|r| r.control.is_some() && r.treatment.is_some())
        .collect();

    println!();
    println!("{}", "\u{2550}".repeat(66));
    println!(
        "End-to-End Eval: {dataset} ({} tasks, model: {model})",
        agg.count
    );
    println!("{}", "\u{2550}".repeat(66));

    if !paired.is_empty() {
        println!();
        println!(
            "  {:20} {:>10} {:>10} {:>10}",
            "Metric", "Control", "Treatment", "Delta"
        );
        println!("  {}", "\u{2500}".repeat(52));

        // Recall
        print_metric_row("Recall", agg.avg_control_recall, agg.avg_treatment_recall);
        // Precision
        print_metric_row(
            "Precision",
            agg.avg_control_precision,
            agg.avg_treatment_precision,
        );
        // Tool calls
        println!(
            "  {:20} {:>10.1} {:>10.1} {:>+9.0}%",
            "Tool calls",
            agg.avg_control_tool_calls,
            agg.avg_treatment_tool_calls,
            -agg.tool_call_reduction_pct,
        );
        // Tokens
        println!(
            "  {:20} {:>9.1}k {:>9.1}k {:>+9.0}%",
            "Tokens (total)",
            agg.avg_control_tokens / 1000.0,
            agg.avg_treatment_tokens / 1000.0,
            -agg.token_reduction_pct,
        );
        // Wall time
        println!(
            "  {:20} {:>9.1}s {:>9.1}s {:>+9.0}%",
            "Wall time",
            agg.avg_control_wall_time_ms / 1000.0,
            agg.avg_treatment_wall_time_ms / 1000.0,
            -agg.time_reduction_pct,
        );
        // Codemap calls
        println!(
            "  {:20} {:>10.1} {:>10.1} {:>10}",
            "Codemap calls", agg.avg_control_codemap_calls, agg.avg_treatment_codemap_calls, "n/a",
        );
        // First relevant
        if agg.avg_control_first_relevant > 0.0 || agg.avg_treatment_first_relevant > 0.0 {
            println!(
                "  {:20} {:>6.1} {:>6.1} {:>+9.0}%",
                "First relevant",
                agg.avg_control_first_relevant,
                agg.avg_treatment_first_relevant,
                -reduction_pct(
                    agg.avg_control_first_relevant,
                    agg.avg_treatment_first_relevant
                ),
            );
        }

        println!();
        println!(
            "  Win/Loss/Tie: Treatment {} / Control {} / Tie {}",
            agg.treatment_wins, agg.control_wins, agg.ties,
        );
    } else {
        // Single variant mode
        println!();
        for r in results {
            let m = r.control.as_ref().or(r.treatment.as_ref());
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

    println!("{}", "\u{2550}".repeat(66));
    println!();
}

fn print_metric_row(name: &str, control: f64, treatment: f64) {
    let delta_pct = if control > 0.0 {
        (treatment - control) / control * 100.0
    } else {
        0.0
    };
    println!(
        "  {:20} {:>10.2} {:>10.2} {:>+9.0}%",
        name, control, treatment, delta_pct,
    );
}
