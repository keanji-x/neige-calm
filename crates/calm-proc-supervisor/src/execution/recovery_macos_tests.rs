use super::*;
use std::os::unix::process::CommandExt;

fn unknown(temp: &tempfile::TempDir) -> (Arc<Executor>, ExecutionId) {
    let executor = Executor::open(ExecutorConfig {
        journal_directory: temp.path().join("journal"),
        helper_program: "/bin/sh".into(),
        kernel_client_secret: CapabilityToken::fresh().unwrap(),
    })
    .unwrap();
    // Finite native child, reaped before any fallible assertion; never signal stored metadata.
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("0.1")
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
    let count = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            1,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            std::mem::size_of_val(&info) as i32,
        )
    };
    let waited = child.wait();
    assert_eq!(count, std::mem::size_of_val(&info) as i32);
    assert!(waited.unwrap().success());
    let mut record: Journal =
        serde_json::from_str(include_str!("fixtures/linux-v4.execution.json")).unwrap();
    let mut receipt = record.phase.receipt().unwrap().clone();
    receipt.process = NativeProcessIdentity::MacOwnedGroup(MacProcessIdentity {
        pid,
        birth_seconds: info.pbi_start_tvsec,
        birth_microseconds: info.pbi_start_tvusec,
        nonce: CapabilityToken::fresh().unwrap(),
    });
    receipt.scope_mechanism = executor.mechanism.clone();
    record.host_boot_id = executor.host_boot_id.clone();
    record.version = journal::JOURNAL_VERSION;
    record.output_end = OutputEnd::Incomplete {
        reason: "executor_restarted".into(),
    };
    record.phase = Phase::Unknown {
        slot_version: receipt.slot_version,
        receipt: Some(receipt),
        reason: "executor_restarted".into(),
    };
    let id = record.request.identity.execution_id.clone();
    executor.persist(record).unwrap();
    (executor, id)
}
#[tokio::test]
async fn recovery_3450_persistence_failure_never_publishes_handoff() {
    for failure in [1, 2] {
        let temp = tempfile::tempdir().unwrap();
        let (executor, id) = unknown(&temp);
        executor
            .store
            .fail_execution_write
            .store(failure, std::sync::atomic::Ordering::SeqCst);
        assert!(executor.reconcile_interrupted(&id).await.is_err());
        assert!(matches!(
            executor.record(&id).unwrap().phase,
            Phase::Unknown { .. }
        ));
        executor.reconcile_interrupted(&id).await.unwrap();
        assert!(executor.record(&id).unwrap().durably_settled());
        assert!(matches!(
            executor.store.load().unwrap()[0].phase,
            Phase::InterruptedQuiescent { .. }
        ));
    }
}
#[tokio::test]
async fn recovery_3450_receipt_boot_slot_mechanism_and_identity_are_required() {
    let temp = tempfile::tempdir().unwrap();
    let (executor, id) = unknown(&temp);
    let original = executor.record(&id).unwrap();
    for mismatch in 0..8 {
        let mut record = original.clone();
        let Phase::Unknown {
            receipt,
            slot_version,
            ..
        } = &mut record.phase
        else {
            unreachable!()
        };
        match mismatch {
            0 => *receipt = None,
            1 => *slot_version += 1,
            2 => record.host_boot_id.clear(),
            3 => record.host_boot_id = "another-boot".into(),
            4 => {
                receipt.as_mut().unwrap().scope_mechanism = ScopeMechanismIdentity::MacOwnedGroup {
                    protocol_version: 2,
                }
            }
            5 => receipt.as_mut().unwrap().identity.execution_id = ExecutionId::fresh().unwrap(),
            6 => {
                if let NativeProcessIdentity::MacOwnedGroup(p) =
                    &mut receipt.as_mut().unwrap().process
                {
                    p.pid = 0;
                }
            }
            _ => {
                if let NativeProcessIdentity::MacOwnedGroup(p) =
                    &mut receipt.as_mut().unwrap().process
                {
                    p.birth_seconds = 0;
                }
            }
        }
        executor.persist(record).unwrap();
        let _ = executor.reconcile_interrupted(&id).await;
        assert!(
            matches!(executor.record(&id).unwrap().phase, Phase::Unknown { .. }),
            "mismatch {mismatch}"
        );
    }
    executor.persist(original).unwrap();
    executor.reconcile_interrupted(&id).await.unwrap();
    assert!(executor.record(&id).unwrap().durably_settled());
}
