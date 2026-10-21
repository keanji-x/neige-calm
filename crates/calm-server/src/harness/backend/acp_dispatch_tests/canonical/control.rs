//! Canonical control consumes the original driver and the actual caller's writer gate.
#![cfg(test)]
use super::*;

#[tokio::test]
async fn canonical_acp_stop_rechecks_source_at_writer_and_signals_one_exact_driver() {
    let fx = Fixture::new("hold").await;
    fx.dispatch().await.unwrap();
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let target = crate::harness::input_dispatch::execution_target_tx(
        &mut tx,
        &fx.owner.card,
        &fx.owner.worker,
        &AgentProvider::OpenCode,
    )
    .await
    .unwrap()
    .unwrap();
    tx.commit().await.unwrap();
    let admission = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || Box::pin(async { Ok(()) }),
    );
    let mut wrong = target.clone();
    wrong.logical_turn_id = LogicalTurnId::parse(&crate::model::new_id()).unwrap();
    assert!(
        fx.backend
            .interrupt_execution(&wrong, &admission)
            .await
            .is_err()
    );
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let refused = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || {
            Box::pin(async {
                if checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    Ok(())
                } else {
                    Err(CalmError::Forbidden(
                        "source changed behind cancel writer".into(),
                    ))
                }
            })
        },
    );
    assert!(
        fx.backend
            .interrupt_execution(&target, &refused)
            .await
            .unwrap_err()
            .to_string()
            .contains("source changed")
    );
    assert_eq!(checks.load(std::sync::atomic::Ordering::SeqCst), 2);
    let cancels = || {
        std::fs::read_to_string(fx.dir.path().join("requests.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["method"]
                    == "session/cancel"
            })
            .count()
    };
    assert_eq!(cancels(), 0);
    assert!(fx.complete().await.is_none());
    fx.backend
        .interrupt_execution(&target, &admission)
        .await
        .unwrap();
    fx.wait_idle().await;
    assert_eq!(cancels(), 1);
    fx.wait_end().await;
    fx.session.shutdown().await.unwrap();
}
