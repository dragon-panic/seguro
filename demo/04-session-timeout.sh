#!/usr/bin/env bash
# Demo 4: Session timeout
# Shows that --timeout kills a long-running command after the specified duration.
# Uses a 5-second timeout against a command that would run for 10 minutes.
set -e

echo "Starting sandbox with --timeout 5 (5 seconds)..."
echo "Guest will run 'sleep 600' — expect it to be killed after ~5s."

START=$(date +%s)

# The sleep 600 should be killed by the timeout.
# We expect a non-zero exit (timeout error), so invert the check.
if cargo run -- run --timeout 5 -- sleep 600 2>&1; then
  echo "FAIL: command should have been killed by timeout"
  exit 1
fi

ELAPSED=$(( $(date +%s) - START ))
echo "Session terminated after ${ELAPSED}s"

# Sanity check: should have taken roughly boot_time + 5s, not 600s.
if [ "$ELAPSED" -gt 120 ]; then
  echo "FAIL: took too long — timeout did not fire"
  exit 1
fi

echo "PASS: timeout killed the session as expected"
