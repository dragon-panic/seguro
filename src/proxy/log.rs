use color_eyre::eyre::{Result, WrapErr};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A single proxy request record written to the per-session JSONL log.
#[derive(Serialize)]
pub struct RequestRecord {
    pub ts: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: Option<u16>,
    pub bytes: Option<u64>,
    pub blocked: bool,
    pub block_reason: Option<String>,
}

/// Writes request records to a per-session JSONL file.
pub struct RequestLog {
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
        Ok(Self { path, file })
    }

    pub fn write(&mut self, record: &RequestRecord) -> Result<()> {
        let line = serde_json::to_string(record)
            .wrap_err("serializing request record")?;
        writeln!(self.file, "{}", line).wrap_err("writing proxy log")?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
