//! #1704 S2 — the Track tree's Claude permission policy as the ceiling of
//! `calm.terminal.open`, through the real MCP tools, the real operation
//! runtime, a real PTY and the production REST router (the user's PATCH).
use crate::terminal_support::Harness;
use calm_server::db::prelude::*;
use calm_server::model::NewTrack;
use calm_server::terminal_permissions::{
    CeilingCheckedHook, install_ceiling_checked_hook_for_test,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use tower::ServiceExt;

/// Prints `READY <card id>` and keeps the PTY open; the settings file is read
/// from disk by the test, not by the fake.
const FAKE_CLAUDE: &str = "printf 'READY %s\\n' \"$NEIGE_CARD_ID\"; exec cat";

const FLOOR_ASK: [&str; 7] = [
    "Bash(git push *)",
    "Bash(git reset --hard *)",
    "Bash(rm -rf *)",
    "Bash(curl *)",
    "Bash(wget *)",
    "Bash(pip install *)",
    "Bash(npm install *)",
];

fn policy() -> Value {
    json!({
        "edit": ["src/**", "tests/**"],
        "bash": ["git", "python3 -m unittest"],
        "deny": ["git rebase"]
    })
}

/// The block the kernel renders for `scope` in `cwd` (S1's rendering).
fn block(cwd: &str, scope: &Value) -> Value {
    let root = cwd.trim_matches('/');
    let strings = |key: &str| -> Vec<String> {
        scope[key]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut allow: Vec<String> = strings("edit")
        .iter()
        .map(|glob| format!("Edit(//{root}/{glob})"))
        .collect();
    allow.extend(strings("bash").iter().map(|p| format!("Bash({p} *)")));
    let mut ask: Vec<String> = FLOOR_ASK.iter().map(|s| (*s).to_owned()).collect();
    ask.push(format!("Edit(//{root}/.git/**)"));
    let deny: Vec<String> = strings("deny")
        .iter()
        .map(|p| format!("Bash({p} *)"))
        .collect();
    json!({"allow": allow, "ask": ask, "deny": deny})
}

async fn patch_policy(h: &Harness, track_id: &str, policy: Value) {
    let response = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("PATCH")
                .uri(format!("/api/tracks/{track_id}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"claude_permissions_policy": policy}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}

fn open_args(request_id: &str, scope: Option<Value>) -> Value {
    let mut args = json!({"program": FAKE_CLAUDE, "request_id": request_id, "claim": true});
    if let Some(scope) = scope {
        args["claude_permissions"] = scope;
    }
    args
}

fn receipt(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    &response["result"]["structuredContent"]
}

struct Opened {
    response: Value,
    terminal: String,
    card_id: String,
    cwd: String,
    settings_path: PathBuf,
    settings: Value,
    settings_text: String,
}

async fn open_ok(h: &Harness, token: Option<&str>, args: Value) -> Opened {
    let response = match token {
        Some(token) => h.call_with_token(token, "calm.terminal.open", args).await,
        None => h.call("calm.terminal.open", args).await,
    };
    let opened = receipt(&response).clone();
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let card_id = opened["card_id"].as_str().unwrap().to_owned();
    let term = h.state.repo.terminal_get(&terminal).await.unwrap().unwrap();
    let settings_path = PathBuf::from(term.env["NEIGE_CLAUDE_SETTINGS"].as_str().unwrap());
    let settings_text = std::fs::read_to_string(&settings_path).unwrap();
    let settings: Value = serde_json::from_str(&settings_text).unwrap();
    Opened {
        response,
        terminal,
        card_id,
        cwd: term.cwd,
        settings_path,
        settings,
        settings_text,
    }
}

fn summary(response: &Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
}

async fn terminal_cards(h: &Harness, track: &str) -> usize {
    h.state
        .repo
        .cards_by_track(track)
        .await
        .unwrap()
        .iter()
        .filter(|c| c.kind == "terminal")
        .count()
}

/// The stamped card, the file, the receipt and its summary agree on the
/// block and its source.
async fn assert_effective(h: &Harness, opened: &Opened, expected: &Value, source: &str) {
    assert_eq!(
        opened.settings["permissions"], *expected,
        "{}",
        opened.settings_text
    );
    assert_eq!(
        opened
            .settings
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["hooks", "permissions"]
    );
    let card = h
        .state
        .repo
        .card_get(&opened.card_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        card.payload["claude_permissions"], *expected,
        "{}",
        card.payload
    );
    assert_eq!(
        card.payload["claude_permissions_source"], source,
        "{}",
        card.payload
    );
    let result = receipt(&opened.response);
    assert_eq!(result["claude_permissions"], *expected, "{result}");
    assert_eq!(result["claude_permissions_source"], source, "{result}");
    let line = summary(&opened.response);
    println!(
        "RECEIPT source={source} claude_permissions={} summary={line}",
        result["claude_permissions"]
    );
    assert!(
        line.ends_with(&format!(
            " permissions allow {} ask {} deny {} source {source}; full state in structuredContent",
            expected["allow"].as_array().unwrap().len(),
            expected["ask"].as_array().unwrap().len(),
            expected["deny"].as_array().unwrap().len()
        )),
        "{line}"
    );
}

/// (a) policy, no declaration: the policy is the scope (`track_policy`);
/// a scope-less replay returns the same terminal. (b) policy + `{deny}`: the
/// policy's allow, the deny appended (`declared_within_policy`). (b') policy
/// + `{bash}`: `edit` inherited. (c) an exceeding declaration is refused by
/// name with the ceiling, before any create. (d) no policy + declaration:
/// S1's block, source `declared`. (e) a REST terminal card under a policy
/// gets no file and no keys. (f) the third key is server-owned and sticky.
#[tokio::test]
async fn policy_is_the_ceiling_of_every_planner_open() {
    let h = Harness::start().await;

    // (d) before any policy: the declaration alone, source `declared`.
    let declared = json!({"edit": ["**"], "deny": ["git push"]});
    let d = open_ok(&h, None, open_args("declared", Some(declared.clone()))).await;
    assert!(d.cwd.starts_with('/'), "{}", d.cwd);
    assert_effective(&h, &d, &block(&d.cwd, &declared), "declared").await;

    patch_policy(&h, &h.track, policy()).await;

    // (a)
    let a = open_ok(&h, None, open_args("policy-only", None)).await;
    assert_effective(&h, &a, &block(&a.cwd, &policy()), "track_policy").await;
    let replay = receipt(
        &h.call("calm.terminal.open", open_args("policy-only", None))
            .await,
    )
    .clone();
    assert_eq!(replay["terminal_id"], a.terminal);
    assert_eq!(replay["card_id"], a.card_id);
    assert_eq!(replay["claude_permissions_source"], "track_policy");
    assert_eq!(
        std::fs::read_to_string(&a.settings_path).unwrap(),
        a.settings_text,
        "the settings file is untouched by the replay"
    );

    // (b)
    let b = open_ok(
        &h,
        None,
        open_args("narrow-deny", Some(json!({"deny": ["git push"]}))),
    )
    .await;
    let mut merged = policy();
    merged["deny"] = json!(["git rebase", "git push"]);
    assert_effective(&h, &b, &block(&b.cwd, &merged), "declared_within_policy").await;

    // (b')
    let b2 = open_ok(
        &h,
        None,
        open_args(
            "narrow-bash",
            Some(json!({"bash": ["git status", "python3 -m unittest discover"]})),
        ),
    )
    .await;
    let mut merged = policy();
    merged["bash"] = json!(["git status", "python3 -m unittest discover"]);
    assert_effective(&h, &b2, &block(&b2.cwd, &merged), "declared_within_policy").await;
    let cards_before = terminal_cards(&h, &h.track).await;

    // (c)
    for (scope, reason) in [
        (
            json!({"edit": ["**"]}),
            "claude_permissions.edit[0] '**' exceeds the Track policy (edit: src/**, tests/**)",
        ),
        (
            json!({"bash": ["git status", "cargo test"]}),
            "claude_permissions.bash[1] 'cargo test' exceeds the Track policy (bash: git, python3 -m unittest)",
        ),
        (
            json!({"bash": ["git rebase -i"]}),
            "claude_permissions.bash[0] 'git rebase -i' is denied by the Track policy (deny: git rebase)",
        ),
    ] {
        let response = h
            .call(
                "calm.terminal.open",
                open_args("exceeds", Some(scope.clone())),
            )
            .await;
        assert_eq!(response["error"]["code"], -32602, "{scope}: {response}");
        assert_eq!(response["error"]["message"], reason, "{scope}");
        println!("REFUSED {scope} => {}", response["error"]);
    }
    assert_eq!(
        terminal_cards(&h, &h.track).await,
        cards_before,
        "a refused open creates nothing"
    );
    assert!(
        h.state
            .operation_runtime
            .find_by_kind_and_idempotency(
                "terminal-create",
                &format!("planner-terminal:{}:exceeds", h.session_id),
            )
            .await
            .unwrap()
            .is_none(),
        "a refused open submits nothing"
    );

    // (e) a REST terminal card under the policy: no hooks, no file, no keys.
    let response = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{}/terminal-cards", h.track))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"program": FAKE_CLAUDE, "cwd": "", "env": {},
                        "theme": {"fg": [216,219,226], "bg": [15,20,24]}})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let rest_card: Value = serde_json::from_slice(&bytes).unwrap();
    let rest_card_id = rest_card["id"].as_str().unwrap().to_owned();
    let stored = h.state.repo.card_get(&rest_card_id).await.unwrap().unwrap();
    for key in [
        "claude_permissions",
        "claude_permissions_source",
        "terminal_signals",
    ] {
        assert!(
            stored.payload.get(key).is_none(),
            "{key}: {}",
            stored.payload
        );
    }
    let rest_term = h
        .state
        .repo
        .terminal_get_by_card(&rest_card_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        rest_term.env.get("NEIGE_CLAUDE_SETTINGS").is_none(),
        "{}",
        rest_term.env
    );

    // (f) the third key is refused at the card PATCH boundary and sticky
    // across a replacement, like the block.
    let patch = |card_id: &str, body: String| {
        axum::http::Request::builder()
            .method("PATCH")
            .uri(format!("/api/cards/{card_id}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body))
            .unwrap()
    };
    let response = h
        .app
        .clone()
        .oneshot(patch(
            &a.card_id,
            json!({"payload":{"schemaVersion":1,"claude_permissions_source":"declared"}})
                .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    let response = h
        .app
        .clone()
        .oneshot(patch(
            &a.card_id,
            r#"{"payload":{"schemaVersion":1,"terminal_id":"x"}}"#.to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let card = h.state.repo.card_get(&a.card_id).await.unwrap().unwrap();
    assert_eq!(card.payload["terminal_id"], "x");
    assert_eq!(card.payload["claude_permissions"], block(&a.cwd, &policy()));
    assert_eq!(card.payload["claude_permissions_source"], "track_policy");

    for terminal in [
        &d.terminal,
        &a.terminal,
        &b.terminal,
        &b2.terminal,
        &rest_term.id,
    ] {
        h.state.terminal_renderer.drop_entry(terminal).await;
    }
    h.stop(&a.terminal).await;
}

/// (c') the probe: a replayed request_id with the same arguments returns
/// the existing terminal whatever the policy is now; only a fresh request_id
/// meets the narrowed policy.
#[tokio::test]
async fn replay_after_narrowing_returns_the_existing_terminal() {
    let h = Harness::start().await;
    patch_policy(&h, &h.track, json!({"bash": ["git"]})).await;
    let declared = json!({"bash": ["git status"]});
    let first = open_ok(
        &h,
        None,
        open_args("before-narrowing", Some(declared.clone())),
    )
    .await;
    assert_effective(
        &h,
        &first,
        &block(&first.cwd, &declared),
        "declared_within_policy",
    )
    .await;

    patch_policy(&h, &h.track, json!({"bash": ["python3"]})).await;

    // Same request_id, same arguments: the existing terminal, no -32602.
    let replay = h
        .call(
            "calm.terminal.open",
            open_args("before-narrowing", Some(declared.clone())),
        )
        .await;
    let replayed = receipt(&replay).clone();
    assert_eq!(replayed["terminal_id"], first.terminal, "{replayed}");
    assert_eq!(replayed["card_id"], first.card_id);
    assert_eq!(replayed["claude_permissions"], block(&first.cwd, &declared));
    assert_eq!(
        replayed["claude_permissions_source"],
        "declared_within_policy"
    );
    assert_eq!(terminal_cards(&h, &h.track).await, 1);
    // Same request_id, other arguments: S1's payload conflict, not the policy.
    let conflict = h
        .call("calm.terminal.open", open_args("before-narrowing", None))
        .await;
    assert_eq!(conflict["error"]["code"], -32403, "{conflict}");
    assert!(
        conflict["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already used with different payload"),
        "{conflict}"
    );
    // A fresh request_id with the same declaration meets the narrowed policy.
    let fresh = h
        .call(
            "calm.terminal.open",
            open_args("after-narrowing", Some(declared)),
        )
        .await;
    assert_eq!(fresh["error"]["code"], -32602, "{fresh}");
    assert_eq!(
        fresh["error"]["message"],
        "claude_permissions.bash[0] 'git status' exceeds the Track policy (bash: python3)"
    );
    assert_eq!(terminal_cards(&h, &h.track).await, 1);
    h.stop(&first.terminal).await;
}

/// (g) TOCTOU: the policy narrows between the handler's pre-check and the
/// write transaction; the in-tx re-check refuses the open from Pending —
/// `outcome: unavailable` naming the entry, `bad_request`, no card, no
/// terminal row, no file.
#[tokio::test]
async fn policy_narrowed_between_the_precheck_and_the_transaction_fails_the_open() {
    let h = Harness::start().await;
    patch_policy(&h, &h.track, json!({"bash": ["git"]})).await;
    // A prior open tells where the settings files live.
    let prior = open_ok(&h, None, open_args("prior", None)).await;
    let settings_dir = prior.settings_path.parent().unwrap().to_path_buf();
    let files_before = std::fs::read_dir(&settings_dir).unwrap().count();
    let cards_before = terminal_cards(&h, &h.track).await;
    let terminal_rows = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM terminals")
            .fetch_one(h.sql.pool())
            .await
            .unwrap()
    };
    let rows_before = terminal_rows().await;

    let hook = CeilingCheckedHook {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_ceiling_checked_hook_for_test(&h.track, hook.clone());
    let args = open_args("toctou", Some(json!({"bash": ["git"]})));
    let open = {
        let h = &h;
        async move { h.call("calm.terminal.open", args).await }
    };
    let entered = hook.entered.clone();
    let release = hook.release.clone();
    let narrow = async {
        entered.notified().await;
        patch_policy(&h, &h.track, json!({"bash": ["python3"]})).await;
        release.notify_one();
    };
    let (response, ()) = tokio::join!(open, narrow);

    let result = receipt(&response).clone();
    assert_eq!(result["outcome"], "unavailable", "{result}");
    let detail = result["detail"].as_str().unwrap();
    assert!(
        detail
            .contains("claude_permissions.bash[0] 'git' exceeds the Track policy (bash: python3)")
            && detail.contains("bad_request")
            && detail.contains("from_phase: Pending"),
        "{detail}"
    );
    assert!(
        summary(&response).starts_with("terminal open unavailable operation "),
        "{}",
        summary(&response)
    );
    assert_eq!(terminal_cards(&h, &h.track).await, cards_before, "no card");
    assert_eq!(
        std::fs::read_dir(&settings_dir).unwrap().count(),
        files_before,
        "no settings file"
    );
    let operation = h
        .state
        .operation_runtime
        .find_by_kind_and_idempotency(
            "terminal-create",
            &format!("planner-terminal:{}:toctou", h.session_id),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(operation.phase, calm_server::operation::Phase::Failed),
        "{:?}",
        operation.phase
    );
    assert!(operation.tx_output.is_none(), "nothing was prepared");
    assert_eq!(terminal_rows().await, rows_before, "no terminal row");
    // The same request_id replays the Failed row; a fresh one proceeds under
    // the narrowed policy.
    let replay = h
        .call(
            "calm.terminal.open",
            open_args("toctou", Some(json!({"bash": ["git"]}))),
        )
        .await;
    assert_eq!(receipt(&replay)["outcome"], "unavailable", "{replay}");
    let next = open_ok(
        &h,
        None,
        open_args("after-toctou", Some(json!({"bash": ["python3 -c"]}))),
    )
    .await;
    assert_effective(
        &h,
        &next,
        &block(&next.cwd, &json!({"bash": ["python3 -c"]})),
        "declared_within_policy",
    )
    .await;
    h.state.terminal_renderer.drop_entry(&prior.terminal).await;
    h.stop(&next.terminal).await;
}

/// (h) the ceiling resolves the tree ROOT: a child track's Planner opens
/// under the root's policy (rows 3/4/5 hold in the child), its `Edit` rules
/// are anchored at the CHILD's cwd, and the child's own `Track` still shows
/// `claude_permissions_policy: null`.
#[tokio::test]
async fn a_child_track_opens_under_its_root_policy() {
    let h = Harness::start().await;
    patch_policy(&h, &h.track, policy()).await;
    let child_dir = h.root.path().join("child");
    std::fs::create_dir_all(&child_dir).unwrap();
    let repo: Arc<dyn Repo> = h.sql.clone();
    let child = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: h.area_id.clone().into(),
            title: "child".into(),
            sort: None,
            cwd: child_dir.to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let child_id = child.id.to_string();
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
        .bind(&h.track)
        .bind(&child_id)
        .execute(h.sql.pool())
        .await
        .unwrap();
    h.state
        .repo
        .seed_track_area_cache(&h.state.track_area_cache)
        .await
        .unwrap();
    let (token, _) = h
        .planner_token(&child_id, child_dir.to_str().unwrap())
        .await;

    // The child's own row is null on every Track read...
    let response = h
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri(format!("/api/tracks/{child_id}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let detail: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        detail["track"]["claude_permissions_policy"],
        Value::Null,
        "{detail}"
    );
    assert!(detail["track"].get("claude_permissions_policy").is_some());

    // ...and the root's policy is its ceiling: row 3, anchored at the child.
    let a = open_ok(&h, Some(&token), open_args("child-policy", None)).await;
    assert_eq!(a.cwd, child_dir.to_str().unwrap());
    assert_ne!(a.cwd, h.root.path().to_str().unwrap());
    let expected = block(&a.cwd, &policy());
    assert!(
        expected["allow"][0]
            .as_str()
            .unwrap()
            .starts_with(&format!("Edit(//{}/", a.cwd.trim_matches('/'))),
        "{expected}"
    );
    assert_effective(&h, &a, &expected, "track_policy").await;
    // Row 4 in the child.
    let b = open_ok(
        &h,
        Some(&token),
        open_args(
            "child-narrow",
            Some(json!({"edit": ["src/lib/**"], "deny": ["git push"]})),
        ),
    )
    .await;
    let mut merged = policy();
    merged["edit"] = json!(["src/lib/**"]);
    merged["deny"] = json!(["git rebase", "git push"]);
    assert_effective(&h, &b, &block(&b.cwd, &merged), "declared_within_policy").await;
    // Row 5 in the child.
    let refused = h
        .call_with_token(
            &token,
            "calm.terminal.open",
            open_args("child-exceeds", Some(json!({"edit": ["**"]}))),
        )
        .await;
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
    assert_eq!(
        refused["error"]["message"],
        "claude_permissions.edit[0] '**' exceeds the Track policy (edit: src/**, tests/**)"
    );
    assert_eq!(terminal_cards(&h, &child_id).await, 2);

    h.state.terminal_renderer.drop_entry(&a.terminal).await;
    h.stop(&b.terminal).await;
}
