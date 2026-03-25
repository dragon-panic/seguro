use color_eyre::eyre::{Result, eyre};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::cli::ApiUsageArgs;
use crate::config::runtime_dir;

pub async fn execute(args: ApiUsageArgs) -> Result<()> {
    let session_id = resolve_session(args.session_id)?;
    let log_path = runtime_dir().join(&session_id).join("api-usage.jsonl");

    if !log_path.exists() {
        return Err(eyre!(
            "API usage log not found: {}\n\
             The session may not have --tls-inspect enabled, or no AI API requests have been made yet.",
            log_path.display()
        ));
    }

    println!(
        "AI API usage for session {}  (Ctrl+C to stop)\n",
        session_id
    );
    println!(
        "{:<22} {:<12} {:<30} {:>8} {:>8} {:>8} {:>9} {:>6}",
        "TIME", "PROVIDER", "MODEL", "IN_TOK", "OUT_TOK", "CACHE", "LATENCY", "STATUS"
    );
    println!("{}", "\u{2500}".repeat(115));

    tail_usage(&log_path).await
}

async fn tail_usage(path: &std::path::Path) -> Result<()> {
    use std::time::Duration;
    use tokio::fs::File;

    let file = File::open(path).await?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();

    let mut total_in: u64 = 0;
    let mut total_out: u64 = 0;
    let mut total_cache: u64 = 0;
    let mut count: u64 = 0;

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // Print running totals then wait
            if count > 0 {
                print_totals(count, total_in, total_out, total_cache);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(r) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let ts = r["ts"].as_str().unwrap_or("-");
            let provider = r["provider"].as_str().unwrap_or("-");
            let model = r["model"].as_str().unwrap_or("-");
            let in_tok = r["input_tokens"].as_u64();
            let out_tok = r["output_tokens"].as_u64();
            let cache = r["cache_read_tokens"].as_u64();
            let latency = r["latency_ms"].as_u64().unwrap_or(0);
            let status = r["status"].as_u64().unwrap_or(0);

            total_in += in_tok.unwrap_or(0);
            total_out += out_tok.unwrap_or(0);
            total_cache += cache.unwrap_or(0);
            count += 1;

            let status_str = format!("{}", status);
            let status_colored = if status >= 400 {
                format!("\x1b[31m{:>6}\x1b[0m", status_str)
            } else {
                format!("{:>6}", status_str)
            };

            println!(
                "{:<22} {:<12} {:<30} {:>8} {:>8} {:>8} {:>7}ms {}",
                &ts[..ts.len().min(22)],
                provider,
                &model[..model.len().min(30)],
                fmt_tok(in_tok),
                fmt_tok(out_tok),
                fmt_tok(cache),
                latency,
                status_colored,
            );
        } else {
            println!("{}", trimmed);
        }
    }
}

fn fmt_tok(v: Option<u64>) -> String {
    match v {
        Some(n) => format!("{}", n),
        None => "-".into(),
    }
}

fn print_totals(count: u64, total_in: u64, total_out: u64, total_cache: u64) {
    // Move cursor up past the totals line if we printed one before, then reprint
    print!("\x1b[2K\r");
    print!(
        "\x1b[90m{} requests | input: {} | output: {} | cache_read: {} | total: {}\x1b[0m",
        count,
        total_in,
        total_out,
        total_cache,
        total_in + total_out,
    );
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn resolve_session(explicit: Option<String>) -> Result<String> {
    if let Some(id) = explicit {
        return Ok(id);
    }

    let run_dir = runtime_dir();
    if !run_dir.exists() {
        return Err(eyre!("no active sessions"));
    }

    // Look for sessions that have api-usage.jsonl
    let sessions: Vec<String> = std::fs::read_dir(&run_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir() && e.path().join("api-usage.jsonl").exists())
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();

    match sessions.len() {
        0 => {
            // Fall back to sessions with proxy.jsonl
            let proxy_sessions: Vec<String> = std::fs::read_dir(&run_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir() && e.path().join("proxy.jsonl").exists())
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .collect();

            if proxy_sessions.is_empty() {
                Err(eyre!("no sessions found"))
            } else {
                Err(eyre!(
                    "no sessions have API usage logs (is --tls-inspect enabled?)\n\
                     Found {} session(s) with proxy logs but no api-usage.jsonl.",
                    proxy_sessions.len()
                ))
            }
        }
        1 => Ok(sessions.into_iter().next().unwrap()),
        _ => Err(eyre!(
            "multiple sessions with API usage: {}\nSpecify SESSION_ID explicitly.",
            sessions.join(", ")
        )),
    }
}
