use calm_truth::db::sqlite::{SqlxRepo, begin_immediate_tx, gate_execution as sql};
use serde_json::{Value, json};

use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo, TxOutput, new_id};

#[tokio::test]
async fn c3_keyed_record_cas_never_touches_another_key() {
    let db = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let repo = SqlxOperationRepo::new(db.db().clone());
    let id = repo
        .insert_operation(
            "keyed-owner",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    let mut output = TxOutput::new("unknown", None, json!({}));
    output.data = json!({"other":"preserved"});
    sqlx::query("UPDATE operations SET phase='spawn_started',tx_output_json=?2 WHERE id=?1")
        .bind(&id)
        .bind(serde_json::to_string(&output).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    let gate = json!({"version":1,"execution_id":"gate","release":"Held","decision":"Observing"});
    let terminal =
        json!({"version":1,"execution_id":"terminal","release":"Held","decision":"Observing"});
    let updated = json!({"version":1,"execution_id":"gate","release":{"Released":{"authorized_at_ms":42}},"decision":"Observing"});
    let mut tx = begin_immediate_tx(db.db()).await.unwrap();
    sql::insert(&mut tx, &id, "gate_execution", &gate.to_string())
        .await
        .unwrap();
    sql::insert(&mut tx, &id, "terminal_launch", &terminal.to_string())
        .await
        .unwrap();
    assert!(
        sql::insert(&mut tx, &id, "gate_execution", &terminal.to_string())
            .await
            .is_err()
    );
    // A snapshot from another key cannot authorize a write or satisfy a completion fence.
    assert!(
        sql::compare_exchange(
            &mut tx,
            &id,
            "terminal_launch",
            &gate.to_string(),
            &updated.to_string()
        )
        .await
        .is_err()
    );
    assert!(
        sql::require_record(&mut tx, &id, "terminal_launch", &gate.to_string())
            .await
            .is_err()
    );
    sql::compare_exchange(
        &mut tx,
        &id,
        "gate_execution",
        &gate.to_string(),
        &updated.to_string(),
    )
    .await
    .unwrap();
    assert!(
        sql::compare_exchange(
            &mut tx,
            &id,
            "gate_execution",
            &gate.to_string(),
            &terminal.to_string()
        )
        .await
        .is_err()
    );
    assert!(
        sql::require_record(&mut tx, &id, "gate_execution", &gate.to_string())
            .await
            .is_err()
    );
    sql::require_record(&mut tx, &id, "gate_execution", &updated.to_string())
        .await
        .unwrap();
    sql::require_record(&mut tx, &id, "terminal_launch", &terminal.to_string())
        .await
        .unwrap();
    let reordered = r#"{"version":1,"release":{"Released":{"authorized_at_ms":42}},"decision":"Observing","execution_id":"gate"}"#;
    sql::require_record(&mut tx, &id, "gate_execution", reordered)
        .await
        .unwrap();
    let mut extra = updated.clone();
    extra["unexpected"] = json!(true);
    assert!(
        sql::require_record(&mut tx, &id, "gate_execution", &extra.to_string())
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    let value: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
        .bind(&id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&value).unwrap();
    assert_eq!(
        value["data"],
        json!({"other":"preserved","gate_execution":updated,"terminal_launch":terminal})
    );
}
