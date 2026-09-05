use super::*;

#[test]
fn boundary_stale_capture_pid_cannot_adopt_sibling_namespace() {
    use std::os::unix::fs::PermissionsExt;
    let mut sibling = Fixture::new();
    let h = sibling.prepare();
    sibling.runtime.start(&h).unwrap();
    wait_file(&sibling.config.workspace.join("beats"));
    let f = Fixture::new();
    let wrapper = f._root.path().join("stale-bwrap");
    std::fs::write(&wrapper, format!(
        "#!/usr/bin/python3\nimport os,sys,time,json\nif 'check' in sys.argv:\n os.execv('/usr/bin/bwrap',['/usr/bin/bwrap']+sys.argv[1:])\nos.write(4,json.dumps({{'child-pid':{}}}).encode())\nos.close(4)\ntime.sleep(1)\n", h.init.pid
    )).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let runtime = Runtime::new(RuntimeConfig {
        state_root: f._root.path().join("decoy-state"),
        helper: env!("CARGO_BIN_EXE_calm-worker-boundary").into(),
        bwrap: wrapper,
        timeout: Duration::from_secs(4),
    })
    .unwrap();
    assert!(runtime.prepare("stale-capture", &f.config).is_err());
    assert_eq!(sibling.runtime.probe(&h).unwrap(), BoundaryState::Running);
}

#[test]
fn boundary_stdio_completion_is_not_process_quiescence() {
    let mut f = Fixture::new();
    let h = f.prepare();
    let mut stream = f.runtime.connect_stdio(&h).unwrap();
    f.runtime.start(&h).unwrap();
    wait_file(&f.config.workspace.join("beats"));
    stream.write_all(b"close-stdio\n").unwrap();
    wait_file(&f.config.workspace.join("stdio-closed"));
    let before = size(&f.config.workspace.join("beats"));
    std::thread::sleep(Duration::from_millis(100));
    assert!(size(&f.config.workspace.join("beats")) > before);
    assert_eq!(f.runtime.probe(&h).unwrap(), BoundaryState::Running);
}

#[test]
fn boundary_concurrent_prepare_and_start_share_one_namespace() {
    let mut f = Fixture::new();
    std::thread::scope(|scope| {
        let a = scope.spawn(|| f.runtime.prepare("run-b", &f.config).unwrap());
        let b = scope.spawn(|| f.runtime.prepare("run-b", &f.config).unwrap());
        assert_eq!(a.join().unwrap(), b.join().unwrap());
    });
    let h = f.prepare();
    std::thread::scope(|scope| {
        let a = scope.spawn(|| f.runtime.start(&h).unwrap());
        let b = scope.spawn(|| f.runtime.start(&h).unwrap());
        a.join().unwrap();
        b.join().unwrap();
    });
    wait_file(&f.config.workspace.join("beats"));
    assert_eq!(f.runtime.prepare("run-b", &f.config).unwrap(), h);
}

#[test]
fn boundary_lost_response_then_stopping_prepared_run_preserves_closed_admission() {
    let mut f = Fixture::new();
    let h = f.prepare();
    let repeated = f.runtime.prepare("run-b", &f.config).unwrap();
    assert_eq!(h, repeated);
    assert!(matches!(
        f.runtime.stop(&repeated, Duration::from_secs(3)).unwrap(),
        BoundaryState::Quiesced(_)
    ));
    assert!(!f.config.workspace.join("started").exists());
    assert!(f.runtime.start(&h).is_err());
}

#[test]
fn boundary_natural_exit_drains_final_provider_response() {
    let mut f = Fixture::new();
    let h = f.prepare();
    let mut stream = f.runtime.connect_stdio(&h).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    f.runtime.start(&h).unwrap();
    stream.write_all(b"final-exit\n").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    assert_eq!(line, "final response\n");
    wait_quiesced(&f.runtime, &h);
}

#[test]
fn boundary_provider_exit_reaps_background_writer() {
    let mut f = Fixture::new();
    let h = f.prepare();
    let mut stream = f.runtime.connect_stdio(&h).unwrap();
    f.runtime.start(&h).unwrap();
    wait_file(&f.config.workspace.join("beats"));
    stream.write_all(b"exit\n").unwrap();
    wait_quiesced(&f.runtime, &h);
    let bytes = size(&f.config.workspace.join("beats"));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(size(&f.config.workspace.join("beats")), bytes);
}

fn wait_quiesced(runtime: &Runtime, handle: &BoundaryHandle) {
    let until = Instant::now() + Duration::from_secs(3);
    while !matches!(runtime.probe(handle).unwrap(), BoundaryState::Quiesced(_))
        && Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(
        runtime.probe(handle).unwrap(),
        BoundaryState::Quiesced(_)
    ));
}

#[test]
fn boundary_caller_reconnect_does_not_restart_provider() {
    let mut f = Fixture::new();
    let h = f.prepare();
    f.runtime.start(&h).unwrap();
    wait_file(&f.config.workspace.join("beats"));
    let resumed = Runtime::new(RuntimeConfig {
        state_root: f._root.path().join("state"),
        helper: env!("CARGO_BIN_EXE_calm-worker-boundary").into(),
        bwrap: "/usr/bin/bwrap".into(),
        timeout: Duration::from_secs(4),
    })
    .unwrap();
    assert_eq!(resumed.prepare("run-b", &f.config).unwrap(), h);
    resumed.start(&h).unwrap();
    assert_eq!(resumed.probe(&h).unwrap(), BoundaryState::Running);
    assert!(matches!(
        resumed.stop(&h, Duration::from_secs(3)).unwrap(),
        BoundaryState::Quiesced(_)
    ));
}

#[test]
fn boundary_missing_evidence_is_unknown_and_cannot_reallocate() {
    let mut f = Fixture::new();
    let h = f.prepare();
    f.runtime.start(&h).unwrap();
    wait_file(&f.config.workspace.join("beats"));
    std::fs::remove_file(f._root.path().join("state/run-b/record.json")).unwrap();
    assert!(matches!(
        f.runtime.probe(&h).unwrap(),
        BoundaryState::Unknown(_)
    ));
    assert!(f.runtime.prepare("run-b", &f.config).is_err());
    assert!(f.runtime.start(&h).is_err());
}

#[test]
fn boundary_launcher_death_before_start_never_executes_provider() {
    let mut f = Fixture::new();
    let h = f.prepare();
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(f._root.path().join("state/run-b/record.json")).unwrap(),
    )
    .unwrap();
    let pid = record["phase"]["Prepared"]["launcher_pid"]
        .as_i64()
        .unwrap() as i32;
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    assert!(raw >= 0);
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        },
        0
    );
    wait_quiesced(&f.runtime, &h);
    assert!(!f.config.workspace.join("started").exists());
    assert!(f.runtime.start(&h).is_err());
}

#[test]
fn boundary_explicit_environment_and_private_metadata_mounts() {
    let mut f = Fixture::new();
    f.config
        .environment
        .insert("BOUNDARY_EXPLICIT".into(), "allowed".into());
    let h = f.prepare();
    let mut stream = f.runtime.connect_stdio(&h).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    f.runtime.start(&h).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    for (request, expected) in [
        ("env BOUNDARY_EXPLICIT".to_owned(), "allowed"),
        ("env CARGO_MANIFEST_DIR".to_owned(), "absent"),
        (
            format!(
                "exists {}",
                f._root.path().join("state/run-b/record.json").display()
            ),
            "false",
        ),
    ] {
        writeln!(stream, "{request}").unwrap();
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        assert_eq!(response.trim(), expected);
    }
    let mut exposed = f.config.clone();
    exposed.mounts.push(Mount {
        source: f._root.path().join("state"),
        destination: "/private".into(),
        writable: false,
    });
    assert!(f.runtime.prepare("another-run", &exposed).is_err());
}

#[test]
fn boundary_altered_handle_cannot_stop_a_live_sibling() {
    let mut a = Fixture::new();
    let mut b = Fixture::new();
    let ah = a.prepare();
    let bh = b.prepare();
    a.runtime.start(&ah).unwrap();
    b.runtime.start(&bh).unwrap();
    let mut forged = ah.clone();
    forged.init = bh.init.clone();
    assert!(a.runtime.stop(&forged, Duration::from_millis(20)).is_err());
    assert!(matches!(
        a.runtime.probe(&forged).unwrap(),
        BoundaryState::Unknown(_)
    ));
    assert_eq!(b.runtime.probe(&bh).unwrap(), BoundaryState::Running);
}

#[test]
fn boundary_unsupported_bwrap_is_an_explicit_error() {
    let f = Fixture::new();
    let runtime = Runtime::new(RuntimeConfig {
        state_root: f._root.path().join("unavailable"),
        helper: env!("CARGO_BIN_EXE_calm-worker-boundary").into(),
        bwrap: "/usr/bin/false".into(),
        timeout: Duration::from_secs(2),
    })
    .unwrap();
    assert!(matches!(
        runtime.preflight(NetworkPolicy::Provider),
        Err(calm_worker_runtime::Error::Unsupported(_))
    ));
    assert!(matches!(
        runtime.prepare("no-fallback", &f.config),
        Err(calm_worker_runtime::Error::Unsupported(_))
    ));
    assert!(!f.config.workspace.join("started").exists());
}
