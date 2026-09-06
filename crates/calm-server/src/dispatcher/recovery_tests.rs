//! A historical gate observation must resolve the evidence it names.
use super::*;
use calm_truth::db::{RepoRead, RepoSyncDomainRaw};

#[tokio::test]
async fn task_recovery_gate_observation_reads_exact_execution_and_gate_bytes() {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let logs = tempfile::tempdir().unwrap();
    let area = repo
        .area_create(crate::model::NewArea {
            name: "history".into(),
            color: "#000000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(crate::model::NewTrack {
            template_input: None,
            area_id: area.id,
            title: "history".into(),
            sort: None,
            cwd: logs.path().display().to_string(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let previous_id = format!("{}:b", track.id);
    let refs = vec![calm_types::event::TaskContextRef {
        track_id: track.id.clone(),
        block_id: "b_1000".into(),
        rev: 1,
        hash: "0".repeat(64),
        is_root: true,
    }];
    let gate = serde_json::json!({"steps":[{"name":"check","cmd":"true"}]}).to_string();
    let mut tx = crate::db::sqlite::begin_immediate_tx(repo.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,gate_json,gate_attempt,claim_context_json,created_at_ms,updated_at_ms,finished_at_ms) VALUES(?1,?2,'b','terminal','true','{}','failed',?3,2,?4,1,1,1)")
        .bind(&previous_id).bind(track.id.as_str()).bind(&gate).bind(serde_json::to_string(&refs).unwrap()).execute(&mut *tx).await.unwrap();
    // A valid historical allocation pair is the input to evidence reading;
    // this does not invoke or advertise S1 post-execution recovery admission.
    let receipt = crate::db::sqlite::task_recovery_allocate_tx(
        &mut tx,
        track.id.as_str(),
        "b",
        &calm_types::task_recovery::TaskRecoveryRequest {
            expected_attempt_id: previous_id.clone(),
            idempotency_key: "history-fixture".into(),
            reason: "historical allocation".into(),
        },
        "history-fixture",
        &calm_types::task_recovery::TaskRecoveryConstraint::V1 {
            refs,
            spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
            declared_by: calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR.into(),
        },
        &ActorId::User,
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,gate_json,gate_attempt,created_at_ms,updated_at_ms) VALUES(?1,?2,'b','terminal','true','{}','verifying',?3,1,2,2)")
        .bind(&receipt.attempt_id).bind(track.id.as_str()).bind(&gate).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    for (file, contents) in [
        (format!("{previous_id}-g1.log"), "old gate one"),
        (format!("{previous_id}-g2.log"), "old gate two"),
        (format!("{}-g1.log", receipt.attempt_id), "current gate"),
    ] {
        std::fs::write(logs.path().join(file), contents).unwrap();
    }
    let write = crate::state::WriteContext::new(
        crate::card_role_cache::CardRoleCache::new(),
        crate::track_area_cache::TrackAreaCache::new(),
    );
    let filesystem = calm_truth::track_fs_view::TrackFsView::new(&repo, &write)
        .with_gate_log_access(CardRole::Planner, logs.path().into());
    assert_eq!(
        filesystem
            .cat(&track, "plan/b/gate.log")
            .await
            .unwrap()
            .content,
        "current gate"
    );
    for (attempt, expected_contents) in [(1, "old gate one"), (2, "old gate two")] {
        let event = Event::TaskGateResult {
            task_id: previous_id.clone(),
            idempotency_key: previous_id.clone(),
            passed: false,
            failing_step: Some("check".into()),
            exit_code: Some(1),
            log_tail: expected_contents.into(),
            log_path: logs
                .path()
                .join(format!("{previous_id}-g{attempt}.log"))
                .display()
                .to_string(),
            attempt,
            agent_message: None,
        };
        let persisted: Event =
            serde_json::from_value(serde_json::to_value(&event).unwrap()).unwrap();
        for input in [&event, &persisted] {
            // Live notification and boot replay share this production resolver.
            let observation = resolve_harness_observation(&repo, &track.id, input)
                .await
                .unwrap()
                .unwrap();
            let text = observation.to_turn_text();
            let named_path = text
                .split_once("Read the full log at ")
                .unwrap()
                .1
                .split(';')
                .next()
                .unwrap();
            assert_eq!(
                named_path,
                calm_truth::track_fs_view::task_gate_log_path(&previous_id, attempt).unwrap()
            );
            assert_eq!(
                filesystem.cat(&track, named_path).await.unwrap().content,
                expected_contents
            );
        }
    }
    assert_eq!(
        RepoRead::task_current_get(&repo, track.id.as_str(), "b")
            .await
            .unwrap()
            .unwrap()
            .id,
        receipt.attempt_id
    );
}
