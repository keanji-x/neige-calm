use super::*;

#[test]
fn execution_directory_helper_report_requires_exact_boot_nonce_receipts_and_roles() {
    let temp = tempfile::tempdir().unwrap();
    let first = File::open(temp.path()).unwrap();
    let second = first.try_clone().unwrap();
    let image = std::fs::metadata("/bin/true").unwrap();
    let context = ProbeContext {
        executor_boot: ExecutorBootId::fresh().unwrap(),
        nonce: TransferNonce::fresh().unwrap(),
        ancestor: ProbeRoot {
            origin: ProbeOrigin::Registered {
                registration: ObjectRegistrationId::fresh().unwrap(),
                incarnation: SourceObjectIncarnation::ExternalDir(
                    ExternalDirectoryIncarnationId::fresh().unwrap(),
                ),
            },
            role: ObjectRole::Root,
        },
        subject: ProbeRoot {
            origin: ProbeOrigin::Registered {
                registration: ObjectRegistrationId::fresh().unwrap(),
                incarnation: SourceObjectIncarnation::ExternalDir(
                    ExternalDirectoryIncarnationId::fresh().unwrap(),
                ),
            },
            role: ObjectRole::Root,
        },
        facts: current_directory_pair(&first, &second).ok().unwrap(),
        mechanism: ScopeMechanismIdentity {
            executable_device: image.dev(),
            executable_inode: image.ino(),
            protocol_version: 1,
        },
    };
    // Only the existing collector boundary is faulted here; integration cases run the actual helper.
    for fault in 0..6 {
        let (mut read, mut write) = process::pipe().unwrap();
        process::nonblocking(read.as_raw_fd()).unwrap();
        let child = std::process::Command::new("/bin/true")
            .env_clear()
            .spawn()
            .unwrap();
        let mut child = OwnedChild::new(child);
        let mut bad = context.clone();
        match fault {
            0 => bad.nonce = TransferNonce::fresh().unwrap(),
            1 => bad.executor_boot = ExecutorBootId::fresh().unwrap(),
            2 => {
                if let ProbeOrigin::Registered { registration, .. } = &mut bad.subject.origin {
                    *registration = ObjectRegistrationId::fresh().unwrap();
                }
            }
            3 => {
                if let ProbeOrigin::Registered { incarnation, .. } = &mut bad.subject.origin {
                    *incarnation = SourceObjectIncarnation::ExternalDir(
                        ExternalDirectoryIncarnationId::fresh().unwrap(),
                    );
                }
            }
            4 => bad.subject.role = ObjectRole::GitDirectory,
            5 => {}
            _ => unreachable!(),
        }
        let mut bytes = serde_json::to_vec(&ProbeReport {
            context: bad,
            outcome: DirectoryRelationOutcome::Disjoint,
        })
        .unwrap();
        if fault == 5 {
            bytes.pop();
        }
        write.write_all(&bytes).unwrap();
        drop(write);
        assert_eq!(
            collect(&mut child, &mut read, &context).unwrap(),
            DirectoryRelationOutcome::Unknown(DirectoryRelationUnknown::HelperProtocol)
        );
        assert!(
            child.is_reaped(),
            "report rejection skipped concrete child reap"
        );
    }
}
