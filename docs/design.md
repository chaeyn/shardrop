# Design and protocol 1

The same executable runs on both ends. `copy` invokes `agent start` over SSH with JSON options on stdin. The source creates a random marked directory under the OS temporary directory, generates TLS credentials and starts an independent `agent run` process. Startup returns an endpoint descriptor over SSH. Client state is persisted before transfer.

A sorted filesystem walk writes a tar stream. The writer buffers up to `compression_workers` raw chunks, compresses that batch concurrently, and commits the resulting gzip members in order. Downloads overlap later compression batches. Memory usage scales with `chunk_mib × compression_workers`; compressed output and download buffers add overhead. Raw size is limited to 64 MiB per chunk and the configured raw batch is limited to 512 MiB. This is batching, not a continuous all-stages worker queue.

Each source commit writes and syncs a temporary chunk, renames it to a 12-digit index, appends and syncs its record, and updates status. Records are `{index, bytes, raw_bytes, sha256}`. A manifest's SHA-256 covers compact JSON serialization of the ordered record array. It provides an integrity cross-check; the pinned TLS channel supplies transport authenticity. Local users who can modify a backup and its manifest are outside the threat model.

The TLS API requires `Authorization: Bearer <token>`:

- `GET /manifest?after=N`: current status and at most 256 committed records.
- `GET /chunk/N`: one committed gzip member.
- `POST /finish`: body is the final manifest hash; accepted only for a ready matching job.
- `POST /cancel`: explicit discard of source staging, including unfinished compression.

The receiver validates metadata, downloads each member to a temporary file, checks length and SHA-256, syncs it, and renames it. On resume it rechecks saved chunks and downloads missing/corrupt ones. An incomplete final JSONL append may be truncated; a corrupt committed line is an error. A per-directory lock excludes concurrent receivers. Failures retain completed chunks. The client reconnects on failed transfer attempts, preferring direct TLS when configured and otherwise opening an SSH tunnel. It does not periodically switch a healthy tunnel back to a direct connection.

Once the source is ready and every chunk is present, the receiver validates the final manifest, streams the concatenated gzip members through a tar reader, and drains to EOF to verify all gzip trailers. It saves `receipt.json` before requesting cleanup. `cleanup.json` records an accepted request, not an independently observed deletion. A crashed or disconnected cleanup can be retried explicitly.

TTL stops the source service without deleting staging. Completed archives can be served again with the same CA/token. A worker interrupted while compressing becomes failed on restart; it cannot regenerate later chunks from a changed live tree. This intentionally requires a new backup.

Saved destination layout:

```
backup/
  chunks/000000000000.gz ...
  chunks.jsonl
  manifest.json       # final metadata
  session.json        # private connection credentials
  receipt.json        # present after successful verification
  cleanup.json        # optional accepted cleanup request
  client.lock
```

No giant intermediate tar or joined gzip copy is written. Both source and receiver retain the complete compressed chunk set until source cleanup. Restore writes ordinary files and therefore needs space for the expanded archive too.
