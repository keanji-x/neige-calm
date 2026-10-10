//! The real `desktop` binary: the handshake, its private layout, the stream
//! route with a Wayland window from the fake Chrome, the tools, and its exit.
#![cfg(target_os = "linux")]

mod support;

use std::os::unix::process::ExitStatusExt;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use support::*;
use tokio_tungstenite::tungstenite::Message;
use window_stream::protocol::FrameHeader;

const SRC: &str = "/api/plugins/desktop/ws/apps/chrome/stream";

#[test]
fn the_handshake_sets_up_a_private_layout_and_no_tcp_listener() {
    let tmp = tmp();
    let data = tmp.path().join("desktop");
    std::fs::create_dir_all(data.join("home")).unwrap();
    for dir in [&data, &data.join("home")] {
        std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    let mut plugin = Plugin::spawn(&data);

    let early = plugin.request("tools/call", json!({ "name": "window_ls" }));
    assert_eq!(early["error"]["code"], -32002, "{early}");

    let reply = plugin.initialize(FAKE_CHROME);
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25", "{reply}");
    assert_eq!(
        reply["result"]["_meta"]["dev.neige/auth"]["echoed_token"],
        "tok-123"
    );

    // Every directory 0700 (also those that existed), the socket 0600.
    let runs: Vec<_> = std::fs::read_dir(data.join("run"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    for dir in [
        data.clone(),
        data.join("run"),
        runs[0].clone(),
        data.join("home"),
        data.join("profiles"),
        data.join("profiles/chrome"),
    ] {
        assert_eq!(mode(&dir), 0o700, "{}", dir.display());
    }
    let socket = plugin.socket();
    assert_eq!(mode(&socket), 0o600);
    assert_eq!(mode(&runs[0].join("wayland-0")), 0o600);

    // One route, one app.
    assert_eq!(
        http_status(&socket, &upgrade_request("/apps/firefox/stream")),
        404
    );
    assert_eq!(http_status(&socket, &upgrade_request("/apps")), 404);

    // Nothing runs yet, and nothing is cached.
    let ls = plugin.tool("window_ls", json!({})).unwrap();
    assert_eq!(
        ls["structuredContent"],
        json!({ "windows": [], "apps": [{ "app": "chrome", "running": false, "src": SRC }] })
    );
    let err = plugin.tool("page_cat", json!({})).unwrap_err();
    assert_eq!(err["code"], -32503, "{err}");
    let err = plugin
        .tool("window_cat", json!({ "window_id": 1 }))
        .unwrap_err();
    assert_eq!(err["code"], -32404, "{err}");
    let err = plugin
        .tool("window_ls", json!({ "all": true }))
        .unwrap_err();
    assert_eq!(err["code"], -32602, "{err}");
    let err = plugin.tool("window_rm", json!({})).unwrap_err();
    assert_eq!(err["code"], -32601, "{err}");

    // A1: the plugin listens on no TCP port.
    assert_eq!(tcp_listeners(plugin.pid()), Vec::<u64>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_streams_the_main_window_the_tools_drive_it_and_closed_ends_it() {
    let tmp = tmp();
    let mut plugin = Plugin::spawn(&tmp.path().join("desktop"));
    assert!(plugin.initialize(FAKE_CHROME).get("result").is_some());

    // The stream launches the app and serves its main window.
    let stream = tokio::net::UnixStream::connect(plugin.socket())
        .await
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/apps/chrome/stream", stream)
        .await
        .expect("upgrade");
    let hello = next_text(&mut ws).await;
    assert_eq!(
        hello,
        json!({ "type": "hello", "version": 1, "codec": "jpeg",
                "width": SIZE.0, "height": SIZE.1, "title": "Fake Chrome" })
    );
    let frame = next_binary(&mut ws).await;
    let (header, jpeg) = FrameHeader::parse(&frame).unwrap();
    assert_eq!((header.width, header.height), SIZE);
    let mut decoder = jpeg_decoder::Decoder::new(jpeg);
    let pixels = decoder.decode().unwrap();
    assert!(near(centre_rgb(&pixels, 640, 480), BLUE, 12));

    let fake = plugin.fake_pid();
    assert!(alive(fake));
    assert_eq!(tcp_listeners(fake), Vec::<u64>::new());

    let ls = plugin.tool("window_ls", json!({})).unwrap()["structuredContent"].clone();
    let window_id = ls["windows"][0]["window_id"].as_u64().unwrap();
    assert_eq!(
        ls,
        json!({
            "windows": [{ "window_id": window_id, "app": "chrome", "title": "Fake Chrome",
                          "width": SIZE.0, "height": SIZE.1 }],
            "apps": [{ "app": "chrome", "running": true, "src": SRC }],
        })
    );

    let cat = plugin
        .tool("window_cat", json!({ "window_id": window_id }))
        .unwrap();
    assert_eq!(cat["content"][0]["type"], "image");
    assert_eq!(cat["content"][0]["mimeType"], "image/png");
    let (w, h, rgb) = decode_png(cat["content"][0]["data"].as_str().unwrap());
    assert_eq!((w, h), (640, 480));
    assert_eq!(centre_rgb(&rgb, w, h), BLUE);
    assert_eq!(
        cat["structuredContent"],
        json!({ "window_id": window_id, "title": "Fake Chrome", "width": 640, "height": 480 })
    );

    // Input reaches the window in order; `r` makes it redraw red at its next
    // frame callback, which the read-back sees.
    let ok = plugin
        .tool(
            "window_input",
            json!({ "window_id": window_id, "events": [
                { "type": "key", "keys": "r" },
                { "type": "text", "text": "A" },
                { "type": "click", "x": 10, "y": 10 },
            ] }),
        )
        .unwrap();
    assert_eq!(ok["structuredContent"], json!({ "ok": true }));
    let cat = plugin
        .tool("window_cat", json!({ "window_id": window_id }))
        .unwrap();
    let (w, h, rgb) = decode_png(cat["content"][0]["data"].as_str().unwrap());
    assert_eq!(centre_rgb(&rgb, w, h), RED);
    let log = plugin.fake_log();
    assert_eq!(
        log,
        [
            "key 19 1",
            "key 19 0",
            "key 42 1",
            "key 30 1",
            "key 30 0",
            "key 42 0",
            "button 272 1",
            "button 272 0"
        ]
    );

    let err = plugin
        .tool(
            "window_input",
            json!({ "window_id": window_id, "events": [{ "type": "text", "text": "héllo" }] }),
        )
        .unwrap_err();
    assert_eq!(err["code"], -32602, "{err}");
    assert!(err["message"].as_str().unwrap().contains("IME"), "{err}");
    assert_eq!(plugin.fake_log().len(), log.len(), "nothing was typed");

    // The fake lists no page: refused, never guessed.
    let err = plugin.tool("page_cat", json!({})).unwrap_err();
    assert_eq!(err["code"], -32409, "{err}");
    assert_eq!(err["data"]["refusal"], "no_visible_page", "{err}");
    let err = plugin
        .tool("page_open", json!({ "url": "file:///etc/passwd" }))
        .unwrap_err();
    assert_eq!(err["code"], -32602, "{err}");

    // The window closes: `closed`, and no frame after it.
    plugin.close_fake_window();
    loop {
        match ws.next().await {
            Some(Ok(Message::Binary(_))) => continue,
            Some(Ok(Message::Text(text))) => {
                assert_eq!(
                    serde_json::from_str::<Value>(&text).unwrap(),
                    json!({ "type": "closed" })
                );
                break;
            }
            other => panic!("expected closed, got {other:?}"),
        }
    }
    match ws.next().await {
        None | Some(Ok(Message::Close(_))) | Some(Err(_)) => {}
        other => panic!("nothing may follow closed, got {other:?}"),
    }

    // The app is not running any more; the closed window is unavailable.
    wait_for(Duration::from_secs(10), || !alive(fake));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let ls = plugin.tool("window_ls", json!({})).unwrap()["structuredContent"].clone();
        if ls["apps"][0]["running"] == false && ls["windows"] == json!([]) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{ls}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let err = plugin
        .tool("window_cat", json!({ "window_id": window_id }))
        .unwrap_err();
    assert_eq!(err["code"], -32404, "{err}");

    // The next demand relaunches the app.
    std::fs::remove_file(plugin.home().join("fake-chrome.pid")).unwrap();
    std::fs::remove_file(plugin.home().join("close")).unwrap();
    let relaunched = launch_fake(&mut plugin);
    assert_ne!(relaunched, fake);
    let ls = plugin.tool("window_ls", json!({})).unwrap()["structuredContent"].clone();
    assert_eq!(ls["apps"][0]["running"], true, "{ls}");
}

#[tokio::test(flavor = "multi_thread")]
async fn stdin_eof_stops_chrome_and_ends_the_process() {
    let tmp = tmp();
    let mut plugin = Plugin::spawn(&tmp.path().join("desktop"));
    plugin.initialize(FAKE_CHROME);
    let fake = launch_fake(&mut plugin);
    let run = std::fs::read_dir(plugin.data.join("run"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    plugin.close_stdin();
    let status = plugin
        .wait_exit(Duration::from_secs(15))
        .expect("desktop exits");
    assert_eq!(status.code(), Some(0), "{status}");
    assert!(!alive(fake));
    assert!(!run.exists(), "the run dir is removed on the way out");
}

/// The kernel dies (stdin EOF, and its ends of stdout and stderr are gone):
/// the plugin still stops Chrome in order and exits cleanly.
#[tokio::test(flavor = "multi_thread")]
async fn a_dead_kernel_stops_chrome_and_ends_the_process() {
    let tmp = tmp();
    let mut plugin = Plugin::spawn_kernel_like(&tmp.path().join("desktop"));
    plugin.initialize(FAKE_CHROME);
    let fake = launch_fake(&mut plugin);
    plugin.kernel_gone();
    let status = plugin
        .wait_exit(Duration::from_secs(15))
        .expect("desktop exits");
    assert_eq!(status.code(), Some(0), "{status}");
    assert!(!alive(fake));
    let runs = std::fs::read_dir(plugin.data.join("run")).unwrap().count();
    assert_eq!(runs, 0, "the run dir is removed on the way out");
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_stops_chrome_and_ends_the_process() {
    let tmp = tmp();
    let mut plugin = Plugin::spawn(&tmp.path().join("desktop"));
    plugin.initialize(FAKE_CHROME);
    let fake = launch_fake(&mut plugin);
    // The test's own child.
    unsafe { libc::kill(plugin.pid() as i32, libc::SIGTERM) };
    let status = plugin
        .wait_exit(Duration::from_secs(15))
        .expect("desktop exits");
    assert_eq!(status.code(), Some(0), "{status}");
    assert!(!alive(fake));
}

/// A SIGKILLed desktop (the kernel's plugin stop) takes Chrome with it, and a
/// new desktop on the same work dir takes over the socket and the profile.
#[tokio::test(flavor = "multi_thread")]
async fn a_killed_desktop_takes_chrome_along_and_a_new_one_takes_over() {
    let tmp = tmp();
    let data = tmp.path().join("desktop");
    let mut first = Plugin::spawn(&data);
    first.initialize(FAKE_CHROME);
    let fake = launch_fake(&mut first);

    // Restart overlap: the new process waits while the old socket answers.
    let mut second = Plugin::spawn(&data);
    let killer = {
        let pid = first.pid();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            // The test's own child.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        })
    };
    let started = std::time::Instant::now();
    let reply = second.initialize(FAKE_CHROME);
    let waited = started.elapsed();
    killer.join().unwrap();
    assert!(reply.get("result").is_some(), "{reply}");
    // It bound only once the old socket stopped answering.
    assert!(waited >= Duration::from_millis(500), "{waited:?}");
    let status = first.wait_exit(Duration::from_secs(5)).expect("killed");
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{status}");
    wait_for(Duration::from_secs(5), || !alive(fake));

    std::fs::remove_file(second.home().join("fake-chrome.pid")).unwrap();
    let relaunched = launch_fake(&mut second);
    assert_ne!(relaunched, fake);
    assert!(alive(relaunched));
}

/// Starts the fake Chrome through `page_open`, which the fake cannot serve.
fn launch_fake(plugin: &mut Plugin) -> u32 {
    let err = plugin
        .tool("page_open", json!({ "url": "https://example.com/" }))
        .unwrap_err();
    assert_eq!(err["code"], -32409, "{err}");
    let fake = plugin.fake_pid();
    assert!(alive(fake));
    fake
}

type Ws = tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>;

async fn next_message(ws: &mut Ws) -> Message {
    tokio::time::timeout(Duration::from_secs(30), ws.next())
        .await
        .expect("a message in time")
        .expect("the stream is open")
        .expect("no transport error")
}

async fn next_text(ws: &mut Ws) -> Value {
    match next_message(ws).await {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected text, got {other:?}"),
    }
}

async fn next_binary(ws: &mut Ws) -> Vec<u8> {
    match next_message(ws).await {
        Message::Binary(bytes) => bytes.to_vec(),
        other => panic!("expected binary, got {other:?}"),
    }
}
