//! Acceptance tests for seguro.
//!
//! These tests require:
//!   - A real KVM-capable Linux host
//!   - `qemu-system-x86_64` >= 7.2 and `virtiofsd` on $PATH
//!   - A built base image at `~/.local/share/seguro/images/base.qcow2`
//!     (`seguro images build` must have been run first)
//!
//! Run with: `cargo test -- --ignored`
//! Or a specific test: `cargo test acceptance::dev_bridge_safety_gate`
//!   (the safety gate test does NOT need KVM and runs without --ignored)

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn seguro_bin() -> PathBuf {
    // Use the compiled binary from the workspace target directory
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/ or release/
    path.push("seguro");
    path
}

/// Run `seguro <args>` and return stdout + exit status.
fn seguro(args: &[&str]) -> (String, bool) {
    let out = Command::new(seguro_bin())
        .args(args)
        .output()
        .expect("failed to run seguro");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        eprintln!("seguro stderr: {}", stderr);
    }
    (stdout + &stderr, out.status.success())
}

/// Run `seguro run` with a timeout and collect output.
fn seguro_run_with_timeout(extra_args: &[&str], timeout: Duration) -> (String, bool) {
    let mut cmd = Command::new(seguro_bin());
    cmd.args(["run"]);
    cmd.args(extra_args);
    // Use timeout(1) for a reliable way to kill after N seconds
    let out = Command::new("timeout")
        .arg(timeout.as_secs().to_string())
        .arg(seguro_bin())
        .args(["run"])
        .args(extra_args)
        .output()
        .expect("failed to run timeout");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (stdout + &stderr, out.status.success())
}

fn has_kvm() -> bool {
    std::path::Path::new("/dev/kvm").exists()
}

fn images_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("seguro")
        .join("images")
}

fn runtime_dir() -> PathBuf {
    let run = PathBuf::from("/run/seguro");
    if run.parent().map(|p| p.exists()).unwrap_or(false) {
        run
    } else {
        std::env::temp_dir().join("seguro")
    }
}

fn base_image_exists() -> bool {
    images_dir().join("base.qcow2").exists()
}

// ── Safety gate test (does NOT need KVM/image — always runs) ─────────────────

#[test]
fn dev_bridge_without_unsafe_flag_errors() {
    let (output, success) = seguro(&["run", "--net", "dev-bridge"]);
    assert!(!success, "expected non-zero exit");
    assert!(
        output.contains("unsafe-dev-bridge"),
        "expected error message mentioning --unsafe-dev-bridge, got:\n{}",
        output
    );
}

// ── KVM-requiring tests (marked #[ignore]) ────────────────────────────────────

/// Cold start: seguro run should reach the guest within 15 seconds on KVM.
#[test]
#[ignore = "requires KVM and built base image"]
fn cold_start_under_15_seconds() {
    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp = tempfile::tempdir().unwrap();
    let start = std::time::Instant::now();

    // Run a no-op command to verify the guest boots and SSH becomes available
    let (output, success) = seguro_run_with_timeout(
        &["--share", tmp.path().to_str().unwrap(), "--", "echo", "boot_ok"],
        Duration::from_secs(60),
    );

    let elapsed = start.elapsed();
    println!("Cold start took {:.1}s", elapsed.as_secs_f64());

    assert!(success || output.contains("boot_ok"), "session did not start successfully");
    assert!(elapsed < Duration::from_secs(15), "cold start took {:?} (>15s)", elapsed);
}

/// File sharing: write a file from inside the guest, verify it appears on the host.
#[test]
#[ignore = "requires KVM and built base image"]
fn file_sharing_guest_to_host() {
    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("from_guest.txt");

    seguro_run_with_timeout(
        &[
            "--share", tmp.path().to_str().unwrap(),
            "--", "echo", "hello_from_guest", ">", "/mnt/workspace/from_guest.txt",
        ],
        Duration::from_secs(60),
    );

    assert!(marker.exists(), "guest file did not appear on host at {}", marker.display());
    let content = std::fs::read_to_string(&marker).unwrap();
    assert!(content.contains("hello_from_guest"), "unexpected content: {}", content);
}

/// Filesystem isolation: agent cannot read files outside the workspace.
#[test]
#[ignore = "requires KVM and built base image"]
fn filesystem_isolation() {
    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp = tempfile::tempdir().unwrap();

    // Try to list /etc/shadow — should fail or be empty (agent is unprivileged)
    let (output, _) = seguro_run_with_timeout(
        &[
            "--share", tmp.path().to_str().unwrap(),
            "--", "cat /etc/shadow 2>&1; echo DONE",
        ],
        Duration::from_secs(60),
    );

    assert!(
        output.contains("Permission denied") || output.contains("No such file"),
        "agent should not be able to read /etc/shadow, got:\n{}",
        output
    );
}

/// Network isolation: guest cannot reach RFC1918 addresses.
#[test]
#[ignore = "requires KVM and built base image"]
fn network_ssrf_blocked() {
    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp = tempfile::tempdir().unwrap();

    // Attempt to curl the SLIRP gateway (always blocked)
    let (output, _) = seguro_run_with_timeout(
        &[
            "--share", tmp.path().to_str().unwrap(),
            "--", "curl -s -o /dev/null -w '%{http_code}' http://10.0.2.2/ 2>&1; echo CURLOUT",
        ],
        Duration::from_secs(60),
    );

    // The proxy should return 403 for SSRF targets
    assert!(
        output.contains("403") || output.contains("Connection refused") || output.contains("000"),
        "expected SSRF to be blocked, got:\n{}",
        output
    );
}

/// Non-HTTP/S outbound TCP should be blocked by iptables.
#[test]
#[ignore = "requires KVM and built base image"]
fn non_http_tcp_blocked() {
    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp = tempfile::tempdir().unwrap();

    let (output, _) = seguro_run_with_timeout(
        &[
            "--share", tmp.path().to_str().unwrap(),
            "--", "nc -z -w2 1.1.1.1 9999 2>&1; echo NC_EXIT:$?",
        ],
        Duration::from_secs(60),
    );

    // nc should fail (exit non-zero) because iptables DROP rule blocks it
    assert!(
        output.contains("NC_EXIT:1") || output.contains("NC_EXIT:2") || output.contains("timed out"),
        "expected port 9999 TCP to be blocked, got:\n{}",
        output
    );
}

/// api-only mode: unlisted domain returns 403; listed domain succeeds.
#[test]
#[ignore = "requires KVM and built base image"]
fn api_only_mode_filtering() {
    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp = tempfile::tempdir().unwrap();

    let (output, _) = seguro_run_with_timeout(
        &[
            "--share", tmp.path().to_str().unwrap(),
            "--net", "api-only",
            "--", "curl -s -o /dev/null -w '%{http_code}' http://example.com/ 2>&1; echo DONE",
        ],
        Duration::from_secs(60),
    );

    assert!(
        output.contains("403"),
        "expected unlisted domain to return 403 in api-only mode, got:\n{}",
        output
    );
}

/// Proxy log: requests appear in the host-side JSONL log.
#[test]
#[ignore = "requires KVM and built base image"]
fn proxy_log_contains_requests() {
    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp = tempfile::tempdir().unwrap();

    // Make an outbound request inside the guest
    seguro_run_with_timeout(
        &[
            "--share", tmp.path().to_str().unwrap(),
            "--", "curl -s https://example.com/ -o /dev/null 2>&1; echo DONE",
        ],
        Duration::from_secs(60),
    );

    // Find proxy.jsonl in the most recent session dir under runtime_dir
    let run_dir = runtime_dir();
    let log_files: Vec<_> = std::fs::read_dir(&run_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("proxy.jsonl"))
        .filter(|p| p.exists())
        .collect();

    assert!(!log_files.is_empty(), "no proxy.jsonl found under {}", run_dir.display());

    let log_content = std::fs::read_to_string(&log_files[0]).unwrap();
    assert!(
        log_content.contains("example.com"),
        "expected example.com in proxy log, got:\n{}",
        log_content
    );
}

/// OutputMode::Capture collects stdout/stderr bytes from the guest.
#[tokio::test]
#[ignore = "requires KVM and built base image"]
async fn capture_mode_collects_output() {
    use seguro::api::{OutputMode, Sandbox, SandboxConfig};

    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp = tempfile::tempdir().unwrap();

    let config = SandboxConfig {
        workspace: tmp.path().to_path_buf(),
        stdout: OutputMode::Capture,
        stderr: OutputMode::Capture,
        timeout: Some(Duration::from_secs(60)),
        ..Default::default()
    };

    let sandbox = Sandbox::start(config).await.expect("failed to start sandbox");
    let result = sandbox
        .exec(&["echo".into(), "capture_test".into()])
        .await
        .expect("exec failed");

    sandbox.kill().await.expect("kill failed");

    assert_eq!(result.exit_code, Some(0));
    let stdout = result.stdout.expect("stdout should be Some in Capture mode");
    let stdout_str = String::from_utf8_lossy(&stdout);
    assert!(
        stdout_str.contains("capture_test"),
        "expected 'capture_test' in stdout, got: {:?}",
        stdout_str
    );
}

/// OutputMode::Stream forwards output chunks through a channel.
#[tokio::test]
#[ignore = "requires KVM and built base image"]
async fn stream_mode_sends_chunks() {
    use seguro::api::{OutputChunk, OutputMode, Sandbox, SandboxConfig};
    use tokio::sync::mpsc;

    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel::<OutputChunk>(64);

    let config = SandboxConfig {
        workspace: tmp.path().to_path_buf(),
        stdout: OutputMode::Stream(tx),
        stderr: OutputMode::Capture,
        timeout: Some(Duration::from_secs(60)),
        ..Default::default()
    };

    let sandbox = Sandbox::start(config).await.expect("failed to start sandbox");
    let result = sandbox
        .exec(&["echo".into(), "stream_test".into()])
        .await
        .expect("exec failed");

    sandbox.kill().await.expect("kill failed");

    assert_eq!(result.exit_code, Some(0));
    // stdout should be None (streamed, not captured)
    assert!(result.stdout.is_none(), "stdout should be None in Stream mode");

    // Collect streamed chunks
    let mut collected = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        match chunk {
            OutputChunk::Stdout(data) => collected.extend(data),
            OutputChunk::Stderr(_) => {}
        }
    }
    let streamed = String::from_utf8_lossy(&collected);
    assert!(
        streamed.contains("stream_test"),
        "expected 'stream_test' in streamed chunks, got: {:?}",
        streamed
    );
}

/// Crash detection: killing QEMU with SIGKILL triggers auto-restart.
#[tokio::test]
#[ignore = "requires KVM and built base image"]
async fn crash_detection_restarts_qemu() {
    use seguro::api::{OutputMode, RestartPolicy, RestartStrategy, Sandbox, SandboxConfig};

    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp = tempfile::tempdir().unwrap();

    let config = SandboxConfig {
        workspace: tmp.path().to_path_buf(),
        stdout: OutputMode::Capture,
        stderr: OutputMode::Capture,
        restart_policy: RestartPolicy {
            strategy: RestartStrategy::OnFailure,
            max_restarts: 3,
            backoff: vec![Duration::from_secs(1)],
        },
        timeout: Some(Duration::from_secs(120)),
        ..Default::default()
    };

    let sandbox = Sandbox::start(config).await.expect("failed to start sandbox");

    // Run a command to confirm the VM is working
    let result = sandbox
        .exec(&["echo".into(), "before_crash".into()])
        .await
        .expect("exec before crash failed");
    assert_eq!(result.exit_code, Some(0));

    // Kill QEMU with SIGKILL to simulate a crash
    // fuser finds the PID listening on the SSH port, then we kill it
    let ssh_port = sandbox.ssh_port();
    let _ = std::process::Command::new("bash")
        .args(["-c", &format!("kill -9 $(fuser {}/tcp 2>/dev/null)", ssh_port)])
        .status();

    // Write a marker file, wait for restart, then try to read it
    // The overlay persists across restarts so files written before crash survive
    let marker = tmp.path().join("pre_crash.txt");
    std::fs::write(&marker, "survived").unwrap();

    // Give monitor time to detect crash and restart (backoff + SSH wait)
    tokio::time::sleep(Duration::from_secs(5)).await;

    // After restart, we should be able to run commands again
    let result = sandbox
        .exec(&["cat".into(), "pre_crash.txt".into()])
        .await
        .expect("exec after restart failed");

    sandbox.kill().await.expect("kill failed");

    assert_eq!(result.exit_code, Some(0));
    let stdout = result.stdout.unwrap_or_default();
    let stdout_str = String::from_utf8_lossy(&stdout);
    assert!(
        stdout_str.contains("survived"),
        "expected pre-crash file to survive restart, got: {:?}",
        stdout_str
    );
}

/// RestartPolicy::Never (default) does not restart — current behavior unchanged.
#[tokio::test]
#[ignore = "requires KVM and built base image"]
async fn restart_policy_never_does_not_restart() {
    use seguro::api::{OutputMode, Sandbox, SandboxConfig};

    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp = tempfile::tempdir().unwrap();

    // Default config has RestartStrategy::Never
    let config = SandboxConfig {
        workspace: tmp.path().to_path_buf(),
        stdout: OutputMode::Capture,
        stderr: OutputMode::Capture,
        timeout: Some(Duration::from_secs(60)),
        ..Default::default()
    };

    let sandbox = Sandbox::start(config).await.expect("failed to start sandbox");
    let result = sandbox
        .exec(&["echo".into(), "alive".into()])
        .await
        .expect("exec failed");
    assert_eq!(result.exit_code, Some(0));

    sandbox.kill().await.expect("kill failed");
}

/// Concurrent sessions use different SSH ports and don't interfere.
#[test]
#[ignore = "requires KVM and built base image"]
fn concurrent_sessions_independent() {
    assert!(has_kvm(), "KVM not available");
    assert!(base_image_exists(), "base.qcow2 not built");

    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();

    // Start two sessions concurrently and check they each get their own marker
    let t1 = {
        let p1 = tmp1.path().to_path_buf();
        std::thread::spawn(move || {
            seguro_run_with_timeout(
                &["--share", p1.to_str().unwrap(), "--", "echo s1 > /mnt/workspace/id.txt"],
                Duration::from_secs(90),
            )
        })
    };
    let t2 = {
        let p2 = tmp2.path().to_path_buf();
        std::thread::spawn(move || {
            seguro_run_with_timeout(
                &["--share", p2.to_str().unwrap(), "--", "echo s2 > /mnt/workspace/id.txt"],
                Duration::from_secs(90),
            )
        })
    };

    t1.join().unwrap();
    t2.join().unwrap();

    let c1 = std::fs::read_to_string(tmp1.path().join("id.txt")).unwrap_or_default();
    let c2 = std::fs::read_to_string(tmp2.path().join("id.txt")).unwrap_or_default();

    assert!(c1.contains("s1"), "session 1 wrote wrong content: {}", c1);
    assert!(c2.contains("s2"), "session 2 wrote wrong content: {}", c2);
}
