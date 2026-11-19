//! The legacy Claude PTY shares the existing model route with its shell commands.
use super::*;
use calm_truth::db::RepoOutOfDomain as _;

const SERVICE_HTTP: &str = "http://service-model-proxy:8181";
const SERVICE_HTTPS: &str = "http://service-model-proxy:8182";
const CONFIGURED_HTTP: &str = "http://configured-model-proxy:8281";
const CONFIGURED_HTTPS: &str = "http://configured-model-proxy:8282";

fn project_proxies(unset: bool) -> &'static str {
    if unset {
        "unset HTTP_PROXY http_proxy HTTPS_PROXY https_proxy\n"
    } else {
        concat!(
            "export HTTP_PROXY=http://project-proxy:8381\n",
            "export http_proxy=http://project-proxy:8381\n",
            "export HTTPS_PROXY=http://project-proxy:8382\n",
            "export https_proxy=http://project-proxy:8382\n"
        )
    }
}

#[tokio::test]
async fn legacy_pty_preserves_selected_model_proxies_across_restart() {
    let test = stringify!(legacy_pty_preserves_selected_model_proxies_across_restart);
    if isolated_with_environment(
        &format!("proxy::{test}"),
        false,
        &[("HTTP_PROXY", SERVICE_HTTP), ("https_proxy", SERVICE_HTTPS)],
    ) {
        return;
    }
    let live = EnvironmentCard::create(project_proxies(false)).await;
    for (index, (configured, unset)) in [(false, false), (false, true), (true, false), (true, true)]
        .into_iter()
        .enumerate()
    {
        if configured {
            live.boot
                .repo
                .settings_upsert("http_proxy", CONFIGURED_HTTP)
                .await
                .unwrap();
            live.boot
                .repo
                .settings_upsert("https_proxy", CONFIGURED_HTTPS)
                .await
                .unwrap();
        }
        if index != 0 {
            std::fs::write(live.source.join(".envrc"), project_proxies(unset)).unwrap();
            allow(&live.source);
            let (status, response) = post_restart(live.boot.app.clone(), &live.card).await;
            assert_eq!(status, StatusCode::OK, "{response}");
        }
        live.wait_for_starts(index + 1).await;
        let expected = if configured {
            [
                CONFIGURED_HTTP,
                CONFIGURED_HTTP,
                CONFIGURED_HTTPS,
                CONFIGURED_HTTPS,
            ]
        } else {
            [SERVICE_HTTP, "unset", "unset", SERVICE_HTTPS]
        };
        for (key, expected) in ["http-upper", "http-lower", "https-upper", "https-lower"]
            .into_iter()
            .zip(expected)
        {
            assert_eq!(
                live.captured(index, key),
                expected,
                "{key} on start {index}"
            );
        }
    }
    live.assert_project_values_not_persisted(&[SERVICE_HTTP, SERVICE_HTTPS])
        .await;
}

#[tokio::test]
async fn legacy_pty_without_selected_model_proxy_keeps_project_proxies() {
    let test = stringify!(legacy_pty_without_selected_model_proxy_keeps_project_proxies);
    if isolated_with_environment(&format!("proxy::{test}"), false, &[]) {
        return;
    }
    let live = EnvironmentCard::create(project_proxies(false)).await;
    live.wait_for_starts(1).await;
    for (key, expected) in [
        ("http-upper", "http://project-proxy:8381"),
        ("http-lower", "http://project-proxy:8381"),
        ("https-upper", "http://project-proxy:8382"),
        ("https-lower", "http://project-proxy:8382"),
    ] {
        assert_eq!(live.captured(0, key), expected);
    }
}

#[tokio::test]
async fn legacy_pty_preserves_model_proxies_supplied_only_by_supervisor() {
    let test = stringify!(legacy_pty_preserves_model_proxies_supplied_only_by_supervisor);
    if isolated_with_environment(&format!("proxy::{test}"), false, &[]) {
        return;
    }
    // The production legacy socket accepts a supervisor with its own launch environment.
    let supervisor = crate::support::execution::Fixture::start_with_environment(vec![
        ("HTTP_PROXY".into(), SERVICE_HTTP.into()),
        ("https_proxy".into(), SERVICE_HTTPS.into()),
    ]);
    tokio::time::timeout(BUDGET, async {
        loop {
            if calm_session::control::execution::connect_legacy(&supervisor.socket)
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the independently configured supervisor is ready");
    let live =
        EnvironmentCard::create_with_socket(project_proxies(false), &supervisor.socket).await;
    for index in 0..2 {
        if index != 0 {
            std::fs::write(live.source.join(".envrc"), project_proxies(true)).unwrap();
            allow(&live.source);
            let (status, response) = post_restart(live.boot.app.clone(), &live.card).await;
            assert_eq!(status, StatusCode::OK, "{response}");
        }
        live.wait_for_starts(index + 1).await;
        for (key, expected) in [
            ("http-upper", SERVICE_HTTP),
            ("http-lower", "unset"),
            ("https-upper", "unset"),
            ("https-lower", SERVICE_HTTPS),
        ] {
            assert_eq!(
                live.captured(index, key),
                expected,
                "{key} on start {index}"
            );
        }
    }
    live.assert_project_values_not_persisted(&[SERVICE_HTTP, SERVICE_HTTPS])
        .await;
}
