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

/// Create an in-memory database for testing.
#[cfg(test)]
pub fn init_test_db() -> Result<Connection> {
    init_db(":memory:")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        init_test_db().unwrap()
    }

    // --- init_db ---

    #[test]
    fn init_creates_tables() {
        let conn = setup();
        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        let symbols: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        let edges: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 0);
        assert_eq!(symbols, 0);
        assert_eq!(edges, 0);
    }

    #[test]
    fn init_has_enrichment_columns() {
        let conn = setup();
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(files)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(columns.contains(&"summary_enriched".to_string()));
        assert!(columns.contains(&"when_to_use_enriched".to_string()));
        assert!(columns.contains(&"enriched_at".to_string()));
        assert!(columns.contains(&"mtime".to_string()));
    }

    // --- upsert_file ---

    #[test]
    fn upsert_inserts_new_file() {
        let conn = setup();
        let id = upsert_file(&conn, "src/foo.ts", "abc123", 0.5).unwrap();
        assert!(id > 0);
        assert_eq!(get_file_hash(&conn, "src/foo.ts").unwrap(), "abc123");
    }

    #[test]
    fn upsert_updates_existing_file() {
        let conn = setup();
        let id1 = upsert_file(&conn, "src/foo.ts", "abc123", 0.5).unwrap();
        let id2 = upsert_file(&conn, "src/foo.ts", "def456", 0.8).unwrap();
        assert_eq!(id1, id2);
        assert_eq!(get_file_hash(&conn, "src/foo.ts").unwrap(), "def456");
    }

    #[test]
    fn upsert_clears_enrichment_on_hash_change() {
        let conn = setup();
        upsert_file(&conn, "src/foo.ts", "abc123", 0.5).unwrap();
        set_enrichment(&conn, "src/foo.ts", "A summary", "When to use").unwrap();

        upsert_file(&conn, "src/foo.ts", "different_hash", 0.5).unwrap();

        let stats = get_enrichment_stats(&conn).unwrap();
        assert_eq!(stats.enriched_files, 0);
    }

    #[test]
    fn upsert_preserves_enrichment_on_same_hash() {
        let conn = setup();
        upsert_file(&conn, "src/foo.ts", "abc123", 0.5).unwrap();
        set_enrichment(&conn, "src/foo.ts", "A summary", "When to use").unwrap();

        upsert_file(&conn, "src/foo.ts", "abc123", 0.8).unwrap();

        let stats = get_enrichment_stats(&conn).unwrap();
        assert_eq!(stats.enriched_files, 1);
    }

    // --- get_file_hash / get_file_id ---

    #[test]
    fn get_hash_nonexistent() {
        let conn = setup();
        assert!(get_file_hash(&conn, "nope.ts").is_none());
    }

    #[test]
    fn get_id_nonexistent() {
        let conn = setup();
        assert!(get_file_id(&conn, "nope.ts").is_none());
    }

    #[test]
    fn get_id_after_upsert() {
        let conn = setup();
        let id = upsert_file(&conn, "src/foo.ts", "abc", 0.0).unwrap();
        assert_eq!(get_file_id(&conn, "src/foo.ts").unwrap(), id);
    }

    // --- delete_stale_files ---

    #[test]
    fn delete_stale_removes_unknown_files() {
        let conn = setup();
        upsert_file(&conn, "keep.ts", "a", 0.0).unwrap();
        upsert_file(&conn, "stale.ts", "b", 0.0).unwrap();

        let deleted = delete_stale_files(&conn, &["keep.ts".to_string()]).unwrap();
        assert_eq!(deleted, 1);
        assert!(get_file_hash(&conn, "stale.ts").is_none());
        assert!(get_file_hash(&conn, "keep.ts").is_some());
    }

    #[test]
    fn delete_stale_empty_known_deletes_all() {
        let conn = setup();
        upsert_file(&conn, "a.ts", "a", 0.0).unwrap();
        upsert_file(&conn, "b.ts", "b", 0.0).unwrap();

        let deleted = delete_stale_files(&conn, &[]).unwrap();
        assert_eq!(deleted, 2);
    }

    // --- insert_symbols / query_symbols ---

    #[test]
    fn insert_and_query_symbols() {
        let conn = setup();
        let id = upsert_file(&conn, "src/foo.ts", "abc", 0.0).unwrap();
        let symbols = vec![
            (
                "doThing".to_string(),
                "function".to_string(),
                true,
                Some(1),
                3_usize,
            ),
            (
                "helper".to_string(),
                "function".to_string(),
                false,
                Some(5),
                1_usize,
            ),
        ];
        insert_symbols(&conn, id, &symbols).unwrap();

        let results = query_symbols(&conn, "doThing", 10, true).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "doThing");
        assert!(results[0].is_exported);
        assert_eq!(results[0].ref_count, 3);
    }

    #[test]
    fn query_symbols_substring() {
        let conn = setup();
        let id = upsert_file(&conn, "src/foo.ts", "abc", 0.0).unwrap();
        let symbols = vec![
            (
                "handleClick".to_string(),
                "function".to_string(),
                true,
                Some(1),
                0_usize,
            ),
            (
                "handleSubmit".to_string(),
                "function".to_string(),
                true,
                Some(5),
                0_usize,
            ),
        ];
        insert_symbols(&conn, id, &symbols).unwrap();

        let results = query_symbols(&conn, "handle", 10, false).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn query_symbols_respects_limit() {
        let conn = setup();
        let id = upsert_file(&conn, "src/foo.ts", "abc", 0.0).unwrap();
        let symbols: Vec<_> = (0..10)
            .map(|i| {
                (
                    format!("sym{i}"),
                    "variable".to_string(),
                    true,
                    Some(i),
                    0_usize,
                )
            })
            .collect();
        insert_symbols(&conn, id, &symbols).unwrap();

        let results = query_symbols(&conn, "sym", 3, false).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn insert_symbols_replaces_previous() {
        let conn = setup();
        let id = upsert_file(&conn, "src/foo.ts", "abc", 0.0).unwrap();

        let syms1 = vec![(
            "old".to_string(),
            "function".to_string(),
            true,
            Some(1),
            0_usize,
        )];
        insert_symbols(&conn, id, &syms1).unwrap();

        let syms2 = vec![(
            "new_sym".to_string(),
            "function".to_string(),
            true,
            Some(1),
            0_usize,
        )];
        insert_symbols(&conn, id, &syms2).unwrap();

        assert!(query_symbols(&conn, "old", 10, true).unwrap().is_empty());
        assert_eq!(query_symbols(&conn, "new_sym", 10, true).unwrap().len(), 1);
    }

    // --- insert_edges / get_file_deps ---

    #[test]
    fn insert_and_query_edges() {
        let conn = setup();
        let id_a = upsert_file(&conn, "a.ts", "a", 0.0).unwrap();
        let id_b = upsert_file(&conn, "b.ts", "b", 0.0).unwrap();

        let edges = vec![(id_b, "import".to_string(), Some("foo".to_string()))];
        insert_edges(&conn, id_a, &edges).unwrap();

        let imports = get_file_deps(&conn, "a.ts", "imports").unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].file_path, "b.ts");

        let importers = get_file_deps(&conn, "b.ts", "importers").unwrap();
        assert_eq!(importers.len(), 1);
        assert_eq!(importers[0].file_path, "a.ts");
    }

    // --- update_ranks ---

    #[test]
    fn update_ranks_changes_rank() {
        let conn = setup();
        upsert_file(&conn, "a.ts", "a", 0.0).unwrap();
        upsert_file(&conn, "b.ts", "b", 0.0).unwrap();

        let ranks = vec![("a.ts".to_string(), 0.9), ("b.ts".to_string(), 0.1)];
        update_ranks(&conn, &ranks).unwrap();

        let files = get_ranked_files_with_enrichment(&conn, 10).unwrap();
        assert_eq!(files[0].path, "a.ts");
        assert!((files[0].rank - 0.9).abs() < f64::EPSILON);
    }

    // --- get_stats ---

    #[test]
    fn get_stats_counts() {
        let conn = setup();
        let id = upsert_file(&conn, "a.ts", "a", 0.0).unwrap();
        let id_b = upsert_file(&conn, "b.ts", "b", 0.0).unwrap();
        insert_symbols(
            &conn,
            id,
            &[(
                "foo".to_string(),
                "function".to_string(),
                true,
                Some(1),
                0_usize,
            )],
        )
        .unwrap();
        insert_edges(&conn, id, &[(id_b, "import".to_string(), None)]).unwrap();

        let (files, exports, edges) = get_stats(&conn).unwrap();
        assert_eq!(files, 2);
        assert_eq!(exports, 1);
        assert_eq!(edges, 1);
    }

    // --- enrichment ---

    #[test]
    fn set_and_get_enrichment() {
        let conn = setup();
        upsert_file(&conn, "a.ts", "a", 0.0).unwrap();
        set_enrichment(&conn, "a.ts", "File summary", "When modifying X").unwrap();

        let stats = get_enrichment_stats(&conn).unwrap();
        assert_eq!(stats.total_files, 1);
        assert_eq!(stats.enriched_files, 1);

        let files = get_ranked_files_with_enrichment(&conn, 10).unwrap();
        assert_eq!(files[0].summary_enriched.as_deref(), Some("File summary"));
        assert_eq!(
            files[0].when_to_use_enriched.as_deref(),
            Some("When modifying X")
        );
    }

    #[test]
    fn set_enrichment_nonexistent_file() {
        let conn = setup();
        assert!(set_enrichment(&conn, "nope.ts", "summary", "when").is_err());
    }

    #[test]
    fn clear_enrichment_single() {
        let conn = setup();
        upsert_file(&conn, "a.ts", "a", 0.0).unwrap();
        set_enrichment(&conn, "a.ts", "summary", "when").unwrap();
        clear_enrichment(&conn, "a.ts").unwrap();

        assert_eq!(get_enrichment_stats(&conn).unwrap().enriched_files, 0);
    }

    #[test]
    fn clear_enrichment_nonexistent() {
        let conn = setup();
        assert!(clear_enrichment(&conn, "nope.ts").is_err());
    }

    #[test]
    fn clear_all_enrichments_works() {
        let conn = setup();
        upsert_file(&conn, "a.ts", "a", 0.0).unwrap();
        upsert_file(&conn, "b.ts", "b", 0.0).unwrap();
        set_enrichment(&conn, "a.ts", "s1", "w1").unwrap();
        set_enrichment(&conn, "b.ts", "s2", "w2").unwrap();

        assert_eq!(clear_all_enrichments(&conn).unwrap(), 2);
        assert_eq!(get_enrichment_stats(&conn).unwrap().enriched_files, 0);
    }

    // --- get_files_with_exports ---

    #[test]
    fn files_with_exports_unenriched_filter() {
        let conn = setup();
        let id_a = upsert_file(&conn, "a.ts", "a", 0.5).unwrap();
        let id_b = upsert_file(&conn, "b.ts", "b", 0.3).unwrap();
        insert_symbols(
            &conn,
            id_a,
            &[(
                "foo".to_string(),
                "function".to_string(),
                true,
                Some(1),
                0_usize,
            )],
        )
        .unwrap();
        insert_symbols(
            &conn,
            id_b,
            &[(
                "bar".to_string(),
                "function".to_string(),
                true,
                Some(1),
                0_usize,
            )],
        )
        .unwrap();
        set_enrichment(&conn, "a.ts", "summary", "when").unwrap();

        let unenriched = get_files_with_exports(&conn, true).unwrap();
        assert_eq!(unenriched.len(), 1);
        assert_eq!(unenriched[0].path, "b.ts");

        let all = get_files_with_exports(&conn, false).unwrap();
        assert_eq!(all.len(), 2);
    }

    // --- mtime ---

    #[test]
    fn file_mtime_roundtrip() {
        let conn = setup();
        upsert_file(&conn, "a.ts", "a", 0.0).unwrap();
        assert!(get_file_mtime(&conn, "a.ts").is_none());

        update_file_mtime(&conn, "a.ts", 1234567890).unwrap();
        assert_eq!(get_file_mtime(&conn, "a.ts").unwrap(), 1234567890);
    }

    // --- importer_counts ---

    #[test]
    fn importer_counts() {
        let conn = setup();
        let id_a = upsert_file(&conn, "a.ts", "a", 0.0).unwrap();
        let id_b = upsert_file(&conn, "b.ts", "b", 0.0).unwrap();
        let id_c = upsert_file(&conn, "c.ts", "c", 0.0).unwrap();

        insert_edges(&conn, id_a, &[(id_c, "import".to_string(), None)]).unwrap();
        insert_edges(&conn, id_b, &[(id_c, "import".to_string(), None)]).unwrap();

        let counts = get_importer_counts(&conn).unwrap();
        assert_eq!(*counts.get(&id_c).unwrap(), 2);
        assert!(!counts.contains_key(&id_a));
    }
}
