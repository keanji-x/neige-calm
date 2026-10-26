use super::assistant_canonical::{carrier, complete_assistant_turn, track};
use super::*;
use calm_truth::db::RepoRead;
#[tokio::test]
async fn u11_canonical_legacy_child_journal_allows_original_assistant_once() {
    journal_flow(false).await;
}
#[tokio::test]
async fn u11_canonical_modern_child_journal_damage_refuses_native_activation() {
    journal_flow(true).await;
}
async fn journal_flow(damaged: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let parent = track(&app, &cookie).await;
    let parent_row = fixture.repo.track_get(&parent).await.unwrap().unwrap();
    let borrowed = parent_row.workspace.path;
    sqlx::query("UPDATE tracks SET workspace_kind='attached' WHERE id=?1")
        .bind(&parent)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let mut children = Vec::new();
    for damage in if damaged { 0..13 } else { 0..1 } {
        let created_track = post(
            &app,
            &cookie,
            "/api/tracks",
            &format!("child-{damage}"),
            json!({"area_id":parent_row.area_id,"title":"released Child","planner_provider":"codex",
                "theme":{"fg":[255,255,255],"bg":[0,0,0]}}),
        )
        .await;
        children.push(created_track["id"].as_str().unwrap().to_owned());
    }
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    for (damage, child) in children.into_iter().enumerate() {
        sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
            .bind(&parent)
            .bind(&child)
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        if !damaged {
            sqlx::query("UPDATE tracks SET workspace_kind='attached',workspace_path=?2,workspace_worktree_path=NULL WHERE id=?1")
            .bind(&child).bind(&borrowed).execute(fixture.repo.pool()).await.unwrap();
        }
        let cards: Vec<(String, String)> =
            sqlx::query_as("SELECT id,role FROM cards WHERE track_id=?1")
                .bind(&child)
                .fetch_all(fixture.repo.pool())
                .await
                .unwrap();
        let id = |role| cards.iter().find(|(_, r)| r == role).unwrap().0.clone();
        sqlx::query("UPDATE tracks SET workspace_frozen_at=1 WHERE id=?1")
            .bind(&child)
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        let frozen = fixture.repo.track_get(&child).await.unwrap().unwrap();
        let legacy = json!({"child_track_id":child,"planner_card_id":id("planner"),
            "report_card_id":id("reportcard"),"seed":"released seed","cwd":frozen.workspace.path});
        let mut output =
            crate::operation::TxOutput::new("track", Some(child.clone()), legacy.clone());
        output.data = legacy;
        let mut phase = "succeeded";
        if damaged {
            output.data["workspace_track"] = json!(frozen);
            output.data["parent_head"] = Value::Null;
            output.data["canonical_worker"] = Value::Null;
            output.result = output.data.clone();
            match damage {
                0 => {
                    output
                        .data
                        .as_object_mut()
                        .unwrap()
                        .remove("workspace_track");
                }
                1 => output.data["workspace_track"] = Value::Null,
                2 => {
                    output.data.as_object_mut().unwrap().remove("parent_head");
                }
                3 => output.data["parent_head"] = json!(17),
                4 => {
                    output
                        .data
                        .as_object_mut()
                        .unwrap()
                        .remove("canonical_worker");
                }
                5 => output.data["canonical_worker"] = json!(17),
                6 => {
                    output.data["workspace_track"]["workspace"]["path"] = json!("/drift");
                    output.data["cwd"] = json!("/drift");
                }
                7 => phase = "awaiting_retry",
                8 => output.data["workspace_error"] = json!("not prepared"),
                9 => output.data["workspace_track"]["workspace"]["kind"] = json!("attached"),
                10 => output.data["workspace_track"]["workspace"]["frozen_at"] = Value::Null,
                11 => output.data["workspace_track"]["workspace"]["frozen_at"] = json!(2),
                12 => {} // Positive control: the unchanged modern contract activates too.
                _ => unreachable!(),
            }
        }
        // Keep result/data consistent except the missing U11 worker marker.
        if damaged && damage != 4 {
            output.result = output.data.clone();
        }
        sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,
            target_type,target_id,target_json,payload_json,tx_output_json,phase,phase_detail_json,created_at_ms,updated_at_ms)
            VALUES(?1,?1,'child-track',?1,'released','track',?2,'{}','{}',?3,?4,
            '{\"kind\":\"mint_and_await\"}',1,1)")
            .bind(format!("old-child-{child}")).bind(&child)
            .bind(serde_json::to_string(&output).unwrap()).bind(phase)
            .execute(fixture.repo.pool()).await.unwrap();
        let uri = format!("/api/tracks/{child}/conversations");
        let body = json!({"provider":"codex","text":FIRST_BODY});
        let created = post(&app, &cookie, &uri, "journal-assistant", body.clone()).await;
        let card = created["id"].as_str().unwrap();
        let original = carrier(&fixture, card).await.0;
        if damaged && damage != 12 {
            let native = tokio::time::timeout(Duration::from_millis(350), async {
                loop {
                    let call = request(&mut peer).await;
                    let result = match call["method"].as_str().unwrap() {
                        "config/read" => json!({"config":{}}),
                        "model/list" => json!({"data":[]}),
                        _ => break call,
                    };
                    send(&mut peer, json!({"id":call["id"],"result":result})).await;
                }
            })
            .await;
            assert!(
                native.is_err(),
                "proof {damage} activated: {:?}",
                native.as_ref().ok().map(|call| &call["method"])
            );
            let (_, snapshot) = carrier(&fixture, card).await;
            assert_eq!(snapshot.canonical_inputs.pending.len(), 1);
            assert!(snapshot.canonical_inputs.attempts.is_empty());
        } else {
            complete_assistant_turn(&fixture, &state, &mut peer, card).await;
            let (_, snapshot) = carrier(&fixture, card).await;
            assert!(snapshot.canonical_inputs.pending.is_empty());
            assert_eq!(snapshot.canonical_inputs.attempts.len(), 1);
            assert_eq!(fixture.counts(card).await, (1, 1, 1, 1, 0));
        }
        assert_eq!(carrier(&fixture, card).await.0, original);
        assert_eq!(
            post(&app, &cookie, &uri, "journal-assistant", body).await["initialInput"],
            created["initialInput"]
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(150), request(&mut peer))
                .await
                .is_err()
        );
    }
    drop(client);
    fixture.shutdown_process(&state).await;
}
