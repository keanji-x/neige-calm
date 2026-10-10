use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::cdp::tests::{Peer, connected};

fn page(id: &str, visible: bool) -> PageInfo {
    PageInfo {
        target_id: id.into(),
        url: format!("https://{id}.test/"),
        title: id.into(),
        visible,
    }
}

#[test]
fn exactly_one_visible_page_is_selected() {
    let pages = [page("a", false), page("b", true), page("c", false)];
    assert_eq!(select_visible(&pages).unwrap(), 1);
}

#[test]
fn no_visible_page_names_every_candidate() {
    let pages = [page("a", false), page("b", false)];
    match select_visible(&pages) {
        Err(Error::NoVisiblePage { pages: named }) => assert_eq!(named, pages),
        other => panic!("expected NoVisiblePage, got {other:?}"),
    }
}

#[test]
fn several_visible_pages_are_ambiguous_and_named() {
    let pages = [page("a", true), page("b", false), page("c", true)];
    match select_visible(&pages) {
        Err(Error::AmbiguousPage { pages: named }) => {
            assert_eq!(named, [pages[0].clone(), pages[2].clone()]);
        }
        other => panic!("expected AmbiguousPage, got {other:?}"),
    }
}

/// Every `(method, sessionId)` the scripted browser received.
type Log = Vec<(String, Option<String>)>;

/// A scripted browser: page targets with fixed visibility. Records every
/// `(method, sessionId)` it receives.
struct Script {
    targets: Vec<(&'static str, &'static str)>,
    log: Arc<Mutex<Log>>,
}

impl Script {
    fn new(targets: &[(&'static str, &'static str)]) -> Self {
        Self {
            targets: targets.to_vec(),
            log: Arc::default(),
        }
    }

    fn log(&self) -> Log {
        self.log.lock().unwrap().clone()
    }

    fn serve(&self, mut peer: Peer) -> tokio::task::JoinHandle<()> {
        let targets = self.targets.clone();
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
                let result = match method.as_str() {
                    "Target.getTargets" => {
                        json!({ "targetInfos": targets.iter().map(|(t, _)| json!({
                        "targetId": t, "type": "page", "url": format!("https://{t}.test/"),
                        "title": t, "attached": false })).chain([json!({"targetId": "sw",
                        "type": "service_worker", "url": "https://sw.test/", "title": "sw"})])
                        .collect::<Vec<_>>() })
                    }
                    "Target.attachToTarget" => {
                        json!({ "sessionId": format!("S-{}", message["params"]["targetId"].as_str().unwrap()) })
                    }
                    "Runtime.evaluate" => {
                        let session = session.as_deref().unwrap();
                        let expression = message["params"]["expression"].as_str().unwrap();
                        if expression == "document.visibilityState" {
                            json!({ "result": { "type": "string", "value": visibility(session) } })
                        } else {
                            json!({ "result": { "type": "object", "value": {
                                "url": "https://after.test/", "title": "after", "text": "body text" } } })
                        }
                    }
                    "Page.navigate" => {
                        peer.send(
                            json!({ "id": id, "result": { "frameId": "F", "loaderId": "L-new" } }),
                        )
                        .await;
                        // Noise first: the old document's load, another session's
                        // load, then the new document's load.
                        let session = session.unwrap();
                        for (sid, loader) in [
                            (session.as_str(), "L-old"),
                            ("S-other", "L-new"),
                            (session.as_str(), "L-new"),
                        ] {
                            peer.send(json!({ "method": "Page.lifecycleEvent", "sessionId": sid,
                                "params": { "frameId": "F", "loaderId": loader, "name": "load" } }))
                                .await;
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

#[tokio::test]
async fn navigate_targets_the_visible_page_and_waits_for_its_load() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("hidden", "hidden"), ("shown", "visible")]);
    let server = script.serve(peer);
    let navigated = navigate(&cdp, "https://after.test/", Duration::from_secs(5))
        .await
        .unwrap();
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
    let detached = log
        .iter()
        .filter(|(m, _)| m == "Target.detachFromTarget")
        .count();
    assert_eq!(detached, 2, "every attached session is detached: {log:?}");
}

#[tokio::test]
async fn read_refuses_two_visible_pages_and_names_both() {
    let (cdp, peer) = connected();
    let script = Script::new(&[("one", "visible"), ("two", "visible"), ("bg", "hidden")]);
    let server = script.serve(peer);
    let result = read(&cdp).await;
    server.abort();
    match result {
        Err(Error::AmbiguousPage { pages }) => {
            let ids: Vec<&str> = pages.iter().map(|p| p.target_id.as_str()).collect();
            assert_eq!(ids, ["one", "two"]);
        }
        other => panic!("expected AmbiguousPage, got {other:?}"),
    }
    let log = script.log();
    let detached = log
        .iter()
        .filter(|(m, _)| m == "Target.detachFromTarget")
        .count();
    assert_eq!(detached, 3, "every attached session is detached: {log:?}");
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
