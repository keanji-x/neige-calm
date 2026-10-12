//! Every public async method of `Store` returns a `Send` future, so the
//! capture job can drive the Store from spawned tasks.

use research_archive::Store;
use research_archive::types::{Finish, RunId, Source, UnixMs};

use crate::support::{as_of, at, endpoint, fresh, key, ok, plan_request, window};

fn assert_send<T: Send>(_: T) {}

#[tokio::test]
async fn store_futures_are_send() {
    let f = fresh().await;
    let s = &f.store;
    let k = key("AAPL.US");
    let e = endpoint("consensus");
    let req = plan_request(1, &["AAPL.US"], &[]);
    let body = ok(b"x");
    let path = f.path.clone();
    // Built, never polled: only the types are checked.
    assert_send(Store::open(&path));
    assert_send(Store::open_with_catalog(&path, s.catalog()));
    assert_send(s.clone().close());
    assert_send(s.plan_run(&req));
    assert_send(s.plan(RunId(1)));
    assert_send(s.record_observation(RunId(1), &k, e, at(1, 2), &body));
    assert_send(s.freeze_membership(RunId(1), at(1, 2)));
    assert_send(s.end_run(RunId(1), at(1, 2), Finish::Partial, None));
    assert_send(s.close_dangling_runs(UnixMs(0)));
    assert_send(s.record_missed(Source::Longbridge, as_of(1), window(1), at(2, 0)));
    assert_send(s.read_at(&k, e, at(1, 2)));
    assert_send(s.universe_at(at(1, 2)));
    assert_send(s.coverage(RunId(1)));
    assert_send(s.attempts(RunId(1), &k, e));
    assert_send(s.first_as_of(Source::Longbridge));
    assert_send(s.frozen_at(RunId(1)));
    assert_send(s.dominant_failure(RunId(1), &research_archive::capture::COUNTED));
}
