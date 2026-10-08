use crate::model::*;
use anyhow::{ensure, Context, Result};
use base64::Engine;
use std::{
    io::Write,
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::Duration,
};

pub fn quote_posix(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn remote_command(remote: &Remote, args: &[&str]) -> String {
    match remote.shell {
        RemoteShell::Posix => {
            let prefix = if remote.sudo { "sudo -n -- " } else { "" };
            format!(
                "{prefix}{} {}",
                quote_posix(&remote.binary),
                args.iter()
                    .map(|s| quote_posix(s))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
        RemoteShell::Powershell => {
            let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
            let script = format!(
                "& {} {}; exit $LASTEXITCODE",
                quote(&remote.binary),
                args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ")
            );
            let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            format!(
                "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        }
    }
}
fn base(remote: &Remote) -> Command {
    let mut command = Command::new("ssh");
    command.args([
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
    ]);
    if let Some(port) = remote.port {
        command.arg("-p").arg(port.to_string());
    }
    command
}
pub fn invoke(remote: &Remote, args: &[&str], input: &[u8]) -> Result<Descriptor> {
    remote.validate()?;
    let mut command = base(remote);
    command
        .arg(&remote.host)
        .arg(remote_command(remote, args))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command
        .spawn()
        .context("cannot start OpenSSH; install ssh and test your host connection")?;
    child.stdin.take().unwrap().write_all(input)?;
    let output = child.wait_with_output()?;
    ensure!(
        output.status.success(),
        "remote command failed ({})",
        output.status
    );
    let descriptor: Descriptor = serde_json::from_slice(&output.stdout).context("remote did not return a valid endpoint; install the same shardrop version and keep shell startup output off stdout")?;
    descriptor.validate()?;
    Ok(descriptor)
}
pub fn start(remote: &Remote, options: &Options) -> Result<Descriptor> {
    invoke(remote, &["agent", "start"], &serde_json::to_vec(options)?)
}
pub fn resume(remote: &Remote, descriptor: &Descriptor) -> Result<Descriptor> {
    invoke(
        remote,
        &[
            "agent",
            "resume",
            "--job",
            descriptor.job_dir.to_str().context("non-UTF8 job path")?,
        ],
        &[],
    )
}
pub struct Tunnel {
    child: Child,
    pub port: u16,
}
impl Tunnel {
    pub fn open(remote: &Remote, server_port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let mut command = base(remote);
        command
            .args(["-N", "-o", "ExitOnForwardFailure=yes", "-L"])
            .arg(format!("127.0.0.1:{port}:127.0.0.1:{server_port}"))
            .arg(&remote.host)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let mut tunnel = Self {
            child: command.spawn()?,
            port,
        };
        for _ in 0..150 {
            ensure!(tunnel.child.try_wait()?.is_none(), "SSH tunnel exited");
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Ok(tunnel);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        anyhow::bail!("SSH tunnel did not become ready")
    }
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
