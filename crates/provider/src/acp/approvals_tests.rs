//! The adapter over a real `provider::acp` connection to an in-memory peer: what reaches the
//! held-request channel, and the exact frames each answer writes.

use super::*;
use crate::acp::Connection;
use calm_types::event::ASK_MAX_TEXT_CHARS;
use serde_json::json;
use std::future::Future;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc;

const BUDGET: Duration = Duration::from_secs(2);

struct Peer {
    connection: Connection,
    reader: BufReader<ReadHalf<DuplexStream>>,
    /// Kept open so the transport does not see the agent leave.
    _writer: WriteHalf<DuplexStream>,
    held: mpsc::UnboundedReceiver<HeldRequestMessage>,
    approvals: Approvals,
}

fn peer(mode: PlannerPermissionMode) -> Peer {
    peer_with(mode, AgentAsks::default())
}

fn peer_with(mode: PlannerPermissionMode, asks: AgentAsks) -> Peer {
    let (client, agent) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(client);
    let (agent_read, agent_write) = tokio::io::split(agent);
    let connection = Connection::new(read, write);
    let (sender, held) = mpsc::unbounded_channel();
    let approvals = Approvals::for_turn(mode, &sender, "turn-1", &connection.client, asks);
    Peer {
        connection,
        reader: BufReader::new(agent_read),
        _writer: agent_write,
        held,
        approvals,
    }
}

impl Peer {
    async fn read(&mut self) -> Value {
        let mut line = String::new();
        tokio::time::timeout(BUDGET, self.reader.read_line(&mut line))
            .await
            .expect("a frame in time")
            .expect("read");
        serde_json::from_str(&line).expect("json")
    }

    async fn request(&self, id: i64, params: Value) {
        self.approvals
            .request(json!(id), permission::METHOD, params)
            .await
            .expect("request handled");
    }

    async fn next_held(&mut self) -> HeldRequestMessage {
        tokio::time::timeout(BUDGET, self.held.recv())
            .await
            .expect("a held message in time")
            .expect("sender alive")
    }

    /// Nothing but `sentinel` was written after the frames already read.
    async fn wrote_nothing_more(&mut self) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.connection
            .client
            .notify_gated("sentinel", json!({}), &allow())
            .await
            .unwrap();
        assert_eq!(self.read().await["method"], "sentinel");
    }
}

/// What OpenCode 2.1.3 sent for a correlated `shell` call with native shell permissions set to ask.
fn shell() -> Value {
    json!({
        "sessionId": "ses_1",
        "toolCall": {"toolCallId": "call_12", "name": "shell", "title": "echo one > one.txt", "kind": "execute",
                     "status": "pending", "locations": [], "rawInput": {"command": "echo one > one.txt"}},
        "options": [
            {"optionId": "once", "kind": "allow_once", "name": "Allow once"},
            {"optionId": "always", "kind": "allow_always", "name": "Always allow"},
            {"optionId": "reject", "kind": "reject_once", "name": "Reject"}
        ]
    })
}

fn shell_question() -> AskQuestion {
    AskQuestion {
        title: "Run: echo one > one.txt".into(),
        options: vec!["Allow once".into(), "Always allow".into(), "Reject".into()],
    }
}

fn expect_open(message: HeldRequestMessage, id: i64) -> Box<dyn HeldResponder> {
    match message {
        HeldRequestMessage::Open {
            request_key,
            connection,
            questions,
            responder,
        } => {
            assert_eq!(request_key, RequestKey(format!("turn-1/{id}")));
            assert_eq!(connection, ConnectionId("turn-1".into()));
            assert_eq!(questions, vec![shell_question()]);
            responder
        }
        HeldRequestMessage::Gone { .. } => panic!("expected Open, got Gone"),
        HeldRequestMessage::ConnectionLost { .. } => panic!("expected Open, got ConnectionLost"),
    }
}

fn answer(id: i64, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

#[tokio::test]
async fn an_ask_turn_holds_each_request_and_answers_the_chosen_option_id() {
    let mut peer = peer(PlannerPermissionMode::Ask);
    for (id, option, option_id) in [(0, 0, "once"), (1, 1, "always"), (2, 2, "reject")] {
        peer.request(id, shell()).await;
        let responder = expect_open(peer.next_held().await, id);
        responder.respond(option);
        assert_eq!(
            peer.read().await,
            answer(
                id,
                json!({"outcome": {"outcome": "selected", "optionId": option_id}})
            )
        );
    }
}

#[tokio::test]
async fn a_responder_dropped_unanswered_answers_cancelled() {
    let mut peer = peer(PlannerPermissionMode::Ask);
    peer.request(4, shell()).await;
    drop(expect_open(peer.next_held().await, 4));
    assert_eq!(
        peer.read().await,
        answer(4, json!({"outcome": {"outcome": "cancelled"}}))
    );
}

/// The cancel is `session/cancel`, then `cancelled` for what is pending; each pending request is
/// then `Gone`. A later answer from its responder writes nothing, and a request after the cancel
/// is answered `cancelled` without asking.
#[tokio::test]
async fn a_cancel_answers_pending_requests_cancelled_and_reports_them_gone() {
    let mut peer = peer(PlannerPermissionMode::Ask);
    peer.request(7, shell()).await;
    let responder = expect_open(peer.next_held().await, 7);
    peer.approvals.cancel("native", &allow()).await.unwrap();
    assert_eq!(
        peer.read().await,
        json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "native"}})
    );
    assert_eq!(
        peer.read().await,
        answer(7, json!({"outcome": {"outcome": "cancelled"}}))
    );
    match peer.next_held().await {
        HeldRequestMessage::Gone { request_key } => {
            assert_eq!(request_key, RequestKey("turn-1/7".into()))
        }
        _ => panic!("expected Gone"),
    }
    responder.respond(0);
    peer.request(8, shell()).await;
    assert_eq!(
        peer.read().await,
        answer(8, json!({"outcome": {"outcome": "cancelled"}}))
    );
    peer.wrote_nothing_more().await;
    assert!(
        peer.held.try_recv().is_err(),
        "nothing asked after the cancel"
    );
}

/// The turn's read ended (the prompt settled, the process exited, a protocol error) while a request
/// waited: the close answers it `cancelled`. The person's answer after that writes nothing, and a
/// request after it is answered `cancelled` without asking.
#[tokio::test]
async fn an_answer_after_the_turn_closed_writes_nothing() {
    let mut peer = peer(PlannerPermissionMode::Ask);
    peer.request(9, shell()).await;
    let responder = expect_open(peer.next_held().await, 9);
    peer.approvals.close().await;
    assert_eq!(
        peer.read().await,
        answer(9, json!({"outcome": {"outcome": "cancelled"}}))
    );
    responder.respond(0);
    peer.request(10, shell()).await;
    assert_eq!(
        peer.read().await,
        answer(10, json!({"outcome": {"outcome": "cancelled"}}))
    );
    peer.wrote_nothing_more().await;
    assert!(
        peer.held.try_recv().is_err(),
        "nothing asked after the close"
    );
}

/// A cancel its caller's timeout cut short fenced the requests before answering them: an answer
/// after it is `cancelled`, never the chosen option.
#[tokio::test]
async fn an_answer_after_a_cut_short_cancel_is_cancelled() {
    let mut peer = peer(PlannerPermissionMode::Ask);
    peer.request(11, shell()).await;
    let responder = expect_open(peer.next_held().await, 11);
    // Block admission, poll cancel once, and drop it before the first write,
    // as the driver's timeout would.
    let gate = crate::send_boundary::PreSendGate::new(|| Box::pin(std::future::pending()));
    let mut cancel = Box::pin(peer.approvals.cancel("native", &gate));
    let first = std::future::poll_fn(|cx| std::task::Poll::Ready(cancel.as_mut().poll(cx))).await;
    assert!(
        first.is_pending(),
        "the cancel was cut short before its answers"
    );
    // Admission was cancelled: not even `session/cancel` went out.
    drop(cancel);
    responder.respond(0);
    assert_eq!(
        peer.read().await,
        answer(11, json!({"outcome": {"outcome": "cancelled"}}))
    );
}

#[tokio::test]
async fn dropping_an_ask_turns_approvals_loses_its_connection() {
    let mut peer = peer(PlannerPermissionMode::Ask);
    let replacement = Approvals::for_turn(
        PlannerPermissionMode::Never,
        &mpsc::unbounded_channel().0,
        "turn-2",
        &peer.connection.client,
        AgentAsks::default(),
    );
    drop(std::mem::replace(&mut peer.approvals, replacement));
    match peer.next_held().await {
        HeldRequestMessage::ConnectionLost { connection } => {
            assert_eq!(connection, ConnectionId("turn-1".into()))
        }
        _ => panic!("expected ConnectionLost"),
    }
}

/// `never` answers `cancelled` where the request is read and tells the channel nothing, not even
/// the end of the process; its cancel is `session/cancel` alone. `full` (#2441) is the same: a
/// stray request gets no new answering path.
#[tokio::test]
async fn a_never_or_full_turn_answers_cancelled_itself() {
    for mode in [PlannerPermissionMode::Never, PlannerPermissionMode::Full] {
        let mut peer = peer(mode);
        peer.request(0, shell()).await;
        assert_eq!(
            peer.read().await,
            answer(0, json!({"outcome": {"outcome": "cancelled"}})),
            "{mode:?}"
        );
        peer.approvals.cancel("native", &allow()).await.unwrap();
        assert_eq!(peer.read().await["method"], "session/cancel");
        let replacement = Approvals::for_turn(
            mode,
            &mpsc::unbounded_channel().0,
            "turn-2",
            &peer.connection.client,
            AgentAsks::default(),
        );
        drop(std::mem::replace(&mut peer.approvals, replacement));
        peer.wrote_nothing_more().await;
        assert!(peer.held.try_recv().is_err(), "{mode:?}");
    }
}

/// A malformed permission request is refused without asking; any other client method is not
/// supported (OpenCode sends `fs/write_text_file` after an approved edit).
#[tokio::test]
async fn a_malformed_request_or_another_method_is_refused_without_asking() {
    let mut peer = peer(PlannerPermissionMode::Ask);
    peer.request(3, json!({"toolCall": {"toolCallId": "t"}}))
        .await;
    assert_eq!(
        peer.read().await,
        answer(3, json!({"outcome": {"outcome": "cancelled"}}))
    );
    peer.approvals
        .request(
            json!(5),
            "fs/write_text_file",
            json!({"path": "/ws/e.txt", "content": "ho\n"}),
        )
        .await
        .unwrap();
    assert_eq!(peer.read().await["error"]["code"], -32601);
    assert!(peer.held.try_recv().is_err());
}

#[test]
fn the_question_names_the_call_its_kind_and_its_files() {
    let request = |tool_call: Value| {
        PermissionRequest::decode(json!({
            "toolCall": tool_call,
            "options": [{"optionId": "once", "kind": "allow_once", "name": "Allow once"}]
        }))
        .unwrap()
    };
    // OpenCode's request for a directory outside the workspace: kind `other`, which shows no
    // word; the title is the directory.
    let outside = question(&request(json!({
        "toolCallId": "call_5", "title": "/probe/outside", "kind": "other",
        "locations": [{"path": "/probe/outside/x.txt"}, {"path": "/probe/outside"}]
    })));
    assert_eq!(outside.title, "/probe/outside\nPath: /probe/outside/x.txt");
    assert_eq!(outside.options, vec!["Allow once".to_string()]);
    // OpenCode 2.1.3's edit of a file outside the workspace: the file is the title and its one
    // location, named once.
    let edit = question(&request(json!({
        "toolCallId": "call_38", "title": "/probe/outside/e.txt", "kind": "edit",
        "locations": [{"path": "/probe/outside/e.txt"}]
    })));
    assert_eq!(edit.title, "Edit: /probe/outside/e.txt");
    // Each kind of ACP's closed set reads as a plain word, never the raw kind; a kind this client
    // does not know shows none.
    for (kind, title) in [
        ("read", "Read: x"),
        ("edit", "Edit: x"),
        ("delete", "Delete: x"),
        ("move", "Move: x"),
        ("search", "Search: x"),
        ("execute", "Run: x"),
        ("think", "Think: x"),
        ("fetch", "Fetch: x"),
        ("switch_mode", "Switch mode: x"),
        ("other", "x"),
        ("teleport", "x"),
    ] {
        let asked = question(&request(
            json!({"toolCallId": "c", "title": "x", "kind": kind}),
        ));
        assert_eq!(asked.title, title, "{kind}");
    }
    // Without a title the call is named by its id.
    let untitled = question(&request(json!({"toolCallId": "call_9", "kind": "execute"})));
    assert_eq!(untitled.title, "Run: call_9");
    let bare = question(&request(json!({"toolCallId": "call_1"})));
    assert_eq!(bare.title, "call_1");
    let long = question(&request(json!({
        "toolCallId": "call_2", "title": "x".repeat(10_000), "kind": "execute"
    })));
    assert_eq!(long.title.chars().count(), ASK_MAX_TEXT_CHARS);
    // A long call is cut, not the files it names.
    let long_edit = question(&request(json!({
        "toolCallId": "call_3", "title": "y".repeat(10_000), "kind": "edit",
        "locations": [{"path": "/ws/a.rs"}, {"path": "/ws/b.rs"}]
    })));
    assert_eq!(long_edit.title.chars().count(), ASK_MAX_TEXT_CHARS);
    assert!(
        long_edit
            .title
            .ends_with("y…\nPath: /ws/a.rs\nPath: /ws/b.rs"),
        "{}",
        &long_edit.title[long_edit.title.len() - 60..]
    );
}

/// How Grok Build 1.0.62 asks before calling an MCP tool.
const GROK: McpToolRequest = McpToolRequest {
    kind: "other",
    separator: "__",
    raw_input_name: "tool_name",
};

/// The client's server as registered under the key `neige-k1`, listing two tools.
fn own_tools() -> AgentAsks {
    AgentAsks {
        own_tools: OwnTools::new(
            GROK,
            "neige-k1",
            ["neige_report_commit", "neige_report_read"],
        ),
        decline_ends_turn: false,
    }
}

/// What Grok Build 1.0.62 sent before calling a tool of an MCP server the client registered:
/// `use_tool` titled with the tool's qualified name, which its `rawInput.tool_name` repeats.
fn grok_mcp_tool(title: &str, kind: &str) -> Value {
    grok_use_tool(
        title,
        kind,
        json!({"variant": "UseTool", "tool_name": title, "tool_input": {}}),
    )
}

fn grok_use_tool(title: &str, kind: &str, raw_input: Value) -> Value {
    json!({
        "sessionId": "01a1",
        "toolCall": {"toolCallId": "call-1", "kind": kind, "title": title, "rawInput": raw_input},
        "options": [
            {"optionId": "always-allow", "name": "always allow", "kind": "allow_always"},
            {"optionId": "allow-once", "name": "allow once", "kind": "allow_once"},
            {"optionId": "reject-once", "name": "reject once", "kind": "reject_once"}
        ]
    })
}

const MODES: [PlannerPermissionMode; 3] = [
    PlannerPermissionMode::Never,
    PlannerPermissionMode::Ask,
    PlannerPermissionMode::Full,
];

/// Under every mode, a request for a listed tool of the client's own MCP server is answered with
/// the agent's `allow_once` option, and nothing reaches the held-request channel.
#[tokio::test]
async fn an_own_server_tool_is_allowed_once_under_every_mode_without_asking() {
    for mode in MODES {
        for title in [
            "neige-k1__neige_report_commit",
            "neige-k1__neige_report_read",
        ] {
            let mut peer = peer_with(mode, own_tools());
            peer.request(0, grok_mcp_tool(title, "other")).await;
            assert_eq!(
                peer.read().await,
                answer(
                    0,
                    json!({"outcome": {"outcome": "selected", "optionId": "allow-once"}})
                ),
                "{mode:?} {title}"
            );
            peer.wrote_nothing_more().await;
            assert!(peer.held.try_recv().is_err(), "{mode:?}");
        }
    }
}

/// Only the exact qualified name of a listed tool, in both the title and `rawInput.tool_name` of a
/// request of the declared kind, is the server's. Another server whose key starts with the
/// client's, the same tool under another server, an unlisted tool, and a title the input does not
/// repeat are each answered as the mode answers any request: `cancelled` under `never` and `full`,
/// put to the person under `ask`.
#[tokio::test]
async fn any_other_request_is_answered_as_the_mode_answers_it() {
    let title = "neige-k1__neige_report_commit";
    let use_tool = |name: &str| json!({"variant": "UseTool", "tool_name": name, "tool_input": {}});
    let requests = [
        // A server `neige-k1__evil` (Grok admits `_` in a server name) with a tool `run`.
        grok_mcp_tool("neige-k1__evil__run", "other"),
        // A server `neige-k1_` and its tool `_neige_report_commit`.
        grok_mcp_tool("neige-k1___neige_report_commit", "other"),
        // The same tool under the default key or another server.
        grok_mcp_tool("neige__neige_report_commit", "other"),
        grok_mcp_tool("neige-k2__neige_report_commit", "other"),
        // A tool the server does not list.
        grok_mcp_tool("neige-k1__neige_track_close", "other"),
        grok_mcp_tool("neige-k1__", "other"),
        grok_mcp_tool(title, "execute"),
        // The title of a listed tool over another call's input, or none.
        grok_use_tool(title, "other", use_tool("neige-k1__evil__run")),
        grok_use_tool(title, "other", json!({"variant": "UseTool"})),
        grok_use_tool(title, "other", Value::Null),
        grok_mcp_tool("neige-k1__neige_report_commit && rm -rf /", "other"),
        grok_mcp_tool("Execute `neige-k1__neige_report_commit`", "other"),
    ];
    for mode in MODES {
        for request in &requests {
            let mut peer = peer_with(mode, own_tools());
            peer.request(4, request.clone()).await;
            if mode == PlannerPermissionMode::Ask {
                assert!(
                    matches!(peer.next_held().await, HeldRequestMessage::Open { .. }),
                    "{request}"
                );
            } else {
                assert_eq!(
                    peer.read().await,
                    answer(4, json!({"outcome": {"outcome": "cancelled"}})),
                    "{mode:?} {request}"
                );
            }
        }
        // No own tools: even the listed name is the mode's.
        let mut peer = peer_with(mode, AgentAsks::default());
        peer.request(5, grok_mcp_tool(title, "other")).await;
        if mode == PlannerPermissionMode::Ask {
            assert!(matches!(
                peer.next_held().await,
                HeldRequestMessage::Open { .. }
            ));
        } else {
            assert_eq!(
                peer.read().await,
                answer(5, json!({"outcome": {"outcome": "cancelled"}}))
            );
        }
    }
}

/// A server key holding the separator has no tools, and a tool name holding it is left out: either
/// could be split at another place.
#[test]
fn an_ambiguous_server_or_tool_name_is_never_an_own_tool() {
    assert!(OwnTools::new(GROK, "neige__k1", ["neige_report_commit"]).is_none());
    assert!(OwnTools::new(GROK, "", ["neige_report_commit"]).is_none());
    let tools = OwnTools::new(GROK, "neige-k1", ["evil__run", "", "neige_report_read"]).unwrap();
    assert_eq!(
        tools.qualified,
        HashSet::from(["neige-k1__neige_report_read".to_owned()])
    );
}

/// After the turn's fence (a cancel, or the end of its read) nothing is approved.
#[tokio::test]
async fn an_own_server_tool_is_not_allowed_after_the_fence() {
    let title = "neige-k1__neige_report_commit";
    for mode in [PlannerPermissionMode::Never, PlannerPermissionMode::Ask] {
        let mut peer = peer_with(mode, own_tools());
        peer.approvals.cancel("native", &allow()).await.unwrap();
        assert_eq!(peer.read().await["method"], "session/cancel");
        peer.request(6, grok_mcp_tool(title, "other")).await;
        assert_eq!(
            peer.read().await,
            answer(6, json!({"outcome": {"outcome": "cancelled"}})),
            "{mode:?}"
        );
        let mut peer = peer_with(mode, own_tools());
        peer.approvals.close().await;
        peer.request(7, grok_mcp_tool(title, "other")).await;
        assert_eq!(
            peer.read().await,
            answer(7, json!({"outcome": {"outcome": "cancelled"}})),
            "{mode:?}"
        );
    }
}

fn ends_turn() -> AgentAsks {
    AgentAsks {
        decline_ends_turn: true,
        ..own_tools()
    }
}

/// A request the mode declined is why an agent that ends its turn on a decline ended it; an
/// allowed own tool, a request after the fence, and an agent that does not end its turn so say
/// nothing. Under `never` it also names the modes that would let the request through.
#[tokio::test]
async fn a_request_the_mode_declined_names_why_the_turn_ended() {
    let mut peer = peer_with(PlannerPermissionMode::Never, ends_turn());
    peer.request(0, grok_mcp_tool("neige-k1__neige_report_read", "other"))
        .await;
    peer.read().await;
    assert_eq!(peer.approvals.declined(), None, "an allowed own tool");
    peer.request(1, shell()).await;
    assert_eq!(
        peer.read().await,
        answer(1, json!({"outcome": {"outcome": "cancelled"}}))
    );
    assert_eq!(
        peer.approvals.declined().as_deref(),
        Some(
            "The agent ended the turn because Neige declined `echo one > one.txt` under the \
             `never` permission mode. Switch the permission mode to `ask` to approve such a \
             request, or to `full` to allow it."
        )
    );
    let mut peer = peer_with(PlannerPermissionMode::Full, ends_turn());
    peer.request(1, shell()).await;
    peer.read().await;
    assert_eq!(
        peer.approvals.declined().as_deref(),
        Some(
            "The agent ended the turn because Neige declined `echo one > one.txt` under the \
             `full` permission mode."
        )
    );
    let mut peer = peer_with(PlannerPermissionMode::Full, own_tools());
    peer.request(1, shell()).await;
    peer.read().await;
    assert_eq!(peer.approvals.declined(), None, "no profile declaration");
    let mut peer = peer_with(PlannerPermissionMode::Never, ends_turn());
    peer.approvals.close().await;
    peer.request(1, shell()).await;
    peer.read().await;
    assert_eq!(peer.approvals.declined(), None, "after the fence");
}

/// Under `ask`, a reject the person chose is why the turn ended; an allow is not.
#[tokio::test]
async fn a_reject_the_person_chose_names_why_the_turn_ended() {
    for (option, declined) in [
        (0, None),
        (
            2,
            Some("The agent ended the turn because the person declined `echo one > one.txt`."),
        ),
    ] {
        let mut peer = peer_with(PlannerPermissionMode::Ask, ends_turn());
        peer.request(0, shell()).await;
        let HeldRequestMessage::Open { responder, .. } = peer.next_held().await else {
            panic!("expected Open");
        };
        responder.respond(option);
        peer.read().await;
        assert_eq!(peer.approvals.declined().as_deref(), declined, "{option}");
    }
}

fn allow() -> crate::send_boundary::PreSendGate<'static> {
    crate::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) }))
}
