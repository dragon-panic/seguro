## Goal
Add a "cua" profile that enables computer use (screen interaction via VNC).

## What's needed
- Profile with Xvfb, x11vnc, lightweight WM (openbox/cage)
- `--gui` CLI flag (alias for `--profile cua`)
- VNC port forwarding from guest to host
- Guest startup: Xvfb :99, x11vnc on :99, DISPLAY=:99
- Claude's CUA tool connects to the forwarded VNC port

## Blocked on
Profile system is done. This just needs the image built and VNC wiring.
Could also wait for elu to handle image building.
