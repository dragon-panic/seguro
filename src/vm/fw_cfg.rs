use color_eyre::eyre::Result;

/// Build the `-fw_cfg` arguments to pass per-session data into the guest.
///
/// Keys injected:
///   `opt/seguro/authorized_key`  — SSH public key (read by rc.local)
///   `opt/seguro/env/<VAR>`       — env vars for the agent session
pub fn build_args(ssh_pubkey: &str, env_vars: &[(String, String)]) -> Result<Vec<String>> {
    let mut args = Vec::new();

    // Inline the public key as a string value (avoids temp file requirement).
    // Commas must be escaped as \, in QEMU's fw_cfg string syntax.
    let key_escaped = ssh_pubkey.trim().replace(',', "\\,");
    args.push("-fw_cfg".to_string());
    args.push(format!("name=opt/seguro/authorized_key,string={}", key_escaped));

    // Inject each env var as its own fw_cfg entry
    for (k, v) in env_vars {
        let v_escaped = v.replace(',', "\\,");
        args.push("-fw_cfg".to_string());
        args.push(format!("name=opt/seguro/env/{},string={}", k, v_escaped));
    }

    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_authorized_key_arg() {
        let args = build_args("ssh-ed25519 AAAA comment", &[]).unwrap();
        assert_eq!(args[0], "-fw_cfg");
        assert!(args[1].starts_with("name=opt/seguro/authorized_key,string=ssh-ed25519 AAAA"));
    }

    #[test]
    fn generates_env_var_args() {
        let env = vec![("ANTHROPIC_API_KEY".into(), "sk-test".into())];
        let args = build_args("ssh-ed25519 AAAA", &env).unwrap();
        assert!(args.iter().any(|a| a.contains("opt/seguro/env/ANTHROPIC_API_KEY")));
    }

    #[test]
    fn escapes_commas_in_value() {
        let env = vec![("FOO".into(), "a,b,c".into())];
        let args = build_args("key", &env).unwrap();
        let val_arg = args.iter().find(|a| a.contains("opt/seguro/env/FOO")).unwrap();
        assert!(val_arg.contains("a\\,b\\,c"));
    }
}
