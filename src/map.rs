/// Code map generation: ranked file signatures for context injection.
use crate::{db, parser};
use anyhow::Result;
use std::path::Path;

pub fn run_map(root: &Path, tokens: usize, no_instructions: bool) -> Result<()> {
    let conn = db::open_index(root)?;

    let files = db::get_ranked_files(&conn, 500)?;
    if files.is_empty() {
        eprintln!("codemap: index is empty");
        return Ok(());
    }

    let output = generate_map(root, &conn, &files, tokens, !no_instructions)?;
    print!("{output}");
    Ok(())
}

/// Generate the map string.
pub fn generate_map(
    root: &Path,
    conn: &rusqlite::Connection,
    files: &[db::RankedFile],
    tokens: usize,
    with_instructions: bool,
) -> Result<String> {
    let char_budget = tokens * 4; // ~4 chars per token
    let mut output = String::new();
    let mut chars_used = 0usize;

    // Header with stats
    let (file_count, export_count, edge_count) = db::get_stats(conn)?;
    let header = format!(
        "## Codebase Map (codemap v{})\nIndexed {} files | {} exports | {} import edges\nTop files by structural importance (PageRank):\n\n",
        env!("CARGO_PKG_VERSION"),
        file_count,
        export_count,
        edge_count,
    );
    output.push_str(&header);
    chars_used += header.len();

    // Instructions footer (reserve space if enabled)
    let instructions = if with_instructions {
        "\n## Codemap Commands\n\
         Run these in Bash for deeper codebase queries:\n\
         - `codemap deps <file>` — imports and importers of a file\n\
         - `codemap symbol <name>` — find where a symbol is defined and who uses it\n\
         - `codemap context \"<task>\"` — suggest the most relevant files for a task\n\
         - `codemap map --tokens 3000` — regenerate this map with a larger budget\n"
    } else {
        ""
    };
    let reserved = instructions.len();

    // Load importer counts for per-file metadata
    let importer_counts = db::get_importer_counts(conn)?;

    for file in files {
        if chars_used + reserved >= char_budget {
            break;
        }

        let abs_path = root.join(&file.path);
        let source = match std::fs::read_to_string(&abs_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let signatures = parser::extract_signatures(&abs_path, &source);
        if signatures.is_empty() {
            continue;
        }

        let importers = importer_counts.get(&file.id).copied().unwrap_or(0);
        let mut entry = format!(
            "{} [rank: {:.2} | {} importers]\n",
            file.path, file.rank, importers
        );
        for sig in &signatures {
            entry.push_str(&format!("  {sig}\n"));
        }

        if let Ok(deps) = db::get_file_deps(conn, &file.path, "imports")
            && !deps.is_empty()
        {
            let shown: Vec<&str> = deps.iter().map(|d| d.file_path.as_str()).take(5).collect();
            entry.push_str(&format!("  → imports: {}\n", shown.join(", ")));
        }
        entry.push('\n');

        if chars_used + entry.len() + reserved > char_budget {
            break;
        }
        chars_used += entry.len();
        output.push_str(&entry);
    }

    if with_instructions {
        output.push_str(instructions);
    }

    Ok(output)
}
