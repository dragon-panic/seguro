use color_eyre::eyre::{Result, WrapErr, eyre};
use std::path::{Path, PathBuf};

/// Locate the base image to use for a session.
///
/// Search order:
///   1. `config_override` if provided
///   2. `~/.local/share/seguro/images/base[-{suffix}].qcow2`
///
/// The `image_suffix` comes from the resolved profile config.
/// `None` → `base.qcow2`, `Some("browser")` → `base-browser.qcow2`, etc.
pub fn locate_base(image_suffix: Option<&str>, config_override: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = config_override {
        if p.exists() {
            return Ok(p.to_path_buf());
        }
        return Err(eyre!("configured base image not found: {}", p.display()));
    }

    let name = image_name(image_suffix);
    let path = crate::config::images_dir().join(&name);
    if path.exists() {
        Ok(path)
    } else {
        let hint = match image_suffix {
            Some(s) => format!(" --profile {s}"),
            None => String::new(),
        };
        Err(eyre!(
            "base image not found at {}.\n\
             Run `seguro images build{hint}` to create it.",
            path.display(),
        ))
    }
}

/// Returns the image filename for a given profile suffix.
/// `None` → `base.qcow2`, `Some("browser")` → `base-browser.qcow2`.
pub fn image_name(suffix: Option<&str>) -> String {
    match suffix {
        Some(s) => format!("base-{s}.qcow2"),
        None => "base.qcow2".into(),
    }
}

/// Create a qcow2 copy-on-write overlay on top of `base`.
pub async fn create_overlay(base: &Path, overlay: &Path) -> Result<()> {
    let status = tokio::process::Command::new("qemu-img")
        .args([
            "create", "-q",
            "-f", "qcow2",
            "-b", base.to_str().ok_or_else(|| eyre!("non-UTF8 path"))?,
            "-F", "qcow2",
            overlay.to_str().ok_or_else(|| eyre!("non-UTF8 path"))?,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .wrap_err("launching qemu-img create")?;

    if !status.success() {
        return Err(eyre!("qemu-img create overlay failed ({})", status));
    }
    Ok(())
}

/// Save a snapshot of the running disk image.
///
/// Calls `qemu-img snapshot -c <name> <image>`.
pub async fn snapshot_save(image: &Path, name: &str) -> Result<()> {
    let status = tokio::process::Command::new("qemu-img")
        .args(["snapshot", "-c", name, image.to_str().unwrap()])
        .status()
        .await
        .wrap_err("launching qemu-img snapshot -c")?;

    if !status.success() {
        return Err(eyre!("qemu-img snapshot save '{}' failed ({})", name, status));
    }
    Ok(())
}

/// Restore a named snapshot into `target_overlay` from `base`.
///
/// Creates a fresh overlay then applies the snapshot.
pub async fn snapshot_restore(base: &Path, name: &str, target_overlay: &Path) -> Result<()> {
    create_overlay(base, target_overlay).await?;

    let status = tokio::process::Command::new("qemu-img")
        .args(["snapshot", "-a", name, target_overlay.to_str().unwrap()])
        .status()
        .await
        .wrap_err("launching qemu-img snapshot -a")?;

    if !status.success() {
        return Err(eyre!("qemu-img snapshot restore '{}' failed ({})", name, status));
    }
    Ok(())
}

/// List all *.qcow2 files in `images_dir` with their on-disk sizes.
pub fn list_images(images_dir: &Path) -> Result<Vec<(PathBuf, u64)>> {
    let mut out = Vec::new();
    if !images_dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(images_dir).wrap_err("reading images dir")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("qcow2") {
            let size = entry.metadata()?.len();
            out.push((path, size));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Liveness state of a session directory.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionState {
    /// QEMU PID is dead or missing — safe to remove.
    Dead,
    /// QEMU PID is alive but the guest is unreachable (SSH banner check failed).
    Zombie,
    /// QEMU PID is alive and guest is reachable.
    Alive,
}

/// A session directory with its assessed liveness state.
#[derive(Debug)]
pub struct SessionInfo {
    pub dir: PathBuf,
    pub pid: Option<i32>,
    pub ssh_port: Option<u16>,
    pub state: SessionState,
    /// The managing seguro process is no longer running.
    /// True when `seguro.pid` is absent or the PID is dead.
    pub orphaned: bool,
    /// When the session directory was created.
    pub created: Option<std::time::SystemTime>,
}

/// Scan `runtime_dir` and classify every session directory by liveness.
///
/// A session dir is any subdirectory of `runtime_dir`.  Classification:
///   - **Dead**: no `qemu.pid`, or the PID is not a running QEMU process.
///   - **Zombie**: QEMU PID is alive but the guest SSH port is unreachable
///     (TCP connect + SSH banner check fails within `ssh_timeout`).
///   - **Alive**: QEMU PID is alive and guest SSH responds.
///
/// Each session is also checked for orphan status: if `seguro.pid` is absent
/// or the managing process is no longer running, `orphaned` is set to `true`.
pub fn classify_sessions(runtime_dir: &Path, ssh_timeout: std::time::Duration) -> Result<Vec<SessionInfo>> {
    let mut sessions = Vec::new();
    if !runtime_dir.exists() {
        return Ok(sessions);
    }
    for entry in std::fs::read_dir(runtime_dir)? {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        let pid = read_pid(&dir);
        let ssh_port = read_ssh_port(&dir);
        let qemu_alive = pid.is_some_and(is_qemu_pid_alive);

        let state = if !qemu_alive {
            SessionState::Dead
        } else if let Some(port) = ssh_port {
            if is_guest_reachable(port, ssh_timeout) {
                SessionState::Alive
            } else {
                SessionState::Zombie
            }
        } else {
            // QEMU alive but no ssh.port file — treat as zombie
            SessionState::Zombie
        };

        let orphaned = is_seguro_pid_dead(&dir);
        let created = dir_created_time(&dir);

        sessions.push(SessionInfo { dir, pid, ssh_port, state, orphaned, created });
    }
    Ok(sessions)
}

/// List session dirs that are dead (QEMU not running).
///
/// Kept for backward-compat with callers that don't need the full classification.
pub fn list_orphaned_sessions(runtime_dir: &Path) -> Result<Vec<PathBuf>> {
    let sessions = classify_sessions(runtime_dir, std::time::Duration::from_secs(3))?;
    Ok(sessions
        .into_iter()
        .filter(|s| s.state == SessionState::Dead)
        .map(|s| s.dir)
        .collect())
}

fn read_pid(session_dir: &Path) -> Option<i32> {
    let content = std::fs::read_to_string(session_dir.join("qemu.pid")).ok()?;
    content.trim().parse().ok()
}

fn read_ssh_port(session_dir: &Path) -> Option<u16> {
    let content = std::fs::read_to_string(session_dir.join("ssh.port")).ok()?;
    content.trim().parse().ok()
}

pub fn is_qemu_pid_alive(pid: i32) -> bool {
    let comm_path = format!("/proc/{}/comm", pid);
    std::fs::read_to_string(comm_path)
        .map(|s| s.trim().starts_with("qemu-system"))
        .unwrap_or(false)
}

/// Check whether the managing seguro process (from `seguro.pid`) is dead or missing.
///
/// Returns `true` (orphaned) if:
///   - `seguro.pid` doesn't exist (legacy session, no managing process recorded)
///   - The PID is no longer a running process
fn is_seguro_pid_dead(session_dir: &Path) -> bool {
    let Some(pid) = read_seguro_pid(session_dir) else {
        return true; // no seguro.pid file → orphaned
    };
    // Check if process exists (signal 0 = no signal, just check)
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err()
}

fn read_seguro_pid(session_dir: &Path) -> Option<i32> {
    let content = std::fs::read_to_string(session_dir.join("seguro.pid")).ok()?;
    content.trim().parse().ok()
}

/// Get directory creation time (falls back to mtime).
fn dir_created_time(dir: &Path) -> Option<std::time::SystemTime> {
    let meta = std::fs::metadata(dir).ok()?;
    // Try birth time first, fall back to mtime
    meta.created().or_else(|_| meta.modified()).ok()
}

/// Check whether the guest is reachable by attempting a TCP connect to the SSH
/// port and reading the SSH banner.  Returns `false` on timeout or connection
/// failure.
fn is_guest_reachable(ssh_port: u16, timeout: std::time::Duration) -> bool {
    use std::io::Read;
    use std::net::{SocketAddr, TcpStream};

    let addr = SocketAddr::from(([127, 0, 0, 1], ssh_port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    stream.set_read_timeout(Some(timeout)).ok();
    let mut buf = [0u8; 8];
    match stream.read(&mut buf) {
        Ok(n) if n >= 4 => buf[..4] == *b"SSH-",
        _ => false,
    }
}

/// Kill a QEMU process: SIGTERM, brief wait, SIGKILL if still alive.
///
/// Blocks for up to ~2s waiting for graceful shutdown before escalating to
/// SIGKILL.
pub fn kill_qemu_pid(pid: i32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let nix_pid = Pid::from_raw(pid);

    // SIGTERM first
    let _ = kill(nix_pid, Signal::SIGTERM);

    // Poll up to 2s for exit
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !is_qemu_pid_alive(pid) {
            return;
        }
    }

    // SIGKILL
    let _ = kill(nix_pid, Signal::SIGKILL);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Helper: create a fake session dir with optional qemu.pid and ssh.port files.
    fn make_session(
        parent: &Path,
        name: &str,
        pid: Option<&str>,
        ssh_port: Option<&str>,
    ) -> PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(p) = pid {
            std::fs::write(dir.join("qemu.pid"), p).unwrap();
        }
        if let Some(port) = ssh_port {
            std::fs::write(dir.join("ssh.port"), port).unwrap();
        }
        dir
    }

    #[test]
    fn classify_empty_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = classify_sessions(tmp.path(), Duration::from_millis(100)).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn classify_nonexistent_dir_returns_empty() {
        let sessions = classify_sessions(
            Path::new("/tmp/seguro-test-nonexistent-dir"),
            Duration::from_millis(100),
        ).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn session_with_no_pid_file_is_dead() {
        let tmp = tempfile::tempdir().unwrap();
        make_session(tmp.path(), "sess-1", None, Some("22222"));

        let sessions = classify_sessions(tmp.path(), Duration::from_millis(100)).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, SessionState::Dead);
        assert_eq!(sessions[0].pid, None);
    }

    #[test]
    fn session_with_bogus_pid_is_dead() {
        let tmp = tempfile::tempdir().unwrap();
        // PID 999999999 almost certainly doesn't exist
        make_session(tmp.path(), "sess-2", Some("999999999"), Some("22222"));

        let sessions = classify_sessions(tmp.path(), Duration::from_millis(100)).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, SessionState::Dead);
        assert_eq!(sessions[0].pid, Some(999999999));
    }

    #[test]
    fn session_with_garbage_pid_file_is_dead() {
        let tmp = tempfile::tempdir().unwrap();
        make_session(tmp.path(), "sess-3", Some("not-a-number"), Some("22222"));

        let sessions = classify_sessions(tmp.path(), Duration::from_millis(100)).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, SessionState::Dead);
        assert_eq!(sessions[0].pid, None);
    }

    #[test]
    fn non_dir_entries_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a regular file (not a directory) in the runtime dir
        std::fs::write(tmp.path().join("stray-file.txt"), "hello").unwrap();
        make_session(tmp.path(), "real-session", None, None);

        let sessions = classify_sessions(tmp.path(), Duration::from_millis(100)).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].dir.file_name().unwrap().to_str().unwrap(),
            "real-session",
        );
    }

    #[test]
    fn session_without_overlay_is_still_detected() {
        let tmp = tempfile::tempdir().unwrap();
        // No session.qcow2 — old code would miss this
        make_session(tmp.path(), "no-overlay", Some("999999999"), Some("22222"));

        let sessions = classify_sessions(tmp.path(), Duration::from_millis(100)).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, SessionState::Dead);
    }

    #[test]
    fn list_orphaned_sessions_returns_only_dead() {
        let tmp = tempfile::tempdir().unwrap();
        make_session(tmp.path(), "dead-1", Some("999999999"), None);
        make_session(tmp.path(), "dead-2", None, None);

        let orphans = list_orphaned_sessions(tmp.path()).unwrap();
        assert_eq!(orphans.len(), 2);
    }

    #[test]
    fn is_qemu_pid_alive_rejects_nonexistent_pid() {
        assert!(!is_qemu_pid_alive(999999999));
    }

    #[test]
    fn is_guest_reachable_returns_false_for_unbound_port() {
        // Port 1 is privileged and almost certainly not listening
        assert!(!is_guest_reachable(1, Duration::from_millis(100)));
    }

    #[test]
    fn session_without_seguro_pid_is_orphaned() {
        let tmp = tempfile::tempdir().unwrap();
        // No seguro.pid file → orphaned
        make_session(tmp.path(), "no-manager", Some("999999999"), None);

        let sessions = classify_sessions(tmp.path(), Duration::from_millis(100)).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].orphaned);
    }

    #[test]
    fn session_with_dead_seguro_pid_is_orphaned() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session(tmp.path(), "dead-manager", Some("999999999"), None);
        // Write a bogus seguro.pid (PID that doesn't exist)
        std::fs::write(dir.join("seguro.pid"), "999999998").unwrap();

        let sessions = classify_sessions(tmp.path(), Duration::from_millis(100)).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].orphaned);
    }

    #[test]
    fn session_with_live_seguro_pid_is_not_orphaned() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session(tmp.path(), "live-manager", Some("999999999"), None);
        // Write our own PID as the seguro.pid — we're alive
        std::fs::write(dir.join("seguro.pid"), std::process::id().to_string()).unwrap();

        let sessions = classify_sessions(tmp.path(), Duration::from_millis(100)).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(!sessions[0].orphaned);
    }

    #[test]
    fn session_has_created_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        make_session(tmp.path(), "timestamped", None, None);

        let sessions = classify_sessions(tmp.path(), Duration::from_millis(100)).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].created.is_some());
    }
}
