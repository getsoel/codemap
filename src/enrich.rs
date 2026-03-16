/// Enrich command: manage LLM-generated file metadata.
use crate::{api, db};
use anyhow::Result;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct EnrichOpts<'a> {
    pub list: bool,
    pub set: Option<&'a str>,
    pub summary: Option<&'a str>,
    pub when_to_use: Option<&'a str>,
    pub clear: Option<&'a str>,
    pub clear_all: bool,
    pub stats: bool,
    pub api: bool,
    pub api_key: Option<&'a str>,
    pub provider: &'a str,
    pub model: Option<&'a str>,
    pub top: Option<usize>,
    pub force: bool,
    pub dry_run: bool,
    pub concurrency: usize,
    pub json: bool,
    pub if_available: bool,
}

pub fn run_enrich(root: &Path, opts: EnrichOpts<'_>) -> Result<()> {
    // Early exit: --api --if-available with no API key skips all work (including DB open)
    if opts.api
        && opts.if_available
        && api::resolve_provider(opts.provider, opts.api_key, opts.model).is_err()
    {
        return Ok(());
    }

    let conn = db::open_index(root)?;

    if opts.stats {
        return print_stats(&conn, opts.json);
    }

    if opts.clear_all {
        let count = db::clear_all_enrichments(&conn)?;
        eprintln!("codemap: cleared enrichment for {count} files");
        return Ok(());
    }

    if let Some(path) = opts.clear {
        db::clear_enrichment(&conn, path)?;
        eprintln!("codemap: cleared enrichment for {path}");
        return Ok(());
    }

    if let Some(path) = opts.set {
        let summary = opts
            .summary
            .ok_or_else(|| anyhow::anyhow!("--summary is required with --set"))?;
        let when = opts
            .when_to_use
            .ok_or_else(|| anyhow::anyhow!("--when-to-use is required with --set"))?;
        db::set_enrichment(&conn, path, summary, when)?;
        eprintln!("codemap: enriched {path}");
        return Ok(());
    }

    if opts.list {
        return print_list(&conn, opts.json);
    }

    if opts.api {
        return run_api_enrich(&conn, &opts);
    }

    // No action specified — show stats by default
    print_stats(&conn, opts.json)
}

fn print_stats(conn: &rusqlite::Connection, json: bool) -> Result<()> {
    let stats = db::get_enrichment_stats(conn)?;
    let unenriched = stats.total_files - stats.enriched_files;

    if json {
        let output = serde_json::json!({
            "total_files": stats.total_files,
            "enriched_files": stats.enriched_files,
            "unenriched_files": unenriched,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "Enrichment: {}/{} files ({:.0}%)",
            stats.enriched_files,
            stats.total_files,
            if stats.total_files > 0 {
                stats.enriched_files as f64 / stats.total_files as f64 * 100.0
            } else {
                0.0
            }
        );
        if unenriched > 0 {
            println!(
                "{unenriched} files need enrichment. Run `codemap enrich --api` or `codemap enrich --list`."
            );
        }
    }
    Ok(())
}

fn print_list(conn: &rusqlite::Connection, json: bool) -> Result<()> {
    let files = db::get_files_with_exports(conn, true)?;
    let total_files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;

    if json {
        let items: Vec<serde_json::Value> = files
            .iter()
            .map(|f| {
                serde_json::json!({
                    "path": f.path,
                    "rank": f.rank,
                    "exports": f.exports,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        println!(
            "Files missing enrichment ({} of {}):\n",
            files.len(),
            total_files
        );
        for f in &files {
            println!("{}", f.path);
            if !f.exports.is_empty() {
                let shown: Vec<&str> = f.exports.iter().map(|s| s.as_str()).take(5).collect();
                let suffix = if f.exports.len() > 5 {
                    format!(", ... +{}", f.exports.len() - 5)
                } else {
                    String::new()
                };
                println!("  exports: {}{}", shown.join(", "), suffix);
            }
        }
    }
    Ok(())
}

fn run_api_enrich(conn: &rusqlite::Connection, opts: &EnrichOpts<'_>) -> Result<()> {
    // If if_available was set, resolve_provider already succeeded in run_enrich's early check.
    // If not set, this propagates the error with the helpful message.
    let provider = api::resolve_provider(opts.provider, opts.api_key, opts.model)?;

    let mut files = db::get_files_with_exports(conn, !opts.force)?;

    // Limit by --top N
    if let Some(n) = opts.top {
        files.truncate(n);
    }

    if files.is_empty() {
        eprintln!("codemap: all files are already enriched");
        return Ok(());
    }

    // Dry run: estimate cost
    if opts.dry_run {
        let total_chars: usize = files.iter().map(estimate_prompt_chars).sum();
        let total_tokens_est = total_chars as f64 / 3.5;
        let output_tokens_est = files.len() as f64 * 150.0; // ~150 output tokens per file

        println!(
            "Dry run — {} provider, {} files",
            provider.name(),
            files.len()
        );
        println!("Estimated input tokens:  {:.0}", total_tokens_est);
        println!("Estimated output tokens: {:.0}", output_tokens_est);

        match provider.name() {
            "gemini" => {
                let cost = (total_tokens_est * 0.10 + output_tokens_est * 0.40) / 1_000_000.0;
                println!("Estimated cost: ${cost:.4} (Gemini Flash pricing)");
            }
            "anthropic" => {
                let cost = (total_tokens_est * 1.0 + output_tokens_est * 5.0) / 1_000_000.0;
                println!("Estimated cost: ${cost:.4} (Haiku pricing)");
            }
            _ => {}
        }

        return Ok(());
    }

    let total = files.len();
    let enriched_count = AtomicUsize::new(0);

    eprintln!(
        "codemap: enriching {} files via {} (concurrency: {})...",
        total,
        provider.name(),
        opts.concurrency
    );

    // Build the thread pool with bounded concurrency
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.concurrency)
        .build()?;

    // Collect results, then write to DB sequentially (rusqlite Connection isn't Send)
    let results: Vec<(String, Result<api::EnrichmentResult>)> = pool.install(|| {
        use rayon::prelude::*;
        files
            .par_iter()
            .map(|file| {
                let req = api::EnrichmentRequest {
                    file_path: file.path.clone(),
                    language: api::detect_language(&file.path).to_string(),
                    imports: Vec::new(),
                    exports: file.exports.clone(),
                };

                let result = provider.enrich(&req);

                let done = enriched_count.fetch_add(1, Ordering::Relaxed) + 1;
                if done.is_multiple_of(10) || done == total {
                    let pct = done * 100 / total;
                    eprintln!("codemap: enriched {done}/{total} files ({pct}%)...");
                }

                (file.path.clone(), result)
            })
            .collect()
    });

    // Write results to DB in a single transaction for performance
    let tx = conn.unchecked_transaction()?;
    let mut success = 0usize;
    let mut failed = 0usize;
    for (path, result) in results {
        match result {
            Ok(enrichment) => {
                db::set_enrichment(&tx, &path, &enrichment.summary, &enrichment.when_to_use)?;
                success += 1;
            }
            Err(e) => {
                tracing::warn!("failed to enrich {path}: {e}");
                failed += 1;
            }
        }
    }
    tx.commit()?;

    eprintln!("codemap: done — {success} enriched, {failed} failed");
    Ok(())
}

fn estimate_prompt_chars(file: &db::FileWithExports) -> usize {
    let base = 200; // prompt template overhead
    let path_chars = file.path.len();
    let export_chars: usize = file.exports.iter().map(|e| e.len() + 4).sum();
    base + path_chars + export_chars
}
