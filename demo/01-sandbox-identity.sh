#!/usr/bin/env bash
# Demo 1: Sandbox identity
# Shows the agent runs as an isolated user inside a fresh Ubuntu VM.
set -e

cargo run -- run -- bash -c \
  'echo "user: $(whoami)" && echo "os: $(uname -r)" && echo "python: $(python3 --version)"'
