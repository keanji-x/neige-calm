use super::*;
use crate::error::ErrorBody;
use axum::response::IntoResponse;

#[derive(Clone, Copy, Debug)]
enum WriteKind {
    Event,
    Events,
    ActorEvents,
    InTx,
}

const KINDS: [WriteKind; 4] = [
    WriteKind::Event,
    WriteKind::Events,
    WriteKind::ActorEvents,
    WriteKind::InTx,
];

async fn body(tx: &mut Transaction<'_, Sqlite>, error: Option<CalmError>) -> Result<Event> {
    sqlx::query("INSERT INTO typed_write_probe VALUES (42)")
        .execute(&mut **tx)
        .await?;
    if let Some(error) = error {
        return Err(error);
    }
    Ok(Event::PluginState {
        id: "probe".into(),
        state: "running".into(),
        last_error: None,
    })
}

// Exercise the server entry points with real SQLite transactions, including writes before refusal.
async fn write(
    kind: WriteKind,
    repo: &sqlite::SqlxRepo,
    bus: &EventBus,
    error: Option<CalmError>,
) -> Result<()> {
    let context = WriteContext::new(
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
    );
    match kind {
        WriteKind::Event => write_with_event_typed(
            repo,
            ActorId::Kernel,
            EventScope::System,
            None,
            bus,
            &context,
            move |tx| Box::pin(async move { Ok(((), body(tx, error).await?)) }),
        )
        .await
        .map(|_| ()),
        WriteKind::Events => {
            write_with_events_typed(repo, ActorId::Kernel, None, bus, &context, move |tx| {
                Box::pin(
                    async move { Ok(((), vec![(EventScope::System, body(tx, error).await?)])) },
                )
            })
            .await
            .map(|_| ())
        }
        WriteKind::ActorEvents => {
            write_with_actor_events_typed(repo, None, bus, &context, move |tx| {
                Box::pin(async move {
                    Ok((
                        (),
                        vec![(ActorId::Kernel, EventScope::System, body(tx, error).await?)],
                    ))
                })
            })
            .await
            .map(|_| ())
        }
        WriteKind::InTx => {
            write_in_tx_typed(repo, move |tx| {
                Box::pin(async move {
                    body(tx, error).await?;
                    Ok(())
                })
            })
            .await
        }
    }
}

async fn assert_refusal(kind: WriteKind, expected: CalmError) {
    let repo = sqlite::SqlxRepo::open("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE typed_write_probe(value INTEGER)")
        .execute(repo.pool())
        .await
        .unwrap();
    let bus = EventBus::new();
    let mut receiver = bus.subscribe();
    let (variant, code, status, field, reason) = (
        std::mem::discriminant(&expected),
        expected.code(),
        expected.status(),
        expected.field().map(str::to_owned),
        expected.reason(),
    );
    let actual = write(kind, &repo, &bus, Some(expected)).await.unwrap_err();
    // Check rollback and both event channels even when the error's shape is wrong.
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM typed_write_probe")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "{kind:?}: closure writes must roll back");
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(events, 0);
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(
        std::mem::discriminant(&actual),
        variant,
        "{kind:?}: {actual:?}"
    );
    assert_eq!(actual.code(), code);
    assert_eq!(actual.status(), status);
    assert_eq!(actual.field(), field.as_deref());
    assert_eq!(actual.reason(), reason);
    let response = actual.into_response();
    assert_eq!(response.status(), status);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: ErrorBody = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body.code, code);
    assert_eq!(body.field, field);
    assert_eq!(body.error, reason);
}

macro_rules! refusal_test {
    ($name:ident, $kind:ident, $error:expr) => {
        #[tokio::test]
        async fn $name() {
            assert_refusal(WriteKind::$kind, $error).await;
        }
    };
}

refusal_test!(
    event_preserves_invalid_field,
    Event,
    CalmError::InvalidField {
        field: "config.retries".into(),
        reason: "must be positive".into()
    }
);
refusal_test!(
    events_preserves_invalid_field,
    Events,
    CalmError::InvalidField {
        field: "config.retries".into(),
        reason: "must be positive".into()
    }
);
refusal_test!(
    actor_events_preserves_invalid_field,
    ActorEvents,
    CalmError::InvalidField {
        field: "config.retries".into(),
        reason: "must be positive".into()
    }
);
refusal_test!(
    in_tx_preserves_invalid_field,
    InTx,
    CalmError::InvalidField {
        field: "config.retries".into(),
        reason: "must be positive".into()
    }
);
refusal_test!(
    event_preserves_plugin_conflict,
    Event,
    CalmError::PluginConflict("already installed".into())
);
refusal_test!(
    events_preserves_plugin_conflict,
    Events,
    CalmError::PluginConflict("already installed".into())
);
refusal_test!(
    actor_events_preserves_plugin_conflict,
    ActorEvents,
    CalmError::PluginConflict("already installed".into())
);
refusal_test!(
    in_tx_preserves_plugin_conflict,
    InTx,
    CalmError::PluginConflict("already installed".into())
);

#[tokio::test]
async fn typed_writes_preserve_idempotency_and_replacement_refusals() {
    for kind in KINDS {
        for error in [
            CalmError::IdempotencyKeyReused("x".into()),
            CalmError::IdempotencyKeyExhausted("x".into()),
            CalmError::IdempotencyKeyConcurrent("x".into()),
            CalmError::PlannerTurnNotReplaceable("x".into()),
        ] {
            assert_refusal(kind, error).await;
        }
    }
}

#[tokio::test]
async fn typed_writes_map_commit_failure_to_db_without_events() {
    for kind in KINDS {
        let repo = sqlite::SqlxRepo::open("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE typed_write_parent(id INTEGER PRIMARY KEY)")
            .execute(repo.pool())
            .await
            .unwrap();
        sqlx::query("CREATE TABLE typed_write_probe(value INTEGER REFERENCES typed_write_parent(id) DEFERRABLE INITIALLY DEFERRED)").execute(repo.pool()).await.unwrap();
        let bus = EventBus::new();
        let mut receiver = bus.subscribe();
        let error = write(kind, &repo, &bus, None).await.unwrap_err();
        assert!(
            matches!(error, CalmError::Db(sqlx::Error::Database(_))),
            "{kind:?}: {error:?}"
        );
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM typed_write_probe")
            .fetch_one(repo.pool())
            .await
            .unwrap();
        assert_eq!(rows, 0);
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
            .fetch_one(repo.pool())
            .await
            .unwrap();
        assert_eq!(events, 0);
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
}
