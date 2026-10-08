# Validation record

Checked on 2026-10-09. This record describes executed checks; it does not claim a published release or a public CI run.

## Executed

- macOS ARM64: native Rust build and 10 integration tests passed.
- Static checks: `cargo fmt --check` and `cargo clippy --locked --all-targets -- -D warnings`. `cargo package --locked --allow-dirty` built and verified the source package. The local Unix install script and installed `doctor` command passed.
- Linux x86-64: musl cross-build from macOS, followed by execution on a Linux server. The final release build also passed source-worker restart, chunk repair, restore and cleanup on Linux.
- Actual Linux → macOS transfer over OpenSSH: 10 MiB + 123 bytes of random file data, Unicode filename, empty directory, symlink and hardlink. The archive had **11 chunks, 10,488,289 compressed bytes, 10,490,880 raw tar bytes**, with zero warnings.
- Direct endpoint failure → SSH tunnel fallback was exercised. This was not a direct-LAN throughput benchmark.
- Deleted chunk 1 and corrupted chunk 2, then resumed. Both repaired; chunk 0 retained its modification timestamp. Restored file SHA-256 matched the source, and restored link targets/inodes matched expectations.
- Linux-native local pack and restore preserved a non-UTF8 nested filename. macOS's test filesystem rejects that filename at creation, so that fixture is Linux-specific.
- Real Ctrl+C during a 64 MiB transfer: stopped with exit code **130** after 2 received chunks, resumed, reused the intact first chunk, and restored matching file contents.
- Source-job directories and the temporary Linux test fixture were independently confirmed absent after cleanup. Original user backup files were not used as test fixtures.

## Automated test coverage

1. Local TLS pack/restore, damaged and missing chunk repair, intact-chunk reuse, token/CA rejection and cleanup.
2. Exclusion patterns and rejection of output nested inside a source.
3. Incomplete journal recovery and rejection of corrupt committed records.
4. Gzip CRC failure after tar end markers.
5. Parent-path traversal rejection.
6. POSIX shell quoting and SSH host option-injection rejection.
7. Symlink escape during extraction.
8. Read-only directory restoration and sparse-file logical contents.
9. Completed source-worker restart without recompression.
10. Interrupted source compression cannot resume as a new mixed snapshot.

## Not yet verified

- Windows runtime, remote Windows OpenSSH startup/detachment, ACL inheritance and symlink privileges.
- Linux ARM64 and Intel macOS runtime.
- Full 24-hour lifetime expiry or prolonged Wi-Fi roaming.
- Public-network denial-of-service resistance, large concurrent-client stress or third-party security audit.
- Snapshot consistency for changing databases; the tool does not provide snapshots.
- Public GitHub CI, a published GitHub release or crates.io installation.

The release workflow targets these additional operating systems, but its presence is not proof they have run successfully. Check their CI and host tests before expanding the verified support claim.
