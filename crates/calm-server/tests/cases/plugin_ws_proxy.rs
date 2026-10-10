//! #2530 E3a: `GET /api/plugins/{id}/ws/{*path}` through the production router assembly (both the
//! application and the public mobile router), a real `PluginHost` running the echo stub, and a
//! WebSocket server on the plugin's declared Unix socket. The socket server lives in this test
//! process, so a closed tunnel is the kernel's cancellation, never the plugin process dying.

#![cfg(unix)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::{Message as WsMessage, WebSocketUpgrade};
use axum::extract::{RawQuery, Request};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::get;
use axum::serve::ListenerExt;
use calm_server::auth::{AuthState, SessionAuthority};
use calm_server::db::Repo;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use futures::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream, UnixListener};
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest};

use super::auth::live_auth_state;

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");
/// Declares `http_socket`.
const SOCKET_PLUGIN: &str = "wsplug";
/// Running, but declares no `http_socket`.
const PLAIN_PLUGIN: &str = "plainplug";

type Router = fn(AppState, AuthState) -> axum::Router;
const ROUTERS: [(&str, Router); 2] = [
    ("application", routes::application_router),
    ("mobile", routes::public_mobile_router),
];

struct Stack {
    addr: SocketAddr,
    state: AppState,
    /// `calm-session=<a live session>`.
    session: String,
    socket_path: PathBuf,
    _tmp: TempDir,
}

fn install_stub(plugins_dir: &Path, id: &str, http_socket: Option<&str>) -> (Manifest, PathBuf) {
    let install_dir = plugins_dir.join(id);
    std::fs::create_dir_all(install_dir.join("bin")).unwrap();
    std::os::unix::fs::symlink(ECHO_BIN, install_dir.join("bin/stub")).unwrap();
    let mut manifest = json!({
        "manifest_version": 6,
        "id": id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": id,
        "entrypoint": { "command": "bin/stub" },
    });
    if let Some(name) = http_socket {
        manifest["http_socket"] = json!(name);
    }
    (Manifest::parse(&manifest.to_string()).unwrap(), install_dir)
}

/// Both plugins installed and enabled; only [`PLAIN_PLUGIN`] is spawned.
async fn stack(router: Router) -> Stack {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("data");
    let mut entries = Vec::new();
    for (id, socket) in [(SOCKET_PLUGIN, Some("http.sock")), (PLAIN_PLUGIN, None)] {
        let (manifest, install_dir) = install_stub(&plugins_dir, id, socket);
        entries.push((manifest, Some(install_dir)));
    }
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    for (manifest, install_dir) in &entries {
        repo.plugin_install(calm_server::model::NewPlugin {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            install_path: install_dir.as_ref().unwrap().display().to_string(),
            manifest: manifest.to_json(),
            enabled: true,
            user_config: json!({}),
        })
        .await
        .unwrap();
    }
    let events = EventBus::new();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::from_manifests(entries)),
        repo.clone(),
        plugins_dir,
        plugins_data_dir.clone(),
        Vec::new(),
        events.clone(),
        WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
    ));
    plugin.spawn(PLAIN_PLUGIN).await.unwrap();
    let state = AppState::from_parts(
        repo,
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin,
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    let auth = live_auth_state("alice", "pw");
    let session = format!(
        "calm-session={}",
        auth.sessions.create(SessionAuthority::PasswordLogin)
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone(), auth);
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Stack {
        addr,
        state,
        session,
        socket_path: plugins_data_dir.join(SOCKET_PLUGIN).join("http.sock"),
        _tmp: tmp,
    }
}

/// A WebSocket server on the plugin's socket; the counter is its accepted connections. Its first
/// frame reports the path, query, `Cookie` and `x-probe` it received; then it echoes text frames.
/// `/set-cookie` answers the upgrade with `Set-Cookie` headers.
async fn plugin_server(path: &Path) -> Arc<AtomicUsize> {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    let field = |headers: &HeaderMap, name: &str| {
        headers
            .get(name)
            .map_or("-", |v| v.to_str().unwrap())
            .to_owned()
    };
    let report = move |ws: WebSocketUpgrade, RawQuery(query): RawQuery, req: Request| async move {
        let seen = format!(
            "path={} query={} cookie={} probe={}",
            req.uri().path(),
            query.as_deref().unwrap_or("-"),
            field(req.headers(), "cookie"),
            field(req.headers(), "x-probe"),
        );
        ws.on_upgrade(|mut socket| async move {
            if socket.send(WsMessage::text(seen)).await.is_err() {
                return;
            }
            while let Some(Ok(message)) = socket.next().await {
                if let WsMessage::Text(_) = message
                    && socket.send(message).await.is_err()
                {
                    break;
                }
            }
        })
    };
    let set_cookie = |ws: WebSocketUpgrade| async move {
        let mut resp = ws.on_upgrade(|_| async {});
        for cookie in [
            "calm-session=PWN; Path=/",
            "__Host-calm-session=Z; Path=/; Secure",
            "calm%2Dsession=E; Path=/",
            // Nameless: a browser stores the value `calm-session=N` and sends it back as is.
            "=calm-session=N; Path=/",
            // Cookie names are case-sensitive; auth never reads this one, so it passes.
            "CALM-SESSION=C",
            "ok=1",
        ] {
            resp.headers_mut()
                .append(header::SET_COOKIE, cookie.parse().unwrap());
        }
        resp
    };
    // Reports every forwarding header it received.
    let forwarded = |ws: WebSocketUpgrade, headers: HeaderMap| async move {
        let mut names: Vec<_> = headers
            .keys()
            .map(|name| name.as_str().to_owned())
            .filter(|name| name == "forwarded" || name.starts_with("x-forwarded-"))
            .collect();
        names.sort();
        let seen = format!("forwarded=[{}]", names.join(","));
        ws.on_upgrade(|mut socket| async move {
            let _ = socket.send(WsMessage::text(seen)).await;
        })
    };
    let app = axum::Router::new()
        .route("/set-cookie", get(set_cookie))
        .route("/forwarded", get(forwarded))
        .fallback(report);
    let listener = UnixListener::bind(path).unwrap().tap_io(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    accepted
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// `Err` is the HTTP status of a refused handshake. `origin: None` sends calm's own origin.
async fn ws_connect(
    stack: &Stack,
    path: &str,
    cookie: Option<&str>,
    origin: Option<&str>,
) -> Result<(Ws, tungstenite::handshake::client::Response), StatusCode> {
    let mut request = format!("ws://{}{path}", stack.addr)
        .into_client_request()
        .unwrap();
    let own = format!("http://{}", stack.addr);
    let headers = request.headers_mut();
    headers.insert("origin", origin.unwrap_or(&own).parse().unwrap());
    headers.insert("x-probe", "1".parse().unwrap());
    if let Some(cookie) = cookie {
        headers.insert("cookie", cookie.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok(connected) => Ok(connected),
        Err(tungstenite::Error::Http(resp)) => Err(resp.status()),
        Err(other) => panic!("WS handshake failed without a status: {other}"),
    }
}

/// A plain (non-upgrade) GET carrying the session.
async fn plain_get(stack: &Stack, path: &str) -> StatusCode {
    let stream = TcpStream::connect(stack.addr).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(conn);
    let request = axum::http::Request::get(path)
        .header(header::HOST, stack.addr.to_string())
        .header(header::COOKIE, &stack.session)
        .body(Body::empty())
        .unwrap();
    sender.send_request(request).await.unwrap().status()
}

async fn next_frame(socket: &mut Ws) -> Option<Result<Message, tungstenite::Error>> {
    tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("frame or close within 5 s")
}

async fn assert_tunnel_closed(socket: &mut Ws, why: &str) {
    let next = tokio::time::timeout(Duration::from_secs(5), socket.next()).await;
    match next.unwrap_or_else(|_| panic!("tunnel still open 5 s after {why}")) {
        None | Some(Err(_)) | Some(Ok(Message::Close(_))) => {}
        Some(Ok(other)) => panic!("tunnel still open after {why}: {other:?}"),
    }
}

/// An open tunnel to the serving plugin, its first frame consumed.
async fn open_tunnel(stack: &Stack) -> Ws {
    let path = format!("/api/plugins/{SOCKET_PLUGIN}/ws/stream");
    let (mut socket, _) = ws_connect(stack, &path, Some(&stack.session), None)
        .await
        .unwrap();
    next_frame(&mut socket).await.unwrap().unwrap();
    socket
}

async fn wait_for_running(state: &AppState, id: &str) {
    for _ in 0..200 {
        let status = state.plugin.status(id).await.map(|s| s.status);
        if status == Some(PluginRuntimeStatus::Running) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("`{id}` never became running");
}

#[tokio::test]
async fn refused_upgrades_never_reach_the_plugin_socket_on_either_router() {
    for (name, router) in ROUTERS {
        let stack = stack(router).await;
        let accepted = plugin_server(&stack.socket_path).await;
        stack.state.plugin.spawn(SOCKET_PLUGIN).await.unwrap();
        let path = format!("/api/plugins/{SOCKET_PLUGIN}/ws/stream");
        for jar in [None, Some("calm-session=forged")] {
            let refused = ws_connect(&stack, &path, jar, None).await.err();
            assert_eq!(refused, Some(StatusCode::UNAUTHORIZED), "{name} {jar:?}");
        }
        let foreign = format!("http://127.0.0.1:{}", stack.addr.port() + 1);
        let refused = ws_connect(&stack, &path, Some(&stack.session), Some(&foreign))
            .await
            .err();
        assert_eq!(refused, Some(StatusCode::FORBIDDEN), "{name} cross-origin");
        assert_eq!(
            plain_get(&stack, &path).await,
            StatusCode::BAD_REQUEST,
            "{name}: a request that is not an upgrade"
        );
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            0,
            "{name}: plugin contacted"
        );

        let (mut socket, _) = ws_connect(&stack, &path, Some(&stack.session), None)
            .await
            .unwrap_or_else(|status| panic!("{name}: same-origin upgrade refused: {status}"));
        next_frame(&mut socket).await.unwrap().unwrap();
        assert_eq!(accepted.load(Ordering::SeqCst), 1, "{name}");
    }
}

#[tokio::test]
async fn the_upgrade_reaches_the_plugin_without_calm_session() {
    let stack = stack(routes::application_router).await;
    plugin_server(&stack.socket_path).await;
    stack.state.plugin.spawn(SOCKET_PLUGIN).await.unwrap();
    // A planted session cookie in front of the genuine one: both are stripped, the rest pass.
    let jar = format!("calm-session=EVIL; XSRF-TOKEN=abc; {}; t=d", stack.session);
    let path = format!("/api/plugins/{SOCKET_PLUGIN}/ws/apps/a%2Fb/stream?fps=10&x=%20");
    let (mut socket, _) = ws_connect(&stack, &path, Some(&jar), None).await.unwrap();
    let first = next_frame(&mut socket).await.unwrap().unwrap();
    assert_eq!(
        first,
        Message::text(
            "path=/apps/a%2Fb/stream query=fps=10&x=%20 cookie=XSRF-TOKEN=abc; t=d probe=1"
        )
    );
    socket.send(Message::text("ping")).await.unwrap();
    let echoed = next_frame(&mut socket).await.unwrap().unwrap();
    assert_eq!(echoed, Message::text("ping"));

    // A jar holding only calm's session reaches the plugin with no `Cookie` header at all.
    let (mut socket, _) = ws_connect(&stack, &path, Some(&stack.session), None)
        .await
        .unwrap();
    let first = next_frame(&mut socket)
        .await
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    assert!(first.contains(" cookie=- "), "{first}");
}

#[tokio::test]
async fn a_set_cookie_for_calm_session_from_the_plugin_is_dropped() {
    let stack = stack(routes::application_router).await;
    plugin_server(&stack.socket_path).await;
    stack.state.plugin.spawn(SOCKET_PLUGIN).await.unwrap();
    let path = format!("/api/plugins/{SOCKET_PLUGIN}/ws/set-cookie");
    let (_, resp) = ws_connect(&stack, &path, Some(&stack.session), None)
        .await
        .unwrap();
    let set: Vec<_> = resp.headers().get_all(header::SET_COOKIE).iter().collect();
    assert_eq!(set, ["CALM-SESSION=C", "ok=1"]);
}

#[tokio::test]
async fn not_serving_is_503_and_no_declared_socket_is_404() {
    let stack = stack(routes::application_router).await;
    let ws = |id: &str| format!("/api/plugins/{id}/ws/stream");
    let path = ws(SOCKET_PLUGIN);
    // Installed and enabled, not spawned.
    let refused = ws_connect(&stack, &path, Some(&stack.session), None)
        .await
        .err();
    assert_eq!(refused, Some(StatusCode::SERVICE_UNAVAILABLE));
    // Running, but nothing listens on the socket yet.
    stack.state.plugin.spawn(SOCKET_PLUGIN).await.unwrap();
    let refused = ws_connect(&stack, &path, Some(&stack.session), None)
        .await
        .err();
    assert_eq!(refused, Some(StatusCode::SERVICE_UNAVAILABLE));
    for id in [PLAIN_PLUGIN, "nosuchplugin"] {
        let refused = ws_connect(&stack, &ws(id), Some(&stack.session), None)
            .await
            .err();
        assert_eq!(refused, Some(StatusCode::NOT_FOUND), "{id}");
    }
    // Disabled: refused like any plugin that is not serving.
    plugin_server(&stack.socket_path).await;
    open_tunnel(&stack).await;
    stack.state.plugin.disable(SOCKET_PLUGIN).await.unwrap();
    let refused = ws_connect(&stack, &path, Some(&stack.session), None)
        .await
        .err();
    assert_eq!(refused, Some(StatusCode::SERVICE_UNAVAILABLE));
}

#[tokio::test]
async fn stop_disable_restart_and_crash_close_open_tunnels() {
    let stack = stack(routes::application_router).await;
    plugin_server(&stack.socket_path).await;
    let plugin = &stack.state.plugin;
    plugin.spawn(SOCKET_PLUGIN).await.unwrap();

    let mut socket = open_tunnel(&stack).await;
    plugin.restart(SOCKET_PLUGIN).await.unwrap();
    assert_tunnel_closed(&mut socket, "restart").await;

    let mut socket = open_tunnel(&stack).await;
    let pid = plugin.status(SOCKET_PLUGIN).await.unwrap().pid.unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    assert_tunnel_closed(&mut socket, "a crash").await;
    wait_for_running(&stack.state, SOCKET_PLUGIN).await;

    let mut socket = open_tunnel(&stack).await;
    plugin.stop(SOCKET_PLUGIN).await.unwrap();
    assert_tunnel_closed(&mut socket, "stop").await;

    plugin.spawn(SOCKET_PLUGIN).await.unwrap();
    let mut socket = open_tunnel(&stack).await;
    plugin.disable(SOCKET_PLUGIN).await.unwrap();
    assert_tunnel_closed(&mut socket, "disable").await;
}

#[tokio::test]
async fn client_forwarding_headers_never_reach_the_plugin() {
    let stack = stack(routes::application_router).await;
    plugin_server(&stack.socket_path).await;
    stack.state.plugin.spawn(SOCKET_PLUGIN).await.unwrap();
    let mut request = format!(
        "ws://{}/api/plugins/{SOCKET_PLUGIN}/ws/forwarded",
        stack.addr
    )
    .into_client_request()
    .unwrap();
    let headers = request.headers_mut();
    headers.insert("cookie", stack.session.parse().unwrap());
    for (name, value) in [
        ("forwarded", "for=6.6.6.6;host=evil"),
        ("x-forwarded-for", "6.6.6.6"),
        ("x-forwarded-host", "evil"),
        ("x-forwarded-proto", "https"),
    ] {
        headers.insert(name, value.parse().unwrap());
    }
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let first = next_frame(&mut socket).await.unwrap().unwrap();
    assert_eq!(first, Message::text("forwarded=[]"));
}

/// A plugin that answers an upgrade with a page instead of `101` must not serve it on calm's
/// origin: the client gets a bare 502, and the plugin's connection is dropped at once.
#[tokio::test]
async fn a_plugin_answer_that_is_not_101_is_a_bare_502_and_its_connection_closes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let stack = stack(routes::application_router).await;
    std::fs::create_dir_all(stack.socket_path.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&stack.socket_path).unwrap();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(socket.read_u8().await.unwrap());
        }
        let page = b"HTTP/1.1 200 OK\r\ncontent-type: text/html\r\n\
                     set-cookie: plug=1; Path=/\r\ntransfer-encoding: chunked\r\n\r\n";
        socket.write_all(page).await.unwrap();
        // An endless body: a chunk every 10 ms until the kernel stops reading.
        while socket.write_all(b"7\r\n<p>x</p\r\n").await.is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = closed_tx.send(());
    });
    stack.state.plugin.spawn(SOCKET_PLUGIN).await.unwrap();

    let stream = TcpStream::connect(stack.addr).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(conn);
    let request = axum::http::Request::get(format!("/api/plugins/{SOCKET_PLUGIN}/ws/page"))
        .header(header::HOST, stack.addr.to_string())
        .header(header::COOKIE, &stack.session)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap();
    let resp = sender.send_request(request).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    for name in [
        header::SET_COOKIE,
        header::CONTENT_TYPE,
        header::TRANSFER_ENCODING,
    ] {
        assert!(
            !resp.headers().contains_key(&name),
            "{name}: {:?}",
            resp.headers()
        );
    }
    let body = tokio::time::timeout(
        Duration::from_secs(5),
        http_body_util::BodyExt::collect(resp.into_body()),
    )
    .await
    .expect("the 502 has a finite body")
    .unwrap()
    .to_bytes();
    assert!(body.is_empty(), "{body:?}");
    tokio::time::timeout(Duration::from_secs(5), closed_rx)
        .await
        .expect("the plugin's connection must close")
        .unwrap();
}
