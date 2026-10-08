use crate::{
    archive::{self, Progress, Shared},
    model::*,
    store,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use axum::{
    body::Body,
    extract::State,
    http::{Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use rand::RngCore;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use std::{
    fs,
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
#[derive(Clone)]
struct HttpState {
    dir: PathBuf,
    job: JobConfig,
    shared: Shared,
    stop: Arc<AtomicBool>,
    remove: Arc<AtomicBool>,
}
async fn handle(State(context): State<HttpState>, request: Request<Body>) -> Response {
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = format!("Bearer {}", context.job.token);
    if !bool::from(supplied.as_bytes().ct_eq(expected.as_bytes())) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let url = request.uri().to_string();
    if request.method() == Method::GET && url.starts_with("/manifest?after=") {
        let after = url
            .trim_start_matches("/manifest?after=")
            .parse::<usize>()
            .unwrap_or(usize::MAX);
        let state = context.shared.lock().unwrap();
        if after > state.chunks.len() {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let reply = ManifestReply {
            status: state.status.clone(),
            chunks: state.chunks.iter().skip(after).take(256).cloned().collect(),
        };
        return Json(reply).into_response();
    }
    if request.method() == Method::GET && url.starts_with("/chunk/") {
        let index = url
            .trim_start_matches("/chunk/")
            .parse::<usize>()
            .unwrap_or(usize::MAX);
        let length = {
            let state = context.shared.lock().unwrap();
            match state.chunks.get(index) {
                Some(chunk) => chunk.bytes,
                None => return StatusCode::NOT_FOUND.into_response(),
            }
        };
        return match tokio::fs::File::open(store::chunk_path(&context.dir, index as u64)).await {
            Ok(file) => Response::builder()
                .header("content-length", length)
                .body(Body::from_stream(
                    tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024),
                ))
                .unwrap(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
    }
    if request.method() == Method::POST && (url == "/finish" || url == "/cancel") {
        let bytes = match tokio::time::timeout(
            Duration::from_secs(10),
            axum::body::to_bytes(request.into_body(), 1024),
        )
        .await
        {
            Ok(Ok(bytes)) => bytes,
            _ => return StatusCode::BAD_REQUEST.into_response(),
        };
        let body = match std::str::from_utf8(&bytes) {
            Ok(body) => body,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        };
        let state = context.shared.lock().unwrap();
        if url == "/finish"
            && (state.status.phase != Phase::Ready
                || state.status.manifest_sha256.as_deref() != Some(body.trim()))
        {
            return StatusCode::CONFLICT.into_response();
        }
        drop(state);
        context.remove.store(true, Ordering::Relaxed);
        context.stop.store(true, Ordering::Relaxed);
        return "ok\n".into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
pub fn run(dir: PathBuf) -> Result<()> {
    let job = config(&dir)?;
    let worker = store::lock(&dir.join("worker.lock"))?;
    store::atomic_json(&dir.join("worker-pid.json"), &std::process::id())?;
    let bind = if job.options.direct_host.is_some() {
        "0.0.0.0"
    } else {
        "127.0.0.1"
    };
    let listener = std::net::TcpListener::bind((bind, job.options.listen_port))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let network_result = runtime.block_on(async {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            dir.join("certificate.pem"),
            dir.join("key.pem"),
        )
        .await?;
        let context = HttpState {
            dir: dir.clone(),
            job,
            shared,
            stop: stop.clone(),
            remove: remove.clone(),
        };
        let app = Router::new()
            .fallback(any(handle))
            .with_state(context)
            .layer(tower::limit::ConcurrencyLimitLayer::new(32));
        let handle = axum_server::Handle::new();
        let shutdown_handle = handle.clone();
        let shutdown_stop = stop.clone();
        let watcher = tokio::spawn(async move {
            while !shutdown_stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            shutdown_stop.store(true, Ordering::Relaxed);
            shutdown_handle.graceful_shutdown(Some(Duration::from_secs(1)));
        });
        let result = axum_server::from_tcp_rustls(listener, tls)?
            .handle(handle)
            .serve(app.into_make_service())
            .await;
        watcher.abort();
        result
    });
    drop(runtime);
    stop.store(true, Ordering::Relaxed);
    if let Some(thread) = producer {
        thread
            .join()
            .map_err(|_| anyhow!("archive worker panicked"))?;
    }
    drop(worker);
    if remove.load(Ordering::Relaxed) {
        config(&dir)?;
        fs::remove_dir_all(&dir)?;
    } else {
        let _ = fs::remove_file(dir.join("endpoint.json"));
    }
    network_result?;
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
