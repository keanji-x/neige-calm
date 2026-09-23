//! #1780 S2a: the preview gateway over real TCP — a pool port proxying to a real upstream.

use axum::body::Body;
use axum::extract::ws::{Message as WsMessage, WebSocketUpgrade};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use calm_server::ids::TrackId;
use calm_server::preview::{PreviewPorts, PreviewRegistry, gateway};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;

/// A gateway on an ephemeral pool port; `target: Some(port)` registers that upstream.
async fn gateway_for(target: Option<u16>) -> (u16, CancellationToken) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let ports = PreviewPorts::parse(&format!("{port}-{port}")).unwrap();
    let registry = Arc::new(PreviewRegistry::new(ports, 1));
    if let Some(target) = target {
        let track = TrackId::from("track-1");
        assert_eq!(registry.register(&track, "fe", "FE", target), Ok(port));
    }
    let stop = CancellationToken::new();
    gateway::serve(listener, registry, stop.clone()).unwrap();
    (port, stop)
}

async fn upstream() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let echo = |headers: HeaderMap| async move {
        let field = |name| {
            headers
                .get(name)
                .map_or("-", |v| v.to_str().unwrap())
                .to_owned()
        };
        format!(
            "host={} origin={} cookie={}",
            field(header::HOST),
            field(header::ORIGIN),
            field(header::COOKIE)
        )
    };
    let app = axum::Router::new()
        .route("/echo", get(echo))
        .route(
            "/set-cookie",
            get(|| async { [(header::SET_COOKIE, "calm-session=Y; Path=/")] }),
        )
        .route(
            "/redirect",
            get(move || async move {
                let location = format!("http://127.0.0.1:{port}/x");
                (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
            }),
        )
        .route(
            "/ws",
            get(|ws: WebSocketUpgrade| async move {
                ws.protocols(["vite-hmr"])
                    .on_upgrade(|mut socket| async move {
                        while let Some(Ok(message)) = socket.next().await {
                            if let WsMessage::Text(_) = message
                                && socket.send(message).await.is_err()
                            {
                                break;
                            }
                        }
                    })
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

async fn send(port: u16, request: Request<Body>) -> axum::http::Response<hyper::body::Incoming> {
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(conn);
    sender.send_request(request).await.unwrap()
}

async fn get_text(port: u16, path: &str, headers: &[(&str, String)]) -> (StatusCode, String) {
    let mut request = Request::get(path).header(header::HOST, format!("preview.lan:{port}"));
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let resp = send(port, request.body(Body::empty()).unwrap()).await;
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn host_is_rewritten_and_only_the_own_origin_is_translated() {
    let target = upstream().await;
    let (port, _stop) = gateway_for(Some(target)).await;
    let own = format!("http://preview.lan:{port}");
    let (status, body) = get_text(port, "/echo", &[("origin", own)]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        format!("host=127.0.0.1:{target} origin=http://127.0.0.1:{target} cookie=-")
    );
    let foreign = "http://192.168.1.5:4040".to_owned();
    let (_, body) = get_text(port, "/echo", &[("origin", foreign)]).await;
    assert!(body.contains("origin=http://192.168.1.5:4040 "), "{body}");
    // Equality, not prefix: a longer port sharing the own origin's digits is foreign.
    let longer = format!("http://preview.lan:{port}0");
    let (_, body) = get_text(port, "/echo", &[("origin", longer.clone())]).await;
    assert!(body.contains(&format!("origin={longer} ")), "{body}");
}

#[tokio::test]
async fn cookies_are_scoped_to_the_pool_port_both_ways() {
    let target = upstream().await;
    let (port, _stop) = gateway_for(Some(target)).await;
    let jar = format!(
        "calm-session=X; pv{port}_sid=a; pv{other}_sid=b; t=d",
        other = port.wrapping_add(1)
    );
    let (_, body) = get_text(port, "/echo", &[("cookie", jar)]).await;
    assert!(body.ends_with("cookie=sid=a; t=d"), "{body}");

    let request = Request::get("/set-cookie").body(Body::empty()).unwrap();
    let resp = send(port, request).await;
    let set: Vec<_> = resp.headers().get_all(header::SET_COOKIE).iter().collect();
    assert_eq!(set, [format!("pv{port}_calm-session=Y; Path=/").as_str()]);
}

#[tokio::test]
async fn upstream_absolute_location_becomes_relative() {
    let target = upstream().await;
    let (port, _stop) = gateway_for(Some(target)).await;
    let resp = send(port, Request::get("/redirect").body(Body::empty()).unwrap()).await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(resp.headers()[header::LOCATION], "/x");
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
    let (port, _stop) = gateway_for(Some(target)).await;
    let first_chunk = async {
        let resp = send(port, Request::get("/").body(Body::empty()).unwrap()).await;
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
async fn websocket_upgrade_tunnels_with_its_subprotocol() {
    let target = upstream().await;
    let (port, _stop) = gateway_for(Some(target)).await;
    let mut request = format!("ws://127.0.0.1:{port}/ws")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("sec-websocket-protocol", "vite-hmr".parse().unwrap());
    let (mut socket, resp) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(resp.headers()["sec-websocket-protocol"], "vite-hmr");
    use tokio_tungstenite::tungstenite::Message;
    socket.send(Message::text("hmr-ping")).await.unwrap();
    let echoed = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(echoed, Message::text("hmr-ping"));
}

#[tokio::test]
async fn dead_target_is_502_offline_and_unregistered_port_is_404() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = closed.local_addr().unwrap().port();
    drop(closed);
    let (port, _stop) = gateway_for(Some(dead)).await;
    let (status, body) = get_text(port, "/", &[]).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        body.contains("http-equiv=\"refresh\" content=\"3\""),
        "{body}"
    );

    let (port, _stop) = gateway_for(None).await;
    let (status, _) = get_text(port, "/", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
