//! Recoverable operation blockers shared by startup and checkout admission.
//! A blocker is diagnostic state, never sufficient evidence to release a checkout:
//! the reader must also verify the operation's task binding and pre-effect state.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "version", rename_all = "snake_case", deny_unknown_fields)]
pub enum PendingBlocker {
    V1 {
        class: PendingBlockerClass,
        #[serde(default, skip_serializing_if = "PendingRetryOwner::is_driver")]
        retry_owner: PendingRetryOwner,
        #[serde(deserialize_with = "nonempty")]
        owner_scope: String,
        #[serde(deserialize_with = "nonempty")]
        resource: String,
        #[serde(deserialize_with = "nonempty")]
        retry_when: String,
    },
}

/// Who may claim the Pending operation after a blocked drive returns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingRetryOwner {
    /// Also the interpretation of blockers written before retry ownership was recorded.
    #[default]
    Driver,
    Scheduler,
}

impl PendingRetryOwner {
    fn is_driver(&self) -> bool {
        *self == Self::Driver
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingBlockerClass {
    DriverBusy,
    LaunchFence,
    LocationOwner,
    WorkspaceRemoval,
    Checkout,
}

fn nonempty<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.trim().is_empty() {
        return Err(serde::de::Error::custom("blocker fact must not be empty"));
    }
    Ok(value)
}

impl PendingBlocker {
    pub fn new(
        class: PendingBlockerClass,
        owner_scope: impl Into<String>,
        resource: impl Into<String>,
        retry_when: impl Into<String>,
    ) -> Self {
        Self::V1 {
            class,
            retry_owner: PendingRetryOwner::Driver,
            owner_scope: owner_scope.into(),
            resource: resource.into(),
            retry_when: retry_when.into(),
        }
    }

    pub fn with_retry_owner(mut self, owner: PendingRetryOwner) -> Self {
        let Self::V1 { retry_owner, .. } = &mut self;
        *retry_owner = owner;
        self
    }

    pub fn diagnostic(&self, operation_id: &str) -> String {
        format!("operation {operation_id} is blocked: {self}")
    }
}

impl std::fmt::Display for PendingBlocker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::V1 {
            class,
            owner_scope,
            resource,
            retry_when,
            ..
        } = self;
        write!(
            formatter,
            "{class:?} at {owner_scope} resource {resource}; resumes when {retry_when}"
        )
    }
}
