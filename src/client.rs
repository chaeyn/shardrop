use crate::{archive, model::*, server, ssh, store, transport::Transport};
use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::HashSet,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);
fn check_interrupted() -> Result<()> {
    ensure!(
        !INTERRUPTED.load(Ordering::Relaxed),
        "transfer paused; run shardrop resume with the same destination"
    );
    Ok(())
}
pub fn create(
    dir: &Path,
    options: Options,
    remote: Option<Remote>,
    jobs: usize,
    direct_port: Option<u16>,
    keep_source: bool,
) -> Result<Receipt> {
    ensure!((1..=32).contains(&jobs), "download workers must be 1..32");
    let _lock = store::empty_destination(dir)?;
    if remote.is_none() {
        let destination = std::fs::canonicalize(dir)?;
        for source in &options.sources {
            let source = std::fs::canonicalize(source)?;
            ensure!(
                !destination.starts_with(&source),
                "output directory cannot be inside a source"
            );
        }
    }
    store::private_dir(&dir.join("chunks"))?;
    eprintln!("Preparing source archive.");
    let descriptor = match &remote {
        Some(remote) => ssh::start(remote, &options)?,
        None => server::start(options.clone())?,
    };
    eprintln!("Source ready; connecting to data service.");
    let session = Session {
        protocol: PROTOCOL,
        remote,
        descriptor,
        reserve_bytes: options.reserve_bytes,
        options,
        direct_port,
    };
    store::atomic_json(&dir.join("session.json"), &session)?;
    transfer(dir, session, jobs, keep_source)
}
pub fn resume(dir: &Path, jobs: usize, keep_source: bool) -> Result<Receipt> {
    let _lock = store::lock(&dir.join("client.lock"))?;
    let session: Session = store::json(&dir.join("session.json"))?;
    ensure!(session.protocol == PROTOCOL, "incompatible session format");
    if dir.join("manifest.json").exists() {
        if let Ok(receipt) = archive::verify(dir) {
            store::atomic_json(&dir.join("receipt.json"), &receipt)?;
            if !keep_source && !dir.join("cleanup.json").exists() {
                finish_best_effort(dir, &session, &receipt);
            }
            return Ok(receipt);
        }
    }
    transfer(dir, session, jobs, keep_source)
}
fn reconnect(dir: &Path, session: &mut Session) -> Result<Transport> {
    if let Ok(transport) = Transport::connect(session) {
        return Ok(transport);
    }
    let endpoint = match &session.remote {
        Some(remote) => ssh::resume(remote, &session.descriptor)?,
        None => server::launch(&session.descriptor.job_dir)?,
    };
    ensure!(
        endpoint.id == session.descriptor.id
            && endpoint.ca_der == session.descriptor.ca_der
            && endpoint.token == session.descriptor.token,
        "resumed server identity changed"
    );
    session.descriptor = endpoint;
    store::atomic_json(&dir.join("session.json"), session)?;
    Transport::connect(session)
}
fn transfer(dir: &Path, mut session: Session, jobs: usize, keep_source: bool) -> Result<Receipt> {
    ensure!((1..=32).contains(&jobs), "download workers must be 1..32");
    let mut chunks: Vec<Chunk> = store::records(&dir.join("chunks.jsonl"))?;
    let mut verified = HashSet::new();
    for (index, chunk) in chunks.iter().enumerate() {
        chunk.validate(index)?;
        if store::verify_chunk(dir, chunk) {
            verified.insert(chunk.index);
        }
    }
    let mut transport = reconnect(dir, &mut session)
        .context("cannot reach source; completed chunks are preserved")?;
    let mut failures = 0;
    let mut last_report = Instant::now() - Duration::from_secs(5);
    loop {
        check_interrupted()?;
        let attempt = (|| -> Result<Option<Manifest>> {
            let reply = transport.connection.manifest(chunks.len())?;
            for chunk in reply.chunks {
                chunk.validate(chunks.len())?;
                store::append_record(&dir.join("chunks.jsonl"), &chunk)?;
                if store::verify_chunk(dir, &chunk) {
                    verified.insert(chunk.index);
                }
                chunks.push(chunk);
            }
            if reply.status.phase == Phase::Failed || reply.status.phase == Phase::Cancelled {
                bail!(
                    "source failed: {}",
                    reply.status.error.unwrap_or_else(|| "cancelled".into())
                );
            }
            let needed: Vec<_> = chunks
                .iter()
                .filter(|c| !verified.contains(&c.index))
                .collect();
            for batch in needed.chunks(jobs) {
                check_interrupted()?;
                // Reserve for the whole batch; individual workers also check before writing.
                store::ensure_space(
                    dir,
                    session.reserve_bytes,
                    batch.iter().map(|c| c.bytes).sum(),
                )?;
                let results = std::thread::scope(|scope| {
                    let handles: Vec<_> = batch
                        .iter()
                        .map(|chunk| {
                            let connection = &transport.connection;
                            scope.spawn(move || {
                                connection
                                    .download(dir, chunk, session.reserve_bytes)
                                    .map(|_| chunk.index)
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| {
                            h.join()
                                .map_err(|_| anyhow::anyhow!("download worker panicked"))?
                        })
                        .collect::<Vec<_>>()
                });
                let mut failure = None;
                for result in results {
                    match result {
                        Ok(index) => {
                            verified.insert(index);
                        }
                        Err(error) => failure = Some(error),
                    }
                }
                if last_report.elapsed() >= Duration::from_secs(1) {
                    eprintln!(
                        "Verified {}/{} available chunks ({} MiB produced)",
                        verified.len(),
                        chunks.len(),
                        reply.status.bytes / MIB
                    );
                    last_report = Instant::now();
                }
                if let Some(error) = failure {
                    return Err(error);
                }
            }
            if reply.status.phase == Phase::Ready && chunks.len() == reply.status.chunks {
                let manifest = Manifest { protocol: PROTOCOL, sources: session.options.sources.clone(), chunks: chunks.clone(),
                    sha256: reply.status.manifest_sha256.context("source omitted final checksum")?, warnings: reply.status.warnings,
                    warning_count: reply.status.warning_count, metadata: "tar: contents, directories, symlinks, mtime, basic modes; Unix hard links. ACLs, xattrs, ownership restoration, Windows ADS and sparse allocation are not preserved.".into() };
                manifest.validate()?;
                return Ok(Some(manifest));
            }
            Ok(None)
        })();
        match attempt {
            Ok(Some(manifest)) => {
                store::atomic_json(&dir.join("manifest.json"), &manifest)?;
                eprintln!("All chunks downloaded. Checking gzip CRCs and archive structure.");
                let receipt = archive::verify(dir)?;
                store::atomic_json(&dir.join("receipt.json"), &receipt)?;
                if !keep_source {
                    match transport.connection.finish(&receipt.manifest_sha256) {
                        Ok(()) => store::atomic_json(&dir.join("cleanup.json"), &serde_json::json!({"requested":true}))?,
                        Err(error) => eprintln!("Backup verified; source cleanup is pending: {error}. Run cleanup later."),
                    }
                }
                for warning in &manifest.warnings {
                    eprintln!("Warning: {warning}");
                }
                return Ok(receipt);
            }
            Ok(None) => {
                failures = 0;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(error) => {
                check_interrupted()?;
                failures += 1;
                if failures >= 5 {
                    return Err(error)
                        .context("transfer stopped; completed chunks are preserved for resume");
                }
                eprintln!("Transfer attempt {failures}/5 failed: {error:#}. Reconnecting.");
                std::thread::sleep(Duration::from_secs(1));
                transport = reconnect(dir, &mut session)?;
            }
        }
    }
}
fn finish_best_effort(dir: &Path, session: &Session, receipt: &Receipt) {
    match Transport::connect(session).and_then(|t| t.connection.finish(&receipt.manifest_sha256)) {
        Ok(()) => {
            let _ = store::atomic_json(
                &dir.join("cleanup.json"),
                &serde_json::json!({"requested":true}),
            );
        }
        Err(error) => {
            eprintln!("Backup verified; source cleanup is pending: {error}. Run cleanup later.")
        }
    }
}
pub fn cleanup(dir: &Path, cancel: bool) -> Result<()> {
    let _lock = store::lock(&dir.join("client.lock"))?;
    let mut session: Session = store::json(&dir.join("session.json"))?;
    // Verify before opening a source connection when completing a successful backup.
    let receipt = if cancel {
        None
    } else {
        Some(archive::verify(dir)?)
    };
    let transport = reconnect(dir, &mut session)?;
    match receipt {
        Some(receipt) => transport.connection.finish(&receipt.manifest_sha256)?,
        None => transport.connection.cancel()?,
    }
    store::atomic_json(
        &dir.join("cleanup.json"),
        &serde_json::json!({"requested":true,"cancelled":cancel}),
    )?;
    Ok(())
}
