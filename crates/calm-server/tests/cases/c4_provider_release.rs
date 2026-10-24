//! Release payload regressions share the production Planner fixture.
use super::c4_provider_execution::{claim, script};
use super::claude_planner_session_fixture::RegisteredRig;
use calm_truth::db::sqlite::provider_execution as sql;
use provider::control::EditPreparation;
use std::time::Duration;

#[tokio::test]
async fn c4_release_authorization_precedes_first_byte() {
    let rig = RegisteredRig::new("exit").await;
    script(&rig, "touch ready; exit 0");
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "c4-release-authorized",
        &rig.worker_session_id,
        pause.clone(),
    );
    let task = {
        let session = rig.session().clone();
        let thread = rig.thread.clone();
        tokio::spawn(async move { session.prepare_edit_for_test(&thread).await })
    };
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    assert!(matches!(
        claim(&rig).await.unwrap().record().release,
        sql::Release::Releasing { .. }
    ));
    assert!(!rig.dir.path().join("ws/ready").exists());
    pause.release.notify_one();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        EditPreparation::Prepared { .. }
    ));
}

#[tokio::test]
pub(super) async fn c4_release_continues_partial_payload_writes() {
    let rig = RegisteredRig::new("exit").await;
    script(&rig, "printf '%s' \"$HTTP_PROXY\" > received; exit 0");
    let proxy = format!("http://{}:password@proxy.invalid", "u".repeat(12000));
    let session = rig
        .open_session_with_environment(
            vec![("HTTP_PROXY".into(), "HTTP_PROXY".into(), proxy.clone())],
            rig.host.execution.engine().cloned(),
        )
        .await;
    assert!(matches!(
        session.prepare_edit_for_test(&rig.thread).await.unwrap(),
        EditPreparation::Prepared { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(rig.dir.path().join("ws/received")).unwrap(),
        proxy
    );
}

#[tokio::test]
async fn c4_rewind_proxy_override_order_is_preserved() {
    let rig = RegisteredRig::new("exit").await;
    script(&rig, "printf '%s' \"$HTTP_PROXY\" > received; exit 0");
    for (first, last) in [
        ("http://user:secret@proxy.invalid", "http://public.invalid"),
        ("http://public.invalid", "http://user:secret@proxy.invalid"),
    ] {
        let session = rig
            .open_session_with_environment(
                vec![
                    ("HTTP_PROXY".into(), "http_proxy".into(), first.into()),
                    ("HTTP_PROXY".into(), "http_proxy".into(), last.into()),
                ],
                rig.host.execution.engine().cloned(),
            )
            .await;
        assert!(matches!(
            session.prepare_edit_for_test(&rig.thread).await.unwrap(),
            EditPreparation::Prepared { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(rig.dir.path().join("ws/received")).unwrap(),
            last
        );
    }
}
