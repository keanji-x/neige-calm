use super::wire::*;
use crate::error::Result;
use crate::planner_model::TurnModelSelection;
use serde_json::Value;
use std::{path::Path, time::Duration};

pub struct TestingCodexAppServer {
    inner: super::wire::CodexAppServer,
}
impl TestingCodexAppServer {
    pub async fn connect(path: impl AsRef<Path>) -> Result<(Self, NotificationStream)> {
        let (inner, notifications) = super::wire::CodexAppServer::connect(path).await?;
        Ok((Self { inner }, notifications))
    }
    pub fn with_request_timeout(self, timeout: Duration) -> Self {
        Self {
            inner: self.inner.with_request_timeout(timeout),
        }
    }
    pub fn request_timeout(&self) -> Duration {
        self.inner.request_timeout()
    }
    pub async fn initialize(&self, client_info: ClientInfo) -> Result<InitializeResult> {
        self.inner.initialize(client_info).await
    }
    pub async fn thread_start(&self, developer_instructions: Option<&str>) -> Result<ThreadResult> {
        self.inner.thread_start(developer_instructions).await
    }
    pub async fn thread_start_with_params(
        &self,
        params: ThreadStartParams,
    ) -> Result<ThreadResult> {
        self.inner.thread_start_with_params(params).await
    }
    pub async fn thread_read_full(&self, thread_id: &str) -> Result<ThreadResult> {
        self.inner.thread_read_full(thread_id).await
    }
    pub async fn thread_resume(&self, thread_id: &str) -> Result<ThreadResult> {
        self.inner.thread_resume(thread_id).await
    }
    pub async fn thread_resume_with_config(
        &self,
        thread_id: &str,
        config: Option<serde_json::Value>,
    ) -> Result<ThreadResult> {
        self.inner
            .thread_resume_with_config(thread_id, config)
            .await
    }
    pub async fn thread_resume_with_sandbox(
        &self,
        thread_id: &str,
        config: Option<serde_json::Value>,
        sandbox: Option<&str>,
    ) -> Result<ThreadResult> {
        self.inner
            .thread_resume_with_sandbox(thread_id, config, sandbox)
            .await
    }
    pub async fn thread_resume_with_permissions(
        &self,
        thread_id: &str,
        config: Option<Value>,
        permissions: &PermissionsChoice,
    ) -> Result<ThreadResult> {
        self.inner
            .thread_resume_with_permissions(thread_id, config, permissions)
            .await
    }
    pub async fn background_terminals_stopped(&self, thread: &str) -> Result<bool> {
        self.inner.background_terminals_stopped(thread).await
    }
    pub async fn thread_read(
        &self,
        thread_id: &str,
        include_turns: bool,
    ) -> Result<ThreadReadResponse> {
        self.inner.thread_read(thread_id, include_turns).await
    }
    pub async fn thread_loaded_list(&self) -> Result<Vec<String>> {
        self.inner.thread_loaded_list().await
    }
    pub async fn thread_unsubscribe(
        &self,
        thread_id: &str,
        deadline: tokio::time::Instant,
    ) -> Result<ThreadUnsubscribeResponse> {
        self.inner.thread_unsubscribe(thread_id, deadline).await
    }
    pub async fn turn_start(
        &self,
        thread_id: &str,
        input: Vec<InputItem>,
        selection: &TurnModelSelection,
    ) -> Result<TurnStartResult> {
        self.inner.turn_start(thread_id, input, selection).await
    }
    pub async fn turn_start_with_client_id(
        &self,
        thread_id: &str,
        input: Vec<InputItem>,
        selection: &TurnModelSelection,
        client_user_message_id: Option<&str>,
    ) -> Result<TurnStartResult> {
        self.inner
            .turn_start_with_client_id(thread_id, input, selection, client_user_message_id)
            .await
    }
    pub async fn turn_start_with_permissions(
        &self,
        thread_id: &str,
        input: Vec<InputItem>,
        selection: &TurnModelSelection,
        client_user_message_id: Option<&str>,
        permissions: &PermissionsChoice,
    ) -> Result<TurnStartResult> {
        self.inner
            .turn_start_with_permissions(
                thread_id,
                input,
                selection,
                client_user_message_id,
                permissions,
            )
            .await
    }
    pub async fn model_list(
        &self,
        cursor: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<ModelListPage> {
        self.inner.model_list(cursor, deadline).await
    }
    pub async fn config_read(
        &self,
        cwd: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<ConfigReadResponse> {
        self.inner.config_read(cwd, deadline).await
    }
    pub async fn account_read(&self, deadline: tokio::time::Instant) -> Result<AccountRead> {
        self.inner.account_read(deadline).await
    }
    pub async fn turn_steer(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<InputItem>,
        client_user_message_id: Option<&str>,
    ) -> Result<TurnSteerResult> {
        self.inner
            .turn_steer(thread_id, expected_turn_id, input, client_user_message_id)
            .await
    }
    pub async fn inject_items(&self, thread_id: &str, items: Vec<Value>) -> Result<()> {
        self.inner.inject_items(thread_id, items).await
    }
    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        self.inner.turn_interrupt(thread_id, turn_id).await
    }
}
