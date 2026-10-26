//! A peer double that answers as Grok Build 1.0.62: `_meta.agentVersion` only, `cached_token` on
//! every launch, `loadSession` declared, and a turn it ends `cancelled` once a request is declined.
//! Canonical input runs against it; a Grok Planner stored before canonical input stays historical.
#![cfg(test)]
use super::*;

#[tokio::test]
async fn canonical_grok_kept_unknown_loads_same_native_on_new_process() {
    super::continuation::kept_unknown_loads_same_native_on_new_process(AgentProvider::Grok).await;
}

#[tokio::test]
async fn canonical_grok_unsupported_load_and_registration_drift_refuse_without_new() {
    super::continuation::unsupported_load_and_registration_drift_refuse_without_new(
        AgentProvider::Grok,
    )
    .await;
}

/// The Neige-authored decline the person sees, with the permission-mode advice.
const DECLINED: &str = "The agent ended the turn because Neige declined `Execute `echo one > \
                        one.txt`` under the `never` permission mode. Switch the permission mode \
                        to `ask` to approve such a request, or to `full` to allow it.";

/// Under `never` Neige answers Grok's shell request `cancelled` and Grok ends its turn
/// `cancelled`: the receipt settles known and failed with the decline, never interrupted, END
/// qualifies once, and the next input continues the same session.
#[tokio::test]
async fn canonical_grok_declined_request_ends_failed_and_settles_its_receipt() {
    let fx = Fixture::with_provider(AgentProvider::Grok, "grok-shell", false).await;
    let report = fx.dispatch().await.unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    fx.wait_idle().await;
    let (_, _, state, outcome) = fx.receipt_bytes(&fx.submission).await;
    assert_eq!(state, "completed");
    let outcome: serde_json::Value = serde_json::from_str(&outcome.unwrap()).unwrap();
    assert_eq!(outcome["status"], "failed", "{outcome}");
    assert_eq!(outcome["error"]["message"], DECLINED, "{outcome}");
    let replies: Vec<serde_json::Value> =
        std::fs::read_to_string(fx.dir.path().join("permission-log.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(
        replies,
        [serde_json::json!({"event":"reply","outcome":{"outcome":"cancelled"}})]
    );
    fx.wait_end().await;
    assert!(fx.complete().await.is_none(), "END is consumed once");
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    assert!(carrier.checkpoint().active.is_none());
    // The turn ended: neither interrupted nor wedged, and the next input is admitted.
    assert_eq!(carrier.phase().unwrap(), HarnessPhaseTag::TurnCompleted);
    tx.commit().await.unwrap();
    // One terminal row, failed, carrying the reason the person reads today: the driver records
    // Neige's decline before END, and END does not replace an existing row. Whether ACP canonical
    // execution should apply `canonical_diagnostic` to it instead is tracked in #3091.
    let ended: Vec<String> = sqlx::query_scalar(
        "SELECT params FROM harness_items WHERE worker_session_id=?1 AND method='turn/completed'",
    )
    .bind(&fx.owner.worker)
    .fetch_all(fx.owner.repo.pool())
    .await
    .unwrap();
    assert_eq!(ended.len(), 1, "{ended:?}");
    let ended: serde_json::Value = serde_json::from_str(&ended[0]).unwrap();
    assert_eq!(ended["status"], "failed", "{ended}");
    assert_eq!(ended["error"]["message"], DECLINED, "{ended}");
    assert!(
        !crate::db::sqlite::acp_submission_unresolved(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap(),
        "a declined turn leaves no unresolved receipt"
    );
    std::fs::write(fx.dir.path().join("scenario"), "reply").unwrap();
    let successor = fx.claim_successor().await;
    fx.dispatch_submission(&successor).await;
    fx.wait_idle().await;
    fx.wait_end().await;
    let (_, _, state, outcome) = fx.receipt_bytes(&successor).await;
    assert_eq!(state, "completed");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&outcome.unwrap()).unwrap()["status"],
        "completed"
    );
    let requests = fx.requests();
    let prompts: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 2);
    let loaded: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "session/load")
        .collect();
    assert_eq!(loaded.len(), 1);
    assert_eq!(
        loaded[0]["params"]["sessionId"],
        prompts[0]["params"]["sessionId"]
    );
    fx.session.shutdown().await.unwrap();
}

/// A Grok Planner stored before canonical input keeps its historical entry: the prompt runs on
/// the historical receipt codec and no canonical Turn, submission or claim appears.
#[tokio::test]
async fn canonical_grok_historical_storage_keeps_its_historical_entry() {
    let (_backend, session, owner, dir, _held) =
        super::super::fixture_storage_for(AgentProvider::Grok, false).await;
    let request = provider::control::TurnRequest {
        input: provider::control::InputRequest {
            items: vec![InputItem::text("historical")],
            correlation: provider::control::CorrelationKey("historical-grok".into()),
        },
        selection: TurnModelSelection::inherit(),
        permission: PlannerPermissionMode::Never,
        edit: provider::control::TurnEdit::NoEdit,
    };
    let claim = [crate::harness::QueueEntry::user_message(
        "historical".into(),
        None,
        vec![],
    )];
    let TurnStartOutcome::Started { .. } = session
        .start_turn(
            &owner.thread,
            request,
            &claim,
            &session.handoff_for_test().await,
        )
        .await
        .unwrap()
    else {
        panic!("historical Grok turn did not start");
    };
    let receipt = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let receipt: Option<(String, String)> = sqlx::query_as(
                "SELECT input_json,state FROM acp_submissions WHERE worker_session_id=?1",
            )
            .bind(&owner.worker)
            .fetch_optional(owner.repo.pool())
            .await
            .unwrap();
            if let Some((input, state)) = receipt
                && state == "completed"
            {
                break input;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        crate::db::sqlite::AcpSubmissionInput::decode(&receipt).unwrap(),
        crate::db::sqlite::AcpSubmissionInput::Historical(_)
    ));
    let mut connection = owner.repo.pool().acquire().await.unwrap();
    assert!(
        !card_has_canonical_storage(&mut connection, &owner.card)
            .await
            .unwrap()
    );
    drop(connection);
    for table in ["conversation_turns", "conversation_input_submissions"] {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(owner.repo.pool())
            .await
            .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
    let methods: Vec<String> = std::fs::read_to_string(dir.path().join("requests.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["method"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "authenticate",
            "session/new",
            "session/prompt"
        ]
    );
    session.shutdown().await.unwrap();
}
