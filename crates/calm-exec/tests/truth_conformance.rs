//! Scoped T1-T3 truth-layer conformance tests; implementations live in `calm-truth-test-harness`.

#[tokio::test]
async fn t1_decision_write_couples_state_and_event() {
    calm_truth_test_harness::t1_decision_write_couples_state_and_event().await;
}

#[tokio::test]
async fn t1_saga_in_tx_decision_write_couples_state_and_event() {
    calm_truth_test_harness::t1_saga_in_tx_decision_write_couples_state_and_event().await;
}

#[tokio::test]
async fn t1_denied_decision_rolls_back_state_and_event() {
    calm_truth_test_harness::t1_denied_decision_rolls_back_state_and_event().await;
}

#[tokio::test]
async fn t1_gate_can_read_track_root_inside_tx() {
    calm_truth_test_harness::t1_gate_can_read_track_root_inside_tx().await;
}

#[tokio::test]
async fn t2_observation_writes_can_skip_events() {
    calm_truth_test_harness::t2_observation_writes_can_skip_events().await;
}

#[tokio::test]
async fn t3_state_is_not_fold_events() {
    calm_truth_test_harness::t3_state_is_not_fold_events().await;
}

#[tokio::test]
async fn provider_conformance_fake() {
    calm_truth_test_harness::provider_conformance_fake().await;
}
