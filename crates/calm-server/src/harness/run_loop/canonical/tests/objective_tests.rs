use super::*;

#[tokio::test]
async fn objective_canonical_usage_endpoints_and_maintenance() {
    let fixture = Fixture::new().await;
    crate::harness::run_loop::objective_usage::exercise_objective_usage(&fixture.harness.inner)
        .await;
    fixture.harness.shutdown().await.unwrap();
}
