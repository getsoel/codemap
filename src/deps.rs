/// Dependency inspection command: show imports and importers of a file.
use crate::db;
use anyhow::Result;
use std::collections::{HashSet, VecDeque};
use std::path::Path;

struct DepEntry {
    file_path: String,
    edge_type: String,
    specifier: Option<String>,
    depth: usize,
}

pub fn run_deps(
    root: &Path,
    file: &str,
    direction: &str,
    depth: usize,
    all: bool,
    json: bool,
) -> Result<()> {
    let conn = db::open_index(root)?;

    // Normalize the input file path
    let rel_path = normalize_path(root, file);

    if db::get_file_id(&conn, &rel_path).is_none() {
        anyhow::bail!("File not found in index: {file}. Run `codemap index` first.");
    }

    let directions: Vec<&str> = match direction {
        "imports" => vec!["imports"],
        "importers" => vec!["importers"],
        _ => vec!["imports", "importers"],
    };

    let mut imports: Vec<DepEntry> = Vec::new();
    let mut importers: Vec<DepEntry> = Vec::new();

    for dir in &directions {
        let entries = gather_deps(&conn, &rel_path, dir, depth)?;
        match *dir {
            "imports" => imports = entries,
            "importers" => importers = entries,
            _ => {}
        }
    }

    if json {
        print_json(&rel_path, &imports, &importers, all)?;
    } else {
        print_text(&rel_path, &imports, &importers, all)?;
    }

    Ok(())
}

fn normalize_path(root: &Path, file: &str) -> String {
    let normalized = file.replace('\\', "/");
    let path = Path::new(&normalized);

    if path.is_absolute()
        && let Ok(stripped) = path.strip_prefix(root)
    {
        return stripped.to_string_lossy().replace('\\', "/");
    }

    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_string()
}

fn gather_deps(
    conn: &rusqlite::Connection,
    file_path: &str,
    direction: &str,
    max_depth: usize,
) -> Result<Vec<DepEntry>> {
    if max_depth == 0 {
        return Ok(Vec::new());
    }

    let mut results: Vec<DepEntry> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(file_path.to_string());

    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((file_path.to_string(), 0));

    while let Some((current, current_depth)) = queue.pop_front() {
        if current_depth >= max_depth {
            continue;
        }

        let deps = db::get_file_deps(conn, &current, direction)?;
        for dep in deps {
            let is_new = visited.insert(dep.file_path.clone());
            results.push(DepEntry {
                file_path: dep.file_path.clone(),
                edge_type: dep.edge_type,
                specifier: dep.specifier,
                depth: current_depth + 1,
            });
            if is_new && current_depth + 1 < max_depth {
                queue.push_back((dep.file_path, current_depth + 1));
            }
        }
    }

    Ok(results)
}

fn dep_to_json(entry: &DepEntry) -> serde_json::Value {
    serde_json::json!({
        "file": entry.file_path,
        "edge_type": entry.edge_type,
        "specifier": entry.specifier,
        "depth": entry.depth,
    })
}

fn format_depth_prefix(depth: usize) -> String {
    if depth > 1 {
        format!("(depth {depth}) ")
    } else {
        String::new()
    }
}

/// Print a truncated list with "... and N more" hint.
fn print_truncated_list(items: &[DepEntry], all: bool, max_show: usize) {
    let total = items.len();
    let show = if all { total } else { total.min(max_show) };
    for entry in &items[..show] {
        println!("  {}{}", format_depth_prefix(entry.depth), entry.file_path);
    }
    if !all && total > max_show {
        println!(
            "  ... and {} more (use --all to show all)",
            total - max_show
        );
    }
}

fn print_text(file: &str, imports: &[DepEntry], importers: &[DepEntry], all: bool) -> Result<()> {
    println!("{file}");

    if !imports.is_empty() {
        println!();
        println!("Imports ({}):", imports.len());
        for entry in imports {
            println!(
                "  {}{}  [{}]",
                format_depth_prefix(entry.depth),
                entry.file_path,
                entry.edge_type
            );
        }
    }

    if !importers.is_empty() {
        println!();
        println!("Imported by ({}):", importers.len());
        print_truncated_list(importers, all, 5);
    }

    if imports.is_empty() && importers.is_empty() {
        println!();
        println!("No dependencies found.");
    }

    Ok(())
}

fn print_json(file: &str, imports: &[DepEntry], importers: &[DepEntry], all: bool) -> Result<()> {
    let imports_json: Vec<serde_json::Value> = imports.iter().map(dep_to_json).collect();

    let total_importers = importers.len();
    let show = if all {
        total_importers
    } else {
        total_importers.min(5)
    };
    let shown_importers: Vec<serde_json::Value> =
        importers[..show].iter().map(dep_to_json).collect();

    let output = serde_json::json!({
        "file": file,
        "imports": imports_json,
        "importers": shown_importers,
        "total_importers": total_importers,
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
