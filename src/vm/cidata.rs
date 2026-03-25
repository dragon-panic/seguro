use color_eyre::eyre::{Result, WrapErr};
use std::io::{Seek, Write};
use std::path::Path;

/// Create a NoCloud seed disk image (FAT12, 512 KB) at `path`.
///
/// The disk contains two files expected by cloud-init:
///   - `meta-data`  — instance-id + local-hostname
///   - `user-data`  — #cloud-config that injects the SSH public key into the
///                    agent user and, optionally, installs a TLS inspection CA
pub fn create_cidata_seed(
    session_id: &str,
    pubkey: &str,
    ca_cert_pem: Option<&str>,
    path: &Path,
) -> Result<()> {
    const DISK_SIZE: usize = 512 * 1024; // 512 KB

    let mut buf = vec![0u8; DISK_SIZE];

    {
        let cursor = std::io::Cursor::new(&mut buf[..]);
        let options = fatfs::FormatVolumeOptions::new()
            .volume_label(*b"cidata     "); // exactly 11 bytes
        fatfs::format_volume(cursor, options).wrap_err("formatting FAT volume")?;
    }

    {
        let cursor = std::io::Cursor::new(&mut buf[..]);
        let fs = fatfs::FileSystem::new(cursor, fatfs::FsOptions::new())
            .wrap_err("opening FAT filesystem")?;
        let root = fs.root_dir();

        // meta-data
        let meta = format!(
            "instance-id: {}\nlocal-hostname: seguro-guest\n",
            session_id
        );
        let mut f = root.create_file("meta-data").wrap_err("creating meta-data")?;
        f.seek(std::io::SeekFrom::Start(0)).wrap_err("seeking meta-data")?;
        f.write_all(meta.as_bytes()).wrap_err("writing meta-data")?;

        // user-data
        let user_data = build_user_data(pubkey.trim(), ca_cert_pem);
        let mut f = root.create_file("user-data").wrap_err("creating user-data")?;
        f.seek(std::io::SeekFrom::Start(0)).wrap_err("seeking user-data")?;
        f.write_all(user_data.as_bytes()).wrap_err("writing user-data")?;
    }

    std::fs::write(path, &buf).wrap_err("writing cidata disk image")?;
    Ok(())
}

fn build_user_data(pubkey: &str, ca_cert_pem: Option<&str>) -> String {
    // Use the cloud-init `users:` stanza to inject the SSH key into the agent
    // user that was created during the image build phase.  cloud-init's
    // cc_users_groups module updates ssh_authorized_keys on every boot when the
    // instance-id changes (which it does — each session gets a fresh UUID).
    //
    // NOTE: do NOT use Rust line-continuation (\) inside the string — it strips
    // all leading whitespace from the next line, destroying YAML indentation.
    let mut s = String::new();
    s.push_str("#cloud-config\n");
    s.push_str("users:\n");
    s.push_str("  - name: agent\n");
    s.push_str("    shell: /bin/bash\n");
    s.push_str("    lock_passwd: true\n");
    s.push_str("    ssh_authorized_keys:\n");
    s.push_str(&format!("      - {}\n", pubkey));

    // write_files: always needed for iptables sudoers, optionally for TLS CA cert
    s.push_str("write_files:\n");
    // Allow agent to run iptables via sudo (for network isolation rules)
    s.push_str("  - path: /etc/sudoers.d/agent-iptables\n");
    s.push_str("    permissions: '0440'\n");
    s.push_str("    content: |\n");
    s.push_str("      agent ALL=(root) NOPASSWD: /usr/sbin/iptables, /usr/bin/mkdir, /usr/bin/mount\n");

    // TLS inspection CA cert (only when --tls-inspect is active)
    if let Some(pem) = ca_cert_pem {
        let indented: String = pem
            .lines()
            .map(|l| format!("      {}\n", l))
            .collect();
        s.push_str("  - path: /usr/local/share/ca-certificates/seguro-inspect-ca.crt\n");
        s.push_str("    permissions: '0644'\n");
        s.push_str("    content: |\n");
        s.push_str(&indented);
        s.push_str("runcmd:\n");
        s.push_str("  - update-ca-certificates\n");
    }

    s
}
