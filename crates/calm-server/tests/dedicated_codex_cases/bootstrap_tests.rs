use super::*;

#[tokio::test]
async fn dedicated_codex_mounts_only_known_executables_and_selects_inner_bwrap() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("bootstrap-mounts").await;
    let executable_mounts: Vec<_> = endpoint
        .launch_request()
        .mounts
        .iter()
        .filter(|mount| mount.destination.starts_with("/provider-bin"))
        .collect();
    assert!(
        executable_mounts
            .iter()
            .all(|mount| mount.source.is_file() && !mount.writable),
        "never expose a whole host binary directory"
    );
    assert!(
        executable_mounts
            .iter()
            .any(|mount| mount.source == f.config.sandbox_bwrap
                && mount.destination == std::path::Path::new("/provider-bin/bwrap"))
    );
    assert!(executable_mounts.iter().any(|mount| {
        mount.source == f.config.code_mode_host_binary
            && mount.destination == std::path::Path::new("/provider-bin/codex-code-mode-host")
            && !mount.writable
    }));
    let policy = std::fs::read_to_string(endpoint.home.home.join("config.toml"))
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    assert_eq!(
        policy["shell_environment_policy"]["set"]["PATH"].as_str(),
        Some("/provider-bin:/usr/bin:/bin")
    );
}

#[tokio::test]
async fn dedicated_codex_unsupported_inner_bwrap_refused_before_credentials() {
    let f = Fixture::new("normal");
    let original = f.prepare("supported-helper").await;
    std::fs::write(
        &f.config.sandbox_bwrap,
        "#!/bin/sh\nprintf '%s\\n' '--perms --ro-bind --unshare-user --unshare-net'\n",
    )
    .unwrap();
    let controller = Controller::new(f.config.clone()).unwrap();
    let mut request = original.request.clone();
    request.identity.run_id = "unsupported-helper".into();
    let result = controller.prepare(request, &f.seed, &f.native).await;
    if let Ok(endpoint) = &result {
        f.endpoints.lock().unwrap().push(endpoint.clone());
    }
    assert!(matches!(result, Err(Error::Unsupported(_))));
    assert!(
        !f.config
            .private_root
            .join("unsupported-helper/home/auth.json")
            .exists()
    );
}

#[tokio::test]
async fn dedicated_codex_missing_protected_directory_requires_caller_preparation() {
    let f = Fixture::new("normal");
    let original = f.prepare("prepared-directory").await;
    let workspace = f.root.path().join("unprepared");
    std::fs::create_dir(&workspace).unwrap();
    let mut request = original.request.clone();
    request.identity.run_id = "missing-directory".into();
    request.workspace = workspace.clone();
    let result = f.controller.prepare(request, &f.seed, &f.native).await;
    if let Ok(endpoint) = &result {
        f.endpoints.lock().unwrap().push(endpoint.clone());
    }
    assert!(matches!(
        result,
        Err(Error::WorkspacePrecondition(
            WorkspaceRequirement::ProtectedConfigDirectory
        ))
    ));
    assert!(
        !workspace.join(".codex").exists(),
        "controller cannot mutate already-frozen workspace"
    );
    assert!(
        !f.config
            .private_root
            .join("missing-directory/home/auth.json")
            .exists()
    );
}

#[tokio::test]
async fn dedicated_codex_workspace_cannot_supply_trusted_sandbox_executable() {
    let f = Fixture::new("normal");
    let original = f.prepare("tool-outside").await;
    let helper = original.request.workspace.join("writable-bwrap");
    std::fs::copy(&f.config.sandbox_bwrap, &helper).unwrap();
    for (name, companion) in [("writable-helper", false), ("writable-companion", true)] {
        let mut config = f.config.clone();
        if companion {
            config.code_mode_host_binary = helper.clone();
        } else {
            config.sandbox_bwrap = helper.clone();
        }
        let controller = Controller::new(config).unwrap();
        let mut request = original.request.clone();
        request.identity.run_id = name.into();
        let result = controller.prepare(request, &f.seed, &f.native).await;
        if let Ok(endpoint) = &result {
            f.endpoints.lock().unwrap().push(endpoint.clone());
        }
        assert!(matches!(result, Err(Error::Configuration(_))));
        assert!(
            !f.config
                .private_root
                .join(name)
                .join("home/auth.json")
                .exists()
        );
    }
}

#[tokio::test]
async fn dedicated_codex_protected_directory_rejects_file_and_symlink_without_changes() {
    let f = Fixture::new("normal");
    let original = f.prepare("real-directory").await;
    for (name, symlink) in [("config-file", false), ("config-link", true)] {
        let workspace = f.root.path().join(name);
        std::fs::create_dir(&workspace).unwrap();
        let path = workspace.join(".codex");
        if symlink {
            std::os::unix::fs::symlink(&original.request.workspace, &path).unwrap();
        } else {
            std::fs::write(&path, b"retain file").unwrap();
        }
        let mut request = original.request.clone();
        request.identity.run_id = name.into();
        request.workspace = workspace;
        let result = f.controller.prepare(request, &f.seed, &f.native).await;
        if let Ok(endpoint) = &result {
            f.endpoints.lock().unwrap().push(endpoint.clone());
        }
        assert!(matches!(
            result,
            Err(Error::WorkspacePrecondition(
                WorkspaceRequirement::ProtectedConfigDirectory
            ))
        ));
        if symlink {
            assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
        } else {
            assert_eq!(std::fs::read(path).unwrap(), b"retain file");
        }
    }
}

#[tokio::test]
async fn dedicated_codex_old_bootstrap_endpoint_can_stop_but_never_activate() {
    let f = Fixture::new("normal");
    let mut endpoint = f.prepare("old-bootstrap").await;
    endpoint.version = 1;
    let journal = Journal::default();
    let mut record = journal.prepared(endpoint.clone());
    assert!(matches!(
        f.controller.connect(record.clone(), &journal).await,
        Err(Error::Unsupported(_))
    ));
    assert_eq!(
        f.controller.probe(&endpoint).await.unwrap(),
        BoundaryState::Prepared
    );
    assert!(Fixture::calls(&endpoint).is_empty());
    assert!(matches!(
        f.controller
            .stop(&mut record, &journal, Duration::from_secs(3))
            .await
            .unwrap(),
        BoundaryState::Quiesced(_)
    ));
}

#[tokio::test]
async fn dedicated_codex_fixture_command_cannot_run_after_thread_creation() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("fixture-command").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    session.create_thread(&journal).await.unwrap();
    let calls = Fixture::calls(&endpoint).len();
    assert!(matches!(
        session
            .command_for_fixture(vec!["/bin/true".into()], BTreeMap::new())
            .await,
        Err(Error::Conflict(_))
    ));
    assert_eq!(Fixture::calls(&endpoint).len(), calls);
}
