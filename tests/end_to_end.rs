use assert_cmd::Command;
use shardrop::{archive, model::*, store, transport::Transport};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

fn cli() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("shardrop"))
}
fn fixture(root: &Path) -> PathBuf {
    let source = root.join("source");
    fs::create_dir_all(source.join("empty")).unwrap();
    fs::write(source.join("한글 file.txt"), b"hello\n").unwrap();
    let mut state = 42u64;
    let data: Vec<u8> = (0..3 * 1024 * 1024 + 19)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    fs::write(source.join("random.bin"), data).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("한글 file.txt", source.join("symbolic")).unwrap();
        fs::hard_link(source.join("random.bin"), source.join("hardlink.bin")).unwrap();
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt;
            fs::write(
                source.join(std::ffi::OsString::from_vec(vec![b'n', 0xff])),
                b"nonutf8",
            )
            .unwrap();
        }
    }
    source
}
fn pack(source: &Path, output: &Path) {
    cli()
        .arg("pack")
        .arg(source)
        .arg("-o")
        .arg(output)
        .args([
            "--chunk-mib",
            "1",
            "--compression-workers",
            "2",
            "--reserve-mib",
            "0",
            "--keep-source",
            "--json",
        ])
        .assert()
        .success();
}
fn cleanup(output: &Path) {
    let session: Session = store::json(&output.join("session.json")).unwrap();
    cli().arg("cleanup").arg(output).assert().success();
    for _ in 0..100 {
        if !session.descriptor.job_dir.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("source staging was not removed");
}
#[test]
fn roundtrip_repair_and_authentication() {
    let temp = tempfile::tempdir().unwrap();
    let source = fixture(temp.path());
    let output = temp.path().join("backup");
    pack(&source, &output);
    let manifest: Manifest = store::json(&output.join("manifest.json")).unwrap();
    assert!(manifest.chunks.len() >= 4);
    let good = store::chunk_path(&output, 0);
    let before = fs::metadata(&good).unwrap().modified().unwrap();
    fs::remove_file(store::chunk_path(&output, 1)).unwrap();
    fs::write(store::chunk_path(&output, 2), b"broken").unwrap();
    cli().arg("verify").arg(&output).assert().failure();
    cli()
        .arg("resume")
        .arg(&output)
        .arg("--keep-source")
        .assert()
        .success();
    assert_eq!(before, fs::metadata(good).unwrap().modified().unwrap());
    let restored = temp.path().join("restored");
    cli()
        .arg("restore")
        .arg(&output)
        .arg("--to")
        .arg(&restored)
        .assert()
        .success();
    assert_eq!(
        fs::read(source.join("random.bin")).unwrap(),
        fs::read(restored.join("source/random.bin")).unwrap()
    );
    assert!(restored.join("source/empty").is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::read_link(restored.join("source/symbolic")).unwrap(),
            PathBuf::from("한글 file.txt")
        );
        assert_eq!(
            fs::metadata(restored.join("source/random.bin"))
                .unwrap()
                .ino(),
            fs::metadata(restored.join("source/hardlink.bin"))
                .unwrap()
                .ino()
        );
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt;
            assert_eq!(
                fs::read(
                    restored
                        .join("source")
                        .join(std::ffi::OsString::from_vec(vec![b'n', 0xff]))
                )
                .unwrap(),
                b"nonutf8"
            );
        }
    }
    let mut session: Session = store::json(&output.join("session.json")).unwrap();
    session.descriptor.token = "0".repeat(64);
    assert!(Transport::connect(&session).is_err());
    let mut session: Session = store::json(&output.join("session.json")).unwrap();
    session.descriptor.ca_der = vec![1, 2, 3];
    assert!(Transport::connect(&session).is_err());
    cleanup(&output);
    assert!(source.join("random.bin").exists());
    cli().arg("verify").arg(&output).assert().success();
}
#[test]
fn exclude_and_output_guard() {
    let temp = tempfile::tempdir().unwrap();
    let source = fixture(temp.path());
    cli()
        .arg("pack")
        .arg(&source)
        .arg("-o")
        .arg(source.join("backup"))
        .args(["--reserve-mib", "0"])
        .assert()
        .failure();
    fs::remove_dir_all(source.join("backup")).unwrap();
    let output = temp.path().join("backup");
    cli()
        .arg("pack")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .args([
            "--reserve-mib",
            "0",
            "--exclude",
            "source/empty",
            "--keep-source",
        ])
        .assert()
        .success();
    let dest = temp.path().join("restored");
    archive::restore(&output, &dest).unwrap();
    assert!(!dest.join("source/empty").exists());
    cleanup(&output);
}
#[test]
fn truncated_journal_recovers_but_committed_corruption_fails() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    fs::write(&path, b"1\n2\n{\"").unwrap();
    assert_eq!(store::records::<u64>(&path).unwrap(), vec![1, 2]);
    assert_eq!(fs::read(&path).unwrap(), b"1\n2\n");
    fs::write(&path, b"1\ninvalid\n").unwrap();
    assert!(store::records::<u64>(&path).is_err());
}
fn crafted_archive(dir: &Path, raw: &[u8], bad_crc: bool) {
    store::private_dir(&dir.join("chunks")).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(raw).unwrap();
    let mut bytes = encoder.finish().unwrap();
    if bad_crc {
        let at = bytes.len() - 8;
        bytes[at] ^= 1;
    }
    fs::write(store::chunk_path(dir, 0), &bytes).unwrap();
    let chunks = vec![Chunk {
        index: 0,
        bytes: bytes.len() as u64,
        raw_bytes: raw.len() as u64,
        sha256: store::bytes_hash(&bytes),
    }];
    let manifest = Manifest {
        protocol: PROTOCOL,
        sources: vec![],
        sha256: store::manifest_hash(&chunks).unwrap(),
        chunks,
        warnings: vec![],
        warning_count: 0,
        metadata: String::new(),
    };
    store::atomic_json(&dir.join("manifest.json"), &manifest).unwrap();
}
#[test]
fn checks_gzip_trailer_after_tar_end() {
    let temp = tempfile::tempdir().unwrap();
    crafted_archive(temp.path(), &[0; 1024], true);
    assert!(archive::verify(temp.path()).is_err());
}
#[test]
fn rejects_traversal() {
    let temp = tempfile::tempdir().unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.as_mut_bytes()[..10].copy_from_slice(b"../escaped");
    header.set_cksum();
    let mut raw = header.as_bytes().to_vec();
    raw.extend_from_slice(&[0; 1024]);
    crafted_archive(temp.path(), &raw, false);
    assert!(archive::verify(temp.path()).is_err());
}
#[test]
fn shell_quoting_is_literal() {
    assert_eq!(
        shardrop::ssh::quote_posix("a'b $HOME;$(id)"),
        "'a'\\''b $HOME;$(id)'"
    );
    let remote = Remote {
        host: "-oProxyCommand=bad".into(),
        port: None,
        binary: "shardrop".into(),
        shell: RemoteShell::Posix,
        sudo: false,
    };
    assert!(remote.validate().is_err());
}

#[test]
fn symlink_cannot_redirect_later_extraction() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    let mut link = tar::Header::new_gnu();
    link.set_size(0);
    link.set_mode(0o777);
    link.set_entry_type(tar::EntryType::Symlink);
    builder
        .append_link(&mut link, "escape", outside.path())
        .unwrap();
    let mut file = tar::Header::new_gnu();
    file.set_size(7);
    file.set_mode(0o644);
    builder
        .append_data(&mut file, "escape/payload", &b"outside"[..])
        .unwrap();
    builder.finish().unwrap();
    let bytes = builder.into_inner().unwrap();
    let backup = temp.path().join("backup");
    crafted_archive(&backup, &bytes, false);
    assert!(archive::restore(&backup, &temp.path().join("restored")).is_err());
    assert!(!outside.path().join("payload").exists());
}
#[cfg(unix)]
#[test]
fn readonly_directory_and_sparse_bytes_restore() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("readonly")).unwrap();
    fs::write(source.join("readonly/file"), b"data").unwrap();
    let sparse = fs::File::create(source.join("sparse")).unwrap();
    sparse.set_len(2 * 1024 * 1024).unwrap();
    drop(sparse);
    fs::set_permissions(source.join("readonly"), fs::Permissions::from_mode(0o555)).unwrap();
    let output = temp.path().join("backup");
    pack(&source, &output);
    let restored = temp.path().join("restored");
    archive::restore(&output, &restored).unwrap();
    assert_eq!(
        fs::read(restored.join("source/readonly/file")).unwrap(),
        b"data"
    );
    assert_eq!(
        store::file_hash(&source.join("sparse")).unwrap(),
        store::file_hash(&restored.join("source/sparse")).unwrap()
    );
    cleanup(&output);
    fs::set_permissions(source.join("readonly"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(
        restored.join("source/readonly"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

#[cfg(unix)]
fn kill_worker(session: &Session) {
    let pid: u32 = store::json(&session.descriptor.job_dir.join("worker-pid.json")).unwrap();
    assert!(std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    for _ in 0..100 {
        if store::lock(&session.descriptor.job_dir.join("worker.lock")).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("worker did not release its lock");
}
#[cfg(unix)]
#[test]
fn completed_source_worker_restarts_without_recompressing() {
    let temp = tempfile::tempdir().unwrap();
    let source = fixture(temp.path());
    let output = temp.path().join("backup");
    pack(&source, &output);
    let session: Session = store::json(&output.join("session.json")).unwrap();
    let source_chunk = store::chunk_path(&session.descriptor.job_dir, 0);
    let before = fs::metadata(&source_chunk).unwrap().modified().unwrap();
    kill_worker(&session);
    fs::remove_file(store::chunk_path(&output, 0)).unwrap();
    cli()
        .arg("resume")
        .arg(&output)
        .arg("--keep-source")
        .assert()
        .success();
    assert_eq!(
        before,
        fs::metadata(source_chunk).unwrap().modified().unwrap()
    );
    cleanup(&output);
}
#[cfg(unix)]
#[test]
fn interrupted_source_refuses_to_mix_snapshots() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    // A large raw batch leaves time to interrupt before the first commit.
    let mut file = fs::File::create(source.join("large")).unwrap();
    let data: Vec<u8> = (0..1024 * 1024).map(|_| rand::random()).collect();
    for _ in 0..128 {
        file.write_all(&data).unwrap();
    }
    drop(file);
    let options = Options {
        sources: vec![source],
        chunk_mib: 64,
        compression_workers: 1,
        compression_level: 9,
        reserve_bytes: 0,
        ..Options::default()
    };
    let output = cli()
        .args(["agent", "start"])
        .write_stdin(serde_json::to_vec(&options).unwrap())
        .output()
        .unwrap();
    assert!(output.status.success());
    let descriptor: Descriptor = serde_json::from_slice(&output.stdout).unwrap();
    let session = Session {
        protocol: PROTOCOL,
        remote: None,
        descriptor,
        options,
        direct_port: None,
        reserve_bytes: 0,
    };
    kill_worker(&session);
    let output = cli()
        .args(["agent", "resume", "--job"])
        .arg(&session.descriptor.job_dir)
        .output()
        .unwrap();
    assert!(output.status.success());
    let descriptor: Descriptor = serde_json::from_slice(&output.stdout).unwrap();
    let session = Session {
        descriptor,
        ..session
    };
    let transport = Transport::connect(&session).unwrap();
    let reply = transport.connection.manifest(0).unwrap();
    assert_eq!(reply.status.phase, Phase::Failed);
    transport.connection.cancel().unwrap();
    for _ in 0..100 {
        if !session.descriptor.job_dir.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("failed source staging was not removed");
}
