//! Fakes for session tests: a window source that a test publishes frames to,
//! an in-memory WebSocket with a bounded send buffer, and a counting encoder.

use std::collections::VecDeque;
use std::fmt;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::{FutureExt, Sink, Stream};
use tokio::sync::{Notify, mpsc};
use window_stream::protocol::{FrameHeader, ServerMessage};
use window_stream::{
    Codec, EncodeError, EncodedFrame, Frame, FrameEncoder, FrameFeed, Rect, SourceError,
    StreamInput, WindowSource, WsMessage,
};

pub const SIZE: (u32, u32) = (64, 32);
pub const WAIT: Duration = Duration::from_secs(10);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A frame whose first eight pixel bytes carry `number`; damage is the full frame.
pub fn numbered_frame(number: u64) -> Frame {
    let mut pixels = vec![0u8; (SIZE.0 * SIZE.1 * 4) as usize];
    pixels[..8].copy_from_slice(&number.to_le_bytes());
    Frame {
        size: SIZE,
        stride: SIZE.0 * 4,
        xrgb8888: Arc::from(pixels),
        damage: vec![Rect {
            x: 0,
            y: 0,
            width: SIZE.0,
            height: SIZE.1,
        }],
    }
}

/// One watch on the fake source.
struct Watcher {
    /// `None` once the window is gone.
    tx: Option<mpsc::UnboundedSender<Frame>>,
    /// Frames handed to this watcher and not yet taken by its feed.
    queued: Arc<AtomicUsize>,
    /// Every frame handed to this watcher, by number.
    delivered: Vec<(u64, Weak<[u8]>)>,
}

/// A window whose frames the test publishes. Every watcher gets its own copy
/// of each frame, so the frames a session still holds can be counted.
#[derive(Default)]
pub struct FakeSource {
    watchers: Mutex<Vec<Watcher>>,
    pub title: Arc<Mutex<String>>,
    pub inputs: Mutex<Vec<StreamInput>>,
    /// When set, `watch` fails as for a window that is already gone.
    unavailable: bool,
    watch_calls: AtomicUsize,
}

impl FakeSource {
    pub fn new(title: &str) -> Arc<Self> {
        let source = Self::default();
        *lock(&source.title) = title.to_string();
        Arc::new(source)
    }

    /// A window that is already gone: every `watch` fails.
    pub fn unavailable() -> Arc<Self> {
        Arc::new(Self {
            unavailable: true,
            ..Self::default()
        })
    }

    /// Hands frame `number` to every live watcher. Never waits.
    pub fn publish(&self, number: u64) {
        for watcher in lock(&self.watchers).iter_mut() {
            let Some(tx) = &watcher.tx else {
                continue;
            };
            let frame = numbered_frame(number);
            let weak = Arc::downgrade(&frame.xrgb8888);
            watcher.queued.fetch_add(1, Ordering::SeqCst);
            if tx.send(frame).is_ok() {
                watcher.delivered.push((number, weak));
            } else {
                watcher.queued.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    /// Ends every feed: the window is gone.
    pub fn end(&self) {
        for watcher in lock(&self.watchers).iter_mut() {
            watcher.tx = None;
        }
    }

    pub fn watch_calls(&self) -> usize {
        self.watch_calls.load(Ordering::SeqCst)
    }

    /// Watchers whose feed is still alive.
    pub fn live_watchers(&self) -> usize {
        lock(&self.watchers)
            .iter()
            .filter(|w| w.tx.as_ref().is_some_and(|tx| !tx.is_closed()))
            .count()
    }

    /// Frames handed to watcher `index` and not yet taken by its feed.
    pub fn queued(&self, index: usize) -> usize {
        lock(&self.watchers)[index].queued.load(Ordering::SeqCst)
    }

    /// Numbers of the frames handed to watcher `index` that are still in memory.
    pub fn alive(&self, index: usize) -> Vec<u64> {
        lock(&self.watchers)[index]
            .delivered
            .iter()
            .filter(|(_, weak)| weak.strong_count() > 0)
            .map(|(number, _)| *number)
            .collect()
    }
}

struct FakeFeed {
    rx: mpsc::UnboundedReceiver<Frame>,
    queued: Arc<AtomicUsize>,
    title: Arc<Mutex<String>>,
}

impl FrameFeed for FakeFeed {
    fn next(&mut self) -> BoxFuture<'_, Option<Frame>> {
        async move {
            let frame = self.rx.recv().await;
            if frame.is_some() {
                self.queued.fetch_sub(1, Ordering::SeqCst);
            }
            frame
        }
        .boxed()
    }

    fn title(&self) -> String {
        lock(&self.title).clone()
    }
}

impl WindowSource for FakeSource {
    fn watch(&self) -> Result<Box<dyn FrameFeed>, SourceError> {
        self.watch_calls.fetch_add(1, Ordering::SeqCst);
        if self.unavailable {
            return Err(SourceError("window 1 is gone".into()));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        lock(&self.watchers).push(Watcher {
            tx: Some(tx),
            queued: queued.clone(),
            delivered: Vec::new(),
        });
        Ok(Box::new(FakeFeed {
            rx,
            queued,
            title: self.title.clone(),
        }))
    }

    fn input(&self, events: Vec<StreamInput>) -> Result<(), SourceError> {
        lock(&self.inputs).extend(events);
        Ok(())
    }
}

/// Counts calls; the payload is the frame number from [`numbered_frame`].
pub struct CountingEncoder {
    calls: Arc<AtomicUsize>,
}

impl CountingEncoder {
    pub fn boxed() -> (Box<dyn FrameEncoder>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Box::new(Self {
                calls: calls.clone(),
            }),
            calls,
        )
    }
}

impl FrameEncoder for CountingEncoder {
    fn codec(&self) -> Codec {
        Codec::Jpeg
    }

    fn encode(&mut self, frame: &Frame) -> Result<EncodedFrame, EncodeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(EncodedFrame {
            keyframe: true,
            data: frame.xrgb8888[..8].to_vec(),
        })
    }
}

#[derive(Debug)]
pub struct SocketError;

impl fmt::Display for SocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("test socket failed")
    }
}

/// Server → client messages in flight, at most `capacity` of them.
struct Outbox {
    queue: VecDeque<WsMessage>,
    capacity: usize,
    /// The session's sink, waiting for room.
    sender: Option<Waker>,
    server_gone: bool,
}

struct Shared {
    outbox: Mutex<Outbox>,
    arrived: Notify,
}

/// The session's end of an in-memory WebSocket.
pub struct ServerSocket {
    inbound: mpsc::UnboundedReceiver<WsMessage>,
    shared: Arc<Shared>,
}

/// The viewer's end. Dropping it closes the server's inbound stream.
pub struct ClientSocket {
    outbound: Option<mpsc::UnboundedSender<WsMessage>>,
    shared: Arc<Shared>,
}

/// A socket pair whose server side can have `capacity` unread messages in
/// flight before its sink stops taking more, like a full TCP send buffer.
pub fn socket_pair(capacity: usize) -> (ServerSocket, ClientSocket) {
    let (tx, rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        outbox: Mutex::new(Outbox {
            queue: VecDeque::new(),
            capacity,
            sender: None,
            server_gone: false,
        }),
        arrived: Notify::new(),
    });
    (
        ServerSocket {
            inbound: rx,
            shared: shared.clone(),
        },
        ClientSocket {
            outbound: Some(tx),
            shared,
        },
    )
}

impl Stream for ServerSocket {
    type Item = Result<WsMessage, SocketError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inbound.poll_recv(cx).map(|m| m.map(Ok))
    }
}

impl Sink<WsMessage> for ServerSocket {
    type Error = SocketError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), SocketError>> {
        let mut outbox = lock(&self.shared.outbox);
        if outbox.queue.len() < outbox.capacity {
            Poll::Ready(Ok(()))
        } else {
            outbox.sender = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    fn start_send(self: Pin<&mut Self>, item: WsMessage) -> Result<(), SocketError> {
        lock(&self.shared.outbox).queue.push_back(item);
        self.shared.arrived.notify_one();
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), SocketError>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), SocketError>> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for ServerSocket {
    fn drop(&mut self) {
        lock(&self.shared.outbox).server_gone = true;
        self.shared.arrived.notify_one();
    }
}

/// What the viewer received.
#[derive(Debug, Clone, PartialEq)]
pub enum Received {
    Server(ServerMessage),
    Frame(FrameHeader, Vec<u8>),
    Close,
}

impl Received {
    /// The frame number of a frame encoded by [`CountingEncoder`].
    pub fn number(&self) -> Option<u64> {
        match self {
            Received::Frame(_, payload) => Some(u64::from_le_bytes(payload[..8].try_into().ok()?)),
            _ => None,
        }
    }
}

impl ClientSocket {
    pub fn send(&self, text: &str) {
        if let Some(tx) = &self.outbound {
            let _ = tx.send(WsMessage::Text(text.to_string()));
        }
    }

    pub fn send_close(&self) {
        if let Some(tx) = &self.outbound {
            let _ = tx.send(WsMessage::Close);
        }
    }

    /// The next message, or `None` once the server side is gone and drained.
    pub async fn recv(&self) -> Option<Received> {
        loop {
            {
                let mut outbox = lock(&self.shared.outbox);
                if let Some(message) = outbox.queue.pop_front() {
                    if let Some(waker) = outbox.sender.take() {
                        waker.wake();
                    }
                    return Some(decode(message));
                }
                if outbox.server_gone {
                    return None;
                }
            }
            self.shared.arrived.notified().await;
        }
    }

    /// Like [`recv`](Self::recv) but fails the test after [`WAIT`].
    pub async fn expect(&self) -> Option<Received> {
        tokio::time::timeout(WAIT, self.recv())
            .await
            .expect("no message from the session in time")
    }

    /// Messages currently in flight to this viewer.
    pub fn in_flight(&self) -> usize {
        lock(&self.shared.outbox).queue.len()
    }
}

fn decode(message: WsMessage) -> Received {
    match message {
        WsMessage::Text(text) => Received::Server(
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("bad server text {text:?}: {e}")),
        ),
        WsMessage::Binary(bytes) => {
            let (header, payload) = FrameHeader::parse(&bytes).expect("frame header");
            Received::Frame(header, payload.to_vec())
        }
        WsMessage::Close => Received::Close,
    }
}

/// Polls `check` every few milliseconds until it holds or [`WAIT`] passes.
pub async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !check() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
