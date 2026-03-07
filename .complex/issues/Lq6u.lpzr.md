## Task
Run demo/03 and confirm air-gapped blocks traffic and full-outbound allows it.
If it fails, fix it.

## Acceptance
- air-gapped mode: curl fails (BLOCKED)
- full-outbound mode: curl succeeds (HTTP 200)