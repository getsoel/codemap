/// Claude CLI session runner and stream-json parser.
///
/// Spawns `claude -p` with `--output-format stream-json`, captures the output,
/// and parses it into structured metrics for evaluation.
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct RawSessionOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub wall_clock_ms: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SessionMetrics {
    pub variant: String,
    pub case_id: String,

    // Tool usage
    pub tool_calls: usize,
    pub tool_calls_by_name: HashMap<String, usize>,
    pub codemap_calls: usize,

    // File discovery
    pub files_read: HashSet<String>,
    pub files_identified: Vec<String>,
    pub files_mentioned: HashSet<String>,

    // Cost
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub wall_clock_ms: u64,

    // Speed
    pub turns: usize,
    pub first_relevant_file_turn: Option<usize>,
}

/// Spawn claude CLI and capture stream-json output.
pub fn run_claude_session(
    working_dir: &Path,
    task_prompt: &str,
    model: &str,
    max_turns: usize,
    timeout_secs: u64,
    append_system_prompt: Option<&str>,
) -> Result<RawSessionOutput> {
    let mut cmd = Command::new("claude");
    cmd.current_dir(working_dir)
        .arg("-p")
        .arg(task_prompt)
        .args(["--output-format", "stream-json"])
        .args(["--model", model])
        .args(["--max-turns", &max_turns.to_string()])
        .args(["--permission-mode", "plan"]);

    if let Some(prompt) = append_system_prompt {
        cmd.args(["--append-system-prompt", prompt]);
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .context("failed to spawn claude CLI — is it installed?")?;

    // Read stdout/stderr in separate threads to avoid pipe deadlock
    let stdout_pipe = child.stdout.take().unwrap();
    let stderr_pipe = child.stderr.take().unwrap();

    let stdout_thread = thread::spawn(move || {
        let mut buf = String::new();
        std::io::BufReader::new(stdout_pipe)
            .read_to_string(&mut buf)
            .ok();
        buf
    });
    let stderr_thread = thread::spawn(move || {
        let mut buf = String::new();
        std::io::BufReader::new(stderr_pipe)
            .read_to_string(&mut buf)
            .ok();
        buf
    });

    // Wait for process with timeout
    let timeout = Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait()? {
            Some(status) => {
                let elapsed = start.elapsed();
                let stdout = stdout_thread.join().unwrap_or_default();
                let stderr = stderr_thread.join().unwrap_or_default();
                return Ok(RawSessionOutput {
                    stdout,
                    stderr,
                    exit_code: status.code(),
                    wall_clock_ms: elapsed.as_millis() as u64,
                });
            }
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait(); // reap zombie
                    let elapsed = start.elapsed();
                    let stdout = stdout_thread.join().unwrap_or_default();
                    let stderr = stderr_thread.join().unwrap_or_default();
                    eprintln!("    Session timed out after {timeout_secs}s");
                    return Ok(RawSessionOutput {
                        stdout,
                        stderr,
                        exit_code: None,
                        wall_clock_ms: elapsed.as_millis() as u64,
                    });
                }
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Parse stream-json output into structured metrics.
///
/// The stream-json format emits newline-delimited JSON events with a `type` field.
/// Key event types: `assistant`, `tool_use`, `tool_result`, `result`.
pub fn parse_stream_output(
    raw: &RawSessionOutput,
    known_files: &HashSet<String>,
    expected_files: &HashSet<String>,
    variant: &str,
    case_id: &str,
    workspace_prefix: &str,
) -> SessionMetrics {
    let mut metrics = SessionMetrics {
        variant: variant.to_string(),
        case_id: case_id.to_string(),
        wall_clock_ms: raw.wall_clock_ms,
        ..Default::default()
    };

    let mut turn_counter = 0usize;

    for line in raw.stdout.lines() {
        if !line.starts_with('{') {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "tool_use" => {
                let name = event
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                metrics.tool_calls += 1;
                *metrics
                    .tool_calls_by_name
                    .entry(name.to_string())
                    .or_default() += 1;
                turn_counter += 1;

                // Track file reads via Read tool
                if name == "Read"
                    && let Some(path) = event.pointer("/input/file_path").and_then(|v| v.as_str())
                {
                    let normalized = normalize_path(path, workspace_prefix);
                    if metrics.first_relevant_file_turn.is_none()
                        && expected_files.contains(&normalized)
                    {
                        metrics.first_relevant_file_turn = Some(turn_counter);
                    }
                    metrics.files_read.insert(normalized);
                }

                // Track codemap usage via Bash tool
                if name == "Bash"
                    && let Some(cmd) = event.pointer("/input/command").and_then(|v| v.as_str())
                    && cmd.contains("codemap")
                {
                    metrics.codemap_calls += 1;
                }
            }
            "assistant" => {
                if let Some(text) = extract_text_from_message(&event) {
                    for file in known_files.iter().filter(|f| text.contains(f.as_str())) {
                        if metrics.first_relevant_file_turn.is_none()
                            && expected_files.contains(file)
                        {
                            metrics.first_relevant_file_turn = Some(turn_counter.max(1));
                        }
                        metrics.files_mentioned.insert(file.clone());
                    }
                }
            }
            "result" => {
                // Extract token usage
                if let Some(usage) = event.get("usage") {
                    metrics.input_tokens = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    metrics.output_tokens = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                }

                // Extract file mentions and structured file list from final result
                if let Some(result_text) = event.get("result").and_then(|v| v.as_str()) {
                    for file in known_files
                        .iter()
                        .filter(|f| result_text.contains(f.as_str()))
                    {
                        metrics.files_mentioned.insert(file.clone());
                    }

                    if let Some(files) = extract_json_file_list(result_text) {
                        metrics.files_identified = files;
                    }
                }
            }
            _ => {}
        }
    }

    metrics.turns = turn_counter;
    metrics
}

/// Strip workspace temp directory prefix to get repo-relative paths.
fn normalize_path(path: &str, workspace_prefix: &str) -> String {
    path.strip_prefix(workspace_prefix)
        .unwrap_or(path)
        .trim_start_matches('/')
        .to_string()
}

/// Extract text content from an assistant message event.
///
/// Handles both `{"message":{"content":[...]}}` and `{"content":[...]}` shapes.
fn extract_text_from_message(event: &Value) -> Option<String> {
    let content = event
        .pointer("/message/content")
        .or_else(|| event.get("content"))
        .and_then(|v| v.as_array())?;

    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(|v| v.as_str()) == Some("text")
            && let Some(t) = block.get("text").and_then(|v| v.as_str())
        {
            text.push_str(t);
        }
    }

    if text.is_empty() { None } else { Some(text) }
}

/// Try to parse a JSON file list from Claude's response text.
///
/// Looks for `{"relevant_files": ["path1", "path2", ...]}` anywhere in the text.
fn extract_json_file_list(text: &str) -> Option<Vec<String>> {
    let start = text.find('{')?;
    let end = text.rfind('}')? + 1;
    if start >= end {
        return None;
    }
    let json_str = &text[start..end];

    let parsed: Value = serde_json::from_str(json_str).ok()?;
    let files = parsed.get("relevant_files")?.as_array()?;

    Some(
        files
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_prefix() {
        assert_eq!(
            normalize_path("/tmp/abc123/src/main.ts", "/tmp/abc123/"),
            "src/main.ts"
        );
    }

    #[test]
    fn normalize_no_prefix() {
        assert_eq!(normalize_path("src/main.ts", "/tmp/other/"), "src/main.ts");
    }

    #[test]
    fn extract_json_file_list_valid() {
        let text = r#"Here are the files: {"relevant_files": ["src/a.ts", "src/b.ts"]}"#;
        let files = extract_json_file_list(text).unwrap();
        assert_eq!(files, vec!["src/a.ts", "src/b.ts"]);
    }

    #[test]
    fn extract_json_file_list_no_json() {
        assert!(extract_json_file_list("no json here").is_none());
    }

    #[test]
    fn extract_json_file_list_wrong_key() {
        let text = r#"{"files": ["a.ts"]}"#;
        assert!(extract_json_file_list(text).is_none());
    }

    #[test]
    fn extract_text_nested_message() {
        let event: Value = serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello world"}]}}"#,
        ).unwrap();
        assert_eq!(extract_text_from_message(&event).unwrap(), "hello world");
    }

    #[test]
    fn extract_text_flat_content() {
        let event: Value = serde_json::from_str(
            r#"{"type":"assistant","content":[{"type":"text","text":"flat"}]}"#,
        )
        .unwrap();
        assert_eq!(extract_text_from_message(&event).unwrap(), "flat");
    }

    #[test]
    fn parse_stream_basic() {
        let known: HashSet<String> = ["src/a.ts", "src/b.ts"]
            .into_iter()
            .map(Into::into)
            .collect();
        let expected: HashSet<String> = ["src/a.ts"].into_iter().map(Into::into).collect();

        let raw = RawSessionOutput {
            stdout: [
                r#"{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/ws/src/a.ts"}}"#,
                r#"{"type":"tool_result","name":"Read","content":"..."}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Found src/a.ts and src/b.ts"}]}}"#,
                r#"{"type":"result","result":"{\"relevant_files\": [\"src/a.ts\", \"src/b.ts\"]}","usage":{"input_tokens":1000,"output_tokens":200}}"#,
            ].join("\n"),
            stderr: String::new(),
            exit_code: Some(0),
            wall_clock_ms: 5000,
        };

        let metrics = parse_stream_output(&raw, &known, &expected, "test", "t-001", "/tmp/ws/");

        assert_eq!(metrics.tool_calls, 1);
        assert_eq!(metrics.turns, 1);
        assert!(metrics.files_read.contains("src/a.ts"));
        assert!(metrics.files_mentioned.contains("src/a.ts"));
        assert!(metrics.files_mentioned.contains("src/b.ts"));
        assert_eq!(metrics.files_identified, vec!["src/a.ts", "src/b.ts"]);
        assert_eq!(metrics.input_tokens, 1000);
        assert_eq!(metrics.output_tokens, 200);
        assert_eq!(metrics.first_relevant_file_turn, Some(1));
        assert_eq!(metrics.wall_clock_ms, 5000);
    }

    #[test]
    fn parse_stream_codemap_bash() {
        let known: HashSet<String> = HashSet::new();
        let expected: HashSet<String> = HashSet::new();

        let raw = RawSessionOutput {
            stdout: r#"{"type":"tool_use","name":"Bash","input":{"command":"codemap context \"JWT auth\""}}"#.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            wall_clock_ms: 1000,
        };

        let metrics = parse_stream_output(&raw, &known, &expected, "test", "t-002", "/tmp/ws/");

        assert_eq!(metrics.codemap_calls, 1);
        assert_eq!(metrics.tool_calls_by_name.get("Bash"), Some(&1));
    }
}
