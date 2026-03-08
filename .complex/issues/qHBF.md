## Goal
Replace `scripts/build-image.sh` with elu crate dependency. seguro profiles
map to elu stacks. `seguro images build --profile X` calls `elu::build()`
instead of shelling out to bash.

## Blocked on
elu project reaching MVP (store, manifest, layer stacking, output formats).
See ~/projects/elu/.complex/ for elu task tracking.
