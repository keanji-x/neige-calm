//! WA1a contracts through production admission, release, Engine and supervisor.
use super::claude_planner_session_fixture::RegisteredRig;
use calm_server::{
    error::{CalmError, Result},
    provider_execution::{self as owner, Admission},
};
use calm_truth::db::sqlite::{provider_execution as sql, workspace_execution as executions};
use std::sync::atomic::{AtomicUsize, Ordering};
use workspace_runtime::execution::*;

type Tx<'a> = sqlx::Transaction<'a, sqlx::Sqlite>;
fn intent(rig: &RegisteredRig) -> ExecutionIntent {
    let mut intent = test_support::intent(rig.dir.path().join("ws"));
    intent.metadata.consumer = rig.host.provider_execution.owner.consumer().into();
    intent.metadata.alias = ProcessAlias(
        rig.host
            .provider_execution
            .owner
            .alias(&rig.worker_session_id, intent.metadata.id.as_str()),
    );
    intent.metadata.program.program = "/bin/sh".into();
    intent.metadata.program.args = vec![
        "-c".into(),
        "read data; printf '%s' \"$data\" > received; exec sleep 120".into(),
    ];
    intent
}
async fn admit(rig: &RegisteredRig) -> (owner::Claim, owner::DriverGuard) {
    rig.host
        .provider_execution
        .admit_for_test(
            rig.repo.db(),
            rig.host.execution.engine().unwrap(),
            &rig.worker_session_id,
            owner::PreparedIntent(intent(rig)),
            owner::Purpose::RewindCheck,
        )
        .await
        .unwrap()
}
async fn audit(rig: &RegisteredRig) {
    sqlx::query("CREATE TABLE w_authority (value TEXT)")
        .execute(rig.repo.pool())
        .await
        .unwrap();
}
struct Authority {
    calls: AtomicUsize,
    refuse: bool,
    session: String,
}
#[async_trait::async_trait]
impl owner::release::ReleaseAuthority for Authority {
    async fn authorize_tx(&self, tx: &mut Tx<'_>) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            sql::load(&mut **tx, &self.session)
                .await?
                .unwrap()
                .record()
                .release,
            sql::Release::Held
        );
        sqlx::query("INSERT INTO w_authority VALUES ('authorized')")
            .execute(&mut **tx)
            .await?;
        if self.refuse {
            return Err(CalmError::Conflict("caller refused".into()));
        }
        Ok(())
    }
}
#[tokio::test]
async fn w_owner_alias_preserves_planner_bytes_claude() {
    let rig = RegisteredRig::new("exit").await;
    let owner = &rig.host.provider_execution.owner;
    assert_eq!(
        owner.alias("session", "execution"),
        "planner:session:execution"
    );
    let (claim, _guard) = admit(&rig).await;
    let id = &claim.record().execution_id;
    let row = executions::load(rig.repo.db().read_pool(), id)
        .await
        .unwrap()
        .unwrap();
    let metadata: ExecutionMetadata = serde_json::from_str(&row.intent_json).unwrap();
    assert_eq!(
        metadata.alias.0,
        format!("planner:{}:{id}", rig.worker_session_id)
    );
}
#[test]
fn w_owner_alias_prefix_cannot_overlap_planner() {
    for prefix in ["", "p", "plan", "planner", "planner:", "planner:worker"] {
        assert!(owner::AliasPrefix::new(prefix).is_err(), "{prefix}");
    }
    assert_eq!(
        owner::AliasPrefix::new("worker").unwrap().alias("s", "i"),
        "worker:s:i"
    );
}
struct OtherAlias(std::sync::Arc<dyn owner::ProviderExecutionOwner>);
#[async_trait::async_trait]
impl owner::ProviderExecutionOwner for OtherAlias {
    fn consumer(&self) -> &'static str {
        self.0.consumer()
    }
    fn alias_prefix(&self) -> owner::AliasPrefix {
        owner::AliasPrefix::new("test-owner").unwrap()
    }
    fn alias(&self, session: &str, id: &str) -> String {
        self.alias_prefix().alias(session, id)
    }
    async fn complete(
        &self,
        session: &str,
        record: &owner::Record,
        evidence: owner::CompletionEvidence,
        known: Option<owner::KnownEnd>,
    ) -> Result<()> {
        self.0.complete(session, record, evidence, known).await
    }
}
fn other_port(rig: &RegisteredRig) -> owner::ProviderExecution {
    owner::ProviderExecution {
        owner: std::sync::Arc::new(OtherAlias(rig.host.provider_execution.owner.clone())),
        drivers: rig.host.provider_execution.drivers.clone(),
    }
}
#[tokio::test]
async fn w_non_planner_owner_cannot_carry_upgrade_markers() {
    let rig = RegisteredRig::new("exit").await;
    let port = other_port(&rig);
    for key in [
        calm_server::claude_planner::config::MARKER_KEY,
        calm_server::acp_planner::config::MARKER_KEY,
    ] {
        let mut intent = intent(&rig);
        intent.metadata.alias = ProcessAlias(
            port.owner
                .alias(&rig.worker_session_id, intent.metadata.id.as_str()),
        );
        intent
            .metadata
            .program
            .environment
            .push((key.into(), "instance:session".into()));
        assert!(
            port.admit_for_test(
                rig.repo.db(),
                rig.host.execution.engine().unwrap(),
                &rig.worker_session_id,
                owner::PreparedIntent(intent),
                owner::Purpose::RewindCheck
            )
            .await
            .is_err()
        );
        assert!(
            sql::load(rig.repo.pool(), &rig.worker_session_id)
                .await
                .unwrap()
                .is_none()
        );
    }
}
#[tokio::test]
async fn w_stop_session_is_shared() {
    let rig = RegisteredRig::new("exit").await;
    let port = other_port(&rig);
    let mut intent = intent(&rig);
    intent.metadata.alias = ProcessAlias(
        port.owner
            .alias(&rig.worker_session_id, intent.metadata.id.as_str()),
    );
    let (claim, guard) = port
        .admit_for_test(
            rig.repo.db(),
            rig.host.execution.engine().unwrap(),
            &rig.worker_session_id,
            owner::PreparedIntent(intent),
            owner::Purpose::RewindCheck,
        )
        .await
        .unwrap();
    port.sweep(rig.repo.db(), rig.host.execution.engine())
        .await
        .unwrap();
    assert_eq!(
        sql::load(rig.repo.pool(), &rig.worker_session_id)
            .await
            .unwrap()
            .unwrap()
            .record()
            .decision,
        owner::Decision::Observing
    );
    let id = owner::execution_id(claim.record()).unwrap();
    // Keep the live driver registered: shared stop must settle, leaving completion to it.
    port.stop_session(
        rig.repo.db(),
        rig.host.execution.engine(),
        &rig.worker_session_id,
        owner::StopReason::Interrupted,
        std::time::Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert!(
        owner::settlement(rig.repo.db(), &id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        sql::load(rig.repo.pool(), &rig.worker_session_id)
            .await
            .unwrap()
            .unwrap()
            .record()
            .decision,
        owner::Decision::Stopping {
            reason: owner::StopReason::Interrupted,
            ..
        }
    ));
    drop(guard);
    port.stop_session(
        rig.repo.db(),
        rig.host.execution.engine(),
        &rig.worker_session_id,
        owner::StopReason::Interrupted,
        std::time::Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert!(
        sql::load(rig.repo.pool(), &rig.worker_session_id)
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn w_release_runs_caller_authority_in_release_tx() {
    let rig = RegisteredRig::new("exit").await;
    audit(&rig).await;
    let (claim, _guard) = admit(&rig).await;
    // Failure after authorization must roll back its write too, proving shared transaction.
    sqlx::query(concat!(
        "CREATE TRIGGER w_fail_release BEFORE UPDATE OF provider_execution_json ",
        "ON worker_sessions WHEN NEW.provider_execution_json LIKE '%Releasing%' ",
        "BEGIN SELECT RAISE(ABORT, 'test release rollback'); END",
    ))
    .execute(rig.repo.pool())
    .await
    .unwrap();
    let authority = Authority {
        calls: 0.into(),
        refuse: false,
        session: rig.worker_session_id.clone(),
    };
    assert!(
        owner::release::send(
            rig.repo.db(),
            rig.host.execution.engine().unwrap(),
            &rig.worker_session_id,
            &claim,
            b"secret\n",
            &authority
        )
        .await
        .is_err()
    );
    assert_eq!(authority.calls.load(Ordering::SeqCst), 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM w_authority")
        .fetch_one(rig.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "authority write rolls back with release CAS");
    sqlx::query("DROP TRIGGER w_fail_release")
        .execute(rig.repo.pool())
        .await
        .unwrap();
    owner::release::send(
        rig.repo.db(),
        rig.host.execution.engine().unwrap(),
        &rig.worker_session_id,
        &claim,
        b"secret\n",
        &authority,
    )
    .await
    .unwrap();
    assert_eq!(authority.calls.load(Ordering::SeqCst), 2);
    assert!(matches!(
        sql::load(rig.repo.pool(), &rig.worker_session_id)
            .await
            .unwrap()
            .unwrap()
            .record()
            .release,
        sql::Release::Released { .. }
    ));
}
#[tokio::test]
async fn w_release_refused_writes_stop_without_bytes() {
    let rig = RegisteredRig::new("exit").await;
    audit(&rig).await;
    let (claim, _guard) = admit(&rig).await;
    let authority = Authority {
        calls: 0.into(),
        refuse: true,
        session: rig.worker_session_id.clone(),
    };
    assert!(
        owner::release::send(
            rig.repo.db(),
            rig.host.execution.engine().unwrap(),
            &rig.worker_session_id,
            &claim,
            b"secret\n",
            &authority
        )
        .await
        .is_err()
    );
    assert_eq!(authority.calls.load(Ordering::SeqCst), 1);
    let claim = sql::load(rig.repo.pool(), &rig.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.record().release, sql::Release::Held);
    assert!(matches!(
        claim.record().decision,
        owner::Decision::Stopping {
            reason: owner::StopReason::ReleaseRefused,
            ..
        }
    ));
    let row = executions::load(rig.repo.pool(), &claim.record().execution_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.desired, "stop");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM w_authority")
        .fetch_one(rig.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "authority and Stop commit together");
    let id = owner::execution_id(claim.record()).unwrap();
    owner::wait_settled(
        rig.host.execution.engine().unwrap(),
        &id,
        std::time::Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert!(
        !rig.dir.path().join("ws/received").exists(),
        "no payload byte reaches child"
    );
}
struct RefusedAdmission {
    intent: ExecutionIntent,
    built: std::sync::Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Admission for RefusedAdmission {
    async fn authorize_tx(&self, tx: &mut Tx<'_>) -> Result<()> {
        sqlx::query("INSERT INTO w_authority VALUES ('admission')")
            .execute(&mut **tx)
            .await?;
        Err(CalmError::Conflict("admission refused".into()))
    }
    async fn intent_tx(self, _: &mut Tx<'_>) -> Result<ExecutionIntent> {
        self.built.fetch_add(1, Ordering::SeqCst);
        Ok(self.intent)
    }
}
#[tokio::test]
async fn w_admission_refused_leaves_no_claim_or_intent() {
    let rig = RegisteredRig::new("exit").await;
    audit(&rig).await;
    let built = std::sync::Arc::new(AtomicUsize::new(0));
    let result = rig
        .host
        .provider_execution
        .admit_with_track_for_test(
            rig.repo.db(),
            rig.host.execution.engine().unwrap(),
            &rig.worker_session_id,
            RefusedAdmission {
                intent: intent(&rig),
                built: built.clone(),
            },
            "test-track",
            owner::Purpose::RewindCheck,
        )
        .await;
    assert!(result.is_err());
    assert_eq!(
        built.load(Ordering::SeqCst),
        0,
        "no intent or token construction before authority"
    );
    assert!(
        sql::load(rig.repo.pool(), &rig.worker_session_id)
            .await
            .unwrap()
            .is_none()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_executions")
        .fetch_one(rig.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM w_authority")
        .fetch_one(rig.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}
