//! Noninteractive Task permissions. One lock orders the grant write and cancellation fence.
use crate::model::TaskAccess;
use provider::{
    acp::{
        Client, Error,
        permission::{self, PermissionHandler, PermissionRequest},
    },
    send_boundary::PreSendGate,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

pub(crate) struct WorkerPermissions {
    client: Client,
    access: TaskAccess,
    cwd: PathBuf,
    fenced: Mutex<bool>,
    admission: super::admission::Admission,
    declined: std::sync::Mutex<Option<provider::acp::approvals::DeclinedPermission>>,
}
impl WorkerPermissions {
    pub(crate) fn new(
        client: Client,
        access: TaskAccess,
        cwd: PathBuf,
        admission: super::admission::Admission,
    ) -> Self {
        Self {
            client,
            access,
            cwd,
            fenced: Mutex::new(false),
            admission,
            declined: Default::default(),
        }
    }
    pub(crate) fn selection<'a>(&self, request: &'a PermissionRequest) -> Option<&'a str> {
        if self.access != TaskAccess::ReadWrite
            || !matches!(request.tool_call.kind.as_deref(), Some("read" | "edit"))
        {
            return None;
        }
        let locations = request
            .tool_call
            .locations
            .as_ref()
            .filter(|locations| !locations.is_empty())?;
        let root = workspace::worktree::physical_path(&self.cwd).ok()?;
        for location in locations {
            if location.path.trim().is_empty() {
                return None;
            }
            let path = Path::new(&location.path);
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.cwd.join(path)
            };
            let physical = workspace::worktree::physical_path(&absolute).ok()?;
            if !physical.starts_with(&root) {
                return None;
            }
        }
        request.allow_once()
    }
}
#[async_trait::async_trait]
impl PermissionHandler for WorkerPermissions {
    async fn request(&self, id: Value, method: &str, params: Value) -> Result<(), Error> {
        if method != permission::METHOD {
            return provider::acp::approvals::refuse(&self.client, id, method).await;
        }
        let tool_call_id = params["toolCall"]["toolCallId"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let fenced = self.fenced.lock().await;
        let selected = if !*fenced && self.admission.check(false).await.is_ok() {
            PermissionRequest::decode(params)
                .ok()
                .and_then(|request| self.selection(&request).map(str::to_owned))
        } else {
            None
        };
        if let Some(option) = selected {
            match self
                .client
                .respond_gated(
                    id.clone(),
                    permission::selected(&option),
                    &self.admission.gate(false),
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(provider::acp::WriteFailure::BeforeWrite(_)) => {}
                Err(error) => return Err(error.into_error()),
            }
        }
        *self.declined.lock().expect("Worker permission diagnostic") = Some(provider::acp::approvals::DeclinedPermission {
            tool_call_id,
            reason: "The Worker declined a native tool permission request under its task access policy.".into(),
            diagnostic: provider::acp::diagnostics::AcpDiagnostic::Rejected,
        });
        self.client.respond(id, permission::cancelled()).await
    }
    fn declined(&self) -> Option<String> {
        self.declined_permission().map(|declined| declined.reason)
    }
    fn declined_permission(&self) -> Option<provider::acp::approvals::DeclinedPermission> {
        self.declined
            .lock()
            .expect("Worker permission diagnostic")
            .clone()
    }
    async fn cancel(&self, native: &str, gate: &PreSendGate<'_>) -> Result<(), Error> {
        let mut fenced = self.fenced.lock().await;
        *fenced = true;
        provider::acp::protocol::cancel(&self.client, native, gate)
            .await
            .map_err(|error| error.into_error())
    }
    async fn close(&self) {
        *self.fenced.lock().await = true;
    }
}
