use super::*;

#[tokio::test]
async fn objective_planner_source_cannot_define() {
    let repo = fixture().await;
    let result = react(
        repo.db(),
        planner(
            Event::DefinitionChanged {
                objective: definition(),
            },
            "define",
        ),
    )
    .await;
    assert!(matches!(result, Err(TruthError::Forbidden(_))));
    assert_eq!(counts(&repo).await, (0, 0, 0));
}

async fn report_row(repo: &SqlxRepo, block: &serde_json::Value) {
    let payload = serde_json::json!({"schemaVersion":1,"docRev":7,"summary":"","body":"","blocks":[{"id":"b_1111","kind":"objective","rev":0,"payload":block}]});
    sqlx::query(
        "INSERT INTO cards
        (id,track_id,kind,sort,payload,role,deletable,created_at,updated_at)
        VALUES('report','t','track-report',-1,?,'reportcard',0,0,0)",
    )
    .bind(payload.to_string())
    .execute(repo.pool())
    .await
    .unwrap();
}
fn report_source() -> InputSource {
    serde_json::from_value(serde_json::json!({"source":"report_block","report_card_id":"report","doc_rev":7,"block_id":"b_1111"})).unwrap()
}
#[tokio::test]
async fn objective_report_source_must_match_committed_block() {
    let repo = fixture().await;
    report_row(&repo, &serde_json::to_value(definition()).unwrap()).await;
    let valid = ObjectiveInput {
        track_id: "t".into(),
        event: Event::DefinitionChanged {
            objective: definition(),
        },
        source: report_source(),
    };
    let mut forged = definition();
    forged.title = "Forged".into();
    let mut cases = vec![
        ObjectiveInput {
            event: Event::DefinitionChanged { objective: forged },
            ..valid.clone()
        },
        ObjectiveInput {
            track_id: "other".into(),
            ..valid.clone()
        },
        ObjectiveInput {
            event: Event::Closed {
                objective: definition().key,
            },
            ..valid.clone()
        },
    ];
    for (field, value) in [
        ("doc_rev", serde_json::json!(8)),
        ("report_card_id", serde_json::json!("p")),
        ("block_id", serde_json::json!("b_other")),
    ] {
        let mut source = serde_json::to_value(report_source()).unwrap();
        source[field] = value;
        cases.push(ObjectiveInput {
            source: serde_json::from_value(source).unwrap(),
            ..valid.clone()
        });
    }
    for input in cases {
        assert!(matches!(
            react(repo.db(), input).await,
            Err(TruthError::Forbidden(_))
        ));
        assert_eq!(counts(&repo).await, (0, 0, 0));
    }
    // Equal f64 values can still name different serialized approval standards.
    let positive = objective_float_definition(0.0);
    let negative = objective_float_definition(-0.0);
    assert_ne!(standard_hash(&positive), standard_hash(&negative));
    let raw: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id='report'")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let mut row: serde_json::Value = serde_json::from_str(&raw).unwrap();
    row["blocks"][0]["payload"] = serde_json::to_value(negative).unwrap();
    sqlx::query("UPDATE cards SET payload=? WHERE id='report'")
        .bind(row.to_string())
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(matches!(
        react(
            repo.db(),
            ObjectiveInput {
                event: Event::DefinitionChanged {
                    objective: positive
                },
                ..valid.clone()
            }
        )
        .await,
        Err(TruthError::Forbidden(_))
    ));
    assert_eq!(counts(&repo).await, (0, 0, 0));
    row["blocks"][0]["payload"] = serde_json::to_value(definition()).unwrap();
    sqlx::query("UPDATE cards SET payload=? WHERE id='report'")
        .bind(row.to_string())
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET payload=json_remove(payload,'$.docRev') WHERE id='report'")
        .execute(repo.pool())
        .await
        .unwrap();
    let missing_revision = ObjectiveInput {
        source: InputSource::ReportBlock {
            report_card_id: "report".into(),
            doc_rev: 0,
            block_id: "b_1111".into(),
        },
        ..valid.clone()
    };
    assert!(matches!(
        react(repo.db(), missing_revision).await,
        Err(TruthError::Forbidden(_))
    ));
    assert_eq!(counts(&repo).await, (0, 0, 0));
    sqlx::query("UPDATE cards SET payload=json_set(payload,'$.docRev',7) WHERE id='report'")
        .execute(repo.pool())
        .await
        .unwrap();
    react(repo.db(), valid.clone()).await.unwrap();
    assert!(matches!(
        react(repo.db(), valid.clone()).await.unwrap().reaction(),
        Reaction::Duplicate { .. }
    ));
    let rows = log::read_tx(
        &mut begin_immediate_tx(repo.db()).await.unwrap(),
        &"t".into(),
    )
    .await
    .unwrap();
    assert_eq!(rows[0].source, report_source());
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.blocks[0].kind','app') WHERE id='report'",
    )
    .execute(repo.pool())
    .await
    .unwrap();
    assert!(matches!(
        react(repo.db(), valid).await,
        Err(TruthError::Forbidden(_))
    ));
}

fn block(definition: &ObjectiveDefinition) -> calm_types::track_report::ReportBlock {
    calm_types::track_report::ReportBlock {
        id: "b_1111".into(),
        kind: "objective".into(),
        rev: 0,
        payload: serde_json::to_value(definition).unwrap(),
    }
}
async fn project(
    repo: &SqlxRepo,
    before: &[calm_types::track_report::ReportBlock],
    after: &[calm_types::track_report::ReportBlock],
    rev: u64,
) -> Result<()> {
    let mut tx = begin_immediate_tx(repo.db()).await?;
    let payload =
        serde_json::json!({"schemaVersion":1,"docRev":rev,"summary":"","body":"","blocks":after});
    sqlx::query("UPDATE cards SET payload=? WHERE id='report'")
        .bind(payload.to_string())
        .execute(&mut **tx)
        .await?;
    super::super::report::project_report_tx(
        &mut tx,
        super::super::report::ReportObjectiveWrite {
            track_id: &"t".into(),
            report_card_id: "report",
            before,
            after,
            doc_rev: rev,
        },
        0,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
#[tokio::test]
async fn objective_added_changed_removed_lifecycle() {
    let repo = fixture().await;
    report_row(&repo, &serde_json::to_value(definition()).unwrap()).await;
    let old = vec![block(&definition())];
    project(&repo, &[], &old, 1).await.unwrap();
    assert_eq!(load_state(repo.db(), &"t".into()).await.unwrap().0.0, 1);
    let mut def = definition();
    def.title = "changed".into();
    let changed = vec![block(&def)];
    project(&repo, &old, &changed, 2).await.unwrap();
    assert_eq!(
        load_state(repo.db(), &"t".into())
            .await
            .unwrap()
            .1
            .objectives[&def.key]
            .definition,
        def
    );
    assert!(matches!(
        project(&repo, &changed, &[], 3).await,
        Err(TruthError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
    assert_eq!(load_state(repo.db(), &"t".into()).await.unwrap().0.0, 2);
    fixture_react(
        &repo,
        Event::Closed {
            objective: def.key.clone(),
        },
        "close",
    )
    .await;
    assert!(matches!(
        project(&repo, &changed, &old, 3).await,
        Err(TruthError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
    project(&repo, &changed, &[], 3).await.unwrap();
    assert!(matches!(
        project(&repo, &[], &old, 4).await,
        Err(TruthError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
    let floats = fixture().await;
    let positive = objective_float_definition(0.0);
    let negative = objective_float_definition(-0.0);
    assert_ne!(standard_hash(&positive), standard_hash(&negative));
    report_row(&floats, &serde_json::to_value(&positive).unwrap()).await;
    let old = vec![block(&positive)];
    let changed = vec![block(&negative)];
    project(&floats, &[], &old, 1).await.unwrap();
    project(&floats, &old, &changed, 2).await.unwrap();
    let (seq, state) = load_state(floats.db(), &"t".into()).await.unwrap();
    assert_eq!(seq.0, 2, "a changed canonical standard must reach the host");
    assert_eq!(
        standard_hash(&state.objectives[&negative.key].definition),
        standard_hash(&negative)
    );
    fixture_react(
        &floats,
        Event::Closed {
            objective: negative.key.clone(),
        },
        "close-float",
    )
    .await;
    assert!(matches!(
        project(&floats, &changed, &old, 3).await,
        Err(TruthError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
}

fn objective_float_definition(tolerance: f64) -> ObjectiveDefinition {
    let mut definition = definition();
    definition.checks[0].config = CheckConfig::Metric(objective::check::MetricParams {
        command: "bench".into(),
        direction: objective::check::Direction::Lower,
        tolerance: Some(tolerance),
    });
    definition
}
