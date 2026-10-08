use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PROTOCOL: u32 = 1;
pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;

#[derive(Clone, Serialize, Deserialize)]
pub struct Options {
    pub sources: Vec<PathBuf>,
    pub excludes: Vec<String>,
    pub chunk_mib: usize,
    pub compression_workers: usize,
    pub compression_level: u32,
    pub reserve_bytes: u64,
    pub ttl_seconds: u64,
    pub listen_port: u16,
    pub direct_host: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            excludes: Vec::new(),
            chunk_mib: 8,
            compression_workers: 4,
            compression_level: 3,
            reserve_bytes: 2 * GIB,
            ttl_seconds: 24 * 3600,
            listen_port: 0,
            direct_host: None,
        }
    }
}

impl Options {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.sources.is_empty(), "at least one source is required");
        ensure!(
            (1..=64).contains(&self.chunk_mib),
            "chunk size must be 1..64 MiB"
        );
        ensure!(
            (1..=32).contains(&self.compression_workers),
            "compression workers must be 1..32"
        );
        ensure!(
            (1..=9).contains(&self.compression_level),
            "compression level must be 1..9"
        );
        ensure!(
            (60..=604800).contains(&self.ttl_seconds),
            "server lifetime must be 60 seconds..7 days"
        );
        ensure!(
            self.chunk_mib * self.compression_workers <= 512,
            "compression memory window is too large; lower chunk size or workers"
        );
        for path in &self.sources {
            ensure!(
                path.is_absolute(),
                "source must be absolute: {}",
                path.display()
            );
            ensure!(
                path.to_str().is_some(),
                "source root must be UTF-8; nested Unix names may contain arbitrary bytes"
            );
        }
        if let Some(host) = &self.direct_host {
            ensure!(
                !host.is_empty() && !host.chars().any(char::is_whitespace),
                "invalid direct host"
            );
            ensure!(
                !host.contains('/') && !host.contains('@'),
                "direct host must be a hostname or IP, without a URL or credentials"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct JobConfig {
    pub protocol: u32,
    pub id: String,
    pub token: String,
    pub options: Options,
    pub ca_der: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Descriptor {
    pub protocol: u32,
    pub id: String,
    pub job_dir: PathBuf,
    pub token: String,
    pub ca_der: Vec<u8>,
    pub port: u16,
}

impl Descriptor {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.protocol == PROTOCOL,
            "incompatible server protocol {}",
            self.protocol
        );
        ensure!(
            self.id.len() == 32 && self.id.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid job id"
        );
        ensure!(
            self.token.len() == 64 && self.token.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid job token"
        );
        ensure!(
            self.port > 0 && !self.ca_der.is_empty(),
            "incomplete endpoint description"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub index: u64,
    pub bytes: u64,
    pub raw_bytes: u64,
    pub sha256: String,
}

impl Chunk {
    pub fn validate(&self, expected: usize) -> Result<()> {
        ensure!(
            self.index == expected as u64,
            "missing or reordered chunk at {expected}"
        );
        ensure!(
            self.raw_bytes > 0 && self.raw_bytes <= 64 * MIB,
            "invalid raw chunk size"
        );
        ensure!(
            self.bytes > 0 && self.bytes <= 65 * MIB,
            "invalid compressed chunk size"
        );
        ensure!(
            self.sha256.len() == 64 && self.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid chunk checksum"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Compressing,
    Ready,
    Failed,
    Cancelled,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Status {
    pub protocol: u32,
    pub phase: Phase,
    pub chunks: usize,
    pub bytes: u64,
    pub raw_bytes: u64,
    pub manifest_sha256: Option<String>,
    pub warnings: Vec<String>,
    pub warning_count: usize,
    pub error: Option<String>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            protocol: PROTOCOL,
            phase: Phase::Starting,
            chunks: 0,
            bytes: 0,
            raw_bytes: 0,
            manifest_sha256: None,
            warnings: Vec::new(),
            warning_count: 0,
            error: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ManifestReply {
    pub status: Status,
    pub chunks: Vec<Chunk>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub protocol: u32,
    pub sources: Vec<PathBuf>,
    pub chunks: Vec<Chunk>,
    pub sha256: String,
    pub warnings: Vec<String>,
    pub warning_count: usize,
    pub metadata: String,
}

impl Manifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.protocol == PROTOCOL, "unsupported archive protocol");
        for (index, chunk) in self.chunks.iter().enumerate() {
            chunk.validate(index)?;
        }
        ensure!(!self.chunks.is_empty(), "empty chunk manifest");
        ensure!(
            crate::store::manifest_hash(&self.chunks)? == self.sha256,
            "manifest checksum mismatch"
        );
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Remote {
    pub host: String,
    pub port: Option<u16>,
    pub binary: String,
    pub shell: RemoteShell,
    pub sudo: bool,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum RemoteShell {
    #[default]
    Posix,
    Powershell,
}

impl Remote {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.host.is_empty()
                && !self.host.starts_with('-')
                && !self.host.chars().any(char::is_whitespace),
            "invalid SSH host"
        );
        ensure!(
            !self.binary.is_empty() && !self.binary.contains('\0'),
            "invalid remote executable"
        );
        if self.sudo && matches!(self.shell, RemoteShell::Powershell) {
            bail!("--sudo requires a POSIX remote shell");
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub protocol: u32,
    pub remote: Option<Remote>,
    pub descriptor: Descriptor,
    pub options: Options,
    pub direct_port: Option<u16>,
    pub reserve_bytes: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub protocol: u32,
    pub manifest_sha256: String,
    pub archive_entries: u64,
    pub compressed_bytes: u64,
    pub chunk_sha256_verified: bool,
    pub gzip_crc_verified: bool,
    pub warning_count: usize,
}
