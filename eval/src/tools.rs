/// Tool definitions and execution for A/B eval sessions.
///
/// Provides the tool schemas (in Anthropic format) and implementations
/// that execute against a fixture repository.
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

/// Tools available to both control and treatment variants.
pub fn control_tools() -> Vec<Value> {
    vec![grep_tool(), glob_tool(), read_file_tool()]
}

/// All tools: control tools + codemap structural tools.
pub fn treatment_tools() -> Vec<Value> {
    let mut tools = control_tools();
    tools.push(codemap_context_tool());
    tools.push(codemap_symbol_tool());
    tools.push(codemap_deps_tool());
    tools
}

fn grep_tool() -> Value {
    json!({
        "name": "grep",
        "description": "Search file contents for a regex pattern. Returns matching file paths and line content.",
        "input_schema": {
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "Directory or file to search in. Defaults to repo root." },
                "glob": { "type": "string", "description": "File glob filter, e.g. '*.ts'" }
            },
            "required": ["pattern"]
        }
    })
}

fn glob_tool() -> Value {
    json!({
        "name": "glob",
        "description": "Find files matching a glob pattern. Returns file paths sorted by modification time.",
        "input_schema": {
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. 'src/**/*.ts'" }
            },
            "required": ["pattern"]
        }
    })
}

fn read_file_tool() -> Value {
    json!({
        "name": "read_file",
        "description": "Read the contents of a file.",
        "input_schema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to repo root" },
                "limit": { "type": "integer", "description": "Max lines to read. Default: 200" }
            },
            "required": ["path"]
        }
    })
}

fn codemap_context_tool() -> Value {
    json!({
        "name": "codemap_context",
        "description": "Find the most relevant files for a task description, ranked by structural importance.",
        "input_schema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Natural language task description" },
                "limit": { "type": "integer", "description": "Max results. Default: 10" }
            },
            "required": ["query"]
        }
    })
}

fn codemap_symbol_tool() -> Value {
    json!({
        "name": "codemap_symbol",
        "description": "Find where a symbol is defined and who references it.",
        "input_schema": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Symbol name or pattern" },
                "exact": { "type": "boolean", "description": "Exact match only. Default: false" }
            },
            "required": ["name"]
        }
    })
}

fn codemap_deps_tool() -> Value {
    json!({
        "name": "codemap_deps",
        "description": "Show imports and importers of a file.",
        "input_schema": {
            "type": "object",
            "properties": {
                "file": { "type": "string", "description": "File path to inspect" },
                "direction": { "type": "string", "enum": ["imports", "importers", "both"], "description": "Default: both" }
            },
            "required": ["file"]
        }
    })
}

/// Execute a tool call and return the result text.
pub fn execute_tool(name: &str, input: &Value, repo_dir: &Path, codemap_bin: &Path) -> String {
    match name {
        "grep" => exec_grep(input, repo_dir),
        "glob" => exec_glob(input, repo_dir),
        "read_file" => exec_read_file(input, repo_dir),
        "codemap_context" => exec_codemap_context(input, repo_dir, codemap_bin),
        "codemap_symbol" => exec_codemap_symbol(input, repo_dir, codemap_bin),
        "codemap_deps" => exec_codemap_deps(input, repo_dir, codemap_bin),
        _ => format!("Unknown tool: {name}"),
    }
}

fn exec_grep(input: &Value, repo_dir: &Path) -> String {
    let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let sub_path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let glob_filter = input.get("glob").and_then(|v| v.as_str());

    let search_path = repo_dir.join(sub_path);
    let mut cmd = Command::new("rg");
    cmd.args(["--max-count", "20", "-n", "--no-heading"]);
    if let Some(g) = glob_filter {
        cmd.args(["--glob", g]);
    }
    cmd.arg(pattern).arg(&search_path);

    let output = run_command(&mut cmd);
    // Strip the repo_dir prefix from output paths for cleaner results
    let prefix = format!("{}/", repo_dir.display());
    output
        .lines()
        .map(|line| line.strip_prefix(&prefix).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn exec_glob(input: &Value, repo_dir: &Path) -> String {
    let pattern = input
        .get("pattern")
        .and_then(|v| v.as_str())
        .unwrap_or("**/*");

    // Use rg --files with glob filter (respects .gitignore)
    let mut cmd = Command::new("rg");
    cmd.args(["--files", "--glob", pattern]).arg(repo_dir);

    let output = run_command(&mut cmd);
    // Strip repo_dir prefix
    let prefix = format!("{}/", repo_dir.display());
    output
        .lines()
        .map(|line| line.strip_prefix(&prefix).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn exec_read_file(input: &Value, repo_dir: &Path) -> String {
    let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

    let file_path = repo_dir.join(path);
    match std::fs::read_to_string(&file_path) {
        Ok(content) => content
            .lines()
            .take(limit)
            .enumerate()
            .map(|(i, line)| format!("{:>4}  {}", i + 1, line))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(e) => format!("Error reading {path}: {e}"),
    }
}

fn exec_codemap_context(input: &Value, repo_dir: &Path, codemap_bin: &Path) -> String {
    let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);

    let mut cmd = Command::new(codemap_bin);
    cmd.current_dir(repo_dir)
        .args(["context", "--json", query, "--limit", &limit.to_string()]);
    run_command(&mut cmd)
}

fn exec_codemap_symbol(input: &Value, repo_dir: &Path, codemap_bin: &Path) -> String {
    let name = input.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let exact = input
        .get("exact")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut cmd = Command::new(codemap_bin);
    cmd.current_dir(repo_dir).args(["symbol", "--json", name]);
    if exact {
        cmd.arg("--exact");
    }
    run_command(&mut cmd)
}

fn exec_codemap_deps(input: &Value, repo_dir: &Path, codemap_bin: &Path) -> String {
    let file = input.get("file").and_then(|v| v.as_str()).unwrap_or("");
    let direction = input
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("both");

    let mut cmd = Command::new(codemap_bin);
    cmd.current_dir(repo_dir)
        .args(["deps", "--json", file, "--direction", direction]);
    run_command(&mut cmd)
}

fn run_command(cmd: &mut Command) -> String {
    match cmd.output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if output.status.success() {
                if stdout.is_empty() {
                    "No results found.".to_string()
                } else if stdout.len() > 8000 {
                    // Truncate at a char boundary to avoid panic on multi-byte UTF-8
                    let end = (0..=8000)
                        .rev()
                        .find(|&i| stdout.is_char_boundary(i))
                        .unwrap_or(0);
                    format!(
                        "{}\n... (truncated, {} total chars)",
                        &stdout[..end],
                        stdout.len()
                    )
                } else {
                    stdout.into_owned()
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.is_empty() {
                    "No results found.".to_string()
                } else {
                    format!("Error: {stderr}")
                }
            }
        }
        Err(e) => format!("Failed to execute command: {e}"),
    }
}
