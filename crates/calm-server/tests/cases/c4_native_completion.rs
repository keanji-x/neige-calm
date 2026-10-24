//! Exercise the real Claude owner with Mac evidence even on a Linux test host.
use super::{
    c4_provider_turn::turn_compaction_turn, claude_planner_session_fixture::RegisteredRig,
};
use calm_server::provider_execution::{
    CompletionEvidence, KnownEnd, ProviderExecutionOwner, Record,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use workspace_runtime::execution::{ExecutionState, Settlement};

struct NativeEvidence {
    inner: Arc<dyn ProviderExecutionOwner>,
    forwarded: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl ProviderExecutionOwner for NativeEvidence {
    fn consumer(&self) -> &'static str {
        self.inner.consumer()
    }
    fn alias_prefix(&self) -> calm_server::provider_execution::AliasPrefix {
        self.inner.alias_prefix()
    }
    fn alias(&self, session: &str, id: &str) -> String {
        self.inner.alias(session, id)
    }
    async fn complete(
        &self,
        session: &str,
        record: &Record,
        evidence: CompletionEvidence,
        known: Option<KnownEnd>,
    ) -> calm_server::error::Result<()> {
        // Substitute only the platform completion fact. The real driver builds KnownEnd
        // and the real owner decides the outcome, confirmation and compaction qualification.
        let evidence = match evidence {
            CompletionEvidence::Settled(Settlement::Journal(
                ExecutionState::Drained { status } | ExecutionState::OriginalGroupEmpty { status },
            )) => {
                self.forwarded.fetch_add(1, Ordering::SeqCst);
                CompletionEvidence::Settled(Settlement::Journal(
                    ExecutionState::OriginalGroupEmpty { status },
                ))
            }
            other => other,
        };
        self.inner.complete(session, record, evidence, known).await
    }
}

#[tokio::test]
async fn c4_native_group_settlement_preserves_turn_compaction_completion() {
    let forwarded = Arc::new(AtomicUsize::new(0));
    let count = forwarded.clone();
    let rig = RegisteredRig::with_completion_owner("exit", move |inner| {
        Arc::new(NativeEvidence {
            inner,
            forwarded: count,
        })
    })
    .await;
    turn_compaction_turn(&rig).await;
    assert_eq!(forwarded.load(Ordering::SeqCst), 3);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_macos_claude_turn_compaction_turn_completes() {
    // No evidence substitution: exercise the native supervisor on an actual Mac.
    let rig = RegisteredRig::new("exit").await;
    turn_compaction_turn(&rig).await;
}
