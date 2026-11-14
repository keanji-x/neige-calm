//! Process-local unlock credentials. Only a digest and immutable link identity
//! survive the unlock request; owner sessions never select these credentials.
use crate::auth::{SessionAuthority, SessionIdentity, SessionStore};
use axum::http::{HeaderMap, header};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug)]
pub(crate) struct Credential {
    pub link_id: String,
    pub hash: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ShareSessions(Arc<Mutex<HashMap<String, Credential>>>);

impl ShareSessions {
    pub fn create(
        &self,
        sessions: &SessionStore,
        identity: SessionIdentity,
        credential: Credential,
    ) -> String {
        let id = sessions.create(identity, SessionAuthority::ShareLink);
        self.0
            .lock()
            .expect("share sessions lock")
            .insert(id.clone(), credential);
        id
    }

    pub fn get(&self, session: &str) -> Option<Credential> {
        self.0
            .lock()
            .ok()?
            .get(session)
            .filter(|c| c.expires_at > crate::model::now_ms())
            .cloned()
    }

    /// End both halves of this credential through the session lifecycle owner.
    pub fn remove(&self, sessions: &SessionStore, session: &str) -> crate::error::Result<()> {
        self.0.lock().expect("share sessions lock").remove(session);
        sessions.remove(session)
    }

    pub fn revoke(&self, sessions: &SessionStore, link: &str) -> crate::error::Result<()> {
        let ids: Vec<_> = self
            .0
            .lock()
            .expect("share sessions lock")
            .iter()
            .filter(|(_, c)| c.link_id == link)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.remove(sessions, &id)?;
        }
        Ok(())
    }
}

pub(crate) fn cookie_name(id: &str) -> String {
    format!("calm-share-session-{id}")
}

pub(crate) fn cookies<'a>(headers: &'a HeaderMap, name: &'a str) -> impl Iterator<Item = &'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|h| h.split(';'))
        .filter_map(move |p| {
            let (n, v) = p.trim().split_once('=')?;
            (n == name).then_some(v.trim())
        })
}

/// Presence selects share authentication even for an empty/malformed selector.
/// Conflicting selectors fail closed rather than selecting another credential.
pub(crate) fn selector(headers: &HeaderMap, uri: &axum::http::Uri) -> Option<String> {
    let query: Vec<(String, String)> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .filter(|(k, _)| k == "share_link")
            .collect();
    let hs: Vec<_> = headers.get_all("x-calm-share-link").iter().collect();
    if hs.is_empty() && query.is_empty() {
        return None;
    }
    let header = match hs.first() {
        Some(value) => match value.to_str() {
            Ok(value) => Some(value),
            Err(_) => return Some(String::new()),
        },
        None => None,
    };
    let q = query.first().map(|(_, v)| v.as_str());
    let id = header.or(q).unwrap_or("");
    if hs.len() > 1
        || query.len() > 1
        || (header.is_some() && q.is_some() && header != q)
        || id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Some(String::new());
    }
    Some(id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_header_never_falls_back_to_query() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-calm-share-link",
            axum::http::HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        for uri in ["/api/fs/roots?share_link=valid", "/api/fs/roots"] {
            assert_eq!(
                selector(&headers, &uri.parse().unwrap()),
                Some(String::new())
            );
        }
    }
}
