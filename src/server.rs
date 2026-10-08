use crate::{
    archive::{self, Progress, Shared},
    model::*,
    store,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use rand::RngCore;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tiny_http::{Response, Server, SslConfig, StatusCode};

fn random_hex(size: usize) -> String {
    let mut value = vec![0; size];
    rand::thread_rng().fill_bytes(&mut value);
    hex::encode(value)
}
fn config(dir: &Path) -> Result<JobConfig> {
    let value: JobConfig = store::json(&dir.join("job.json"))?;
    ensure!(
        value.protocol == PROTOCOL
            && value.id.len() == 32
            && value.id.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid job marker"
    );
    ensure!(
        dir.file_name().and_then(|s| s.to_str()) == Some(&format!("shardrop-{}", value.id)),
        "job directory does not match marker"
    );
    ensure!(
        !fs::symlink_metadata(dir)?.file_type().is_symlink(),
        "job directory cannot be a symlink"
    );
    Ok(value)
}
pub fn start(options: Options) -> Result<Descriptor> {
    options.validate()?;
    let id = random_hex(16);
    let dir = std::env::temp_dir().join(format!("shardrop-{id}"));
    fs::create_dir(&dir)?;
    store::private_dir(&dir)?;
    let result = (|| {
        let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let ca_key = KeyPair::generate()?;
        let ca = ca_params.self_signed(&ca_key)?;
        let mut names = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
        if let Some(host) = &options.direct_host {
            names.push(host.clone());
        }
        let leaf_key = KeyPair::generate()?;
        let leaf = CertificateParams::new(names)?.signed_by(&leaf_key, &ca, &ca_key)?;
        store::atomic_bytes(&dir.join("certificate.pem"), leaf.pem().as_bytes())?;
        store::atomic_bytes(&dir.join("key.pem"), leaf_key.serialize_pem().as_bytes())?;
        let job = JobConfig {
            protocol: PROTOCOL,
            id,
            token: random_hex(32),
            options,
            ca_der: ca.der().to_vec(),
        };
        store::atomic_json(&dir.join("job.json"), &job)?;
        launch(&dir)
    })();
    // A failed launch can already have spawned a worker; keep its marked directory for diagnosis.
    result.with_context(|| format!("server startup failed; job directory: {}", dir.display()))
}
pub fn launch(dir: &Path) -> Result<Descriptor> {
    let job = config(dir)?;
    let _bootstrap = store::lock(&dir.join("bootstrap.lock"))?;
    if let Ok(lock) = store::lock(&dir.join("worker.lock")) {
        let _ = fs::remove_file(dir.join("endpoint.json"));
        drop(lock);
        let log = store::private_file(&dir.join("server.log"))?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("agent")
            .arg("run")
            .arg("--job")
            .arg(dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Rust's Windows process launcher inherits existing inheritable handles.
            // Do not let this long-lived worker retain the caller's capture pipes.
            // The explicit log/null Stdio handles are duplicated by Command below.
            use windows_sys::Win32::{
                Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE},
                System::Console::{
                    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
                },
            };
            for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                // SAFETY: GetStdHandle returns borrowed process handles. We only
                // clear an inheritance flag; we neither close nor take ownership.
                unsafe {
                    let handle = GetStdHandle(stream);
                    if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                        ensure!(
                            SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) != 0,
                            "cannot disable standard-handle inheritance: {}",
                            std::io::Error::last_os_error()
                        );
                    }
                }
            }
            command.creation_flags(0x08000000 | 0x00000200);
        }
        command.spawn()?;
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(endpoint) = store::json::<Descriptor>(&dir.join("endpoint.json")) {
            endpoint.validate()?;
            ensure!(endpoint.id == job.id, "endpoint identity mismatch");
            return Ok(endpoint);
        }
        ensure!(
            Instant::now() < deadline,
            "server did not become ready; inspect {}",
            dir.join("server.log").display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn reply_json(request: tiny_http::Request, body: &impl serde::Serialize) {
    let body = serde_json::to_vec(body).unwrap_or_default();
    let _ =
        request.respond(Response::from_data(body).with_header(
            tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
        ));
}
fn handle(
    mut request: tiny_http::Request,
    dir: &Path,
    job: &JobConfig,
    shared: &Shared,
    stop: &AtomicBool,
    remove: &AtomicBool,
) {
    let supplied = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .map(|h| h.value.as_str())
        .unwrap_or("");
    let expected = format!("Bearer {}", job.token);
    if !bool::from(supplied.as_bytes().ct_eq(expected.as_bytes())) {
        let _ = request.respond(Response::empty(StatusCode(401)));
        return;
    }
    let url = request.url().to_owned();
    if request.method() == &tiny_http::Method::Get && url.starts_with("/manifest?after=") {
        let after = url
            .trim_start_matches("/manifest?after=")
            .parse::<usize>()
            .unwrap_or(usize::MAX);
        let state = shared.lock().unwrap();
        if after > state.chunks.len() {
            let _ = request.respond(Response::empty(StatusCode(400)));
            return;
        }
        let reply = ManifestReply {
            status: state.status.clone(),
            chunks: state.chunks.iter().skip(after).take(256).cloned().collect(),
        };
        drop(state);
        reply_json(request, &reply);
    } else if request.method() == &tiny_http::Method::Get && url.starts_with("/chunk/") {
        let index = url
            .trim_start_matches("/chunk/")
            .parse::<usize>()
            .unwrap_or(usize::MAX);
        if index >= shared.lock().unwrap().chunks.len() {
            let _ = request.respond(Response::empty(StatusCode(404)));
            return;
        }
        match File::open(store::chunk_path(dir, index as u64)) {
            Ok(file) => {
                let _ = request.respond(Response::from_file(file));
            }
            Err(_) => {
                let _ = request.respond(Response::empty(StatusCode(500)));
            }
        }
    } else if request.method() == &tiny_http::Method::Post && (url == "/finish" || url == "/cancel")
    {
        let mut body = String::new();
        if request
            .as_reader()
            .take(1024)
            .read_to_string(&mut body)
            .is_err()
        {
            let _ = request.respond(Response::empty(StatusCode(400)));
            return;
        }
        let state = shared.lock().unwrap();
        if url == "/finish"
            && (state.status.phase != Phase::Ready
                || state.status.manifest_sha256.as_deref() != Some(body.trim()))
        {
            let _ = request.respond(Response::empty(StatusCode(409)));
            return;
        }
        drop(state);
        remove.store(true, Ordering::Relaxed);
        stop.store(true, Ordering::Relaxed);
        let _ = request.respond(Response::from_string("ok\n"));
    } else {
        let _ = request.respond(Response::empty(StatusCode(404)));
    }
}
pub fn run(dir: PathBuf) -> Result<()> {
    let job = config(&dir)?;
    let worker = store::lock(&dir.join("worker.lock"))?;
    store::atomic_json(&dir.join("worker-pid.json"), &std::process::id())?;
    let ssl = SslConfig {
        certificate: fs::read(dir.join("certificate.pem"))?,
        private_key: fs::read(dir.join("key.pem"))?,
    };
    let bind = if job.options.direct_host.is_some() {
        "0.0.0.0"
    } else {
        "127.0.0.1"
    };
    let server = Arc::new(
        Server::https((bind, job.options.listen_port), ssl)
            .map_err(|e| anyhow!("TLS server: {e}"))?,
    );
    let port = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| anyhow!("no TCP address"))?
        .port();
    let mut progress = Progress::default();
    if dir.join("status.json").exists() {
        progress.status = store::json(&dir.join("status.json"))?;
        progress.chunks = store::records(&dir.join("chunks.jsonl"))?;
        if progress.status.phase != Phase::Ready {
            progress.status.phase = Phase::Failed;
            progress.status.error = Some("source compression was interrupted; start a new backup to avoid mixing file versions".into());
        }
    }
    let should_produce = progress.status.phase == Phase::Starting;
    if should_produce {
        // Persist the phase before any producer writes: a crash before its first chunk
        // must not make a partially started job look like a new snapshot.
        progress.status.phase = Phase::Compressing;
        store::atomic_json(&dir.join("status.json"), &progress.status)?;
    } else if progress.status.phase == Phase::Ready {
        for (index, chunk) in progress.chunks.iter().enumerate() {
            chunk.validate(index)?;
        }
        ensure!(
            progress.status.chunks == progress.chunks.len(),
            "source journal count mismatch"
        );
        ensure!(
            progress.status.manifest_sha256.as_deref()
                == Some(store::manifest_hash(&progress.chunks)?.as_str()),
            "source manifest hash mismatch"
        );
    }
    let shared = Arc::new(Mutex::new(progress));
    let stop = Arc::new(AtomicBool::new(false));
    let remove = Arc::new(AtomicBool::new(false));
    let producer = if should_produce {
        let (dir, options, shared, stop) = (
            dir.clone(),
            job.options.clone(),
            shared.clone(),
            stop.clone(),
        );
        Some(std::thread::spawn(move || {
            if let Err(error) = archive::produce(&dir, &options, shared.clone(), stop) {
                let mut state = shared.lock().unwrap();
                state.status.phase = Phase::Failed;
                state.status.error = Some(format!("{error:#}"));
                let _ = store::atomic_json(&dir.join("status.json"), &state.status);
            }
        }))
    } else {
        None
    };
    let endpoint = Descriptor {
        protocol: PROTOCOL,
        id: job.id.clone(),
        job_dir: dir.clone(),
        token: job.token.clone(),
        ca_der: job.ca_der.clone(),
        port,
    };
    store::atomic_json(&dir.join("endpoint.json"), &endpoint)?;
    let deadline = Instant::now() + Duration::from_secs(job.options.ttl_seconds);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let (server, shared, stop, remove, dir, job) =
                (&server, &shared, &stop, &remove, &dir, &job);
            scope.spawn(move || {
                while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                    match server.recv_timeout(Duration::from_millis(200)) {
                        Ok(Some(request)) => handle(request, dir, job, shared, stop, remove),
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
                stop.store(true, Ordering::Relaxed);
            });
        }
    });
    if let Some(thread) = producer {
        thread
            .join()
            .map_err(|_| anyhow!("archive worker panicked"))?;
    }
    drop(server);
    drop(worker);
    if remove.load(Ordering::Relaxed) {
        config(&dir)?;
        fs::remove_dir_all(&dir)?;
    } else {
        let _ = fs::remove_file(dir.join("endpoint.json"));
    }
    Ok(())
}
pub fn read_options() -> Result<Options> {
    let mut input = String::new();
    std::io::stdin()
        .take(1024 * 1024)
        .read_to_string(&mut input)?;
    if input.is_empty() {
        bail!("expected options JSON on stdin");
    }
    Ok(serde_json::from_str(&input)?)
}
pub fn emit_descriptor(endpoint: &Descriptor) -> Result<()> {
    serde_json::to_writer(std::io::stdout(), endpoint)?;
    std::io::stdout().write_all(b"\n")?;
    Ok(())
}
