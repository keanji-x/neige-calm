//! #2348 A2: Planner approvals over a real WS-over-UDS connection: the production reader and
//! dispatch on one end, the test as codex on the other.
use super::*;
use crate::TurnModelSelection;
use crate::codex::{CodexAppServer, InputItem, NotificationStream};
use crate::held_requests::{self, HeldRequestReceiver};
use calm_types::event::ASK_MAX_TEXT_CHARS;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

const THREAD: &str = "thread-a";

async fn recv(server: &mut WebSocketStream<UnixStream>) -> Value {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Message::Text(text) = server.next().await.unwrap().unwrap() {
                return serde_json::from_str(&text).unwrap();
            }
        }
    })
    .await
    .expect("frame deadline")
}

async fn send(server: &mut WebSocketStream<UnixStream>, value: Value) {
    server.send(Message::Text(value.to_string())).await.unwrap();
}

async fn next_message(rx: &mut HeldRequestReceiver) -> HeldRequestMessage {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("held-request deadline")
        .expect("held-request channel closed")
}

struct Opened {
    request_key: RequestKey,
    connection: ConnectionId,
    questions: Vec<AskQuestion>,
    responder: Box<dyn HeldResponder>,
}

async fn next_open(rx: &mut HeldRequestReceiver) -> Opened {
    match next_message(rx).await {
        HeldRequestMessage::Open {
            request_key,
            connection,
            questions,
            responder,
        } => Opened {
            request_key,
            connection,
            questions,
            responder,
        },
        _ => panic!("expected an Open"),
    }
}

fn command_request(id: Value, thread: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"item/commandExecution/requestApproval","params":{
        "kind":"command","threadId":thread,"turnId":"turn-a","itemId":"call-a",
        "reason":"needs to write outside the workspace",
        "command":"/bin/zsh -lc 'touch /outside/a'","cwd":"/work",
        "availableDecisions":["accept","cancel"]
    }})
}

fn file_change_request(id: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"item/fileChange/requestApproval","params":{
        "threadId":THREAD,"turnId":"turn-a","itemId":"patch-a","reason":null,"grantRoot":"/outside"
    }})
}

fn mcp_request(id: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"mcpServer/elicitation/request","params":{
        "threadId":THREAD,"turnId":"turn-a","serverName":"neige","mode":"form",
        "_meta":{"codex_approval_kind":"mcp_tool_call","persist":["session","always"]},
        "message":"Allow the neige MCP server to run tool \"t_none\"?",
        "requestedSchema":{"type":"object","properties":{}}
    }})
}

struct Rig {
    client: CodexAppServer,
    _notifications: NotificationStream,
    server: WebSocketStream<UnixStream>,
    routes: Arc<ApprovalRoutes>,
}

async fn connection() -> Rig {
    let routes = Arc::new(ApprovalRoutes::default());
    let (client, notifications, server) =
        CodexAppServer::connect_pair_routed_for_test(routes.clone()).await;
    Rig {
        client,
        _notifications: notifications,
        server,
        routes,
    }
}

/// A connection with [`THREAD`] routed to the returned channel.
async fn routed() -> (Rig, ApprovalRoute, HeldRequestReceiver) {
    let rig = connection().await;
    let (tx, rx) = held_requests::channel();
    let route = rig.routes.route(THREAD, tx);
    (rig, route, rx)
}

#[test]
fn a_planner_turn_says_all_three_settings_for_its_mode_and_a_worker_turn_none() {
    let frame = |approvals: TurnApprovals| {
        let mut map = Map::new();
        approvals.insert_into(&mut map);
        Value::Object(map)
    };
    assert_eq!(
        frame(TurnApprovals::Explicit(PlannerPermissionMode::Never)),
        json!({
            "approvalPolicy": "never",
            "approvalsReviewer": "user",
            "sandboxPolicy": {"type": "workspaceWrite", "networkAccess": true},
        })
    );
    assert_eq!(
        frame(TurnApprovals::Explicit(PlannerPermissionMode::Ask)),
        json!({
            "approvalPolicy": {"granular": {
                "sandbox_approval": true,
                "rules": true,
                "request_permissions": false,
                "mcp_elicitations": false,
                "skill_approval": false,
            }},
            "approvalsReviewer": "user",
            "sandboxPolicy": {"type": "workspaceWrite", "networkAccess": true},
        })
    );
    assert_eq!(
        frame(TurnApprovals::Explicit(PlannerPermissionMode::Full)),
        json!({
            "approvalPolicy": "never",
            "approvalsReviewer": "user",
            "sandboxPolicy": {"type": "dangerFullAccess"},
        })
    );
    assert_eq!(frame(TurnApprovals::Unchanged), json!({}));
}

/// #2441: codex keeps a turn's sandbox on the thread, so the turn after a `full` one is sandboxed
/// again only because its own `turn/start` says so. Two turns on one thread, as codex reads them.
#[tokio::test]
async fn a_turn_after_a_full_turn_says_workspace_write_again() {
    let (mut rig, _route, _rx) = routed().await;
    let selection = TurnModelSelection::inherit();
    let mut sandboxes = Vec::new();
    for (n, mode) in [
        PlannerPermissionMode::Full,
        PlannerPermissionMode::Never,
        PlannerPermissionMode::Full,
        PlannerPermissionMode::Ask,
    ]
    .into_iter()
    .enumerate()
    {
        let start = rig.client.turn_start_with_client_id(
            THREAD,
            vec![InputItem::text("hi")],
            &selection,
            TurnApprovals::Explicit(mode),
            None,
        );
        let server = &mut rig.server;
        let peer = async {
            let request = recv(server).await;
            send(
                server,
                json!({"id":request["id"],"result":{"turn":{"id":format!("turn-{n}")}}}),
            )
            .await;
            request
        };
        let (started, request) = tokio::join!(start, peer);
        started.unwrap();
        assert_eq!(request["method"], "turn/start");
        sandboxes.push(request["params"]["sandboxPolicy"].clone());
    }
    let workspace_write = json!({"type": "workspaceWrite", "networkAccess": true});
    let full = json!({"type": "dangerFullAccess"});
    assert_eq!(
        sandboxes,
        [full.clone(), workspace_write.clone(), full, workspace_write]
    );
}

/// The settings reach the `turn/start` frame the client writes.
#[tokio::test]
async fn the_turn_start_frame_carries_the_ask_settings() {
    let (mut rig, _route, _rx) = routed().await;
    let selection = TurnModelSelection::inherit();
    let start = rig.client.turn_start_with_client_id(
        THREAD,
        vec![InputItem::text("hi")],
        &selection,
        TurnApprovals::Explicit(PlannerPermissionMode::Ask),
        Some("entry-1"),
    );
    let server = &mut rig.server;
    let peer = async {
        let request = recv(server).await;
        send(
            server,
            json!({"id":request["id"],"result":{"turn":{"id":"turn-a"}}}),
        )
        .await;
        request
    };
    let (started, request) = tokio::join!(start, peer);
    started.unwrap();
    let params = &request["params"];
    assert_eq!(request["method"], "turn/start");
    assert_eq!(params["approvalsReviewer"], "user");
    assert_eq!(
        params["approvalPolicy"]["granular"]["sandbox_approval"],
        true
    );
    assert_eq!(params["sandboxPolicy"]["type"], "workspaceWrite");
    assert_eq!(params["clientUserMessageId"], "entry-1");
}

/// A request frame for an id, its title, and the answer to each option.
type Case = (fn(Value) -> Value, &'static str, [Value; 3]);

/// Each approval kind is one question with three options, and each option is its exact answer.
#[tokio::test]
async fn each_option_of_each_approval_kind_is_its_exact_answer() {
    let cases: [Case; 3] = [
        (
            |id| command_request(id, THREAD),
            "Run `/bin/zsh -lc 'touch /outside/a'`?\nIn /work\nReason: needs to write outside the workspace",
            [
                json!({"decision":"accept"}),
                json!({"decision":"acceptForSession"}),
                json!({"decision":"decline"}),
            ],
        ),
        (
            file_change_request,
            "Apply a file change?\nGrants write access under /outside",
            [
                json!({"decision":"accept"}),
                json!({"decision":"acceptForSession"}),
                json!({"decision":"decline"}),
            ],
        ),
        (
            mcp_request,
            "Allow the neige MCP server to run tool \"t_none\"?",
            [
                json!({"action":"accept","content":{}}),
                json!({"action":"accept","content":{},"_meta":{"persist":"session"}}),
                json!({"action":"decline","content":null}),
            ],
        ),
    ];
    let (mut rig, _route, mut rx) = routed().await;
    let mut n = 0;
    for (request, title, answers) in cases {
        for (option, answer) in answers.into_iter().enumerate() {
            n += 1;
            let id = json!(format!("req-{n}"));
            send(&mut rig.server, request(id.clone())).await;
            let opened = next_open(&mut rx).await;
            assert_eq!(
                opened.questions,
                vec![AskQuestion {
                    title: title.into(),
                    options: vec![
                        "Allow".into(),
                        "Allow for this session".into(),
                        "Deny".into()
                    ],
                }]
            );
            opened.responder.respond(option);
            assert_eq!(
                recv(&mut rig.server).await,
                json!({"jsonrpc":"2.0","id":id,"result":answer}),
                "option {option} of {title}"
            );
        }
    }
}

/// A responder dropped unanswered denies its request in that request's own form.
#[tokio::test]
async fn a_responder_dropped_unanswered_denies() {
    let (mut rig, _route, mut rx) = routed().await;
    for (request, deny) in [
        (
            command_request(json!(7), THREAD),
            json!({"decision":"decline"}),
        ),
        (file_change_request(json!(8)), json!({"decision":"decline"})),
        (
            mcp_request(json!(9)),
            json!({"action":"decline","content":null}),
        ),
    ] {
        let id = request["id"].clone();
        send(&mut rig.server, request).await;
        drop(next_open(&mut rx).await);
        assert_eq!(
            recv(&mut rig.server).await,
            json!({"jsonrpc":"2.0","id":id,"result":deny})
        );
    }
}

/// `serverRequest/resolved` withdraws the request it names, on the channel its `Open` went to,
/// after that `Open`.
#[tokio::test]
async fn resolved_is_gone_for_the_request_it_names() {
    let (mut rig, _route, mut rx) = routed().await;
    send(&mut rig.server, command_request(json!(3), THREAD)).await;
    let opened = next_open(&mut rx).await;
    send(
        &mut rig.server,
        json!({"jsonrpc":"2.0","method":"serverRequest/resolved","params":{"threadId":THREAD,"requestId":3}}),
    )
    .await;
    match next_message(&mut rx).await {
        HeldRequestMessage::Gone { request_key } => assert_eq!(request_key, opened.request_key),
        _ => panic!("expected Gone"),
    }
}

/// A thread no harness routes, a request that is not an approval and an elicitation that is not a
/// tool approval are refused as before.
#[tokio::test]
async fn everything_but_a_routed_approval_is_refused() {
    let (mut rig, _route, mut rx) = routed().await;
    let mut elicitation = mcp_request(json!(12));
    elicitation["params"]["_meta"] = json!({});
    for request in [
        command_request(json!(10), "thread-unrouted"),
        json!({"jsonrpc":"2.0","id":11,"method":"item/tool/call","params":{"threadId":THREAD}}),
        elicitation,
    ] {
        let id = request["id"].clone();
        send(&mut rig.server, request).await;
        let reply = recv(&mut rig.server).await;
        assert_eq!(reply["id"], id);
        assert_eq!(reply["error"]["code"], -32601);
    }
    assert!(rx.try_recv().is_err(), "nothing was handed over");
}

/// An exiting harness drops its route after its successor routed the same thread: the successor
/// keeps the thread. Once the successor's route is dropped too, the thread is refused.
#[tokio::test]
async fn an_earlier_route_dropped_late_leaves_its_successors_route() {
    let mut rig = connection().await;
    let (earlier_tx, mut earlier_rx) = held_requests::channel();
    let earlier = rig.routes.route(THREAD, earlier_tx);
    let (successor_tx, mut successor_rx) = held_requests::channel();
    let successor = rig.routes.route(THREAD, successor_tx);
    drop(earlier);

    send(&mut rig.server, command_request(json!(20), THREAD)).await;
    let opened = next_open(&mut successor_rx).await;
    assert!(earlier_rx.try_recv().is_err());
    opened.responder.respond(0);
    assert_eq!(
        recv(&mut rig.server).await["result"],
        json!({"decision":"accept"})
    );

    drop(successor);
    send(&mut rig.server, command_request(json!(21), THREAD)).await;
    assert_eq!(recv(&mut rig.server).await["error"]["code"], -32601);
}

/// Aborting the reader (a replace or a heal drops the old client) tells every harness it handed
/// a request that the connection is gone, naming that connection.
#[tokio::test]
async fn an_aborted_reader_tells_connection_lost() {
    let (mut rig, _route, mut rx) = routed().await;
    send(&mut rig.server, command_request(json!(30), THREAD)).await;
    let opened = next_open(&mut rx).await;
    rig.client.reader.abort();
    match next_message(&mut rx).await {
        HeldRequestMessage::ConnectionLost { connection } => {
            assert_eq!(connection, opened.connection)
        }
        _ => panic!("expected ConnectionLost"),
    }
}

/// A command longer than the kernel's limit is cut, never refused for size.
#[test]
fn a_long_command_title_is_cut_to_the_kernels_limit() {
    let mut request = command_request(json!(1), THREAD);
    request["params"]["command"] = json!("x".repeat(3 * ASK_MAX_TEXT_CHARS));
    let parsed =
        ApprovalRequest::parse(request["method"].as_str().unwrap(), &request["params"]).unwrap();
    let title = &parsed.questions()[0].title;
    assert_eq!(title.chars().count(), ASK_MAX_TEXT_CHARS);
    assert!(title.ends_with('…'));
}
