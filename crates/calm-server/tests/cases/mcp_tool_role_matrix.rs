//! Every kernel tool's declared `roles` is what `tools/call` enforces (#2289 D1): each registered
//! kernel tool, called with `{}` by every role that holds a card token, is refused for the role
//! reason (`-32403`, the registry's gate) exactly when its declaration excludes that role. The
//! compiled plugin natives need their plugin running and in scope, which this fixture has not;
//! `builtin_plugins::tests::native_tools_refuse_exactly_the_roles_they_do_not_declare` covers them.

#![cfg(unix)]

use crate::support;

use calm_server::model::CardRole;
use serde_json::{Value, json};
use support::mcp::{boot_with_role, connect, handshake, recv_frame, send_frame};

/// Kernel tools with their declared roles; the compiled plugin natives are excluded (see above).
fn kernel_tools() -> Vec<(String, &'static [CardRole])> {
    let mut tools = calm_server::mcp_server::build_default_registry()
        .descriptors()
        .into_iter()
        .filter(|descriptor| calm_server::builtin_plugins::owner(&descriptor.name).is_none())
        .map(|descriptor| (descriptor.name, descriptor.roles))
        .collect::<Vec<_>>();
    tools.sort_by(|a, b| a.0.cmp(&b.0));
    tools
}

/// The registry's role refusal, and only it: a data-level `-32403` (a grant, a scope) does not count.
fn role_refused(response: &Value, role: CardRole) -> bool {
    let Some(error) = response.get("error") else {
        return false;
    };
    let message = error["message"].as_str().unwrap_or_default();
    error["code"].as_i64() == Some(-32403)
        && message.contains("tool requires role in [")
        && message.contains(&format!("got={role:?}"))
}

async fn assert_role_matrix_row(role: CardRole) {
    let tools = kernel_tools();
    assert!(
        tools.len() >= 40,
        "anti-vacuity: {} kernel tools",
        tools.len()
    );
    let boot = boot_with_role(role).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    handshake(&mut rd, &mut wr, &boot.raw_token).await;

    let mut wrong = Vec::new();
    for (idx, (tool, roles)) in tools.iter().enumerate() {
        send_frame(
            &mut wr,
            json!({
                "jsonrpc": "2.0",
                "id": 100 + idx,
                "method": "tools/call",
                "params": { "name": tool, "arguments": {} }
            }),
        )
        .await;
        let response = recv_frame(&mut rd).await;
        // An admitted call reaches its handler: whatever it answers, it is never "unknown tool".
        assert_ne!(
            response["error"]["code"].as_i64(),
            Some(-32601),
            "`{tool}` must be served to {role:?}: {response:#?}"
        );
        let refused = role_refused(&response, role);
        if refused == roles.contains(&role) {
            wrong.push(format!(
                "{tool}: declared roles {roles:?}, {role:?} {} -> {}",
                if refused { "refused" } else { "admitted" },
                response.get("error").unwrap_or(&Value::Null)
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{role:?}: the role gate disagrees with the declared roles:\n{}",
        wrong.join("\n")
    );
    let _ = (&boot.server, &boot.repo);
}

#[tokio::test]
async fn planner_is_refused_exactly_the_tools_that_do_not_declare_it() {
    assert_role_matrix_row(CardRole::Planner).await;
}

#[tokio::test]
async fn worker_is_refused_exactly_the_tools_that_do_not_declare_it() {
    assert_role_matrix_row(CardRole::Worker).await;
}

#[tokio::test]
async fn assistant_is_refused_exactly_the_tools_that_do_not_declare_it() {
    assert_role_matrix_row(CardRole::Assistant).await;
}

#[tokio::test]
async fn report_card_is_refused_exactly_the_tools_that_do_not_declare_it() {
    assert_role_matrix_row(CardRole::ReportCard).await;
}
