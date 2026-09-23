//! #1780 S2a: the preview gateway over real TCP — a pool port proxying to a real upstream.

use axum::body::Body;
use axum::extract::RawQuery;
use axum::extract::ws::{Message as WsMessage, WebSocketUpgrade};
use axum::http::{HeaderMap, Request, StatusCode, Version, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::serve::ListenerExt;
use calm_server::auth::SessionAuthority;
use calm_server::ids::TrackId;
use calm_server::preview::{PreviewPorts, PreviewRegistry, gateway};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest};
use tokio_util::sync::CancellationToken;

use super::auth::live_auth_state;

struct Gw {
    port: u16,
    registry: Arc<PreviewRegistry>,
    /// `calm-session=<a live session>`.
    session: String,
    stop: CancellationToken,
}

/// A gateway on an ephemeral pool port; `target: Some(port)` registers that upstream.
async fn gateway_for(target: Option<u16>) -> Gw {
    gateway_with_timeout(target, gateway::RESPONSE_TIMEOUT).await
}

async fn gateway_with_timeout(target: Option<u16>, response_timeout: Duration) -> Gw {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let ports = PreviewPorts::parse(&format!("{port}-{port}")).unwrap();
    let registry = Arc::new(PreviewRegistry::new(ports, 1));
    if let Some(target) = target {
        let track = TrackId::from("track-1");
        assert_eq!(registry.register(&track, "fe", "FE", target), Ok(port));
    }
    let auth = live_auth_state("alice", "pw");
    let session = format!(
        "calm-session={}",
        auth.sessions.create(SessionAuthority::PasswordLogin)
    );
    let stop = CancellationToken::new();
    gateway::serve_with_response_timeout(
        listener,
        registry.clone(),
        auth,
        stop.clone(),
        response_timeout,
    )
    .unwrap();
    Gw {
        port,
        registry,
        session,
        stop,
    }
}

/// A real upstream; the counter is its number of accepted TCP connections.
async fn upstream() -> (u16, Arc<AtomicUsize>) {
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let field = |headers: &HeaderMap, name| {
        headers
            .get(name)
            .map_or("-", |v| v.to_str().unwrap())
            .to_owned()
    };
    let echo = move |version: Version, headers: HeaderMap| async move {
        format!(
            "{version:?} host={} origin={} cookie={}",
            field(&headers, header::HOST),
            field(&headers, header::ORIGIN),
            field(&headers, header::COOKIE)
        )
    };
    let set_cookie = || async {
        let mut headers = HeaderMap::new();
        for cookie in [
            "calm-session=Y; Path=/",
            " calm-session=V",
            "__Host-calm-session=Z; Path=/; Secure",
            "__Secure-calm-session=W; Secure",
            "XSRF-TOKEN=abc; Path=/",
            "Calm-Session=U",
            "calm%2Dsession=EVIL; Path=/",
            "__Secure-calm%2dsession=E; Secure",
        ] {
            headers.append(header::SET_COOKIE, cookie.parse().unwrap());
        }
        headers
    };
    let redirect = move |RawQuery(to): RawQuery| async move {
        let location = format!("http://127.0.0.1:{port}{}", to.unwrap());
        (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
    };
    // Sends the Cookie header it received, then echoes text frames.
    let ws = move |ws: WebSocketUpgrade, headers: HeaderMap| async move {
        let cookie = format!("cookie={}", field(&headers, header::COOKIE));
        ws.protocols(["vite-hmr"])
            .on_upgrade(|mut socket| async move {
                if socket.send(WsMessage::text(cookie)).await.is_err() {
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
    let app = axum::Router::new()
        .route("/echo", get(echo))
        .route("/set-cookie", get(set_cookie))
        .route("/redirect", get(redirect))
        .route("/ws", get(ws));
    let listener = listener.tap_io(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, accepted)
}

async fn send(port: u16, request: Request<Body>) -> axum::http::Response<hyper::body::Incoming> {
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(conn);
    sender.send_request(request).await.unwrap()
}

fn get_req(port: u16, path: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut request = Request::get(path).header(header::HOST, format!("preview.lan:{port}"));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.body(Body::empty()).unwrap()
}

async fn text(resp: axum::http::Response<hyper::body::Incoming>) -> (StatusCode, String) {
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

async fn get_text(gw: &Gw, path: &str, headers: &[(&str, &str)]) -> (StatusCode, String) {
    let mut all = vec![("cookie", gw.session.as_str())];
    all.extend_from_slice(headers);
    text(send(gw.port, get_req(gw.port, path, &all)).await).await
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// `Err` is the HTTP status of a refused handshake.
async fn ws_connect(
    gw: &Gw,
    cookie: Option<&str>,
) -> Result<(Ws, tungstenite::handshake::client::Response), StatusCode> {
    let mut request = format!("ws://127.0.0.1:{}/ws", gw.port)
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    headers.insert("sec-websocket-protocol", "vite-hmr".parse().unwrap());
    if let Some(cookie) = cookie {
        headers.insert("cookie", cookie.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok(connected) => Ok(connected),
        Err(tungstenite::Error::Http(resp)) => Err(resp.status()),
        Err(other) => panic!("WS handshake failed without a status: {other}"),
    }
}

async fn next_frame(socket: &mut Ws) -> Option<Result<Message, tungstenite::Error>> {
    tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("frame or close within 5 s")
}

#[tokio::test]
async fn requests_without_a_live_calm_session_never_reach_the_target() {
    let (target, accepted) = upstream().await;
    let gw = gateway_for(Some(target)).await;
    for jar in [None, Some("calm-session=forged"), Some("t=d")] {
        let headers: Vec<_> = jar.iter().map(|j| ("cookie", *j)).collect();
        let (status, body) = text(send(gw.port, get_req(gw.port, "/echo", &headers)).await).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{jar:?}");
        assert!(body.contains("Log in to calm"), "{body}");
        let refused = ws_connect(&gw, jar).await.err();
        assert_eq!(refused, Some(StatusCode::UNAUTHORIZED), "{jar:?}");
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "the target was contacted"
    );
    let (status, _) = get_text(&gw, "/echo", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ws_connect(&gw, Some(&gw.session)).await.is_ok());
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn host_is_rewritten_and_only_the_own_origin_is_translated() {
    let (target, _) = upstream().await;
    let gw = gateway_for(Some(target)).await;
    let own = format!("http://preview.lan:{}", gw.port);
    // An HTTP/1.0 client is forwarded as HTTP/1.1.
    let mut request = get_req(
        gw.port,
        "/echo",
        &[("origin", &own), ("cookie", &gw.session)],
    );
    *request.version_mut() = Version::HTTP_10;
    let (status, body) = text(send(gw.port, request).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        format!("HTTP/1.1 host=127.0.0.1:{target} origin=http://127.0.0.1:{target} cookie=-")
    );
    let (_, body) = get_text(&gw, "/echo", &[("origin", "http://192.168.1.5:4040")]).await;
    assert!(body.contains("origin=http://192.168.1.5:4040 "), "{body}");
    // Equality, not prefix: a longer port sharing the own origin's digits is foreign.
    let longer = format!("{own}0");
    let (_, body) = get_text(&gw, "/echo", &[("origin", &longer)]).await;
    assert!(body.contains(&format!("origin={longer} ")), "{body}");
}

#[tokio::test]
async fn only_calm_session_is_filtered_from_cookies_both_ways() {
    let (target, _) = upstream().await;
    let gw = gateway_for(Some(target)).await;
    // Two Cookie headers, calm's session in each position; the live one last so auth passes.
    let request = Request::get("/echo")
        .header(
            header::COOKIE,
            "calm-session=X; XSRF-TOKEN=abc; calm%2Dsession=Q",
        )
        .header(header::COOKIE, format!("t=d; {}", gw.session))
        .body(Body::empty())
        .unwrap();
    let (_, body) = text(send(gw.port, request).await).await;
    // The encoded name is not calm's cookie (auth.rs), so it passes as an ordinary one.
    assert!(
        body.ends_with(" cookie=XSRF-TOKEN=abc; calm%2Dsession=Q; t=d"),
        "{body}"
    );

    let resp = send(
        gw.port,
        get_req(gw.port, "/set-cookie", &[("cookie", &gw.session)]),
    )
    .await;
    let set: Vec<_> = resp.headers().get_all(header::SET_COOKIE).iter().collect();
    assert_eq!(set, ["XSRF-TOKEN=abc; Path=/", "Calm-Session=U"]);
}

#[tokio::test]
async fn a_jar_holding_only_calm_session_reaches_upstream_without_cookie_header() {
    let (target, _) = upstream().await;
    let gw = gateway_for(Some(target)).await;
    let (_, body) = get_text(&gw, "/echo", &[]).await;
    assert!(body.ends_with(" cookie=-"), "{body}");
    let (mut socket, _) = ws_connect(&gw, Some(&gw.session)).await.unwrap();
    let first = next_frame(&mut socket).await.unwrap().unwrap();
    assert_eq!(first, Message::text("cookie=-"));
}

#[tokio::test]
async fn loopback_location_becomes_absolute_on_the_preview_origin() {
    let (target, _) = upstream().await;
    let gw = gateway_for(Some(target)).await;
    let own = format!("http://preview.lan:{}", gw.port);
    for (to, want) in [
        ("/x", format!("{own}/x")),
        ("//example.org/x", format!("{own}//example.org/x")),
    ] {
        let path = format!("/redirect?{to}");
        let resp = send(gw.port, get_req(gw.port, &path, &[("cookie", &gw.session)])).await;
        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(resp.headers()[header::LOCATION], want.as_str());
    }
}

/// The first chunk must reach the browser while the dev server still holds the body open.
#[tokio::test]
async fn response_body_streams_before_upstream_finishes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = listener.local_addr().unwrap().port();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(socket.read_u8().await.unwrap());
        }
        let first = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nfirst\r\n";
        socket.write_all(first).await.unwrap();
        released.await.unwrap();
        socket.write_all(b"4\r\nlast\r\n0\r\n\r\n").await.unwrap();
    });
    let gw = gateway_for(Some(target)).await;
    let first_chunk = async {
        let resp = send(gw.port, get_req(gw.port, "/", &[("cookie", &gw.session)])).await;
        let mut body = resp.into_body();
        let frame = body.frame().await.unwrap().unwrap();
        (frame, body)
    };
    let (frame, body) = tokio::time::timeout(Duration::from_secs(5), first_chunk)
        .await
        .expect("first chunk must arrive while upstream holds the body open");
    assert_eq!(frame.into_data().unwrap(), "first");
    release.send(()).unwrap();
    assert_eq!(body.collect().await.unwrap().to_bytes(), "last");
}

#[tokio::test]
async fn upstream_that_never_answers_is_504() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
        drop(socket);
    });
    let gw = gateway_with_timeout(Some(target), Duration::from_millis(300)).await;
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), get_text(&gw, "/", &[]))
        .await
        .expect("the response timeout must fire");
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
}

#[tokio::test]
async fn websocket_upgrade_tunnels_with_its_subprotocol() {
    let (target, _) = upstream().await;
    let gw = gateway_for(Some(target)).await;
    let jar = format!("XSRF-TOKEN=abc; {}", gw.session);
    let (mut socket, resp) = ws_connect(&gw, Some(&jar)).await.unwrap();
    assert_eq!(resp.headers()["sec-websocket-protocol"], "vite-hmr");
    let first = next_frame(&mut socket).await.unwrap().unwrap();
    assert_eq!(first, Message::text("cookie=XSRF-TOKEN=abc"));
    socket.send(Message::text("hmr-ping")).await.unwrap();
    let echoed = next_frame(&mut socket).await.unwrap().unwrap();
    assert_eq!(echoed, Message::text("hmr-ping"));
}

#[tokio::test]
async fn unregister_closes_an_open_tunnel() {
    let (target, _) = upstream().await;
    let gw = gateway_for(Some(target)).await;
    let (mut socket, _) = ws_connect(&gw, Some(&gw.session)).await.unwrap();
    next_frame(&mut socket).await.unwrap().unwrap();
    assert_eq!(
        gw.registry.unregister(&TrackId::from("track-1"), "fe"),
        Some(gw.port)
    );
    assert_tunnel_closed(&mut socket, "unregister").await;
}

#[tokio::test]
async fn dead_target_is_502_offline_and_unregistered_port_is_404() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = closed.local_addr().unwrap().port();
    drop(closed);
    let gw = gateway_for(Some(dead)).await;
    let (status, body) = get_text(&gw, "/", &[]).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        body.contains("http-equiv=\"refresh\" content=\"3\""),
        "{body}"
    );

    let gw = gateway_for(None).await;
    let (status, _) = get_text(&gw, "/", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_set_cookie_that_is_not_ascii_is_dropped() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(socket.read_u8().await.unwrap());
        }
        let resp = b"HTTP/1.1 200 OK\r\nset-cookie: calm-session=PWN; Path=/; X=\xe9\r\n\
                     set-cookie: ok=1\r\ncontent-length: 0\r\n\r\n";
        socket.write_all(resp).await.unwrap();
    });
    let gw = gateway_for(Some(target)).await;
    let resp = send(gw.port, get_req(gw.port, "/", &[("cookie", &gw.session)])).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let set: Vec<_> = resp.headers().get_all(header::SET_COOKIE).iter().collect();
    assert_eq!(set, ["ok=1"]);
}

async fn assert_tunnel_closed(socket: &mut Ws, why: &str) {
    match next_frame(socket).await {
        None | Some(Err(_)) | Some(Ok(Message::Close(_))) => {}
        Some(Ok(other)) => panic!("tunnel still open after {why}: {other:?}"),
    }
}

#[tokio::test]
async fn reregister_keeps_the_tunnel_for_the_same_target_and_closes_it_for_another() {
    let (target, _) = upstream().await;
    let (other, _) = upstream().await;
    let gw = gateway_for(Some(target)).await;
    let track = TrackId::from("track-1");
    let (mut socket, _) = ws_connect(&gw, Some(&gw.session)).await.unwrap();
    next_frame(&mut socket).await.unwrap().unwrap();
    assert_eq!(
        gw.registry.register(&track, "fe", "FE v2", target),
        Ok(gw.port)
    );
    socket.send(Message::text("still-there")).await.unwrap();
    let echoed = next_frame(&mut socket).await.unwrap().unwrap();
    assert_eq!(echoed, Message::text("still-there"));
    assert_eq!(gw.registry.register(&track, "fe", "FE", other), Ok(gw.port));
    assert_tunnel_closed(&mut socket, "re-register to another target").await;
}

#[tokio::test]
async fn gateway_shutdown_closes_an_open_tunnel() {
    let (target, _) = upstream().await;
    let gw = gateway_for(Some(target)).await;
    let (mut socket, _) = ws_connect(&gw, Some(&gw.session)).await.unwrap();
    next_frame(&mut socket).await.unwrap().unwrap();
    gw.stop.cancel();
    assert_tunnel_closed(&mut socket, "gateway shutdown").await;
}
