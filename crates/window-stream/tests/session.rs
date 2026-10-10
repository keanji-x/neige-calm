//! Viewer sessions over a fake window source and an in-memory socket.

mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use support::{CountingEncoder, FakeSource, Received, SIZE, WAIT, eventually, socket_pair};
use window_stream::protocol::ServerMessage;
use window_stream::{
    Codec, JpegEncoder, SessionEnd, SessionSummary, StreamInput, WindowSource, serve_viewer,
};

fn spawn_session(
    server: support::ServerSocket,
    source: &Arc<FakeSource>,
    encoder: Box<dyn window_stream::FrameEncoder>,
) -> tokio::task::JoinHandle<SessionSummary> {
    let source: Arc<dyn WindowSource> = source.clone();
    tokio::spawn(serve_viewer(server, source, encoder))
}

async fn finished(session: tokio::task::JoinHandle<SessionSummary>) -> SessionSummary {
    tokio::time::timeout(WAIT, session)
        .await
        .expect("the session did not end")
        .expect("the session panicked")
}

fn is_hello(message: &Option<Received>) -> bool {
    matches!(message, Some(Received::Server(ServerMessage::Hello { .. })))
}

/// A6: frames are encoded only while a viewer is connected, and the window is
/// watched only while a session runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a6_nothing_is_encoded_without_a_viewer() {
    let source = FakeSource::new("w");
    let (encoder, calls) = CountingEncoder::boxed();
    for n in 0..20 {
        source.publish(n);
    }
    assert_eq!(source.watch_calls(), 0, "no viewer, no watch");

    let (server, client) = socket_pair(64);
    let session = spawn_session(server, &source, encoder);
    eventually("the session watches", || source.live_watchers() == 1).await;
    source.publish(100);
    assert!(is_hello(&client.expect().await));
    assert_eq!(client.expect().await.and_then(|m| m.number()), Some(100));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // The viewer closes; its socket would still take messages.
    client.send_close();
    tokio::time::sleep(Duration::from_millis(50)).await;
    for n in 200..220 {
        source.publish(n);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "nothing encoded after the viewer left"
    );
    let summary = finished(session).await;
    assert_eq!(summary.end, SessionEnd::ViewerLeft);
    assert_eq!(summary.frames_sent, 1);
    assert_eq!(source.live_watchers(), 0, "the watch ends with the viewer");
}

/// A7: a viewer that never reads holds one unsent frame, keeps draining its
/// feed, and does not slow down another viewer of the same window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a7_a_stalled_viewer_holds_one_frame_and_blocks_nobody() {
    const LAST: u64 = 299;
    let source = FakeSource::new("w");

    let (stalled_server, stalled_client) = socket_pair(2);
    let (stalled_encoder, stalled_calls) = CountingEncoder::boxed();
    let stalled = spawn_session(stalled_server, &source, stalled_encoder);
    eventually("the stalled session watches", || {
        source.live_watchers() == 1
    })
    .await;
    let (active_server, active_client) = socket_pair(64);
    let (active_encoder, _) = CountingEncoder::boxed();
    let active = spawn_session(active_server, &source, active_encoder);
    eventually("the active session watches", || source.live_watchers() == 2).await;

    let reader = tokio::spawn(async move {
        let mut numbers = Vec::new();
        while let Some(message) = active_client.expect().await {
            if let Some(number) = message.number() {
                numbers.push(number);
                if number == LAST {
                    break;
                }
            }
        }
        (numbers, active_client)
    });
    for n in 0..=LAST {
        source.publish(n);
        if n % 10 == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    let (numbers, active_client) = tokio::time::timeout(WAIT, reader)
        .await
        .expect("the active viewer did not get the last frame")
        .unwrap();
    assert!(
        numbers.windows(2).all(|pair| pair[0] < pair[1]),
        "frames arrive newest-last: {numbers:?}"
    );
    assert_eq!(numbers.last(), Some(&LAST));

    // index 0 is the stalled session's watch.
    eventually("the stalled session drains its feed", || {
        source.queued(0) == 0
    })
    .await;
    eventually("the stalled session holds only the newest frame", || {
        source.alive(0) == vec![LAST]
    })
    .await;
    assert_eq!(
        stalled_client.in_flight(),
        2,
        "hello and one frame fill the socket"
    );
    assert_eq!(
        stalled_calls.load(Ordering::SeqCst),
        1,
        "nothing is encoded while the socket cannot take it"
    );

    drop(stalled_client);
    assert_eq!(finished(stalled).await.end, SessionEnd::ViewerLeft);
    assert!(
        source.alive(0).is_empty(),
        "the stalled session freed its frame"
    );
    active_client.send_close();
    assert_eq!(finished(active).await.end, SessionEnd::ViewerLeft);
}

/// A8: once the feed ends the viewer gets `closed` and then nothing but the close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a8_closed_follows_the_last_live_frame() {
    let source = FakeSource::new("w");
    let (encoder, _) = CountingEncoder::boxed();
    let (server, client) = socket_pair(64);
    let session = spawn_session(server, &source, encoder);
    eventually("the session watches", || source.live_watchers() == 1).await;
    source.publish(1);
    assert!(is_hello(&client.expect().await));
    assert_eq!(client.expect().await.and_then(|m| m.number()), Some(1));
    source.publish(2);
    assert_eq!(client.expect().await.and_then(|m| m.number()), Some(2));

    source.end();
    assert_eq!(
        client.expect().await,
        Some(Received::Server(ServerMessage::Closed))
    );
    assert_eq!(client.expect().await, Some(Received::Close));
    let summary = finished(session).await;
    assert_eq!(client.expect().await, None, "nothing after the close");
    assert_eq!(summary.end, SessionEnd::WindowGone);
    assert_eq!(summary.frames_sent, 2);
}

/// A8: a frame still unsent when the window goes away is dropped, not sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a8_a_frame_pending_when_the_window_goes_is_never_sent() {
    let source = FakeSource::new("w");
    let (encoder, _) = CountingEncoder::boxed();
    let (server, client) = socket_pair(2);
    let session = spawn_session(server, &source, encoder);
    eventually("the session watches", || source.live_watchers() == 1).await;
    source.publish(1);
    eventually("hello and frame 1 fill the socket", || {
        client.in_flight() == 2
    })
    .await;
    source.publish(2);
    eventually("frame 2 waits in the session", || source.queued(0) == 0).await;
    source.end();
    eventually("the session drops frame 2 when the window goes", || {
        source.alive(0).is_empty()
    })
    .await;

    assert!(is_hello(&client.expect().await));
    assert_eq!(client.expect().await.and_then(|m| m.number()), Some(1));
    assert_eq!(
        client.expect().await,
        Some(Received::Server(ServerMessage::Closed))
    );
    assert_eq!(client.expect().await, Some(Received::Close));
    assert_eq!(finished(session).await.end, SessionEnd::WindowGone);
    assert_eq!(client.expect().await, None);
}

/// A8: a window that is already gone gets `closed` and no frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a8_an_unavailable_window_gets_closed_only() {
    let source = FakeSource::unavailable();
    let (encoder, calls) = CountingEncoder::boxed();
    let (server, client) = socket_pair(64);
    let summary = finished(spawn_session(server, &source, encoder)).await;
    assert_eq!(summary.end, SessionEnd::WindowGone);
    assert_eq!(
        client.expect().await,
        Some(Received::Server(ServerMessage::Closed))
    );
    assert_eq!(client.expect().await, Some(Received::Close));
    assert_eq!(client.expect().await, None);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Protocol round trip: `hello` fields, then a JPEG keyframe whose header
/// parses back; a title change arrives before the next frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_then_a_jpeg_keyframe_then_title_updates() {
    let source = FakeSource::new("Example Domain");
    let (server, client) = socket_pair(64);
    let session = spawn_session(server, &source, Box::new(JpegEncoder::new(80)));
    eventually("the session watches", || source.live_watchers() == 1).await;
    source.publish(7);

    assert_eq!(
        client.expect().await,
        Some(Received::Server(ServerMessage::Hello {
            version: 1,
            codec: Codec::Jpeg,
            width: SIZE.0,
            height: SIZE.1,
            title: "Example Domain".into(),
        }))
    );
    let Some(Received::Frame(header, payload)) = client.expect().await else {
        panic!("expected a frame");
    };
    assert_eq!(header.codec, Codec::Jpeg);
    assert!(header.keyframe);
    assert_eq!((header.width, header.height), SIZE);
    assert_eq!(&payload[..2], &[0xff, 0xd8], "JPEG start of image");
    assert_eq!(
        &payload[payload.len() - 2..],
        &[0xff, 0xd9],
        "JPEG end of image"
    );

    *source.title.lock().unwrap() = "Next".into();
    source.publish(8);
    assert_eq!(
        client.expect().await,
        Some(Received::Server(ServerMessage::Title {
            title: "Next".into()
        }))
    );
    assert!(matches!(client.expect().await, Some(Received::Frame(..))));
    client.send_close();
    assert_eq!(finished(session).await.end, SessionEnd::ViewerLeft);
}

/// Client JSON is mapped to input in window pixels and evdev codes; unknown
/// codes and malformed messages are ignored and counted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_messages_reach_the_source() {
    let source = FakeSource::new("w");
    let (encoder, _) = CountingEncoder::boxed();
    let (server, client) = socket_pair(64);
    let session = spawn_session(server, &source, encoder);
    eventually("the session watches", || source.live_watchers() == 1).await;
    source.publish(1);
    assert!(is_hello(&client.expect().await));

    for message in [
        r#"{"type":"pointer","x":-3,"y":1000}"#,
        r#"{"type":"pointer","x":10.5,"y":4}"#,
        r#"{"type":"button","button":0,"pressed":true}"#,
        r#"{"type":"button","button":2,"pressed":false}"#,
        r#"{"type":"button","button":9,"pressed":true}"#,
        r#"{"type":"wheel","dx":0,"dy":120}"#,
        r#"{"type":"key","code":"KeyA","pressed":true}"#,
        r#"{"type":"key","code":"ShiftLeft","pressed":false}"#,
        r#"{"type":"key","code":"Lang1","pressed":true}"#,
        r#"{"type":"paste","text":"x"}"#,
        "not json",
    ] {
        client.send(message);
    }
    client.send_close();
    let summary = finished(session).await;

    assert_eq!(summary.end, SessionEnd::ViewerLeft);
    assert_eq!(summary.unknown_codes, 2, "button 9 and Lang1");
    assert_eq!(summary.malformed_messages, 2);
    assert_eq!(
        *source.inputs.lock().unwrap(),
        vec![
            StreamInput::Pointer {
                x: 0.0,
                y: f64::from(SIZE.1 - 1)
            },
            StreamInput::Pointer { x: 10.5, y: 4.0 },
            StreamInput::Button {
                code: 0x110,
                pressed: true
            },
            StreamInput::Button {
                code: 0x111,
                pressed: false
            },
            StreamInput::Wheel { dx: 0.0, dy: 120.0 },
            StreamInput::Key {
                evdev: 30,
                pressed: true
            },
            StreamInput::Key {
                evdev: 42,
                pressed: false
            },
        ]
    );
}
