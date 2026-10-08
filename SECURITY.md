# Security

The source executable creates a per-job certificate authority, TLS leaf certificate and 256-bit bearer token. The SSH startup channel carries the pinned CA and token. Direct and tunnel data connections both validate the server certificate, hostname and token. Redirects and environment HTTP proxies are disabled. SSH host-key policy is left to OpenSSH; the utility does not disable host-key checking.

Treat `session.json`, source `job.json` and `key.pem` as secrets. Do not attach them to public issues. Archives are plaintext at rest. Unix private directories use mode 0700 and private files use mode 0600. On Windows, use a private user-owned directory with appropriate ACLs; the application does not configure Windows ACLs.

The data API serves numbered immutable chunks and bounded manifest pages. It exposes no arbitrary filesystem read or shell execution route. Cleanup checks the random job marker and targets only that job's staging directory. Archive extraction validates paths and uses the tar library's protection against traversal and symlink escape. It does not sandbox the entire process. Do not restore untrusted backups as an administrator or root.

Direct mode exposes an authenticated TLS listener on IPv4 interfaces. It is intended for trusted LANs and has not received a public-network denial-of-service audit. Use SSH tunnel mode when direct exposure is unnecessary.

Report vulnerabilities through [GitHub private vulnerability reporting](https://github.com/chaeyn/shardrop/security/advisories/new). Do not publish working tokens or sensitive file paths in reports.
