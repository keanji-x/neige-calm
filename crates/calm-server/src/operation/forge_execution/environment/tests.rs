use super::*;

#[tokio::test]
async fn forge_environment_owner_db_error_stays_outer_and_payload_source_error_stays_inner() {
    let fx = crate::operation::forge_action_adapter::tests::forge_runtime_fixture().await;
    // Do not authorize this rc: owner rejection must happen even before direnv trust evaluation.
    let marker = fx.cwd.path().join("rc-evaluated");
    std::fs::write(fx.cwd.path().join(".envrc"), "touch rc-evaluated\n").unwrap();
    let payload =
        forge::tests::no_event_payload(&fx, "owner-error", vec!["/bin/true".into()], "owner-error");
    let prepared = fx.prepare_operation(&payload).await;
    let frozen =
        forge::FrozenForge::from_output(crate::operation::required_output(&prepared).unwrap())
            .unwrap();
    let ctx = fx.runtime.reclaim_context();
    let id = ExecutionId::fresh().unwrap();
    let refused = WorkspaceEnvironment::prepare(ctx, &frozen, &id, vec![], None, || async {
        Err(CalmError::Db(sqlx::Error::PoolTimedOut))
    })
    .await;
    assert!(matches!(
        refused,
        Err(CalmError::Db(sqlx::Error::PoolTimedOut))
    ));
    assert!(!marker.exists());

    let mut missing = frozen.clone();
    missing.cwd_lease = fx.cwd.path().join("missing");
    let payload_error = WorkspaceEnvironment::prepare(ctx, &missing, &id, vec![], None, || async {
        panic!("payload source metadata already failed before resource admission")
    })
    .await;
    assert!(matches!(payload_error, Ok(Err(CalmError::Io(_)))));
    assert!(!marker.exists());
}
