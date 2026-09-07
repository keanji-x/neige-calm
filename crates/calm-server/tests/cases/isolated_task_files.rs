//! Exact accepted artifact bytes from actual provider-created files and runtime stop.
use super::*;
use axum::http::{HeaderMap, header};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::os::unix::fs::{PermissionsExt, symlink};

async fn completed_files(references: &[&str]) -> (Fixture, Task, PathBuf, Value) {
    let fx = fixture("files").await;
    start_task(&fx).await;
    let (task, workspace) = launch(&fx).await;
    std::fs::write(
        workspace.join("reported-files.json"),
        serde_json::to_vec(references).unwrap(),
    )
    .unwrap();
    let output = finish(&fx, &task, &workspace, true).await;
    (fx, task, workspace, output)
}
fn artifact_route(fx: &Fixture, task: &Task, index: usize) -> String {
    route(fx, &format!("attempts/{}/artifacts/{index}", task.id))
}
async fn get(
    fx: &Fixture,
    path: &str,
    actor: &str,
    authenticated: bool,
) -> (StatusCode, HeaderMap, Value) {
    let app = calm_server::routes::protected_router()
        .with_state(fx.state.clone())
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ));
    let app = if authenticated {
        app.layer(Extension(crate::task_projection_acceptance::principal()))
    } else {
        app
    };
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .header("X-Calm-Actor", actor)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn file(fx: &Fixture, task: &Task, index: usize, expected: StatusCode) -> Value {
    let (status, headers, value) = get(fx, &artifact_route(fx, task, index), "user", true).await;
    assert_eq!(status, expected, "{value}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    if !status.is_success() {
        assert!(
            !value.to_string().contains(fx.root.path().to_str().unwrap()),
            "private path leak"
        );
        assert!(!value.to_string().contains("FAKE"));
    }
    value
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completed_file_exact_bytes_scope_and_card_deletion() {
    let refs = [
        "result.txt",
        "nested/报告 🧊.txt",
        "/workspace/blob.bin",
        "empty.txt",
        "limit.bin",
        "large.bin",
    ];
    let (mut fx, task, _, output) = completed_files(&refs).await;
    for (index, name, expected) in [
        (0, "result.txt", b"42\n".as_slice()),
        (1, "报告 🧊.txt", "<script>hello</script> 雪\n".as_bytes()),
        (2, "blob.bin", [0, 255, 128, 1].as_slice()),
        (3, "empty.txt", b"".as_slice()),
    ] {
        let value = file(&fx, &task, index, StatusCode::OK).await;
        assert_eq!(
            value,
            json!({"attemptId":task.id,"index":index,"name":name,"size":expected.len(),"contentBase64":STANDARD.encode(expected)})
        );
        assert_eq!(
            STANDARD
                .decode(value["contentBase64"].as_str().unwrap())
                .unwrap(),
            expected
        );
    }
    let large = file(&fx, &task, 4, StatusCode::OK).await;
    assert_eq!(large["size"], 8 * 1024 * 1024);
    assert_eq!(
        STANDARD
            .decode(large["contentBase64"].as_str().unwrap())
            .unwrap()
            .len(),
        8 * 1024 * 1024
    );
    file(&fx, &task, 5, StatusCode::PAYLOAD_TOO_LARGE).await;
    file(&fx, &task, 6, StatusCode::NOT_FOUND).await;
    let path = artifact_route(&fx, &task, 0);
    assert_eq!(
        get(&fx, &path, "user", false).await.0,
        StatusCode::UNAUTHORIZED
    );
    for actor in ["ai:codex", "ai:claude", "ai:planner"] {
        assert_eq!(get(&fx, &path, actor, true).await.0, StatusCode::FORBIDDEN);
    }
    for path in [
        path.replace("tasks/retry/", "tasks/wrong/"),
        path.replace(&task.id, "wrong-attempt"),
        path.replace(fx.boot.track_id.as_str(), "wrong-track"),
    ] {
        assert_eq!(get(&fx, &path, "user", true).await.0, StatusCode::NOT_FOUND);
    }
    let card = output["data"]["isolated_execution"]["request"]["identity"]["card_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        rest(&fx, "DELETE", &format!("/api/cards/{card}"), Value::Null)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        file(&fx, &task, 0, StatusCode::OK).await["contentBase64"],
        "NDIK"
    );
    // Reassemble the read service with no execution backend configured.
    fx.state = crate::task_projection_acceptance::route_state(&fx.boot).await;
    assert_eq!(
        rest(
            &fx,
            "POST",
            &format!("/api/tracks/{}/isolated-tasks", fx.boot.track_id),
            json!({"key":"disabled","goal":"not launched","ifDocRev":1})
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        file(&fx, &task, 0, StatusCode::OK).await["contentBase64"],
        "NDIK"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_reference_and_filesystem_escape_refused() {
    let refs = [
        "../result.txt",
        "./result.txt",
        "nested//x",
        "/etc/passwd",
        "https://example.test/a",
        "file:///etc/passwd",
        "",
        "/workspace/",
        ".codex/config.toml",
        "nested/.codex/x",
        "bad\\name",
        "bad\nname",
        "link",
        "parent/value",
        "hard",
        "fifo",
        "nested",
        "missing",
        "/proc/self/fd/0",
    ];
    let (fx, task, workspace, _) = completed_files(&refs).await;
    for index in 0..12 {
        file(&fx, &task, index, StatusCode::BAD_REQUEST).await;
    }
    symlink("empty.txt", workspace.join("link")).unwrap();
    symlink(fx.root.path(), workspace.join("parent")).unwrap();
    std::fs::hard_link(workspace.join("result.txt"), workspace.join("hard")).unwrap();
    nix::unistd::mkfifo(
        &workspace.join("fifo"),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    for index in 12..18 {
        tokio::time::timeout(
            Duration::from_secs(2),
            file(&fx, &task, index, StatusCode::NOT_FOUND),
        )
        .await
        .unwrap();
    }
    file(&fx, &task, 18, StatusCode::BAD_REQUEST).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_stop_and_owner_identity_required() {
    let (fx, task, workspace, original) = completed_files(&["result.txt"]).await;
    fx.state.dispatcher.semaphore().close();
    for (path, value) in [
        ("/data/isolated_execution/admission", json!("open")),
        (
            "/data/isolated_execution/provider/record/stop",
            json!("Requested"),
        ),
        (
            "/data/isolated_execution/provider/record/stop/Quiesced/handle/init/namespace_inode",
            json!(0),
        ),
        (
            "/data/isolated_execution/request/workspace",
            json!("/foreign/root"),
        ),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(path).unwrap() = value;
        set_output(&fx, &task, &changed).await;
        file(&fx, &task, 0, StatusCode::CONFLICT).await;
    }
    set_output(&fx, &task, &original).await;
    let (op_id, _, _) = operation(&fx, &task).await;
    let marker = workspace.parent().unwrap().join(format!("{op_id}.owner"));
    let original_marker = std::fs::read(&marker).unwrap();
    std::fs::write(&marker, b"0:0").unwrap();
    file(&fx, &task, 0, StatusCode::CONFLICT).await;
    std::fs::write(&marker, &original_marker).unwrap();
    std::fs::rename(&workspace, workspace.with_extension("retained")).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("result.txt"), b"substituted").unwrap();
    file(&fx, &task, 0, StatusCode::CONFLICT).await;
    std::fs::remove_dir_all(&workspace).unwrap();
    symlink(workspace.with_extension("retained"), &workspace).unwrap();
    file(&fx, &task, 0, StatusCode::CONFLICT).await;
    std::fs::remove_file(&workspace).unwrap();
    std::fs::rename(workspace.with_extension("retained"), &workspace).unwrap();
    std::fs::remove_file(&marker).unwrap();
    nix::unistd::mkfifo(
        &marker,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        file(&fx, &task, 0, StatusCode::CONFLICT),
    )
    .await
    .unwrap();
    std::fs::remove_file(&marker).unwrap();
    std::fs::write(&marker, &original_marker).unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    let saved_marker = marker.with_extension("saved");
    std::fs::rename(&marker, &saved_marker).unwrap();
    symlink(&saved_marker, &marker).unwrap();
    file(&fx, &task, 0, StatusCode::CONFLICT).await;
    std::fs::remove_file(&marker).unwrap();
    std::fs::rename(&saved_marker, &marker).unwrap();
    let parent = workspace.parent().unwrap();
    let saved_parent = parent.with_extension("saved");
    std::fs::rename(parent, &saved_parent).unwrap();
    symlink(&saved_parent, parent).unwrap();
    file(&fx, &task, 0, StatusCode::CONFLICT).await;
    std::fs::remove_file(parent).unwrap();
    std::fs::rename(&saved_parent, parent).unwrap();
    std::fs::rename(&workspace, workspace.with_extension("saved")).unwrap();
    file(&fx, &task, 0, StatusCode::CONFLICT).await;
    assert!(
        !workspace.exists(),
        "file access must never recreate a missing workspace"
    );
    std::fs::rename(workspace.with_extension("saved"), &workspace).unwrap();
    assert_eq!(
        file(&fx, &task, 0, StatusCode::OK).await["contentBase64"],
        "NDIK"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_requires_accepted_completed_event_and_sole_succeeded_operation() {
    use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
    let (fx, task, _, _) = completed_files(&["result.txt"]).await;
    fx.state.dispatcher.semaphore().close();
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    for phase in ["failed", "spawn_started", "compensating", "stuck"] {
        sqlx::query("UPDATE operations SET phase=?1 WHERE idempotency_key=?2")
            .bind(phase)
            .bind(&task.id)
            .execute(&pool)
            .await
            .unwrap();
        file(&fx, &task, 0, StatusCode::CONFLICT).await;
    }
    sqlx::query("UPDATE operations SET phase='succeeded' WHERE idempotency_key=?1")
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET status='running' WHERE id=?1")
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    file(&fx, &task, 0, StatusCode::CONFLICT).await;
    sqlx::query("UPDATE tasks SET status='done' WHERE id=?1")
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    for column in ["spawn_artifacts_json", "compensation_state"] {
        sqlx::query(&format!(
            "UPDATE operations SET {column}='{{}}' WHERE idempotency_key=?1"
        ))
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
        file(&fx, &task, 0, StatusCode::CONFLICT).await;
        sqlx::query(&format!(
            "UPDATE operations SET {column}=NULL WHERE idempotency_key=?1"
        ))
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    }
    let (event_id, actor): (i64,String) = sqlx::query_as("SELECT id,actor FROM events WHERE kind='task.completed' AND json_extract(payload,'$.idempotency_key')=?1")
        .bind(&task.id).fetch_one(&pool).await.unwrap();
    sqlx::query("UPDATE events SET actor=?1 WHERE id=?2")
        .bind(serde_json::to_string(&calm_server::ids::ActorId::KernelDispatcher).unwrap())
        .bind(event_id)
        .execute(&pool)
        .await
        .unwrap();
    file(&fx, &task, 0, StatusCode::NOT_FOUND).await;
    sqlx::query("UPDATE events SET actor=?1 WHERE id=?2")
        .bind(actor)
        .bind(event_id)
        .execute(&pool)
        .await
        .unwrap();
    let ops = SqlxOperationRepo::new(pool.clone());
    for (kind, key) in [
        ("foreign-worker", Some(task.id.clone())),
        ("task-verify", None),
    ] {
        let payload = json!({"track_id":task.track_id,"task_id":task.id});
        let id = ops
            .insert_operation(
                kind,
                OperationKey {
                    operation_key: format!("file-conflict-{kind}"),
                    idempotency_key: key,
                    payload_hash: calm_server::routes::terminal_cards::stable_payload_hash(
                        &payload,
                    )
                    .unwrap(),
                },
                payload,
            )
            .await
            .unwrap();
        file(&fx, &task, 0, StatusCode::CONFLICT).await;
        sqlx::query("UPDATE operations SET idempotency_key=NULL,payload_json='{}' WHERE id=?1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(
        file(&fx, &task, 0, StatusCode::OK).await["contentBase64"],
        "NDIK"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_retry_history_keeps_exact_attempt_bytes() {
    let fx = fixture("files").await;
    start_task(&fx).await;
    let (first, workspace) = launch(&fx).await;
    std::fs::write(workspace.join("reported-files.json"), "[\"result.txt\"]").unwrap();
    finish(&fx, &first, &workspace, false).await;
    file(&fx, &first, 0, StatusCode::NOT_FOUND).await;
    let (status, receipt) = rest(&fx, "POST", &route(&fx, "recover"), recovery(&first)).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    let (second, second_workspace) = launch(&fx).await;
    std::fs::write(
        second_workspace.join("reported-files.json"),
        "[\"result.txt\"]",
    )
    .unwrap();
    finish(&fx, &second, &second_workspace, true).await;
    assert_ne!(workspace, second_workspace);
    std::fs::write(workspace.join("result.txt"), b"old failure bytes").unwrap();
    assert_eq!(
        file(&fx, &second, 0, StatusCode::OK).await["contentBase64"],
        "NDIK"
    );
    file(&fx, &first, 0, StatusCode::NOT_FOUND).await;
    let history = rest(&fx, "GET", &route(&fx, "attempts"), Value::Null)
        .await
        .1;
    assert_eq!(history["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(history["attempts"][0]["status"], "failed");
    assert_eq!(history["attempts"][1]["status"], "done");
}
