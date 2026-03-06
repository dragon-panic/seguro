use color_eyre::eyre::Result;

/// Build the `-fw_cfg` arguments to inject env vars into the guest.
pub fn build_args(env_vars: &[(String, String)]) -> Result<Vec<String>> {
    let mut args = Vec::new();

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
    fn generates_env_var_args() {
        let env = vec![("ANTHROPIC_API_KEY".into(), "sk-test".into())];
        let args = build_args(&env).unwrap();
        assert!(args.iter().any(|a| a.contains("opt/seguro/env/ANTHROPIC_API_KEY")));
    }

    #[test]
    fn escapes_commas_in_value() {
        let env = vec![("FOO".into(), "a,b,c".into())];
        let args = build_args(&env).unwrap();
        let val_arg = args.iter().find(|a| a.contains("opt/seguro/env/FOO")).unwrap();
        assert!(val_arg.contains("a\\,b\\,c"));
    }

    #[test]
    fn empty_env_produces_no_args() {
        let args = build_args(&[]).unwrap();
        assert!(args.is_empty());
    }
}
