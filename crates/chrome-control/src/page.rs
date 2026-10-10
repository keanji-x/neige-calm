//! Page targets and the one visible page.
//!
//! CDP targets carry no window identity, so "the page the owner sees" is the
//! page target whose `document.visibilityState` is `"visible"`. Every
//! operation attaches flat sessions for the duration of the call and detaches
//! them afterwards; no session state outlives a call.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;

use crate::cdp::Cdp;
use crate::{Error, Result};

/// Timeout for each single CDP call.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// A page target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PageInfo {
    pub target_id: String,
    pub url: String,
    pub title: String,
    /// `document.visibilityState == "visible"`.
    pub visible: bool,
}

/// The visible page after [`crate::Chrome::navigate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Navigated {
    pub url: String,
    pub title: String,
    /// `false` when the timeout passed before the new document's load event.
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

struct Attached {
    info: PageInfo,
    session: String,
}

pub(crate) async fn list(cdp: &Cdp) -> Result<Vec<PageInfo>> {
    let attached = attach_all(cdp).await?;
    detach(cdp, &attached).await;
    Ok(attached.into_iter().map(|page| page.info).collect())
}

pub(crate) async fn navigate(cdp: &Cdp, url: &str, timeout: Duration) -> Result<Navigated> {
    let page = attach_visible(cdp).await?;
    let result = navigate_in(cdp, &page.session, url, timeout).await;
    detach(cdp, std::slice::from_ref(&page)).await;
    result
}

pub(crate) async fn read(cdp: &Cdp) -> Result<PageText> {
    let page = attach_visible(cdp).await?;
    let result = evaluate(
        cdp,
        &page.session,
        "({url: location.href, title: document.title, \
          text: document.body ? document.body.innerText : ''})",
    )
    .await;
    detach(cdp, std::slice::from_ref(&page)).await;
    parse(result?)
}

/// The index of the one visible page, or the typed refusal naming the candidates.
pub(crate) fn select_visible(pages: &[PageInfo]) -> Result<usize> {
    let visible: Vec<usize> = (0..pages.len()).filter(|&i| pages[i].visible).collect();
    match visible.as_slice() {
        [one] => Ok(*one),
        [] => Err(Error::NoVisiblePage {
            pages: pages.to_vec(),
        }),
        many => Err(Error::AmbiguousPage {
            pages: many.iter().map(|&i| pages[i].clone()).collect(),
        }),
    }
}

async fn attach_visible(cdp: &Cdp) -> Result<Attached> {
    let mut attached = attach_all(cdp).await?;
    let infos: Vec<PageInfo> = attached.iter().map(|page| page.info.clone()).collect();
    match select_visible(&infos) {
        Ok(index) => {
            let page = attached.swap_remove(index);
            detach(cdp, &attached).await;
            Ok(page)
        }
        Err(error) => {
            detach(cdp, &attached).await;
            Err(error)
        }
    }
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

async fn attach_all(cdp: &Cdp) -> Result<Vec<Attached>> {
    let targets: Targets = parse(
        cdp.call("Target.getTargets", json!({}), None, CALL_TIMEOUT)
            .await?,
    )?;
    let mut attached = Vec::new();
    for target in targets.infos.into_iter().filter(|t| t.kind == "page") {
        match attach_one(cdp, target).await {
            Ok(page) => attached.push(page),
            Err(error) => {
                detach(cdp, &attached).await;
                return Err(error);
            }
        }
    }
    Ok(attached)
}

async fn attach_one(cdp: &Cdp, target: TargetInfo) -> Result<Attached> {
    let reply = cdp
        .call(
            "Target.attachToTarget",
            json!({ "targetId": target.id, "flatten": true }),
            None,
            CALL_TIMEOUT,
        )
        .await?;
    let session = reply
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| unexpected("Target.attachToTarget", &reply))?
        .to_owned();
    let page = Attached {
        info: PageInfo {
            target_id: target.id,
            url: target.url,
            title: target.title,
            visible: false,
        },
        session,
    };
    match evaluate(cdp, &page.session, "document.visibilityState").await {
        Ok(state) => Ok(Attached {
            info: PageInfo {
                visible: state == "visible",
                ..page.info
            },
            session: page.session,
        }),
        Err(error) => {
            detach(cdp, std::slice::from_ref(&page)).await;
            Err(error)
        }
    }
}

async fn detach(cdp: &Cdp, pages: &[Attached]) {
    for page in pages {
        let params = json!({ "sessionId": page.session });
        if let Err(error) = cdp
            .call("Target.detachFromTarget", params, None, CALL_TIMEOUT)
            .await
        {
            tracing::debug!(%error, target = %page.info.target_id, "detach failed");
        }
    }
}

async fn navigate_in(cdp: &Cdp, session: &str, url: &str, timeout: Duration) -> Result<Navigated> {
    let deadline = Instant::now() + timeout;
    let session_call =
        |method: &'static str, params: Value| cdp.call(method, params, Some(session), CALL_TIMEOUT);
    session_call("Page.enable", json!({})).await?;
    session_call("Page.setLifecycleEventsEnabled", json!({ "enabled": true })).await?;
    let mut events = cdp.subscribe()?;
    let loaded = match cdp
        .call(
            "Page.navigate",
            json!({ "url": url }),
            Some(session),
            timeout,
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
                Some(loader) => wait_for_load(&mut events, session, loader, deadline).await?,
            }
        }
    };
    let now = evaluate(
        cdp,
        session,
        "({url: location.href, title: document.title})",
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
    deadline: Instant,
) -> Result<bool> {
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
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

async fn evaluate(cdp: &Cdp, session: &str, expression: &str) -> Result<Value> {
    let mut reply = cdp
        .call(
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true }),
            Some(session),
            CALL_TIMEOUT,
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
