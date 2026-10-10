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
fn exactly_one_visible_page_is_selected_even_beside_unknown_pages() {
    let pages = [page("a", Hidden), page("b", Visible), page("c", Unknown)];
    assert_eq!(select_visible(&pages).unwrap(), 1);
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
    silent: Vec<&'static str>,
    loads: Vec<(&'static str, &'static str)>,
    log: Arc<Mutex<Log>>,
}

impl Script {
    fn new(targets: &[(&'static str, &'static str)]) -> Self {
        Self {
            targets: targets.to_vec(),
            silent: Vec::new(),
            loads: LOAD_FOR_THE_NEW_DOCUMENT.to_vec(),
            log: Arc::default(),
        }
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
                                    "url": format!("https://{t}.test/"), "title": t })
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
                        "url": "https://after.test/", "title": "after", "text": "body text" } } }),
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
    let result = read(&cdp).await;
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
    let text = read(&cdp).await.unwrap();
    server.abort();
    assert_eq!(text.text, "body text");
    assert_eq!(text.url, "https://after.test/");
}

fn ids(pages: &[PageInfo]) -> Vec<&str> {
    pages.iter().map(|p| p.target_id.as_str()).collect()
}

/// A background page that never answers its visibility check does not block
/// the one visible page.
#[tokio::test]
async fn an_unanswered_visibility_check_does_not_block_the_visible_page() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("shown", "visible"), ("stuck", "silent")]);
    let server = script.serve(peer);
    let text = tokio::time::timeout(Duration::from_secs(5), read(&cdp))
        .await
        .expect("one stuck page must not stall the read")
        .unwrap();
    server.abort();
    assert_eq!(text.text, "body text");
}

/// With no visible page, the refusal names the pages of unknown visibility too.
#[tokio::test]
async fn no_visible_page_names_the_unknown_pages() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("bg", "hidden"), ("stuck", "silent")]);
    let server = script.serve(peer);
    let result = tokio::time::timeout(Duration::from_secs(5), read(&cdp))
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
    let result = tokio::time::timeout(Duration::from_secs(5), read(&cdp))
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
        tokio::time::timeout(Duration::from_millis(300), read(&cdp))
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
