#!/usr/bin/env bash
# Demo 2: Bidirectional file sharing via virtiofs
# The guest can read files from the host workspace and write back.
set -e

SHARE=$(mktemp -d)
echo "hello from the host" > "$SHARE/hello.txt"
echo "Workspace: $SHARE"

cargo run -- run --share "$SHARE" -- bash -c \
  'cat ~/workspace/hello.txt && echo "hello from the guest" >> ~/workspace/hello.txt'

echo "--- host sees ---"
cat "$SHARE/hello.txt"
