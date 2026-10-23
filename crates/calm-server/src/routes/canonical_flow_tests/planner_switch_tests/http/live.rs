//! The browser's track subscription, over the authenticated application router.
use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{WebSocketStream, tungstenite::client::IntoClientRequest};

type Socket = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
pub(super) struct Events {
    socket: Socket,
    server: tokio::task::JoinHandle<()>,
}
impl Events {
    pub(super) async fn open(p: &Pair) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = p.app.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let mut request = format!("ws://{addr}/api/events")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(header::COOKIE, p.cookie.parse().unwrap());
        let (socket, response) = tokio_tungstenite::connect_async(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        let mut events = Self { socket, server };
        // Replay completion acknowledges that the real handler installed the subscription.
        // Start at the persisted tip so creation/initial-turn events are not observations
        // of this switch; all following frames exercise live delivery.
        let tip: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id),0) FROM events")
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap();
        events
            .socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"sub":[format!("track:{}",p.track)],"since":tip}).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(events.next().await["ev"], "_replay_complete");
        events
    }
    async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = self.socket.next().await.unwrap().unwrap();
                if let tokio_tungstenite::tungstenite::Message::Text(text) = frame {
                    return serde_json::from_str(&text).unwrap();
                }
            }
        })
        .await
        .expect("product event outlet must deliver the switch")
    }
    pub(super) async fn switched(&mut self, p: &Pair, receipt: &Value) {
        let detail = detail(p).await;
        assert_eq!(detail["planner_revision"], receipt["planner_revision"]);
        let mut previous = 0;
        for (index, provider_key) in [(0, "assistant_provider"), (1, "planner_provider")] {
            let event = self.next().await;
            assert_eq!(event["ev"], "card.updated", "{event}");
            let id = event["_id"].as_i64().unwrap();
            assert!(id > previous, "demotion must precede promotion: {event}");
            previous = id;
            let card = &event["data"];
            assert_eq!(card["id"], p.cards[index]);
            assert_eq!(card["payload"]["planner_harness"] == true, index == 1);
            if index == 0 {
                assert_eq!(card["payload"]["harness_profile"], "assistant");
            }
            let stored = detail["cards"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == p.cards[index])
                .unwrap();
            assert_eq!(card["payload"], stored["payload"]);
            assert_eq!(card["payload"][provider_key], "codex");
        }
    }
}
impl Drop for Events {
    fn drop(&mut self) {
        self.server.abort();
    }
}
