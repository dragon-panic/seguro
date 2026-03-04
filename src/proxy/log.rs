use color_eyre::eyre::{Result, WrapErr};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A single proxy request record (one JSONL line).
#[derive(Serialize)]
pub struct RequestRecord {
    pub ts: String,
    pub method: String,
    pub host: String,
    /// Path component. Redacted to "-" for HTTPS tunnels without TLS inspection.
    pub path: String,
    pub status: Option<u16>,
    pub bytes: Option<u64>,
    pub blocked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
}

impl RequestRecord {
    pub fn now() -> String {
        // RFC 3339 timestamp — use std since we can't add chrono dependency
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Minimal ISO-8601 UTC formatting without chrono
        format_unix_ts(secs)
    }
}

fn format_unix_ts(secs: u64) -> String {
    // Very small ISO-8601 UTC formatter (avoids chrono dependency)
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400; // days since 1970-01-01
    let (y, mo, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, m, s)
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Gregorian calendar calculation
    let mut y = 1970u64;
    loop {
        let leap = is_leap(y);
        let dy = if leap { 366 } else { 365 };
        if days < dy { break; }
        days -= dy;
        y += 1;
    }
    let leap = is_leap(y);
    let months = [31u64, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut mo = 1u64;
    for &dm in &months {
        if days < dm { break; }
        days -= dm;
        mo += 1;
    }
    (y, mo, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Thread-safe per-session request log writer (JSONL format).
#[derive(Clone)]
pub struct RequestLog(Arc<Mutex<Inner>>);

struct Inner {
    path: PathBuf,
    file: std::fs::File,
}

impl RequestLog {
    pub fn open(session_dir: &Path) -> Result<Self> {
        let path = session_dir.join("proxy.jsonl");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .wrap_err_with(|| format!("opening proxy log {}", path.display()))?;
        Ok(Self(Arc::new(Mutex::new(Inner { path, file }))))
    }

    pub fn write(&self, record: &RequestRecord) -> Result<()> {
        let line = serde_json::to_string(record).wrap_err("serializing record")?;
        let mut inner = self.0.lock().unwrap();
        writeln!(inner.file, "{}", line).wrap_err("writing proxy log")
    }

    pub fn path(&self) -> PathBuf {
        self.0.lock().unwrap().path.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_format_looks_right() {
        // 2024-01-15T00:00:00Z = 1705276800
        let ts = format_unix_ts(1705276800);
        assert_eq!(ts, "2024-01-15T00:00:00Z");
    }

    #[test]
    fn record_serializes_to_json() {
        let r = RequestRecord {
            ts: "2024-01-01T00:00:00Z".into(),
            method: "CONNECT".into(),
            host: "api.anthropic.com".into(),
            path: "-".into(),
            status: Some(200),
            bytes: None,
            blocked: false,
            block_reason: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("api.anthropic.com"));
        assert!(!json.contains("block_reason")); // skipped when None
    }
}
