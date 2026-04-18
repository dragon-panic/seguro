## Goal

Stop `session.qcow2` from living on tmpfs. It moves to `$XDG_STATE_HOME/seguro/overlays/<id>.qcow2` (default) or `$SEGURO_OVERLAY_DIR/<id>.qcow2` (env override).

Runtime dir keeps everything else (sockets, pids, cidata, session.json, ssh key). That mix is correct for tmpfs.

## Acceptance

A fresh `seguro run --persistent … -- bash -c 'sleep 5'` produces:
- `$XDG_RUNTIME_DIR/seguro/<id>/` with session.json, qemu.pid, ssh.port, ssh key, vhost sockets, cidata.img — **no** session.qcow2.
- `$XDG_STATE_HOME/seguro/overlays/<id>.qcow2` present.
- `session.json` carries `overlay_path = …/overlays/<id>.qcow2`.
- `recover(id)` reopens the VM against the correct overlay.
- `Session::cleanup()` deletes both the runtime subdir and the overlay file.
- `classify_sessions` marks a session Dead when the overlay file is missing even if qemu.pid looks live (guards against deleting the overlay out from under a zombie).

## Plan

1. **Red**: unit test in `session/mod.rs::tests` — `Session::allocate` returns an `overlay_path` that is NOT under `runtime_dir` and IS under `overlay_dir()`. Stub `overlay_dir()` so the test can redirect it to a tempdir.
2. **Green**: add `config::overlay_dir()`. Point `Session::allocate` at it. Ensure the dir is created. Flip create order: runtime dir first, then overlay. Verify all call sites (`api.rs` launch + recover, prune, snapshot, shell) compile unchanged since they already use `session.overlay_path` / `session.runtime_dir` fields.
3. **Red**: test `Session::cleanup` removes the overlay file from its new location.
4. **Green**: cleanup deletes overlay file explicitly before removing runtime dir.
5. **Red**: test `classify_sessions` returns Dead when overlay file is absent.
6. **Green**: add the overlay-existence check.
7. **Docs**: README note — runtime vs overlay split, `SEGURO_OVERLAY_DIR` env var, `--persistent` does not yet survive reboot.

## Files expected to change

- `src/config.rs` — new `overlay_dir()`; respect `SEGURO_OVERLAY_DIR`.
- `src/session/mod.rs` — `allocate` path assignment, creation order; `cleanup` removes overlay file.
- `src/session/image.rs` — `classify_sessions` checks overlay existence.
- `tests/` or inline `#[cfg(test)]` modules.
- `README.md` (short section).