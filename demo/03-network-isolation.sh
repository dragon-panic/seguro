#!/usr/bin/env bash
# Demo 3: Network isolation
# air-gapped blocks all outbound traffic; full-outbound allows internet.
set -e

echo "=== air-gapped (expect: BLOCKED) ==="
cargo run -- run --net air-gapped -- bash -c \
  'curl -s --max-time 5 https://example.com >/dev/null && echo CONNECTED || echo BLOCKED'

echo "=== full-outbound (expect: HTTP 200) ==="
cargo run -- run --net full-outbound -- bash -c \
  'curl -s --max-time 10 -o /dev/null -w "HTTP %{http_code}\n" https://example.com'
