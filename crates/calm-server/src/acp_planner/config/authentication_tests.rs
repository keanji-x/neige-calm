use super::*;
use serde_json::{Value, json};

fn registration(method: Option<Value>, env: Value) -> Value {
    let mut agent = json!({"provider":"grok","command":"/opt/grok","args":[],
        "env":env,"min_agent_version":"1.0.70"});
    if let Some(method) = method {
        agent["auth_method"] = method;
    }
    agent
}

fn read(agent: Value) -> Result<AcpPlannerConfig> {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), json!({"agents":[agent]}).to_string()).unwrap();
    AcpPlannerConfig::read(file.path())
}

#[test]
fn api_key_credentials_require_an_explicit_api_key_method() {
    for key in ["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"] {
        let env = json!({key:"fixture-private-marker"});
        assert!(read(registration(Some(json!("xai.api_key")), env.clone())).is_ok());
        for method in [None, Some(json!("cached_token"))] {
            let error = read(registration(method, env.clone())).unwrap_err();
            assert!(matches!(error, CalmError::BadRequest(_)));
            assert!(!error.to_string().contains("fixture-private-marker"));
        }
    }
    assert!(read(registration(Some(json!("xai.api_key")), json!({}))).is_ok());
}

#[test]
fn authentication_selection_is_strict_and_profile_owned() {
    for method in [
        json!(null),
        json!(true),
        json!(42),
        json!({}),
        json!([]),
        json!(""),
        json!("grok.com"),
        json!("XAI_API_KEY"),
        json!("private-method-marker"),
    ] {
        let error = read(registration(Some(method), json!({}))).unwrap_err();
        assert!(!error.to_string().contains("private-method-marker"));
    }
    let mut agent = registration(Some(json!("xai.api_key")), json!({}));
    agent["provider"] = json!("opencode");
    assert!(matches!(read(agent), Err(CalmError::BadRequest(_))));
}

#[test]
fn auth_switch_changes_native_session_identity_but_default_is_compatible() {
    let digest = |method| {
        read(registration(method, json!({}))).unwrap().agents[0]
            .native_session_digest(Path::new("/work"))
            .unwrap()
    };
    let original = digest(None);
    // Persisted pre-auth-selection identity for this registration; upgrades must retain it.
    assert_eq!(
        original,
        "90e387acf2be3157362fa0c8b17a431f8c33c1689ec3c0633d1269263875b636"
    );
    assert_eq!(digest(Some(json!("cached_token"))), original);
    assert_ne!(digest(Some(json!("xai.api_key"))), original);
}
