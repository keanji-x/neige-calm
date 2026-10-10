//! Loopback dev harness: Chrome on the compositor, streamed to a local page.
//!
//! Usage: `dev <chrome-binary> <start-url> [--port N] [--quality Q]`
//!
//! Starts the compositor, launches Chrome on it with `start-url`, and serves
//! a viewer page at `http://127.0.0.1:<port>/` whose canvas streams Chrome's
//! most recent open window over `/stream` and sends pointer, wheel and key
//! input back. Port 0 (the default) picks a free port; the URL is printed.
//! `/stream` refuses upgrades whose `Origin` is not that page's origin.
//! `TMPDIR` must name a private directory: Chrome's profile is
//! `$TMPDIR/dev-profile` and its `HOME` (crash database, NSS state) is
//! `$TMPDIR/dev-home`; both are deleted at every start. Stop with
//! SIGINT or SIGTERM; Chrome's process group is stopped with it.
//!
//! The adapter from the compositor's `FrameWatch` to a `FrameFeed` below is the
//! prototype of the one the `desktop` plugin will own (#2530 E4). This example
//! is removed when the Report `window` block lands (#2530 E3b).
#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> dev::Res<()> {
    dev::main().await
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("the dev harness runs on Linux only");
}

#[cfg(target_os = "linux")]
mod dev {
    use std::collections::BTreeMap;
    use std::error::Error;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    use axum::Router;
    use axum::extract::State;
    use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::{Html, IntoResponse, Response};
    use axum::routing::get;
    use compositor::{
        Compositor, Config, DEFAULT_MAX_FPS, DEFAULT_SIZE, FrameWatch, InputEvent, SOCKET_NAME,
        WindowEvent, WindowId, WindowInfo,
    };
    use futures_util::future::{BoxFuture, ready};
    use futures_util::{FutureExt, SinkExt, StreamExt};
    use tokio::sync::mpsc;
    use window_stream::{
        EncodeError, EncodedFrame, Frame, FrameEncoder, FrameFeed, JpegEncoder, Rect, SourceError,
        StreamInput, WindowSource, WsMessage, serve_viewer,
    };

    pub type Res<T> = Result<T, Box<dyn Error>>;

    const PAGE: &str = include_str!("dev.html");
    /// Variables Chrome inherits from this process; everything else is dropped.
    const ENV_ALLOWLIST: [&str; 2] = ["LANG", "PATH"];
    /// How often the bridge thread checks that its viewer is still there.
    const BRIDGE_POLL: Duration = Duration::from_millis(250);
    const STATS_EVERY: Duration = Duration::from_secs(5);

    type Windows = Arc<Mutex<BTreeMap<WindowId, WindowInfo>>>;

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|p| p.into_inner())
    }

    struct Args {
        chrome: PathBuf,
        start_url: String,
        port: u16,
        quality: u8,
    }

    fn args() -> Res<Args> {
        let usage = "usage: dev <chrome-binary> <start-url> [--port N] [--quality Q]";
        let mut args = std::env::args().skip(1);
        let chrome = PathBuf::from(args.next().ok_or(usage)?);
        let start_url = args.next().ok_or(usage)?;
        let (mut port, mut quality) = (0, window_stream::DEFAULT_JPEG_QUALITY);
        while let Some(flag) = args.next() {
            let value = args.next().ok_or(usage)?;
            match flag.as_str() {
                "--port" => port = value.parse()?,
                "--quality" => quality = value.parse()?,
                _ => return Err(usage.into()),
            }
        }
        Ok(Args {
            chrome,
            start_url,
            port,
            quality,
        })
    }

    #[derive(Clone)]
    struct App {
        compositor: Compositor,
        windows: Windows,
        chrome_pid: i32,
        quality: u8,
        stats: Arc<Mutex<EncodeStats>>,
        /// The viewer page's origin; the only one `/stream` accepts.
        origin: Arc<str>,
    }

    pub async fn main() -> Res<()> {
        let args = args()?;
        // No fallback to a shared /tmp path: the profile holds cookies.
        let tmp =
            PathBuf::from(std::env::var_os("TMPDIR").ok_or("set TMPDIR to a private directory")?);
        if !tmp.is_absolute() {
            return Err("TMPDIR must be an absolute path".into());
        }
        let run_dir = tmp.join("dev-run");
        fs::create_dir_all(&run_dir)?;
        fs::set_permissions(&run_dir, fs::Permissions::from_mode(0o700))?;
        // Only this harness uses this run dir, so a leftover socket is from an earlier run.
        let _ = fs::remove_file(run_dir.join(SOCKET_NAME));
        let profile = tmp.join("dev-profile");
        let home = tmp.join("dev-home");
        for dir in [&profile, &home] {
            let _ = fs::remove_dir_all(dir);
            fs::create_dir_all(dir)?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", args.port)).await?;
        let origin: Arc<str> = format!("http://{}", listener.local_addr()?).into();

        let (compositor, events) = compositor::start(Config {
            run_dir: run_dir.clone(),
            size: DEFAULT_SIZE,
            max_fps: DEFAULT_MAX_FPS,
        })?;
        let windows: Windows = Arc::default();
        let registry = windows.clone();
        std::thread::spawn(move || {
            for event in events {
                let mut windows = lock(&registry);
                match event {
                    WindowEvent::Opened(info) => {
                        println!(
                            "window opened: {} pid {} {:?}",
                            info.id, info.pid, info.size
                        );
                        windows.insert(info.id, info);
                    }
                    WindowEvent::Changed(info) => {
                        windows.insert(info.id, info);
                    }
                    WindowEvent::Closed(id) => {
                        println!("window closed: {id}");
                        windows.remove(&id);
                    }
                }
            }
        });

        let mut chrome = launch_chrome(&args, &compositor, &run_dir, &profile, &home)?;
        let app = App {
            compositor,
            windows,
            chrome_pid: chrome.id() as i32,
            quality: args.quality,
            stats: Arc::default(),
            origin,
        };
        let result = serve(app, listener).await;
        stop(&mut chrome);
        result
    }

    fn launch_chrome(
        args: &Args,
        compositor: &Compositor,
        run_dir: &std::path::Path,
        profile: &std::path::Path,
        home: &std::path::Path,
    ) -> Res<Child> {
        let mut command = Command::new(&args.chrome);
        command.env_clear();
        for key in ENV_ALLOWLIST {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("HOME", home)
            .env("WAYLAND_DISPLAY", compositor.wayland_socket())
            .env("XDG_RUNTIME_DIR", run_dir)
            .arg("--ozone-platform=wayland")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(&args.start_url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let child = command.spawn()?;
        println!("chrome pid {}", child.id());
        Ok(child)
    }

    async fn serve(app: App, listener: tokio::net::TcpListener) -> Res<()> {
        let router = Router::new()
            .route("/", get(|| async { Html(PAGE) }))
            .route("/stream", get(stream))
            .with_state(app.clone());
        println!("listening on {}/", app.origin);
        tokio::spawn(report_stats(app.stats.clone()));
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let shutdown = async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        };
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown)
            .await?;
        println!("stopping");
        Ok(())
    }

    async fn stream(
        State(app): State<App>,
        headers: HeaderMap,
        upgrade: WebSocketUpgrade,
    ) -> Response {
        // Another site in the same browser must not drive this window.
        if headers.get(header::ORIGIN).map(|o| o.as_bytes()) != Some(app.origin.as_bytes()) {
            return (StatusCode::FORBIDDEN, "cross-origin stream refused").into_response();
        }
        upgrade
            .on_upgrade(move |socket| async move {
                let source = Arc::new(MainWindow {
                    compositor: app.compositor.clone(),
                    windows: app.windows.clone(),
                    chrome_pid: app.chrome_pid,
                    watched: Mutex::new(None),
                });
                let encoder = TimedEncoder {
                    inner: JpegEncoder::new(app.quality),
                    stats: app.stats.clone(),
                };
                let summary = serve_viewer(adapt(socket), source, Box::new(encoder)).await;
                println!("viewer session ended: {summary:?}");
            })
            .into_response()
    }

    /// axum's socket as the session's `Stream` + `Sink` of `WsMessage`.
    fn adapt(
        socket: WebSocket,
    ) -> impl futures_util::Stream<Item = Result<WsMessage, axum::Error>>
    + futures_util::Sink<WsMessage, Error = axum::Error>
    + Send
    + Unpin {
        socket
            .with(|message: WsMessage| {
                ready(Ok::<_, axum::Error>(match message {
                    WsMessage::Text(text) => Message::Text(text.into()),
                    WsMessage::Binary(bytes) => Message::Binary(bytes.into()),
                    WsMessage::Close => Message::Close(None),
                }))
            })
            .filter_map(|message| {
                ready(match message {
                    Ok(Message::Text(text)) => Some(Ok(WsMessage::Text(text.to_string()))),
                    Ok(Message::Binary(bytes)) => Some(Ok(WsMessage::Binary(bytes.to_vec()))),
                    Ok(Message::Close(_)) => Some(Ok(WsMessage::Close)),
                    Ok(Message::Ping(_) | Message::Pong(_)) => None,
                    Err(e) => Some(Err(e)),
                })
            })
    }

    /// Chrome's most recent open window, chosen when the viewer connects.
    struct MainWindow {
        compositor: Compositor,
        windows: Windows,
        chrome_pid: i32,
        watched: Mutex<Option<WindowId>>,
    }

    impl WindowSource for MainWindow {
        fn watch(&self) -> Result<Box<dyn FrameFeed>, SourceError> {
            let id = lock(&self.windows)
                .values()
                .filter(|w| w.pid == self.chrome_pid)
                .map(|w| w.id)
                .max()
                .ok_or_else(|| SourceError("Chrome has no open window".into()))?;
            let watch = self
                .compositor
                .watch(id)
                .map_err(|e| SourceError(e.to_string()))?;
            *lock(&self.watched) = Some(id);
            let (tx, rx) = mpsc::channel(1);
            std::thread::spawn(move || bridge(watch, tx));
            Ok(Box::new(WatchFeed {
                rx,
                id,
                windows: self.windows.clone(),
            }))
        }

        fn input(&self, events: Vec<StreamInput>) -> Result<(), SourceError> {
            let id = lock(&self.watched).ok_or_else(|| SourceError("no window watched".into()))?;
            let events = events
                .into_iter()
                .map(|event| match event {
                    StreamInput::Pointer { x, y } => InputEvent::Motion { x, y },
                    StreamInput::Button { code, pressed } => InputEvent::Button { code, pressed },
                    StreamInput::Wheel { dx, dy } => InputEvent::Axis { dx, dy },
                    StreamInput::Key { evdev, pressed } => InputEvent::Key { evdev, pressed },
                })
                .collect();
            self.compositor
                .input(id, events)
                .map_err(|e| SourceError(e.to_string()))
        }
    }

    /// Moves frames from the synchronous `FrameWatch` to the session. Ends when
    /// the window is gone (the channel closes, so the feed returns `None`) or
    /// the viewer left (the feed is dropped); dropping the watch is the unwatch.
    ///
    /// Harness only, not a pattern for E4: the `mpsc` between the watch and
    /// the feed can still hold a frame captured just before the window closed,
    /// and hand it out after the compositor already reported the window gone.
    /// E4's `FrameFeed` must read the `FrameWatch` slot directly when it wakes,
    /// with no queue in between, so a closed window yields `None` and never
    /// an older frame.
    fn bridge(watch: FrameWatch, tx: mpsc::Sender<Frame>) {
        loop {
            match watch.take_timeout(BRIDGE_POLL) {
                Ok(Some(frame)) => {
                    let frame = Frame {
                        size: frame.size,
                        stride: frame.stride,
                        xrgb8888: frame.xrgb8888,
                        damage: frame
                            .damage
                            .into_iter()
                            .map(|r| Rect {
                                x: r.x,
                                y: r.y,
                                width: r.width,
                                height: r.height,
                            })
                            .collect(),
                    };
                    if tx.blocking_send(frame).is_err() {
                        return;
                    }
                }
                Ok(None) if tx.is_closed() => return,
                Ok(None) => {}
                Err(_) => return,
            }
        }
    }

    struct WatchFeed {
        rx: mpsc::Receiver<Frame>,
        id: WindowId,
        windows: Windows,
    }

    impl FrameFeed for WatchFeed {
        fn next(&mut self) -> BoxFuture<'_, Option<Frame>> {
            self.rx.recv().boxed()
        }

        fn title(&self) -> String {
            lock(&self.windows)
                .get(&self.id)
                .map(|w| w.title.clone())
                .unwrap_or_default()
        }
    }

    #[derive(Default)]
    struct EncodeStats {
        frames: u64,
        bytes: u64,
        busy: Duration,
        max: Duration,
        size: (u32, u32),
    }

    /// The JPEG encoder, timed.
    struct TimedEncoder {
        inner: JpegEncoder,
        stats: Arc<Mutex<EncodeStats>>,
    }

    impl FrameEncoder for TimedEncoder {
        fn codec(&self) -> window_stream::Codec {
            self.inner.codec()
        }

        fn encode(&mut self, frame: &Frame) -> Result<EncodedFrame, EncodeError> {
            let started = Instant::now();
            let encoded = self.inner.encode(frame)?;
            let took = started.elapsed();
            let mut stats = lock(&self.stats);
            stats.frames += 1;
            stats.bytes += encoded.data.len() as u64;
            stats.busy += took;
            stats.max = stats.max.max(took);
            stats.size = frame.size;
            Ok(encoded)
        }
    }

    async fn report_stats(stats: Arc<Mutex<EncodeStats>>) {
        let mut tick = tokio::time::interval(STATS_EVERY);
        tick.tick().await;
        loop {
            tick.tick().await;
            let s = std::mem::take(&mut *lock(&stats));
            if s.frames == 0 {
                continue;
            }
            println!(
                "encode stats over {}s: {} frames ({:.1} fps) of {}x{}, mean {:.2} ms, max {:.2} ms, mean {:.1} KiB",
                STATS_EVERY.as_secs(),
                s.frames,
                s.frames as f64 / STATS_EVERY.as_secs_f64(),
                s.size.0,
                s.size.1,
                s.busy.as_secs_f64() * 1000.0 / s.frames as f64,
                s.max.as_secs_f64() * 1000.0,
                s.bytes as f64 / 1024.0 / s.frames as f64,
            );
        }
    }

    fn stop(child: &mut Child) {
        let group = format!("-{}", child.id());
        for (signal, wait) in [("-TERM", Duration::from_secs(2)), ("-KILL", Duration::ZERO)] {
            let _ = Command::new("kill").args([signal, "--", &group]).status();
            std::thread::sleep(wait);
        }
        let _ = child.wait();
    }
}
