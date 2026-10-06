//! #2175: every documented operation answers a request its extractors cannot read with an
//! `ErrorBody` (`{error, code}`) at 400, never axum's plain text. This is the behavioural proof that
//! each documented handler extracts through `calm_server::extract`; `extractor_scan.rs` only catches
//! the spellings a contributor would naturally write, undocumented routes included.
//!
//! The cases come from the OpenAPI document, so a new route is swept as soon as it is documented.
//! Each request goes through the production router with a session, a same-origin `Origin` and the
//! loopback peer, so it reaches the operation's extractors, and every other input is well-formed so
//! only the one under test can be refused:
//! - a JSON request body: `{`, sent as `application/json`;
//! - a path parameter: a value its type cannot parse (`%FF`, not UTF-8, for a string; a word for
//!   an integer);
//! - a query string: the required parameters dropped, or else an unparseable value for one typed
//!   parameter (an integer, a boolean, an enum);
//! - a query string again: one scalar parameter given twice (`?path=x&path=x`), the rest filled
//!   with valid values. A typed query is refused for a duplicate field whatever its types, so every
//!   operation with a scalar query parameter has this case, optional strings included.
//!
//! The case counts are checked against counts read off the document, and an operation with no case
//! is printed with the reason. No operation is exempt: each one's extractors run before anything it
//! answers on its own.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::openapi::ApiDoc;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use utoipa::OpenApi;

use super::auth::live_auth_state;
use super::auth_origin::{HOST, app_and_cookie};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Body,
    Path,
    /// The required query parameters dropped, or an unparseable value for a typed one.
    Query,
    /// One scalar query parameter given twice.
    QueryDuplicate,
}

/// The schema a parameter or `$ref` names, with `$ref`s followed.
fn resolve<'a>(doc: &'a Value, schema: &'a Value) -> &'a Value {
    match schema["$ref"].as_str() {
        Some(reference) => {
            let name = reference.trim_start_matches("#/components/schemas/");
            resolve(doc, &doc["components"]["schemas"][name])
        }
        None => schema,
    }
}

/// The types a schema admits, `null` aside: `type` as a string or a list, or `oneOf` members.
fn types(doc: &Value, schema: &Value) -> Vec<String> {
    let schema = resolve(doc, schema);
    if let Some(members) = schema["oneOf"].as_array() {
        return members.iter().flat_map(|m| types(doc, m)).collect();
    }
    if schema["enum"].is_array() {
        return vec!["enum".into()];
    }
    match &schema["type"] {
        Value::String(t) if t != "null" => vec![t.clone()],
        Value::Array(ts) => ts
            .iter()
            .filter_map(Value::as_str)
            .filter(|t| *t != "null")
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

/// A value the schema accepts.
fn good(doc: &Value, schema: &Value) -> String {
    let resolved = resolve(doc, schema);
    if let Some(members) = resolved["oneOf"].as_array()
        && let Some(member) = members.iter().find(|m| !types(doc, m).is_empty())
    {
        return good(doc, member);
    }
    if let Some(first) = resolved["enum"].as_array().and_then(|e| e.first()) {
        return first.as_str().unwrap_or_default().to_string();
    }
    match types(doc, schema).first().map(String::as_str) {
        Some("integer" | "number") => "1".into(),
        Some("boolean") => "true".into(),
        _ => "x".into(),
    }
}

/// A value the schema's extractor cannot parse, when one exists.
fn bad(doc: &Value, schema: &Value, kind: Kind) -> Option<String> {
    let types = types(doc, schema);
    if types.iter().any(|t| t == "integer" || t == "number") {
        Some("not-a-number".into())
    } else if types.iter().any(|t| t == "boolean") {
        Some("maybe".into())
    } else if types.iter().any(|t| t == "enum") {
        Some("not-a-variant".into())
    } else if kind == Kind::Path {
        // Every path parameter is percent-decoded into UTF-8 before it is parsed.
        Some("%FF".into())
    } else {
        None
    }
}

struct Case {
    kind: Kind,
    method: String,
    uri: String,
    body: &'static str,
    headers: Vec<(String, String)>,
}

/// The sweep's cases for one operation: each kind it declares and can be fed unreadable input for.
fn cases(doc: &Value, path: &str, method: &str, operation: &Value) -> Vec<Case> {
    let parameters = operation["parameters"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let of = |place: &str| -> Vec<Value> {
        parameters
            .iter()
            .filter(|p| p["in"] == place)
            .cloned()
            .collect()
    };
    let (path_params, query_params, header_params) = (of("path"), of("query"), of("header"));
    let json_body = operation["requestBody"]["content"]["application/json"].is_object();

    // The well-formed request every case starts from.
    let path_values: BTreeMap<String, String> = path_params
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_string(),
                good(doc, &p["schema"]),
            )
        })
        .collect();
    let query_values: Vec<(String, String)> = query_params
        .iter()
        .filter(|p| p["required"] == true)
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_string(),
                good(doc, &p["schema"]),
            )
        })
        .collect();
    let headers: Vec<(String, String)> = header_params
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_string(),
                "sweep-key".to_string(),
            )
        })
        .collect();
    let uri = |paths: &BTreeMap<String, String>, query: &[(String, String)]| {
        let mut uri = path.to_string();
        for (name, value) in paths {
            uri = uri.replace(&format!("{{{name}}}"), value);
        }
        if !query.is_empty() {
            let pairs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
            uri.push('?');
            uri.push_str(&pairs.join("&"));
        }
        uri
    };
    let body = if json_body { "{}" } else { "" };
    let case = |kind, uri: String, body| Case {
        kind,
        method: method.to_uppercase(),
        uri,
        body,
        headers: headers.clone(),
    };

    let mut out = Vec::new();
    if json_body {
        out.push(case(Kind::Body, uri(&path_values, &query_values), "{"));
    }
    if let Some((name, value)) = path_params.iter().find_map(|p| {
        bad(doc, &p["schema"], Kind::Path).map(|v| (p["name"].as_str().unwrap().to_string(), v))
    }) {
        let mut paths = path_values.clone();
        paths.insert(name, value);
        out.push(case(Kind::Path, uri(&paths, &query_values), body));
    }
    if !query_values.is_empty() {
        out.push(case(Kind::Query, uri(&path_values, &[]), body));
    } else if let Some((name, value)) = query_params.iter().find_map(|p| {
        bad(doc, &p["schema"], Kind::Query).map(|v| (p["name"].as_str().unwrap().to_string(), v))
    }) {
        out.push(case(Kind::Query, uri(&path_values, &[(name, value)]), body));
    }
    if let Some(repeated) = query_params.iter().find(|p| scalar(doc, &p["schema"])) {
        let name = repeated["name"].as_str().unwrap().to_string();
        let value = good(doc, &repeated["schema"]);
        let mut query = query_values.clone();
        if !query.iter().any(|(k, _)| *k == name) {
            query.push((name.clone(), value.clone()));
        }
        query.push((name, value));
        out.push(case(Kind::QueryDuplicate, uri(&path_values, &query), body));
    }
    out
}

/// A parameter that is one value, so a second occurrence is a duplicate field; an array or
/// sequence takes repeated keys legitimately.
fn scalar(doc: &Value, schema: &Value) -> bool {
    !types(doc, schema)
        .iter()
        .any(|t| t == "array" || t == "object")
}

/// The cases the document calls for, counted from its declarations alone, and the reason an
/// operation has none.
fn expected(doc: &Value, operation: &Value) -> (BTreeMap<Kind, usize>, Option<&'static str>) {
    let parameters = operation["parameters"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let query: Vec<&Value> = parameters.iter().filter(|p| p["in"] == "query").collect();
    let mut counts = BTreeMap::new();
    if operation["requestBody"]["content"]["application/json"].is_object() {
        counts.insert(Kind::Body, 1);
    }
    if parameters.iter().any(|p| p["in"] == "path") {
        counts.insert(Kind::Path, 1);
    }
    if query
        .iter()
        .any(|p| p["required"] == true || bad(doc, &p["schema"], Kind::Query).is_some())
    {
        counts.insert(Kind::Query, 1);
    }
    if query.iter().any(|p| scalar(doc, &p["schema"])) {
        counts.insert(Kind::QueryDuplicate, 1);
    }
    let reason = counts.is_empty().then_some(if query.is_empty() {
        "no JSON body, no path parameter, no query parameter"
    } else {
        "no JSON body, no path parameter, and every query parameter is an array or object"
    });
    (counts, reason)
}

async fn send(
    app: &axum::Router,
    cookie: &str,
    case: &Case,
) -> (StatusCode, Result<Value, String>) {
    let mut request = Request::builder()
        .method(case.method.as_str())
        .uri(&case.uri)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .extension(axum::extract::ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            4444,
        ))));
    for (name, value) in &case.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(case.body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice::<Value>(&bytes)
        .map_err(|_| String::from_utf8_lossy(&bytes).into_owned());
    (status, body)
}

#[tokio::test]
async fn every_documented_operation_answers_unreadable_input_with_an_error_body() {
    let doc = serde_json::to_value(ApiDoc::openapi()).expect("the document serializes");
    let (app, cookie) = app_and_cookie(live_auth_state("alice", "pw")).await;
    let mut swept: BTreeMap<Kind, usize> = BTreeMap::new();
    let mut declared: BTreeMap<Kind, usize> = BTreeMap::new();
    let mut operations = 0;
    let mut uncased = Vec::new();
    let mut violations = Vec::new();
    for (path, item) in doc["paths"].as_object().expect("paths") {
        for (method, operation) in item.as_object().expect("path item") {
            if !["get", "put", "post", "delete", "patch"].contains(&method.as_str()) {
                continue;
            }
            operations += 1;
            let (counts, reason) = expected(&doc, operation);
            for (kind, n) in counts {
                *declared.entry(kind).or_default() += n;
            }
            if let Some(reason) = reason {
                uncased.push(format!("{method} {path}: {reason}"));
            }
            for case in cases(&doc, path, method, operation) {
                *swept.entry(case.kind).or_default() += 1;
                let (status, body) = send(&app, &cookie, &case).await;
                let error_body = body
                    .as_ref()
                    .is_ok_and(|b| b["code"] == "bad_request" && b["error"].is_string());
                if status != StatusCode::BAD_REQUEST || !error_body {
                    violations.push(format!(
                        "{method} {path} ({:?}: {} {}): {status} {body:?}",
                        case.kind, case.method, case.uri
                    ));
                }
            }
        }
    }
    eprintln!(
        "swept {operations} operations: {swept:?}; {} with nothing to refuse:\n{}",
        uncased.len(),
        uncased.join("\n")
    );
    // Every case the document calls for was sent, and every kind is present: a sweep that dropped
    // cases, or a document that lost a kind, would otherwise pass vacuously.
    assert_eq!(
        swept, declared,
        "cases sent vs cases the document calls for"
    );
    assert_eq!(swept.len(), 4, "{swept:?}");
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}
