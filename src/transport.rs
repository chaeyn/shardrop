use crate::{
    model::*,
    ssh::{self, Tunnel},
    store,
};
use anyhow::{ensure, Context, Result};
use std::{
    io::{Read, Write},
    path::Path,
    sync::Arc,
    time::Duration,
};

#[derive(Clone)]
pub struct Connection {
    pub agent: ureq::Agent,
    pub base: String,
    token: String,
}
impl Connection {
    fn new(session: &Session, host: &str, port: u16) -> Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(rustls::pki_types::CertificateDer::from(
            session.descriptor.ca_der.clone(),
        ))?;
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let agent = ureq::AgentBuilder::new()
            .tls_config(Arc::new(tls))
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(30))
            .timeout_write(Duration::from_secs(15))
            .redirects(0)
            .try_proxy_from_env(false)
            .build();
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        Ok(Self {
            agent,
            base: format!("https://{host}:{port}"),
            token: session.descriptor.token.clone(),
        })
    }
    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }
    pub fn manifest(&self, after: usize) -> Result<ManifestReply> {
        let response = self
            .agent
            .get(&format!("{}/manifest?after={after}", self.base))
            .set("Authorization", &self.auth())
            .call()
            .context("manifest request failed")?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 1024 * 1024, "manifest response too large");
        let reply: ManifestReply = serde_json::from_slice(&bytes)?;
        ensure!(
            reply.status.protocol == PROTOCOL,
            "incompatible source protocol"
        );
        Ok(reply)
    }
    pub fn download(&self, dir: &Path, chunk: &Chunk, reserve: u64) -> Result<()> {
        store::ensure_space(dir, reserve, chunk.bytes)?;
        let response = self
            .agent
            .get(&format!("{}/chunk/{}", self.base, chunk.index))
            .set("Authorization", &self.auth())
            .call()
            .context("chunk request failed")?;
        let mut temporary = tempfile::NamedTempFile::new_in(dir.join("chunks"))?;
        let count = std::io::copy(
            &mut response.into_reader().take(chunk.bytes + 1),
            &mut temporary,
        )?;
        ensure!(
            count == chunk.bytes,
            "chunk {} length mismatch",
            chunk.index
        );
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        ensure!(
            store::file_hash(temporary.path())? == chunk.sha256,
            "chunk {} checksum mismatch",
            chunk.index
        );
        temporary
            .persist(store::chunk_path(dir, chunk.index))
            .map_err(|e| e.error)?;
        Ok(())
    }
    pub fn finish(&self, hash: &str) -> Result<()> {
        self.agent
            .post(&format!("{}/finish", self.base))
            .set("Authorization", &self.auth())
            .send_string(hash)?;
        Ok(())
    }
    pub fn cancel(&self) -> Result<()> {
        self.agent
            .post(&format!("{}/cancel", self.base))
            .set("Authorization", &self.auth())
            .send_string("")?;
        Ok(())
    }
}
pub struct Transport {
    pub connection: Connection,
    _tunnel: Option<Tunnel>,
}
impl Transport {
    pub fn connect(session: &Session) -> Result<Self> {
        session.descriptor.validate()?;
        if let Some(host) = &session.options.direct_host {
            let connection = Connection::new(
                session,
                host,
                session.direct_port.unwrap_or(session.descriptor.port),
            )?;
            if connection.manifest(0).is_ok() {
                return Ok(Self {
                    connection,
                    _tunnel: None,
                });
            }
            eprintln!("Direct connection unavailable; opening SSH tunnel.");
        }
        if let Some(remote) = &session.remote {
            let tunnel = ssh::Tunnel::open(remote, session.descriptor.port)?;
            let connection = Connection::new(session, "127.0.0.1", tunnel.port)?;
            connection.manifest(0)?;
            Ok(Self {
                connection,
                _tunnel: Some(tunnel),
            })
        } else {
            let connection = Connection::new(session, "127.0.0.1", session.descriptor.port)?;
            connection.manifest(0)?;
            Ok(Self {
                connection,
                _tunnel: None,
            })
        }
    }
}
