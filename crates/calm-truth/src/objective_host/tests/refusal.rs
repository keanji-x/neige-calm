use super::*;

#[tokio::test]
async fn identical_planner_retry_is_duplicate() {
    let repo = fixture().await;
    let input = planner(
        Event::SpecChanged {
            objective: definition(),
        },
        "same",
    );
    react(repo.db(), input.clone()).await.unwrap();
    let n = counts(&repo).await;
    assert!(matches!(
        react(repo.db(), input).await.unwrap().reaction(),
        Reaction::Duplicate { seq: LogSeq(1) }
    ));
    assert_eq!(counts(&repo).await, n);
    let result = react(
        repo.db(),
        planner(
            Event::Closed {
                objective: definition().key,
            },
            "same",
        ),
    )
    .await;
    // A source may emit distinct event kinds; a changed payload of the same kind conflicts.
    result.unwrap();
    let mut changed = definition();
    changed.title = "changed".into();
    assert!(matches!(
        react(
            repo.db(),
            planner(Event::SpecChanged { objective: changed }, "same")
        )
        .await,
        Err(TruthError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
    assert_eq!(counts(&repo).await.0, 2);
}

#[tokio::test]
async fn assistant_session_cannot_define() {
    let repo = fixture().await;
    let mut input = planner(
        Event::SpecChanged {
            objective: definition(),
        },
        "a",
    );
    input.source = InputSource::Planner {
        principal: principal("a"),
        request_id: PlannerRequestId::new("a"),
    };
    assert!(matches!(
        react(repo.db(), input).await,
        Err(TruthError::Forbidden(_))
    ));
    assert_eq!(counts(&repo).await, (0, 0, 0));
}

#[tokio::test]
async fn closed_authorization_matrix() {
    let repo = fixture().await;
    let mut events = scenario();
    events.push(Event::RevisionDelivered {
        revision: Revision::new("r1"),
    });
    for (i, event) in events.into_iter().enumerate() {
        for source in ["p", "w", "foreign", "missing"] {
            for harness_source in [false, true] {
                let allowed = source == "p"
                    && if harness_source {
                        matches!(event, Event::HostIdle { .. } | Event::Usage { .. })
                    } else {
                        matches!(
                            event,
                            Event::SpecChanged { .. }
                                | Event::Requested { .. }
                                | Event::Settled { .. }
                                | Event::Closed { .. }
                        )
                    };
                let input = ObjectiveInput {
                    track_id: "t".into(),
                    event: event.clone(),
                    source: if harness_source {
                        InputSource::HarnessTurn {
                            session_id: source.into(),
                            turn_id: format!("{i}-{source}"),
                        }
                    } else {
                        InputSource::Planner {
                            principal: principal(source),
                            request_id: PlannerRequestId::new(format!("{i}-{source}")),
                        }
                    },
                };
                let before = counts(&repo).await;
                let result = react(repo.db(), input).await;
                assert_eq!(
                    result.is_ok(),
                    allowed,
                    "{event:?} {source} {harness_source}: {result:?}"
                );
                if !allowed {
                    assert_eq!(counts(&repo).await, before);
                }
            }
        }
    }
    for principal in [Principal::Kernel, Principal::User] {
        let mut input = planner(
            Event::SpecChanged {
                objective: definition(),
            },
            "non-agent",
        );
        input.source = InputSource::Planner {
            principal,
            request_id: PlannerRequestId::new("non-agent"),
        };
        assert!(matches!(
            react(repo.db(), input).await,
            Err(TruthError::Forbidden(_))
        ));
    }
    sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id='p'")
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(
        react(repo.db(), harness(Event::HostIdle { queue_len: 0 }, "dead"))
            .await
            .is_err()
    );
    assert!(
        react(
            repo.db(),
            planner(
                Event::Closed {
                    objective: definition().key
                },
                "dead"
            )
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn harness_source_is_session_bound_and_active() {
    let repo = fixture().await;
    let input = harness(
        Event::Usage {
            usage: Usage::default(),
        },
        "turn",
    );
    react(repo.db(), input.clone()).await.unwrap();
    assert!(matches!(
        react(repo.db(), input).await.unwrap().reaction(),
        Reaction::Duplicate { .. }
    ));
    // Still active in worker_sessions, but no longer the card's selected session.
    sqlx::query("UPDATE cards SET session_id=NULL WHERE id='p'")
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(
        react(
            repo.db(),
            harness(Event::HostIdle { queue_len: 0 }, "stale")
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn non_finite_metric_is_refused_and_writes_nothing() {
    let repo = fixture().await;
    let event = Event::CheckFinished {
        objective: definition().key,
        evaluation: EvaluationId::new(0),
        check: CheckId::new("check"),
        revision: Revision::new("r0"),
        sample: 0,
        output: SampleOutput::Metric(MetricOutput { value: f64::NAN }),
    };
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let result = react_fixture_tx(&mut tx, &planner(event, "nan"), 0).await;
    assert!(matches!(
        result,
        Err(TruthError::Core(calm_types::error::CoreError::BadRequest(
            _
        )))
    ));
    tx.rollback().await.unwrap();
    assert_eq!(counts(&repo).await, (0, 0, 0));
}

#[tokio::test]
async fn intent_insert_failure_rolls_back_every_write() {
    let repo = fixture().await;
    react(
        repo.db(),
        harness(
            Event::Usage {
                usage: Usage::default(),
            },
            "first",
        ),
    )
    .await
    .unwrap();
    let before = load_state(repo.db(), &"t".into()).await.unwrap();
    sqlx::query("CREATE TRIGGER refuse_intents BEFORE INSERT ON objective_intents BEGIN SELECT RAISE(ABORT,'test failure'); END").execute(repo.pool()).await.unwrap();
    assert!(
        react(
            repo.db(),
            planner(
                Event::SpecChanged {
                    objective: definition()
                },
                "fail"
            )
        )
        .await
        .is_err()
    );
    assert_eq!(load_state(repo.db(), &"t".into()).await.unwrap(), before);
    assert_eq!(counts(&repo).await, (1, 1, 0));
}

#[tokio::test]
async fn effect_collision_rolls_back() {
    let repo = fixture().await;
    fixture_react(
        &repo,
        Event::SpecChanged {
            objective: definition(),
        },
        "define",
    )
    .await;
    fixture_react(&repo, scenario()[1].clone(), "approve").await;
    fixture_react(&repo, scenario()[2].clone(), "request").await;
    let before = counts(&repo).await;
    let state = load_state(repo.db(), &"t".into()).await.unwrap();
    // Corrupt only the snapshot to provoke an engine effect identity collision on the next event.
    let mut rewound = state.1.clone();
    let objective = rewound.objectives.get_mut(&definition().key).unwrap();
    objective.evaluation = None;
    objective.evaluations = 0;
    sqlx::query("UPDATE objective_states SET state_json=?")
        .bind(serde_json::to_string(&rewound).unwrap())
        .execute(repo.pool())
        .await
        .unwrap();
    let result = react(repo.db(), planner(scenario()[2].clone(), "collision")).await;
    assert!(matches!(result, Err(TruthError::Internal(_))), "{result:?}");
    assert_eq!(counts(&repo).await, before);
    assert_eq!(load_state(repo.db(), &"t".into()).await.unwrap().1, rewound);
}

#[tokio::test]
async fn log_is_append_only() {
    let repo = fixture().await;
    react(
        repo.db(),
        planner(
            Event::SpecChanged {
                objective: definition(),
            },
            "first",
        ),
    )
    .await
    .unwrap();
    assert!(
        sqlx::query("UPDATE objective_events SET appended_at_ms=1")
            .execute(repo.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM objective_events")
            .execute(repo.pool())
            .await
            .is_err()
    );
    assert_eq!(counts(&repo).await, (1, 1, 1));
}
