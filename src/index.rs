/// Index command: discover, parse, resolve, graph, rank, persist.
use crate::{db, graph, hash, parser, resolver, types, walk};
use anyhow::Result;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Convert an absolute path to a relative path string under `root`.
fn to_relative(abs: &str, root: &Path) -> String {
    let p = Path::new(abs);
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .to_string()
}

/// Get file mtime as seconds since epoch, or None on error.
fn file_mtime(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

pub fn run_index(root: &Path, force: bool, incremental: bool) -> Result<()> {
    // Ensure .codemap directory exists
    let codemap_dir = root.join(".codemap");
    std::fs::create_dir_all(&codemap_dir)?;

    // Warn if .codemap/ isn't gitignored
    warn_if_not_gitignored(root);
    let db_path = codemap_dir.join("index.db");
    let db_path_str = db_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 database path"))?;
    let conn = db::init_db(db_path_str)?;

    // Step 1: Discover files
    let files = walk::discover_files(root);
    eprintln!("codemap: found {} files", files.len());

    if files.is_empty() {
        eprintln!("codemap: no JS/TS files found");
        return Ok(());
    }

    // Step 2: Read files, hash, and check for changes
    let mut changed_files: Vec<(String, String, String, String)> = Vec::new(); // (rel_path, abs_path, hash, source)
    let mut all_paths: Vec<String> = Vec::new();
    let mut skipped = 0usize;

    for file_path in &files {
        let rel_path = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();
        let abs_path = file_path.to_string_lossy().to_string();
        all_paths.push(rel_path.clone());

        // Cache mtime once (avoid redundant stat calls)
        let fs_mtime = file_mtime(file_path);

        // Incremental: skip files whose mtime hasn't changed
        if incremental
            && !force
            && let (Some(db_mt), Some(fs_mt)) = (db::get_file_mtime(&conn, &rel_path), fs_mtime)
            && fs_mt <= db_mt
        {
            skipped += 1;
            continue;
        }

        let source = match std::fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to read {}: {}", abs_path, e);
                continue;
            }
        };
        let file_hash = hash::hash_bytes(source.as_bytes());

        // Hash-based skip (non-incremental mode, or incremental with newer mtime)
        if !force
            && let Some(existing_hash) = db::get_file_hash(&conn, &rel_path)
            && existing_hash == file_hash
        {
            // Mtime changed but content didn't — update mtime only
            if let (true, Some(fs_mt)) = (incremental, fs_mtime) {
                let _ = db::update_file_mtime(&conn, &rel_path, fs_mt);
            }
            skipped += 1;
            continue;
        }
        changed_files.push((rel_path, abs_path, file_hash, source));
    }

    let all_paths_set: HashSet<&str> = all_paths.iter().map(|s| s.as_str()).collect();

    eprintln!(
        "codemap: {} changed, {} unchanged",
        changed_files.len(),
        skipped
    );

    // Early exit for incremental with no changes
    if incremental && changed_files.is_empty() {
        // Still delete stale files
        let deleted = db::delete_stale_files(&conn, &all_paths)?;
        if deleted > 0 {
            eprintln!("codemap: removed {deleted} stale files from index");
        }
        eprintln!("codemap: up to date (0 changes)");
        return Ok(());
    }

    // Delete stale files no longer on disk
    let deleted = db::delete_stale_files(&conn, &all_paths)?;
    if deleted > 0 {
        eprintln!("codemap: removed {deleted} stale files from index");
    }

    // Step 3: Parse changed files in parallel, then build graph sequentially
    let import_resolver = resolver::create_resolver();

    struct FileData {
        analysis: types::FileAnalysis,
        /// (resolved_rel_path, edge_kind, specifier)
        resolved_edges: Vec<(String, graph::EdgeKind, String)>,
    }

    // Parallel: parse + resolve imports (CPU-heavy work)
    let parse_results: Vec<Option<(String, FileData)>> = changed_files
        .par_iter()
        .map(|(rel_path, abs_path, _hash, source)| {
            let analysis = match parser::analyze_file(Path::new(abs_path), source) {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {}", rel_path, e);
                    return None;
                }
            };

            let from_dir = Path::new(abs_path).parent().unwrap_or(Path::new("."));
            let mut resolved_edges = Vec::new();

            for import in &analysis.imports {
                if let Some(resolved) =
                    resolver::resolve_import(&import_resolver, from_dir, &import.source)
                {
                    let resolved_rel = to_relative(&resolved, root);
                    if all_paths_set.contains(resolved_rel.as_str()) {
                        let kind = match import.kind {
                            types::ImportKind::Namespace => graph::EdgeKind::TypeImport,
                            _ => graph::EdgeKind::Import,
                        };
                        resolved_edges.push((resolved_rel, kind, import.source.clone()));
                    }
                }
            }

            for reexport in &analysis.reexports {
                if let Some(resolved) =
                    resolver::resolve_import(&import_resolver, from_dir, &reexport.source)
                {
                    let resolved_rel = to_relative(&resolved, root);
                    if all_paths_set.contains(resolved_rel.as_str()) {
                        resolved_edges.push((
                            resolved_rel,
                            graph::EdgeKind::ReExport,
                            reexport.source.clone(),
                        ));
                    }
                }
            }

            Some((
                rel_path.clone(),
                FileData {
                    analysis,
                    resolved_edges,
                },
            ))
        })
        .collect();

    // Sequential: build graph from parallel results
    let parse_errors = parse_results.iter().filter(|r| r.is_none()).count();
    let mut dep_graph = graph::DependencyGraph::new();
    let mut file_data: HashMap<String, FileData> = HashMap::new();

    for path in &all_paths {
        dep_graph.add_file(path);
    }

    for (rel_path, data) in parse_results.into_iter().flatten() {
        for (resolved_rel, kind, _specifier) in &data.resolved_edges {
            dep_graph.add_edge(&rel_path, resolved_rel, *kind);
        }
        file_data.insert(rel_path, data);
    }

    if parse_errors > 0 {
        eprintln!("codemap: {parse_errors} parse errors");
    }

    // Step 4: Compute PageRank
    let ranks = dep_graph.compute_ranks();
    let rank_map: HashMap<&str, f64> = ranks.iter().map(|(p, r)| (p.as_str(), *r)).collect();

    // Step 5: Persist everything in a transaction
    let tx = conn.unchecked_transaction()?;

    // Upsert all changed files and collect file IDs for edge insertion
    let mut file_id_cache: HashMap<String, i64> = HashMap::new();

    for (rel_path, abs_path, file_hash, _source) in &changed_files {
        let rank = rank_map.get(rel_path.as_str()).copied().unwrap_or(0.0);
        let file_id = db::upsert_file(&tx, rel_path, file_hash, rank)?;
        file_id_cache.insert(rel_path.clone(), file_id);

        // Store mtime (stat once; file was already read so metadata should be cached by OS)
        if let Some(mt) = file_mtime(Path::new(abs_path)) {
            db::update_file_mtime(&tx, rel_path, mt)?;
        }

        if let Some(data) = file_data.get(rel_path) {
            // Insert symbols
            let symbols: Vec<(String, String, bool, Option<i32>, usize)> = data
                .analysis
                .symbols
                .iter()
                .map(|s| {
                    let kind = if s.is_exported {
                        data.analysis
                            .exports
                            .iter()
                            .find(|e| e.name == s.name)
                            .map(|e| format!("{:?}", e.kind))
                            .unwrap_or("Variable".to_string())
                    } else {
                        "Variable".to_string()
                    };
                    (s.name.clone(), kind, s.is_exported, None, s.reference_count)
                })
                .collect();
            db::insert_symbols(&tx, file_id, &symbols)?;
        }
    }

    // Bulk-load all file IDs we don't have yet (for unchanged files that are edge targets)
    {
        let mut stmt = tx.prepare("SELECT id, path FROM files")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(0)?))
        })?;
        for row in rows {
            let (path, id) = row?;
            file_id_cache.entry(path).or_insert(id);
        }
    }

    // Insert edges using cached resolved data and file IDs
    for (rel_path, _abs_path, _hash, _source) in &changed_files {
        if let (Some(source_file_id), Some(data)) =
            (file_id_cache.get(rel_path), file_data.get(rel_path))
        {
            let edges: Vec<(i64, String, Option<String>)> = data
                .resolved_edges
                .iter()
                .filter_map(|(resolved_rel, kind, specifier)| {
                    file_id_cache
                        .get(resolved_rel)
                        .map(|&target_id| (target_id, format!("{kind:?}"), Some(specifier.clone())))
                })
                .collect();
            db::insert_edges(&tx, *source_file_id, &edges)?;
        }
    }

    // Update ranks for all files (including unchanged ones)
    db::update_ranks(&tx, &ranks)?;

    tx.commit()?;

    eprintln!(
        "codemap: indexed {} files ({} parsed, {} skipped)",
        all_paths.len(),
        changed_files.len() - parse_errors,
        skipped
    );

    Ok(())
}

/// Check if `.codemap/` is covered by the project's `.gitignore`. Warn once if not.
fn warn_if_not_gitignored(root: &Path) {
    let gitignore_path = root.join(".gitignore");
    let contents = match std::fs::read_to_string(&gitignore_path) {
        Ok(s) => s,
        Err(_) => {
            // No .gitignore at all — warn
            eprintln!(
                "codemap: warning: .codemap/ is not in .gitignore — \
                 consider adding it to avoid committing the index database"
            );
            return;
        }
    };

    let dominated = contents.lines().any(|line| {
        let trimmed = line.trim();
        trimmed == ".codemap"
            || trimmed == ".codemap/"
            || trimmed == "/.codemap"
            || trimmed == "/.codemap/"
    });

    if !dominated {
        eprintln!(
            "codemap: warning: .codemap/ is not in .gitignore — \
             consider adding it to avoid committing the index database"
        );
    }
}
