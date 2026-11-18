//! A queued production resolver must not run project code after its owner disappears.
use super::*;

#[test]
fn queued_directory_evaluation_cancelled_before_ready_never_runs_rc() {
    std::fs::create_dir_all("_local/tmp").unwrap();
    let directory = tempfile::tempdir_in("_local/tmp").unwrap();
    let source = directory.path().canonicalize().unwrap();
    std::fs::write(
        source.join(".envrc"),
        "printf evaluated >> \"$PWD/evaluation-marker\"\n",
    )
    .unwrap();
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
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            let (entered, occupying) = tokio::sync::oneshot::channel();
            let (release, blocked) = tokio::sync::oneshot::channel();
            let worker = tokio::task::spawn_blocking(move || {
                let _ = entered.send(());
                // Sender destruction also releases the worker on a failed assertion.
                let _ = blocked.blocking_recv();
            });
            occupying.await.unwrap();

            // This production function submits its blocking job before its first
            // await. With the only worker held, Pending proves the job is queued.
            let mut evaluation =
                Box::pin(resolve_for_directory_admitted(&source, || async { Ok(()) }));
            let queued = futures_util::poll!(evaluation.as_mut()).is_pending();
            drop(evaluation); // Drop the owned future, not a borrowed Pin reference.
            release.send(()).unwrap();
            worker.await.unwrap();
            // The single worker drains the earlier evaluator before this barrier.
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert!(
                queued,
                "the real resolver must wait for its execution resource"
            );
            assert!(
                !source.join("evaluation-marker").exists(),
                "cancelled queued resolver executed project code"
            );

            resolve_for_directory_admitted(&source, || async { Ok(()) })
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(source.join("evaluation-marker")).unwrap(),
                "evaluated",
                "a fresh live owner still resolves the authorized project"
            );
        });
}
