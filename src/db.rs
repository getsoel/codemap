/// SQLite storage layer for codemap.
use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::Path;

pub fn init_db(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS files (
            id         INTEGER PRIMARY KEY,
            path       TEXT NOT NULL UNIQUE,
            hash       TEXT NOT NULL,
            rank       REAL NOT NULL DEFAULT 0.0,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE TABLE IF NOT EXISTS symbols (
            id          INTEGER PRIMARY KEY,
            file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            name        TEXT NOT NULL,
            kind        TEXT NOT NULL,
            is_exported INTEGER NOT NULL DEFAULT 0,
            line        INTEGER,
            ref_count   INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS edges (
            id          INTEGER PRIMARY KEY,
            source_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            target_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            edge_type   TEXT NOT NULL,
            specifier   TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_files_path ON files(path);
        CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_id);
        CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
        CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
        CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);
    ",
    )?;

    // Migrate: add mtime column if absent (v0.2)
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(files)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();

    if !columns.iter().any(|n| n == "mtime") {
        conn.execute_batch("ALTER TABLE files ADD COLUMN mtime INTEGER;")?;
    }

    // Migrate: add enrichment columns if absent (v0.3)
    if !columns.iter().any(|n| n == "summary_enriched") {
        conn.execute_batch(
            "ALTER TABLE files ADD COLUMN summary_enriched TEXT;
             ALTER TABLE files ADD COLUMN when_to_use_enriched TEXT;
             ALTER TABLE files ADD COLUMN enriched_at INTEGER;",
        )?;
    }

    Ok(conn)
}

/// Open the existing index database under `root/.codemap/index.db`.
/// Bails if the index doesn't exist yet.
pub fn open_index(root: &Path) -> Result<Connection> {
    let db_path = root.join(".codemap/index.db");
    if !db_path.exists() {
        anyhow::bail!("No index found. Run `codemap index` first.");
    }
    init_db(
        db_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 database path"))?,
    )
}

pub fn get_file_hash(conn: &Connection, path: &str) -> Option<String> {
    conn.query_row(
        "SELECT hash FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )
    .ok()
}

pub fn get_file_id(conn: &Connection, path: &str) -> Option<i64> {
    conn.query_row(
        "SELECT id FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )
    .ok()
}

pub fn upsert_file(conn: &Connection, path: &str, hash: &str, rank: f64) -> Result<i64> {
    conn.execute(
        "INSERT INTO files (path, hash, rank) VALUES (?1, ?2, ?3)
         ON CONFLICT(path) DO UPDATE SET hash = ?2, rank = ?3, updated_at = unixepoch(),
         summary_enriched = CASE WHEN hash != ?2 THEN NULL ELSE summary_enriched END,
         when_to_use_enriched = CASE WHEN hash != ?2 THEN NULL ELSE when_to_use_enriched END,
         enriched_at = CASE WHEN hash != ?2 THEN NULL ELSE enriched_at END",
        params![path, hash, rank],
    )?;
    let id = conn.query_row(
        "SELECT id FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )?;
    Ok(id)
}

pub fn delete_stale_files(conn: &Connection, known_paths: &[String]) -> Result<usize> {
    if known_paths.is_empty() {
        let deleted = conn.execute("DELETE FROM files", [])?;
        return Ok(deleted);
    }
    let placeholders: Vec<String> = (1..=known_paths.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "DELETE FROM files WHERE path NOT IN ({})",
        placeholders.join(", ")
    );
    let params: Vec<&dyn rusqlite::types::ToSql> = known_paths
        .iter()
        .map(|p| p as &dyn rusqlite::types::ToSql)
        .collect();
    let deleted = conn.execute(&sql, params.as_slice())?;
    Ok(deleted)
}

pub fn insert_symbols(
    conn: &Connection,
    file_id: i64,
    symbols: &[(String, String, bool, Option<i32>, usize)],
) -> Result<()> {
    conn.execute("DELETE FROM symbols WHERE file_id = ?1", params![file_id])?;
    let mut stmt = conn.prepare(
        "INSERT INTO symbols (file_id, name, kind, is_exported, line, ref_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (name, kind, is_exported, line, ref_count) in symbols {
        stmt.execute(params![
            file_id,
            name,
            kind,
            *is_exported as i32,
            line,
            *ref_count as i64
        ])?;
    }
    Ok(())
}

pub fn insert_edges(
    conn: &Connection,
    source_id: i64,
    edges: &[(i64, String, Option<String>)],
) -> Result<()> {
    conn.execute("DELETE FROM edges WHERE source_id = ?1", params![source_id])?;
    let mut stmt = conn.prepare(
        "INSERT INTO edges (source_id, target_id, edge_type, specifier) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (target_id, edge_type, specifier) in edges {
        stmt.execute(params![source_id, target_id, edge_type, specifier])?;
    }
    Ok(())
}

pub fn update_ranks(conn: &Connection, ranks: &[(String, f64)]) -> Result<()> {
    let mut stmt = conn.prepare("UPDATE files SET rank = ?1 WHERE path = ?2")?;
    for (path, rank) in ranks {
        stmt.execute(params![rank, path])?;
    }
    Ok(())
}

pub struct SymbolResult {
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line: Option<i32>,
    pub is_exported: bool,
    pub ref_count: i64,
}

/// Query symbols by name. When `exact` is true, matches the full name;
/// otherwise uses substring (LIKE) matching.
pub fn query_symbols(
    conn: &Connection,
    pattern: &str,
    limit: usize,
    exact: bool,
) -> Result<Vec<SymbolResult>> {
    let (sql_pattern, where_clause) = if exact {
        (pattern.to_string(), "WHERE s.name = ?1")
    } else {
        (format!("%{pattern}%"), "WHERE s.name LIKE ?1")
    };
    let sql = format!(
        "SELECT s.name, s.kind, f.path, s.line, s.is_exported, s.ref_count
         FROM symbols s JOIN files f ON s.file_id = f.id
         {where_clause}
         ORDER BY s.is_exported DESC, s.ref_count DESC
         LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![sql_pattern, limit as i64], |row| {
        Ok(SymbolResult {
            name: row.get(0)?,
            kind: row.get(1)?,
            file_path: row.get(2)?,
            line: row.get(3)?,
            is_exported: row.get::<_, i32>(4)? != 0,
            ref_count: row.get(5)?,
        })
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub struct DepEdge {
    pub file_path: String,
    pub edge_type: String,
    pub specifier: Option<String>,
}

pub fn get_file_deps(conn: &Connection, file_path: &str, direction: &str) -> Result<Vec<DepEdge>> {
    let (sql, param) = match direction {
        "importers" => (
            "SELECT f.path, e.edge_type, e.specifier
             FROM edges e JOIN files f ON e.source_id = f.id
             WHERE e.target_id = (SELECT id FROM files WHERE path = ?1)",
            file_path,
        ),
        _ => (
            "SELECT f.path, e.edge_type, e.specifier
             FROM edges e JOIN files f ON e.target_id = f.id
             WHERE e.source_id = (SELECT id FROM files WHERE path = ?1)",
            file_path,
        ),
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![param], |row| {
        Ok(DepEdge {
            file_path: row.get(0)?,
            edge_type: row.get(1)?,
            specifier: row.get(2)?,
        })
    })?;
    let mut deps = Vec::new();
    for row in rows {
        deps.push(row?);
    }
    Ok(deps)
}

// --- v0.2 helpers ---

pub fn get_stats(conn: &Connection) -> Result<(i64, i64, i64)> {
    let files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    let exports: i64 = conn.query_row(
        "SELECT COUNT(*) FROM symbols WHERE is_exported = 1",
        [],
        |r| r.get(0),
    )?;
    let edges: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
    Ok((files, exports, edges))
}

pub fn get_importer_counts(conn: &Connection) -> Result<HashMap<i64, i64>> {
    let mut stmt = conn.prepare("SELECT target_id, COUNT(*) FROM edges GROUP BY target_id")?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
    let mut map = HashMap::new();
    for row in rows {
        let (id, count) = row?;
        map.insert(id, count);
    }
    Ok(map)
}

pub struct FileWithExports {
    pub path: String,
    pub rank: f64,
    pub exports: Vec<String>,
}

pub fn get_files_with_exports(
    conn: &Connection,
    unenriched_only: bool,
) -> Result<Vec<FileWithExports>> {
    let where_clause = if unenriched_only {
        "WHERE f.summary_enriched IS NULL"
    } else {
        ""
    };
    let sql = format!(
        "SELECT f.path, f.rank, s.name
         FROM files f
         LEFT JOIN symbols s ON s.file_id = f.id AND s.is_exported = 1
         {where_clause}
         ORDER BY f.rank DESC, f.path"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;

    let mut result: Vec<FileWithExports> = Vec::new();
    for row in rows {
        let (path, rank, export_name) = row?;
        // Group consecutive rows by path (ORDER BY guarantees grouping)
        if let Some(last) = result.last_mut()
            && last.path == path
        {
            if let Some(name) = export_name {
                last.exports.push(name);
            }
            continue;
        }
        result.push(FileWithExports {
            path,
            rank,
            exports: export_name.into_iter().collect(),
        });
    }
    Ok(result)
}

pub fn get_file_mtime(conn: &Connection, path: &str) -> Option<i64> {
    conn.query_row(
        "SELECT mtime FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )
    .ok()
    .flatten()
}

pub fn update_file_mtime(conn: &Connection, path: &str, mtime: i64) -> Result<()> {
    conn.execute(
        "UPDATE files SET mtime = ?1 WHERE path = ?2",
        params![mtime, path],
    )?;
    Ok(())
}

// --- Enrichment ---

pub fn set_enrichment(
    conn: &Connection,
    path: &str,
    summary: &str,
    when_to_use: &str,
) -> Result<()> {
    let updated = conn.execute(
        "UPDATE files SET summary_enriched = ?1, when_to_use_enriched = ?2, enriched_at = unixepoch() WHERE path = ?3",
        params![summary, when_to_use, path],
    )?;
    if updated == 0 {
        anyhow::bail!("File not found in index: {path}");
    }
    Ok(())
}

pub fn clear_enrichment(conn: &Connection, path: &str) -> Result<()> {
    let updated = conn.execute(
        "UPDATE files SET summary_enriched = NULL, when_to_use_enriched = NULL, enriched_at = NULL WHERE path = ?1",
        params![path],
    )?;
    if updated == 0 {
        anyhow::bail!("File not found in index: {path}");
    }
    Ok(())
}

pub fn clear_all_enrichments(conn: &Connection) -> Result<usize> {
    let updated = conn.execute(
        "UPDATE files SET summary_enriched = NULL, when_to_use_enriched = NULL, enriched_at = NULL WHERE summary_enriched IS NOT NULL",
        [],
    )?;
    Ok(updated)
}

pub struct EnrichmentStats {
    pub total_files: i64,
    pub enriched_files: i64,
}

pub fn get_enrichment_stats(conn: &Connection) -> Result<EnrichmentStats> {
    let total_files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    let enriched_files: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE summary_enriched IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(EnrichmentStats {
        total_files,
        enriched_files,
    })
}

pub struct RankedFileWithEnrichment {
    pub id: i64,
    pub path: String,
    pub rank: f64,
    pub summary_enriched: Option<String>,
    pub when_to_use_enriched: Option<String>,
}

pub fn get_ranked_files_with_enrichment(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<RankedFileWithEnrichment>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, rank, summary_enriched, when_to_use_enriched FROM files ORDER BY rank DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(RankedFileWithEnrichment {
            id: row.get(0)?,
            path: row.get(1)?,
            rank: row.get(2)?,
            summary_enriched: row.get(3)?,
            when_to_use_enriched: row.get(4)?,
        })
    })?;
    let mut files = Vec::new();
    for row in rows {
        files.push(row?);
    }
    Ok(files)
}

pub struct FileWithExportsAndEnrichment {
    pub path: String,
    pub rank: f64,
    pub exports: Vec<String>,
    pub summary_enriched: Option<String>,
    pub when_to_use_enriched: Option<String>,
}

pub fn get_all_files_with_exports_and_enrichment(
    conn: &Connection,
) -> Result<Vec<FileWithExportsAndEnrichment>> {
    let mut stmt = conn.prepare(
        "SELECT f.path, f.rank, s.name, f.summary_enriched, f.when_to_use_enriched
         FROM files f
         LEFT JOIN symbols s ON s.file_id = f.id AND s.is_exported = 1
         ORDER BY f.rank DESC, f.path",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    let mut result: Vec<FileWithExportsAndEnrichment> = Vec::new();
    for row in rows {
        let (path, rank, export_name, summary, when_to_use) = row?;
        if let Some(last) = result.last_mut()
            && last.path == path
        {
            if let Some(name) = export_name {
                last.exports.push(name);
            }
            continue;
        }
        result.push(FileWithExportsAndEnrichment {
            path,
            rank,
            exports: export_name.into_iter().collect(),
            summary_enriched: summary,
            when_to_use_enriched: when_to_use,
        });
    }
    Ok(result)
}
