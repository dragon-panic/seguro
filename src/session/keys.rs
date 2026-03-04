use color_eyre::eyre::{Result, WrapErr};
use rand::rngs::OsRng;
use ssh_key::{Algorithm, LineEnding, PrivateKey};
use std::path::Path;

/// Generate an ephemeral ed25519 key pair and write to `path` (private) and
/// `path`.pub (public in OpenSSH authorized_keys format).
pub async fn generate(path: &Path) -> Result<()> {
    let private = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)
        .wrap_err("generating ed25519 key")?;

    let private_pem = private
        .to_openssh(LineEnding::LF)
        .wrap_err("serializing private key")?;

    let public_str = private
        .public_key()
        .to_openssh()
        .wrap_err("serializing public key")?;

    std::fs::write(path, private_pem.as_bytes()).wrap_err("writing private key")?;
    // Restrict permissions to owner-read-only
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    let pub_path = path.with_extension("pub");
    std::fs::write(&pub_path, public_str.as_bytes()).wrap_err("writing public key")?;

    Ok(())
}

/// Read the public key from `private_key_path`.pub in OpenSSH authorized_keys format.
pub fn public_key_string(private_key_path: &Path) -> Result<String> {
    let pub_path = private_key_path.with_extension("pub");
    std::fs::read_to_string(&pub_path)
        .wrap_err_with(|| format!("reading {}", pub_path.display()))
}
