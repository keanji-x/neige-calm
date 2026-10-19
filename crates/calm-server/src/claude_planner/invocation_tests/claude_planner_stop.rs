#![cfg(test)]
use calm_server::claude_planner::stop::stop;
use std::time::{Duration, Instant};

/// A `/proc` scan that hangs (an `environ` read blocked on a process's mm lock) fails its stop at
/// the deadline and leaks its blocking thread; until it returns, every later stop fails closed at
/// once instead of leaking one more thread per attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_proc_scan_fails_later_stops_at_once_until_it_returns() {
    use calm_server::claude_planner::stop::{
        MarkerInstance, hold_claude_planner_scans_for_test, hung_claude_planner_scans_for_test,
        stop_by,
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let instance =
        MarkerInstance::for_data_dir(dir.path(), calm_server::claude_planner::stop::MARKER_KEY)
            .expect("instance");
    let id = calm_server::model::new_id();
    hold_claude_planner_scans_for_test(true);
    let first = stop_by(
        &instance,
        &id,
        tokio::time::Instant::now() + Duration::from_millis(300),
    )
    .await;
    let hung_after_first = hung_claude_planner_scans_for_test();
    let asked = Instant::now();
    let second = stop_by(
        &instance,
        &id,
        tokio::time::Instant::now() + Duration::from_secs(3),
    )
    .await;
    let second_took = asked.elapsed();
    let hung_after_second = hung_claude_planner_scans_for_test();
    // Released before asserting, so a red run does not leave blocking threads parked.
    hold_claude_planner_scans_for_test(false);

    let first = first.expect_err("the hung scan fails closed");
    assert!(
        first.to_string().contains("did not finish in time"),
        "{first}"
    );
    assert_eq!(hung_after_first, 1);
    let second = second.expect_err("fails closed at once");
    assert!(second_took < Duration::from_secs(1), "{second_took:?}");
    assert!(second.to_string().contains("still hung"), "{second}");
    assert_eq!(hung_after_second, 1, "no second leaked thread");

    for _ in 0..200 {
        if hung_claude_planner_scans_for_test() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(hung_claude_planner_scans_for_test(), 0);
    stop(&instance, &id).await.expect("scans run again");
}
