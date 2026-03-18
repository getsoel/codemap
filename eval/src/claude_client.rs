/// Claude Messages API client for A/B eval sessions.
///
/// Runs multi-turn tool-use conversations against the Anthropic API,
/// tracking metrics (tool calls, files read, tokens, etc.) per session.
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::thread;
use std::time::Duration;

pub struct ClaudeClient {
    api_key: String,
    model: String,
    agent: ureq::Agent,
}

pub struct SessionConfig<'a> {
    pub system: &'a str,
    pub tools: &'a [Value],
    pub max_turns: usize,
    pub max_input_tokens: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionMetrics {
    pub variant: String,
    pub case_id: String,
    pub tool_calls: usize,
    pub tool_calls_by_type: HashMap<String, usize>,
    pub files_read: HashSet<String>,
    pub files_mentioned: HashSet<String>,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub turns: usize,
    pub first_relevant_file_turn: Option<usize>,
}

impl ClaudeClient {
    pub fn new(api_key: String, model: String) -> Self {
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(Duration::from_secs(120)))
                .http_status_as_error(false)
                .build(),
        );
        Self {
            api_key,
            model,
            agent,
        }
    }

    /// Run a full tool-use session and collect metrics.
    #[allow(clippy::too_many_arguments)]
    pub fn run_session(
        &self,
        config: &SessionConfig,
        user_message: &str,
        tool_executor: &dyn Fn(&str, &Value) -> String,
        expected_files: &HashSet<String>,
        known_files: &HashSet<String>,
        variant: &str,
        case_id: &str,
    ) -> Result<SessionMetrics> {
        let mut messages: Vec<Value> = vec![json!({
            "role": "user",
            "content": user_message
        })];

        let mut metrics = SessionMetrics {
            variant: variant.to_string(),
            case_id: case_id.to_string(),
            tool_calls: 0,
            tool_calls_by_type: HashMap::new(),
            files_read: HashSet::new(),
            files_mentioned: HashSet::new(),
            input_tokens: 0,
            output_tokens: 0,
            turns: 0,
            first_relevant_file_turn: None,
        };

        for turn in 0..config.max_turns {
            if metrics.input_tokens >= config.max_input_tokens {
                eprintln!(
                    "    Token budget exceeded ({} >= {})",
                    metrics.input_tokens, config.max_input_tokens
                );
                break;
            }

            let body = json!({
                "model": &self.model,
                "max_tokens": 4096,
                "system": &config.system,
                "messages": &messages,
                "tools": &config.tools,
                "temperature": 0.0
            });

            let resp = self.call_api(&body)?;

            // Track token usage
            if let Some(usage) = resp.get("usage") {
                metrics.input_tokens += usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                metrics.output_tokens += usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
            }

            metrics.turns = turn + 1;

            let content = resp
                .get("content")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let stop_reason = resp
                .get("stop_reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Append assistant message to conversation
            messages.push(json!({
                "role": "assistant",
                "content": &content
            }));

            // Process content blocks
            let mut tool_results = Vec::new();
            for block in &content {
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        let mentioned = extract_mentioned_files(text, known_files);
                        for f in &mentioned {
                            metrics.files_mentioned.insert(f.clone());
                            if metrics.first_relevant_file_turn.is_none()
                                && expected_files.contains(f)
                            {
                                metrics.first_relevant_file_turn = Some(turn + 1);
                            }
                        }
                    }
                    "tool_use" => {
                        let tool_name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let tool_id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let tool_input = block.get("input").cloned().unwrap_or(json!({}));

                        metrics.tool_calls += 1;
                        *metrics
                            .tool_calls_by_type
                            .entry(tool_name.to_string())
                            .or_insert(0) += 1;

                        // Track files read via read_file
                        if tool_name == "read_file"
                            && let Some(path) = tool_input.get("path").and_then(|v| v.as_str())
                        {
                            metrics.files_read.insert(path.to_string());
                            if metrics.first_relevant_file_turn.is_none()
                                && expected_files.contains(path)
                            {
                                metrics.first_relevant_file_turn = Some(turn + 1);
                            }
                        }

                        eprintln!("    [{variant}] turn {}: {tool_name}", turn + 1);

                        let result = tool_executor(tool_name, &tool_input);
                        tool_results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": tool_id,
                            "content": result
                        }));
                    }
                    _ => {}
                }
            }

            if stop_reason == "end_turn" {
                break;
            }

            if !tool_results.is_empty() {
                messages.push(json!({
                    "role": "user",
                    "content": tool_results
                }));
            } else {
                // No tool calls and stop_reason isn't "tool_use" — done
                break;
            }
        }

        Ok(metrics)
    }

    /// POST to the Messages API with retry logic for transient errors.
    fn call_api(&self, body: &Value) -> Result<Value> {
        let max_retries = 3;
        for attempt in 0..=max_retries {
            let resp = self
                .agent
                .post("https://api.anthropic.com/v1/messages")
                .header("Content-Type", "application/json")
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .send_json(body);

            match resp {
                Ok(mut resp) => {
                    let status = resp.status().as_u16();
                    match status {
                        200..=299 => {
                            let json: Value = resp.body_mut().read_json()?;
                            return Ok(json);
                        }
                        429 if attempt < max_retries => {
                            let delay = Duration::from_millis(2000 * 2u64.pow(attempt as u32));
                            eprintln!("    Rate limited (429), retrying in {delay:?}");
                            thread::sleep(delay);
                        }
                        500 | 502 | 503 if attempt < max_retries => {
                            let delay = Duration::from_millis(1000 * 2u64.pow(attempt as u32));
                            eprintln!("    Server error ({status}), retrying in {delay:?}");
                            thread::sleep(delay);
                        }
                        _ => {
                            let err_body = resp
                                .body_mut()
                                .read_json::<Value>()
                                .ok()
                                .map(|v| v.to_string())
                                .unwrap_or_default();
                            anyhow::bail!("Claude API returned {status}: {err_body}");
                        }
                    }
                }
                Err(e) if attempt < max_retries => {
                    let delay = Duration::from_millis(1000 * 2u64.pow(attempt as u32));
                    eprintln!("    Request error: {e}, retrying in {delay:?}");
                    thread::sleep(delay);
                }
                Err(e) => {
                    anyhow::bail!("Claude API request failed after {max_retries} retries: {e}");
                }
            }
        }
        unreachable!()
    }
}

/// Extract file paths mentioned in Claude's text by matching against known repo files.
pub fn extract_mentioned_files(text: &str, known_files: &HashSet<String>) -> Vec<String> {
    known_files
        .iter()
        .filter(|f| text.contains(f.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_mentioned_files_finds_known() {
        let known: HashSet<String> = ["src/context.ts", "src/types.ts", "src/index.ts"]
            .into_iter()
            .map(Into::into)
            .collect();
        let text = "The relevant files are src/context.ts and src/types.ts";
        let mut found = extract_mentioned_files(text, &known);
        found.sort();
        assert_eq!(found, vec!["src/context.ts", "src/types.ts"]);
    }

    #[test]
    fn extract_mentioned_files_no_match() {
        let known: HashSet<String> = ["src/context.ts"].into_iter().map(Into::into).collect();
        let text = "The context module handles HTTP responses";
        let found = extract_mentioned_files(text, &known);
        assert!(found.is_empty());
    }
}
