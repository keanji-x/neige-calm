use super::{Error, PreparedEndpoint, Result, home};
use crate::codex_appserver::{CodexAppServer, InputItem, TurnStartResult};
use crate::planner_model::TurnModelSelection;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// Required final business admission. The caller wraps ONLY launch.issue() in
/// its TaskLaunch writer/source/owner fence, releases that guard, then returns.
/// There is deliberately no default production implementation.
#[async_trait]
pub trait TurnAdmission: Send + Sync {
    async fn admit(&self, launch: TurnLaunch) -> Result<TurnStartResult>;
}

/// One owned, non-cloneable effect. No checkpoint, database, runtime or recorder
/// handle is available here, so issue cannot nest the caller's Operation writes.
pub struct TurnLaunch {
    client: Arc<CodexAppServer>,
    endpoint: PreparedEndpoint,
    thread_id: String,
    request_key: String,
    prompt: String,
    prompt_digest: String,
    timeout: Duration,
}

impl TurnLaunch {
    pub(crate) fn new(
        client: Arc<CodexAppServer>,
        endpoint: PreparedEndpoint,
        thread_id: String,
        request_key: String,
        prompt: String,
    ) -> Self {
        let prompt_digest = home::digest(prompt.as_bytes());
        let timeout = client.request_timeout();
        Self {
            client,
            endpoint,
            thread_id,
            request_key,
            prompt,
            prompt_digest,
            timeout,
        }
    }

    pub fn endpoint(&self) -> &PreparedEndpoint {
        &self.endpoint
    }
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }
    pub fn request_key(&self) -> &str {
        &self.request_key
    }
    pub fn prompt_digest(&self) -> &str {
        &self.prompt_digest
    }
    pub fn control_timeout(&self) -> Duration {
        self.timeout
    }

    /// Bounded existing turn/start send AND acknowledgement, not turn completion.
    /// Timeout/lost reply leaves the already-persisted IssuingTurn uncertain.
    pub async fn issue(self) -> Result<TurnStartResult> {
        match tokio::time::timeout(
            self.timeout,
            // #1505 S4-3: an isolated task's thread is minted for that one
            // task and no picker ever addresses it, so this kernel has never
            // put a sticky model override on it. `inherit` is that fact
            // spelled out, and it is byte-identical to the frame this call
            // sent before the selection existed.
            self.client.turn_start(
                &self.thread_id,
                vec![InputItem::text(self.prompt)],
                &TurnModelSelection::inherit(),
            ),
        )
        .await
        {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(Error::Unknown(
                "turn control reply unavailable; no automatic replay".into(),
            )),
            Err(_) => Err(Error::Unknown(
                "turn control send/ack deadline elapsed; outcome unknown".into(),
            )),
        }
    }
}
