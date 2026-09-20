//! Tests for the `cli-query` runtime and its bring-up.

use super::bringup::*;
use super::*;
use crate::operation::forge_action_adapter::{
    FORGE_CREDENTIAL_ENV_KEYS, FORGE_NONCREDENTIAL_ENV_KEYS, forge_passthrough_env_keys,
};
use crate::plugin_host::manifest::CliQueryBlock;
use serde_json::{Map, json};

#[cfg(unix)]
use super::super::child_process::{TEST_DRAIN_STARTED, TEST_REAP_STARTED, TestPhaseObserver};

fn tool(args: &[&str]) -> CliQueryTool {
    serde_json::from_value(json!({
        "name": "quote",
        "input_schema": {
            "type": "object",
            "properties": { "symbol": { "type": "string" }, "n": { "type": "number" } },
        },
        "args": args,
    }))
    .unwrap()
}

fn block(v: Value) -> CliQueryBlock {
    serde_json::from_value(v).unwrap()
}

fn no_config() -> BTreeMap<String, String> {
    BTreeMap::new()
}

#[test]
fn a_slot_is_substituted_only_as_a_whole_argv_element() {
    let t = tool(&["quote", "{{symbol}}", "--sym={{symbol}}", "--json"]);
    let argv = render_argv(&t, &json!({ "symbol": "700.HK" }), &no_config()).unwrap();
    assert_eq!(
        argv,
        vec!["quote", "700.HK", "--sym={{symbol}}", "--json"],
        "only the whole-element form substitutes"
    );
}

#[test]
fn a_value_with_shell_metacharacters_is_one_literal_argv_element() {
    let t = tool(&["quote", "{{symbol}}"]);
    let argv = render_argv(
        &t,
        &json!({ "symbol": "a b; rm -rf / && echo $HOME" }),
        &no_config(),
    )
    .unwrap();
    assert_eq!(argv.len(), 2);
    assert_eq!(argv[1], "a b; rm -rf / && echo $HOME");
}

#[test]
fn a_missing_slot_is_refused_by_name_not_rendered_as_an_empty_element() {
    let t = tool(&["quote", "{{symbol}}"]);
    for arguments in [json!({}), json!({ "symbol": null }), json!(null)] {
        let err = render_argv(&t, &arguments, &no_config())
            .unwrap_err_or_panic("a missing slot must be refused", &arguments);
        assert!(err.contains("symbol"), "must name the slot: {err}");
    }
}

#[test]
fn non_string_scalars_render_as_their_json_form() {
    let t = tool(&["{{symbol}}"]);
    for (value, expect) in [
        (json!(1), "1"),
        (json!(-3), "-3"),
        (json!(1.5), "1.5"),
        (json!(true), "true"),
        (json!(false), "false"),
    ] {
        let argv = render_argv(&t, &json!({ "symbol": value }), &no_config()).unwrap();
        assert_eq!(argv, vec![expect.to_string()], "for {value}");
    }
}

#[test]
fn arrays_and_objects_are_refused() {
    let t = tool(&["{{symbol}}"]);
    for value in [json!([1, 2]), json!({ "a": 1 })] {
        let err = render_argv(&t, &json!({ "symbol": value.clone() }), &no_config())
            .expect_err(&format!("{value} must be refused"));
        assert!(err.contains("symbol"), "{err}");
    }
    assert!(render_argv(&t, &json!("nope"), &no_config()).is_err());
}

#[test]
fn unknown_argument_keys_are_ignored() {
    let t = tool(&["quote", "{{symbol}}"]);
    let argv = render_argv(&t, &json!({ "symbol": "X", "unused": "Y" }), &no_config()).unwrap();
    assert_eq!(argv, vec!["quote", "X"]);
    assert!(!argv.iter().any(|a| a == "Y"));
}

#[test]
fn output_under_the_cap_is_untouched_and_unmarked() {
    let out = capped_text(b"hello", 32);
    assert_eq!(out, "hello");
    assert!(!out.contains("truncated"));
}

#[test]
fn output_over_the_cap_is_truncated_with_an_explicit_marker() {
    let src = vec![b'x'; 100];
    let out = capped_text(&src, 40);
    assert!(out.starts_with(&"x".repeat(40)), "{out}");
    assert!(
        out.contains("[truncated at 40 bytes"),
        "the cut must be announced: {out}"
    );
    // The marker must NOT claim a total: the tail is drained uncounted.
    assert!(!out.contains("of 100"), "{out}");
}

/// Cutting at byte 4 of `"aa中文"` lands inside the first multi-byte character.
#[test]
fn truncation_never_splits_a_multi_byte_character() {
    // b"aa" + 3-byte 中 + 3-byte 文 = 8 bytes.
    let src = "aa\u{4e2d}\u{6587}".as_bytes().to_vec();
    assert_eq!(src.len(), 8);
    for cap in [2, 3, 4, 5, 6, 7] {
        let out = capped_text(&src, cap);
        assert!(
            !out.contains('\u{FFFD}'),
            "cap {cap} produced a replacement character: {out:?}"
        );
        let body = out.split("\n[truncated").next().unwrap();
        assert!(
            "aa\u{4e2d}\u{6587}".starts_with(body),
            "cap {cap}: {body:?} is not a prefix of the source"
        );
        assert!(out.contains("truncated"), "cap {cap}: {out:?}");
    }
    // The 3-byte character is only included once the whole of it fits.
    assert!(capped_text(&src, 4).starts_with("aa\n"));
    assert!(capped_text(&src, 5).starts_with("aa\u{4e2d}"));
}

/// Every forge passthrough key set, driven off the production constants so a key added to either bucket is automatically in the fixture.
fn service_env() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = [
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/home/svc"),
        ("LANG", "C.UTF-8"),
        ("TZ", "UTC"),
        ("NOT_ALLOWED", "nope"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for (i, key) in forge_passthrough_env_keys().enumerate() {
        // Distinct values, so a leak "under another name" is detectable.
        env.insert(key.to_string(), format!("forge-passthrough-value-{i}"));
    }
    env
}

#[test]
fn child_env_is_the_base_set_plus_allow_plus_secrets() {
    let b = block(json!({
        "command": "longbridge",
        "env_allow": ["TZ", "ABSENT_FROM_SERVICE"],
        "secret_env": ["LB_TOKEN"],
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let secrets = [("LB_TOKEN".to_string(), "sk-lb".to_string())]
        .into_iter()
        .collect();
    let env = build_child_env(
        &b,
        &secrets,
        &service_env(),
        "/opt/lb/bin:/usr/bin:/bin",
        "s",
        &no_config(),
    )
    .unwrap();

    assert_eq!(env.get("PATH").unwrap(), "/opt/lb/bin:/usr/bin:/bin");
    assert_eq!(env.get("HOME").unwrap(), "/home/svc");
    assert_eq!(env.get("LANG").unwrap(), "C.UTF-8");
    assert_eq!(env.get("TZ").unwrap(), "UTC");
    assert_eq!(env.get("LB_TOKEN").unwrap(), "sk-lb");
    assert!(!env.contains_key("ABSENT_FROM_SERVICE"));
    assert!(!env.contains_key("NOT_ALLOWED"));
    let mut keys: Vec<&str> = env.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["HOME", "LANG", "LB_TOKEN", "PATH", "TZ"]);
}

#[test]
fn no_forge_credential_ever_reaches_the_child_env() {
    let svc = service_env();
    assert!(
        !FORGE_CREDENTIAL_ENV_KEYS.is_empty(),
        "the denylist must not be vacuous"
    );
    for key in forge_passthrough_env_keys() {
        assert!(svc.contains_key(key), "fixture must set {key}");
    }

    let plain = block(json!({
        "command": "longbridge",
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    // …and the manifest that ASKS for all of them: "requesting them does not get them" is a property of the code, not the fixture.
    let greedy = block(json!({
        "command": "longbridge",
        "env_allow": FORGE_CREDENTIAL_ENV_KEYS,
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));

    for (label, b) in [("plain", &plain), ("env_allow-requests-them", &greedy)] {
        let env =
            build_child_env(b, &BTreeMap::new(), &svc, "/usr/bin", "s", &no_config()).unwrap();
        for key in FORGE_CREDENTIAL_ENV_KEYS {
            assert!(
                !env.contains_key(*key),
                "[{label}] {key} leaked into a cli-query child environment: {:?}",
                env.keys().collect::<Vec<_>>()
            );
        }
        // …and none of the VALUES rode along under a different name.
        for value in svc
            .iter()
            .filter(|(k, _)| FORGE_CREDENTIAL_ENV_KEYS.contains(&k.as_str()))
            .map(|(_, v)| v)
        {
            assert!(
                !env.values().any(|v| v == value),
                "[{label}] a forge credential value leaked under another key"
            );
        }
    }
}

#[test]
fn a_manifest_whose_env_allow_names_a_forge_key_is_refused_at_parse_time() {
    use super::super::manifest::Manifest;

    let manifest = |env_allow: Value| {
        json!({
            "manifest_version": 1,
            "kind": "cli-query",
            "id": "lb-query",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "LB",
            "cli_query": {
                "command": "longbridge",
                "env_allow": env_allow,
                "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
            }
        })
        .to_string()
    };

    // The control: a benign allowlist LOADS.
    Manifest::parse(&manifest(json!(["TZ"])))
        .expect("a benign env_allow must still load; otherwise this test proves nothing");

    for key in FORGE_CREDENTIAL_ENV_KEYS {
        let err = Manifest::parse(&manifest(json!(["TZ", key])))
            .err()
            .unwrap_or_else(|| panic!("env_allow naming {key} must be refused"));
        let msg = err.to_string();
        assert!(msg.contains(key), "the refusal must name the key: {msg}");
        assert!(
            msg.contains("env_allow"),
            "the refusal must name the field: {msg}"
        );
    }

    // …and the NON-credential half of the forge passthrough set must LOAD: `no_proxy` is an ordinary need behind a proxy.
    for key in FORGE_NONCREDENTIAL_ENV_KEYS {
        let parsed = Manifest::parse(&manifest(json!(["TZ", key])));
        assert!(
            parsed.is_ok(),
            "env_allow naming the non-credential key {key} must load, got {:?}",
            parsed.err()
        );
    }
    // The proxy variables that were never denied.
    for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
        assert!(
            Manifest::parse(&manifest(json!([key]))).is_ok(),
            "{key} must load"
        );
    }
}

#[test]
fn a_non_credential_forge_key_named_by_env_allow_is_forwarded() {
    assert!(!FORGE_NONCREDENTIAL_ENV_KEYS.is_empty());
    let b = block(json!({
        "command": "longbridge",
        "env_allow": FORGE_NONCREDENTIAL_ENV_KEYS,
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let svc = service_env();
    let env = build_child_env(&b, &BTreeMap::new(), &svc, "/usr/bin", "s", &no_config()).unwrap();
    for key in FORGE_NONCREDENTIAL_ENV_KEYS {
        assert_eq!(
            env.get(*key),
            svc.get(*key),
            "{key} is not a credential and must be forwarded"
        );
    }
}

#[test]
fn secret_env_may_name_a_forge_key_and_gets_the_operators_own_value() {
    let b = block(json!({
        "command": "longbridge",
        "secret_env": ["GH_TOKEN"],
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let secrets = [("GH_TOKEN".to_string(), "operator-authored".to_string())]
        .into_iter()
        .collect();
    let svc = service_env();
    let env = build_child_env(&b, &secrets, &svc, "/usr/bin", "s", &no_config()).unwrap();
    assert_eq!(env.get("GH_TOKEN").unwrap(), "operator-authored");
    assert_ne!(
        env.get("GH_TOKEN").unwrap(),
        svc.get("GH_TOKEN").unwrap(),
        "the SERVICE value must never be the one that lands"
    );
}

#[test]
fn a_secret_env_key_with_no_secret_is_a_bring_up_failure() {
    let b = block(json!({
        "command": "longbridge",
        "secret_env": ["LB_TOKEN"],
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let err = build_child_env(
        &b,
        &BTreeMap::new(),
        &service_env(),
        "/usr/bin",
        "/plugins/lb/secrets.json",
        &no_config(),
    )
    .unwrap_err();
    assert!(err.contains("LB_TOKEN"), "{err}");
    assert!(err.contains("/plugins/lb/secrets.json"), "{err}");
}

/// One block per source on purpose: a fixture naming `PATH` in both `env_allow` and `secret_env` is refused at parse time, so a combined fixture would guard an unreachable state.
#[test]
fn path_cannot_be_overridden_by_env_allow() {
    let b = block(json!({
        "command": "longbridge",
        "env_allow": ["PATH"],
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let env = build_child_env(
        &b,
        &BTreeMap::new(),
        &service_env(),
        "/opt/lb/bin",
        "s",
        &no_config(),
    )
    .unwrap();
    assert_eq!(env.get("PATH").unwrap(), "/opt/lb/bin");
}

#[test]
fn path_cannot_be_overridden_by_secret_env() {
    let b = block(json!({
        "command": "longbridge",
        "secret_env": ["PATH"],
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let secrets = [("PATH".to_string(), "/evil".to_string())]
        .into_iter()
        .collect();
    let env = build_child_env(
        &b,
        &secrets,
        &service_env(),
        "/opt/lb/bin",
        "s",
        &no_config(),
    )
    .unwrap();
    assert_eq!(env.get("PATH").unwrap(), "/opt/lb/bin");
}

#[test]
fn config_env_carries_the_operators_value_into_a_manifest_declared_key() {
    let b = block(json!({
        "command": "longbridge",
        "config_env": ["LB_ACCOUNT"],
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let config = [("LB_ACCOUNT".to_string(), "acct-42".to_string())]
        .into_iter()
        .collect();
    let env = build_child_env(
        &b,
        &BTreeMap::new(),
        &service_env(),
        "/usr/bin",
        "s",
        &config,
    )
    .unwrap();
    assert_eq!(env.get("LB_ACCOUNT").map(String::as_str), Some("acct-42"));
}

#[test]
fn a_configuration_key_the_manifest_did_not_declare_reaches_the_child_under_no_name() {
    let b = block(json!({
        "command": "longbridge",
        "config_env": ["LB_ACCOUNT"],
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let config: BTreeMap<String, String> = [
        ("LB_ACCOUNT".to_string(), "acct-42".to_string()),
        ("endpoint".to_string(), "https://api.example".to_string()),
    ]
    .into_iter()
    .collect();
    let env = build_child_env(
        &b,
        &BTreeMap::new(),
        &service_env(),
        "/usr/bin",
        "s",
        &config,
    )
    .unwrap();
    assert_eq!(env.get("LB_ACCOUNT").map(String::as_str), Some("acct-42"));
    assert!(
        !env.values().any(|v| v == "https://api.example"),
        "an undeclared configuration value must not appear under ANY key: {:?}",
        env.keys().collect::<Vec<_>>()
    );
}

#[test]
fn a_config_env_key_with_no_value_in_force_is_absent_rather_than_a_failure() {
    let b = block(json!({
        "command": "longbridge",
        "config_env": ["LB_ACCOUNT"],
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let env = build_child_env(
        &b,
        &BTreeMap::new(),
        &service_env(),
        "/usr/bin",
        "s",
        &no_config(),
    )
    .unwrap();
    assert!(!env.contains_key("LB_ACCOUNT"), "{env:?}");
}

#[test]
fn configuration_values_flatten_to_one_string_or_refuse() {
    let effective: Map<String, Value> = serde_json::from_value(json!({
        "s": "text", "i": 3, "f": 1.5, "b": true, "gone": null
    }))
    .unwrap();
    let flat = flatten_config(&effective).unwrap();
    assert_eq!(flat.get("s").map(String::as_str), Some("text"));
    assert_eq!(flat.get("i").map(String::as_str), Some("3"));
    assert_eq!(flat.get("f").map(String::as_str), Some("1.5"));
    assert_eq!(flat.get("b").map(String::as_str), Some("true"));
    assert!(!flat.contains_key("gone"), "{flat:?}");

    let bad: Map<String, Value> = serde_json::from_value(json!({ "list": [1, 2] })).unwrap();
    let err = flatten_config(&bad).unwrap_err();
    assert!(err.contains("list"), "{err}");
}

/// Paired on purpose: one assertion without the other proves nothing, since a renderer that merged the two maps passes the positive half whenever the agent happens not to collide.
#[test]
fn an_agent_argument_cannot_displace_a_configuration_slot() {
    let t: CliQueryTool = serde_json::from_value(json!({
        "name": "quote",
        "input_schema": { "type": "object", "properties": { "symbol": { "type": "string" } } },
        "args": ["quote", "{{symbol}}", "--url", "{{config.endpoint}}"],
    }))
    .unwrap();
    let config: BTreeMap<String, String> = [(
        "endpoint".to_string(),
        "https://operator.example".to_string(),
    )]
    .into_iter()
    .collect();

    let argv = render_argv(
        &t,
        &json!({ "symbol": "700.HK", "config.endpoint": "https://attacker.example" }),
        &config,
    )
    .unwrap();
    assert_eq!(
        argv,
        vec!["quote", "700.HK", "--url", "https://operator.example"],
        "the configuration slot must take the operator's value even when the call \
         supplies an argument by that exact name"
    );
    assert!(
        !argv.iter().any(|a| a.contains("attacker")),
        "an agent-supplied `config.*` argument must reach the child under no slot: {argv:?}"
    );
}

#[test]
fn a_configuration_value_never_fills_an_argument_slot() {
    let t: CliQueryTool = serde_json::from_value(json!({
        "name": "quote",
        "input_schema": { "type": "object", "properties": { "endpoint": { "type": "string" } } },
        "args": ["quote", "{{endpoint}}"],
    }))
    .unwrap();
    let config: BTreeMap<String, String> = [(
        "endpoint".to_string(),
        "https://operator.example".to_string(),
    )]
    .into_iter()
    .collect();
    let err = render_argv(&t, &json!({}), &config)
        .expect_err("an argument slot must not be filled from configuration");
    assert!(err.contains("endpoint"), "{err}");
}

#[test]
fn a_configuration_slot_with_no_value_is_refused_by_name() {
    let t: CliQueryTool = serde_json::from_value(json!({
        "name": "quote",
        "input_schema": { "type": "object", "properties": {} },
        "args": ["quote", "{{config.endpoint}}"],
    }))
    .unwrap();
    let err = render_argv(&t, &json!({}), &no_config()).expect_err("no value, no rendering");
    assert!(err.contains("endpoint"), "{err}");
    assert!(err.contains("restart"), "{err}");
}

/// The one input that catches `config.get(key).or_else(|| obj.get(key))`: nothing in `config`, and `arguments` carrying the BARE key.
#[test]
fn a_bare_argument_may_not_back_fill_an_unconfigured_configuration_slot() {
    let t: CliQueryTool = serde_json::from_value(json!({
        "name": "quote",
        // Not `config.endpoint` — the manifest validator refuses that name.
        "input_schema": { "type": "object", "properties": { "endpoint": { "type": "string" } } },
        "args": ["quote", "--url", "{{config.endpoint}}"],
    }))
    .unwrap();

    let err = render_argv(
        &t,
        &json!({ "endpoint": "https://attacker.example" }),
        &no_config(),
    )
    .expect_err(
        "a configuration slot with no configured value must be refused, never \
         back-filled from the agent's arguments",
    );
    assert!(err.contains("endpoint"), "must name the slot: {err}");
    assert!(
        !err.contains("attacker"),
        "not even the diagnostic should echo the agent's value: {err}"
    );

    // Positive control: the SAME call renders the OPERATOR's value once configured.
    let config: BTreeMap<String, String> = [(
        "endpoint".to_string(),
        "https://operator.example".to_string(),
    )]
    .into_iter()
    .collect();
    let argv = render_argv(
        &t,
        &json!({ "endpoint": "https://attacker.example" }),
        &config,
    )
    .expect("a configured slot renders");
    assert_eq!(argv, vec!["quote", "--url", "https://operator.example"]);
    assert!(
        !argv.iter().any(|a| a.contains("attacker")),
        "the agent's bare `endpoint` must land in no argv element: {argv:?}"
    );
}

#[test]
fn a_nul_in_a_configured_value_is_refused_by_key_not_at_exec_time() {
    let effective: Map<String, Value> =
        serde_json::from_value(json!({ "endpoint": "https://a.example\u{0}evil" })).unwrap();
    let err = flatten_config(&effective).expect_err("a NUL cannot reach argv or env");
    assert!(err.contains("endpoint"), "must name the key: {err}");
    assert!(err.contains("NUL"), "{err}");

    // Other control characters are NOT refused.
    let ok: Map<String, Value> =
        serde_json::from_value(json!({ "banner": "line1\nline2\ttabbed" })).unwrap();
    let flat = flatten_config(&ok).expect("only NUL is unrepresentable");
    assert_eq!(
        flat.get("banner").map(String::as_str),
        Some("line1\nline2\ttabbed")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_argv_configuration_slot_with_no_value_fails_bring_up_not_every_call() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(tmp.path(), "ok.sh", "#!/bin/sh\necho ok\n");
    let b = block(json!({
        "command": p.display().to_string(),
        "tools": [{
            "name": "quote",
            "input_schema": { "type": "object", "properties": {} },
            "args": ["quote", "--url", "{{config.endpoint}}"],
        }],
    }));

    // `CliQueryRuntime` has no `Debug` on purpose (it holds secret values), so this cannot be an `expect_err`.
    let Err(err) = bring_up("cli-test", &b, tmp.path(), &Map::new()).await else {
        panic!("an unfillable argv slot must not come up as Running");
    };
    assert!(err.contains("endpoint"), "must name the key: {err}");
    assert!(
        err.contains("quote"),
        "must name the tool that cannot be called: {err}"
    );

    // Positive control, same manifest: once the key has a value in force the connector comes up.
    let effective: Map<String, Value> =
        serde_json::from_value(json!({ "endpoint": "https://operator.example" })).unwrap();
    let rt = bring_up("cli-test", &b, tmp.path(), &effective)
        .await
        .expect("a configured slot comes up");
    let res = rt.tools_call("quote", json!({})).await.unwrap();
    assert_eq!(res.is_error, Some(false));
}

#[test]
fn extras_are_searched_before_the_service_path() {
    let svc = per_connector_path("/usr/bin:/bin", &["/opt/lb/bin".to_string()]);
    assert_eq!(svc, "/opt/lb/bin:/usr/bin:/bin");
}

#[test]
fn an_unresolvable_bare_command_names_the_path_and_every_directory_searched() {
    let service_path = "/usr/bin:/bin";
    let err = resolve_command(
        "definitely-not-a-real-binary-1164",
        &["/opt/lb/bin".to_string()],
        service_path,
    )
    .unwrap_err();
    assert!(
        err.contains(service_path),
        "the reason must carry the service PATH: {err}"
    );
    for dir in [
        "/opt/lb/bin/definitely-not-a-real-binary-1164",
        "/usr/bin/definitely-not-a-real-binary-1164",
        "/bin/definitely-not-a-real-binary-1164",
    ] {
        assert!(err.contains(dir), "must list {dir}: {err}");
    }
}

#[cfg(unix)]
#[test]
fn a_bare_name_resolves_to_an_absolute_path_in_the_extras_first() {
    use std::os::unix::fs::PermissionsExt;
    let tmp_lo = tempfile::tempdir().unwrap();
    let tmp_hi = tempfile::tempdir().unwrap();
    for dir in [tmp_lo.path(), tmp_hi.path()] {
        let p = dir.join("mytool");
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let resolved = resolve_command(
        "mytool",
        &[tmp_hi.path().display().to_string()],
        &tmp_lo.path().display().to_string(),
    )
    .unwrap();
    assert_eq!(resolved, tmp_hi.path().join("mytool"));
    assert!(resolved.is_absolute());
}

#[cfg(unix)]
#[test]
fn a_non_executable_file_is_not_a_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("mytool");
    std::fs::write(&p, "not executable").unwrap();
    assert!(resolve_command("mytool", &[], &tmp.path().display().to_string()).is_err());
    assert!(resolve_command(&p.display().to_string(), &[], "").is_err());
}

#[test]
fn a_relative_path_command_is_refused() {
    let err = resolve_command("./bin/tool", &[], "/usr/bin").unwrap_err();
    assert!(err.contains("absolute"), "{err}");
}

/// Driven with a BARE command name on purpose: an absolute path returns before this code runs.
#[cfg(unix)]
#[test]
fn a_non_absolute_search_entry_is_skipped_and_the_reason_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("bin");
    std::fs::create_dir_all(&sub).unwrap();
    let p = sub.join("mytool");
    std::fs::write(&p, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();

    let err =
        resolve_command("mytool", &[".".to_string(), "bin".to_string()], "bin:.").unwrap_err();
    // Asserting only on the word "SKIPPED" passes with the skip deleted — that literal is in the format string unconditionally.
    for relative in ["\"./mytool\"", "\"bin/mytool\""] {
        assert!(
            !err.contains(relative),
            "a relative candidate was searched: {err}"
        );
    }
    for entry in ["\".\"", "\"bin\""] {
        assert!(
            err.contains(entry),
            "the reason must name the skipped entry {entry}: {err}"
        );
    }
    assert!(
        err.contains("working directory"),
        "the reason must say WHY: {err}"
    );

    let ok = resolve_command("mytool", &[sub.display().to_string()], "").unwrap();
    assert_eq!(ok, p);
    assert!(ok.is_absolute());
}

#[cfg(unix)]
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[cfg(unix)]
fn runtime_for(program: PathBuf, args: &[&str], timeout_ms: u64, cap: usize) -> CliQueryRuntime {
    let mut tools = BTreeMap::new();
    tools.insert("quote".to_string(), tool(args));
    CliQueryRuntime {
        plugin_id: "cli-test".to_string(),
        program,
        fingerprint: "test".to_string(),
        env: BTreeMap::new(),
        tools,
        config: no_config(),
        timeout: Duration::from_millis(timeout_ms),
        max_output_bytes: cap,
    }
}

/// A FIFO whose parent-held read/write descriptor keeps both opens non-blocking, giving process tests an event gate instead of a guessed `sleep`.
#[cfg(unix)]
fn fifo_gate(path: &Path) -> std::fs::File {
    let raw = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: plain libc call on a path inside a fresh temp directory.
    let rc = unsafe { libc::mkfifo(raw.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

/// Poll `future` until the selected real-child phase starts; the production helper freezes Tokio's clock at that event.
#[cfg(unix)]
async fn complete_at_observed_deadline<T>(
    observer: &'static tokio::task::LocalKey<TestPhaseObserver>,
    future: impl std::future::Future<Output = T>,
) -> T {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    observer
        .scope(TestPhaseObserver::new(tx, true), async move {
            tokio::pin!(future);
            let deadline = tokio::select! {
                deadline = rx.recv() => deadline.expect("the phase observer stays alive"),
                _output = &mut future => panic!("the operation finished before the observed phase started"),
            };
            let frozen_at = tokio::time::Instant::now();
            assert!(
                frozen_at < deadline,
                "the fixture must reach its observed phase before the budget expires"
            );
            tokio::time::advance(deadline.duration_since(frozen_at)).await;
            let output = future.await;
            let finished_at = tokio::time::Instant::now();
            assert!(
                finished_at >= deadline
                    && finished_at <= deadline + Duration::from_millis(1),
                "the observed phase finished at {finished_at:?}, outside Tokio's 1 ms timer \
                 precision around its original deadline {deadline:?}"
            );
            output
        })
        .await
}

/// Release a child only after the runtime has drained its closed output and started waiting for its exit status.
#[cfg(unix)]
async fn call_released_after_reap_starts(
    rt: &CliQueryRuntime,
    arguments: Value,
    mut gate: std::fs::File,
) -> Result<CallToolResult, RpcError> {
    use std::io::Write as _;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    TEST_REAP_STARTED
        .scope(TestPhaseObserver::new(tx, false), async {
            let call = rt.tools_call("quote", arguments);
            tokio::pin!(call);
            let deadline = tokio::select! {
                deadline = rx.recv() => deadline.expect("the reap observer stays alive"),
                _result = &mut call => panic!("the call finished before its reap began"),
            };
            assert!(tokio::time::Instant::now() < deadline);
            gate.write_all(b"continue\n").unwrap();
            gate.flush().unwrap();
            call.await
        })
        .await
}

#[cfg(unix)]
#[tokio::test]
async fn a_zero_exit_returns_stdout_and_is_error_false() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(tmp.path(), "ok.sh", "#!/bin/sh\necho \"got:$1\"\n");
    let rt = runtime_for(p, &["{{symbol}}"], 5_000, 4096);
    let res = rt
        .tools_call("quote", json!({ "symbol": "700.HK" }))
        .await
        .unwrap();
    assert_eq!(res.is_error, Some(false));
    assert_eq!(
        res.content[0].text.as_deref(),
        Some("got:700.HK\n"),
        "{:?}",
        res.content
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_non_zero_exit_is_is_error_true_carrying_the_output() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(
        tmp.path(),
        "bad.sh",
        "#!/bin/sh\necho partial\necho boom >&2\nexit 3\n",
    );
    let rt = runtime_for(p, &[], 5_000, 4096);
    let res = rt.tools_call("quote", json!({})).await.unwrap();
    assert_eq!(res.is_error, Some(true));
    assert_eq!(res.content[0].text.as_deref(), Some("partial\n"));
    let detail = res.content[1].text.clone().unwrap();
    assert!(detail.contains("exit"), "{detail}");
    assert!(detail.contains("boom"), "stderr must be carried: {detail}");
}

#[cfg(unix)]
#[tokio::test]
async fn stdout_over_the_cap_is_truncated_with_the_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(
        tmp.path(),
        "big.sh",
        "#!/bin/sh\nfor i in 1 2 3 4 5 6 7 8 9 0; do printf 'aaaaaaaaaa'; done\n",
    );
    let rt = runtime_for(p, &[], 5_000, 16);
    let res = rt.tools_call("quote", json!({})).await.unwrap();
    let text = res.content[0].text.clone().unwrap();
    assert!(
        text.contains("[truncated at 16 bytes"),
        "cap must be enforced and announced: {text:?}"
    );
    assert!(text.starts_with(&"a".repeat(16)));
}

#[cfg(unix)]
#[tokio::test]
async fn a_child_far_over_the_pipe_buffer_still_answers_truncated() {
    let tmp = tempfile::tempdir().unwrap();
    // 2 MiB, far past both the 64-byte cap and the 64 KiB pipe buffer.
    let p = script(
        tmp.path(),
        "flood.sh",
        "#!/bin/sh\nyes aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | head -c 2097152\n",
    );
    let rt = runtime_for(p, &[], 20_000, 64);
    let res = rt.tools_call("quote", json!({})).await.unwrap();
    assert_eq!(res.is_error, Some(false));
    let text = res.content[0].text.clone().unwrap();
    assert!(text.contains("[truncated at 64 bytes"), "{text:?}");
    assert!(text.len() < 512, "materialised {} bytes", text.len());
}

/// The assertion is on the grandchild's pid, not on the call's error: the old test passed with the kill deleted entirely.
#[cfg(unix)]
#[tokio::test]
async fn the_budget_kill_reaches_the_childs_descendants() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild.pid");
    let p = script(
        tmp.path(),
        "wrapper.sh",
        "#!/bin/sh\nsleep 30 &\necho $! > \"$1\"\nsleep 30\n",
    );
    let rt = runtime_for(p, &["{{symbol}}"], 300, 4096);
    let err = rt
        .tools_call("quote", json!({ "symbol": pidfile.display().to_string() }))
        .await
        .unwrap_err();
    assert!(err.message.contains("budget"), "{}", err.message);

    assert_recorded_descendant_dies(&pidfile, "the budget kill").await;
}

/// Poll the pid a fixture wrote until it is gone or has exited, then fail loudly (and clean up).
#[cfg(unix)]
async fn assert_recorded_descendant_dies(pidfile: &Path, what: &str) {
    let pid: i32 = std::fs::read_to_string(pidfile)
        .unwrap_or_else(|e| panic!("the fixture must have recorded a descendant pid: {e}"))
        .trim()
        .parse()
        .expect("the recorded pid must parse");
    crate::test_support::assert_pid_dead(
        pid,
        None,
        &format!("descendant {pid} survived {what} (it was orphaned onto pid 1)"),
    )
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_backgrounded_daemon_does_not_survive_a_successful_call() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("daemon.pid");
    let p = script(
        tmp.path(),
        "daemonize.sh",
        // stdout/stderr detached, so the pipes reach EOF the moment the wrapper exits.
        "#!/bin/sh\nsleep 30 >/dev/null 2>&1 &\necho $! > \"$1\"\necho ok\nexit 0\n",
    );
    let rt = runtime_for(p, &["{{symbol}}"], 20_000, 4096);
    let res = rt
        .tools_call("quote", json!({ "symbol": pidfile.display().to_string() }))
        .await
        .unwrap();

    // The leader is already a zombie when the signal lands, so its real exit status is what `wait()` reports.
    assert_eq!(res.is_error, Some(false), "{:?}", res.content);
    assert_eq!(res.content[0].text.as_deref(), Some("ok\n"));

    assert_recorded_descendant_dies(&pidfile, "a successful call").await;
}

/// The FIFO gate is released only after observing the reap phase, so EOF is guaranteed to precede exit without a wall-clock sleep.
#[cfg(unix)]
#[tokio::test]
async fn a_tool_that_closes_its_output_then_exits_reports_its_real_status() {
    let tmp = tempfile::tempdir().unwrap();
    let gate_path = tmp.path().join("reap.gate");
    let gate = fifo_gate(&gate_path);
    let p = script(
        tmp.path(),
        "linger.sh",
        "#!/bin/sh\necho answer\nexec 1>&- 2>&-\nread _gate < \"$1\"\nexit 0\n",
    );
    let rt = runtime_for(p, &["{{symbol}}"], 20_000, 4096);
    let res = call_released_after_reap_starts(
        &rt,
        json!({ "symbol": gate_path.display().to_string() }),
        gate,
    )
    .await
    .unwrap();

    assert_eq!(
        res.is_error,
        Some(false),
        "a call that exits 0 must not be reported as an error: {:?}",
        res.content
    );
    assert_eq!(res.content[0].text.as_deref(), Some("answer\n"));
    assert_eq!(
        res.content.len(),
        1,
        "no failure detail block: {:?}",
        res.content
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_tool_that_closes_its_output_then_fails_reports_its_real_code() {
    let tmp = tempfile::tempdir().unwrap();
    let gate_path = tmp.path().join("reap.gate");
    let gate = fifo_gate(&gate_path);
    let p = script(
        tmp.path(),
        "linger_fail.sh",
        "#!/bin/sh\necho partial\nexec 1>&- 2>&-\nread _gate < \"$1\"\nexit 7\n",
    );
    let rt = runtime_for(p, &["{{symbol}}"], 20_000, 4096);
    let res = call_released_after_reap_starts(
        &rt,
        json!({ "symbol": gate_path.display().to_string() }),
        gate,
    )
    .await
    .unwrap();
    assert_eq!(res.is_error, Some(true));
    let detail = res.content[1].text.clone().unwrap();
    assert!(
        detail.contains("exit status: 7"),
        "the child's own code must survive: {detail}"
    );
}

/// An OUTER timeout drops the `tools_call` future before any `wait()` has run, so the sweep provably precedes any reap.
#[cfg(unix)]
#[tokio::test]
async fn dropping_the_call_future_kills_the_process_group() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild.pid");
    let p = script(
        tmp.path(),
        "wrapper.sh",
        "#!/bin/sh\nsleep 30 >/dev/null 2>&1 &\necho $! > \"$1\"\nsleep 30\n",
    );
    // Inner budget far longer than the outer one, so the call is CANCELLED rather than expiring.
    let rt = runtime_for(p, &["{{symbol}}"], 30_000, 4096);
    let outcome = tokio::time::timeout(
        Duration::from_millis(300),
        rt.tools_call("quote", json!({ "symbol": pidfile.display().to_string() })),
    )
    .await;
    assert!(
        outcome.is_err(),
        "the outer timeout must fire first, dropping the call future"
    );

    assert_recorded_descendant_dies(&pidfile, "dropping the call future").await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_saturating_cap_returns_the_whole_answer_instead_of_overflowing() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(tmp.path(), "ok.sh", "#!/bin/sh\necho hello\n");
    let rt = runtime_for(p, &[], 5_000, usize::MAX);
    let res = rt.tools_call("quote", json!({})).await.unwrap();
    assert_eq!(res.is_error, Some(false));
    assert_eq!(
        res.content[0].text.as_deref(),
        Some("hello\n"),
        "an unbounded cap must return the answer, not an empty string"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_manifest_at_the_output_ceiling_loads_and_executes() {
    use super::super::manifest::{CLI_QUERY_MAX_OUTPUT_BYTES_CEILING, Manifest};
    let tmp = tempfile::tempdir().unwrap();
    let p = script(tmp.path(), "ok.sh", "#!/bin/sh\necho hello\n");

    let doc = json!({
        "manifest_version": 1,
        "kind": "cli-query",
        "id": "lb-query",
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "LB",
        "cli_query": {
            "command": p.display().to_string(),
            "max_output_bytes": CLI_QUERY_MAX_OUTPUT_BYTES_CEILING,
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }
    });
    let parsed = Manifest::parse(&doc.to_string()).expect("the ceiling itself must load");
    let rt = bring_up(
        "cli-test",
        parsed.cli_query.as_ref().unwrap(),
        tmp.path(),
        &Map::new(),
    )
    .await
    .unwrap();
    assert_eq!(rt.max_output_bytes(), CLI_QUERY_MAX_OUTPUT_BYTES_CEILING);
    let res = rt.tools_call("q", json!({})).await.unwrap();
    assert_eq!(res.is_error, Some(false));
    assert_eq!(res.content[0].text.as_deref(), Some("hello\n"));

    // An over-ceiling value LOADS and is CLAMPED: `registry::load_from_dir` re-parses every installed manifest at boot and only `warn!`s past a failure, so a parse-time refusal would make a working connector silently vanish.
    for over in [
        json!(CLI_QUERY_MAX_OUTPUT_BYTES_CEILING as u64 + 1),
        json!(u64::MAX),
    ] {
        let mut m = doc.clone();
        m["cli_query"]["max_output_bytes"] = over.clone();
        let parsed = Manifest::parse(&m.to_string())
            .unwrap_or_else(|e| panic!("{over} must still LOAD, not be refused: {e}"));
        let block = parsed.cli_query.as_ref().unwrap();
        assert_eq!(
            block.max_output_bytes(),
            CLI_QUERY_MAX_OUTPUT_BYTES_CEILING,
            "{over} must be clamped to the ceiling"
        );
        // The RUNTIME carries the clamped number, not the raw field; the execution check below cannot see the difference.
        let rt = bring_up("cli-test", block, tmp.path(), &Map::new())
            .await
            .unwrap();
        assert_eq!(
            rt.max_output_bytes(),
            CLI_QUERY_MAX_OUTPUT_BYTES_CEILING,
            "{over}: the runtime must enforce the clamped cap, not the raw field"
        );
        let res = rt.tools_call("q", json!({})).await.unwrap();
        assert_eq!(res.is_error, Some(false));
        assert_eq!(res.content[0].text.as_deref(), Some("hello\n"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_child_that_outlives_its_budget_is_killed_and_named() {
    let tmp = tempfile::tempdir().unwrap();
    let gate_path = tmp.path().join("drain.gate");
    let _gate = fifo_gate(&gate_path);
    let p = script(tmp.path(), "slow.sh", "#!/bin/sh\nread _gate < \"$1\"\n");
    let rt = runtime_for(p, &["{{symbol}}"], 20_000, 4096);
    let err = complete_at_observed_deadline(
        &TEST_DRAIN_STARTED,
        rt.tools_call(
            "quote",
            json!({ "symbol": gate_path.display().to_string() }),
        ),
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("20000 ms"), "{}", err.message);
    assert!(err.message.contains("budget"), "{}", err.message);
}

#[cfg(unix)]
#[tokio::test]
async fn an_unknown_tool_name_is_refused_before_any_exec() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(tmp.path(), "ok.sh", "#!/bin/sh\necho hi\n");
    let rt = runtime_for(p, &[], 5_000, 4096);
    let err = rt.tools_call("nope", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32601, "{err:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn bring_up_pins_an_absolute_path_and_records_a_fingerprint() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(
        tmp.path(),
        "vers.sh",
        "#!/bin/sh\necho 'mytool 1.2.3'\necho 'second line'\n",
    );
    let b = block(json!({
        "command": p.display().to_string(),
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let rt = bring_up("cli-test", &b, tmp.path(), &Map::new())
        .await
        .unwrap();
    assert_eq!(rt.program(), p.as_path());
    assert_eq!(rt.fingerprint(), "--version: mytool 1.2.3");
}

#[cfg(unix)]
#[tokio::test]
async fn the_version_probe_never_sees_a_secret_env_value() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let p = script(
        tmp.path(),
        "echoenv.sh",
        "#!/bin/sh\necho \"v1 token=[$LB_TOKEN]\"\n",
    );
    let secrets = tmp.path().join(super::super::connector::SECRETS_FILENAME);
    std::fs::write(&secrets, r#"{"LB_TOKEN":"sk-secret-value"}"#).unwrap();
    std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();

    let b = block(json!({
        "command": p.display().to_string(),
        "secret_env": ["LB_TOKEN"],
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let rt = bring_up("cli-test", &b, tmp.path(), &Map::new())
        .await
        .unwrap();
    assert_eq!(
        rt.fingerprint(),
        "--version: v1 token=[]",
        "the probe must run with the base environment only"
    );
    // …while the CALL environment still has it: the probe is restricted, the connector is not broken.
    assert!(rt.env_keys().contains(&"LB_TOKEN"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_failing_version_probe_falls_back_instead_of_failing_bring_up() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(tmp.path(), "novers.sh", "#!/bin/sh\nexit 1\n");
    let b = block(json!({
        "command": p.display().to_string(),
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let rt = bring_up("cli-test", &b, tmp.path(), &Map::new())
        .await
        .unwrap();
    assert!(
        rt.fingerprint().starts_with("size="),
        "expected the size+mtime fallback, got {}",
        rt.fingerprint()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_hanging_version_probe_costs_the_sub_budget_and_falls_back() {
    // The relationship that matters is the TOTAL the probe can cost, not one term of it.
    assert!(
        VERSION_PROBE_BUDGET < CLI_QUERY_BRINGUP_BUDGET,
        "the sub-budget must be strictly smaller, or a hung probe takes the enable down"
    );
    let tmp = tempfile::tempdir().unwrap();
    let gate_path = tmp.path().join("probe-drain.gate");
    let _gate = fifo_gate(&gate_path);
    let p = script(
        tmp.path(),
        "hang.sh",
        &format!("#!/bin/sh\nread _gate < \"{}\"\n", gate_path.display()),
    );
    let b = block(json!({
        "command": p.display().to_string(),
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let rt = complete_at_observed_deadline(
        &TEST_DRAIN_STARTED,
        bring_up("cli-test", &b, tmp.path(), &Map::new()),
    )
    .await
    .unwrap();

    assert!(
        rt.fingerprint().starts_with("size="),
        "a hung probe must fall back, got {}",
        rt.fingerprint()
    );
}

/// The hanging-probe test never reaches the reap (its child holds stdout open); here stdout closes immediately and the child lingers.
#[cfg(unix)]
#[tokio::test]
async fn a_version_probe_that_lingers_after_closing_stdout_stays_in_the_sub_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let gate_path = tmp.path().join("probe-reap.gate");
    let _gate = fifo_gate(&gate_path);
    let p = script(
        tmp.path(),
        "linger_version.sh",
        &format!(
            "#!/bin/sh\necho 'mytool 9.9'\nexec 1>&-\nread _gate < \"{}\"\n",
            gate_path.display()
        ),
    );
    let b = block(json!({
        "command": p.display().to_string(),
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let rt = complete_at_observed_deadline(
        &TEST_REAP_STARTED,
        bring_up("cli-test", &b, tmp.path(), &Map::new()),
    )
    .await
    .unwrap();
    // It never exited, so there is no usable version line — the fallback.
    assert!(
        rt.fingerprint().starts_with("size="),
        "{}",
        rt.fingerprint()
    );
}

/// `a_child_that_outlives_its_budget_is_killed_and_named` cannot see this: its child holds stdout open, so the drain expires first.
#[cfg(unix)]
#[tokio::test]
async fn a_call_that_lingers_after_closing_its_output_still_honours_its_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let gate_path = tmp.path().join("call-reap.gate");
    let _gate = fifo_gate(&gate_path);
    let p = script(
        tmp.path(),
        "linger_forever.sh",
        "#!/bin/sh\necho partial\nexec 1>&- 2>&-\nread _gate < \"$1\"\n",
    );
    let rt = runtime_for(p, &["{{symbol}}"], 20_000, 4096);
    let err = complete_at_observed_deadline(
        &TEST_REAP_STARTED,
        rt.tools_call(
            "quote",
            json!({ "symbol": gate_path.display().to_string() }),
        ),
    )
    .await
    .unwrap_err();

    assert!(err.message.contains("budget"), "{}", err.message);
}

/// Two spawn failures reachable without root: mode `0o011` (owner class is checked first, `EACCES`) and a `#!` naming a missing interpreter (`ENOENT`). `ENOEXEC` is not one: `execvp` re-execs under `/bin/sh`, so the spawn succeeds and exits 127.
#[cfg(unix)]
#[tokio::test]
async fn a_binary_that_cannot_be_executed_fails_bring_up_instead_of_enabling() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();

    let no_owner_exec = tmp.path().join("noexec.sh");
    std::fs::write(&no_owner_exec, "#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(&no_owner_exec, std::fs::Permissions::from_mode(0o011)).unwrap();

    let dangling_interp = tmp.path().join("dangling.sh");
    std::fs::write(
        &dangling_interp,
        "#!/nonexistent/interpreter-1164\necho hi\n",
    )
    .unwrap();
    std::fs::set_permissions(&dangling_interp, std::fs::Permissions::from_mode(0o755)).unwrap();

    for path in [&no_owner_exec, &dangling_interp] {
        // It RESOLVES — that is the whole problem.
        resolve_command(&path.display().to_string(), &[], "").unwrap_or_else(|e| {
            panic!(
                "{} must still resolve for this test to mean anything: {e}",
                path.display()
            )
        });

        let b = block(json!({
            "command": path.display().to_string(),
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));
        let err = bring_up("cli-test", &b, tmp.path(), &Map::new())
            .await
            .err()
            .unwrap_or_else(|| panic!("{} enabled despite being unrunnable", path.display()));
        assert!(err.contains(&path.display().to_string()), "{err}");
        assert!(
            err.contains("could not be executed"),
            "the reason must say what failed: {err}"
        );
    }
}

/// Driven through the real classifier rather than a re-implementation of it, so the table below is the production decision.
#[test]
fn only_file_shaped_spawn_failures_refuse_an_enable() {
    use std::io::ErrorKind::*;

    // Refuse: the file itself can never be executed by us.
    for kind in [PermissionDenied, NotFound] {
        assert!(
            is_permanent_spawn_failure(&std::io::Error::from(kind)),
            "{kind:?} is a property of the file and must fail the enable"
        );
    }
    // Fall back and enable: the machine is under pressure right now.
    for kind in [
        WouldBlock,   // EAGAIN — RLIMIT_NPROC
        OutOfMemory,  // ENOMEM
        ResourceBusy, // ETXTBSY — the binary is being rewritten
        Interrupted,
        Other,
    ] {
        assert!(
            !is_permanent_spawn_failure(&std::io::Error::from(kind)),
            "{kind:?} is transient; refusing on it strands a good connector as \
             Unavailable with nothing to retry it"
        );
    }
    // EMFILE/ENFILE have no stable `ErrorKind` mapping across releases, so assert on the raw errno.
    for errno in [libc::EMFILE, libc::ENFILE, libc::EAGAIN] {
        assert!(
            !is_permanent_spawn_failure(&std::io::Error::from_raw_os_error(errno)),
            "errno {errno} is transient"
        );
    }
}

/// Asserted on the FINAL child environment, not on `resolve_command`.
#[cfg(unix)]
#[tokio::test]
async fn the_childs_path_never_contains_a_relative_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(tmp.path(), "ok.sh", "#!/bin/sh\necho hi\n");
    let b = block(json!({
        "command": p.display().to_string(),
        "search_path_extra": [".", "bin", "../up", "/opt/lb/bin"],
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let rt = bring_up("cli-test", &b, tmp.path(), &Map::new())
        .await
        .unwrap();

    let path = rt.child_path();
    for entry in path.split(':') {
        assert!(
            Path::new(entry).is_absolute(),
            "the child's PATH carries the relative entry {entry:?}: {path}"
        );
    }
    // …and the absolute extra survived, so the filter is not "drop everything".
    assert!(
        path.split(':').any(|e| e == "/opt/lb/bin"),
        "the absolute extra must still be first-class: {path}"
    );
}

/// Deterministic despite touching the process environment: nextest runs each test in its own process, and the variable is set before any runtime thread exists.
#[cfg(unix)]
#[test]
fn a_non_utf8_service_env_variable_does_not_panic_bring_up() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: `set_var` is sound only with no concurrent reader of `environ`,
    // and nothing here is concurrent yet — no runtime has been built.
    // Cross-test it is sound only because the gate is `cargo nextest` (one process per test): sibling
    // `bring_up` tests call `std::env::vars_os()`, and under `cargo test --lib` this would be real UB.
    unsafe {
        std::env::set_var(OsStr::from_bytes(b"CLI_QUERY_BAD_\xff"), "x");
        std::env::set_var("CLI_QUERY_BAD_VALUE", OsStr::from_bytes(b"v\xff"));
        std::env::set_var("CLI_QUERY_PLAIN_OK", "plain");
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let tmp = tempfile::tempdir().unwrap();
        let p = script(tmp.path(), "ok.sh", "#!/bin/sh\necho hi\n");
        let b = block(json!({
            // Named in `env_allow` ON PURPOSE: with no `env_allow` the child environment is {PATH, HOME, LANG} by construction, so "the bad key is absent" would be vacuous.
            "env_allow": ["CLI_QUERY_BAD_VALUE", "CLI_QUERY_PLAIN_OK"],
            "command": p.display().to_string(),
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));
        let rt = bring_up("cli-test", &b, tmp.path(), &Map::new())
            .await
            .expect("a non-UTF-8 service variable must not fail the enable");
        // A key whose VALUE is not UTF-8 is dropped, not forwarded lossily …
        assert!(
            !rt.env_keys().contains(&"CLI_QUERY_BAD_VALUE"),
            "a non-UTF-8 value must not be forwarded: {:?}",
            rt.env_keys()
        );
        // … the undecodable KEY likewise never appears …
        assert!(
            !rt.env_keys()
                .iter()
                .any(|k| k.starts_with("CLI_QUERY_BAD_"))
        );
        // … and an ordinary allowlisted key IS still forwarded, so the filter is not simply "drop everything".
        assert!(
            rt.env_keys().contains(&"CLI_QUERY_PLAIN_OK"),
            "a decodable env_allow key must still be forwarded: {:?}",
            rt.env_keys()
        );
    });
}

/// So the missing-slot loop can report WHICH input silently succeeded.
trait UnwrapErrOrPanic {
    fn unwrap_err_or_panic(self, ctx: &str, input: &Value) -> String;
}
impl UnwrapErrOrPanic for Result<Vec<String>, String> {
    fn unwrap_err_or_panic(self, ctx: &str, input: &Value) -> String {
        match self {
            Ok(argv) => panic!("{ctx}: {input} rendered {argv:?}"),
            Err(e) => e,
        }
    }
}
