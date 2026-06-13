//! #679 PR5 full-loop and fake contract tests.
//!
//! Implementations live in `calm-truth-test-harness` so `calm-exec/src`
//! stays implementation-free.

#[tokio::test]
async fn full_loop_dispatch_to_lifecycle_done() {
    calm_truth_test_harness::full_loop_dispatch_to_lifecycle_done().await;
}

#[tokio::test]
async fn full_loop_cross_principal_denied() {
    calm_truth_test_harness::full_loop_cross_principal_denied().await;
}

#[tokio::test]
async fn fake_provider_contract() {
    calm_truth_test_harness::fake_provider_contract().await;
}

#[tokio::test]
async fn fake_root_contract() {
    calm_truth_test_harness::fake_root_contract().await;
}

#[tokio::test]
async fn fake_observation_sink_contract() {
    calm_truth_test_harness::fake_observation_sink_contract().await;
}
