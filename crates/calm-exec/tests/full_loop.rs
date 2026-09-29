//! Fake contract tests; implementations live in `calm-truth-test-harness`.

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
