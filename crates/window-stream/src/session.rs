//! One viewer session: frames out, input back, latest wins.

use std::fmt::Display;
use std::future::poll_fn;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::{Sink, SinkExt, Stream, StreamExt};

use tokio::sync::Notify;
use tokio::time::{Instant, sleep_until};

use crate::frame::{EncodedFrame, Frame, FrameEncoder, Rect};
use crate::input::{Ignored, StreamInput, translate};
use crate::protocol::{ClientMessage, FrameHeader, ServerMessage, VERSION};

/// How long a session whose window is gone keeps trying to deliver `closed`.
const CLOSE_GRACE: Duration = Duration::from_secs(5);
/// Above this many rectangles merged damage collapses to the full frame.
const MAX_DAMAGE_RECTS: usize = 32;

/// The WebSocket messages a session uses. Transports map their own message
/// type to this one and handle ping/pong themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

/// A source failed or its window is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SourceError(pub String);

/// One watcher's frames of one window. Dropping the feed ends the watch.
pub trait FrameFeed: Send {
    /// The next frame, waiting until there is one. `None` means the window is
    /// gone, and is final. The future must be cancel safe.
    fn next(&mut self) -> BoxFuture<'_, Option<Frame>>;
    /// The window title now.
    fn title(&self) -> String;
}

/// One window: implemented over the compositor by the binary that wires them.
pub trait WindowSource: Send + Sync {
    /// Starts watching. The first frame of a feed is a full frame. Called once
    /// per session, when the viewer connects.
    fn watch(&self) -> Result<Box<dyn FrameFeed>, SourceError>;
    /// Delivers input to the window. Called on the session's task, so it must
    /// return promptly.
    fn input(&self, events: Vec<StreamInput>) -> Result<(), SourceError>;
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    /// The window was unavailable or went away; `closed` was sent if the socket took it.
    WindowGone,
    /// The viewer closed the socket.
    ViewerLeft,
    /// The socket failed.
    Transport(String),
    /// The encoder failed; the socket was dropped without `closed`.
    Encoder(String),
}

/// What a finished session did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub end: SessionEnd,
    pub frames_sent: u64,
    /// `key` codes and `button` indices that have no evdev mapping; ignored.
    pub unknown_codes: u64,
    /// Client messages that are not valid protocol JSON; ignored.
    pub malformed_messages: u64,
}

/// Serves one viewer until the window is gone or the viewer leaves.
///
/// The session watches `source` only while it runs. It encodes a frame only
/// when the socket can take the next message; frames that arrive meanwhile
/// replace each other, so it holds at most one unsent frame. It sends `hello`
/// right before the first frame, and `closed` as its last message once the
/// feed ends. This function is the only transport entry point.
pub async fn serve_viewer<S, E>(
    socket: S,
    source: Arc<dyn WindowSource>,
    encoder: Box<dyn FrameEncoder>,
) -> SessionSummary
where
    S: Stream<Item = Result<WsMessage, E>> + Sink<WsMessage, Error = E> + Send + Unpin,
    E: Display,
{
    let (mut stream, mut sink) = crate::socket::split(socket);
    let mut summary = SessionSummary {
        end: SessionEnd::WindowGone,
        frames_sent: 0,
        unknown_codes: 0,
        malformed_messages: 0,
    };
    let mut feed = match source.watch() {
        Ok(feed) => feed,
        Err(e) => {
            tracing::debug!("window unavailable at session start: {e}");
            let _ = tokio::time::timeout(CLOSE_GRACE, send_closed(&mut sink)).await;
            return summary;
        }
    };

    let latest = Latest::default();
    let end = {
        let pump = async {
            while let Some(frame) = feed.next().await {
                latest.offer(frame, feed.title());
            }
            latest.close();
        };
        let writer = write(&mut sink, &latest, encoder, &mut summary.frames_sent);
        let reader = read(
            &mut stream,
            &latest,
            &*source,
            &mut summary.unknown_codes,
            &mut summary.malformed_messages,
        );
        tokio::pin!(pump, writer, reader);
        let grace = sleep_until(Instant::now() + Duration::from_secs(86_400 * 365));
        tokio::pin!(grace);
        let mut pump_done = false;
        loop {
            tokio::select! {
                biased;
                end = &mut writer => break end,
                () = &mut pump, if !pump_done => {
                    pump_done = true;
                    grace.as_mut().reset(Instant::now() + CLOSE_GRACE);
                }
                end = &mut reader => break end,
                () = &mut grace, if pump_done => break SessionEnd::WindowGone,
            }
        }
    };
    summary.end = end;
    summary
}

/// The newest unsent frame and title of one session.
#[derive(Default)]
struct Latest {
    state: Mutex<Pending>,
    wake: Notify,
}

#[derive(Default)]
struct Pending {
    frame: Option<Frame>,
    /// Size of the newest frame from the feed, sent or not.
    size: Option<(u32, u32)>,
    title: String,
    title_changed: bool,
    gone: bool,
}

impl Latest {
    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Replaces the unsent frame with `frame`, keeping the damage of both.
    fn offer(&self, mut frame: Frame, title: String) {
        let mut state = self.lock();
        if let Some(previous) = state.frame.take() {
            if previous.size == frame.size {
                let mut damage = previous.damage;
                damage.append(&mut frame.damage);
                frame.damage = damage;
            } else {
                frame.damage = vec![full(frame.size)];
            }
        }
        if frame.damage.len() > MAX_DAMAGE_RECTS {
            frame.damage = vec![full(frame.size)];
        }
        state.size = Some(frame.size);
        state.frame = Some(frame);
        if state.title != title {
            state.title = title;
            state.title_changed = true;
        }
        drop(state);
        self.wake.notify_one();
    }

    /// The window is gone: the unsent frame is dropped, never sent.
    fn close(&self) {
        let mut state = self.lock();
        state.gone = true;
        state.frame = None;
        drop(state);
        self.wake.notify_one();
    }

    fn size(&self) -> Option<(u32, u32)> {
        self.lock().size
    }

    fn is_gone(&self) -> bool {
        self.lock().gone
    }

    /// Waits until there is something to send: the window is gone, a frame
    /// is pending, or (after `hello`) the title changed.
    async fn work(&self, hello_sent: bool) {
        loop {
            {
                let state = self.lock();
                if state.gone || state.frame.is_some() || (hello_sent && state.title_changed) {
                    return;
                }
            }
            self.wake.notified().await;
        }
    }
}

fn full(size: (u32, u32)) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: size.0,
        height: size.1,
    }
}

async fn write<K, E>(
    sink: &mut K,
    latest: &Latest,
    encoder: Box<dyn FrameEncoder>,
    frames_sent: &mut u64,
) -> SessionEnd
where
    K: Sink<WsMessage, Error = E> + Unpin,
    E: Display,
{
    let transport = |e: E| SessionEnd::Transport(e.to_string());
    let codec = encoder.codec();
    let mut encoder = Some(encoder);
    let mut sent_size = None;
    loop {
        latest.work(sent_size.is_some()).await;
        // Take nothing until the socket can take the next message: frames
        // that arrive meanwhile replace each other in `latest`.
        if let Err(e) = poll_fn(|cx| sink.poll_ready_unpin(cx)).await {
            return transport(e);
        }
        let next = {
            let mut state = latest.lock();
            if state.gone {
                Next::Closed
            } else if sent_size.is_none() {
                state.title_changed = false;
                match state.frame.as_ref() {
                    Some(frame) => Next::Text(ServerMessage::Hello {
                        version: VERSION,
                        codec,
                        width: frame.size.0,
                        height: frame.size.1,
                        title: state.title.clone(),
                    }),
                    None => Next::Frame,
                }
            } else if state.title_changed {
                state.title_changed = false;
                Next::Text(ServerMessage::Title {
                    title: state.title.clone(),
                })
            } else {
                Next::Frame
            }
        };
        match next {
            Next::Closed => {
                let _ = send_closed(sink).await;
                return SessionEnd::WindowGone;
            }
            Next::Text(text) => {
                if let Err(e) = sink.feed(WsMessage::Text(text.to_json())).await {
                    return transport(e);
                }
                if let Err(e) = poll_fn(|cx| sink.poll_ready_unpin(cx)).await {
                    return transport(e);
                }
            }
            Next::Frame => {}
        }
        let Some(frame) = latest.lock().frame.take() else {
            if let Err(e) = sink.flush().await {
                return transport(e);
            }
            continue;
        };
        if sent_size == Some(frame.size) && frame.damage.is_empty() {
            continue;
        }
        let size = frame.size;
        let Some(owned) = encoder.take() else {
            return SessionEnd::Encoder("encoder lost".into());
        };
        let (owned, encoded) = match encode(owned, frame).await {
            Ok(done) => done,
            Err(e) => return SessionEnd::Encoder(e),
        };
        encoder = Some(owned);
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(e) => return SessionEnd::Encoder(e.to_string()),
        };
        if latest.is_gone() {
            // The window went away while this frame was encoded: never send it.
            continue;
        }
        let message = frame_message(codec, size, &encoded);
        if let Err(e) = sink.start_send_unpin(WsMessage::Binary(message)) {
            return transport(e);
        }
        if let Err(e) = sink.flush().await {
            return transport(e);
        }
        sent_size = Some(size);
        *frames_sent += 1;
    }
}

enum Next {
    Closed,
    Text(ServerMessage),
    Frame,
}

type Encoded = Result<EncodedFrame, crate::frame::EncodeError>;

/// Runs the encoder off the async workers; it hands the encoder back.
async fn encode(
    mut encoder: Box<dyn FrameEncoder>,
    frame: Frame,
) -> Result<(Box<dyn FrameEncoder>, Encoded), String> {
    tokio::task::spawn_blocking(move || {
        let encoded = encoder.encode(&frame);
        (encoder, encoded)
    })
    .await
    .map_err(|e| format!("encoder task failed: {e}"))
}

fn frame_message(codec: crate::frame::Codec, size: (u32, u32), encoded: &EncodedFrame) -> Vec<u8> {
    FrameHeader {
        codec,
        keyframe: encoded.keyframe,
        width: size.0,
        height: size.1,
    }
    .message(&encoded.data)
}

async fn send_closed<K, E>(sink: &mut K) -> Result<(), E>
where
    K: Sink<WsMessage, Error = E> + Unpin,
{
    sink.feed(WsMessage::Text(ServerMessage::Closed.to_json()))
        .await?;
    sink.feed(WsMessage::Close).await?;
    sink.flush().await
}

async fn read<R, E>(
    stream: &mut R,
    latest: &Latest,
    source: &dyn WindowSource,
    unknown_codes: &mut u64,
    malformed: &mut u64,
) -> SessionEnd
where
    R: Stream<Item = Result<WsMessage, E>> + Unpin,
    E: Display,
{
    while let Some(message) = stream.next().await {
        let text = match message {
            Err(e) => return SessionEnd::Transport(e.to_string()),
            Ok(WsMessage::Close) => return SessionEnd::ViewerLeft,
            Ok(WsMessage::Binary(_)) => {
                *malformed += 1;
                continue;
            }
            Ok(WsMessage::Text(text)) => text,
        };
        let message = match serde_json::from_str::<ClientMessage>(&text) {
            Ok(message) => message,
            Err(e) => {
                tracing::debug!("ignoring malformed client message: {e}");
                *malformed += 1;
                continue;
            }
        };
        match translate(message, latest.size()) {
            Ok(input) => {
                if let Err(e) = source.input(vec![input]) {
                    tracing::debug!("input not delivered: {e}");
                }
            }
            Err(Ignored::UnknownKey(code)) => {
                tracing::debug!("ignoring key code {code:?} with no evdev mapping");
                *unknown_codes += 1;
            }
            Err(Ignored::UnknownButton(button)) => {
                tracing::debug!("ignoring mouse button {button} with no evdev mapping");
                *unknown_codes += 1;
            }
            Err(Ignored::NoSize) => {}
        }
    }
    SessionEnd::ViewerLeft
}
