## Problem
In `full-outbound` mode, proxy env vars are not set because Claude Code / Node.js
hangs when forced through an HTTP proxy. This means proxy-unaware tools bypass
logging and SSRF protection entirely.

## Options
- iptables DNAT redirect 80/443 to proxy (requires proxy to handle transparent mode)
- eBPF-based transparent redirect
- DNS-level interception (resolve through proxy)
- Accept the gap for v1 (current state)

## Context
`api-only` mode still forces proxy via iptables DROP + env vars — that's the
enforcement point. `full-outbound` is best-effort logging only.
