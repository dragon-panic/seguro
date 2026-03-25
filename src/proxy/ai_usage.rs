use color_eyre::eyre::{Result, WrapErr};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ── Provider detection ───────────────────────────────────────────────────────

/// Known AI API provider, identified by destination hostname.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    OpenAI,
    Google,
    Mistral,
    Custom,
}

/// Map of hostnames → providers. Built from defaults + config overrides.
pub struct ProviderMap {
    entries: Vec<(String, Provider)>,
}

impl ProviderMap {
    pub fn new(custom: &HashMap<String, Vec<String>>) -> Self {
        let mut entries: Vec<(String, Provider)> = vec![
            ("api.anthropic.com".into(), Provider::Anthropic),
            ("api.openai.com".into(), Provider::OpenAI),
            ("generativelanguage.googleapis.com".into(), Provider::Google),
            ("api.mistral.ai".into(), Provider::Mistral),
        ];

        // Config overrides: if a user defines "anthropic = [...]" it replaces the
        // default entries for that provider name; unknown names map to Custom.
        for (name, hosts) in custom {
            let provider = match name.as_str() {
                "anthropic" => Provider::Anthropic,
                "openai" => Provider::OpenAI,
                "google" => Provider::Google,
                "mistral" => Provider::Mistral,
                _ => Provider::Custom,
            };
            for host in hosts {
                // Remove any existing entry for this host (config wins)
                entries.retain(|(h, _)| h != host);
                entries.push((host.clone(), provider));
            }
        }

        Self { entries }
    }

    /// Look up a hostname. Returns None if not a recognised AI API host.
    pub fn lookup(&self, host: &str) -> Option<Provider> {
        self.entries
            .iter()
            .find(|(h, _)| h == host)
            .map(|(_, p)| *p)
    }
}

impl Default for ProviderMap {
    fn default() -> Self {
        Self::new(&HashMap::new())
    }
}

// ── Token extraction ─────────────────────────────────────────────────────────

/// Extracted usage metadata from an AI API response.
#[derive(Debug, Default)]
pub struct ExtractedUsage {
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
}

/// Try to extract token usage from a response body. Best-effort: returns
/// defaults on parse failure.
pub fn extract_usage(provider: Provider, body: &[u8]) -> ExtractedUsage {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) else {
        return ExtractedUsage::default();
    };
    match provider {
        Provider::Anthropic => extract_anthropic(&json),
        Provider::OpenAI => extract_openai(&json),
        Provider::Google => extract_google(&json),
        Provider::Mistral => extract_openai(&json), // Mistral uses OpenAI-compatible format
        Provider::Custom => extract_openai(&json),   // best guess
    }
}

/// Extract from final SSE data line. For streaming responses, the caller
/// accumulates `data:` lines and passes the last non-empty one.
pub fn extract_usage_from_sse(provider: Provider, last_data: &[u8]) -> ExtractedUsage {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(last_data) else {
        return ExtractedUsage::default();
    };
    match provider {
        Provider::Anthropic => extract_anthropic_sse(&json),
        Provider::OpenAI => extract_openai_sse(&json),
        _ => extract_openai_sse(&json),
    }
}

fn extract_anthropic(json: &serde_json::Value) -> ExtractedUsage {
    let usage = &json["usage"];
    ExtractedUsage {
        model: json["model"].as_str().map(|s| s.to_string()),
        input_tokens: usage["input_tokens"].as_u64(),
        output_tokens: usage["output_tokens"].as_u64(),
        cache_read_tokens: usage["cache_read_input_tokens"].as_u64(),
        cache_creation_tokens: usage["cache_creation_input_tokens"].as_u64(),
    }
}

fn extract_anthropic_sse(json: &serde_json::Value) -> ExtractedUsage {
    // Anthropic streaming: message_stop event has a preceding message_delta
    // with usage, or the final message object includes usage.
    let usage = &json["usage"];
    ExtractedUsage {
        model: json["model"].as_str().map(|s| s.to_string()),
        input_tokens: usage["input_tokens"].as_u64(),
        output_tokens: usage["output_tokens"].as_u64(),
        cache_read_tokens: usage["cache_read_input_tokens"].as_u64(),
        cache_creation_tokens: usage["cache_creation_input_tokens"].as_u64(),
    }
}

fn extract_openai(json: &serde_json::Value) -> ExtractedUsage {
    let usage = &json["usage"];
    ExtractedUsage {
        model: json["model"].as_str().map(|s| s.to_string()),
        input_tokens: usage["prompt_tokens"].as_u64(),
        output_tokens: usage["completion_tokens"].as_u64(),
        cache_read_tokens: None,
        cache_creation_tokens: None,
    }
}

fn extract_openai_sse(json: &serde_json::Value) -> ExtractedUsage {
    let usage = &json["usage"];
    ExtractedUsage {
        model: json["model"].as_str().map(|s| s.to_string()),
        input_tokens: usage["prompt_tokens"].as_u64(),
        output_tokens: usage["completion_tokens"].as_u64(),
        cache_read_tokens: None,
        cache_creation_tokens: None,
    }
}

fn extract_google(json: &serde_json::Value) -> ExtractedUsage {
    let meta = &json["usageMetadata"];
    ExtractedUsage {
        model: json["modelVersion"].as_str().map(|s| s.to_string()),
        input_tokens: meta["promptTokenCount"].as_u64(),
        output_tokens: meta["candidatesTokenCount"].as_u64(),
        cache_read_tokens: meta["cachedContentTokenCount"].as_u64(),
        cache_creation_tokens: None,
    }
}

// ── Usage log record ─────────────────────────────────────────────────────────

/// A single AI API usage record (one JSONL line in api-usage.jsonl).
#[derive(Serialize)]
pub struct ApiUsageRecord {
    pub ts: String,
    pub provider: Provider,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub endpoint: String,
    pub latency_ms: u64,
    pub request_bytes: u64,
    pub response_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u64>,
    pub status: u16,
    pub streaming: bool,
}

// ── Usage log writer ─────────────────────────────────────────────────────────

/// Thread-safe per-session API usage log writer (JSONL format).
#[derive(Clone)]
pub struct ApiUsageLog(Arc<Mutex<std::fs::File>>);

impl ApiUsageLog {
    pub fn open(session_dir: &Path) -> Result<Self> {
        let path = session_dir.join("api-usage.jsonl");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .wrap_err_with(|| format!("opening API usage log {}", path.display()))?;
        Ok(Self(Arc::new(Mutex::new(file))))
    }

    pub fn write(&self, record: &ApiUsageRecord) -> Result<()> {
        let line = serde_json::to_string(record).wrap_err("serializing API usage record")?;
        let mut file = self.0.lock().unwrap();
        writeln!(file, "{}", line).wrap_err("writing API usage log")
    }
}

// ── Request timing ───────────────────────────────────────────────────────────

/// Tracks the start time of a request for latency calculation.
pub fn start_timer() -> Instant {
    Instant::now()
}

pub fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_provider_map_detects_anthropic() {
        let map = ProviderMap::default();
        assert_eq!(map.lookup("api.anthropic.com"), Some(Provider::Anthropic));
        assert_eq!(map.lookup("api.openai.com"), Some(Provider::OpenAI));
        assert_eq!(map.lookup("unknown.com"), None);
    }

    #[test]
    fn custom_provider_map_adds_hosts() {
        let mut custom = HashMap::new();
        custom.insert("custom".into(), vec!["my-llm.internal".into()]);
        let map = ProviderMap::new(&custom);
        assert_eq!(map.lookup("my-llm.internal"), Some(Provider::Custom));
        // defaults still present
        assert_eq!(map.lookup("api.anthropic.com"), Some(Provider::Anthropic));
    }

    #[test]
    fn extract_anthropic_usage() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "usage": {
                "input_tokens": 1520,
                "output_tokens": 380,
                "cache_read_input_tokens": 1000,
                "cache_creation_input_tokens": 0
            }
        });
        let usage = extract_usage(Provider::Anthropic, body.to_string().as_bytes());
        assert_eq!(usage.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(usage.input_tokens, Some(1520));
        assert_eq!(usage.output_tokens, Some(380));
        assert_eq!(usage.cache_read_tokens, Some(1000));
    }

    #[test]
    fn extract_openai_usage() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "usage": {
                "prompt_tokens": 800,
                "completion_tokens": 200,
                "total_tokens": 1000
            }
        });
        let usage = extract_usage(Provider::OpenAI, body.to_string().as_bytes());
        assert_eq!(usage.model.as_deref(), Some("gpt-4o"));
        assert_eq!(usage.input_tokens, Some(800));
        assert_eq!(usage.output_tokens, Some(200));
    }

    #[test]
    fn extract_google_usage() {
        let body = serde_json::json!({
            "modelVersion": "gemini-2.0-flash",
            "usageMetadata": {
                "promptTokenCount": 500,
                "candidatesTokenCount": 150,
                "cachedContentTokenCount": 100
            }
        });
        let usage = extract_usage(Provider::Google, body.to_string().as_bytes());
        assert_eq!(usage.model.as_deref(), Some("gemini-2.0-flash"));
        assert_eq!(usage.input_tokens, Some(500));
        assert_eq!(usage.output_tokens, Some(150));
        assert_eq!(usage.cache_read_tokens, Some(100));
    }

    #[test]
    fn extract_handles_malformed_body() {
        let usage = extract_usage(Provider::Anthropic, b"not json at all");
        assert!(usage.model.is_none());
        assert!(usage.input_tokens.is_none());
    }

    #[test]
    fn api_usage_record_serializes() {
        let record = ApiUsageRecord {
            ts: "2026-03-25T14:32:12Z".into(),
            provider: Provider::Anthropic,
            model: Some("claude-sonnet-4-20250514".into()),
            endpoint: "/v1/messages".into(),
            latency_ms: 2340,
            request_bytes: 12400,
            response_bytes: 3200,
            input_tokens: Some(1520),
            output_tokens: Some(380),
            cache_read_tokens: Some(1000),
            cache_creation_tokens: None,
            status: 200,
            streaming: false,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"provider\":\"anthropic\""));
        assert!(!json.contains("cache_creation_tokens")); // skipped when None
    }
}
