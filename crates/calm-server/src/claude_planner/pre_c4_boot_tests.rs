use std::{process::Command, time::Duration};

#[tokio::test]
async fn c4_pre_c4_boot_sweep_still_runs() {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let claude = super::config::ClaudePlannerHost::unconfigured_scratch().unwrap();
    let acp = crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap();
    let mut children = Vec::new();
    for (instance, key, ids) in [
        (&claude.instance, "NEIGE_CLAUDE_PLANNER", &["readiness"][..]),
        (
            &acp.instance,
            "NEIGE_ACP_PLANNER",
            &["readiness", "catalog"][..],
        ),
    ] {
        for id in ids {
            children.push(
                Command::new("/bin/sleep")
                    .arg("300")
                    .env_clear()
                    .env(key, instance.marker(id))
                    .spawn()
                    .unwrap(),
            );
        }
    }
    super::lifecycle::boot(&repo, &claude).await.unwrap();
    crate::acp_planner::config::boot(&repo, &acp).await.unwrap();
    let stopped = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if children
                .iter_mut()
                .all(|child| child.try_wait().unwrap().is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    for child in &mut children {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(
        stopped.is_ok(),
        "both boot sweeps must cover pre-C4 readiness probes"
    );
}
