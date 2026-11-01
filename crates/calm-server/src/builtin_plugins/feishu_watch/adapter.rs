//! Kernel-owned transactional publication; Feishu policy stays in the feature.
use super::PLUGIN_ID;
use crate::{
    db::{RouteRepo, write_in_tx_typed, write_with_events_typed},
    error::CalmError,
    event::{Event, EventBus, EventScope},
    ids::{ActorId, TrackId},
    mcp_server::registry::{AppContext, ToolCallIdentity},
    plugin_host::{ConnectorClient, PluginHost},
    state::WriteContext,
};
use plugin::{
    builtin::feishu_watch::{
        MAX_STATE_BYTES, Storage,
        model::{Config, State, Wake},
    },
    host::ports::BuiltinRun,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Serialize, Deserialize)]
struct Stored {
    config: Config,
    state: State,
}
#[derive(Clone)]
pub(super) struct Adapter {
    pub config: Config,
    repo: Arc<dyn RouteRepo>,
    events: EventBus,
    write: WriteContext,
    host: Arc<PluginHost>,
    run: Arc<dyn BuiltinRun>,
    user_config: Value,
    identity: Option<ToolCallIdentity>,
}
impl Adapter {
    pub async fn capture(
        ctx: &AppContext,
        identity: Option<ToolCallIdentity>,
    ) -> Result<Self, String> {
        let host = ctx
            .plugin_host
            .get()
            .cloned()
            .ok_or("plugin host unavailable")?;
        let _guard = host
            .try_lock_lifecycle(PLUGIN_ID)
            .map_err(|_| "inspection lifecycle busy")?;
        let Some(ConnectorClient::Builtin(run)) = host.connector_client(PLUGIN_ID).await else {
            return Err("inspection is not running".into());
        };
        // Captures the values consumed at enable/reload, rather than pending config edits.
        let reply = run
            .tools_call("configuration", &json!({}))
            .await
            .map_err(|_| "inspection configuration unavailable")?;
        let config: Config = serde_json::from_value(
            reply
                .structured_content
                .ok_or("missing inspection configuration")?,
        )
        .map_err(|_| "invalid inspection configuration")?;
        config.validate()?;
        let row = ctx
            .repo
            .plugin_get_by_id(PLUGIN_ID)
            .await
            .map_err(|_| "cannot read inspection configuration")?
            .ok_or("inspection is not installed")?;
        let pending: Config = serde_json::from_value(row.user_config.clone())
            .map_err(|_| "inspection configuration needs reload")?;
        if pending != config || !row.enabled {
            return Err("inspection configuration needs reload".into());
        }
        if let Some(caller) = &identity
            && (caller.role != crate::model::CardRole::Planner
                || caller.track_id.as_deref() != Some(config.track_id.as_str()))
        {
            return Err("only the configured Track Planner may access inspection data".into());
        }
        Ok(Self {
            config,
            repo: ctx.repo.clone(),
            events: ctx.events.clone(),
            write: ctx.write.clone(),
            host: host.clone(),
            run,
            user_config: row.user_config,
            identity,
        })
    }
    pub async fn before_read(&self) -> Result<(), String> {
        let _guard = self.lifecycle().await?;
        let this = self.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move { this.authorize_tx(tx).await.map(|_| ()) })
        })
        .await
        .map_err(|e| e.to_string())
    }
    pub fn key(&self) -> String {
        format!("watch:{}", self.config.track_id)
    }
    async fn lifecycle(&self) -> Result<crate::plugin_host::LifecycleGuard, String> {
        let guard = self
            .host
            .try_lock_lifecycle(PLUGIN_ID)
            .map_err(|_| "inspection lifecycle busy")?;
        match self.host.connector_client(PLUGIN_ID).await {
            Some(ConnectorClient::Builtin(current)) if Arc::ptr_eq(&current, &self.run) => {
                Ok(guard)
            }
            _ => Err("inspection runtime changed; discard stale scan".into()),
        }
    }
    async fn authorize_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<crate::model::Track, CalmError> {
        let row: Option<(bool, String)> =
            sqlx::query_as("SELECT enabled,user_config FROM plugins WHERE id=?")
                .bind(PLUGIN_ID)
                .fetch_optional(&mut **tx)
                .await?;
        if !row.is_some_and(|(enabled, raw)| {
            enabled && serde_json::from_str::<Value>(&raw).ok().as_ref() == Some(&self.user_config)
        }) {
            return Err(CalmError::Conflict(
                "inspection configuration changed".into(),
            ));
        }
        let track =
            crate::db::sqlite::track_get_tx(tx, &TrackId(self.config.track_id.clone())).await?;
        if track.closed_at.is_some()
            || !crate::mcp_server::tool_visibility::plugin_scope_for_track_row(
                &track,
                Some(&self.host),
            )
            .await
            .allows(PLUGIN_ID)
        {
            return Err(CalmError::Conflict(
                "inspection Track is closed or outside plugin scope".into(),
            ));
        }
        if let Some(identity) = &self.identity
            && !identity
                .session_is_active(tx, &self.config.track_id)
                .await?
        {
            return Err(CalmError::Conflict(
                "inspection Planner session is no longer current".into(),
            ));
        }
        Ok(track)
    }
    fn event(&self, track: &crate::model::Track, wake: Wake) -> (EventScope, Event) {
        (
            EventScope::Track {
                track: track.id.clone(),
                area: track.area_id.clone(),
            },
            Event::TrackWakeRequested {
                track_id: track.id.clone(),
                source: PLUGIN_ID.into(),
                key: wake.key,
                text: wake.text,
            },
        )
    }
    /// Capacity/import errors have a bounded publication independent of a full inbox.
    pub async fn diagnostic(&self, failed: bool) -> Result<(), String> {
        let _guard = self.lifecycle().await?;
        let key = format!("diagnostic:{}", self.config.track_id);
        let this = self.clone();
        write_with_events_typed(
            self.repo.as_ref(),
            ActorId::Kernel,
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let track = this.authorize_tx(tx).await?;
                    let old: Option<String> = sqlx::query_scalar(
                        "SELECT value FROM plugin_kv WHERE plugin_id=? AND key=?",
                    )
                    .bind(PLUGIN_ID)
                    .bind(&key)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let old = old.and_then(|s| serde_json::from_str::<Value>(&s).ok());
                    if old
                        .as_ref()
                        .and_then(|v| v.get("failed"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        == failed
                    {
                        return Err(CalmError::Conflict(
                            "inspection diagnostic unchanged".into(),
                        ));
                    }
                    let at = crate::model::now_ms();
                    let reason = if failed {
                        plugin::builtin::feishu_watch::FAILURE_NOTICE.trim_end()
                    } else {
                        "Inspection publication recovered."
                    };
                    let value = json!({"failed":failed,"at_ms":at,"reason":reason});
                    sqlx::query(
                        "INSERT INTO plugin_kv(plugin_id,key,value,updated_at) VALUES(?,?,?,?) \
                ON CONFLICT(plugin_id,key) DO UPDATE \
                SET value=excluded.value,updated_at=excluded.updated_at",
                    )
                    .bind(PLUGIN_ID)
                    .bind(&key)
                    .bind(value.to_string())
                    .bind(at)
                    .execute(&mut **tx)
                    .await?;
                    Ok::<_, CalmError>((
                        (),
                        vec![this.event(
                            &track,
                            Wake {
                                key: format!(
                                    "diagnostic:{at}:{}",
                                    if failed { "failed" } else { "recovered" }
                                ),
                                text: reason.into(),
                            },
                        )],
                    ))
                })
            },
        )
        .await
        .map(|_| ())
        .or_else(|error| match error {
            CalmError::Conflict(reason) if reason == "inspection diagnostic unchanged" => Ok(()),
            _ => Err("inspection diagnostic publication refused".into()),
        })
    }
    pub async fn diagnostic_value(&self) -> Result<Option<Value>, String> {
        self.repo
            .plugin_kv_get(PLUGIN_ID, &format!("diagnostic:{}", self.config.track_id))
            .await
            .map_err(|_| "cannot read inspection diagnostic".into())
    }
}
#[async_trait::async_trait]
impl Storage for Adapter {
    async fn load(&self) -> Result<Option<State>, String> {
        let _guard = self.lifecycle().await?;
        let this = self.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| Box::pin(async move {
            this.authorize_tx(tx).await?;
            let raw: Option<String> = sqlx::query_scalar("SELECT value FROM plugin_kv WHERE plugin_id=? AND key=?")
                .bind(PLUGIN_ID).bind(this.key()).fetch_optional(&mut **tx).await?;
            let Some(raw) = raw else { return Ok(None) };
            if raw.len()>MAX_STATE_BYTES { return Err(CalmError::Conflict("inspection state exceeds byte limit".into())); }
            let stored: Stored = serde_json::from_str(&raw).map_err(|_|CalmError::Conflict("invalid inspection state".into()))?;
            if stored.config != this.config { return Err(CalmError::Conflict("inspection configuration differs from retained state; operator reconciliation required".into())); }
            Ok(Some(stored.state))
        })).await.map_err(|e|e.to_string())
    }
    async fn commit(
        &self,
        expected_revision: Option<u64>,
        state: State,
        wake: Option<Wake>,
    ) -> Result<(), String> {
        let _guard = self.lifecycle().await?;
        if state.revision
            != expected_revision
                .unwrap_or(0)
                .checked_add(1)
                .ok_or("inspection revision exhausted")?
        {
            return Err("invalid inspection revision".into());
        }
        let raw = serde_json::to_string(&Stored {
            config: self.config.clone(),
            state,
        })
        .map_err(|_| "cannot encode inspection state")?;
        if raw.len() > MAX_STATE_BYTES {
            return Err(
                "inspection state exceeds 1 MiB; previous inbox and cursors retained".into(),
            );
        }
        let this = self.clone();
        match wake {
            Some(wake) => write_with_events_typed(
                self.repo.as_ref(),
                ActorId::Kernel,
                None,
                &self.events,
                &self.write,
                move |tx| {
                    Box::pin(async move {
                        let track = this.replace_tx(tx, expected_revision, raw).await?;
                        Ok(((), vec![this.event(&track, wake)]))
                    })
                },
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
            None => write_in_tx_typed(self.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    this.replace_tx(tx, expected_revision, raw)
                        .await
                        .map(|_| ())
                })
            })
            .await
            .map_err(|e| e.to_string()),
        }
    }
}
impl Adapter {
    /// Both publication modes share the same authority/CAS/write path. Only the
    /// eventful mode uses the kernel's deliberately non-empty event transaction.
    async fn replace_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        expected: Option<u64>,
        raw: String,
    ) -> Result<crate::model::Track, CalmError> {
        let track = self.authorize_tx(tx).await?;
        let old: Option<String> =
            sqlx::query_scalar("SELECT value FROM plugin_kv WHERE plugin_id=? AND key=?")
                .bind(PLUGIN_ID)
                .bind(self.key())
                .fetch_optional(&mut **tx)
                .await?;
        let old = old
            .map(|v| serde_json::from_str::<Stored>(&v))
            .transpose()
            .map_err(|_| CalmError::Conflict("invalid retained inspection state".into()))?;
        if old.as_ref().map(|v| v.state.revision) != expected
            || old.as_ref().is_some_and(|v| v.config != self.config)
        {
            return Err(CalmError::Conflict(
                "inspection state changed; retry latest checkpoint".into(),
            ));
        }
        sqlx::query(
            "INSERT INTO plugin_kv(plugin_id,key,value,updated_at) VALUES(?,?,?,?) \
            ON CONFLICT(plugin_id,key) DO UPDATE \
            SET value=excluded.value,updated_at=excluded.updated_at",
        )
        .bind(PLUGIN_ID)
        .bind(self.key())
        .bind(raw)
        .bind(crate::model::now_ms())
        .execute(&mut **tx)
        .await?;
        Ok(track)
    }
}
