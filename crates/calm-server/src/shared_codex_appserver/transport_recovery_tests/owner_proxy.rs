//! Owner proxy settings overlay the admitted project environment, never its evaluation.
use super::*;
use crate::db::RepoOutOfDomain as _;

const OWNER_HTTP: &str = "http://owner:synthetic-db-proxy@owner-proxy:8181";
const OWNER_HTTPS: &str = "http://owner:synthetic-db-secure-proxy@owner-secure:8182";
const SERVICE_HTTP: &str = "http://service:synthetic-service-proxy@service-proxy:8282";
const AMBIENT_UPPER: &str = "http://ambient-upper:8383";
const AMBIENT_LOWER: &str = "http://ambient-lower:8484";
const PROJECT_UPPER: &str = "http://project-upper:8585";
const PROJECT_LOWER: &str = "http://project-lower:8686";
const PROJECT_HTTPS: &str = "http://project-secure:8787";
const PROJECT_ALL: &str = "socks5://project-all:8888";

fn isolated(function: &str, upper: &str, lower: &str) -> bool {
    if std::env::var_os("CODEX_OWNER_PROXY_TEST_CHILD").is_some() {
        return false;
    }
    std::fs::create_dir_all("_local/tmp").unwrap();
    let root = tempfile::tempdir_in("_local/tmp").unwrap();
    let home = std::fs::canonicalize(root.path()).unwrap();
    let name = crate::test_support::exact_test_name(module_path!(), function);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        // Keep native sockets under the harness's short temp root; account and
        // direnv trust remain private to this child.
        .env("TMPDIR", std::env::temp_dir())
        .env("HTTP_PROXY", upper)
        .env("http_proxy", lower)
        .env("CODEX_OWNER_PROXY_TEST_CHILD", "1")
        .output()
        .unwrap();
    crate::test_support::assert_isolated_test_passed(&output);
    true
}

fn project(f: &Fixture, before_exports: &str) {
    std::fs::write(
        f._root.path().join(".envrc"),
        format!(
            "{before_exports}\nprintf evaluated >> \"$PWD/project-evaluated\"\nexport HTTP_PROXY={PROJECT_UPPER}\nexport http_proxy={PROJECT_LOWER}\nexport HTTPS_PROXY={PROJECT_HTTPS}\nexport https_proxy={PROJECT_HTTPS}\nexport ALL_PROXY={PROJECT_ALL}\nexport all_proxy={PROJECT_ALL}\n"
        ),
    )
    .unwrap();
    let output = std::process::Command::new("direnv")
        .arg("allow")
        .arg(f._root.path())
        .env_clear()
        .envs(crate::workspace_service::environment::directory_base_environment())
        .output()
        .unwrap();
    assert!(output.status.success());
    f.answers.lock().unwrap().insert(
        "config/read".into(),
        json!({"result":{"config":{"shell_environment_policy":{
            "filters":{"*":"include","http_proxy":"exclude"},
            "set":{"GH_TOKEN":"synthetic-stale-global"}
        }}}}),
    );
}

async fn start(f: &Fixture) -> Value {
    let card = seed_thread_card(f).await;
    f.daemon
        .thread_start_mint_for_card(
            &card,
            SharedThreadStartParams {
                cwd: f._root.path().display().to_string(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    f.requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request["method"] == "thread/start")
        .cloned()
        .expect("real native mint")
}

fn assert_proxy_environment(request: &Value, upper: &str, lower: &str, https: &str) {
    let policy = &request["params"]["config"]["shell_environment_policy"];
    assert_eq!(policy["inherit"], "none");
    let values = &policy["set"];
    for (name, expected) in [
        ("HTTP_PROXY", upper),
        ("http_proxy", lower),
        ("HTTPS_PROXY", https),
        ("https_proxy", https),
        ("ALL_PROXY", PROJECT_ALL),
        ("all_proxy", PROJECT_ALL),
    ] {
        assert_eq!(values[name], expected, "{name}");
        assert!(
            policy["include_only"]
                .as_array()
                .unwrap()
                .contains(&json!(name))
        );
    }
    assert!(values.get("GH_TOKEN").is_none());
    assert_eq!(policy["exclude"], json!([]));
}

#[tokio::test]
async fn codex_owner_http_proxy_reaches_new_native_thread_after_private_project_evaluation() {
    if isolated(
        stringify!(
            codex_owner_http_proxy_reaches_new_native_thread_after_private_project_evaluation
        ),
        SERVICE_HTTP,
        SERVICE_HTTP,
    ) {
        return;
    }
    let f = fixture().await;
    f.repo
        .settings_upsert("http_proxy", OWNER_HTTP)
        .await
        .unwrap();
    project(
        &f,
        "test \"${HTTP_PROXY-unset}\" = unset || exit 71\ntest \"${http_proxy-unset}\" = unset || exit 72",
    );
    let request = start(&f).await;
    assert!(f._root.path().join("project-evaluated").exists());
    assert_proxy_environment(&request, OWNER_HTTP, OWNER_HTTP, PROJECT_HTTPS);
    assert!(
        !f.daemon
            .status_snapshot()
            .last_error
            .unwrap_or_default()
            .contains("synthetic-db-proxy")
    );
}

#[tokio::test]
async fn codex_owner_http_proxy_reaches_cold_native_reload_without_persisting_it() {
    if isolated(
        stringify!(codex_owner_http_proxy_reaches_cold_native_reload_without_persisting_it),
        SERVICE_HTTP,
        SERVICE_HTTP,
    ) {
        return;
    }
    let f = fixture().await;
    f.repo
        .settings_upsert("http_proxy", OWNER_HTTP)
        .await
        .unwrap();
    f.repo
        .settings_upsert("https_proxy", OWNER_HTTPS)
        .await
        .unwrap();
    let (_, worker_session) = seed_cold_owner(&f).await;
    project(
        &f,
        "test \"${HTTP_PROXY-unset}\" = unset || exit 71\ntest \"${http_proxy-unset}\" = unset || exit 72\ntest \"${HTTPS_PROXY-unset}\" = unset || exit 73\ntest \"${https_proxy-unset}\" = unset || exit 74",
    );
    f.daemon.connected_client().await.unwrap();
    let request = f
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request["method"] == "thread/resume")
        .cloned()
        .unwrap();
    assert_proxy_environment(&request, OWNER_HTTP, OWNER_HTTP, OWNER_HTTPS);
    let persisted: Option<String> =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(worker_session)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    let persisted = persisted.unwrap_or_default();
    for secret in ["synthetic-db-proxy", "synthetic-db-secure-proxy"] {
        assert!(!persisted.contains(secret));
    }
}

#[tokio::test]
async fn codex_without_owner_proxy_keeps_project_aliases_and_explicit_unsets() {
    if isolated(
        stringify!(codex_without_owner_proxy_keeps_project_aliases_and_explicit_unsets),
        AMBIENT_UPPER,
        AMBIENT_LOWER,
    ) {
        return;
    }
    for unset in [false, true] {
        let f = fixture().await;
        project(
            &f,
            &format!(
                "test \"$HTTP_PROXY\" = {AMBIENT_UPPER} || exit 75\ntest \"$http_proxy\" = {AMBIENT_LOWER} || exit 76"
            ),
        );
        if unset {
            use std::io::Write;
            writeln!(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(f._root.path().join(".envrc"))
                    .unwrap(),
                "unset HTTP_PROXY http_proxy"
            )
            .unwrap();
            let output = std::process::Command::new("direnv")
                .arg("allow")
                .arg(f._root.path())
                .env_clear()
                .envs(crate::workspace_service::environment::directory_base_environment())
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        let request = start(&f).await;
        if unset {
            let values = &request["params"]["config"]["shell_environment_policy"]["set"];
            assert!(values.get("HTTP_PROXY").is_none());
            assert!(values.get("http_proxy").is_none());
            assert_eq!(values["HTTPS_PROXY"], PROJECT_HTTPS);
            assert_eq!(values["ALL_PROXY"], PROJECT_ALL);
        } else {
            assert_proxy_environment(&request, PROJECT_UPPER, PROJECT_LOWER, PROJECT_HTTPS);
        }
    }
}

#[tokio::test]
async fn codex_loaded_thread_retains_proxy_snapshot_without_project_or_metadata_reload() {
    if isolated(
        stringify!(codex_loaded_thread_retains_proxy_snapshot_without_project_or_metadata_reload),
        SERVICE_HTTP,
        SERVICE_HTTP,
    ) {
        return;
    }
    let f = fixture().await;
    seed_cold_owner(&f).await;
    f.repo
        .settings_upsert("http_proxy", OWNER_HTTP)
        .await
        .unwrap();
    std::fs::write(f._root.path().join(".envrc"), "touch project-evaluated\n").unwrap();
    f.answers.lock().unwrap().insert(
        "thread/read".into(),
        json!({"result":{"thread":{
            "id":"cold-thread","status":{"type":"idle"},"turns":[]
        }}}),
    );
    f.daemon.connected_client().await.unwrap();
    let requests = f.requests.lock().unwrap();
    let resumed = requests
        .iter()
        .find(|request| request["method"] == "thread/resume")
        .unwrap();
    assert!(resumed["params"].get("config").is_none());
    assert!(
        !requests
            .iter()
            .any(|request| request["method"] == "config/read" || request["method"] == "turn/start")
    );
    assert!(!f._root.path().join("project-evaluated").exists());
}
