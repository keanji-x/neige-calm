use super::tests::task;
use super::*;

#[test]
fn task_worker_agent_dispatch_uses_agent_worker() {
    let mut row = task("agent", TaskStatus::Pending, &[], 0);
    row.kind = TaskKind::Agent;
    let (kind, payload) = build_worker_payload(&row).unwrap();
    assert_eq!(kind, "agent-worker");
    assert_eq!(payload["idempotency_key"], row.id);
}
