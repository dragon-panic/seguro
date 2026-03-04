use color_eyre::eyre::{Result, eyre};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::cli::ProxyLogArgs;
use crate::config::runtime_dir;

pub async fn execute(args: ProxyLogArgs) -> Result<()> {
    let session_id = resolve_session(args.session_id)?;
    let log_path = runtime_dir().join(&session_id).join("proxy.jsonl");

    if !log_path.exists() {
        return Err(eyre!(
            "proxy log not found: {}\n\
             The session may not have started yet or the proxy has not received any requests.",
            log_path.display()
        ));
    }

    println!("Tailing proxy log for session {}  (Ctrl+C to stop)\n", session_id);
    println!("{:<25} {:<8} {:<40} {}", "TIMESTAMP", "STATUS", "HOST", "PATH");
    println!("{}", "-".repeat(100));

    tail_jsonl(&log_path).await
}

async fn tail_jsonl(path: &std::path::Path) -> Result<()> {
    use tokio::fs::File;
    use std::time::Duration;

    let file = File::open(path).await?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // No new data — wait and retry (tail -f behaviour)
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(record) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let ts = record["ts"].as_str().unwrap_or("-");
            let host = record["host"].as_str().unwrap_or("-");
            let path = record["path"].as_str().unwrap_or("-");
            let status = record["status"]
                .as_u64()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".into());
            let blocked = record["blocked"].as_bool().unwrap_or(false);

            let status_colored = if blocked || status.starts_with('4') || status.starts_with('5') {
                format!("\x1b[31m{:<8}\x1b[0m", status) // red
            } else {
                format!("{:<8}", status)
            };

            println!(
                "{:<25} {} {:<40} {}",
                &ts[..ts.len().min(25)],
                status_colored,
                &host[..host.len().min(40)],
                &path[..path.len().min(60)],
            );
        } else {
            // Not valid JSON — print raw
            println!("{}", trimmed);
        }
    }
}

fn resolve_session(explicit: Option<String>) -> Result<String> {
    if let Some(id) = explicit {
        return Ok(id);
    }

    let run_dir = runtime_dir();
    if !run_dir.exists() {
        return Err(eyre!("no active sessions"));
    }

    let sessions: Vec<String> = std::fs::read_dir(&run_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir() && e.path().join("proxy.jsonl").exists())
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();

    match sessions.len() {
        0 => Err(eyre!("no proxy logs found")),
        1 => Ok(sessions.into_iter().next().unwrap()),
        _ => Err(eyre!(
            "multiple sessions: {}\nSpecify SESSION_ID explicitly.",
            sessions.join(", ")
        )),
    }
}
