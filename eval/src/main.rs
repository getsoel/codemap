/// codemap-eval: evaluate scorer relevance quality and track results over time.
mod history;
mod metrics;
mod report;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use codemap::{db, scorer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "codemap-eval", about = "Evaluate codemap scorer quality")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run eval against a dataset and archive results
    Run {
        /// Path to dataset JSON file (or directory of datasets)
        #[arg(short, long)]
        dataset: PathBuf,

        /// Output format: table, json
        #[arg(short, long, default_value = "table")]
        format: String,

        /// Skip archiving results to history.db
        #[arg(long)]
        no_archive: bool,
    },

    /// Compare current results against a previous run
    Compare {
        /// Path to dataset JSON file
        #[arg(short, long)]
        dataset: PathBuf,

        /// Git commit (prefix) to compare against. Defaults to most recent archived run.
        #[arg(long)]
        against: Option<String>,
    },

    /// List archived runs
    History {
        /// Filter by dataset name
        #[arg(short, long)]
        dataset: Option<String>,

        /// Max results
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
}

#[derive(serde::Deserialize)]
struct EvalDataset {
    repo: String,
    #[serde(default = "default_language")]
    language: String,
    #[allow(dead_code)]
    #[serde(default)]
    commit: String,
    index_db: String,
    cases: Vec<EvalCase>,
}

fn default_language() -> String {
    "js/ts".to_string()
}

#[derive(serde::Deserialize)]
struct EvalCase {
    id: String,
    query: String,
    expected_files: Vec<ExpectedFile>,
}

#[derive(serde::Deserialize)]
struct ExpectedFile {
    path: String,
    relevance: u8,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            dataset,
            format,
            no_archive,
        } => run_eval(&dataset, &format, no_archive),
        Commands::Compare { dataset, against } => run_compare(&dataset, against.as_deref()),
        Commands::History { dataset, limit } => run_history(dataset.as_deref(), limit),
    }
}

/// Score all cases in a dataset against the given DB, returning per-case metrics.
fn score_dataset(
    cases: &[EvalCase],
    files: &[db::FileWithExportsAndEnrichment],
    conn: &rusqlite::Connection,
) -> Vec<metrics::CaseMetrics> {
    let mut results = Vec::new();
    for case in cases {
        let keywords = scorer::tokenize_query(&case.query);
        if keywords.is_empty() {
            continue;
        }

        let scored = scorer::score_files(&keywords, files, conn);
        let returned: Vec<String> = scored.into_iter().map(|s| s.path).collect();
        let relevance_map: HashMap<String, u8> = case
            .expected_files
            .iter()
            .map(|f| (f.path.clone(), f.relevance))
            .collect();

        results.push(metrics::CaseMetrics::compute(
            &case.id,
            &case.query,
            &returned,
            &relevance_map,
        ));
    }
    results
}

/// Open a fixture DB and load files for scoring.
fn open_fixture(
    eval_dir: &Path,
    index_db: &str,
) -> Result<Option<(rusqlite::Connection, Vec<db::FileWithExportsAndEnrichment>)>> {
    let db_path = eval_dir.join(index_db);
    if !db_path.exists() {
        return Ok(None);
    }
    let db_str = db_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path"))?;
    let conn = db::init_db(db_str)?;
    let files = db::get_all_files_with_exports_and_enrichment(&conn)?;
    Ok(Some((conn, files)))
}

fn run_eval(dataset_path: &Path, format: &str, no_archive: bool) -> Result<()> {
    let datasets = load_datasets(dataset_path)?;

    let eval_dir = find_eval_dir()?;
    let (git_commit, git_dirty) = history::get_git_info();

    let history_conn = if !no_archive {
        Some(history::open_history(&eval_dir)?)
    } else {
        None
    };

    for ds in &datasets {
        let Some((conn, files)) = open_fixture(&eval_dir, &ds.index_db)? else {
            eprintln!(
                "Warning: fixture DB not found: {} (skipping {})",
                eval_dir.join(&ds.index_db).display(),
                ds.repo
            );
            continue;
        };

        let case_metrics = score_dataset(&ds.cases, &files, &conn);
        let agg = metrics::aggregate(&case_metrics);

        match format {
            "json" => report::print_json(&ds.repo, &ds.language, &case_metrics, &agg, &git_commit),
            _ => report::print_table(&ds.repo, &ds.language, &case_metrics, &agg),
        }

        // Archive results
        if let Some(ref hist_conn) = history_conn {
            let metrics_json = serde_json::to_value(&agg)?;
            history::save_run(
                hist_conn,
                &git_commit,
                git_dirty,
                "relevance",
                &ds.repo,
                &ds.language,
                &metrics_json,
                None,
            )?;
            let dirty_marker = if git_dirty { " (dirty)" } else { "" };
            eprintln!("Archived: {} @ {}{dirty_marker}", ds.repo, git_commit);
        }
    }

    Ok(())
}

fn run_compare(dataset_path: &Path, against: Option<&str>) -> Result<()> {
    let datasets = load_datasets(dataset_path)?;
    let eval_dir = find_eval_dir()?;
    let history_conn = history::open_history(&eval_dir)?;

    for ds in &datasets {
        let Some((conn, files)) = open_fixture(&eval_dir, &ds.index_db)? else {
            eprintln!("Warning: fixture DB not found (skipping {})", ds.repo);
            continue;
        };

        let baseline = history::get_run(&history_conn, &ds.repo, against)?;
        let Some(baseline) = baseline else {
            eprintln!(
                "No previous run found for {} (run `codemap-eval run` first)",
                ds.repo
            );
            continue;
        };

        let case_metrics = score_dataset(&ds.cases, &files, &conn);
        let agg = metrics::aggregate(&case_metrics);
        report::print_comparison(&ds.repo, &agg, &baseline);
    }

    Ok(())
}

fn run_history(dataset: Option<&str>, limit: usize) -> Result<()> {
    let eval_dir = find_eval_dir()?;
    let conn = history::open_history(&eval_dir)?;
    let runs = history::list_runs(&conn, dataset, limit)?;

    if runs.is_empty() {
        println!("No archived runs found.");
        return Ok(());
    }

    println!(
        "  {:>4}  {:>8}  {:>10}  {:>12}  {:>6}  {:>6}  {:>6}  {:>8}",
        "ID", "Commit", "Date", "Dataset", "P@10", "R@10", "MRR", "NDCG@10"
    );
    println!("{}", "─".repeat(78));

    for run in &runs {
        let date = if run.timestamp.len() >= 10 {
            &run.timestamp[..10]
        } else {
            &run.timestamp
        };
        let dirty = if run.git_dirty { "*" } else { "" };
        println!(
            "  {:>4}  {:>7}{:<1}  {:>10}  {:>12}  {:>5.2}  {:>5.2}  {:>5.2}  {:>8.2}",
            run.id,
            run.git_commit,
            dirty,
            date,
            run.dataset,
            history::json_f64(&run.metrics, "precision_at_10"),
            history::json_f64(&run.metrics, "recall_at_10"),
            history::json_f64(&run.metrics, "mrr"),
            history::json_f64(&run.metrics, "ndcg_at_10"),
        );
    }
    println!();

    Ok(())
}

fn load_datasets(path: &Path) -> Result<Vec<EvalDataset>> {
    if path.is_dir() {
        let mut datasets = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(path)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let content = std::fs::read_to_string(entry.path())?;
            let ds: EvalDataset = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse {}", entry.path().display()))?;
            datasets.push(ds);
        }
        Ok(datasets)
    } else {
        let content = std::fs::read_to_string(path)?;
        let ds: EvalDataset = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(vec![ds])
    }
}

fn find_eval_dir() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        let eval_dir = dir.join("eval");
        if eval_dir.is_dir() {
            return Ok(eval_dir);
        }
        if !dir.pop() {
            anyhow::bail!("Could not find eval/ directory. Run from the codemap project root.");
        }
    }
}
