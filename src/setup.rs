/// Setup command: configure Claude Code hooks.
use crate::{db, index};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn run_setup(root: &Path, no_post_hook: bool, global: bool, dry_run: bool) -> Result<()> {
    // Step 1: Run index
    eprintln!("Indexing codebase...");
    index::run_index(root, false, false)?;

    let conn = db::open_index(root)?;
    let (files, exports, edges) = db::get_stats(&conn)?;
    eprintln!("✓ Indexed {files} files ({exports} exports, {edges} edges)");

    // Step 2: Determine config path
    let config_path = if global {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .map_err(|_| anyhow::anyhow!("Cannot determine home directory"))?;
        home.join(".claude/settings.json")
    } else {
        root.join(".claude/settings.local.json")
    };

    // Step 3: Read existing config
    let mut config: serde_json::Value = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    // Step 4: Build hook entries
    let session_hook = serde_json::json!({
        "hooks": [{
            "type": "command",
            "command": "codemap instructions",
            "timeout": 10
        }]
    });

    let post_tool_hook = serde_json::json!({
        "matcher": "Write|Edit|MultiEdit",
        "hooks": [{
            "type": "command",
            "command": "codemap index --incremental && codemap enrich --api --top 10 --if-available",
            "timeout": 60,
            "async": true
        }]
    });

    // Step 5: Merge hooks into config
    let hooks = config
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Config is not a JSON object"))?
        .entry("hooks")
        .or_insert(serde_json::json!({}));

    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("hooks is not a JSON object"))?;

    // SessionStart: remove existing codemap entries, append new
    {
        let arr = hooks_obj
            .entry("SessionStart")
            .or_insert(serde_json::json!([]));
        if let Some(entries) = arr.as_array_mut() {
            entries.retain(|entry| !is_codemap_hook_entry(entry));
            entries.push(session_hook);
        }
    }

    // PostToolUse: remove existing codemap entries, append new (unless --no-post-hook)
    {
        let arr = hooks_obj
            .entry("PostToolUse")
            .or_insert(serde_json::json!([]));
        if let Some(entries) = arr.as_array_mut() {
            entries.retain(|entry| !is_codemap_hook_entry(entry));
            if !no_post_hook {
                entries.push(post_tool_hook);
            }
        }
    }

    // Step 6: Write config
    let pretty = serde_json::to_string_pretty(&config)?;
    if dry_run {
        eprintln!("Would write to {}:", config_path.display());
        eprintln!("{pretty}");
    } else {
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&config_path, pretty.as_bytes())?;
        eprintln!("✓ Wrote SessionStart hook to {}", config_path.display());
        if !no_post_hook {
            eprintln!("✓ Wrote PostToolUse hook (async re-index + enrich on file changes)");
        }
    }

    // Step 7: Summary
    eprintln!();
    eprintln!("codemap is ready. Start Claude Code and it will automatically");
    eprintln!("receive the codebase map and know how to use codemap commands.");

    Ok(())
}

/// Check if a hook entry contains a codemap command.
fn is_codemap_hook_entry(entry: &serde_json::Value) -> bool {
    // Check hooks[].command for "codemap" prefix
    if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
        return hooks.iter().any(|h| {
            h.get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c.starts_with("codemap"))
        });
    }
    false
}
