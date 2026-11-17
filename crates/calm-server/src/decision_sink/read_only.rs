//! A read-only completion samples the original lease checkout outside the writer, then rechecks its SQL witness.
use crate::{
    db::{RouteRepo, write_in_tx_typed},
    error::{CalmError, Result},
    model::{Task, TaskAccess, TaskStatus},
    operation::{
        Tx,
        task_verify_adapter::target::{self, Expected},
        workspace_lease::LeaseBase,
    },
};
use sqlx::Row;
use std::{path::PathBuf, sync::Arc};
#[derive(Clone)]
pub(super) struct Checked {
    task: Task,
    session: String,
    lease: String,
    operation: String,
    output: String,
    identity: crate::mcp_server::registry::ToolCallIdentity,
}
impl Checked {
    pub(super) async fn recheck(&self, tx: &mut Tx<'_>) -> Result<()> {
        super::worker_report::admit_worker_identity_tx(
            tx,
            &self.identity,
            &self.task.track_id.clone().into(),
        )
        .await?;
        if crate::db::sqlite::task_get_tx(tx, &self.task.id)
            .await?
            .as_ref()
            != Some(&self.task)
        {
            return Err(CalmError::Conflict(
                "read-only completion task changed during sampling".into(),
            ));
        }
        let same:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspace_leases l JOIN operations o ON o.id=l.lease_owner JOIN cards c ON c.id=l.card_id WHERE l.lease_id=?1 AND l.state='held' AND o.id=?2 AND o.tx_output_json=?3 AND c.session_id=?4)").bind(&self.lease).bind(&self.operation).bind(&self.output).bind(&self.session).fetch_one(&mut **tx).await?;
        if !same {
            return Err(CalmError::Conflict(
                "read-only completion lease changed during sampling".into(),
            ));
        }
        Ok(())
    }
}
pub(super) async fn check(
    repo: Arc<dyn RouteRepo>,
    identity: &crate::mcp_server::registry::ToolCallIdentity,
    attempt: &str,
) -> Result<Option<Checked>> {
    let identity = identity.clone();
    let session = identity.session_id.clone();
    let attempt = attempt.to_owned();
    let facts=write_in_tx_typed(repo.as_ref(),move|tx|Box::pin(async move {
        let Some(task)=crate::db::sqlite::task_get_tx(tx,&attempt).await? else{return Ok(None)};
        super::worker_report::admit_worker_identity_tx(tx,&identity,&task.track_id.clone().into()).await?;
        let owns=crate::db::sqlite::worker_op_targets_card_tx(tx,&attempt,&identity.card_id).await?;
        super::worker_report::admit_worker_report_tx(tx,&attempt,&task.track_id,crate::db::sqlite::TaskReporter::Card{card_id:&identity.card_id,owns_key:owns},true).await?;
        if task.access!=TaskAccess::ReadOnly || task.agent_provider().map_err(CalmError::BadRequest)?.is_none() || !matches!(task.status,TaskStatus::Dispatched|TaskStatus::Running){return Ok(None)}
        let row=sqlx::query("SELECT l.lease_id,l.path,l.base_sha,l.base_source,l.base_attempt_id,l.canonical_path,l.git_common_dir,o.id AS operation_id,o.tx_output_json FROM workspace_leases l JOIN operations o ON o.id=l.lease_owner JOIN cards c ON c.id=l.card_id WHERE c.session_id=?1 AND o.idempotency_key=?2 AND l.state='held' ORDER BY l.created_at_ms DESC LIMIT 1").bind(&session).bind(&attempt).fetch_optional(&mut **tx).await?;
        let Some(row)=row else{return Err(CalmError::Conflict("worker-read-only-unsampled: original held lease is unavailable".into()))};
        let base=LeaseBase::from_row(&row)?;
        let lease:String=row.try_get("lease_id")?;
        let cwd:String=row.try_get("path")?;
        let operation:String=row.try_get("operation_id")?;
        let output:String=row.try_get("tx_output_json")?;
        let value:serde_json::Value=serde_json::from_str(&output)?;let data=&value["data"];
        // Pre-upgrade readers recorded neither a lease base nor an output common dir.
        // New readers still have no LeaseBase, but freeze git_common_dir in the output.
        if base.is_none() && (data["git_common_dir"].is_null() || data["git_common_dir"].as_str() == Some("")) {
            return Ok(None);
        }
        let field=|name:&str|data[name].as_str().filter(|value|!value.is_empty()).map(str::to_owned).ok_or_else(||CalmError::Conflict(format!("worker-read-only-unsampled: original {name} is unavailable")));
        let expected=Expected{canonical_path:field("canonical_path")?,git_common_dir:field("git_common_dir")?,commit_sha:field("base_sha")?};
        Ok(Some((Checked{task,session,lease,operation,output,identity},PathBuf::from(cwd),expected)))
    })).await?;
    let Some((checked, cwd, expected)) = facts else {
        return Ok(None);
    };
    let problem = match target::sample(&cwd, &expected).await {
        Ok(sampled) if target::reasons(&sampled, &expected).is_empty() => None,
        Ok(_) => Some((
            "worker-read-only-changed",
            "read-only Worker changed HEAD, checkout identity, or tracked/untracked contents",
        )),
        Err(_) => Some((
            "worker-read-only-unsampled",
            "read-only Worker checkout could not be sampled",
        )),
    };
    if let Some((class, reason)) = problem {
        let witness = checked.clone();
        write_in_tx_typed(repo.as_ref(),move|tx|Box::pin(async move{
            witness.recheck(tx).await?;
            sqlx::query("UPDATE tasks SET status_detail=?2 WHERE id=?1 AND status IN ('dispatched','running')").bind(&witness.task.id).bind(crate::db::sqlite::status_detail_with_reason(class,reason)).execute(&mut **tx).await?;Ok(())
        })).await?;
        return Err(CalmError::Conflict(format!("{class}: {reason}")));
    }
    Ok(Some(checked))
}
