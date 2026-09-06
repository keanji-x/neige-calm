use super::*;

#[tokio::test]
async fn dedicated_codex_thread_ack_save_failure_is_unknown_without_reissue() {
    let f = Fixture::new("normal");
    let endpoint = f.prepare("thread-ack-failed").await;
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    *journal.reject.lock().unwrap() = Some("thread-ack");
    assert!(matches!(
        session.create_thread(&journal).await,
        Err(Error::Unknown(_))
    ));
    assert_eq!(session.record().phase, RequestPhase::CreatingThread);
    *journal.reject.lock().unwrap() = None;
    session.reconcile(&journal).await.unwrap();
    assert_eq!(
        session.create_thread(&journal).await.unwrap(),
        "owned-thread"
    );
    assert_eq!(
        Fixture::calls(&endpoint)
            .iter()
            .filter(|c| c["method"] == "thread/start")
            .count(),
        1
    );
}

#[tokio::test]
async fn dedicated_codex_long_private_path_and_max_run_connect_to_same_endpoint() {
    let f = Fixture::with_config("normal", |config| {
        config.private_root = config
            .private_root
            .join("p".repeat(90))
            .join("q".repeat(90));
        config.connect_timeout = Duration::from_secs(1);
    });
    let endpoint = f.prepare(&"r".repeat(64)).await;
    assert!(endpoint.home.socket.as_os_str().len() > 108);
    let journal = Journal::default();
    let mut session = f
        .controller
        .connect(journal.prepared(endpoint.clone()), &journal)
        .await
        .unwrap();
    session.create_thread(&journal).await.unwrap();
    session
        .begin_turn("long", "prompt", &journal, &Allow)
        .await
        .unwrap();
    let record = session.record().clone();
    drop(session);
    let mut same = f.controller.connect(record, &journal).await.unwrap();
    same.reconcile(&journal).await.unwrap();
    assert_eq!(same.record().endpoint, endpoint);
}

#[tokio::test]
async fn dedicated_codex_binary_directory_alias_refused_before_credentials() {
    let f = Fixture::new("normal");
    let original = f.prepare("original-layout").await;
    let directory = f.root.path().join("provider");
    std::fs::create_dir(&directory).unwrap();
    let executable = directory.join("fake-provider");
    std::fs::hard_link(&f.config.codex_binary, &executable).unwrap();
    let alias = f.root.path().join("provider-alias");
    std::os::unix::fs::symlink(&directory, &alias).unwrap();
    for private in [directory.join("private"), alias.join("canonical-private")] {
        let mut config = f.config.clone();
        config.codex_binary = executable.clone();
        config.private_root = private.clone();
        let result = match Controller::new(config) {
            Ok(controller) => {
                controller
                    .prepare(original.request.clone(), &f.seed, &f.native)
                    .await
            }
            Err(error) => Err(error),
        };
        if let Ok(endpoint) = &result {
            f.endpoints.lock().unwrap().push(endpoint.clone());
        }
        assert!(
            matches!(result, Err(Error::Configuration(_))),
            "private alias accepted: {private:?}"
        );
        assert!(!private.join("original-layout/home/auth.json").exists());
        assert!(!private.join("original-layout/home/config.toml").exists());
    }
    // A component-adjacent sibling must remain a supported layout.
    let mut config = f.config.clone();
    config.codex_binary = executable;
    config.private_root = f.root.path().join("provider-private");
    let mut request = original.request.clone();
    request.identity.run_id = "disjoint".into();
    let endpoint = Controller::new(config)
        .unwrap()
        .prepare(request, &f.seed, &f.native)
        .await
        .unwrap();
    f.endpoints.lock().unwrap().push(endpoint);
}

#[test]
fn dedicated_codex_fixed_toolchain_alias_refused_before_directory_creation() {
    let f = Fixture::new("normal");
    let mut config = f.config.clone();
    config.private_root =
        std::path::Path::new("/usr/lib").join(format!("neige-denied-{}", uuid::Uuid::new_v4()));
    let path = config.private_root.clone();
    assert!(matches!(
        Controller::new(config),
        Err(Error::Configuration(_))
    ));
    assert!(!path.exists());
}
