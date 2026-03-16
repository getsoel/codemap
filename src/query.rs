/// Symbol/file search command.
use crate::db;
use anyhow::Result;
use std::path::Path;

pub fn run_query(root: &Path, pattern: &str, limit: usize) -> Result<()> {
    let db_path = root.join(".codemap/index.db");
    if !db_path.exists() {
        anyhow::bail!("No index found. Run `codemap index` first.");
    }
    let conn = db::init_db(
        db_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 database path"))?,
    )?;

    let results = db::query_symbols(&conn, pattern, limit)?;
    if results.is_empty() {
        eprintln!("No symbols matching '{pattern}'");
        return Ok(());
    }

    for result in &results {
        let exported = if result.is_exported {
            "exported"
        } else {
            "local"
        };
        let line_info = result.line.map(|l| format!(":{l}")).unwrap_or_default();
        println!(
            "{}{line_info}  {} {} ({exported}, {} refs)",
            result.file_path, result.name, result.kind, result.ref_count
        );
    }

    Ok(())
}
