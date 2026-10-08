use crate::model::Chunk;
use anyhow::{anyhow, ensure, Context, Result};
use fs2::FileExt;
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn private_file(path: &Path) -> Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    Ok(opts.open(path)?)
}

pub fn atomic_bytes(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(data)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    atomic_bytes(path, &serde_json::to_vec_pretty(value)?)
}

pub fn json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    serde_json::from_reader(file).with_context(|| format!("invalid JSON in {}", path.display()))
}

pub fn file_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn bytes_hash(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}
pub fn manifest_hash(chunks: &[Chunk]) -> Result<String> {
    Ok(bytes_hash(&serde_json::to_vec(chunks)?))
}
pub fn chunk_name(index: u64) -> String {
    format!("{index:012}.gz")
}
pub fn chunk_path(dir: &Path, index: u64) -> PathBuf {
    dir.join("chunks").join(chunk_name(index))
}

pub fn ensure_space(dir: &Path, reserve: u64, additional: u64) -> Result<()> {
    let available = fs2::available_space(dir)?;
    ensure!(
        available >= reserve.saturating_add(additional),
        "free-space reserve reached at {} ({} bytes available)",
        dir.display(),
        available
    );
    Ok(())
}

pub fn lock(path: &Path) -> Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).write(true).read(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(path)?;
    file.try_lock_exclusive()
        .with_context(|| format!("another process holds {}", path.display()))?;
    Ok(file)
}

pub fn append_record<T: Serialize>(path: &Path, item: &T) -> Result<()> {
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    serde_json::to_writer(&mut file, item)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

/// Recover only an incomplete final append; committed corrupt records remain errors.
pub fn records<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read(path)?;
    let mut result = Vec::new();
    let mut offset = 0;
    for line in data.split_inclusive(|b| *b == b'\n') {
        match serde_json::from_slice(line) {
            Ok(value) => result.push(value),
            Err(_) if !line.ends_with(b"\n") && offset + line.len() == data.len() => {
                let file = OpenOptions::new().write(true).open(path)?;
                file.set_len(offset as u64)?;
                file.sync_all()?;
                return Ok(result);
            }
            Err(error) => return Err(error).context("corrupt committed manifest record"),
        }
        offset += line.len();
    }
    if !data.is_empty() && !data.ends_with(b"\n") {
        let mut file = OpenOptions::new().append(true).open(path)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    Ok(result)
}

pub fn verify_chunk(dir: &Path, item: &Chunk) -> bool {
    let path = chunk_path(dir, item.index);
    fs::metadata(&path)
        .map(|m| m.len() == item.bytes)
        .unwrap_or(false)
        && file_hash(&path)
            .map(|hash| hash == item.sha256)
            .unwrap_or(false)
}

pub fn empty_destination(path: &Path) -> Result<File> {
    if path.exists() {
        ensure!(
            fs::read_dir(path)?.next().is_none(),
            "destination is not empty; use resume or choose a new directory"
        );
    }
    private_dir(path)?;
    lock(&path.join("client.lock"))
}
