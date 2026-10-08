# Shardrop

[![CI](https://github.com/chaeyn/shardrop/actions/workflows/ci.yml/badge.svg)](https://github.com/chaeyn/shardrop/actions/workflows/ci.yml)

Compress, split and transfer a directory in one command. Resume an interrupted transfer without downloading valid chunks again.

```sh
shardrop copy user@server /home/user/project -o ./project-backup
shardrop restore ./project-backup --to ./restored
```

[한국어](README.ko.md) · [Design](docs/design.md) · [Compatibility](docs/compatibility.md) · [Validation](docs/validation.md)

This is a Rust implementation with native compression, archive handling and TLS. Install the same executable version on the source and destination machines. Remote copies use the system's OpenSSH client for authentication, startup and tunnelling. Local archives need no SSH. Python, GNU tar and an OpenSSL executable are not required.

## Install

Download the executable for your OS from [GitHub Releases](https://github.com/chaeyn/shardrop/releases). Early versions are marked as pre-releases. To build from source:

```sh
git clone https://github.com/chaeyn/shardrop.git
cd shardrop
```

```sh
cargo install --path . --locked
shardrop doctor
```

Rust 1.88 or newer is required to build. End users of a prebuilt executable do not need Rust. A supplied release archive contains one executable, documentation, shell completions and a manual page. Verify its `.sha256` file before extracting, then put the executable on `PATH`. On Unix, `scripts/install-local.sh ./shardrop` installs a local executable into `~/.local/bin`. On Windows, `scripts/install-local.ps1 -Binary .\shardrop.exe` installs into the user's local application directory and updates the user PATH. Neither script downloads or runs remote code.

For the remote machine, install its own platform's executable. Use `--remote-bin /absolute/path/shardrop` when it is not on the SSH session's PATH.

## Use

Copy over SSH, keeping 2 GiB free on both machines by default:

```sh
shardrop copy server /srv/projects /srv/photos -o ./backup
```

Use a faster direct LAN data connection, with SSH as the fallback:

```sh
shardrop copy server /srv/projects -o ./backup --direct 192.168.1.20
```

`--direct` names the source machine, not the destination. This binds the source data listener to all IPv4 interfaces; only enable it on a network where inbound access is intended. It does not modify firewall or router settings. Use `--listen-port 9443` for a fixed source port and `--direct-port` only if a port mapping changes the externally reachable port. SSH control must remain reachable to start or restart a job. Direct IPv6 listeners are not supported in this release.

Press Ctrl+C to pause the receiver. The source keeps preparing chunks until it finishes, fails, or reaches its lifetime limit. After reconnecting:

```sh
shardrop resume ./backup
```

Completed downloads also verify offline:

```sh
shardrop verify ./backup
shardrop restore ./backup --to ./empty-directory
```

Create a local archive:

```sh
shardrop pack ./project -o ./project-backup
```

Adjust compression, chunk size and download concurrency:

```sh
shardrop copy server /srv/project -o ./backup \
  --chunk-mib 8 --compression-workers 4 --level 3 -j 8 \
  --exclude '**/node_modules/**' --exclude '**/.cache/**'
```

Exclusion globs match source paths and archive paths. Source roots become their basenames, so `/srv/project` restores as `project/`. Multiple roots must have different basenames. Symlinks are archived without following their targets. Special files such as sockets and FIFOs are skipped with warnings. A file that changes while being read is reported; a file that shrinks mid-read fails the job. Use filesystem snapshots or stop writers when application consistency matters.

Remote Windows sources using an OpenSSH server:

```powershell
shardrop copy user@windows 'C:\Users\User\Documents' -o backup --remote-shell powershell --remote-bin 'C:\Tools\shardrop.exe'
```

Use `--sudo` only with an existing `sudo -n` permission on a POSIX source. This tool does not collect or store sudo passwords. Sources must be absolute paths on the remote OS. Local `pack` accepts relative paths.

## Space, integrity and cleanup

- Raw tar data is split into independently compressed gzip members. The receiver downloads finished members while the source continues compressing.
- Every chunk has SHA-256 and a length. Resume hashes existing files before reusing them. Final verification checks chunk ordering, the manifest hash, gzip CRCs and tar structure.
- Restore verifies before extraction and requires an empty directory. It rejects archive traversal and uses the tar library's extraction protections.
- Each side needs space for the compressed archive. Source staging lives in the source OS temporary directory. It is not removed incrementally, so missing receiver chunks remain available during a partial transfer.
- Successful verification requests source staging cleanup. Use `--keep-source` on `copy`, `pack` and `resume` to keep repair copies; later run `shardrop cleanup ./backup`.
- `shardrop cleanup ./backup --cancel` discards an unfinished source job. It leaves local chunks intact. The source paths themselves are never cleanup targets.
- The default source server lifetime is 24 hours (`--ttl-seconds`, 60 seconds to 7 days). Expiration stops serving and keeps staging. `resume` can restart a completed source job. If compression itself was interrupted, start a new backup; the tool refuses to mix snapshots.
- The source machine or its temporary-file policy may delete staging. After staging is gone, a damaged local backup cannot be repaired by resume.
- A receipt means the downloaded archive verified. It does not prove application-level consistency, remote deletion completion or preservation of every filesystem attribute.

Backup directories contain a private `session.json` with the source job token and certificate. Keep them private. Data is encrypted in transit, but chunks are not encrypted at rest. Unix directories are created with mode 0700; Windows permissions depend on the user's directory ACLs. See [security](SECURITY.md).

## Automation and development

`--json` prints the final verification receipt on stdout; progress and warnings go to stderr. Exit codes: `0` success (possibly with recorded warnings), `1` operation failure, `2` invalid CLI arguments, `130` paused by Ctrl+C. Inspect `warning_count` when warnings must fail your workflow.

```sh
shardrop completions bash > shardrop.bash
shardrop man > shardrop.1
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

The project includes Linux, macOS and Windows CI, plus tag-triggered release packaging. Check [Actions](https://github.com/chaeyn/shardrop/actions) for current build results. See [CONTRIBUTING.md](CONTRIBUTING.md) for the release procedure and verification boundaries.

MIT licensed. The packaging and cross-platform CLI approach takes inspiration from [uutils/coreutils](https://github.com/uutils/coreutils); this is an independent transfer utility.
