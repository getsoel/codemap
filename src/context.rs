/// Context command: suggest the most relevant files for a task description.
use crate::{db, scorer};
use anyhow::Result;
use std::path::Path;

pub fn run_context(
    root: &Path,
    query: &str,
    limit: usize,
    json: bool,
    include_content: bool,
) -> Result<()> {
    let conn = db::open_index(root)?;

    let files = db::get_all_files_with_exports_and_enrichment(&conn)?;
    let keywords = scorer::tokenize_query(query);

    if keywords.is_empty() {
        anyhow::bail!(
            "No searchable keywords in query after stop-word removal. Try a more specific description."
        );
    }

    let scored = scorer::score_files(&keywords, &files, &conn);
    let top: Vec<&scorer::ScoredFile> = scored.iter().take(limit).collect();

    if top.is_empty() {
        if json {
            println!("[]");
        } else {
            eprintln!("No relevant files found for: \"{query}\"");
        }
        return Ok(());
    }

    if json {
        print_json(root, query, &top, include_content)?;
    } else {
        print_text(root, query, &top, include_content)?;
    }

    Ok(())
}

fn print_text(
    root: &Path,
    query: &str,
    files: &[&scorer::ScoredFile],
    include_content: bool,
) -> Result<()> {
    println!("Relevant files for: \"{query}\"");
    println!();

    let max_path_len = files.iter().map(|f| f.path.len()).max().unwrap_or(0);

    for sf in files {
        let reasons = sf.match_reasons.join(", ");
        let padding = " ".repeat(max_path_len.saturating_sub(sf.path.len()));
        println!(
            "  {}{}  [rank: {:.2} | match: {}]",
            sf.path, padding, sf.rank, reasons
        );
    }

    if include_content {
        println!();
        for sf in files {
            let abs_path = root.join(&sf.path);
            println!("--- {} ---", sf.path);
            match std::fs::read_to_string(&abs_path) {
                Ok(content) => println!("{content}"),
                Err(e) => println!("(could not read: {e})"),
            }
            println!();
        }
    }

    Ok(())
}

fn print_json(
    root: &Path,
    query: &str,
    files: &[&scorer::ScoredFile],
    include_content: bool,
) -> Result<()> {
    let results: Vec<serde_json::Value> = files
        .iter()
        .map(|sf| {
            let mut obj = serde_json::json!({
                "path": sf.path,
                "score": sf.score,
                "rank": sf.rank,
                "match_reasons": sf.match_reasons,
            });
            if include_content {
                let abs_path = root.join(&sf.path);
                let content = std::fs::read_to_string(&abs_path).unwrap_or_default();
                obj["content"] = serde_json::Value::String(content);
            }
            obj
        })
        .collect();

    let output = serde_json::json!({
        "query": query,
        "results": results,
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
