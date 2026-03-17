/// LLM provider abstraction for file enrichment.
use anyhow::Result;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

pub struct EnrichmentRequest {
    pub file_path: String,
    pub language: String,
    pub imports: Vec<String>,
    pub exports: Vec<String>,
}

pub struct EnrichmentResult {
    pub summary: String,
    pub when_to_use: String,
}

pub trait EnrichmentProvider: Send + Sync {
    fn name(&self) -> &str;
    fn enrich(&self, req: &EnrichmentRequest) -> Result<EnrichmentResult>;
}

// --- Rate limiter ---

/// Throttles outbound requests so no two fire closer than `min_interval` apart.
pub struct RateLimiter {
    next_allowed: Mutex<Instant>,
    min_interval: Duration,
}

impl RateLimiter {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            next_allowed: Mutex::new(Instant::now()),
            min_interval,
        }
    }

    /// Blocks the calling thread until the next request slot is available.
    pub fn wait(&self) {
        let sleep_until = {
            let mut next = self.next_allowed.lock().unwrap();
            let now = Instant::now();
            if *next > now {
                let target = *next;
                *next += self.min_interval;
                target
            } else {
                *next = now + self.min_interval;
                now // proceed immediately
            }
        };
        // Sleep outside the lock so other threads can queue up
        let now = Instant::now();
        if sleep_until > now {
            thread::sleep(sleep_until - now);
        }
    }
}

/// Build the prompt sent to any provider.
fn build_prompt(req: &EnrichmentRequest) -> String {
    let mut prompt = format!(
        "Analyze this {} file and describe its purpose.\n\nFile: {}",
        req.language, req.file_path
    );
    if !req.imports.is_empty() {
        prompt.push_str(&format!("\nImports: {}", req.imports.join(", ")));
    }
    if !req.exports.is_empty() {
        prompt.push_str("\nExports:");
        for exp in &req.exports {
            prompt.push_str(&format!("\n  {exp}"));
        }
    }
    prompt.push_str(
        "\n\nProvide a JSON object with two fields:\n\
         - \"summary\": 1-2 sentences describing what this file does. Focus on purpose and behavior, not structure.\n\
         - \"when_to_use\": When would a developer need to look at or modify this file? List specific scenarios.",
    );
    prompt
}

fn build_agent_with_timeout(timeout_secs: u64) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(timeout_secs)))
            .http_status_as_error(false)
            .build(),
    )
}

fn build_agent() -> ureq::Agent {
    build_agent_with_timeout(30)
}

/// Check if a JSON object has a boolean field set to true.
fn json_is_true(obj: &serde_json::Value, key: &str) -> bool {
    obj.get(key).and_then(|v| v.as_bool()) == Some(true)
}

/// Extract summary + when_to_use from a JSON object (shared by both providers).
fn extract_enrichment(obj: &serde_json::Value) -> Result<EnrichmentResult> {
    let summary = obj
        .get("summary")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'summary' in response"))?
        .to_string();
    let when_to_use = obj
        .get("when_to_use")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'when_to_use' in response"))?
        .to_string();
    Ok(EnrichmentResult {
        summary,
        when_to_use,
    })
}

// --- Gemini ---

pub struct GeminiProvider {
    api_key: String,
    model: String,
    agent: ureq::Agent,
    generation_config: serde_json::Value,
}

impl GeminiProvider {
    pub fn new(api_key: String, model: Option<String>) -> Self {
        Self {
            api_key,
            model: model.unwrap_or_else(|| "gemini-2.5-flash-lite".to_string()),
            agent: build_agent(),
            generation_config: serde_json::json!({
                "responseMimeType": "application/json",
                "responseSchema": {
                    "type": "OBJECT",
                    "properties": {
                        "summary": { "type": "STRING" },
                        "when_to_use": { "type": "STRING" }
                    },
                    "required": ["summary", "when_to_use"]
                },
                "maxOutputTokens": 1024,
                "temperature": 0.2
            }),
        }
    }

    /// Batch enrich files via Gemini's batchGenerateContent endpoint.
    /// Sends up to `batch_size` requests per HTTP call, polls if async.
    pub fn batch_enrich(
        &self,
        reqs: &[EnrichmentRequest],
        batch_size: usize,
    ) -> Vec<(String, Result<EnrichmentResult>)> {
        let agent = build_agent_with_timeout(300);
        let mut results = Vec::with_capacity(reqs.len());
        let total_batches = reqs.len().div_ceil(batch_size);
        let model_path = format!("models/{}", self.model);

        for (chunk_idx, chunk) in reqs.chunks(batch_size).enumerate() {
            eprintln!(
                "codemap: sending batch {}/{} ({} files)...",
                chunk_idx + 1,
                total_batches,
                chunk.len()
            );

            let inline_requests: Vec<serde_json::Value> = chunk
                .iter()
                .map(|req| {
                    serde_json::json!({
                        "request": {
                            "model": model_path,
                            "contents": [{
                                "parts": [{ "text": build_prompt(req) }]
                            }],
                            "generationConfig": self.generation_config.clone()
                        }
                    })
                })
                .collect();

            let body = serde_json::json!({
                "batch": {
                    "displayName": format!("codemap-enrich-{}", chunk_idx),
                    "inputConfig": {
                        "requests": {
                            "requests": inline_requests
                        }
                    }
                }
            });

            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:batchGenerateContent?key={}",
                self.model, self.api_key
            );

            match self.execute_batch(&agent, &url, &body) {
                Ok(responses) => {
                    for (i, file_req) in chunk.iter().enumerate() {
                        let result = responses
                            .get(i)
                            .ok_or_else(|| anyhow::anyhow!("missing response for index {i}"))
                            .and_then(parse_gemini_candidate);
                        results.push((file_req.file_path.clone(), result));
                    }
                }
                Err(e) => {
                    // If the batch call itself fails, mark all files in this chunk as failed
                    for file_req in chunk {
                        results.push((
                            file_req.file_path.clone(),
                            Err(anyhow::anyhow!("batch request failed: {e}")),
                        ));
                    }
                }
            }
        }

        results
    }

    /// Execute a batch request, polling if the operation is async.
    fn execute_batch(
        &self,
        agent: &ureq::Agent,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<Vec<serde_json::Value>> {
        let mut resp = agent
            .post(url)
            .header("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| anyhow::anyhow!("batch request failed: {e}"))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            anyhow::bail!("batch API returned status {status}");
        }

        let json: serde_json::Value = resp.body_mut().read_json()?;

        // If the response is an async operation, poll until done
        let json = if json.get("done").is_some() {
            self.poll_operation(agent, json)?
        } else {
            json
        };

        // Extract inline responses
        Self::extract_batch_responses(json)
    }

    /// Poll a long-running operation until it completes.
    fn poll_operation(
        &self,
        agent: &ureq::Agent,
        initial: serde_json::Value,
    ) -> Result<serde_json::Value> {
        if json_is_true(&initial, "done") {
            return Ok(initial);
        }

        let op_name = initial
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing operation name in batch response"))?
            .to_string();

        let poll_url = format!(
            "https://generativelanguage.googleapis.com/v1beta/{op_name}?key={}",
            self.api_key
        );

        let start = Instant::now();
        let max_wait = Duration::from_secs(600); // 10 minute timeout
        let mut poll_interval = Duration::from_secs(5);

        loop {
            thread::sleep(poll_interval);
            let elapsed = start.elapsed();
            if elapsed > max_wait {
                anyhow::bail!("batch operation timed out after {elapsed:?}");
            }

            eprintln!(
                "codemap: polling batch operation... ({:.0}s elapsed)",
                elapsed.as_secs_f64()
            );

            let mut resp = agent
                .get(&poll_url)
                .call()
                .map_err(|e| anyhow::anyhow!("poll request failed: {e}"))?;

            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                anyhow::bail!("poll returned status {status}");
            }

            let json: serde_json::Value = resp.body_mut().read_json()?;

            if json_is_true(&json, "done") {
                if let Some(error) = json.get("error") {
                    anyhow::bail!("batch operation failed: {error}");
                }
                return Ok(json);
            }

            // Increase poll interval gradually, cap at 15s
            poll_interval = (poll_interval * 3 / 2).min(Duration::from_secs(15));
        }
    }

    /// Extract responses from a completed batch result.
    fn extract_batch_responses(mut json: serde_json::Value) -> Result<Vec<serde_json::Value>> {
        // Try inline responses directly (synchronous batch)
        if let Some(responses) = json.get_mut("responses").and_then(|v| v.as_array_mut()) {
            return Ok(std::mem::take(responses));
        }

        // Try from operation result wrapper ("result" or "response" key)
        for key in ["result", "response"] {
            if let Some(responses) = json
                .get_mut(key)
                .and_then(|r| r.get_mut("inlinedResponses"))
                .and_then(|v| v.as_array_mut())
            {
                let extracted: Vec<serde_json::Value> = responses
                    .iter_mut()
                    .filter_map(|r| r.get_mut("response").map(serde_json::Value::take))
                    .collect();
                return Ok(extracted);
            }
        }

        anyhow::bail!(
            "unexpected batch response structure: {}",
            serde_json::to_string_pretty(&json).unwrap_or_default()
        )
    }
}

/// Parse a single Gemini generateContent response into an EnrichmentResult.
fn parse_gemini_candidate(resp: &serde_json::Value) -> Result<EnrichmentResult> {
    // Check for blocked prompt
    if resp
        .get("candidates")
        .and_then(|c| c.as_array())
        .is_none_or(|c| c.is_empty())
    {
        let block_reason = resp
            .pointer("/promptFeedback/blockReason")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        anyhow::bail!("Gemini blocked the prompt (reason: {block_reason})");
    }

    let finish_reason = resp
        .pointer("/candidates/0/finishReason")
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN");
    if finish_reason != "STOP" {
        anyhow::bail!(
            "Gemini response incomplete (finishReason: {finish_reason}). \
             Response may have been truncated."
        );
    }

    let text = resp
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("unexpected Gemini response structure"))?;
    let parsed: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| anyhow::anyhow!("failed to parse Gemini JSON response: {e}"))?;
    extract_enrichment(&parsed)
}

impl EnrichmentProvider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    fn enrich(&self, req: &EnrichmentRequest) -> Result<EnrichmentResult> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            self.model, self.api_key
        );

        let body = serde_json::json!({
            "contents": [{
                "parts": [{ "text": build_prompt(req) }]
            }],
            "generationConfig": self.generation_config.clone()
        });

        let resp = retry_request_json(&self.agent, &url, &body, &[], &self.model)?;
        parse_gemini_candidate(&resp)
    }
}

// --- Anthropic ---

pub struct AnthropicProvider {
    api_key: String,
    model: String,
    agent: ureq::Agent,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: Option<String>) -> Self {
        Self {
            api_key,
            model: model.unwrap_or_else(|| "claude-haiku-4-5-20251001".to_string()),
            agent: build_agent(),
        }
    }
}

impl EnrichmentProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn enrich(&self, req: &EnrichmentRequest) -> Result<EnrichmentResult> {
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 300,
            "tools": [{
                "name": "record_file_metadata",
                "description": "Record metadata about a source file.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "summary": {
                            "type": "string",
                            "description": "1-2 sentence description of what this file does. Focus on purpose and behavior, not structure."
                        },
                        "when_to_use": {
                            "type": "string",
                            "description": "When would a developer need to look at or modify this file? List specific scenarios."
                        }
                    },
                    "required": ["summary", "when_to_use"]
                }
            }],
            "tool_choice": { "type": "tool", "name": "record_file_metadata" },
            "messages": [{
                "role": "user",
                "content": build_prompt(req)
            }]
        });

        let headers = [
            ("x-api-key", self.api_key.as_str()),
            ("anthropic-version", "2023-06-01"),
        ];
        let resp = retry_request_json(
            &self.agent,
            "https://api.anthropic.com/v1/messages",
            &body,
            &headers,
            &self.model,
        )?;
        let content = resp
            .get("content")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("unexpected Anthropic response structure"))?;
        let tool_input = content
            .iter()
            .find(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
            .and_then(|block| block.get("input"))
            .ok_or_else(|| anyhow::anyhow!("no tool_use block in Anthropic response"))?;
        extract_enrichment(tool_input)
    }
}

// --- Shared retry logic ---

/// Parse the `Retry-After` header as seconds, capped at 60s.
fn parse_retry_after(resp: &ureq::http::Response<ureq::Body>) -> Option<Duration> {
    let header = resp.headers().get("retry-after")?;
    let secs: u64 = header.to_str().ok()?.trim().parse().ok()?;
    Some(Duration::from_secs(secs.min(60)))
}

fn retry_request_json(
    agent: &ureq::Agent,
    url: &str,
    body: &serde_json::Value,
    extra_headers: &[(&str, &str)],
    model: &str,
) -> Result<serde_json::Value> {
    let max_retries = 3;
    for attempt in 0..=max_retries {
        let mut req = agent.post(url).header("Content-Type", "application/json");
        for &(k, v) in extra_headers {
            req = req.header(k, v);
        }
        match req.send_json(body) {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                match status {
                    200..=299 => {
                        let json: serde_json::Value = resp.body_mut().read_json()?;
                        return Ok(json);
                    }
                    429 if attempt < max_retries => {
                        let delay = parse_retry_after(&resp).unwrap_or_else(|| {
                            Duration::from_millis(500 * 2u64.pow(attempt as u32))
                        });
                        tracing::warn!("HTTP 429 rate limited, retrying in {delay:?}");
                        thread::sleep(delay);
                    }
                    500 | 502 | 503 if attempt < max_retries => {
                        let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
                        tracing::warn!("HTTP {status}, retrying in {delay:?}");
                        thread::sleep(delay);
                    }
                    403 => {
                        anyhow::bail!(
                            "API key is invalid or unauthorized (403). Check your API key."
                        );
                    }
                    404 => {
                        anyhow::bail!(
                            "Model not found (404). The model '{model}' may not be available \
                             for your API key. Try a different model with --model."
                        );
                    }
                    429 | 500 | 502 | 503 => {
                        anyhow::bail!(
                            "API request failed with status {status} after {max_retries} retries"
                        );
                    }
                    _ => {
                        anyhow::bail!("API request failed with status {status}");
                    }
                }
            }
            Err(e) if attempt < max_retries => {
                let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
                tracing::warn!("request error: {e}, retrying in {delay:?}");
                thread::sleep(delay);
            }
            Err(e) => {
                anyhow::bail!("API request failed after {max_retries} retries: {e}");
            }
        }
    }
    unreachable!()
}

/// Resolve an API key from explicit arg or env var.
fn resolve_api_key(
    api_key: Option<&str>,
    env_var: &str,
    provider_name: &str,
    signup_url: &str,
) -> Result<String> {
    api_key
        .map(String::from)
        .or_else(|| std::env::var(env_var).ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No {provider_name} API key found. Set {env_var} or pass --api-key.\n\
                 Get a key at {signup_url}"
            )
        })
}

/// Resolve which provider to use based on CLI args and env vars.
pub fn resolve_provider(
    provider_name: &str,
    api_key: Option<&str>,
    model: Option<&str>,
) -> Result<Box<dyn EnrichmentProvider>> {
    let model = model.map(String::from);

    match provider_name {
        "gemini" => {
            let key = resolve_api_key(
                api_key,
                "GEMINI_API_KEY",
                "Gemini",
                "https://aistudio.google.com/apikey",
            )?;
            Ok(Box::new(GeminiProvider::new(key, model)))
        }
        "anthropic" => {
            let key = resolve_api_key(
                api_key,
                "ANTHROPIC_API_KEY",
                "Anthropic",
                "https://console.anthropic.com/settings/keys",
            )?;
            Ok(Box::new(AnthropicProvider::new(key, model)))
        }
        other => anyhow::bail!("Unknown provider: {other}. Use 'gemini' or 'anthropic'."),
    }
}

/// Resolve a Gemini provider directly (for batch mode).
pub fn resolve_gemini_provider(
    api_key: Option<&str>,
    model: Option<&str>,
) -> Result<GeminiProvider> {
    let key = resolve_api_key(
        api_key,
        "GEMINI_API_KEY",
        "Gemini",
        "https://aistudio.google.com/apikey",
    )?;
    Ok(GeminiProvider::new(key, model.map(String::from)))
}

/// Detect language from file extension.
pub fn detect_language(path: &str) -> &str {
    if path.ends_with(".ts") || path.ends_with(".tsx") {
        "TypeScript"
    } else {
        "JavaScript"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- RateLimiter ---

    #[test]
    fn rate_limiter_enforces_interval() {
        let limiter = RateLimiter::new(Duration::from_millis(50));
        let start = Instant::now();
        limiter.wait(); // first call should be immediate
        limiter.wait(); // second should wait ~50ms
        limiter.wait(); // third should wait ~50ms more
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(100),
            "expected >= 100ms, got {elapsed:?}"
        );
    }

    // --- detect_language ---

    #[test]
    fn detect_typescript() {
        assert_eq!(detect_language("src/foo.ts"), "TypeScript");
        assert_eq!(detect_language("src/bar.tsx"), "TypeScript");
    }

    #[test]
    fn detect_javascript() {
        assert_eq!(detect_language("src/foo.js"), "JavaScript");
        assert_eq!(detect_language("src/bar.jsx"), "JavaScript");
    }

    // --- build_prompt ---

    #[test]
    fn build_prompt_includes_file_path() {
        let req = EnrichmentRequest {
            file_path: "src/utils.ts".to_string(),
            language: "TypeScript".to_string(),
            imports: vec![],
            exports: vec![],
        };
        let prompt = build_prompt(&req);
        assert!(prompt.contains("src/utils.ts"));
        assert!(prompt.contains("TypeScript"));
    }

    #[test]
    fn build_prompt_includes_imports() {
        let req = EnrichmentRequest {
            file_path: "src/foo.ts".to_string(),
            language: "TypeScript".to_string(),
            imports: vec!["react".to_string(), "lodash".to_string()],
            exports: vec![],
        };
        let prompt = build_prompt(&req);
        assert!(prompt.contains("react"));
        assert!(prompt.contains("lodash"));
    }

    #[test]
    fn build_prompt_includes_exports() {
        let req = EnrichmentRequest {
            file_path: "src/foo.ts".to_string(),
            language: "TypeScript".to_string(),
            imports: vec![],
            exports: vec!["handleClick".to_string(), "useAuth".to_string()],
        };
        let prompt = build_prompt(&req);
        assert!(prompt.contains("handleClick"));
        assert!(prompt.contains("useAuth"));
    }

    #[test]
    fn build_prompt_omits_empty_sections() {
        let req = EnrichmentRequest {
            file_path: "src/foo.ts".to_string(),
            language: "TypeScript".to_string(),
            imports: vec![],
            exports: vec![],
        };
        let prompt = build_prompt(&req);
        assert!(!prompt.contains("Imports:"));
        assert!(!prompt.contains("Exports:"));
    }

    // --- extract_enrichment ---

    #[test]
    fn extract_enrichment_valid() {
        let json = serde_json::json!({
            "summary": "Handles auth",
            "when_to_use": "When modifying login"
        });
        let result = extract_enrichment(&json).unwrap();
        assert_eq!(result.summary, "Handles auth");
        assert_eq!(result.when_to_use, "When modifying login");
    }

    #[test]
    fn extract_enrichment_missing_summary() {
        let json = serde_json::json!({
            "when_to_use": "When modifying login"
        });
        assert!(extract_enrichment(&json).is_err());
    }

    #[test]
    fn extract_enrichment_missing_when_to_use() {
        let json = serde_json::json!({
            "summary": "Handles auth"
        });
        assert!(extract_enrichment(&json).is_err());
    }

    #[test]
    fn extract_enrichment_wrong_types() {
        let json = serde_json::json!({
            "summary": 42,
            "when_to_use": true
        });
        assert!(extract_enrichment(&json).is_err());
    }

    // --- resolve_provider ---

    #[test]
    fn resolve_provider_unknown() {
        let result = resolve_provider("openai", None, None);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_provider_gemini_with_key() {
        let provider = resolve_provider("gemini", Some("test-key"), None).unwrap();
        assert_eq!(provider.name(), "gemini");
    }

    #[test]
    fn resolve_provider_anthropic_with_key() {
        let provider = resolve_provider("anthropic", Some("test-key"), None).unwrap();
        assert_eq!(provider.name(), "anthropic");
    }

    // --- Live API tests (run with `cargo test -- --ignored`) ---

    fn test_request() -> EnrichmentRequest {
        EnrichmentRequest {
            file_path: "src/utils.ts".to_string(),
            language: "TypeScript".to_string(),
            imports: vec!["lodash".to_string()],
            exports: vec!["formatDate".to_string(), "parseConfig".to_string()],
        }
    }

    #[test]
    #[ignore]
    fn gemini_api_returns_valid_enrichment() {
        let key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");
        let provider = GeminiProvider::new(key, None);
        let result = provider
            .enrich(&test_request())
            .expect("Gemini API call failed");
        assert!(!result.summary.is_empty(), "summary should not be empty");
        assert!(
            !result.when_to_use.is_empty(),
            "when_to_use should not be empty"
        );
    }

    #[test]
    #[ignore]
    fn anthropic_api_returns_valid_enrichment() {
        let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");
        let provider = AnthropicProvider::new(key, None);
        let result = provider
            .enrich(&test_request())
            .expect("Anthropic API call failed");
        assert!(!result.summary.is_empty(), "summary should not be empty");
        assert!(
            !result.when_to_use.is_empty(),
            "when_to_use should not be empty"
        );
    }
}
