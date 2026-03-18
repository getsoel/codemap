/// Results archive: store and compare eval runs over time.
use anyhow::Result;
use rusqlite::{Connection, params};
use std::path::Path;

pub fn open_history(eval_dir: &Path) -> Result<Connection> {
    let results_dir = eval_dir.join("results");
    std::fs::create_dir_all(&results_dir)?;
    let db_path = results_dir.join("history.db");
    let conn = Connection::open(&db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS runs (
            id          INTEGER PRIMARY KEY,
            git_commit  TEXT NOT NULL,
            git_dirty   INTEGER NOT NULL,
            timestamp   TEXT NOT NULL DEFAULT (datetime('now')),
            layer       TEXT NOT NULL,
            dataset     TEXT NOT NULL,
            language    TEXT NOT NULL DEFAULT 'js/ts',
            metrics     TEXT NOT NULL,
            config      TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_runs_commit ON runs(git_commit);
        CREATE INDEX IF NOT EXISTS idx_runs_dataset ON runs(dataset);",
    )?;
    Ok(conn)
}

pub fn get_git_info() -> (String, bool) {
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    let dirty = std::process::Command::new("git")
        .args(["diff", "--quiet", "HEAD"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);

    (commit, dirty)
}

#[allow(clippy::too_many_arguments)]
pub fn save_run(
    conn: &Connection,
    git_commit: &str,
    git_dirty: bool,
    layer: &str,
    dataset: &str,
    language: &str,
    metrics: &serde_json::Value,
    config: Option<&serde_json::Value>,
) -> Result<i64> {
    let metrics_str = serde_json::to_string(metrics)?;
    let config_str = config.map(serde_json::to_string).transpose()?;

    conn.execute(
        "INSERT INTO runs (git_commit, git_dirty, timestamp, layer, dataset, language, metrics, config)
         VALUES (?1, ?2, datetime('now'), ?3, ?4, ?5, ?6, ?7)",
        params![
            git_commit,
            git_dirty as i32,
            layer,
            dataset,
            language,
            metrics_str,
            config_str,
        ],
    )?;

    Ok(conn.last_insert_rowid())
}

#[derive(Debug)]
pub struct HistoricRun {
    pub id: i64,
    pub git_commit: String,
    pub git_dirty: bool,
    pub timestamp: String,
    pub dataset: String,
    #[allow(dead_code)] // stored for future per-language reporting
    pub language: String,
    pub metrics: serde_json::Value,
}

fn row_to_historic_run(row: &rusqlite::Row) -> rusqlite::Result<HistoricRun> {
    let metrics_str: String = row.get(6)?;
    Ok(HistoricRun {
        id: row.get(0)?,
        git_commit: row.get(1)?,
        git_dirty: row.get::<_, i32>(2)? != 0,
        timestamp: row.get(3)?,
        dataset: row.get(4)?,
        language: row.get(5)?,
        metrics: serde_json::from_str(&metrics_str).unwrap_or(serde_json::Value::Null),
    })
}

/// Get the most recent run for a dataset, optionally filtered by commit.
pub fn get_run(
    conn: &Connection,
    dataset: &str,
    commit: Option<&str>,
) -> Result<Option<HistoricRun>> {
    let (sql, param_commit);
    let params_vec: Vec<&dyn rusqlite::types::ToSql>;

    if let Some(c) = commit {
        sql = "SELECT id, git_commit, git_dirty, timestamp, dataset, language, metrics
               FROM runs WHERE dataset = ?1 AND git_commit LIKE ?2 AND layer = 'relevance'
               ORDER BY id DESC LIMIT 1";
        param_commit = format!("{c}%");
        params_vec = vec![&dataset as &dyn rusqlite::types::ToSql, &param_commit];
    } else {
        sql = "SELECT id, git_commit, git_dirty, timestamp, dataset, language, metrics
               FROM runs WHERE dataset = ?1 AND layer = 'relevance'
               ORDER BY id DESC LIMIT 1";
        params_vec = vec![&dataset as &dyn rusqlite::types::ToSql];
    };

    let result = conn.query_row(sql, params_vec.as_slice(), row_to_historic_run);

    match result {
        Ok(run) => Ok(Some(run)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get all runs for a dataset, ordered by most recent first.
pub fn list_runs(
    conn: &Connection,
    dataset: Option<&str>,
    limit: usize,
) -> Result<Vec<HistoricRun>> {
    let limit_i64 = limit as i64;

    let query_and_map = |stmt: &mut rusqlite::Statement,
                         p: &[&dyn rusqlite::types::ToSql]|
     -> Result<Vec<HistoricRun>> {
        let rows = stmt.query_map(p, row_to_historic_run)?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    };

    if let Some(ds) = dataset {
        let mut stmt = conn.prepare(
            "SELECT id, git_commit, git_dirty, timestamp, dataset, language, metrics
             FROM runs WHERE dataset = ?1 AND layer = 'relevance'
             ORDER BY id DESC LIMIT ?2",
        )?;
        query_and_map(&mut stmt, &[&ds as &dyn rusqlite::types::ToSql, &limit_i64])
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, git_commit, git_dirty, timestamp, dataset, language, metrics
             FROM runs WHERE layer = 'relevance'
             ORDER BY id DESC LIMIT ?1",
        )?;
        query_and_map(&mut stmt, &[&limit_i64 as &dyn rusqlite::types::ToSql])
    }
}

pub fn json_f64(value: &serde_json::Value, key: &str) -> f64 {
    value.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
}
