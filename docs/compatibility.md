# Compatibility

| Area | Behavior |
|---|---|
| Linux / macOS clients and sources | Native Rust implementation; see validation for measured environments |
| Windows client / source | Implementation and CI target; remote source needs an OpenSSH server and `--remote-shell powershell` |
| CPU targets in release workflow | Linux x86-64/ARM64 musl, macOS x86-64/ARM64, Windows x86-64 MSVC |
| External runtime | OpenSSH client for remote operations; no external dependency for local pack/verify/restore |
| File data, empty directories | Preserved |
| mtime, basic Unix mode | Stored in tar; destination OS support governs restoration |
| Symlinks | Stored without traversing; Windows creation may require Developer Mode or elevation |
| Hardlinks | Detected by device/inode on Unix sources; Windows sources store file contents separately |
| Non-UTF8 paths | Nested Unix paths supported where the filesystem permits them; root paths must be UTF-8 |
| Cross-OS paths | Names unsupported by the destination filesystem can cause restore failure; no lossy renaming |
| ACLs / xattrs / ownership restore / ADS | Not preserved |
| Sparse files | Logical bytes preserved; sparse allocation not preserved |
| Special files | Sockets, FIFOs, devices skipped with warnings |
| Live changing files | Changes detected through size/mtime produce warnings; shrinking reads fail; no snapshot isolation |
| Source staging lifetime | Default 24-hour service, retained files until explicit/successful cleanup or OS deletion |
| Network fallback | Direct IPv4 TLS → SSH tunnel on reconnection; SSH control must be reachable |
| Python prototype | Existing 0.1 sessions are not compatible with this Rust protocol |

Cross-platform source compilation does not establish runtime support. The Windows SSH source path, user ACLs, symlink privileges and service process lifetime need host testing. Refer to validation rather than treating the build matrix as completed tests.
