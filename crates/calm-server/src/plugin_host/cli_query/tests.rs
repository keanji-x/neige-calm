//! Tests for the `cli-query` runtime and its bring-up.
//!
//! Split out of the module body (rather than inlined) for the same reason
//! `forge_action_adapter` does it: the production halves stay readable and
//! inside the per-file size governance.

use super::bringup::*;
use super::*;
use crate::operation::forge_action_adapter::{
    FORGE_CREDENTIAL_ENV_KEYS, FORGE_NONCREDENTIAL_ENV_KEYS, forge_passthrough_env_keys,
};
use crate::plugin_host::manifest::CliQueryBlock;
use serde_json::json;

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

// ---- argv templating ----------------------------------------------

/// A `{{slot}}` element is replaced WHOLESALE, and only when it is the
/// whole element. `--sym={{symbol}}` stays literal — there is no string
/// concatenation in this templater, which is what keeps a value from ever
/// being parsed as anything but one argv element.
#[test]
fn a_slot_is_substituted_only_as_a_whole_argv_element() {
    let t = tool(&["quote", "{{symbol}}", "--sym={{symbol}}", "--json"]);
    let argv = render_argv(&t, &json!({ "symbol": "700.HK" })).unwrap();
    assert_eq!(
        argv,
        vec!["quote", "700.HK", "--sym={{symbol}}", "--json"],
        "only the whole-element form substitutes"
    );
}

/// The value lands as ONE element even when it contains shell metacharacters
/// and whitespace — the whole reason there is no `/bin/sh` here.
#[test]
fn a_value_with_shell_metacharacters_is_one_literal_argv_element() {
    let t = tool(&["quote", "{{symbol}}"]);
    let argv = render_argv(&t, &json!({ "symbol": "a b; rm -rf / && echo $HOME" })).unwrap();
    assert_eq!(argv.len(), 2);
    assert_eq!(argv[1], "a b; rm -rf / && echo $HOME");
}

/// A missing slot is a refusal that NAMES the slot — never an empty argv
/// element, which the child would read as a real (empty) argument.
#[test]
fn a_missing_slot_is_refused_by_name_not_rendered_as_an_empty_element() {
    let t = tool(&["quote", "{{symbol}}"]);
    for arguments in [json!({}), json!({ "symbol": null }), json!(null)] {
        let err = render_argv(&t, &arguments)
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
        let argv = render_argv(&t, &json!({ "symbol": value })).unwrap();
        assert_eq!(argv, vec![expect.to_string()], "for {value}");
    }
}

#[test]
fn arrays_and_objects_are_refused() {
    let t = tool(&["{{symbol}}"]);
    for value in [json!([1, 2]), json!({ "a": 1 })] {
        let err = render_argv(&t, &json!({ "symbol": value.clone() }))
            .expect_err(&format!("{value} must be refused"));
        assert!(err.contains("symbol"), "{err}");
    }
    // …and a non-object `arguments` payload entirely.
    assert!(render_argv(&t, &json!("nope")).is_err());
}

/// v0 does not do full JSON-Schema validation; an unknown key is simply
/// never referenced, so it cannot reach the child.
#[test]
fn unknown_argument_keys_are_ignored() {
    let t = tool(&["quote", "{{symbol}}"]);
    let argv = render_argv(&t, &json!({ "symbol": "X", "unused": "Y" })).unwrap();
    assert_eq!(argv, vec!["quote", "X"]);
    assert!(!argv.iter().any(|a| a == "Y"));
}

// ---- output capping -------------------------------------------------

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
    // The marker must NOT claim a total: the tail is drained uncounted, so
    // any "of M" here would be a number nobody measured.
    assert!(!out.contains("of 100"), "{out}");
}

/// The cap is a BYTE bound, but the result must be valid UTF-8: cutting at
/// byte 4 of `"aa中文"` lands inside the first multi-byte character. The
/// window backs off to the boundary instead of emitting a U+FFFD.
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

// ---- environment ----------------------------------------------------

/// A service environment that has EVERY forge passthrough key set —
/// credential and non-credential alike, driven off the production
/// constants, so a key added to either bucket is automatically in the
/// fixture instead of silently untested.
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
    )
    .unwrap();

    assert_eq!(env.get("PATH").unwrap(), "/opt/lb/bin:/usr/bin:/bin");
    assert_eq!(env.get("HOME").unwrap(), "/home/svc");
    assert_eq!(env.get("LANG").unwrap(), "C.UTF-8");
    assert_eq!(env.get("TZ").unwrap(), "UTC");
    assert_eq!(env.get("LB_TOKEN").unwrap(), "sk-lb");
    // An allowlisted key the service does not have is simply not forwarded.
    assert!(!env.contains_key("ABSENT_FROM_SERVICE"));
    // Nothing outside the enumeration.
    assert!(!env.contains_key("NOT_ALLOWED"));
    let mut keys: Vec<&str> = env.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["HOME", "LANG", "LB_TOKEN", "PATH", "TZ"]);
}

/// Design §4 acceptance #4 — the forge credential passthrough must never
/// reach a `cli-query` child, **even when the service environment has all
/// four set**. A connector is not a forge action: it is authored in a
/// manifest and callable by any agent that can see its tools.
///
/// Asserted twice on purpose: once for the plain manifest, and once for a
/// manifest that explicitly ASKS for them via `env_allow`/`secret_env` —
/// because "nobody requested them" is a property of the fixture, while
/// "requesting them does not get them" is a property of the code.
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
    // …and the manifest that ASKS for all of them. This is the arm the
    // previous round documented and never wrote: "nobody requested them" is
    // a property of the fixture; "requesting them does not get them" is a
    // property of the code.
    let greedy = block(json!({
        "command": "longbridge",
        "env_allow": FORGE_CREDENTIAL_ENV_KEYS,
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));

    for (label, b) in [("plain", &plain), ("env_allow-requests-them", &greedy)] {
        let env = build_child_env(b, &BTreeMap::new(), &svc, "/usr/bin", "s").unwrap();
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

/// One key per denylist entry, refused at MANIFEST PARSE time — the
/// earliest and loudest place, so install/reload never produces such a
/// connector at all. Driven off the production constant, so the quantifier
/// covers a key added to it tomorrow.
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

    // The control: a benign allowlist LOADS, so the refusals below are not
    // "cli-query manifests never parse".
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

    // …and the NON-credential half of the forge passthrough set must LOAD
    // (r2 G4). `no_proxy` is an ordinary need for a query CLI behind a
    // proxy; refusing it was both a false claim ("a forge credential") and
    // incoherent, since `HTTP_PROXY` was never on the list at all. Every
    // key in the denylist can also retroactively invalidate an installed
    // manifest at boot, so only real credentials may be in it.
    for key in FORGE_NONCREDENTIAL_ENV_KEYS {
        let parsed = Manifest::parse(&manifest(json!(["TZ", key])));
        assert!(
            parsed.is_ok(),
            "env_allow naming the non-credential key {key} must load, got {:?}",
            parsed.err()
        );
    }
    // The proxy variables that were never denied — named explicitly so the
    // incoherence cannot come back unnoticed.
    for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
        assert!(
            Manifest::parse(&manifest(json!([key]))).is_ok(),
            "{key} must load"
        );
    }
}

/// The runtime filter mirrors the parse-time denylist exactly: a
/// non-credential key that the manifest is ALLOWED to name must actually be
/// forwarded. A filter that silently dropped it would make the manifest a
/// lie in the other direction.
#[test]
fn a_non_credential_forge_key_named_by_env_allow_is_forwarded() {
    assert!(!FORGE_NONCREDENTIAL_ENV_KEYS.is_empty());
    let b = block(json!({
        "command": "longbridge",
        "env_allow": FORGE_NONCREDENTIAL_ENV_KEYS,
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let svc = service_env();
    let env = build_child_env(&b, &BTreeMap::new(), &svc, "/usr/bin", "s").unwrap();
    for key in FORGE_NONCREDENTIAL_ENV_KEYS {
        assert_eq!(
            env.get(*key),
            svc.get(*key),
            "{key} is not a credential and must be forwarded"
        );
    }
}

/// `secret_env` is deliberately NOT denylisted: those values come from the
/// connector's own `secrets.json`, which the operator authored, so there is
/// no escalation from the SERVICE identity. Locked down so the asymmetry
/// cannot be "fixed" by accident.
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
    let env = build_child_env(&b, &secrets, &svc, "/usr/bin", "s").unwrap();
    assert_eq!(env.get("GH_TOKEN").unwrap(), "operator-authored");
    assert_ne!(
        env.get("GH_TOKEN").unwrap(),
        svc.get("GH_TOKEN").unwrap(),
        "the SERVICE value must never be the one that lands"
    );
}

/// A `secret_env` key with no secret behind it fails bring-up loudly, and
/// the message names both the key and the file — an env var that is simply
/// absent turns into a per-call auth failure nobody can trace back here.
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
    )
    .unwrap_err();
    assert!(err.contains("LB_TOKEN"), "{err}");
    assert!(err.contains("/plugins/lb/secrets.json"), "{err}");
}

/// Neither `env_allow` nor `secret_env` may revert the per-connector PATH
/// the command was pinned against.
#[test]
fn path_cannot_be_overridden_by_allow_or_secrets() {
    let b = block(json!({
        "command": "longbridge",
        "env_allow": ["PATH"],
        "secret_env": ["PATH"],
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let secrets = [("PATH".to_string(), "/evil".to_string())]
        .into_iter()
        .collect();
    let env = build_child_env(&b, &secrets, &service_env(), "/opt/lb/bin", "s").unwrap();
    assert_eq!(env.get("PATH").unwrap(), "/opt/lb/bin");
}

// ---- PATH resolution ------------------------------------------------

#[test]
fn extras_are_searched_before_the_service_path() {
    let svc = per_connector_path("/usr/bin:/bin", &["/opt/lb/bin".to_string()]);
    assert_eq!(svc, "/opt/lb/bin:/usr/bin:/bin");
}

/// Design R5 — a docker preview stack with no such binary must be able to
/// see WHY from the reason alone.
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

/// #1164 P3 F6 — a "pinned" path must be absolute, which the SEARCH ENTRIES
/// decide as much as the command does. A `PATH` of `.` or `bin` would yield
/// a relative program whose meaning depends on the cwd at exec time.
///
/// Driven with a BARE command name on purpose: every other resolution
/// fixture passes an absolute path, which returns before this code runs.
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

    // The only entries that could resolve `mytool` are relative ones, and
    // they are relative to a cwd we deliberately do not control.
    let err =
        resolve_command("mytool", &[".".to_string(), "bin".to_string()], "bin:.").unwrap_err();
    // The load-bearing half: no relative candidate was ever STAT'd, so no
    // relative candidate could ever have been RETURNED. Asserting only on
    // the word "SKIPPED" passes with the skip deleted — that literal is in
    // the format string unconditionally.
    for relative in ["\"./mytool\"", "\"bin/mytool\""] {
        assert!(
            !err.contains(relative),
            "a relative candidate was searched: {err}"
        );
    }
    // …and the operator is told which entries were dropped, and why.
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

    // …and the same name DOES resolve once the entry is absolute, so the
    // skip is about absoluteness and not about the fixture being broken.
    let ok = resolve_command("mytool", &[sub.display().to_string()], "").unwrap();
    assert_eq!(ok, p);
    assert!(ok.is_absolute());
}

// ---- execution ------------------------------------------------------

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
        timeout: Duration::from_millis(timeout_ms),
        max_output_bytes: cap,
    }
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

/// A child whose output dwarfs both the cap AND the 64 KiB pipe buffer must
/// still return a TRUNCATED ANSWER, not a budget-expiry error: the tail is
/// drained (and discarded) so the child is never blocked on a full pipe.
///
/// Mutation witness: delete the drain loop in `read_capped` and this goes
/// red with "exceeded its … budget" instead of a result.
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
    // The whole answer is the cap plus one short marker line — nothing
    // close to the 2 MiB the child wrote.
    assert!(text.len() < 512, "materialised {} bytes", text.len());
}

/// #1164 P3 F4 — the budget kill must reach the child's DESCENDANTS. A
/// wrapper that backgrounds work is the normal shape of a query CLI, and
/// `Child::kill`/`kill_on_drop` reach only the direct child: the reviewer
/// observed `sleep 30` still alive with PPID 1 after this call returned.
///
/// The assertion is on the grandchild's pid, not on the call's error: the
/// old test passed with the kill deleted entirely.
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

/// Poll the pid a fixture wrote until it is gone, then fail loudly (and
/// clean up) if it never is.
///
/// The pid is the assertion. "the call returned an error in ~200 ms" is
/// not: the previous round's test passed with the kill deleted entirely.
#[cfg(unix)]
async fn assert_recorded_descendant_dies(pidfile: &Path, what: &str) {
    let pid: i32 = std::fs::read_to_string(pidfile)
        .unwrap_or_else(|e| panic!("the fixture must have recorded a descendant pid: {e}"))
        .trim()
        .parse()
        .expect("the recorded pid must parse");
    assert!(pid > 1, "implausible descendant pid {pid}");
    // SAFETY: `kill(pid, 0)` only probes for existence; it delivers no
    // signal and touches no memory.
    let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;

    // The SIGKILL is asynchronous; give the kernel a moment, then insist.
    for _ in 0..100 {
        if !alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Do not leave a 30 s sleep behind if the assertion is about to fail.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    panic!("descendant {pid} survived {what} (it was orphaned onto pid 1)");
}

/// #1164 P3 r2 G3 — the guarantee must hold on the SUCCESS path too.
///
/// A wrapper that backgrounds a daemon with its own stdout and then exits 0
/// escaped the timeout-only kill by simply succeeding: reaped, reported
/// `is_error: false`, and left a process holding every `secret_env` value for
/// as long as it felt like.
///
/// **Mutation witness: delete the `kill_process_group(pgid)` line in phase 4 of
/// `tools_call`.** That is the whole of the steady-state sweep, because
/// `wait_and_release_group` has already disarmed `GroupChild`'s own teardown.
///
/// The round-2 docstring here named a witness that did NOT hold — moving the
/// kill into the budget-expiry arm left this green, because the guard was still
/// armed and `Drop` did the same work microseconds later. This test was
/// therefore a second, weaker witness for `Drop` and said nothing about the
/// step it claimed to cover (r3 H1). Separating release from sweep is what
/// makes the two distinguishable at all.
#[cfg(unix)]
#[tokio::test]
async fn a_backgrounded_daemon_does_not_survive_a_successful_call() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("daemon.pid");
    let p = script(
        tmp.path(),
        "daemonize.sh",
        // stdout/stderr detached, so the pipes reach EOF the moment the
        // wrapper exits — exactly the shape that used to escape.
        "#!/bin/sh\nsleep 30 >/dev/null 2>&1 &\necho $! > \"$1\"\necho ok\nexit 0\n",
    );
    let rt = runtime_for(p, &["{{symbol}}"], 20_000, 4096);
    let res = rt
        .tools_call("quote", json!({ "symbol": pidfile.display().to_string() }))
        .await
        .unwrap();

    // The success verdict must be UNCHANGED by the kill: the leader is
    // already a zombie when the signal lands, so its real exit status is
    // what `wait()` reports.
    assert_eq!(res.is_error, Some(false), "{:?}", res.content);
    assert_eq!(res.content[0].text.as_deref(), Some("ok\n"));

    assert_recorded_descendant_dies(&pidfile, "a successful call").await;
}

/// #1164 P3 r3 H2 — exit-status fidelity for a tool that closes its output and
/// then keeps working.
///
/// `…; exec 1>&- 2>&-; sleep 1; exit 0` reaches EOF on both pipes while it is
/// still alive. Round 2 swept the process group at that moment, which SIGKILLed
/// the leader and reported `signal: 9` / `is_error: true` for what was a
/// perfectly successful call. Reaping before sweeping is what makes the
/// reported status the child's own.
///
/// Mutation witness: move the phase-4 sweep back above the `wait`, i.e. sweep
/// the group before `wait_and_release_group`, and this goes red with
/// `is_error: Some(true)`.
#[cfg(unix)]
#[tokio::test]
async fn a_tool_that_closes_its_output_then_exits_reports_its_real_status() {
    let tmp = tempfile::tempdir().unwrap();
    // Answer first, then detach the pipes and keep running for a beat. The
    // sleep must comfortably outlast the drain returning, or the race the test
    // exists for never happens.
    let p = script(
        tmp.path(),
        "linger.sh",
        "#!/bin/sh\necho answer\nexec 1>&- 2>&-\nsleep 1\nexit 0\n",
    );
    let rt = runtime_for(p, &[], 20_000, 4096);
    let res = rt.tools_call("quote", json!({})).await.unwrap();

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

/// …and the same for a NON-zero exit, so the fix is "report the truth", not
/// "report success". A pre-reap sweep would flatten both of these into
/// `signal: 9`.
#[cfg(unix)]
#[tokio::test]
async fn a_tool_that_closes_its_output_then_fails_reports_its_real_code() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(
        tmp.path(),
        "linger_fail.sh",
        "#!/bin/sh\necho partial\nexec 1>&- 2>&-\nsleep 1\nexit 7\n",
    );
    let rt = runtime_for(p, &[], 20_000, 4096);
    let res = rt.tools_call("quote", json!({})).await.unwrap();
    assert_eq!(res.is_error, Some(true));
    let detail = res.content[1].text.clone().unwrap();
    assert!(
        detail.contains("exit status: 7"),
        "the child's own code must survive: {detail}"
    );
}

/// #1164 P3 r2 G6 — the cancellation guarantee `GroupChild`'s `Drop`
/// advertises had zero coverage: every tested path took the pgid first, so
/// emptying the whole `Drop` body left the suite fully green.
///
/// Here the inner budget is 30 s and an OUTER timeout of 300 ms drops the
/// `tools_call` future — a client hangup or a task abort, which is the only
/// path `Drop` is responsible for. No `wait()` has run, so the sweep provably
/// precedes any reap and the pgid is unambiguous.
///
/// Mutation witness: empty `GroupChild`'s `Drop` body (`self.pgid = None;`).
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
    // Inner budget far longer than the outer one, so the call is CANCELLED
    // rather than expiring — `kill_now` must not be what saves us.
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

/// #1164 P3 r2 G1 — `cap + 1` overflowed. `usize::MAX` used to load from a
/// manifest; in debug it panicked the request task, and in release it
/// wrapped to `take(0)`, which returned an EMPTY answer with
/// `is_error: false` and skipped the drain too.
///
/// The manifest now has a ceiling, so this drives the runtime directly —
/// the arithmetic must be safe for any cap that reaches it.
///
/// Mutation witness: `cap + 1` in `read_capped` and this panics with
/// "attempt to add with overflow".
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

/// …and the same at the real manifest ceiling, driven end to end through
/// `Manifest::parse` → `bring_up` → `tools_call`, because "it loads" is not
/// the property that broke — "it runs" is.
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
    let rt = bring_up("cli-test", parsed.cli_query.as_ref().unwrap(), tmp.path())
        .await
        .unwrap();
    let res = rt.tools_call("q", json!({})).await.unwrap();
    assert_eq!(res.is_error, Some(false));
    assert_eq!(res.content[0].text.as_deref(), Some("hello\n"));

    // …and an over-ceiling value LOADS and is CLAMPED (r3 H7). It must not be
    // refused at parse time: `registry::load_from_dir` re-parses every
    // installed manifest at boot and only `warn!`s past a failure, so a
    // parse-time refusal would make a connector that worked yesterday silently
    // vanish — the exact retroactive-invalidation hazard this module refuses to
    // accept for the `env_allow` denylist.
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
        // …and the clamped connector still runs, i.e. the clamp is what the
        // runtime actually uses rather than a number only the getter knows.
        let rt = bring_up("cli-test", block, tmp.path()).await.unwrap();
        let res = rt.tools_call("q", json!({})).await.unwrap();
        assert_eq!(res.is_error, Some(false));
        assert_eq!(res.content[0].text.as_deref(), Some("hello\n"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_child_that_outlives_its_budget_is_killed_and_named() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(tmp.path(), "slow.sh", "#!/bin/sh\nsleep 30\n");
    let rt = runtime_for(p, &[], 200, 4096);
    let started = std::time::Instant::now();
    let err = rt.tools_call("quote", json!({})).await.unwrap_err();
    let elapsed = started.elapsed();
    // Tight on purpose (r3 H3). The old bound was 10 s for a 200 ms budget,
    // which could not see a whole extra reap grace being spent after expiry —
    // worst-case latency was `timeout_ms + CHILD_REAP_GRACE` while the error
    // text promised `timeout_ms`. Every phase now shares the one deadline, and
    // teardown after expiry is asynchronous, so the overshoot is scheduling
    // noise rather than another constant.
    assert!(
        elapsed < Duration::from_secs(2),
        "the call took {elapsed:?} against a 200 ms budget; some phase is \
         spending time outside cli_query.timeout_ms"
    );
    assert!(err.message.contains("200 ms"), "{}", err.message);
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
    let rt = bring_up("cli-test", &b, tmp.path()).await.unwrap();
    assert_eq!(rt.program(), p.as_path());
    assert_eq!(rt.fingerprint(), "--version: mytool 1.2.3");
}

/// #1164 P3 F5 — the fingerprint probe runs with the BASE environment only.
/// Its stdout is logged verbatim, so a CLI that echoes its configuration on
/// `--version` would otherwise put a `secret_env` value in the log.
///
/// Mutation witness: pass `&env` instead of `&base_child_env(..)` in
/// `bring_up` and the fingerprint becomes `--version: v1 token=sk-secret`.
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
    let rt = bring_up("cli-test", &b, tmp.path()).await.unwrap();
    assert_eq!(
        rt.fingerprint(),
        "--version: v1 token=[]",
        "the probe must run with the base environment only"
    );
    // …while the CALL environment still has it: the probe is restricted,
    // the connector is not broken.
    assert!(rt.env_keys().contains(&"LB_TOKEN"));
}

/// A `--version` that fails must NOT fail bring-up — the fingerprint is
/// informational.
#[cfg(unix)]
#[tokio::test]
async fn a_failing_version_probe_falls_back_instead_of_failing_bring_up() {
    let tmp = tempfile::tempdir().unwrap();
    let p = script(tmp.path(), "novers.sh", "#!/bin/sh\nexit 1\n");
    let b = block(json!({
        "command": p.display().to_string(),
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let rt = bring_up("cli-test", &b, tmp.path()).await.unwrap();
    assert!(
        rt.fingerprint().starts_with("size="),
        "expected the size+mtime fallback, got {}",
        rt.fingerprint()
    );
}

/// #1164 P3 r2 G7 — a `--version` that HANGS must cost
/// [`VERSION_PROBE_BUDGET`] and then fall back, not consume the whole
/// bring-up. Only `exit 1` was covered before, which the outer budget would
/// have masked entirely.
///
/// Mutation witness: raise `VERSION_PROBE_BUDGET` above
/// [`CLI_QUERY_BRINGUP_BUDGET`] and this goes red on the elapsed assertion
/// — nothing else in the suite distinguishes the two budgets.
#[cfg(unix)]
#[tokio::test]
async fn a_hanging_version_probe_costs_the_sub_budget_and_falls_back() {
    // The relationship that matters is the TOTAL the probe can cost, not one
    // term of it (r3 H3). Round 2 asserted only this comparison while the probe
    // ALSO spent a 5 s reap grace afterwards: 2 + 5 > 5, so a probe wedged in
    // uninterruptible I/O took 7 s, the outer budget fired first, and the
    // connector went `Unavailable` — precisely what the sub-budget exists to
    // prevent. The measured assertion at the end is the real guard.
    assert!(
        VERSION_PROBE_BUDGET < CLI_QUERY_BRINGUP_BUDGET,
        "the sub-budget must be strictly smaller, or a hung probe takes the enable down"
    );
    let tmp = tempfile::tempdir().unwrap();
    let p = script(tmp.path(), "hang.sh", "#!/bin/sh\nsleep 60\n");
    let b = block(json!({
        "command": p.display().to_string(),
        "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
    }));
    let started = std::time::Instant::now();
    let rt = bring_up("cli-test", &b, tmp.path()).await.unwrap();
    let elapsed = started.elapsed();

    assert!(
        rt.fingerprint().starts_with("size="),
        "a hung probe must fall back, got {}",
        rt.fingerprint()
    );
    // The TOTAL, measured. Every probe phase shares one deadline, so a hung
    // `--version` costs the sub-budget plus scheduling noise — not the
    // sub-budget plus a per-phase grace. A term added anywhere in the probe
    // lifecycle shows up here.
    assert!(
        elapsed < VERSION_PROBE_BUDGET + Duration::from_secs(1),
        "bring-up took {elapsed:?}; the WHOLE probe must fit inside its \
         {VERSION_PROBE_BUDGET:?} sub-budget, and some phase is spending time \
         outside it"
    );
    assert!(
        elapsed < CLI_QUERY_BRINGUP_BUDGET,
        "bring-up took {elapsed:?}; a hung probe must never reach the outer budget"
    );
}

/// #1164 P3 r2 G5 — resolution only checks that SOME execute bit is set, so
/// a file we cannot actually exec resolves, enables, publishes as `Running`
/// and then fails every single call.
///
/// Two spawn failures, both reachable without root: mode `0o011` gives
/// group/other the execute bit but not the owner, and Linux checks the
/// owner class first (`EACCES`); a `#!` line naming an interpreter that
/// does not exist is `ENOENT` even though the file itself is right there.
///
/// **`ENOEXEC` is deliberately not one of the cases.** Measured, not
/// assumed: Rust's `Command::spawn` always goes through `execvp`, and
/// glibc's `execvp` implements the POSIX `ENOEXEC` fallback — a text file
/// with no shebang and no ELF header is silently re-exec'd under `/bin/sh`,
/// so the spawn SUCCEEDS and the file merely exits 127. It therefore lands
/// in the informational arm, and a connector pointed at garbage still
/// enables. That residual stands on purpose: a non-zero `--version` cannot
/// be a bring-up failure, because a CLI is entitled not to have one.
///
/// Mutation witness: map the spawn error back onto the size+mtime fallback
/// and this goes red while `a_failing_version_probe_falls_back…` stays
/// green — the two arms must not collapse into one.
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
        let err = bring_up("cli-test", &b, tmp.path())
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

/// #1164 P3 r3 H6 — only a FILE-shaped spawn failure may refuse an enable.
///
/// `fork`/`execve` also fails for reasons that are about the machine, not the
/// binary: `EAGAIN`/`ENOMEM` under `RLIMIT_NPROC` or memory pressure,
/// `EMFILE`/`ENFILE` on descriptor exhaustion, `ETXTBSY` while an upgrade
/// rewrites the file. Bring-up runs inline at boot while every other connector
/// is spawning, so fork pressure there is expected — and refusing on it would
/// leave a perfectly good connector permanently `Unavailable` with nothing to
/// retry it, a regression against the pre-round-2 behaviour.
///
/// Driven through the real classifier rather than a re-implementation of it, so
/// the table below is the production decision.
///
/// Mutation witness: widen `is_permanent_spawn_failure` to `true` (or add
/// `WouldBlock`/`OutOfMemory` to it) and the transient rows go red.
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
    // EMFILE/ENFILE have no stable `ErrorKind` mapping across releases, so
    // assert on the raw errno the kernel actually returns.
    for errno in [libc::EMFILE, libc::ENFILE, libc::EAGAIN] {
        assert!(
            !is_permanent_spawn_failure(&std::io::Error::from_raw_os_error(errno)),
            "errno {errno} is transient"
        );
    }
}

/// #1164 P3 r2 G2 — the child's PATH must carry the same absoluteness rule
/// resolution applies. Refusing to PIN `.` and then exec'ing the child with
/// `PATH=".:…"` just moves the problem: a query CLI that shells out to
/// `git`/`jq` resolves it against the server's working directory, with this
/// connector's secrets already in its environment.
///
/// Asserted on the FINAL child environment, not on `resolve_command`: the
/// existing resolution test passed throughout this defect.
///
/// Mutation witness: drop the `is_absolute` filter from
/// `per_connector_path` and this goes red.
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
    let rt = bring_up("cli-test", &b, tmp.path()).await.unwrap();

    let path = rt.child_path();
    for entry in path.split(':') {
        assert!(
            Path::new(entry).is_absolute(),
            "the child's PATH carries the relative entry {entry:?}: {path}"
        );
    }
    // …and the absolute extra survived, so the filter is not "drop
    // everything".
    assert!(
        path.split(':').any(|e| e == "/opt/lb/bin"),
        "the absolute extra must still be first-class: {path}"
    );
}

/// #1164 P3 F7 — `std::env::vars()` PANICS on a non-UTF-8 variable, which
/// would turn every `cli-query` enable into a panic on the boot path.
///
/// Deterministic despite touching the process environment: nextest runs
/// each test in its own process, and the variable is set before any runtime
/// thread exists, so nothing else can observe it.
///
/// Mutation witness: `std::env::vars()` in `bring_up` and this panics with
/// "environment variable was not valid unicode".
#[cfg(unix)]
#[test]
fn a_non_utf8_service_env_variable_does_not_panic_bring_up() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: `set_var` is sound only with no concurrent reader of `environ`,
    // and nothing here is concurrent yet — no runtime has been built.
    //
    // The CROSS-TEST half of that requirement is not free: every sibling
    // `bring_up` test calls `std::env::vars_os()`, so under a plain
    // `cargo test --lib` (one process, many threads) this would race an
    // `environ` realloc against a live reader — real UB, not a style
    // objection. It is sound because the repo's gate is `cargo nextest`, which
    // runs each test in its own process. If that ever changes, this test moves
    // to its own integration binary rather than losing the assertion (r3 H8).
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
            // Named in `env_allow` ON PURPOSE. Without it the assertions below
            // could not fail: with no `env_allow` the child environment is
            // {PATH, HOME, LANG} by construction, so "the bad key is absent"
            // was true of every possible implementation (r3 H8).
            "env_allow": ["CLI_QUERY_BAD_VALUE", "CLI_QUERY_PLAIN_OK"],
            "command": p.display().to_string(),
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));
        let rt = bring_up("cli-test", &b, tmp.path())
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
        // … and an ordinary allowlisted key IS still forwarded, so the filter
        // is not simply "drop everything".
        assert!(
            rt.env_keys().contains(&"CLI_QUERY_PLAIN_OK"),
            "a decodable env_allow key must still be forwarded: {:?}",
            rt.env_keys()
        );
    });
}

/// Small helper so the missing-slot loop can report WHICH input silently
/// succeeded rather than panicking with no context.
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
