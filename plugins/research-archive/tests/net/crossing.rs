//! The token crosses the SDK's five-minute margin while fetches wait for
//! their rate-limit slots. Token resolution happens once per fetch, before
//! the slot; the send goes through a snapshot client that cannot refresh.
//! So a fetch that resolved before the crossing still sends, with no OAuth
//! request inside `send()`, and a fetch that resolves after it pauses as
//! `auth_unavailable` / `refresh_timeout`. Neither is ever `transient`.

use std::time::{Duration, UNIX_EPOCH};

use research_archive::auth::{self, ClientId, DataDirTokenStorage};
use research_archive::catalog::Catalog;
use research_archive::source::{LongbridgeSource, Rps, Timeouts};
use research_archive::types::{Key, Response};

use crate::fake::{FakeServer, Reply};
use crate::hermetic;
use crate::refresh::{CLIENT, current_thread, refresh_timed_out, write_token};

#[test]
fn a_margin_crossing_during_the_slot_wait_never_refreshes_inside_send() {
    let (_serial, proxy) = hermetic::enter(true);
    let runtime = current_thread();
    let root = tempfile::tempdir().unwrap();
    let storage = DataDirTokenStorage::open(root.path().join("oauth")).unwrap();
    // Outside the margin for at most the next second; inside it from the
    // second after.
    let expires_at = auth::unix_now() + 301;
    write_token(&storage, expires_at);
    let deadline = Duration::from_secs(2);

    runtime.block_on(async {
        let server = FakeServer::start_capture(|_| Reply::data("{}")).await;
        let oauth = auth::build_oauth(&ClientId::parse(CLIENT).unwrap(), storage.clone(), deadline)
            .await
            .expect("the token is outside the margin at build");
        let source = LongbridgeSource::new(
            oauth,
            &server.url,
            Rps::new(1).unwrap(),
            Timeouts {
                token: deadline,
                fetch: deadline,
            },
        );
        let ratings = *Catalog::compiled().get("ratings").unwrap();
        let key = Key::new("AAPL.US").unwrap();

        // Four fetches resolve the token at once, then take slots at 0, 1,
        // 2 and 3 s: the later ones send after the crossing.
        let (a, b, c, d) = tokio::join!(
            source.fetch(&ratings, &key),
            source.fetch(&ratings, &key),
            source.fetch(&ratings, &key),
            source.fetch(&ratings, &key),
        );
        for fetched in [&a, &b, &c, &d] {
            assert_eq!(fetched.response(), &Response::Body(b"{}".to_vec()));
        }
        let seen = server.seen();
        assert_eq!(seen.len(), 4);
        // The snapshot client keeps the client id the SDK sends as
        // `X-Api-Key`, as the shared client would.
        for request in &seen {
            assert_eq!(request.header("x-api-key"), Some(CLIENT));
        }
        let last = seen.iter().map(|s| s.at).max().unwrap();
        let last_secs = last.duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(
            expires_at - last_secs < auth::SDK_REFRESH_MARGIN_SECS,
            "a send happened inside the margin"
        );
        // No OAuth request: nothing refreshed inside a send.
        assert!(proxy.seen().is_empty(), "{:?}", proxy.seen());

        // The next fetch resolves inside the margin: it refreshes, the
        // refresh hangs, and the fetch is an auth pause that sends nothing.
        let next = source.fetch(&ratings, &key).await;
        assert_eq!(next.response(), &refresh_timed_out());
        assert_eq!(proxy.oauth_requests(), 1);
        assert_eq!(server.seen().len(), 4);
    });
}
