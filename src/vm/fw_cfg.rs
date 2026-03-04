use color_eyre::eyre::Result;

use crate::vm::QemuParams;

/// Build the -fw_cfg arguments to pass per-session data into the guest.
///
/// Data passed:
///   opt/seguro/authorized_key  — SSH public key (read by rc.local → authorized_keys)
///   opt/seguro/env/<VAR>       — environment variables injected into agent's session
pub fn build_args(params: &QemuParams) -> Result<Vec<String>> {
    let mut args = Vec::new();

    // SSH public key
    // Embedded as a string value so no temp file is needed.
    let key_escaped = params.ssh_pubkey.trim().replace(',', "\\,");
    args.push("-fw_cfg".to_string());
    args.push(format!("name=opt/seguro/authorized_key,string={}", key_escaped));

    // Environment variables
    for (k, v) in &params.env_vars {
        let v_escaped = v.replace(',', "\\,");
        args.push("-fw_cfg".to_string());
        args.push(format!("name=opt/seguro/env/{},string={}", k, v_escaped));
    }

    Ok(args)
}
