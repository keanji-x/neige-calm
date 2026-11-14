use super::*;
use crate::model::NewTerminal;
use workspace_runtime::execution::{Store, test_support};

async fn card(h: &Harness) -> String {
    dispatch(
        &h.ctx(),
        "neige.card.create",
        json!({"track_id":h.track_id,"kind":"plugin:p1:demo"}),
    )
    .await
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .into()
}
async fn terminal(h: &Harness, card: &str) {
    h.ctx_storage
        .repo
        .terminal_create(NewTerminal {
            card_id: card.into(),
            program: "/bin/true".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn legacy_disposal_plugin_terminal_refused() {
    let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
    let id = card(&h).await;
    terminal(&h, &id).await;
    let error = dispatch(&h.ctx(), "neige.card.delete", json!({"card_id":id}))
        .await
        .unwrap_err();
    assert_eq!(error.code, permission_denied(String::new()).code);
    assert!(h.ctx_storage.repo.card_get(&id).await.unwrap().is_some());
    assert!(
        h.ctx_storage
            .repo
            .terminal_get_by_card(&id)
            .await
            .unwrap()
            .is_some()
    );
}

async fn execution(h: &Harness, id: &str) {
    let store =
        crate::execution_store::SqliteExecutionStore::new(h.ctx_storage.sqlx_repo.db().clone())
            .with_projection(
                test_support::TEST_CONSUMER.into(),
                Arc::new(crate::execution_store::tests::NoopProjection),
            );
    let mut intent = test_support::intent("/".into());
    intent.metadata.track = workspace_runtime::execution::TrackId(h.track_id.clone());
    let owner = intent.metadata.owner.0.clone();
    let output = json!({"target_type":"card", "target_id":id, "result":{}, "data":{"execution_id":intent.metadata.id}});
    h.ctx_storage.sqlx_repo.db().writer().execute(sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_id,target_json,\
             payload_json,tx_output_json,phase,created_at_ms,updated_at_ms) VALUES(?1,?1,'test','hash',\
             'card',?2,'{}','{}',?3,'succeeded',0,0)").bind(&owner).bind(id).bind(output.to_string())).await.unwrap();
    h.ctx_storage
        .sqlx_repo
        .db()
        .writer()
        .with_writer(|writer| {
            Box::pin(async move {
                let mut tx = writer.begin_immediate().await.unwrap();
                store.insert(&mut tx, &intent).await.unwrap();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn legacy_disposal_plugin_owned_execution_refused() {
    let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
    let id = card(&h).await;
    execution(&h, &id).await;
    let error = dispatch(&h.ctx(), "neige.card.delete", json!({"card_id":id}))
        .await
        .unwrap_err();
    assert_eq!(error.code, permission_denied(String::new()).code);
    assert!(h.ctx_storage.repo.card_get(&id).await.unwrap().is_some());
}

#[tokio::test]
async fn legacy_disposal_plugin_transaction_rechecks_terminal_race() {
    transaction_race(false).await;
}

#[tokio::test]
async fn legacy_disposal_plugin_transaction_rechecks_execution_race() {
    transaction_race(true).await;
}

async fn transaction_race(with_execution: bool) {
    let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
    let id = card(&h).await;
    let pause = crate::test_seams::PausePoint::default();
    let point = "plugin-card-delete-before-tx";
    crate::test_seams::install_pause_for_test(point, &id, pause.clone());
    struct Release {
        point: &'static str,
        key: String,
        pause: crate::test_seams::PausePoint,
    }
    impl Drop for Release {
        fn drop(&mut self) {
            crate::test_seams::take(self.point, &self.key);
            self.pause.release.notify_one();
        }
    }
    let _release = Release {
        point,
        key: id.clone(),
        pause: pause.clone(),
    };
    let deleting = async { dispatch(&h.ctx(), "neige.card.delete", json!({"card_id":id})).await };
    let racing = async {
        tokio::time::timeout(std::time::Duration::from_secs(2), pause.entered.notified())
            .await
            .unwrap();
        if with_execution {
            execution(&h, &id).await;
        } else {
            terminal(&h, &id).await;
        }
        pause.release.notify_one();
    };
    let (result, ()) = tokio::join!(deleting, racing);
    assert_eq!(
        result.unwrap_err().code,
        permission_denied(String::new()).code
    );
    assert!(h.ctx_storage.repo.card_get(&id).await.unwrap().is_some());
}
