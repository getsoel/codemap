/// Symbol lookup command: find definitions and references.
use crate::{db, parser};
use anyhow::Result;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

pub fn run_symbol(
    root: &Path,
    pattern: &str,
    limit: usize,
    all: bool,
    exact: bool,
    json: bool,
) -> Result<()> {
    let conn = db::open_index(root)?;

    let results = db::query_symbols(&conn, pattern, limit, exact)?;

    if results.is_empty() {
        if json {
            println!("[]");
        } else {
            eprintln!("No symbols matching '{pattern}'");
        }
        return Ok(());
    }

    // Group results by symbol name
    let mut grouped: BTreeMap<String, Vec<&db::SymbolResult>> = BTreeMap::new();
    for result in &results {
        grouped.entry(result.name.clone()).or_default().push(result);
    }

    if json {
        print_json(root, &conn, &grouped, all)?;
    } else {
        print_text(root, &conn, &grouped, all)?;
    }

    Ok(())
}

/// For each definition, find the signature and collect importers (deduplicated).
struct SymbolDetail {
    defs: Vec<DefInfo>,
    importers: Vec<String>,
}

struct DefInfo {
    file_path: String,
    line: Option<i32>,
    kind: String,
    is_exported: bool,
    signature: Option<String>,
}

fn gather_symbol_details(
    root: &Path,
    conn: &rusqlite::Connection,
    defs: &[&db::SymbolResult],
    name: &str,
) -> SymbolDetail {
    let mut def_infos = Vec::new();
    let mut all_importers = Vec::new();
    let mut seen = HashSet::new();

    for def in defs {
        // Extract signature from source
        let abs_path = root.join(&def.file_path);
        let signature = std::fs::read_to_string(&abs_path).ok().and_then(|source| {
            let sigs = parser::extract_signatures(&abs_path, &source);
            sigs.into_iter().find(|s| s.contains(name))
        });

        def_infos.push(DefInfo {
            file_path: def.file_path.clone(),
            line: def.line,
            kind: def.kind.clone(),
            is_exported: def.is_exported,
            signature,
        });

        // Gather importers of this file
        if let Ok(deps) = db::get_file_deps(conn, &def.file_path, "importers") {
            for dep in deps {
                if seen.insert(dep.file_path.clone()) {
                    all_importers.push(dep.file_path);
                }
            }
        }
    }

    SymbolDetail {
        defs: def_infos,
        importers: all_importers,
    }
}

fn print_text(
    root: &Path,
    conn: &rusqlite::Connection,
    grouped: &BTreeMap<String, Vec<&db::SymbolResult>>,
    all: bool,
) -> Result<()> {
    let mut first = true;
    for (name, defs) in grouped {
        if !first {
            println!();
        }
        first = false;

        let detail = gather_symbol_details(root, conn, defs, name);

        println!("{name}");
        println!("  Defined in:");
        for def in &detail.defs {
            let exported = if def.is_exported { "exported" } else { "local" };
            let line_info = def.line.map(|l| format!(":{l}")).unwrap_or_default();
            println!(
                "    {}{line_info}  [{exported} {}]",
                def.file_path, def.kind
            );
            if let Some(sig) = &def.signature {
                println!("      {sig}");
            }
        }

        let total = detail.importers.len();
        if total > 0 {
            println!("  Referenced in ({total} files):");
            let show = if all { total } else { total.min(5) };
            for imp in &detail.importers[..show] {
                println!("    {imp}");
            }
            if !all && total > 5 {
                println!("    ... and {} more (use --all to show all)", total - 5);
            }
        }
    }

    Ok(())
}

fn print_json(
    root: &Path,
    conn: &rusqlite::Connection,
    grouped: &BTreeMap<String, Vec<&db::SymbolResult>>,
    all: bool,
) -> Result<()> {
    let mut symbols = Vec::new();

    for (name, defs) in grouped {
        let detail = gather_symbol_details(root, conn, defs, name);

        let definitions: Vec<serde_json::Value> = detail
            .defs
            .iter()
            .map(|def| {
                serde_json::json!({
                    "file": def.file_path,
                    "line": def.line,
                    "kind": def.kind,
                    "exported": def.is_exported,
                    "signature": def.signature,
                })
            })
            .collect();

        let total = detail.importers.len();
        let shown: Vec<&str> = if all {
            detail.importers.iter().map(|s| s.as_str()).collect()
        } else {
            detail
                .importers
                .iter()
                .take(5)
                .map(|s| s.as_str())
                .collect()
        };

        symbols.push(serde_json::json!({
            "name": name,
            "definitions": definitions,
            "referenced_in": shown,
            "total_references": total,
        }));
    }

    println!("{}", serde_json::to_string_pretty(&symbols)?);
    Ok(())
}
