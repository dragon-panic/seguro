use color_eyre::eyre::{Result, WrapErr};
use std::io::{Seek, Write};
use std::path::Path;

/// Create a NoCloud seed disk image (FAT12, 512 KB) at `path`.
///
/// The disk contains two files expected by cloud-init:
///   - `meta-data`  — instance-id + local-hostname
///   - `user-data`  — #cloud-config that writes the SSH public key and,
///                    optionally, a TLS inspection CA certificate
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
            "instance-id: {}\nlocal-hostname: alpine-seguro\n",
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
    let mut s = String::from("#cloud-config\nwrite_files:\n");

    // SSH authorised key
    s.push_str(&format!(
        "  - path: /home/agent/.ssh/authorized_keys\n    owner: agent:agent\n    permissions: '0600'\n    content: |\n      {}\n",
        pubkey
    ));

    // TLS inspection CA cert (only when --tls-inspect is active)
    if let Some(pem) = ca_cert_pem {
        // Indent each line of the PEM under the YAML `content` block scalar
        let indented: String = pem
            .lines()
            .map(|l| format!("      {}\n", l))
            .collect();
        s.push_str(&format!(
            "  - path: /usr/local/share/ca-certificates/seguro-inspect-ca.crt\n    permissions: '0644'\n    content: |\n{indented}"
        ));
        // Run update-ca-certificates after the files are written
        s.push_str("runcmd:\n  - update-ca-certificates\n");
    }

    s
}
