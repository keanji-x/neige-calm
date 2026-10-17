//! Fixed-origin GET only. No proxy, redirects, attachment fetches or URL tool input.
use super::{
    Result,
    config::{Limits, Source, decode, string},
    ensure,
};
use serde_json::Value;
use std::{collections::BTreeMap, io::Read, time::Duration};
pub struct Headers {
    cursors: Vec<String>,
    links: Vec<String>,
}
pub struct Http<'a> {
    source: &'a Source,
    limits: &'a Limits,
    agent: ureq::Agent,
}
impl<'a> Http<'a> {
    pub fn new(source: &'a Source, limits: &'a Limits) -> Self {
        Self {
            source,
            limits,
            agent: ureq::AgentBuilder::new()
                .try_proxy_from_env(false)
                .redirects(0)
                .timeout(Duration::from_millis(limits.request_timeout_ms))
                .build(),
        }
    }
    pub fn get(&self, route: &str, query: &BTreeMap<String, String>) -> Result<(Value, Headers)> {
        ensure(
            route.starts_with('/') && !route.starts_with("//") && !route.contains(['?', '#']),
            "http: route",
        )?;
        let mut url = self.source.base.clone();
        url.set_path(route);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let response = self
            .agent
            .get(url.as_str())
            .set("Authorization", &self.source.authorization)
            .set("Accept", "application/json")
            .call()
            .map_err(|e| match e {
                ureq::Error::Status(401 | 403, _) => "http: authentication refused",
                ureq::Error::Status(300..=399, _) => "http: redirect refused",
                ureq::Error::Status(404, _) => "http: not found; coverage unavailable",
                _ => "http: source unavailable",
            })?;
        ensure(
            !(300..400).contains(&response.status()),
            "http: redirect refused",
        )?;
        ensure(response.status() == 200, "http: unexpected status")?;
        ensure(
            response.header("Content-Type").is_some_and(|s| {
                s.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("application/json")
            }),
            "http: content type",
        )?;
        ensure(
            response.header("Content-Encoding").unwrap_or("identity") == "identity",
            "http: encoding",
        )?;
        let headers = Headers {
            cursors: response
                .all("X-Next-Cursor")
                .into_iter()
                .map(String::from)
                .collect(),
            links: response.all("Link").into_iter().map(String::from).collect(),
        };
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(self.limits.max_response_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "http: source unavailable")?;
        ensure(
            bytes.len() <= self.limits.max_response_bytes,
            "http: response budget; coverage unavailable",
        )?;
        Ok((decode(&bytes)?, headers))
    }
    pub fn cursor(
        &self,
        response: &Headers,
        route: &str,
        query: &BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        fn opaque(s: &str) -> Result<String> {
            ensure(string(s) && s.len() <= 4096, "cursor: invalid")?;
            Ok(s.into())
        }
        let cursors = &response.cursors;
        ensure(cursors.len() <= 1, "cursor: ambiguous")?;
        let cursor = cursors.first().map(|s| opaque(s)).transpose()?;
        let links = &response.links;
        ensure(links.len() <= 1, "cursor: ambiguous Link")?;
        let linked = if let Some(link) = links.first() {
            let (target, rel) = link
                .trim()
                .strip_prefix('<')
                .and_then(|s| s.split_once('>'))
                .ok_or("cursor: invalid Link")?;
            ensure(
                rel.trim_start()
                    .strip_prefix(';')
                    .is_some_and(|s| s.trim() == "rel=\"next\""),
                "cursor: invalid Link",
            )?;
            let u = url::Url::parse(target).map_err(|_| "cursor: invalid Link")?;
            ensure(
                u.origin() == self.source.base.origin()
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.path() == route
                    && u.fragment().is_none(),
                "cursor: Link scope",
            )?;
            let pairs = u.query_pairs().collect::<Vec<_>>();
            let args = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>();
            let mut expected = query.clone();
            expected.insert(
                "before".into(),
                args.get("before").ok_or("cursor: Link query")?.clone(),
            );
            ensure(
                args.len() == pairs.len() && args == expected,
                "cursor: Link query",
            )?;
            Some(opaque(&args["before"])?)
        } else {
            None
        };
        ensure(
            cursor.is_none() || linked.is_none() || cursor == linked,
            "cursor: conflict",
        )?;
        Ok(cursor.or(linked))
    }
}
