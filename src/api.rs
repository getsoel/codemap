/// LLM provider abstraction for file enrichment.
use anyhow::Result;
use std::thread;
use std::time::Duration;

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

fn build_agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .build(),
    )
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
}

impl GeminiProvider {
    pub fn new(api_key: String, model: Option<String>) -> Self {
        Self {
            api_key,
            model: model.unwrap_or_else(|| "gemini-2.0-flash".to_string()),
            agent: build_agent(),
        }
    }
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
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseSchema": {
                    "type": "OBJECT",
                    "properties": {
                        "summary": { "type": "STRING" },
                        "when_to_use": { "type": "STRING" }
                    },
                    "required": ["summary", "when_to_use"]
                },
                "maxOutputTokens": 256,
                "temperature": 0.2
            }
        });

        let resp = retry_request_json(&self.agent, &url, &body, &[])?;
        let text = resp
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("unexpected Gemini response structure"))?;
        let parsed: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| anyhow::anyhow!("failed to parse Gemini JSON response: {e}"))?;
        extract_enrichment(&parsed)
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

fn retry_request_json(
    agent: &ureq::Agent,
    url: &str,
    body: &serde_json::Value,
    extra_headers: &[(&str, &str)],
) -> Result<serde_json::Value> {
    let max_retries = 3;
    for attempt in 0..=max_retries {
        let mut req = agent.post(url).header("Content-Type", "application/json");
        for &(k, v) in extra_headers {
            req = req.header(k, v);
        }
        match req.send_json(body) {
            Ok(mut resp) => {
                let json: serde_json::Value = resp.body_mut().read_json()?;
                return Ok(json);
            }
            Err(ureq::Error::StatusCode(429)) if attempt < max_retries => {
                let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
                tracing::warn!("rate limited, retrying in {:?}", delay);
                thread::sleep(delay);
            }
            Err(ureq::Error::StatusCode(403)) => {
                anyhow::bail!("API key is invalid or unauthorized (403). Check your API key.");
            }
            Err(ureq::Error::StatusCode(status)) => {
                anyhow::bail!("API request failed with status {status}");
            }
            Err(e) if attempt < max_retries => {
                let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
                tracing::warn!("request error: {e}, retrying in {:?}", delay);
                thread::sleep(delay);
            }
            Err(e) => {
                anyhow::bail!("API request failed after {max_retries} retries: {e}");
            }
        }
    }
    unreachable!()
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
            let key = api_key
                .map(String::from)
                .or_else(|| std::env::var("GEMINI_API_KEY").ok())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No Gemini API key found. Set GEMINI_API_KEY or pass --api-key.\n\
                         Get a free key at https://aistudio.google.com/apikey"
                    )
                })?;
            Ok(Box::new(GeminiProvider::new(key, model)))
        }
        "anthropic" => {
            let key = api_key
                .map(String::from)
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No Anthropic API key found. Set ANTHROPIC_API_KEY or pass --api-key.\n\
                         Get a key at https://console.anthropic.com/settings/keys"
                    )
                })?;
            Ok(Box::new(AnthropicProvider::new(key, model)))
        }
        other => anyhow::bail!("Unknown provider: {other}. Use 'gemini' or 'anthropic'."),
    }
}

/// Detect language from file extension.
pub fn detect_language(path: &str) -> &str {
    if path.ends_with(".ts") || path.ends_with(".tsx") {
        "TypeScript"
    } else {
        "JavaScript"
    }
}
