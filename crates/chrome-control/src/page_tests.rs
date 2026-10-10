use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::cdp::tests::{Peer, connected};
use Visibility::{Hidden, Unknown, Visible};

fn page(id: &str, visibility: Visibility) -> PageInfo {
    PageInfo {
        target_id: id.into(),
        url: format!("https://{id}.test/"),
        title: id.into(),
        visibility,
    }
}

#[test]
fn exactly_one_visible_page_beside_hidden_ones_is_selected() {
    let pages = [page("a", Hidden), page("b", Visible), page("c", Hidden)];
    assert_eq!(select_visible(&pages).unwrap(), 1);
}

/// A page that did not answer may be a visible window held up by a dialog, so
/// one visible page beside it is not known to be the only one.
#[test]
fn one_visible_page_beside_an_unknown_one_is_ambiguous() {
    let pages = [page("a", Hidden), page("b", Visible), page("c", Unknown)];
    match select_visible(&pages) {
        Err(Error::AmbiguousPage { pages: named }) => {
            assert_eq!(named, [pages[1].clone(), pages[2].clone()]);
        }
        other => panic!("expected AmbiguousPage, got {other:?}"),
    }
}

#[test]
fn no_visible_page_names_every_candidate() {
    let pages = [page("a", Hidden), page("b", Unknown)];
    match select_visible(&pages) {
        Err(Error::NoVisiblePage { pages: named }) => assert_eq!(named, pages),
        other => panic!("expected NoVisiblePage, got {other:?}"),
    }
}

#[test]
fn several_visible_pages_are_ambiguous_and_named_with_the_unknown_ones() {
    let pages = [
        page("a", Visible),
        page("b", Hidden),
        page("c", Unknown),
        page("d", Visible),
    ];
    match select_visible(&pages) {
        Err(Error::AmbiguousPage { pages: named }) => {
            assert_eq!(
                named,
                [pages[0].clone(), pages[2].clone(), pages[3].clone()]
            );
        }
        other => panic!("expected AmbiguousPage, got {other:?}"),
    }
}

/// Every `(method, sessionId)` the scripted browser received.
type Log = Vec<(String, Option<String>)>;

/// The noise and signal sent after `Page.navigate`: (session, loaderId) of a
/// `load` lifecycle event; `OURS` stands for the navigated page's session.
const OURS: &str = "ours";
const LOAD_FOR_THE_NEW_DOCUMENT: &[(&str, &str)] =
    &[(OURS, "L-old"), ("S-other", "L-new"), (OURS, "L-new")];

/// A scripted browser. Each page target answers its visibility check with
/// `"visible"` or `"hidden"`, or never (`"silent"`). Methods in `silent` are
/// never answered. Records every `(method, sessionId)` it receives.
struct Script {
    targets: Vec<(&'static str, &'static str)>,
    /// Page-controlled strings far over every limit: titles, urls and text.
    long: bool,
    silent: Vec<&'static str>,
    loads: Vec<(&'static str, &'static str)>,
    log: Arc<Mutex<Log>>,
}

impl Script {
    fn new(targets: &[(&'static str, &'static str)]) -> Self {
        Self {
            targets: targets.to_vec(),
            long: false,
            silent: Vec::new(),
            loads: LOAD_FOR_THE_NEW_DOCUMENT.to_vec(),
            log: Arc::default(),
        }
    }

    fn long_strings(mut self) -> Self {
        self.long = true;
        self
    }

    fn silent(mut self, methods: &[&'static str]) -> Self {
        self.silent = methods.to_vec();
        self
    }

    fn loads(mut self, loads: &[(&'static str, &'static str)]) -> Self {
        self.loads = loads.to_vec();
        self
    }

    fn log(&self) -> Log {
        self.log.lock().unwrap().clone()
    }

    fn count(&self, method: &str) -> usize {
        self.log().iter().filter(|(m, _)| m == method).count()
    }

    /// Waits up to 2 s for `count(method)` to reach `n`; returns the count.
    async fn wait_for(&self, method: &str, n: usize) -> usize {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while self.count(method) < n && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.count(method)
    }

    fn serve(&self, mut peer: Peer) -> tokio::task::JoinHandle<()> {
        let targets = self.targets.clone();
        let silent = self.silent.clone();
        let loads = self.loads.clone();
        let log = self.log.clone();
        let long = self.long;
        // Over every limit, and ending in a surrogate pair where a cut could land.
        let stretch = move |s: &str| {
            if long {
                format!("{s}{}", "\u{1F600}".repeat(40_000))
            } else {
                s.to_owned()
            }
        };
        tokio::spawn(async move {
            while let Some(message) = peer.recv().await {
                let method = message["method"].as_str().unwrap().to_owned();
                let session = message["sessionId"].as_str().map(str::to_owned);
                log.lock().unwrap().push((method.clone(), session.clone()));
                let id = message["id"].clone();
                let visibility = |session: &str| {
                    targets
                        .iter()
                        .find(|(target, _)| format!("S-{target}") == session)
                        .map(|(_, state)| *state)
                        .unwrap()
                };
                let expression = message["params"]["expression"].as_str().unwrap_or("");
                let is_visibility_check = expression == "document.visibilityState";
                if silent.contains(&method.as_str()) && !is_visibility_check {
                    continue;
                }
                let result = match method.as_str() {
                    "Target.getTargets" => {
                        let mut infos: Vec<Value> = targets
                            .iter()
                            .map(|(t, _)| {
                                json!({ "targetId": t, "type": "page", "attached": false,
                                    "url": stretch(&format!("https://{t}.test/")), "title": stretch(t) })
                            })
                            .collect();
                        infos.push(json!({ "targetId": "sw", "type": "service_worker",
                            "url": "https://sw.test/", "title": "sw" }));
                        json!({ "targetInfos": infos })
                    }
                    "Target.attachToTarget" => {
                        let target = message["params"]["targetId"].as_str().unwrap();
                        json!({ "sessionId": format!("S-{target}") })
                    }
                    "Runtime.evaluate" if is_visibility_check => {
                        match visibility(session.as_deref().unwrap()) {
                            "silent" => continue,
                            state => json!({ "result": { "type": "string", "value": state } }),
                        }
                    }
                    "Runtime.evaluate" => json!({ "result": { "type": "object", "value": {
                        "url": stretch("https://after.test/"), "title": stretch("after"),
                        "text": stretch("body text"),
                        "text_truncated": expression.contains("all, 9)") } } }),
                    "Page.navigate" => {
                        let reply = json!({ "frameId": "F", "loaderId": "L-new" });
                        peer.send(json!({ "id": id, "result": reply })).await;
                        let ours = session.unwrap();
                        for &(sid, loader) in &loads {
                            let sid = if sid == OURS { ours.as_str() } else { sid };
                            let params =
                                json!({ "frameId": "F", "loaderId": loader, "name": "load" });
                            let event = json!({ "method": "Page.lifecycleEvent",
                                "sessionId": sid, "params": params });
                            peer.send(event).await;
                        }
                        continue;
                    }
                    _ => json!({}),
                };
                peer.send(json!({ "id": id, "result": result })).await;
            }
        })
    }
}

const SHORT: Duration = Duration::from_secs(1);
/// `read` text limit for tests that do not look at the cut.
const LIMIT: u32 = 100_000;

#[tokio::test]
async fn navigate_targets_the_visible_page_and_waits_for_its_load() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("hidden", "hidden"), ("shown", "visible")]);
    let server = script.serve(peer);
    let navigated = navigate(&cdp, "https://after.test/", Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(script.wait_for("Target.detachFromTarget", 2).await, 2);
    server.abort();
    assert_eq!(
        navigated,
        Navigated {
            url: "https://after.test/".into(),
            title: "after".into(),
            loaded: true,
        }
    );
    let log = script.log();
    let navigate_calls: Vec<_> = log.iter().filter(|(m, _)| m == "Page.navigate").collect();
    assert_eq!(
        navigate_calls,
        [&("Page.navigate".to_owned(), Some("S-shown".to_owned()))]
    );
}

/// Noise only: a load of another document on our session is not our load.
#[tokio::test]
async fn a_load_of_another_document_is_not_the_load() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("shown", "visible")]).loads(&[(OURS, "L-old")]);
    let server = script.serve(peer);
    let navigated = tokio::time::timeout(SHORT * 3, navigate(&cdp, "https://x.test/", SHORT))
        .await
        .expect("navigate is bounded")
        .unwrap();
    server.abort();
    assert!(!navigated.loaded, "{navigated:?}");
}

/// Noise only: the right document's load on another page's session is not ours.
#[tokio::test]
async fn a_load_on_another_session_is_not_the_load() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("shown", "visible")]).loads(&[("S-other", "L-new")]);
    let server = script.serve(peer);
    let navigated = tokio::time::timeout(SHORT * 3, navigate(&cdp, "https://x.test/", SHORT))
        .await
        .expect("navigate is bounded")
        .unwrap();
    server.abort();
    assert!(!navigated.loaded, "{navigated:?}");
}

/// The navigate timeout bounds the whole call, also when Chrome stalls before
/// the navigation starts.
#[tokio::test]
async fn navigate_returns_within_its_timeout_when_chrome_stalls() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("shown", "visible")]).silent(&["Page.enable"]);
    let server = script.serve(peer);
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        navigate(&cdp, "https://x.test/", Duration::from_millis(300)),
    )
    .await
    .expect("navigate overran its own timeout by seconds");
    server.abort();
    assert!(matches!(result, Err(Error::Timeout { .. })), "{result:?}");
    assert!(started.elapsed() < Duration::from_millis(600));
}

#[tokio::test]
async fn read_refuses_two_visible_pages_and_names_both() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("one", "visible"), ("two", "visible"), ("bg", "hidden")]);
    let server = script.serve(peer);
    let result = read(&cdp, LIMIT).await;
    assert_eq!(script.wait_for("Target.detachFromTarget", 3).await, 3);
    server.abort();
    match result {
        Err(Error::AmbiguousPage { pages }) => {
            let ids: Vec<&str> = pages.iter().map(|p| p.target_id.as_str()).collect();
            assert_eq!(ids, ["one", "two"]);
        }
        other => panic!("expected AmbiguousPage, got {other:?}"),
    }
}

#[tokio::test]
async fn read_returns_the_visible_page_text() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("only", "visible")]);
    let server = script.serve(peer);
    let text = read(&cdp, LIMIT).await.unwrap();
    server.abort();
    assert_eq!(text.text, "body text");
    assert_eq!(text.url, "https://after.test/");
    assert!(!text.text_truncated);
}

/// The limit reaches the page's own script, and the page's verdict comes back.
#[tokio::test]
async fn read_sends_its_limit_into_the_page_and_reports_the_cut() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("only", "visible")]);
    let server = script.serve(peer);
    let text = read(&cdp, 9).await.unwrap();
    server.abort();
    assert!(text.text_truncated);
}

/// Every page-controlled string is cut in the page, before it crosses the
/// pipe, and never inside a surrogate pair.
#[test]
fn the_page_side_reads_cut_every_page_controlled_string() {
    let read = super::read_expression(100_000);
    assert!(read.contains("const text = cut(all, 100000)"), "{read}");
    assert!(
        read.contains("text_truncated: text.length < all.length"),
        "{read}"
    );
    for expression in [read, super::location_expression()] {
        assert!(expression.contains(super::CUT_JS), "{expression}");
        assert!(
            expression.contains("title: cut(document.title, 1000)"),
            "{expression}"
        );
        assert!(
            expression.contains("url: cut(location.href, 4096)"),
            "{expression}"
        );
    }
    assert!(super::CUT_JS.contains("c >= 0xD800 && c <= 0xDBFF"));
}

#[test]
fn a_cut_counts_utf16_units_and_never_splits_a_character() {
    let mut s = "abc\u{1F600}d".to_owned();
    assert!(super::cut(&mut s, 4));
    assert_eq!(s, "abc");
    let mut s = "abc\u{1F600}d".to_owned();
    assert!(super::cut(&mut s, 5));
    assert_eq!(s, "abc\u{1F600}");
    let mut s = "abc\u{1F600}d".to_owned();
    assert!(!super::cut(&mut s, 6));
    assert_eq!(s, "abc\u{1F600}d");
}

/// What a page controls is bounded in what reaches the caller even when the
/// page-side cut was defeated (a page can replace `String.prototype.slice`).
#[tokio::test]
async fn read_bounds_the_title_url_and_text_it_returns() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("only", "visible")]).long_strings();
    let server = script.serve(peer);
    let text = read(&cdp, 50).await.unwrap();
    server.abort();
    assert!(
        text.title.encode_utf16().count() <= 1000,
        "{}",
        text.title.len()
    );
    assert!(
        text.url.encode_utf16().count() <= 4096,
        "{}",
        text.url.len()
    );
    assert!(
        text.text.encode_utf16().count() <= 50,
        "{}",
        text.text.len()
    );
    assert!(text.text_truncated);
    assert!(text.title.starts_with("after"));
}

#[tokio::test]
async fn navigate_bounds_the_title_and_url_it_returns() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("only", "visible")]).long_strings();
    let server = script.serve(peer);
    let navigated = navigate(&cdp, "https://after.test/", Duration::from_secs(5))
        .await
        .unwrap();
    server.abort();
    assert!(navigated.title.encode_utf16().count() <= 1000);
    assert!(navigated.url.encode_utf16().count() <= 4096);
}

/// Refusals name every page; the names are bounded too.
#[tokio::test]
async fn refusals_bound_the_page_titles_and_urls_they_name() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("one", "visible"), ("two", "visible")]).long_strings();
    let server = script.serve(peer);
    let result = read(&cdp, LIMIT).await;
    server.abort();
    let Err(Error::AmbiguousPage { pages }) = result else {
        panic!("expected AmbiguousPage, got {result:?}");
    };
    for page in pages {
        assert!(page.title.encode_utf16().count() <= 1000);
        assert!(page.url.encode_utf16().count() <= 4096);
    }
}

fn ids(pages: &[PageInfo]) -> Vec<&str> {
    pages.iter().map(|p| p.target_id.as_str()).collect()
}

/// A page that never answers its visibility check (an `alert()` in a popup)
/// may be visible: beside a visible page the read refuses and names both.
#[tokio::test]
async fn a_visible_page_beside_an_unanswered_one_is_ambiguous() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("shown", "visible"), ("stuck", "silent")]);
    let server = script.serve(peer);
    let result = tokio::time::timeout(Duration::from_secs(5), read(&cdp, LIMIT))
        .await
        .expect("one stuck page must not stall the read");
    server.abort();
    match result {
        Err(Error::AmbiguousPage { pages }) => assert_eq!(ids(&pages), ["shown", "stuck"]),
        other => panic!("expected AmbiguousPage, got {other:?}"),
    }
}

/// Visibility checks run side by side: three hung pages cost one check timeout.
#[tokio::test]
async fn unanswered_pages_are_probed_in_parallel() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("a", "silent"), ("b", "silent"), ("c", "silent")]);
    let server = script.serve(peer);
    let started = tokio::time::Instant::now();
    let pages = tokio::time::timeout(Duration::from_secs(15), list(&cdp))
        .await
        .expect("list is bounded")
        .unwrap();
    let took = started.elapsed();
    server.abort();
    assert!(pages.iter().all(|p| p.visibility == Unknown), "{pages:?}");
    assert!(
        took < Duration::from_secs(5),
        "three hung pages took {took:?}"
    );
}

/// `Duration::MAX` means no time limit, not an overflow.
#[tokio::test]
async fn navigate_without_a_time_limit() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("shown", "visible")]);
    let server = script.serve(peer);
    let navigated = navigate(&cdp, "https://after.test/", Duration::MAX)
        .await
        .unwrap();
    server.abort();
    assert!(navigated.loaded, "{navigated:?}");
}

/// With no visible page, the refusal names the pages of unknown visibility too.
#[tokio::test]
async fn no_visible_page_names_the_unknown_pages() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("bg", "hidden"), ("stuck", "silent")]);
    let server = script.serve(peer);
    let result = tokio::time::timeout(Duration::from_secs(5), read(&cdp, LIMIT))
        .await
        .expect("one stuck page must not stall the read");
    server.abort();
    match result {
        Err(Error::NoVisiblePage { pages }) => assert_eq!(ids(&pages), ["bg", "stuck"]),
        other => panic!("expected NoVisiblePage, got {other:?}"),
    }
}

/// With several visible pages, the refusal names the unknown pages as candidates too.
#[tokio::test]
async fn ambiguous_pages_include_the_unknown_ones() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("one", "visible"), ("stuck", "silent"), ("two", "visible")]);
    let server = script.serve(peer);
    let result = tokio::time::timeout(Duration::from_secs(5), read(&cdp, LIMIT))
        .await
        .expect("one stuck page must not stall the read");
    server.abort();
    match result {
        Err(Error::AmbiguousPage { pages }) => assert_eq!(ids(&pages), ["one", "stuck", "two"]),
        other => panic!("expected AmbiguousPage, got {other:?}"),
    }
}

/// A dropped `navigate` future still detaches the sessions it attached.
#[tokio::test]
async fn a_dropped_navigate_detaches_its_sessions() {
    let (cdp, peer) = connected();
    let script =
        Script::new(&[("hidden", "hidden"), ("shown", "visible")]).silent(&["Page.navigate"]);
    let server = script.serve(peer);
    let navigating = navigate(&cdp, "https://x.test/", Duration::from_secs(30));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), navigating)
            .await
            .is_err()
    );
    assert_eq!(script.wait_for("Target.detachFromTarget", 2).await, 2);
    server.abort();
}

/// A dropped `read` future still detaches the sessions it attached.
#[tokio::test]
async fn a_dropped_read_detaches_its_sessions() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("shown", "visible")]).silent(&["Runtime.evaluate"]);
    let server = script.serve(peer);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), read(&cdp, LIMIT))
            .await
            .is_err()
    );
    assert_eq!(script.wait_for("Target.detachFromTarget", 1).await, 1);
    server.abort();
}

/// A dropped `list` future still detaches the sessions it attached.
#[tokio::test]
async fn a_dropped_list_detaches_its_sessions() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("shown", "visible"), ("stuck", "silent")]);
    let server = script.serve(peer);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), list(&cdp))
            .await
            .is_err()
    );
    assert_eq!(script.wait_for("Target.detachFromTarget", 2).await, 2);
    server.abort();
}
