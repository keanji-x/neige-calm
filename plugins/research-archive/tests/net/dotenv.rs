//! A `.env` above the working directory cannot steer an in-process test.
//!
//! The SDK runs `dotenv()` when the source builds its snapshot client, which
//! loads the `.env` here. The `.env` sets what would redirect a refresh: the staging OAuth
//! host (`LONGBRIDGE_ENV`), the mainland host (`LONGBRIDGE_REGION`) and a
//! different proxy. Each of those, if it took effect, still lands on a
//! local listener or a closed local port, so a failure of this test cannot
//! reach a real host. `NO_PROXY=*` is left out for that reason: if it took
//! effect the refresh would connect to the real host directly. It is pinned
//! by the same `set_var` as `LONGBRIDGE_ENV`, and dotenv's rule (it never
//! overrides a set variable) is the same for both.

use std::{fs, time::Duration};

use research_archive::auth::{self, ClientId, DataDirTokenStorage};
use research_archive::source::{LongbridgeSource, Rps};
use research_archive::types::{FailureClass, Response};

use research_archive::env_guard::{self, Refusal};

use crate::dotenv_reach::assert_no_dotenv_reach;
use crate::hermetic;
use crate::refresh::{
    CLIENT, DEADLINE, TIMEOUTS, current_thread, fetch_once, refresh_timed_out, write_token,
};

#[test]
fn a_dotenv_in_reach_is_refused_and_cannot_override_the_pins() {
    let (_serial, proxy) = hermetic::enter(true);
    let original = std::env::current_dir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    fs::write(
        root.path().join(".env"),
        "LONGBRIDGE_ENV=staging\nLONGBRIDGE_REGION=CN\nHTTPS_PROXY=http://127.0.0.1:9\n\
         RESEARCH_ARCHIVE_DOTENV_PROBE=loaded\n",
    )
    .unwrap();
    std::env::set_current_dir(&work).unwrap();

    // The guard every other in-process test runs refuses here.
    assert_eq!(
        env_guard::check(std::iter::empty(), &work),
        Err(Refusal::DotEnv(root.path().to_owned()))
    );
    assert!(std::panic::catch_unwind(assert_no_dotenv_reach).is_err());

    // Without the guard, the pins alone keep the refresh on the proxy. The
    // first fetch, before the margin, builds the snapshot client, which is
    // where the SDK runs `dotenv()`; its send fails on the closed port.
    let runtime = current_thread();
    let storage = DataDirTokenStorage::open(root.path().join("oauth")).unwrap();
    write_token(&storage, auth::unix_now() + 302);
    let oauth = runtime
        .block_on(auth::build_oauth(
            &ClientId::parse(CLIENT).unwrap(),
            storage.clone(),
            DEADLINE,
        ))
        .expect("the token is outside the margin at build");
    let source =
        LongbridgeSource::new(oauth, "http://127.0.0.1:1", Rps::new(20).unwrap(), TIMEOUTS);
    let first = fetch_once(&runtime, &source);
    assert!(
        matches!(
            first.response(),
            Response::Failed {
                class: FailureClass::Transient,
                ..
            }
        ),
        "{first:?}"
    );
    assert_eq!(
        std::env::var("RESEARCH_ARCHIVE_DOTENV_PROBE").as_deref(),
        Ok("loaded"),
        "the SDK loaded the .env"
    );
    // Then the token enters the margin and the next fetch refreshes.
    std::thread::sleep(Duration::from_secs(3));
    let fetched = fetch_once(&runtime, &source);
    std::env::set_current_dir(original).unwrap();

    assert_eq!(fetched.response(), &refresh_timed_out());
    assert_eq!(proxy.seen(), [hermetic::OAUTH_CONNECT]);
    assert_eq!(std::env::var("LONGBRIDGE_ENV").as_deref(), Ok(""));
    assert_eq!(std::env::var("NO_PROXY").as_deref(), Ok(hermetic::LOOPBACK));
    assert_eq!(std::env::var("LONGBRIDGE_REGION").as_deref(), Ok("global"));
    assert_eq!(
        std::env::var("HTTPS_PROXY").as_deref(),
        Ok(proxy.url.as_str())
    );
}
