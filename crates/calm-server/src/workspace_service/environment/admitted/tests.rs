use super::*;

fn authorized_source(script: &str) -> tempfile::TempDir {
    std::fs::create_dir_all("_local/tmp").unwrap();
    let root = tempfile::tempdir_in("_local/tmp").unwrap();
    let source = root.path().canonicalize().unwrap();
    std::fs::write(source.join(".envrc"), script).unwrap();
    assert!(
        std::process::Command::new("direnv")
            .arg("allow")
            .arg(&source)
            .env_clear()
            .envs(directory_base_environment())
            .output()
            .unwrap()
            .status
            .success()
    );
    root
}

#[test]
fn cancelled_local_environment_admission_closes_release_before_project_evaluation() {
    let root = authorized_source("printf evaluated >> \"$PWD/environment-evaluated\"\n");
    let source = root.path().canonicalize().unwrap();
    // One blocking worker makes the queued completion barrier prove that the
    // cancelled evaluator exited, rather than merely not having run yet.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            let (checking, checked) = tokio::sync::oneshot::channel();
            let selected = source.clone();
            let evaluation = tokio::spawn(async move {
                resolve_for_directory_admitted(&selected, || async move {
                    checking.send(()).unwrap();
                    std::future::pending::<Result<()>>().await
                })
                .await
            });
            checked.await.unwrap();
            evaluation.abort();
            assert!(matches!(evaluation.await, Err(error) if error.is_cancelled()));
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert!(!source.join("environment-evaluated").exists());

            let refused =
                resolve_for_directory_admitted(&source, || async { Err(CalmError::Unauthorized) })
                    .await;
            assert!(matches!(refused, Err(CalmError::Unauthorized)));
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert!(!source.join("environment-evaluated").exists());

            resolve_for_directory_admitted(&source, || async { Ok(()) })
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(source.join("environment-evaluated")).unwrap(),
                "evaluated"
            );
        });
}

#[tokio::test]
async fn admitted_environment_preserves_owner_error_and_project_error_stages() {
    let root = authorized_source("printf evaluated >> \"$PWD/environment-evaluated\"\nexit 29\n");
    let source = root.path().canonicalize().unwrap();
    let refused = resolve_for_directory_admitted(&source, || async {
        Err(CalmError::Db(sqlx::Error::PoolTimedOut))
    })
    .await;
    assert!(matches!(
        refused,
        Err(CalmError::Db(sqlx::Error::PoolTimedOut))
    ));
    assert!(!source.join("environment-evaluated").exists());

    let evaluated = resolve_for_directory_admitted(&source, || async { Ok(()) })
        .await
        .unwrap();
    assert!(matches!(
        evaluated,
        Err(workspace::environment::EnvironmentError::Failed(_))
    ));
    assert_eq!(
        std::fs::read_to_string(source.join("environment-evaluated")).unwrap(),
        "evaluated"
    );
}

#[tokio::test]
async fn admitted_environment_filters_kernel_names_before_returning_success() {
    let root = authorized_source(
        "export NEIGE_FORGED_WORKSPACE_VALUE=forged\nexport PROJECT_EVALUATION=admitted\n",
    );
    let source = root.path().canonicalize().unwrap();
    let environment = resolve_for_directory_admitted(&source, || async { Ok(()) })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        environment.get(std::ffi::OsStr::new("NEIGE_FORGED_WORKSPACE_VALUE")),
        None
    );
    assert_eq!(
        environment.get(std::ffi::OsStr::new("PROJECT_EVALUATION")),
        Some(std::ffi::OsStr::new("admitted"))
    );
}
