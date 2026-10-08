# Contributing

Use stable Rust and run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --locked`. Integration tests run the actual executable and its local TLS server. They use temporary source fixtures and clean up their own server jobs after successful tests. Inspect failed tests' job paths before removing leftovers.

Preserve compatibility with saved sessions or bump `PROTOCOL` and document the migration boundary. Archive tests should cover bytes, paths, links, failed transfers and cleanup; a successful command exit alone is insufficient. Keep English and Korean README usage in sync. Do not add credentials or backup/session directories to source packages.

The Python 0.1 prototype's sessions are not compatible with the Rust 0.2 format. Do not modify users' original backups to migrate them.

## Release procedure

1. Prepare changes in https://github.com/chaeyn/shardrop. GitHub releases and crates.io publication are separate steps.
2. Update `Cargo.toml`, regenerate `Cargo.lock`, and update `CHANGELOG.md`.
3. Run all three OS CI jobs. Test remote Windows OpenSSH explicitly before advertising it as verified.
4. Review dependencies and compatibility notes. Build the release matrix.
5. Tag the reviewed commit with `v` followed by the Cargo package version.
6. The release workflow creates a **draft** GitHub release containing archives and checksums. Review it before publishing.

`python3 scripts/package.py --binary /path/to/shardrop --target aarch64-apple-darwin --output dist` packages a native executable and generates completions/manual pages by executing it. When cross-compiling, pass `--generator` with a host-native build of the same version. Python is a maintainer packaging dependency; end users do not need it.

Windows release builds use `-C target-feature=+crt-static` with an explicit MSVC target, so prebuilt users do not need a separate Visual C++ runtime installation. The `Rebuild portable Windows draft` workflow can rebuild an existing unpublished draft tag with those flags. It refuses to replace assets on published releases. The source tag remains unchanged.
