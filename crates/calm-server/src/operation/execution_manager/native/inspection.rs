use super::wire::*;
use crate::error::Result;
use std::{path::Path, time::Duration};

pub struct CodexAppServer {
    inner: super::wire::CodexAppServer,
}
impl CodexAppServer {
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
    pub async fn thread_read_full(&self, thread_id: &str) -> Result<ThreadResult> {
        self.inner.thread_read_full(thread_id).await
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
    pub async fn background_terminals_stopped(&self, thread: &str) -> Result<bool> {
        self.inner.background_terminals_stopped(thread).await
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
}
