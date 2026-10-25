use super::config::{AgentBackendConfig, BackendCredentials, ConfigError};
use super::{BackendId, InvalidBackendId};
use clap::{CommandFactory, Parser};
use serde_json::{Value, json};

fn config() -> Value {
    json!({
        "policy": "weighted_least_inflight",
        "cooldown_after_concurrency_limit_secs": 60,
        "cooldown_default_secs": 300,
        "cooldown_max_secs": 3600,
        "backends": [{"provider":"codex", "id":"default", "weight":1,
            "accepts_new_sessions":true,
            "credentials":{"kind":"codex_home", "path":"/not-created/codex-home"}}]
    })
}

fn parse(value: Value) -> Result<AgentBackendConfig, ConfigError> {
    AgentBackendConfig::parse(&serde_json::to_vec(&value).unwrap())
}

#[test]
fn agent_backend_pool_id_contract() {
    for valid in ["default", "team-b", "a0-", "-", &"x".repeat(32)] {
        assert_eq!(
            BackendId::try_from(valid.to_owned()).unwrap().as_str(),
            valid
        );
    }
    for invalid in ["", "Upper", "a_b", "a/b", ".", "é", "a b", &"x".repeat(33)] {
        assert_eq!(
            BackendId::try_from(invalid.to_owned()),
            Err(InvalidBackendId)
        );
        let mut value = config();
        value["backends"][0]["id"] = json!(invalid);
        assert!(matches!(parse(value), Err(ConfigError::Json(_))));
    }
}

#[test]
fn agent_backend_pool_duplicate_id_and_protocol() {
    let mut value = config();
    let profile = value["backends"][0].clone();
    value["backends"].as_array_mut().unwrap().push(profile);
    assert!(matches!(
        parse(value),
        Err(ConfigError::DuplicateBackend(_))
    ));
    for provider in ["claude", "opencode", "grok"] {
        let mut value = config();
        value["backends"][0]["provider"] = json!(provider);
        assert!(matches!(
            parse(value),
            Err(ConfigError::CredentialProtocol(_))
        ));
    }
}

#[test]
fn agent_backend_pool_unknown_fields_and_closed_enums() {
    let mut root = config();
    root["extra"] = json!(true);
    let mut backend = config();
    backend["backends"][0]["extra"] = json!(true);
    let mut credentials = config();
    credentials["backends"][0]["credentials"]["extra"] = json!(true);
    let mut kind = config();
    kind["backends"][0]["credentials"]["kind"] = json!("claude_config_dir");
    let mut policy = config();
    policy["policy"] = json!("random");
    let mut provider = config();
    provider["backends"][0]["provider"] = json!("unknown");
    for value in [root, backend, credentials, kind, policy, provider] {
        assert!(matches!(parse(value), Err(ConfigError::Json(_))));
    }
}

#[test]
fn agent_backend_pool_required_fields_and_numbers() {
    for field in ["kind", "path"] {
        let mut value = config();
        value["backends"][0]["credentials"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(matches!(parse(value), Err(ConfigError::Json(_))));
    }
    for field in [
        "provider",
        "id",
        "weight",
        "accepts_new_sessions",
        "credentials",
    ] {
        let mut value = config();
        value["backends"][0].as_object_mut().unwrap().remove(field);
        assert!(matches!(parse(value), Err(ConfigError::Json(_))));
    }
    for field in [
        "cooldown_after_concurrency_limit_secs",
        "cooldown_default_secs",
        "cooldown_max_secs",
        "backends",
    ] {
        let mut value = config();
        value.as_object_mut().unwrap().remove(field);
        assert!(matches!(parse(value), Err(ConfigError::Json(_))));
    }
    for field in ["weight", "max_inflight"] {
        for number in [json!(0), json!(-1), json!(1.5), json!(u64::MAX)] {
            let mut value = config();
            value["backends"][0][field] = number;
            assert!(matches!(parse(value), Err(ConfigError::Json(_))));
        }
    }
    for field in [
        "cooldown_after_concurrency_limit_secs",
        "cooldown_default_secs",
        "cooldown_max_secs",
    ] {
        let mut value = config();
        value[field] = json!(0);
        assert!(matches!(parse(value), Err(ConfigError::InvalidCooldown)));
    }
    let mut value = config();
    value["cooldown_max_secs"] = json!(59);
    assert!(matches!(parse(value), Err(ConfigError::InvalidCooldown)));
}

#[test]
fn agent_backend_pool_absolute_paths_without_filesystem_validation() {
    for path in ["relative", "", "~/codex", "./home"] {
        let mut value = config();
        value["backends"][0]["credentials"]["path"] = json!(path);
        assert!(matches!(parse(value), Err(ConfigError::RelativePath(_))));
    }
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("absent/../home");
    let mut value = config();
    value["backends"][0]["credentials"]["path"] = json!(home);
    let mut second = value["backends"][0].clone();
    second["id"] = json!("team-b");
    // Same lexical path is intentionally not an isolation check in P1.
    value["backends"].as_array_mut().unwrap().push(second);
    assert_eq!(parse(value).unwrap().backends.len(), 2);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn agent_backend_pool_startup_cli_read_and_implicit_default() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("not-created");
    let implicit = AgentBackendConfig::for_startup(None, &missing).unwrap();
    assert_eq!(implicit.backends.len(), 1);
    let profile = &implicit.backends[0];
    assert_eq!(profile.provider, calm_types::runtime::AgentProvider::Codex);
    assert_eq!(profile.id.as_str(), "default");
    assert_eq!(
        profile.credentials,
        BackendCredentials::CodexHome {
            path: missing.join("codex-home")
        }
    );
    assert!(!missing.exists());
    assert!(matches!(
        AgentBackendConfig::read(&missing),
        Err(ConfigError::Read { .. })
    ));

    let path = temp.path().join("pool.json");
    std::fs::write(&path, serde_json::to_vec(&config()).unwrap()).unwrap();
    let cli = crate::config::Config::try_parse_from([
        std::ffi::OsStr::new("calm-server"),
        std::ffi::OsStr::new("--agent-backend-config"),
        path.as_os_str(),
    ])
    .unwrap();
    let explicit =
        AgentBackendConfig::for_startup(cli.agent_backend_config.as_deref(), &missing).unwrap();
    assert_eq!(explicit, parse(config()).unwrap());
    let cli_arg = crate::config::Config::command()
        .get_arguments()
        .find(|arg| arg.get_id() == "agent_backend_config")
        .unwrap()
        .clone();
    assert!(cli_arg.get_env().is_none());
    assert!(
        crate::config::Config::try_parse_from(["calm-server", "--agent-backend-config"]).is_err()
    );
    std::fs::write(&path, b"{").unwrap();
    assert!(matches!(
        AgentBackendConfig::read(&path),
        Err(ConfigError::Json(_))
    ));
    let mut invalid = config();
    invalid["backends"][0]["credentials"]["path"] = json!("relative");
    std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
    assert!(matches!(
        AgentBackendConfig::read(&path),
        Err(ConfigError::RelativePath(_))
    ));
    assert!(!missing.exists());
}

#[test]
fn agent_backend_pool_default_policy_and_empty_explicit_configuration() {
    let mut value = config();
    value.as_object_mut().unwrap().remove("policy");
    assert_eq!(
        parse(value.clone()).unwrap().policy,
        super::policy::SelectionPolicy::WeightedLeastInflight
    );
    value["policy"] = json!("ordered");
    value["backends"] = json!([]);
    let parsed = parse(value).unwrap();
    assert_eq!(parsed.policy, super::policy::SelectionPolicy::Ordered);
    assert!(parsed.backends.is_empty());
}

#[tokio::test]
async fn agent_backend_pool_invalid_startup_refuses_before_storage_or_runtime_creation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("pool.json");
    let data = temp.path().join("runtime");
    std::fs::write(&path, b"{}").unwrap();
    let mut cli = crate::config::Config::parse_from(["calm-server"]);
    cli.agent_backend_config = Some(path);
    cli.data_dir = Some(data.clone());
    // Neither authentication validation nor the invalid storage URL is reached.
    cli.db_url = "invalid-storage-url".into();
    let result = crate::state::AppState::boot(&cli).await;
    let Err(error) = result else {
        panic!("invalid config must refuse boot")
    };
    assert!(matches!(
        error.downcast_ref::<ConfigError>(),
        Some(ConfigError::Json(_))
    ));
    assert!(!data.exists());
}
