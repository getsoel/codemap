/// SQLite storage layer for codemap.
use anyhow::Result;
use rusqlite::{Connection, params};

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
    Ok(conn)
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
         ON CONFLICT(path) DO UPDATE SET hash = ?2, rank = ?3, updated_at = unixepoch()",
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

pub struct RankedFile {
    pub id: i64,
    pub path: String,
    pub rank: f64,
}

pub fn get_ranked_files(conn: &Connection, limit: usize) -> Result<Vec<RankedFile>> {
    let mut stmt = conn.prepare("SELECT id, path, rank FROM files ORDER BY rank DESC LIMIT ?1")?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(RankedFile {
            id: row.get(0)?,
            path: row.get(1)?,
            rank: row.get(2)?,
        })
    })?;
    let mut files = Vec::new();
    for row in rows {
        files.push(row?);
    }
    Ok(files)
}

pub struct SymbolResult {
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line: Option<i32>,
    pub is_exported: bool,
    pub ref_count: i64,
}

pub fn query_symbols(conn: &Connection, pattern: &str, limit: usize) -> Result<Vec<SymbolResult>> {
    let like_pattern = format!("%{pattern}%");
    let mut stmt = conn.prepare(
        "SELECT s.name, s.kind, f.path, s.line, s.is_exported, s.ref_count
         FROM symbols s JOIN files f ON s.file_id = f.id
         WHERE s.name LIKE ?1
         ORDER BY s.is_exported DESC, s.ref_count DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![like_pattern, limit as i64], |row| {
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
