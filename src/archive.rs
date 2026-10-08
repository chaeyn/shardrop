use crate::{model::*, store};
use anyhow::{ensure, Context, Result};
use flate2::{read::MultiGzDecoder, write::GzEncoder, Compression};
use globset::{Glob, GlobSetBuilder};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tar::{Archive, Builder, Header};
use walkdir::WalkDir;

#[derive(Default)]
pub struct Progress {
    pub status: Status,
    pub chunks: Vec<Chunk>,
}
pub type Shared = Arc<Mutex<Progress>>;

struct ChunkWriter<'a> {
    dir: &'a Path,
    options: &'a Options,
    shared: Shared,
    stop: Arc<AtomicBool>,
    buffer: Vec<u8>,
    pending: Vec<Vec<u8>>,
}
impl ChunkWriter<'_> {
    fn commit(&mut self) -> Result<()> {
        ensure!(!self.stop.load(Ordering::Relaxed), "archive cancelled");
        let level = self.options.compression_level;
        let pending = std::mem::take(&mut self.pending);
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = pending
                .into_iter()
                .map(|raw| {
                    scope.spawn(move || -> Result<(u64, Vec<u8>)> {
                        let mut gzip = GzEncoder::new(Vec::new(), Compression::new(level));
                        gzip.write_all(&raw)?;
                        Ok((raw.len() as u64, gzip.finish()?))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .map_err(|_| anyhow::anyhow!("compression worker panicked"))?
                })
                .collect::<Result<Vec<_>>>()
        })?;
        for (raw_bytes, bytes) in results {
            ensure!(!self.stop.load(Ordering::Relaxed), "archive cancelled");
            store::ensure_space(self.dir, self.options.reserve_bytes, bytes.len() as u64)?;
            let index = self.shared.lock().unwrap().chunks.len() as u64;
            let chunk = Chunk {
                index,
                raw_bytes,
                bytes: bytes.len() as u64,
                sha256: store::bytes_hash(&bytes),
            };
            store::atomic_bytes(&store::chunk_path(self.dir, index), &bytes)?;
            store::append_record(&self.dir.join("chunks.jsonl"), &chunk)?;
            let mut state = self.shared.lock().unwrap();
            state.status.bytes += chunk.bytes;
            state.status.raw_bytes += chunk.raw_bytes;
            state.chunks.push(chunk);
            state.status.chunks = state.chunks.len();
            store::atomic_json(&self.dir.join("status.json"), &state.status)?;
        }
        Ok(())
    }
    fn finish(mut self) -> Result<()> {
        if !self.buffer.is_empty() {
            self.pending.push(std::mem::take(&mut self.buffer));
        }
        self.commit()
    }
}
impl Write for ChunkWriter<'_> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let len = input.len();
        while !input.is_empty() {
            if self.stop.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "archive cancelled",
                ));
            }
            let size = (self.options.chunk_mib * MIB as usize - self.buffer.len()).min(input.len());
            self.buffer.extend_from_slice(&input[..size]);
            input = &input[size..];
            if self.buffer.len() == self.options.chunk_mib * MIB as usize {
                self.pending.push(std::mem::take(&mut self.buffer));
                if self.pending.len() == self.options.compression_workers {
                    self.commit().map_err(io::Error::other)?;
                }
            }
        }
        Ok(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn warning(shared: &Shared, text: String) {
    let mut state = shared.lock().unwrap();
    state.status.warning_count += 1;
    if state.status.warnings.len() < 100 {
        state.status.warnings.push(text);
    }
}

pub fn produce(dir: &Path, options: &Options, shared: Shared, stop: Arc<AtomicBool>) -> Result<()> {
    options.validate()?;
    store::private_dir(&dir.join("chunks"))?;
    shared.lock().unwrap().status.phase = Phase::Compressing;
    let mut patterns = GlobSetBuilder::new();
    for pattern in &options.excludes {
        patterns.add(Glob::new(pattern)?);
    }
    let patterns = patterns.build()?;
    let writer = ChunkWriter {
        dir,
        options,
        shared: shared.clone(),
        stop,
        buffer: Vec::new(),
        pending: Vec::new(),
    };
    let mut tar = Builder::new(writer);
    tar.follow_symlinks(false);
    let mut roots = HashSet::new();
    #[cfg(unix)]
    let mut hardlinks: std::collections::HashMap<(u64, u64), PathBuf> =
        std::collections::HashMap::new();
    let staging = fs::canonicalize(dir)?;
    for source in &options.sources {
        // Canonicalize only the parent: a source symlink itself must stay a symlink.
        let name = source
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("root"));
        ensure!(
            roots.insert(name.to_os_string()),
            "source roots have the same archive name: {}",
            name.to_string_lossy()
        );
        let root = PathBuf::from(name);
        for entry in WalkDir::new(source)
            .follow_links(false)
            .follow_root_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| {
                let archive_path =
                    root.join(entry.path().strip_prefix(source).unwrap_or(Path::new("")));
                !patterns.is_match(&archive_path)
                    && !patterns.is_match(entry.path())
                    && fs::canonicalize(entry.path()).ok().as_deref() != Some(staging.as_path())
            })
        {
            let entry = entry?;
            let path = entry.path();
            let name = root.join(path.strip_prefix(source)?);
            let before = fs::symlink_metadata(path)?;
            let kind = before.file_type();
            if kind.is_symlink() {
                tar.append_path_with_name(path, &name)?;
            } else if kind.is_dir() {
                tar.append_dir(&name, path)?;
            } else if kind.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if before.nlink() > 1 {
                        let key = (before.dev(), before.ino());
                        if let Some(first) = hardlinks.get(&key) {
                            let mut header = Header::new_gnu();
                            header.set_metadata(&before);
                            header.set_entry_type(tar::EntryType::Link);
                            header.set_size(0);
                            tar.append_link(&mut header, &name, first)?;
                            continue;
                        }
                        hardlinks.insert(key, name.clone());
                    }
                }
                let file =
                    File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
                let captured = file.metadata()?;
                let mut header = Header::new_gnu();
                header.set_metadata(&captured);
                header.set_size(captured.len());
                let mut limited = file.take(captured.len());
                tar.append_data(&mut header, &name, &mut limited)?;
                ensure!(
                    limited.limit() == 0,
                    "file shrank while reading: {}",
                    path.display()
                );
                let after = fs::symlink_metadata(path)?;
                if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
                    warning(
                        &shared,
                        format!("file changed while reading: {}", path.display()),
                    );
                }
            } else {
                warning(&shared, format!("skipped special file: {}", path.display()));
            }
        }
    }
    tar.finish()?;
    tar.into_inner()?.finish()?;
    let mut state = shared.lock().unwrap();
    let hash = store::manifest_hash(&state.chunks)?;
    state.status.manifest_sha256 = Some(hash);
    state.status.phase = Phase::Ready;
    store::atomic_json(&dir.join("status.json"), &state.status)?;
    Ok(())
}

struct Parts {
    paths: std::vec::IntoIter<PathBuf>,
    current: Option<File>,
}
impl Read for Parts {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if let Some(file) = &mut self.current {
                let n = file.read(buf)?;
                if n > 0 {
                    return Ok(n);
                }
            }
            match self.paths.next() {
                Some(path) => self.current = Some(File::open(path)?),
                None => return Ok(0),
            }
        }
    }
}
fn decoder(dir: &Path, manifest: &Manifest) -> MultiGzDecoder<impl Read> {
    let paths = manifest
        .chunks
        .iter()
        .map(|c| store::chunk_path(dir, c.index))
        .collect::<Vec<_>>()
        .into_iter();
    MultiGzDecoder::new(Parts {
        paths,
        current: None,
    })
}
fn safe_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}
pub fn verify(dir: &Path) -> Result<Receipt> {
    let manifest: Manifest = store::json(&dir.join("manifest.json"))?;
    manifest.validate()?;
    for chunk in &manifest.chunks {
        ensure!(
            store::verify_chunk(dir, chunk),
            "chunk {} is missing or corrupt; use resume",
            chunk.index
        );
    }
    let mut archive = Archive::new(decoder(dir, &manifest));
    let mut count = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        ensure!(safe_path(&entry.path()?), "unsafe archive path");
        let kind = entry.header().entry_type();
        ensure!(
            kind.is_file() || kind.is_dir() || kind.is_symlink() || kind.is_hard_link(),
            "unsupported archive entry type"
        );
        if entry.header().entry_type().is_hard_link() {
            ensure!(
                entry.link_name()?.is_some_and(|p| safe_path(&p)),
                "unsafe hard link target"
            );
        }
        io::copy(&mut entry, &mut io::sink())?;
        count += 1;
    }
    // tar stops at its end markers; drain gzip to check every member's CRC/trailer.
    io::copy(&mut archive.into_inner(), &mut io::sink())?;
    Ok(Receipt {
        protocol: PROTOCOL,
        manifest_sha256: manifest.sha256,
        archive_entries: count,
        compressed_bytes: manifest.chunks.iter().map(|c| c.bytes).sum(),
        chunk_sha256_verified: true,
        gzip_crc_verified: true,
        warning_count: manifest.warning_count,
    })
}
pub fn restore(dir: &Path, destination: &Path) -> Result<Receipt> {
    let receipt = verify(dir)?;
    if destination.exists() {
        ensure!(
            fs::read_dir(destination)?.next().is_none(),
            "restore destination must be empty"
        );
    }
    store::private_dir(destination)?;
    let manifest: Manifest = store::json(&dir.join("manifest.json"))?;
    let mut archive = Archive::new(decoder(dir, &manifest));
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    // Archive::unpack delays directory permissions until children have been extracted.
    archive.unpack(destination)?;
    Ok(receipt)
}
