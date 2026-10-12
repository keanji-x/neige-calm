//! A local fake of the Longbridge OpenAPI HTTP server, on 127.0.0.1.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Response, Uri},
};
use tokio::net::TcpListener;

/// One request the fake received.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: Method,
    pub path: String,
    pub query: Option<String>,
    pub headers: HeaderMap,
    /// Wall time of arrival.
    pub at: std::time::SystemTime,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// Status, extra headers, body, and what to wait for before replying: a
/// delay, or a gate the test opens.
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
    pub delay: Option<Duration>,
    pub gate: Option<Arc<tokio::sync::Semaphore>>,
}

impl Reply {
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![],
            body: body.into(),
            delay: None,
            gate: None,
        }
    }

    /// Reply only once `gate` has a permit for this request: the test opens
    /// it with `add_permits`, and permits added before the request waits
    /// still count.
    pub fn gated(mut self, gate: &Arc<tokio::sync::Semaphore>) -> Self {
        self.gate = Some(Arc::clone(gate));
        self
    }

    /// Reply only after `delay` (on the tokio clock).
    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    /// A `code: 0` envelope around `data`, given as raw JSON text.
    pub fn data(data: &str) -> Self {
        Self::new(200, format!(r#"{{"code":0,"message":"","data":{data}}}"#))
    }

    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }
}

pub struct FakeServer {
    pub url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeServer {
    pub async fn start(respond: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let respond = Arc::new(respond);
        let log = seen.clone();
        let app = Router::new().fallback(move |method: Method, uri: Uri, headers: HeaderMap| {
            let respond = respond.clone();
            let log = log.clone();
            async move {
                let request = Seen {
                    method,
                    path: uri.path().to_owned(),
                    query: uri.query().map(str::to_owned),
                    headers,
                    at: std::time::SystemTime::now(),
                };
                let reply = respond(&request);
                log.lock().unwrap().push(request);
                if let Some(gate) = &reply.gate {
                    gate.acquire().await.unwrap().forget();
                }
                if let Some(delay) = reply.delay {
                    tokio::time::sleep(delay).await;
                }
                let mut response = Response::builder().status(reply.status);
                for (name, value) in reply.headers {
                    response = response.header(name, value);
                }
                response.body(Body::from(reply.body)).unwrap()
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, seen }
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}
