# Changelog

## 0.2.0 (unreleased)

- Native Rust client and source worker with no Python or external tar dependency.
- Parallel gzip compression and parallel chunk downloads.
- SSH startup, pinned TLS LAN transfers and SSH tunnel fallback.
- Resumable chunk journals, SHA-256 validation, gzip CRC verification and safe extraction.
- Local pack, offline verification, explicit cleanup, JSON receipts, completions and man page.
- Cross-platform CI and draft-release packaging.

The saved session/archive protocol replaces the Python 0.1 prototype and is not compatible with its session files.
