//! Page targets and the one visible page.
//!
//! CDP targets carry no window identity, so "the page the owner sees" is the
//! page target whose `document.visibilityState` is `"visible"`. Every
//! operation attaches flat sessions for its own duration only. They are
//! detached from a spawned task when the operation ends, also when its future
//! is dropped. One race remains: an attach reply already delivered but not
//! yet read when the future is dropped leaks that session (#2547).

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::join_all;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;

use crate::cdp::Cdp;
use crate::{Error, Result};

/// Timeout for each single CDP call.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout for attaching to one page, and again for reading its visibility. A
/// page that misses either (a JS dialog, a busy renderer) has unknown
/// visibility. Pages are probed side by side, so probing takes at most twice
/// this in total.
const VISIBILITY_TIMEOUT: Duration = Duration::from_secs(2);
/// Upper bound of the part of a `navigate` timeout kept for reading the url
/// and title after the load wait.
const READ_RESERVE: Duration = Duration::from_secs(2);

/// Whether a page is the one the owner sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// `document.visibilityState == "visible"`.
    Visible,
    /// Any other visibility state.
    Hidden,
    /// The page could not be attached or did not answer in time.
    Unknown,
}

/// A page target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PageInfo {
    pub target_id: String,
    pub url: String,
    pub title: String,
    pub visibility: Visibility,
}

/// The visible page after [`crate::Chrome::navigate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Navigated {
    pub url: String,
    pub title: String,
    /// `false` when the new document's load event had not come when the load
    /// wait ended.
    pub loaded: bool,
}

#[derive(Deserialize)]
struct Location {
    url: String,
    title: String,
}

/// The visible page's text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PageText {
    pub url: String,
    pub title: String,
    /// `document.body.innerText` (empty when the document has no body).
    pub text: String,
}

/// Sessions attached by one operation. Dropping it detaches them from a
/// spawned task, so a dropped operation future still cleans up.
/// Page probes running side by side record into it as soon as they attach.
struct Sessions {
    cdp: Cdp,
    ids: Mutex<Vec<String>>,
}

impl Sessions {
    fn new(cdp: &Cdp) -> Self {
        Self {
            cdp: cdp.clone(),
            ids: Mutex::new(Vec::new()),
        }
    }

    fn record(&self, session: &str) {
        let mut ids = self.ids.lock().unwrap_or_else(PoisonError::into_inner);
        ids.push(session.to_owned());
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        let ids = std::mem::take(self.ids.get_mut().unwrap_or_else(PoisonError::into_inner));
        if ids.is_empty() {
            return;
        }
        // Without a runtime the browser itself is going away with its sessions.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let cdp = self.cdp.clone();
        runtime.spawn(async move {
            for id in ids {
                let params = json!({ "sessionId": id });
                if let Err(error) = cdp
                    .call("Target.detachFromTarget", params, None, CALL_TIMEOUT)
                    .await
                {
                    tracing::debug!(%error, session = %id, "detach failed");
                }
            }
        });
    }
}

/// A page and, when attaching worked, its session.
struct Probed {
    info: PageInfo,
    session: Option<String>,
}

pub(crate) async fn list(cdp: &Cdp) -> Result<Vec<PageInfo>> {
    let sessions = Sessions::new(cdp);
    let pages = probe_all(cdp, &sessions).await?;
    Ok(pages.into_iter().map(|page| page.info).collect())
}

/// Navigates the visible page. `timeout` bounds the whole call: the load wait
/// ends early enough to leave a reserve for reading the url and title. A
/// timeout too large for a deadline (`Duration::MAX`) means no limit.
pub(crate) async fn navigate(cdp: &Cdp, url: &str, timeout: Duration) -> Result<Navigated> {
    let reserve = (timeout / 5).min(READ_RESERVE);
    let load_deadline = Instant::now().checked_add(timeout - reserve);
    let sessions = Sessions::new(cdp);
    let work = async {
        let session = visible_session(cdp, &sessions).await?;
        navigate_in(cdp, &session, url, load_deadline).await
    };
    tokio::time::timeout(timeout, work)
        .await
        .unwrap_or(Err(Error::Timeout {
            what: format!("navigate to {url}"),
            after: timeout,
        }))
}

pub(crate) async fn read(cdp: &Cdp) -> Result<PageText> {
    let sessions = Sessions::new(cdp);
    let session = visible_session(cdp, &sessions).await?;
    let text = evaluate(
        cdp,
        &session,
        "({url: location.href, title: document.title, \
          text: document.body ? document.body.innerText : ''})",
        CALL_TIMEOUT,
    )
    .await?;
    parse(text)
}

/// The index of the one visible page, or the typed refusal naming the
/// candidates. Fails closed: a page of unknown visibility may be a visible
/// window whose renderer is held up (a JS dialog, a busy main thread), so
/// - exactly one visible page and no unknown page: that page;
/// - no visible page: `NoVisiblePage`, naming every page;
/// - otherwise (several visible, or one beside an unknown page):
///   `AmbiguousPage`, naming the visible and the unknown pages.
pub(crate) fn select_visible(pages: &[PageInfo]) -> Result<usize> {
    let visible: Vec<usize> = (0..pages.len())
        .filter(|&i| pages[i].visibility == Visibility::Visible)
        .collect();
    let unknown = pages.iter().any(|p| p.visibility == Visibility::Unknown);
    match visible.as_slice() {
        [one] if !unknown => Ok(*one),
        [] => Err(Error::NoVisiblePage {
            pages: pages.to_vec(),
        }),
        _ => Err(Error::AmbiguousPage {
            pages: pages
                .iter()
                .filter(|page| page.visibility != Visibility::Hidden)
                .cloned()
                .collect(),
        }),
    }
}

/// The session of the one visible page; every session stays in `sessions`.
async fn visible_session(cdp: &Cdp, sessions: &Sessions) -> Result<String> {
    let pages = probe_all(cdp, sessions).await?;
    let infos: Vec<PageInfo> = pages.iter().map(|page| page.info.clone()).collect();
    let index = select_visible(&infos)?;
    // A visible page answered its check, so it has a session.
    pages[index]
        .session
        .clone()
        .ok_or_else(|| unexpected("visible page without a session", &Value::Null))
}

#[derive(Deserialize)]
struct Targets {
    #[serde(rename = "targetInfos")]
    infos: Vec<TargetInfo>,
}

#[derive(Deserialize)]
struct TargetInfo {
    #[serde(rename = "targetId")]
    id: String,
    #[serde(rename = "type")]
    kind: String,
    url: String,
    title: String,
}

/// Every page target with its visibility, probed side by side, so hung pages
/// cost one attach and one check timeout in total. Attached sessions go into
/// `sessions` as soon as they exist.
async fn probe_all(cdp: &Cdp, sessions: &Sessions) -> Result<Vec<Probed>> {
    let reply = cdp
        .call("Target.getTargets", json!({}), None, CALL_TIMEOUT)
        .await?;
    let targets: Targets = parse(reply)?;
    let probes = targets
        .infos
        .into_iter()
        .filter(|t| t.kind == "page")
        .map(|target| probe_one(cdp, sessions, target));
    join_all(probes).await.into_iter().collect()
}

async fn probe_one(cdp: &Cdp, sessions: &Sessions, target: TargetInfo) -> Result<Probed> {
    let session = match attach(cdp, &target.id).await {
        Ok(session) => {
            sessions.record(&session);
            Some(session)
        }
        Err(Error::Exited) => return Err(Error::Exited),
        Err(error) => {
            tracing::debug!(%error, target = %target.id, "attach failed");
            None
        }
    };
    let visibility = match &session {
        None => Visibility::Unknown,
        Some(session) => {
            let state = "document.visibilityState";
            match evaluate(cdp, session, state, VISIBILITY_TIMEOUT).await {
                Ok(state) if state == "visible" => Visibility::Visible,
                Ok(_) => Visibility::Hidden,
                Err(Error::Exited) => return Err(Error::Exited),
                Err(error) => {
                    tracing::debug!(%error, target = %target.id, "visibility unknown");
                    Visibility::Unknown
                }
            }
        }
    };
    let info = PageInfo {
        target_id: target.id,
        url: target.url,
        title: target.title,
        visibility,
    };
    Ok(Probed { info, session })
}

async fn attach(cdp: &Cdp, target: &str) -> Result<String> {
    let params = json!({ "targetId": target, "flatten": true });
    let reply = cdp
        .call("Target.attachToTarget", params, None, VISIBILITY_TIMEOUT)
        .await?;
    reply
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| unexpected("Target.attachToTarget", &reply))
}

async fn navigate_in(
    cdp: &Cdp,
    session: &str,
    url: &str,
    load_deadline: Option<Instant>,
) -> Result<Navigated> {
    let session_call =
        |method: &'static str, params: Value| cdp.call(method, params, Some(session), CALL_TIMEOUT);
    session_call("Page.enable", json!({})).await?;
    session_call("Page.setLifecycleEventsEnabled", json!({ "enabled": true })).await?;
    let mut events = cdp.subscribe()?;
    let until_deadline = load_deadline.map_or(Duration::MAX, |deadline| {
        deadline.saturating_duration_since(Instant::now())
    });
    let loaded = match cdp
        .call(
            "Page.navigate",
            json!({ "url": url }),
            Some(session),
            until_deadline,
        )
        .await
    {
        Err(Error::Timeout { .. }) => false,
        Err(error) => return Err(error),
        Ok(reply) => {
            if let Some(error) = reply.get("errorText").and_then(Value::as_str) {
                return Err(Error::Navigation {
                    url: url.into(),
                    error: error.into(),
                });
            }
            match reply.get("loaderId").and_then(Value::as_str) {
                // A same-document navigation has no new document to load.
                None => true,
                Some(loader) => wait_for_load(&mut events, session, loader, load_deadline).await?,
            }
        }
    };
    let now = evaluate(
        cdp,
        session,
        "({url: location.href, title: document.title})",
        CALL_TIMEOUT,
    )
    .await?;
    let Location { url, title } = parse(now)?;
    Ok(Navigated { url, title, loaded })
}

/// Waits for the `load` lifecycle event of the document that `loader` commits.
async fn wait_for_load(
    events: &mut tokio::sync::broadcast::Receiver<crate::cdp::Event>,
    session: &str,
    loader: &str,
    deadline: Option<Instant>,
) -> Result<bool> {
    loop {
        let next = match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, events.recv()).await,
            None => Ok(events.recv().await),
        };
        match next {
            Err(_) => return Ok(false),
            Ok(Err(RecvError::Closed)) => return Err(Error::Exited),
            Ok(Err(RecvError::Lagged(missed))) => {
                tracing::debug!(missed, "CDP events lagged while waiting for load");
            }
            Ok(Ok(event)) => {
                if event.method == "Page.lifecycleEvent"
                    && event.session.as_deref() == Some(session)
                    && event.params["name"] == "load"
                    && event.params["loaderId"] == loader
                {
                    return Ok(true);
                }
            }
        }
    }
}

async fn evaluate(cdp: &Cdp, session: &str, expression: &str, timeout: Duration) -> Result<Value> {
    let mut reply = cdp
        .call(
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true }),
            Some(session),
            timeout,
        )
        .await?;
    if let Some(exception) = reply.get("exceptionDetails") {
        return Err(Error::Cdp {
            method: "Runtime.evaluate".into(),
            message: exception.to_string(),
        });
    }
    Ok(reply["result"]
        .get_mut("value")
        .map(Value::take)
        .unwrap_or(Value::Null))
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value.clone()).map_err(|_| unexpected("result", &value))
}

fn unexpected(what: &str, value: &Value) -> Error {
    Error::Cdp {
        method: what.into(),
        message: format!("unexpected reply: {value}"),
    }
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
