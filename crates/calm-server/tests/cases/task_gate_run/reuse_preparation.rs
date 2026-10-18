//! The reuse refs subprocess must not hold the writer, and its run facts are revalidated.
//! These tests change PATH; nextest executes each test in a separate process.
use super::*;
use std::ffi::OsString;

struct GitBarrier {
    _directory: tempfile::TempDir,
    entered: PathBuf,
    release: PathBuf,
    original_path: OsString,
}
impl GitBarrier {
    fn install() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let entered = directory.path().join("entered");
        let release = directory.path().join("release");
        let real_git = std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        assert!(real_git.status.success());
        let real_git = String::from_utf8(real_git.stdout)
            .unwrap()
            .trim()
            .to_owned();
        write_executable(
            &directory.path().join("git"),
            &format!(
                "#!/bin/sh\nif [ \"$1\" = for-each-ref ]; then\n touch '{}'\n while [ ! -f '{}' ]; do sleep 0.02; done\nfi\nexec '{}' \"$@\"\n",
                entered.display(),
                release.display(),
                real_git,
            ),
        );
        let original_path = std::env::var_os("PATH").unwrap();
        let mut paths = vec![directory.path().to_owned()];
        paths.extend(std::env::split_paths(&original_path));
        // SAFETY: the nextest test process owns this environment, and the guard restores it.
        unsafe {
            std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        }
        Self {
            _directory: directory,
            entered,
            release,
            original_path,
        }
    }
    async fn wait_entered(&self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self.entered.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the real reuse path started its refs digest command");
    }
    fn release(&self) {
        std::fs::write(&self.release, "release").unwrap();
    }
}
impl Drop for GitBarrier {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release, "release");
        // SAFETY: same process-owned test environment as install().
        unsafe {
            std::env::set_var("PATH", &self.original_path);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reuse_refs_sampling_releases_writer_and_revalidates_run() {
    for change_run in [false, true] {
        let fx = world(LONG_IDLE).await;
        let a = delivered_after_a_passing_run(&fx, "refs-writer").await;
        let task = current(&fx.boot, &a.task.key).await;
        let barrier = GitBarrier::install();
        let scheduler = fx.scheduler();
        let drive = tokio::spawn(async move {
            scheduler.drive_gate_for_test(task).await.unwrap();
        });
        barrier.wait_entered().await;
        let pool = fx.db();
        let task_id = a.task.id.clone();
        let mut writer = tokio::spawn(async move {
            let mut tx = begin_immediate_tx(&pool).await?;
            // An unrelated update proves the write slot is available while refs is blocked.
            sqlx::query(
                "UPDATE tracks SET title=title WHERE id=(SELECT track_id FROM tasks WHERE id=?1)",
            )
            .bind(&task_id)
            .execute(&mut **tx)
            .await?;
            if change_run {
                sqlx::query(include_str!("../../fixtures/gate_run_verdict_change.sql"))
                    .bind(TASK_GATE_RUN_KIND)
                    .bind(format!("{task_id}#r1"))
                    .execute(&mut **tx)
                    .await?;
            }
            tx.commit().await?;
            Ok::<_, calm_server::error::CalmError>(())
        });
        let committed = tokio::time::timeout(Duration::from_secs(1), &mut writer).await;
        let unbumped = current(&fx.boot, &a.task.key).await.gate_attempt;
        let result_events = fx.events_for("task.gate_result", &a.task.id).await.len();
        barrier.release();
        tokio::time::timeout(Duration::from_secs(30), drive)
            .await
            .unwrap()
            .unwrap();
        if committed.is_err() {
            writer.await.unwrap().unwrap();
        }
        assert!(
            matches!(committed, Ok(Ok(Ok(())))),
            "writer waited for refs subprocess: {committed:?}"
        );
        assert_eq!(unbumped, 0, "external preparation cannot bump the attempt");
        assert_eq!(
            result_events, 0,
            "external preparation cannot publish a result"
        );
        let result = verdict(&fx, &a.task.key).await;
        if change_run {
            assert_gate_ran(&fx, &a, &result, 1);
        } else {
            assert_eq!(
                reused_run(&result),
                Some(format!("{}#r1", a.task.id).as_str())
            );
            assert!(!gate_spawned(&fx, &a.task.id, 1));
        }
    }
}
