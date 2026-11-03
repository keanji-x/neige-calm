use super::{obs_tagged_original_tests as tagged, *};

#[tokio::test]
async fn u13_p1_released_obs_tagged_sources_and_rejections() {
    tagged::source_matrix(|_, w, target, source, value| {
        let Some(FrameFacts::DerivedByReleasedAdapter { derived, .. }) = &w.frame else {
            panic!("released frame rejected")
        };
        let d = derived.iter().find(|d| d.target_path == target).unwrap();
        assert_eq!((&d.ancestor, &d.value), (source, value));
    })
    .await;
    tagged::rejection_matrix(|w| {
        assert!(w.frame.is_none());
        assert!(super::classification_tests::blocked(
            w,
            "",
            FactReason::Malformed
        ));
    })
    .await;
}
