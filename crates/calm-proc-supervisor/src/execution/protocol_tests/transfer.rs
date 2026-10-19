use super::*;

#[tokio::test]
async fn private_prepare_rejects_invalid_descriptors_and_transfer_proof() {
    use workspace::execution::objects::ObjectRole;
    let temp = tempfile::tempdir().unwrap();
    let secret = CapabilityToken::fresh().unwrap();
    let executor = executor(&temp, &secret);
    let (request, held) = private(&executor, temp.path());
    for case in 0..10 {
        let mut bad = request.clone();
        let PreparedSource::Private(t) = &mut bad.source else {
            unreachable!()
        };
        let mut fds = descriptors(&held);
        match case {
            0 => fds.clear(),
            1 => fds.extend(descriptors(&held)),
            2 => fds = vec![std::fs::File::open("/dev/null").unwrap().into()],
            3 => t.shape.objects[0].role = ObjectRole::GitDirectory,
            4 => t.executor_boot = ExecutorBootId::fresh().unwrap(),
            5 => bad.identity.source_registration = ObjectRegistrationId::fresh().unwrap(),
            6 => {
                bad.identity.source_incarnation = SourceObjectIncarnation::ExternalDir(
                    ExternalDirectoryIncarnationId::fresh().unwrap(),
                )
            }
            7 => {
                let ExecutionRegistration::ExternalDir { root, .. } = &mut t.registration else {
                    unreachable!()
                };
                root.inode += 1;
            }
            8 => bad.identity.authority.authorization_version += 1,
            9 => bad.identity.executor_boot = ExecutorBootId::fresh().unwrap(),
            _ => unreachable!(),
        }
        assert!(
            executor.prepare_objects(bad, fds).await.is_err(),
            "case {case}"
        );
        assert_released(&executor);
        assert!(matches!(
            inspect(&executor, &secret, &request).await,
            ExecutionReply::Absent
        ));
    }
    prepare(&executor, &request, &held).await;
    let (mut peer, peer_held) = private(&executor, temp.path());
    let PreparedSource::Private(t) = &mut peer.source else {
        unreachable!()
    };
    let PreparedSource::Private(original) = &request.source else {
        unreachable!()
    };
    t.nonce = original.nonce.clone();
    assert!(
        executor
            .prepare_objects(peer, descriptors(&peer_held))
            .await
            .is_err()
    );
    let mut changed = request.clone();
    let PreparedSource::Private(t) = &mut changed.source else {
        unreachable!()
    };
    t.nonce = TransferNonce::fresh().unwrap();
    assert!(
        executor
            .prepare_objects(changed, descriptors(&held))
            .await
            .is_err()
    );
}
