use crate::{archive, client, model::*, server};
use anyhow::Result;
use clap::{Args, CommandFactory, Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode, sync::atomic::Ordering};

#[derive(Parser)]
#[command(
    name = "shardrop",
    version,
    about = "Resumable compressed backups over SSH and authenticated LAN HTTPS"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Print the final result as JSON; progress stays on stderr"
    )]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Args)]
struct ArchiveOptions {
    #[arg(required = true)]
    sources: Vec<PathBuf>,
    #[arg(short = 'o', long)]
    output: PathBuf,
    #[arg(long, default_value_t = 8)]
    chunk_mib: usize,
    #[arg(long, default_value_t = 4)]
    compression_workers: usize,
    #[arg(long, default_value_t = 3)]
    level: u32,
    #[arg(
        long,
        default_value_t = 2048,
        help = "Keep this many MiB free on source and destination"
    )]
    reserve_mib: u64,
    #[arg(long, default_value_t = 86400)]
    ttl_seconds: u64,
    #[arg(long)]
    exclude: Vec<String>,
    #[arg(short = 'j', long, default_value_t = 8)]
    jobs: usize,
    #[arg(
        long,
        help = "Retain source staging files after verification, until explicit cleanup"
    )]
    keep_source: bool,
}
impl ArchiveOptions {
    fn options(&self) -> Options {
        Options {
            sources: self.sources.clone(),
            excludes: self.exclude.clone(),
            chunk_mib: self.chunk_mib,
            compression_workers: self.compression_workers,
            compression_level: self.level,
            reserve_bytes: self.reserve_mib.saturating_mul(MIB),
            ttl_seconds: self.ttl_seconds,
            ..Options::default()
        }
    }
}
#[derive(Subcommand)]
enum Commands {
    /// Copy remote paths into a resumable local archive directory
    Copy {
        host: String,
        #[command(flatten)]
        archive: ArchiveOptions,
        #[arg(long, default_value = "shardrop")]
        remote_bin: String,
        #[arg(long)]
        ssh_port: Option<u16>,
        #[arg(long, value_enum, default_value = "posix")]
        remote_shell: RemoteShell,
        #[arg(
            long,
            help = "Use sudo -n remotely; requires configured noninteractive permission"
        )]
        sudo: bool,
        #[arg(long, help = "Direct data hostname/IP; SSH tunnel is the fallback")]
        direct: Option<String>,
        #[arg(long, default_value_t = 0)]
        listen_port: u16,
        #[arg(long)]
        direct_port: Option<u16>,
    },
    /// Archive local paths using the same compression and verification pipeline
    Pack {
        #[command(flatten)]
        archive: ArchiveOptions,
    },
    /// Reuse valid local chunks and fetch missing or damaged chunks
    Resume {
        directory: PathBuf,
        #[arg(short = 'j', long, default_value_t = 8)]
        jobs: usize,
        #[arg(long)]
        keep_source: bool,
    },
    /// Verify SHA-256, gzip CRCs and tar structure without a network connection
    Verify { directory: PathBuf },
    /// Verify and extract into an empty directory
    Restore {
        directory: PathBuf,
        #[arg(long)]
        to: PathBuf,
    },
    /// Remove this job's source staging files after verification
    Cleanup {
        directory: PathBuf,
        #[arg(long, help = "Discard an unfinished source job; local chunks remain")]
        cancel: bool,
    },
    /// Show platform and SSH availability
    Doctor,
    /// Generate shell completions
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Generate a manual page on stdout
    Man,
    #[command(hide = true)]
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
}
#[derive(Subcommand)]
enum AgentAction {
    Start,
    Resume {
        #[arg(long)]
        job: PathBuf,
    },
    Run {
        #[arg(long)]
        job: PathBuf,
    },
}
fn result(receipt: &Receipt, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(receipt)?);
    } else {
        println!(
            "Verified {} entries, {} compressed bytes, {} warnings.",
            receipt.archive_entries, receipt.compressed_bytes, receipt.warning_count
        );
    }
    Ok(())
}
fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Copy {
            host,
            archive,
            remote_bin,
            ssh_port,
            remote_shell,
            sudo,
            direct,
            listen_port,
            direct_port,
        } => {
            let remote = Remote {
                host,
                port: ssh_port,
                binary: remote_bin,
                shell: remote_shell,
                sudo,
            };
            remote.validate()?;
            let mut options = archive.options();
            options.direct_host = direct;
            options.listen_port = listen_port;
            result(
                &client::create(
                    &archive.output,
                    options,
                    Some(remote),
                    archive.jobs,
                    direct_port,
                    archive.keep_source,
                )?,
                cli.json,
            )
        }
        Commands::Pack { archive } => {
            let mut options = archive.options();
            for source in &mut options.sources {
                if !source.is_absolute() {
                    *source = std::env::current_dir()?.join(&source);
                }
            }
            result(
                &client::create(
                    &archive.output,
                    options,
                    None,
                    archive.jobs,
                    None,
                    archive.keep_source,
                )?,
                cli.json,
            )
        }
        Commands::Resume {
            directory,
            jobs,
            keep_source,
        } => result(&client::resume(&directory, jobs, keep_source)?, cli.json),
        Commands::Verify { directory } => result(&archive::verify(&directory)?, cli.json),
        Commands::Restore { directory, to } => {
            result(&archive::restore(&directory, &to)?, cli.json)
        }
        Commands::Cleanup { directory, cancel } => {
            client::cleanup(&directory, cancel)?;
            println!("Source cleanup requested.");
            Ok(())
        }
        Commands::Doctor => {
            println!(
                "shardrop {}\nplatform: {} / {}\nssh: {}",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH,
                if std::process::Command::new("ssh").arg("-V").output().is_ok() {
                    "available"
                } else {
                    "missing (local pack still works)"
                }
            );
            Ok(())
        }
        Commands::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "shardrop",
                &mut std::io::stdout(),
            );
            Ok(())
        }
        Commands::Man => {
            clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout())?;
            Ok(())
        }
        Commands::Agent { action } => match action {
            AgentAction::Start => server::emit_descriptor(&server::start(server::read_options()?)?),
            AgentAction::Resume { job } => server::emit_descriptor(&server::launch(&job)?),
            AgentAction::Run { job } => server::run(job),
        },
    }
}
pub fn entry() -> ExitCode {
    let cli = Cli::parse();
    let _ = rustls::crypto::ring::default_provider().install_default();
    if !matches!(cli.command, Commands::Agent { .. }) {
        let _ = ctrlc::set_handler(|| client::INTERRUPTED.store(true, Ordering::Relaxed));
    }
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(if client::INTERRUPTED.load(Ordering::Relaxed) {
                130
            } else {
                1
            })
        }
    }
}
