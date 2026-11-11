//! Persisted guest work is recovered by a fresh real Dispatcher and runtime.
use super::*;
use crate::conversation_inputs::{InputSource, ShareInputSource};
use crate::db::sqlite::conversation::*;
use calm_types::conversation::*;
use calm_types::share_links::ShareScope;
use std::time::Duration;

async fn prepare(
    claim: bool,
    area_scope: bool,
) -> (
    Fixture,
    crate::auth::ValidatedApplicationAuth,
    ShareInputSource,
) {
    let fx = Fixture::build(true, AgentProvider::Codex, true).await;
    let auth = crate::auth::ValidatedApplicationAuth::from_state(crate::auth::AuthState::new(
        crate::auth::AuthConfig {
            username: Some("owner".into()),
            password: Some("password".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        },
    ))
    .unwrap();
    let token = auth.state.sessions.create(
        crate::auth::SessionIdentity::owner(&auth.state.config),
        crate::auth::SessionAuthority::PasswordLogin,
    );
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::COOKIE,
        format!("{}={token}", crate::auth::SESSION_COOKIE)
            .parse()
            .unwrap(),
    );
    let (_, human) = crate::auth::authenticated_source(auth.state.clone(), headers).await;
    let card = fx.repo.card_get(fx.card.as_str()).await.unwrap().unwrap();
    let track = fx
        .repo
        .track_get(card.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let scope = if area_scope {
        ShareScope::Area(track.area_id.to_string())
    } else {
        ShareScope::Track(track.id.to_string())
    };
    let link = crate::share_access::create(fx.repo.as_ref(), human.unwrap(), scope)
        .await
        .unwrap();
    let share = ShareInputSource::new(auth.execution.clone(), link.link.delegation);
    crate::db::write_in_tx_typed(fx.repo.as_ref(), {
        let (card, share) = (fx.card.to_string(), share.clone());
        move |tx| {
            Box::pin(async move {
                if claim {
                    // Production claims after native thread creation. Model a crash
                    // after that durable association but before the first attempt.
                    sqlx::query("UPDATE worker_sessions SET thread_id='accepted-thread', handle_state_json=json_set(handle_state_json,'$.last_thread_id','accepted-thread') WHERE id=(SELECT session_id FROM cards WHERE id=?1)")
                        .bind(&card).execute(&mut **tx).await?;
                }
                let mut carrier = current_input_content_carrier_tx(tx, &card).await?;
                let PendingInput::Human { message, .. } = carrier.checkpoint().pending[0].clone()
                else {
                    panic!("owner")
                };
                mutate_pending_human_tx(
                    tx,
                    &mut carrier,
                    LEGACY_OWNER_USER_ID,
                    &message,
                    PendingHumanChange::Withdraw,
                )
                .await?;
                accept_pending_message_tx(
                    tx,
                    &mut carrier,
                    &MessageAuthor::LinkGuest {
                        share_link_id: share.delegation().id.clone(),
                    },
                    HumanPayload {
                        text: "recover original guest".into(),
                        bound_attachments: vec![],
                    },
                )
                .await?;
                if claim {
                    let plan =
                        pending_plan_for_prefix_tx(tx, &carrier, std::num::NonZeroU32::MIN, vec![])
                            .await?;
                    crate::conversation_inputs::claim_initial_tx(
                        tx,
                        InputSource::Share(share),
                        &mut carrier,
                        plan,
                    )
                    .await?;
                }
                Ok(())
            })
        }
    })
    .await
    .unwrap();
    (fx, auth, share)
}

async fn checkpoint(fx: &Fixture) -> ConversationInputCheckpoint {
    crate::db::write_in_tx_typed(fx.repo.as_ref(), {
        let card = fx.card.to_string();
        move |tx| {
            Box::pin(async move {
                Ok(current_input_content_carrier_tx(tx, &card)
                    .await?
                    .checkpoint()
                    .clone())
            })
        }
    })
    .await
    .unwrap()
}

async fn restart(fx: &mut Fixture) {
    fx.state.dispatcher.retire();
    // Reopen the file with new caches, registry, native client, and Dispatcher.
    let repo = Arc::new(
        SqlxRepo::open(&format!(
            "sqlite://{}?mode=rwc",
            fx._dir.path().join("canonical.sqlite").display()
        ))
        .await
        .unwrap(),
    );
    let events = crate::event::EventBus::new();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        fx._dir.path().join("plugins"),
        fx._dir.path().join("plugin-data"),
        vec![],
        events.clone(),
        WriteContext::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
        ),
    ));
    let native = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    fx.state = AppState::from_parts(
        repo.clone(),
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(repo.card_role_cache().clone()),
        Some(repo.track_area_cache().clone()),
    )
    .with_shared_codex_appserver(native);
    fx.repo = repo;
    assert!(fx.state.harness.get(&fx.worker).is_none());
}

async fn reconcile(fx: &Fixture, auth: &crate::auth::ValidatedApplicationAuth) {
    fx.state
        .dispatcher
        .activate_legacy_execution(auth.execution.clone())
        .await
        .unwrap();
    let track = fx
        .repo
        .card_get(fx.card.as_str())
        .await
        .unwrap()
        .unwrap()
        .track_id;
    let sources = fx.state.harness.canonical_sources(&fx.worker, &track);
    tokio::time::timeout(Duration::from_secs(10), async {
        while sources.has_handoffs() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Dispatcher recovery handoff finished");
}

#[tokio::test]
async fn conversation_inputs_dispatcher_cold_recovers_pending_and_unattempted_guest_after_revocation()
 {
    for claimed in [false, true] {
        let (mut fx, auth, share) = prepare(claimed, false).await;
        let before = checkpoint(&fx).await;
        crate::db::write_in_tx_typed(fx.repo.as_ref(), {
            let id = share.delegation().id.clone();
            move |tx| {
                Box::pin(async move {
                    crate::db::sqlite::share_links::revoke_tx(tx, &id, crate::model::now_ms())
                        .await?;
                    Ok(())
                })
            }
        })
        .await
        .unwrap();
        restart(&mut fx).await;
        assert_eq!(checkpoint(&fx).await, before);
        reconcile(&fx, &auth).await;
        let resumed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let after = checkpoint(&fx).await;
                if after
                    .attempts
                    .iter()
                    .any(|a| matches!(a.delivery, InputDeliveryFact::Accepted { .. }))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            resumed.is_ok(),
            "claimed={claimed}: {:?}",
            checkpoint(&fx).await
        );
        let after = checkpoint(&fx).await;
        assert!(after.pending.is_empty());
        if claimed {
            assert_eq!(after.active, before.active);
        }
        assert_eq!(
            fx.state.shared_codex_appserver.turn_start_count_for_test(),
            1
        );
        let id = after.active.unwrap().submission_ids[0].clone();
        crate::db::write_in_tx_typed(fx.repo.as_ref(), move |tx| Box::pin(async move {
            assert!(matches!(submission_identity_tx(tx, &id).await?.execution, ConversationExecutionProvenance::ShareDelegation { delegation, .. } if delegation == *share.delegation()));
            Ok(())
        })).await.unwrap();
        fx.state.dispatcher.retire();
    }
}

#[tokio::test]
async fn conversation_inputs_dispatcher_cold_holds_guest_on_current_authority_loss() {
    for loss in ["ownership", "issuer", "scope"] {
        for claimed in [false, true] {
            let (mut fx, auth, share) = prepare(claimed, loss == "scope").await;
            let before = checkpoint(&fx).await;
            match loss {
                "ownership" => {
                    fx.repo
                        .db()
                        .writer()
                        .execute(
                            sqlx::query("UPDATE area_members SET role='member' WHERE user_id=?1")
                                .bind(LEGACY_OWNER_USER_ID),
                        )
                        .await
                        .unwrap();
                }
                "scope" => {
                    let area = fx
                        .repo
                        .area_create(
                            LEGACY_OWNER_USER_ID,
                            NewArea {
                                name: "moved".into(),
                                color: "#abc".into(),
                                sort: None,
                            },
                        )
                        .await
                        .unwrap();
                    fx.repo.db().writer().execute(sqlx::query("UPDATE tracks SET area_id=?1 WHERE id=(SELECT track_id FROM cards WHERE id=?2)").bind(area.id.as_str()).bind(fx.card.as_str())).await.unwrap();
                    let ShareScope::Area(old) = &share.delegation().scope else {
                        panic!("area scope")
                    };
                    fx.repo
                        .db()
                        .writer()
                        .execute(sqlx::query("DELETE FROM areas WHERE id=?1").bind(old))
                        .await
                        .unwrap();
                }
                _ => {}
            }
            restart(&mut fx).await;
            if loss == "issuer" {
                let execution = auth.execution.clone();
                drop(auth);
                assert!(matches!(
                    fx.state
                        .dispatcher
                        .activate_legacy_execution(execution)
                        .await,
                    Err(CalmError::Unauthorized)
                ));
            } else {
                reconcile(&fx, &auth).await;
            }
            assert!(
                fx.state.harness.get(&fx.worker).is_none(),
                "lost authority must not restore the native carrier: {loss}"
            );
            assert_eq!(checkpoint(&fx).await, before, "{loss}, claimed={claimed}");
            assert_eq!(
                fx.state.shared_codex_appserver.turn_start_count_for_test(),
                0
            );
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM conversation_messages WHERE author_kind='link_guest'",
            )
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
            assert_eq!(count, 1);
            fx.state.dispatcher.retire();
        }
    }
}

async fn finish(fx: &Fixture) {
    let thread = crate::db::write_in_tx_typed(fx.repo.as_ref(), {
        let (card, worker) = (fx.card.to_string(), fx.worker.clone());
        move |tx| {
            Box::pin(async move {
                let carrier = current_input_content_carrier_tx(tx, &card).await?;
                let original = original_input_execution_tx(tx, &carrier).await?.unwrap();
                let fence = RuntimeExecutionFence::from_carrier(&carrier);
                assert!(
                    complete_input_execution_tx(tx, &card, &worker, &fence, &original)
                        .await?
                        .is_some()
                );
                Ok(original.correlation.thread_id)
            })
        }
    })
    .await
    .unwrap();
    fx.state
        .shared_codex_appserver
        .clear_active_turn_for_test(&thread);
    fx.state
        .harness
        .get(&fx.worker)
        .unwrap()
        .refresh_canonical_input()
        .await
        .unwrap();
}

async fn wait_for_starts(fx: &Fixture, count: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if fx.state.shared_codex_appserver.turn_start_count_for_test() == count as u64
                && checkpoint(fx)
                    .await
                    .attempts
                    .iter()
                    .filter(|a| matches!(a.delivery, InputDeliveryFact::Accepted { .. }))
                    .count()
                    == count
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("ordered native dispatch");
}

#[tokio::test]
async fn conversation_inputs_mixed_guest_owner_system_queue_keeps_exact_prefixes_across_turns() {
    let (mut fx, auth, a) = prepare(false, false).await;
    let (_owner_auth, _token, owner) = source().await;
    let track = fx
        .repo
        .card_get(fx.card.as_str())
        .await
        .unwrap()
        .unwrap()
        .track_id;
    let link = crate::share_access::create(
        fx.repo.as_ref(),
        owner.clone(),
        ShareScope::Track(track.to_string()),
    )
    .await
    .unwrap();
    let b = ShareInputSource::new(auth.execution.clone(), link.link.delegation);
    let (a2, b1, o1, system, a3) = crate::db::write_in_tx_typed(fx.repo.as_ref(), {
        let (card, a, b, track, owner) = (
            fx.card.to_string(),
            a.clone(),
            b.clone(),
            track.clone(),
            owner.clone(),
        );
        move |tx| {
            Box::pin(async move {
                let mut carrier = current_input_content_carrier_tx(tx, &card).await?;
                let a2 = accept_pending_message_tx(
                    tx,
                    &mut carrier,
                    &MessageAuthor::LinkGuest {
                        share_link_id: a.delegation().id.clone(),
                    },
                    HumanPayload {
                        text: "A second".into(),
                        bound_attachments: vec![],
                    },
                )
                .await?
                .input_ref;
                let b1 = accept_pending_message_tx(
                    tx,
                    &mut carrier,
                    &MessageAuthor::LinkGuest {
                        share_link_id: b.delegation().id.clone(),
                    },
                    HumanPayload {
                        text: "B first".into(),
                        bound_attachments: vec![],
                    },
                )
                .await?
                .input_ref;
                let o1 = crate::conversation_inputs::accept_human_tx(
                    tx,
                    &owner.current_principal()?,
                    &mut carrier,
                    HumanPayload {
                        text: "Owner next".into(),
                        bound_attachments: vec![],
                    },
                )
                .await?
                .input_ref;
                let area = conversation_area_tx(tx, &card).await?;
                let event = crate::event::Event::AssistantInputRequested {
                    card_id: card.clone().into(),
                    track_id: track.clone(),
                    original_worker: carrier.worker_session_id().into(),
                    source: calm_types::system_assistant::SystemAssistantSource::TodaySummary,
                    subject: "mixed queue".into(),
                    version: 1,
                    text: "System only".into(),
                };
                let id = crate::db::sqlite::append_decision_event_in_tx(
                    tx,
                    &ActorId::Kernel,
                    &crate::event::EventScope::Card {
                        card: card.clone().into(),
                        track,
                        area: area.id,
                    },
                    None,
                    &event,
                )
                .await?;
                let fact = crate::db::sqlite::event_inputs::event_input_tx(tx, id).await?;
                let system = accept_pending_system_tx(
                    tx,
                    &mut carrier,
                    SystemObservation::try_from(
                        calm_types::observation::Observation::SystemContext {
                            text: "System only".into(),
                        },
                    )
                    .unwrap(),
                    &fact,
                )
                .await?
                .input_ref;
                let a3 = accept_pending_message_tx(
                    tx,
                    &mut carrier,
                    &MessageAuthor::LinkGuest {
                        share_link_id: a.delegation().id.clone(),
                    },
                    HumanPayload {
                        text: "A held behind System".into(),
                        bound_attachments: vec![],
                    },
                )
                .await?
                .input_ref;
                Ok((a2, b1, o1, system, a3))
            })
        }
    })
    .await
    .unwrap();
    let first = checkpoint(&fx).await.pending[0]
        .accepted_input_ref()
        .unwrap();
    restart(&mut fx).await;
    reconcile(&fx, &auth).await;
    wait_for_starts(&fx, 1).await;
    let after_a = checkpoint(&fx).await;
    assert_eq!(
        after_a
            .pending
            .iter()
            .map(|p| p.accepted_input_ref().unwrap())
            .collect::<Vec<_>>(),
        vec![b1.clone(), o1.clone(), system.clone(), a3.clone()]
    );
    let id = after_a.active.unwrap().submission_ids[0].clone();
    crate::db::write_in_tx_typed(fx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            assert_eq!(
                submission_plan_tx(tx, &id)
                    .await?
                    .plan
                    .input_refs()
                    .unwrap(),
                vec![first, a2]
            );
            Ok(())
        })
    })
    .await
    .unwrap();
    finish(&fx).await;
    reconcile(&fx, &auth).await;
    wait_for_starts(&fx, 2).await;
    assert_eq!(
        checkpoint(&fx)
            .await
            .pending
            .iter()
            .map(|p| p.accepted_input_ref().unwrap())
            .collect::<Vec<_>>(),
        vec![o1.clone(), system.clone(), a3.clone()]
    );
    finish(&fx).await;
    crate::planner_hibernation::WakeContext::new(
        RouteState::from_ref(&fx.state),
        fx.repo.clone(),
        crate::state::CodexShellState::from_ref(&fx.state),
    )
    .wake_canonical(&fx.card, InputSource::Human(owner))
    .await
    .unwrap();
    wait_for_starts(&fx, 3).await;
    finish(&fx).await;
    reconcile(&fx, &auth).await;
    let held = checkpoint(&fx).await;
    assert_eq!(
        held.pending
            .iter()
            .map(|p| p.accepted_input_ref().unwrap())
            .collect::<Vec<_>>(),
        vec![system, a3]
    );
    assert!(held.active.is_none());
    assert_eq!(
        fx.state.shared_codex_appserver.turn_start_count_for_test(),
        3
    );
    fx.state.dispatcher.retire();
}
