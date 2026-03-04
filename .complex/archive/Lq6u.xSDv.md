Create the Rust project skeleton.

- `cargo new seguro --bin`
- Set up `Cargo.toml` with all crates from the PRD (clap, tokio, serde, toml, color-eyre, tracing, tracing-subscriber, hudsucker, rcgen, rustls, hyper, ed25519-dalek, ssh-key, uuid, dirs, nix)
- Create the full module tree with stub `mod.rs` files: session/{ports,keys,image}, vm/{virtiofsd,fw_cfg}, proxy/{filter,log,ca}, commands/{run,shell,sessions,images,snapshot,proxy_log}
- Set up `tracing-subscriber` with env-filter in main
- Wire clap subcommands to empty command stubs that print 'not yet implemented'
- Add .gitignore, confirm `cargo check` passes