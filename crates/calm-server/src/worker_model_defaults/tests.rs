use super::*;
use crate::db::{RepoOutOfDomain, RepoRead};
use axum::extract::FromRef;
use serde_json::json;
use std::future::Future;
use std::sync::Arc;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::{Layer, layer::Context, prelude::*};

#[derive(Clone, Debug)]
struct Warning {
    fields: std::collections::BTreeMap<String, String>,
}

impl tracing::field::Visit for Warning {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.fields.insert(field.name().into(), value.into());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().into(), format!("{value:?}"));
    }
}

/// Local to the polled production future, never a process-global subscriber.
#[derive(Clone, Default)]
pub(crate) struct Warnings(Arc<std::sync::Mutex<Vec<Warning>>>);

impl<S: tracing::Subscriber> Layer<S> for Warnings {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if *event.metadata().level() == tracing::Level::WARN {
            let mut warning = Warning {
                fields: Default::default(),
            };
            event.record(&mut warning);
            self.0.lock().unwrap().push(warning);
        }
    }
}

impl Warnings {
    pub(crate) async fn run<T>(&self, future: impl Future<Output = T>) -> T {
        future
            .with_subscriber(tracing_subscriber::registry().with(self.clone()))
            .await
    }

    pub(crate) fn assert_legacy(&self, card_id: &str, provider: WorkerModelProvider) {
        let warnings = self.legacy_for(card_id);
        assert_eq!(
            warnings.len(),
            1,
            "one limitation warning per entry: {warnings:?}"
        );
        let warning = &warnings[0];
        assert_eq!(
            warning.fields.get("provider"),
            Some(&format!("{provider:?}"))
        );
        let message = &warning.fields["message"];
        for meaning in ["model", "effort", "cannot be guaranteed"] {
            assert!(message.contains(meaning), "{warning:?}");
        }
    }

    pub(crate) fn assert_no_legacy(&self, card_id: &str) {
        assert!(
            self.legacy_for(card_id).is_empty(),
            "{:?}",
            self.0.lock().unwrap()
        );
    }

    fn legacy_for(&self, card_id: &str) -> Vec<Warning> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|warning| {
                warning
                    .fields
                    .get("card_id")
                    .is_some_and(|id| id == card_id)
                    && warning.fields.get("message").is_some_and(|message| {
                        message.contains("legacy") && message.contains("model snapshot")
                    })
            })
            .cloned()
            .collect()
    }
}

#[tokio::test]
async fn worker_model_defaults_known_invalid_effort_is_refused_unknown_pair_is_preserved() {
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let wiring = crate::harness::wiring::PlannerWiring::unconfigured_for_test(repo.clone());
    let state = crate::state::AppState::from_parts(
        repo.clone(),
        crate::event::EventBus::new(),
        Arc::new(crate::state::DaemonClient::new_stub()),
        wiring.plugin,
        Arc::new(crate::state::CodexClient::new_stub()),
        None,
        None,
    );
    let model = provider::claude::discovery::ClaudeModel {
        value: "known".into(),
        resolved_model: "provider-reported".into(),
        display_name: "Known".into(),
        description: String::new(),
        effort_levels: vec!["high".into()],
    };
    let routes = crate::state::RouteState::from_ref(&state);
    routes
        .provider_availability
        .seed_claude_catalog(crate::claude_planner::models::ClaudeCatalog {
            default: model.clone(),
            models: vec![model],
            fetched_at_ms: crate::model::now_ms(),
        })
        .await;
    let codex = crate::state::CodexShellState::from_ref(&state);
    let key = WorkerModelProvider::Claude.setting_key();
    let mut patch = std::collections::BTreeMap::from([(
        key.to_owned(),
        Some(json!({"model":"known","effort":"low"}).to_string()),
    )]);
    assert!(matches!(validate_patch(&mut patch, &routes, &codex).await,
        Err(CalmError::InvalidField {field,..}) if field == format!("settings.{key}")));
    patch.insert(
        key.into(),
        Some(json!({"model":"private","effort":"future"}).to_string()),
    );
    validate_patch(&mut patch, &routes, &codex).await.unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(patch[key].as_ref().unwrap()).unwrap(),
        json!({"model":"private","effort":"future"})
    );
    assert!(repo.settings_get_all().await.unwrap().is_empty());
}

#[tokio::test]
async fn worker_model_defaults_transaction_orders_setting_writes_without_mixing_pair() {
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let key = WorkerModelProvider::Codex.setting_key();
    repo.settings_upsert(key, r#"{"model":"a","effort":"high"}"#)
        .await
        .unwrap();
    let (start, started) = tokio::sync::oneshot::channel();
    let (polled, pending) = tokio::sync::oneshot::channel();
    let update_repo = repo.clone();
    let update = tokio::spawn(async move {
        started.await.unwrap();
        let mut update =
            Box::pin(update_repo.settings_upsert(key, r#"{"model":"b","effort":"low"}"#));
        std::future::poll_fn(|cx| {
            assert!(
                update.as_mut().poll(cx).is_pending(),
                "settings writer must wait for publication"
            );
            std::task::Poll::Ready(())
        })
        .await;
        polled.send(()).unwrap();
        update.await.unwrap();
    });
    let first = crate::db::write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            start.send(()).unwrap();
            pending.await.unwrap();
            snapshot_tx(tx, WorkerModelProvider::Codex).await
        })
    })
    .await
    .unwrap();
    assert_eq!(
        selection(&first),
        TurnModelSelection {
            model: Some("a".into()),
            effort: Some("high".into())
        }
    );
    update.await.unwrap();
    let second = crate::db::write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move { snapshot_tx(tx, WorkerModelProvider::Codex).await })
    })
    .await
    .unwrap();
    assert_eq!(
        selection(&second),
        TurnModelSelection {
            model: Some("b".into()),
            effort: Some("low".into())
        }
    );
}

#[test]
fn worker_model_defaults_bad_snapshot_is_not_inheritance_or_another_backend() {
    for value in [
        json!(null),
        json!({"provider":"claude","selection":{"kind":"inherit"}}),
        json!({"provider":"codex","selection":{"kind":"explicit","value":{"model":"","effort":null}}}),
    ] {
        assert!(matches!(
            from_payload(
                &json!({"worker_model_selection":value}),
                WorkerModelProvider::Codex
            ),
            Err(CalmError::Conflict(_))
        ));
    }
    assert_eq!(
        from_payload(&json!({}), WorkerModelProvider::Codex).unwrap(),
        None
    );
}

/// These publication-only fixtures never run a worker or a delivery process. Retire
/// their task and settle the unused delivery before publishing the next worker.
pub(crate) async fn retire_publication_fixture(
    repo: &dyn crate::db::RepoEventWrite,
    task_id: String,
    card_id: String,
) {
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE tasks SET status='done',finished_at_ms=1 WHERE id=?")
                .bind(task_id)
                .execute(&mut **tx)
                .await?;
            let deliveries: Vec<String> = sqlx::query_scalar(
                "SELECT delivery_id FROM task_git_deliveries WHERE card_id=? AND settlement IS NULL"
            ).bind(card_id).fetch_all(&mut **tx).await?;
            for delivery in deliveries {
                crate::git_candidate::delivery::settle_failed_tx(
                    tx,
                    &delivery,
                    1,
                    calm_types::git_candidate::DeliveryFailureCode::Unresolved,
                    "publication fixture has no execution to deliver",
                    false,
                )
                .await?;
            }
            Ok(())
        })
    })
    .await
    .unwrap();
}
