//! #2530 E3c A11: after every refresh the kernel entry's per-tool map is exactly the delegated tools
//! of the installed and enabled plugins, and every path that revokes a grant fails closed.

use super::*;

fn names(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// #2530 A11: after every refresh the kernel entry's grants are exactly the delegated tools of the
/// installed and enabled plugins, and nothing else in the entry changes.
#[tokio::test]
async fn the_grants_follow_enable_disable_and_uninstall_exactly() {
    let fx = fixture_with(&[
        ("probe", &[("web.fetch", true), ("write", false)]),
        ("other", &[("input", true)]),
        ("plain", &[("ls", false)]),
    ])
    .await;
    let rest = fx.entry_rest();
    fx.start();

    fx.host.enable("probe").await.unwrap();
    fx.host.enable("plain").await.unwrap();
    fx.forward();
    fx.wait_reloads(1).await;
    assert_eq!(
        fx.granted(),
        names(&["plugin_probe_web_fetch"]),
        "only the enabled plugin's delegated tool; `other` is installed but disabled"
    );
    assert_eq!(fx.entry_rest(), rest);

    fx.host.enable("other").await.unwrap();
    fx.forward();
    fx.wait_reloads(2).await;
    assert_eq!(
        fx.granted(),
        names(&["plugin_other_input", "plugin_probe_web_fetch"])
    );

    fx.host.disable("probe").await.unwrap();
    fx.forward();
    fx.wait_reloads(3).await;
    assert_eq!(fx.granted(), names(&["plugin_other_input"]));

    fx.host.uninstall("other").await.unwrap();
    fx.forward();
    fx.wait_reloads(4).await;
    assert_eq!(fx.granted(), BTreeSet::new());
    assert_eq!(fx.entry_rest(), rest);
    fx.home
        .verify_expected_mcp_servers(EXPECTED_MCP_SERVERS)
        .expect("the grants live inside the kernel entry");
}

/// Enabled, not running: a stopped plugin keeps its grant (routing refuses its calls), and
/// disabling it then removes the grant although no process stopped.
#[tokio::test]
async fn disabling_a_plugin_that_is_not_running_removes_its_grant() {
    let fx = fixture_with(&[("probe", &[("web.fetch", true)])]).await;
    fx.start();
    fx.host.enable("probe").await.unwrap();
    fx.host.stop("probe").await.unwrap();
    fx.forward();
    fx.wait_reloads(1).await;
    assert_eq!(fx.granted(), names(&["plugin_probe_web_fetch"]));

    fx.host.disable("probe").await.unwrap();
    fx.forward();
    fx.wait_reloads(2).await;
    assert_eq!(fx.granted(), BTreeSet::new());
}

/// A daemon Running refresh writes the same grants, so a calm-server restart re-derives them.
#[tokio::test]
async fn a_daemon_running_refresh_writes_the_grants_and_repeats_them_unchanged() {
    let fx = fixture_with(&[("probe", &[("web.fetch", true), ("write", false)])]).await;
    fx.host.enable("probe").await.unwrap();
    fx.start();
    fx.appserver.publish_readiness_for_test(1, true);
    fx.wait_reloads(1).await;
    assert_eq!(fx.granted(), names(&["plugin_probe_web_fetch"]));
    let first = fx.entry_rest();

    fx.appserver.publish_readiness_for_test(1, false);
    fx.appserver.publish_readiness_for_test(2, true);
    fx.wait_reloads(2).await;
    assert_eq!(fx.granted(), names(&["plugin_probe_web_fetch"]));
    assert_eq!(fx.entry_rest(), first);
    assert_eq!(fx.toolset().as_deref(), Some("2"));
}

/// An enable whose spawn is refused after it turned the bit on rolls the bit back. A refresh that
/// ran in between granted the tools, so the rollback must be announced and revoke them.
#[tokio::test]
async fn an_enable_refused_after_the_flip_revokes_the_grant_it_briefly_had() {
    let fx = fixture_requiring(&[("late", &[("web.fetch", true)])], &["late"]).await;
    fx.start();
    fx.seams.pause_after_enable.store(true, Ordering::SeqCst);
    let host = fx.host.clone();
    let enabling = tokio::spawn(async move { host.enable("late").await });
    fx.seams.flipped.notified().await;

    // A refresh lands while the bit is on.
    fx.appserver.publish_readiness_for_test(1, true);
    fx.wait_reloads(1).await;
    assert_eq!(fx.granted(), names(&["plugin_late_web_fetch"]));

    fx.seams.release.notify_one();
    let refused = enabling.await.unwrap().expect_err("this kernel is too old");
    assert_eq!(
        refused.status(),
        axum::http::StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(
        !fx.repo
            .plugin_get_by_id("late")
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
    fx.forward();
    fx.wait_reloads(2).await;
    assert_eq!(fx.granted(), BTreeSet::new());
}

/// A refresh that cannot read the plugin rows revokes every grant rather than keep one it can no
/// longer vouch for; the next readable refresh restores them.
#[tokio::test]
async fn a_failed_plugin_row_read_revokes_every_grant() {
    let fx = fixture_with(&[("probe", &[("web.fetch", true)])]).await;
    fx.start();
    fx.host.enable("probe").await.unwrap();
    fx.forward();
    fx.wait_reloads(1).await;
    assert_eq!(fx.granted(), names(&["plugin_probe_web_fetch"]));
    let rest = fx.entry_rest();

    fx.seams.list_fails.store(true, Ordering::SeqCst);
    fx.appserver.publish_readiness_for_test(1, true);
    fx.wait_reloads(2).await;
    assert_eq!(fx.granted(), BTreeSet::new());
    assert_eq!(fx.toolset().as_deref(), Some("2"));
    assert_eq!(fx.entry_rest(), rest);

    fx.seams.list_fails.store(false, Ordering::SeqCst);
    fx.appserver.publish_readiness_for_test(1, false);
    fx.appserver.publish_readiness_for_test(2, true);
    fx.wait_reloads(3).await;
    assert_eq!(fx.granted(), names(&["plugin_probe_web_fetch"]));
}

/// The manifest is the source: an enabled plugin reloaded without the key loses its grant at the
/// reload's refresh, and regains it when the key comes back.
#[tokio::test]
async fn a_reload_that_drops_the_delegation_revokes_it_and_re_adding_restores_it() {
    let fx = fixture_with(&[("probe", &[("web.fetch", true), ("write", false)])]).await;
    let plugins_dir = fx._tmp.path().join("plugins");
    fx.start();
    fx.host.enable("probe").await.unwrap();
    fx.forward();
    fx.wait_reloads(1).await;
    assert_eq!(fx.granted(), names(&["plugin_probe_web_fetch"]));

    write_app(
        &plugins_dir,
        "probe",
        &[("web.fetch", false), ("write", false)],
    );
    fx.host.reload("probe").await.unwrap();
    fx.forward();
    fx.wait_reloads(2).await;
    assert_eq!(fx.granted(), BTreeSet::new());

    write_app(
        &plugins_dir,
        "probe",
        &[("web.fetch", true), ("write", false)],
    );
    fx.host.reload("probe").await.unwrap();
    fx.forward();
    fx.wait_reloads(3).await;
    assert_eq!(fx.granted(), names(&["plugin_probe_web_fetch"]));
}
