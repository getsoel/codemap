/// A/B eval orchestration: compare Claude Code with and without codemap tools.
///
/// Runs each eval case as two sessions (control vs treatment), collects metrics,
/// and reports aggregate results showing codemap's impact on file discovery.
use crate::claude_client::{ClaudeClient, SessionConfig, SessionMetrics};
use crate::history;
use crate::tools;
use anyhow::{Context, Result};
use clap::ValueEnum;
use codemap::db;
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

const CONTROL_SYSTEM_PROMPT: &str = "\
You are a coding assistant. Your task is to explore a codebase and identify the files \
most relevant to a given task.

Use the available tools (grep, glob, read_file) to explore the codebase and find the \
files you would need to read or modify to complete the task.

When you are done exploring, list the files you identified as relevant and explain why.";

const TREATMENT_SYSTEM_PROMPT: &str = "\
You are a coding assistant. Your task is to explore a codebase and identify the files \
most relevant to a given task.

You have structural codebase tools available:
- codemap_context: find relevant files for a task (start here)
- codemap_symbol: find where a symbol is defined and referenced
- codemap_deps: show imports and importers of a file

You also have standard tools: grep, glob, read_file.

When you are done exploring, list the files you identified as relevant and explain why.";

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Variant {
    Both,
    Control,
    Treatment,
}

/// Result of running both variants for a single eval case.
#[derive(Debug)]
struct TaskResult {
    case_id: String,
    expected: HashSet<String>,
    control: Option<SessionMetrics>,
    treatment: Option<SessionMetrics>,
}

/// Aggregate metrics across all tasks, comparing control vs treatment.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AbAggregate {
    pub count: usize,
    pub avg_control_tool_calls: f64,
    pub avg_treatment_tool_calls: f64,
    pub tool_call_reduction_pct: f64,
    pub avg_control_files_read: f64,
    pub avg_treatment_files_read: f64,
    pub file_read_reduction_pct: f64,
    pub avg_control_recall_read: f64,
    pub avg_treatment_recall_read: f64,
    pub avg_control_recall_mentioned: f64,
    pub avg_treatment_recall_mentioned: f64,
    pub avg_control_tokens: f64,
    pub avg_treatment_tokens: f64,
    pub token_reduction_pct: f64,
    pub avg_control_first_relevant: f64,
    pub avg_treatment_first_relevant: f64,
    pub treatment_wins: usize,
    pub control_wins: usize,
    pub ties: usize,
}

/// Run the full A/B eval pipeline.
#[allow(clippy::too_many_arguments)]
pub fn run_ab_eval(
    dataset_path: &Path,
    repo_dir: &Path,
    model: &str,
    max_turns: usize,
    max_input_tokens: usize,
    cases_filter: Option<&str>,
    variant: Variant,
    no_archive: bool,
) -> Result<()> {
    let datasets = crate::load_datasets(dataset_path)?;
    let codemap_bin = crate::workspace::find_codemap_bin()?;
    let eval_dir = crate::find_eval_dir()?;

    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        anyhow::anyhow!(
            "ANTHROPIC_API_KEY not set. Required for A/B eval.\n\
             Get a key at https://console.anthropic.com/settings/keys"
        )
    })?;
    let client = ClaudeClient::new(api_key, model.to_string());

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
        // Copy fixture index.db into repo's .codemap/ directory
        let db_source = eval_dir.join(&ds.index_db);
        if !db_source.exists() {
            eprintln!(
                "Warning: fixture DB not found: {} (skipping {})",
                db_source.display(),
                ds.repo
            );
            continue;
        }
        let codemap_dir = repo_dir.join(".codemap");
        std::fs::create_dir_all(&codemap_dir)?;
        std::fs::copy(&db_source, codemap_dir.join("index.db"))?;

        // Load known files from the index
        let db_path = codemap_dir.join("index.db");
        let conn = db::init_db(
            db_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path"))?,
        )?;
        let files = db::get_all_files_with_exports_and_enrichment(&conn)?;
        let known_files: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();

        // Generate codemap map output for treatment system prompt
        let map_output = get_codemap_map(&codemap_bin, &repo_dir)?;

        let control_system = CONTROL_SYSTEM_PROMPT.to_string();
        let treatment_system = format!("{map_output}\n\n{TREATMENT_SYSTEM_PROMPT}");

        let control_tools = tools::control_tools();
        let treatment_tools = tools::treatment_tools();

        eprintln!(
            "\nA/B Eval: {} ({} cases, model: {model})",
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

            eprintln!("\n  {} - {}", case.id, case.query);

            let tool_exec = |name: &str, input: &serde_json::Value| -> String {
                tools::execute_tool(name, input, &repo_dir, &codemap_bin)
            };

            let control_metrics = if matches!(variant, Variant::Both | Variant::Control) {
                eprintln!("    Running control...");
                let config = SessionConfig {
                    system: &control_system,
                    tools: &control_tools,
                    max_turns,
                    max_input_tokens,
                };
                Some(client.run_session(
                    &config,
                    &case.query,
                    &tool_exec,
                    &expected,
                    &known_files,
                    "control",
                    &case.id,
                )?)
            } else {
                None
            };

            let treatment_metrics = if matches!(variant, Variant::Both | Variant::Treatment) {
                eprintln!("    Running treatment...");
                let config = SessionConfig {
                    system: &treatment_system,
                    tools: &treatment_tools,
                    max_turns,
                    max_input_tokens,
                };
                Some(client.run_session(
                    &config,
                    &case.query,
                    &tool_exec,
                    &expected,
                    &known_files,
                    "treatment",
                    &case.id,
                )?)
            } else {
                None
            };

            // Print per-case summary
            print_case_summary(&expected, &control_metrics, &treatment_metrics);

            results.push(TaskResult {
                case_id: case.id.clone(),
                expected,
                control: control_metrics,
                treatment: treatment_metrics,
            });
        }

        // Compute aggregate once, use for both report and archiving
        if !results.is_empty() {
            let agg = compute_aggregate(&results);
            print_ab_report(&ds.repo, &results, &agg);

            // Archive
            if let Some(ref hist_conn) = history_conn {
                let metrics_json = serde_json::to_value(&agg)?;
                let config_json = json!({
                    "model": model,
                    "max_turns": max_turns,
                    "max_input_tokens": max_input_tokens,
                    "variant": format!("{variant:?}"),
                });
                history::save_run(
                    hist_conn,
                    &git_commit,
                    git_dirty,
                    "ab_eval",
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

/// Run `codemap map --tokens 1500 --no-instructions` and capture output.
fn get_codemap_map(codemap_bin: &Path, repo_dir: &Path) -> Result<String> {
    let output = Command::new(codemap_bin)
        .current_dir(repo_dir)
        .args(["map", "--tokens", "1500", "--no-instructions"])
        .output()
        .context("failed to run codemap map")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("codemap map failed: {stderr}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Compute recall: fraction of expected files found in the discovered set.
fn recall(discovered: &HashSet<String>, expected: &HashSet<String>) -> f64 {
    if expected.is_empty() {
        return 0.0;
    }
    let hits = expected.iter().filter(|f| discovered.contains(*f)).count();
    hits as f64 / expected.len() as f64
}

/// Determine winner for a paired case using recall, then tool efficiency as tiebreaker.
fn case_winner(
    control: &SessionMetrics,
    treatment: &SessionMetrics,
    expected: &HashSet<String>,
) -> &'static str {
    let c_recall = recall(&control.files_mentioned, expected);
    let t_recall = recall(&treatment.files_mentioned, expected);
    if t_recall > c_recall + 0.01 {
        "treat"
    } else if c_recall > t_recall + 0.01 {
        "ctrl"
    } else if treatment.tool_calls < control.tool_calls {
        "treat"
    } else if control.tool_calls < treatment.tool_calls {
        "ctrl"
    } else {
        "tie"
    }
}

fn print_case_summary(
    expected: &HashSet<String>,
    control: &Option<SessionMetrics>,
    treatment: &Option<SessionMetrics>,
) {
    if let Some(c) = control {
        let recall_read = recall(&c.files_read, expected);
        let recall_mentioned = recall(&c.files_mentioned, expected);
        eprintln!(
            "    control:   {} tools, {} files read, recall(read)={:.0}% recall(mentioned)={:.0}%, {}+{} tokens",
            c.tool_calls,
            c.files_read.len(),
            recall_read * 100.0,
            recall_mentioned * 100.0,
            c.input_tokens,
            c.output_tokens,
        );
    }
    if let Some(t) = treatment {
        let recall_read = recall(&t.files_read, expected);
        let recall_mentioned = recall(&t.files_mentioned, expected);
        eprintln!(
            "    treatment: {} tools, {} files read, recall(read)={:.0}% recall(mentioned)={:.0}%, {}+{} tokens",
            t.tool_calls,
            t.files_read.len(),
            recall_read * 100.0,
            recall_mentioned * 100.0,
            t.input_tokens,
            t.output_tokens,
        );
    }
}

fn compute_aggregate(results: &[TaskResult]) -> AbAggregate {
    let paired: Vec<_> = results
        .iter()
        .filter(|r| r.control.is_some() && r.treatment.is_some())
        .collect();

    let n = paired.len();
    if n == 0 {
        // Single-variant run: compute what we can
        return compute_single_variant_aggregate(results);
    }

    let mut agg = AbAggregate {
        count: n,
        ..Default::default()
    };

    let mut control_first_count = 0usize;
    let mut treatment_first_count = 0usize;

    for r in &paired {
        let c = r.control.as_ref().unwrap();
        let t = r.treatment.as_ref().unwrap();

        agg.avg_control_tool_calls += c.tool_calls as f64;
        agg.avg_treatment_tool_calls += t.tool_calls as f64;
        agg.avg_control_files_read += c.files_read.len() as f64;
        agg.avg_treatment_files_read += t.files_read.len() as f64;
        agg.avg_control_tokens += (c.input_tokens + c.output_tokens) as f64;
        agg.avg_treatment_tokens += (t.input_tokens + t.output_tokens) as f64;

        // Recall: fraction of expected files that were read or mentioned
        agg.avg_control_recall_read += recall(&c.files_read, &r.expected);
        agg.avg_treatment_recall_read += recall(&t.files_read, &r.expected);
        agg.avg_control_recall_mentioned += recall(&c.files_mentioned, &r.expected);
        agg.avg_treatment_recall_mentioned += recall(&t.files_mentioned, &r.expected);

        if let Some(turn) = c.first_relevant_file_turn {
            agg.avg_control_first_relevant += turn as f64;
            control_first_count += 1;
        }
        if let Some(turn) = t.first_relevant_file_turn {
            agg.avg_treatment_first_relevant += turn as f64;
            treatment_first_count += 1;
        }

        match case_winner(c, t, &r.expected) {
            "treat" => agg.treatment_wins += 1,
            "ctrl" => agg.control_wins += 1,
            _ => agg.ties += 1,
        }
    }

    let nf = n as f64;
    agg.avg_control_tool_calls /= nf;
    agg.avg_treatment_tool_calls /= nf;
    agg.avg_control_files_read /= nf;
    agg.avg_treatment_files_read /= nf;
    agg.avg_control_tokens /= nf;
    agg.avg_treatment_tokens /= nf;
    agg.avg_control_recall_read /= nf;
    agg.avg_treatment_recall_read /= nf;
    agg.avg_control_recall_mentioned /= nf;
    agg.avg_treatment_recall_mentioned /= nf;

    if control_first_count > 0 {
        agg.avg_control_first_relevant /= control_first_count as f64;
    }
    if treatment_first_count > 0 {
        agg.avg_treatment_first_relevant /= treatment_first_count as f64;
    }

    agg.tool_call_reduction_pct =
        reduction_pct(agg.avg_control_tool_calls, agg.avg_treatment_tool_calls);
    agg.file_read_reduction_pct =
        reduction_pct(agg.avg_control_files_read, agg.avg_treatment_files_read);
    agg.token_reduction_pct = reduction_pct(agg.avg_control_tokens, agg.avg_treatment_tokens);

    agg
}

/// Compute aggregate when only one variant was run.
fn compute_single_variant_aggregate(results: &[TaskResult]) -> AbAggregate {
    let n = results.len();
    if n == 0 {
        return AbAggregate::default();
    }

    let mut agg = AbAggregate {
        count: n,
        ..Default::default()
    };

    for r in results {
        if let Some(c) = &r.control {
            agg.avg_control_tool_calls += c.tool_calls as f64;
            agg.avg_control_files_read += c.files_read.len() as f64;
            agg.avg_control_tokens += (c.input_tokens + c.output_tokens) as f64;
            agg.avg_control_recall_read += recall(&c.files_read, &r.expected);
            agg.avg_control_recall_mentioned += recall(&c.files_mentioned, &r.expected);
        }
        if let Some(t) = &r.treatment {
            agg.avg_treatment_tool_calls += t.tool_calls as f64;
            agg.avg_treatment_files_read += t.files_read.len() as f64;
            agg.avg_treatment_tokens += (t.input_tokens + t.output_tokens) as f64;
            agg.avg_treatment_recall_read += recall(&t.files_read, &r.expected);
            agg.avg_treatment_recall_mentioned += recall(&t.files_mentioned, &r.expected);
        }
    }

    let nf = n as f64;
    agg.avg_control_tool_calls /= nf;
    agg.avg_treatment_tool_calls /= nf;
    agg.avg_control_files_read /= nf;
    agg.avg_treatment_files_read /= nf;
    agg.avg_control_tokens /= nf;
    agg.avg_treatment_tokens /= nf;
    agg.avg_control_recall_read /= nf;
    agg.avg_treatment_recall_read /= nf;
    agg.avg_control_recall_mentioned /= nf;
    agg.avg_treatment_recall_mentioned /= nf;

    agg
}

fn reduction_pct(control: f64, treatment: f64) -> f64 {
    if control > 0.0 {
        (control - treatment) / control * 100.0
    } else {
        0.0
    }
}

fn print_ab_report(dataset: &str, results: &[TaskResult], agg: &AbAggregate) {
    let paired: Vec<_> = results
        .iter()
        .filter(|r| r.control.is_some() && r.treatment.is_some())
        .collect();

    println!("\n{}", "=".repeat(78));
    println!("A/B Eval Results: {dataset}");
    println!("{}", "=".repeat(78));

    // Per-case table
    if !paired.is_empty() {
        println!(
            "\n  {:12} {:>8} {:>8} {:>10} {:>10} {:>8}",
            "Case", "C.tools", "T.tools", "C.tokens", "T.tokens", "Winner"
        );
        println!("  {}", "-".repeat(64));

        for r in &paired {
            let c = r.control.as_ref().unwrap();
            let t = r.treatment.as_ref().unwrap();

            let c_total = c.input_tokens + c.output_tokens;
            let t_total = t.input_tokens + t.output_tokens;
            let winner = case_winner(c, t, &r.expected);

            println!(
                "  {:12} {:>8} {:>8} {:>9}k {:>9}k {:>8}",
                r.case_id,
                c.tool_calls,
                t.tool_calls,
                format!("{:.1}", c_total as f64 / 1000.0),
                format!("{:.1}", t_total as f64 / 1000.0),
                winner,
            );
        }
    } else {
        // Single variant
        for r in results {
            let m = r.control.as_ref().or(r.treatment.as_ref());
            if let Some(m) = m {
                let total = m.input_tokens + m.output_tokens;
                println!(
                    "  {:12} [{}] {} tools, {} files read, {:.1}k tokens",
                    r.case_id,
                    m.variant,
                    m.tool_calls,
                    m.files_read.len(),
                    total as f64 / 1000.0,
                );
            }
        }
    }

    // Aggregate
    if !paired.is_empty() {
        println!("\n  {}", "-".repeat(64));
        println!("  Overall ({} tasks):", agg.count);
        println!(
            "    Tool calls:     control avg {:.1}  ->  treatment avg {:.1}  ({:+.0}%)",
            agg.avg_control_tool_calls, agg.avg_treatment_tool_calls, -agg.tool_call_reduction_pct,
        );
        println!(
            "    Files read:     control avg {:.1}  ->  treatment avg {:.1}  ({:+.0}%)",
            agg.avg_control_files_read, agg.avg_treatment_files_read, -agg.file_read_reduction_pct,
        );
        println!(
            "    Recall (read):  control avg {:.2} ->  treatment avg {:.2}",
            agg.avg_control_recall_read, agg.avg_treatment_recall_read,
        );
        println!(
            "    Recall (ment.): control avg {:.2} ->  treatment avg {:.2}",
            agg.avg_control_recall_mentioned, agg.avg_treatment_recall_mentioned,
        );
        println!(
            "    Tokens:         control avg {:.1}k ->  treatment avg {:.1}k ({:+.0}%)",
            agg.avg_control_tokens / 1000.0,
            agg.avg_treatment_tokens / 1000.0,
            -agg.token_reduction_pct,
        );
        if agg.avg_control_first_relevant > 0.0 || agg.avg_treatment_first_relevant > 0.0 {
            println!(
                "    First relevant: control turn {:.1} ->  treatment turn {:.1}",
                agg.avg_control_first_relevant, agg.avg_treatment_first_relevant,
            );
        }
        println!(
            "    Win/Loss/Tie:   Treatment wins {}, Control wins {}, Ties {}",
            agg.treatment_wins, agg.control_wins, agg.ties,
        );
    }

    println!("{}", "=".repeat(78));
    println!();
}
