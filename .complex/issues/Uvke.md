## Goal

After slice 1 ships, crashes mid-creation and reboots leave qcow2 files in `overlay_dir` with no matching `runtime_dir/<id>/`. `sessions prune` needs to reap them.

## Acceptance

`cx sessions prune` (once per invocation) performs a second pass:
- Collect `live_ids` = subdir names in `runtime_dir` whose `qemu.pid` is alive.
- Collect `disk_ids` = `<id>.qcow2` filenames in `overlay_dir`.
- `orphans = disk_ids − live_ids`, filtered by mtime older than `--min-age` (default same as existing prune default).
- Delete each orphan overlay. Report counts.

Post-reboot case: runtime_dir is empty (tmpfs cleared), overlay_dir has files from before reboot → all orphaned → all deleted after grace.

Creation race: overlay written fresh (mtime young), runtime dir not yet populated → not touched thanks to mtime grace.

## Plan

1. **Red**: `list_orphan_overlays(runtime_dir, overlay_dir, grace)` test — scenarios: live runtime dir kept, orphan past grace reported, orphan inside grace ignored, post-reboot (no runtime subdirs at all) all reported.
2. **Green**: implement the function.
3. **Red**: integration test — `prune` command deletes orphan overlays, keeps matched ones.
4. **Green**: wire `list_orphan_overlays` into `commands/sessions.rs::prune`.
5. **Sanity**: exercise `prune --force --min-age 0` end to end on a scratch XDG env.

## Files expected to change

- `src/session/image.rs` — new `list_orphan_overlays`.
- `src/commands/sessions.rs` — second pass in `prune`.
- Tests alongside.