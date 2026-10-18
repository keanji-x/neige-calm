#[test]
fn optional_bound_native_declaration() {
    let entry = crate::builtin::get(PLUGIN_ID).expect("compiled history component");
    assert!(entry.can_disable());
    assert_eq!(
        entry.manifest().agent_tools_scope,
        crate::manifest::AgentToolsScope::BoundTrack
    );
    assert_eq!(entry.tools().len(), 4);
}
use super::*;
use serde_json::json;
mod fixture;
use fixture::*;

#[test]
fn official_contract_query_auth_and_four_capabilities() {
    let fx = Fixture::new();
    assert_eq!(
        fx.call("source_ls").unwrap()["sources"][0]["readiness"],
        "configured_unverified"
    );
    assert!(fx.requests.lock().unwrap().is_empty());
    assert_eq!(
        fx.call("session_ls").unwrap()["items"][0]["id"],
        "ses_allowed"
    );
    assert_eq!(
        fx.call("message_ls").unwrap()["messages"][0]["parts"][0],
        part()
    );
    assert_eq!(
        fx.call("message_cat").unwrap()["messages"][0]["info"]["id"],
        "msg_one"
    );
    let req = fx.requests.lock().unwrap();
    assert!(
        req.iter()
            .all(|r| r.contains("Authorization: Basic b3BlbmNvZGU6c3ludGhldGljLXBhc3N3b3Jk"))
    );
    let list = req.iter().find(|r| r.starts_with("GET /session?")).unwrap();
    for q in ["directory=%2Fwork%2Fallowed", "roots=true", "limit=2"] {
        assert!(list.contains(q), "{list}");
    }
    assert!(!list.contains("scope="));
}
#[test]
fn scope_identity_rejects_entire_session_page() {
    let fx = Fixture::new();
    let mut denied = session();
    denied["directory"] = json!("/work/denied");
    denied["id"] = json!("ses_SECRET");
    fx.set("/session", json!([session(), denied]));
    let error = fx.call("session_ls").unwrap_err();
    assert_eq!(error, "scope: identity mismatch");
    assert!(!error.contains("SECRET"));
}
#[test]
fn selected_session_identity_fences_before_message_request() {
    for key in ["id", "projectID", "directory"] {
        let fx = Fixture::new();
        let mut changed = session();
        changed[key] = json!("different");
        fx.set("/session/ses_allowed", changed);
        assert!(fx.call("message_ls").is_err());
        assert_eq!(fx.requests.lock().unwrap().len(), 5);
    }
}
#[test]
fn project_identity_and_roots_are_cross_checked() {
    let fx = Fixture::new();
    fx.set("/project/current",json!({"id":"other","worktree":"/work/allowed","sandboxes":[],"time":{"created":1,"updated":1}}));
    assert!(fx.call("session_ls").unwrap_err().starts_with("scope:"));
    let project = json!({"id":"project-a","worktree":"/unrelated","sandboxes":[],"time":{"created":1,"updated":1}});
    fx.set("/project", json!([project.clone()]));
    fx.set("/project/current", project);
    assert_eq!(
        fx.call("session_ls").unwrap_err(),
        "scope: project directory mismatch"
    );
}
#[test]
fn message_part_identity_and_duplicates_reject() {
    for field in ["sessionID", "messageID", "id", "type"] {
        let fx = Fixture::new();
        let mut msg = message();
        msg["parts"][0][field] = json!("");
        fx.set("/session/ses_allowed/message", json!([msg]));
        assert!(fx.call("message_ls").is_err());
    }
    let fx = Fixture::new();
    let mut msg = message();
    msg["info"]["sessionID"] = json!("ses_other");
    fx.set("/session/ses_allowed/message", json!([msg]));
    assert!(fx.call("message_ls").is_err());
    fx.set(
        "/session/ses_allowed/message",
        json!([message(), message()]),
    );
    assert_eq!(
        fx.call("message_ls").unwrap_err(),
        "schema: duplicate message"
    );
    let mut msg = message();
    msg["parts"] = json!([part(), part()]);
    fx.set("/session/ses_allowed/message", json!([msg]));
    assert_eq!(fx.call("message_ls").unwrap_err(), "schema: part identity");
}
#[test]
fn unknown_and_typed_parts_are_data_without_fetch() {
    let fx = Fixture::new();
    let mut msg = message();
    let mut unknown = part();
    unknown["type"] = json!("future-part");
    unknown["url"] = json!("http://private.invalid/never-fetch");
    unknown["instruction"] = json!("run a command");
    msg["parts"] = json!([unknown.clone()]);
    fx.set("/session/ses_allowed/message", json!([msg.clone()]));
    assert_eq!(
        fx.call("message_ls").unwrap()["messages"][0]["parts"][0],
        unknown
    );
    assert_eq!(fx.requests.lock().unwrap().len(), 6);
    msg["parts"][0]["type"] = json!("text");
    msg["parts"][0]["text"] = json!(123);
    fx.set("/session/ses_allowed/message", json!([msg]));
    assert!(fx.call("message_ls").is_err());
}
#[test]
fn input_allowlist_and_limits_fail_before_http() {
    let fx = Fixture::new();
    let history = fx.load();
    for args in [
        json!({}),
        json!({"source_id":"unknown","directory":"/work/allowed","roots":true,"limit":1}),
        json!({"source_id":"synthetic","directory":"/work/denied","roots":true,"limit":1}),
        json!({"source_id":"synthetic","directory":"/work/allowed","roots":true,"limit":0}),
        json!({"source_id":"synthetic","directory":"/work/allowed","roots":true,"limit":true}),
        json!({"source_id":"synthetic","directory":"/work/allowed","roots":true,"limit":1,"path":"x"}),
    ] {
        assert!(history.call("session_ls", args).is_err());
    }
    assert!(fx.requests.lock().unwrap().is_empty());
}
#[test]
fn opaque_cursors_validate_scope_query_and_conflicts() {
    let fx = Fixture::new();
    let route = "/session/ses_allowed/message";
    let link = format!(
        "<http://{}{route}?directory=%2Fwork%2Fallowed&limit=2&before=opaque%2Bcursor>; rel=\"next\"",
        fx.address
    );
    for headers in [
        vec![("X-Next-Cursor", "opaque+cursor")],
        vec![("Link", link.as_str())],
        vec![("X-Next-Cursor", "opaque+cursor"), ("Link", link.as_str())],
    ] {
        fx.replies.lock().unwrap().get_mut(route).unwrap().headers = headers
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        assert_eq!(
            fx.call("message_ls").unwrap()["next_cursor"],
            "opaque+cursor"
        );
    }
    for headers in [
        vec![("X-Next-Cursor", "a"), ("X-Next-Cursor", "b")],
        vec![("X-Next-Cursor", "")],
        vec![("Link", "<http://different.invalid/path>; rel=\"next\"")],
        vec![("Link", "not a link")],
        vec![("X-Next-Cursor", "different"), ("Link", link.as_str())],
    ] {
        fx.replies.lock().unwrap().get_mut(route).unwrap().headers = headers
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        assert!(fx.call("message_ls").is_err());
    }
}
#[test]
fn contract_version_schema_http_errors_are_explicit() {
    for status in [301, 302, 307, 401, 403, 404, 500] {
        let fx = Fixture::new();
        {
            let mut replies = fx.replies.lock().unwrap();
            let reply = replies.get_mut("/global/health").unwrap();
            reply.status = status;
            reply
                .headers
                .push(("Location".into(), format!("http://{}/session", fx.address)));
        }
        assert!(fx.call("message_ls").unwrap_err().starts_with("http:"));
        assert_eq!(fx.requests.lock().unwrap().len(), 1);
    }
    let fx = Fixture::new();
    fx.set("/global/health", json!({"healthy":true,"version":"next"}));
    assert!(fx.call("message_ls").unwrap_err().starts_with("contract:"));
    fx.set("/global/health", json!({"healthy":true,"version":"1.19.3"}));
    let mut doc = contract::DOC.clone();
    doc["paths"]["/session"]["get"]["operationId"] = json!("changed");
    fx.set("/doc", doc);
    assert_eq!(
        fx.call("message_ls").unwrap_err(),
        "contract: OpenAPI mismatch"
    );
}
#[test]
fn response_encoding_json_budget_and_timeout_fail() {
    let mut fx = Fixture::new();
    fx.config["limits"]["max_response_bytes"] = json!(10);
    assert!(
        fx.call("message_ls")
            .unwrap_err()
            .contains("response budget")
    );
    fx.config["limits"]["max_response_bytes"] = json!(2097152);
    fx.config["limits"]["request_timeout_ms"] = json!(1);
    fx.replies
        .lock()
        .unwrap()
        .get_mut("/global/health")
        .unwrap()
        .delay = true;
    assert_eq!(
        fx.call("message_ls").unwrap_err(),
        "http: source unavailable"
    );
    fx.replies
        .lock()
        .unwrap()
        .get_mut("/global/health")
        .unwrap()
        .delay = false;
    fx.config["limits"]["request_timeout_ms"] = json!(1000);
    fx.replies
        .lock()
        .unwrap()
        .get_mut("/global/health")
        .unwrap()
        .raw = Some("{\"healthy\":true,\"healthy\":false}".into());
    assert!(fx.call("message_ls").unwrap_err().starts_with("json:"));
}
#[test]
fn evidence_budget_digest_escaping_and_omissions_are_exact() {
    use sha2::{Digest, Sha256};
    let mut fx = Fixture::new();
    let mut msg = message();
    let mut big = part();
    big["id"] = json!("prt_big");
    big["text"] = json!("x".repeat(1000));
    msg["parts"] = json!([part(), big]);
    fx.set("/session/ses_allowed/message", json!([msg]));
    fx.config["limits"]["max_part_bytes"] = json!(300);
    let mut e = fx.call("message_ls").unwrap();
    assert_eq!(e["coverage"]["omitted_parts"], 1);
    assert_eq!(e["coverage"]["retained_parts"], 1);
    assert_eq!(e["messages"][0]["omissions"][0]["reason"], "part_budget");
    let d = e.as_object_mut().unwrap().remove("digest").unwrap();
    assert_eq!(
        d["value"],
        hex::encode(Sha256::digest(e.to_string().as_bytes()))
    );
    fx.config["limits"]["max_result_bytes"] = json!(1800);
    let r = fx
        .load()
        .call("message_ls", Fixture::args("message_ls"))
        .unwrap();
    assert!(serde_json::to_vec(&r).unwrap().len() <= 1800);
    fx.config["limits"]["max_result_bytes"] = json!(1);
    assert!(fx.call("message_ls").unwrap_err().starts_with("budget:"));
}
#[test]
fn typed_config_rejects_invalid_fields_limits_paths_and_duplicates() {
    let fx = Fixture::new();
    assert!(History::load(json!({})).is_err());
    assert!(History::load(json!({"history_config_path":"relative"})).is_err());
    for key in ["sources", "credentials", "limits"] {
        let mut c = fx.config.clone();
        c.as_object_mut().unwrap().remove(key);
        std::fs::write(
            fx.values["history_config_path"].as_str().unwrap(),
            c.to_string(),
        )
        .unwrap();
        assert!(History::load(fx.values.clone()).is_err());
    }
    for (key, cap) in [
        ("page_messages", 50),
        ("max_response_bytes", 2097152),
        ("max_result_bytes", 131072),
        ("max_parts", 128),
        ("max_part_bytes", 16384),
        ("request_timeout_ms", 10000),
    ] {
        for bad in [
            json!(0),
            json!(cap + 1),
            json!(u64::MAX),
            json!(true),
            json!(1.5),
            json!("1"),
            Value::Null,
        ] {
            let mut c = fx.config.clone();
            c["limits"][key] = bad;
            std::fs::write(
                fx.values["history_config_path"].as_str().unwrap(),
                c.to_string(),
            )
            .unwrap();
            assert!(History::load(fx.values.clone()).is_err(), "{key}");
        }
    }
    for raw in [
        "{} trailing",
        "{\"a\":1,\"a\":2}",
        "{\"x\":{\"a\":1,\"a\":2}}",
        "{\"x\":NaN}",
    ] {
        assert!(config::decode(raw.as_bytes()).is_err());
    }
    assert!(config::decode(&[255]).is_err());
}
#[test]
fn typed_sources_credentials_and_origins_are_strict() {
    let mut fx = Fixture::new();
    for origin in [
        "http://localhost/",
        "http://u:p@localhost",
        "http://localhost?",
        "http://localhost#",
        "http://localhost/path/..",
        "file:///a",
        "http://localhost:0",
        "http://localhost\\path",
    ] {
        fx.config["sources"][0]["base_url"] = json!(origin);
        fx.write();
        assert!(History::load(fx.values.clone()).is_err(), "{origin}");
    }
    fx.config["sources"][0]["base_url"] = json!(format!("http://{}", fx.address));
    for dirs in [
        json!([]),
        json!(["relative"]),
        json!(["/work/a", "/work/a"]),
        json!(["/work/\u{0}"]),
    ] {
        fx.config["sources"][0]["directories"] = dirs;
        fx.write();
        assert!(History::load(fx.values.clone()).is_err());
    }
    fx.config["sources"][0]["directories"] = json!(["/work/allowed"]);
    fx.config["sources"][0]["credential_ref"] = json!("unresolved");
    fx.write();
    assert!(History::load(fx.values.clone()).is_err());
}
#[test]
#[cfg(unix)]
fn credential_opened_metadata_and_snapshot() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let history = fx.load();
    let password = fx.tmp.path().join("password");
    std::fs::write(&password, "changed").unwrap();
    let result = history
        .call("message_ls", Fixture::args("message_ls"))
        .unwrap();
    assert!(!serde_json::to_string(&result).unwrap().contains("password"));
    assert!(
        fx.requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.contains("b3BlbmNvZGU6c3ludGhldGljLXBhc3N3b3Jk"))
    );
    fx.requests.lock().unwrap().clear();
    fx.call("message_ls").unwrap();
    assert!(
        fx.requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| !r.contains("b3BlbmNvZGU6c3ludGhldGljLXBhc3N3b3Jk"))
    );
    for bytes in [vec![], vec![255], vec![b'a'; 4097], vec![0]] {
        std::fs::write(&password, bytes).unwrap();
        assert!(History::load(fx.values.clone()).is_err());
    }
    std::fs::write(&password, "password").unwrap();
    std::fs::set_permissions(&password, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(History::load(fx.values.clone()).is_err());
}

#[test]
fn file_tool_compaction_parts_remain_typed_inert_data() {
    let fx = Fixture::new();
    let mut msg = message();
    let mut parts = vec![part()];
    for (id, kind, extra) in [
        (
            "prt_file",
            "file",
            json!({"mime":"text/plain","url":"https://do-not-fetch.invalid/private"}),
        ),
        ("prt_compact", "compaction", json!({"auto":true})),
        (
            "prt_tool",
            "tool",
            json!({"callID":"call_1","tool":"shell","state":{"status":"completed","input":{"cmd":"never execute"},"output":"synthetic","title":"tool result","metadata":{},"time":{"start":1,"end":2}}}),
        ),
    ] {
        let mut p = part();
        p.as_object_mut().unwrap().remove("text");
        p["id"] = json!(id);
        p["type"] = json!(kind);
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        parts.push(p);
    }
    msg["parts"] = json!(parts);
    fx.set("/session/ses_allowed/message", json!([msg]));
    assert_eq!(
        fx.call("message_ls").unwrap()["messages"][0]["parts"],
        json!(parts)
    );
    assert_eq!(fx.requests.lock().unwrap().len(), 6);
}
#[test]
fn result_budget_retains_truncation_and_total_part_coverage() {
    let mut fx = Fixture::new();
    fx.config["limits"]["max_result_bytes"] = json!(1900);
    let mut selected = session();
    selected["title"] = json!("中文\\\"\n".repeat(500));
    fx.set("/session", json!([selected]));
    let mut args = Fixture::args("session_ls");
    args["limit"] = json!(1);
    let result = fx.load().call("session_ls", args).unwrap();
    assert!(serde_json::to_vec(&result).unwrap().len() <= 1900);
    let out: Value = serde_json::from_str(
        serde_json::to_value(result).unwrap()["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(out["items"], json!([]));
    assert_eq!(out["coverage"]["omitted_items"], 1);
    assert_eq!(out["possibly_truncated"], true);
    fx.config["limits"]["max_result_bytes"] = json!(131072);
    fx.config["limits"]["max_parts"] = json!(1);
    let mut msg = message();
    let mut p = part();
    p["id"] = json!("prt_overflow");
    msg["parts"].as_array_mut().unwrap().push(p.clone());
    fx.set("/session/ses_allowed/message", json!([msg]));
    let out = fx.call("message_ls").unwrap();
    assert_eq!(out["coverage"]["retained_parts"], 1);
    assert_eq!(out["coverage"]["omitted_parts"], 1);
    assert_eq!(
        out["coverage"]["omitted_part_bytes"],
        serde_json::to_vec(&p).unwrap().len()
    );
}

#[test]
fn config_source_fields_and_referenced_credentials_are_complete() {
    let fx = Fixture::new();
    for (field, bad) in [
        ("sources", json!([])),
        (
            "sources",
            json!([
                fx.config["sources"][0].clone(),
                fx.config["sources"][0].clone()
            ]),
        ),
        (
            "credentials",
            json!({"local":{"username":"user:bad","password_file":"/synthetic"}}),
        ),
    ] {
        let mut c = fx.config.clone();
        c[field] = bad;
        std::fs::write(
            fx.values["history_config_path"].as_str().unwrap(),
            c.to_string(),
        )
        .unwrap();
        assert!(History::load(fx.values.clone()).is_err());
    }
    let mut c = fx.config.clone();
    c["credentials"]["unreferenced"] =
        json!({"username":"valid","password_file":"/nonexistent/synthetic/password"});
    std::fs::write(
        fx.values["history_config_path"].as_str().unwrap(),
        c.to_string(),
    )
    .unwrap();
    assert!(History::load(fx.values.clone()).is_ok());
    for (pointer, bad) in [
        ("/sources/0/source_id", json!("")),
        ("/sources/0/extra", json!("unknown")),
        ("/credentials/local/extra", json!(true)),
        ("/limits/extra", json!(1)),
    ] {
        let mut c = fx.config.clone();
        if let Some(v) = c.pointer_mut(pointer) {
            *v = bad;
        } else {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            c.pointer_mut(parent).unwrap()[key] = bad;
        }
        std::fs::write(
            fx.values["history_config_path"].as_str().unwrap(),
            c.to_string(),
        )
        .unwrap();
        assert!(History::load(fx.values.clone()).is_err(), "{pointer}");
    }
}
#[test]
#[cfg(unix)]
fn credential_wrong_owner_when_namespace_allows_chown() {
    use std::os::unix::ffi::OsStrExt;
    let fx = Fixture::new();
    let password = fx.tmp.path().join("password");
    let path = std::ffi::CString::new(password.as_os_str().as_bytes()).unwrap();
    // SAFETY: the C string is valid; target is this test's temporary synthetic file.
    let changed = unsafe { libc::chown(path.as_ptr(), libc::getuid().wrapping_add(1), u32::MAX) };
    if changed != 0 {
        eprintln!(
            "SKIP wrong-owner OS case: namespace denies chown ({})",
            std::io::Error::last_os_error()
        );
        return;
    }
    assert_eq!(
        History::load(fx.values.clone()).err().unwrap(),
        "config: credential permissions"
    );
}
#[test]
#[cfg(unix)]
fn nonregular_config_and_credential_files_refuse_without_blocking() {
    use std::os::unix::ffi::OsStrExt;
    let fx = Fixture::new();
    let fifo = fx.tmp.path().join("fifo");
    let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: the C string names a fresh file in this temporary test directory.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(History::load(json!({"history_config_path":fifo})).is_err());
    assert!(History::load(json!({"history_config_path":fx.tmp.path()})).is_err());
    let mut c = fx.config.clone();
    c["credentials"]["local"]["password_file"] = json!(fifo);
    std::fs::write(
        fx.values["history_config_path"].as_str().unwrap(),
        c.to_string(),
    )
    .unwrap();
    assert!(History::load(fx.values.clone()).is_err());
}

#[test]
fn ids_are_encoded_as_single_path_segments() {
    let fx = Fixture::new();
    let sid = "ses_a/b?c#d";
    let mut selected = session();
    selected["id"] = json!(sid);
    fx.set("/session/ses_a%2Fb%3Fc%23d", selected);
    let mut msg = message();
    msg["info"]["sessionID"] = json!(sid);
    msg["parts"][0]["sessionID"] = json!(sid);
    fx.set("/session/ses_a%2Fb%3Fc%23d/message", json!([msg]));
    let mut args = Fixture::args("message_ls");
    args["session_id"] = json!(sid);
    let result = fx.load().call("message_ls", args).unwrap();
    assert!(
        serde_json::to_value(result).unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(sid)
    );
    assert!(
        fx.requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.starts_with("GET /session/ses_a%2Fb%3Fc%23d/message?"))
    );
}

#[test]
fn session_dedup_preserves_upstream_order_and_coverage() {
    let fx = Fixture::new();
    let mut first = session();
    first["id"] = json!("ses_z");
    let mut second = session();
    second["id"] = json!("ses_a");
    let mut last = first.clone();
    last["title"] = json!("latest duplicate");
    fx.set("/session", json!([first, second, last]));
    let mut args = Fixture::args("session_ls");
    args["limit"] = json!(3);
    let r = fx.load().call("session_ls", args).unwrap();
    let wire = serde_json::to_value(r).unwrap();
    let e: Value = serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(e["items"][0]["id"], "ses_z");
    assert_eq!(e["items"][0]["title"], "latest duplicate");
    assert_eq!(e["items"][1]["id"], "ses_a");
    assert_eq!(e["coverage"]["deduplicated_items"], 1);
    assert_eq!(e["possibly_truncated"], true);
}
