use super::tests::task;
use super::*;

#[test]
fn task_worker_agent_dispatch_is_explicitly_unavailable_until_wired() {
    let mut row = task("agent", TaskStatus::Pending, &[], 0);
    row.kind = TaskKind::Agent;
    assert!(matches!(
        super::build_worker_payload(&row),
        Err(crate::error::CalmError::Conflict(_))
    ));
}
