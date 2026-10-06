//! #2132: the statuses the OpenAPI document lists for what an operation declares are the ones it
//! answers. `openapi::DeclaredResponses` adds them from two declarations, and these tests pin both
//! to the code that answers them:
//! - a JSON request body is declared exactly where the handler takes `JsonBody`, so its
//!   400/413/415/422 rejections are listed exactly there;
//! - `security(())` is declared exactly where the router does not apply `require_session`, so its
//!   401, and its 403 for a cross-origin write, are listed exactly where the router answers them.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::openapi::ApiDoc;
use serde_json::Value;
use syn::parse::ParseStream;
use syn::visit::{self, Visit};
use tower::ServiceExt;
use utoipa::OpenApi;

use super::auth::live_auth_state;
use super::auth_origin::{HOST, app_and_cookie};

/// One `#[utoipa::path]` handler as its source declares it.
#[derive(Debug)]
struct Annotated {
    handler: String,
    method: String,
    path: String,
    takes_json_body: bool,
}

#[derive(Default)]
struct Handlers {
    file: String,
    found: Vec<Annotated>,
}

impl Handlers {
    fn check(&mut self, attrs: &[syn::Attribute], signature: &syn::Signature) {
        for attr in attrs {
            let segments: Vec<String> = attr
                .path()
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            if segments != ["utoipa", "path"] {
                continue;
            }
            let (method, path) = attr
                .parse_args_with(method_and_path)
                .unwrap_or_else(|e| panic!("{}::{}: {e}", self.file, signature.ident));
            let takes_json_body = signature.inputs.iter().any(|input| {
                let syn::FnArg::Typed(argument) = input else {
                    return false;
                };
                let mut names = NamesJsonBody::default();
                names.visit_type(&argument.ty);
                names.found
            });
            self.found.push(Annotated {
                handler: format!("{}::{}", self.file, signature.ident),
                method,
                path,
                takes_json_body,
            });
        }
    }
}

impl<'ast> Visit<'ast> for Handlers {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        self.check(&item.attrs, &item.sig);
        visit::visit_item_fn(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.check(&item.attrs, &item.sig);
        visit::visit_impl_item_fn(self, item);
    }
}

/// The attribute's leading method and its top-level `path = "…"`.
fn method_and_path(input: ParseStream) -> syn::Result<(String, String)> {
    let method: syn::Ident = input.parse()?;
    input.step(|cursor| {
        let mut rest = *cursor;
        while let Some((_, next)) = rest.token_tree() {
            if let Some((ident, after_ident)) = rest.ident()
                && ident == "path"
                && let Some((punct, after_eq)) = after_ident.punct()
                && punct.as_char() == '='
                && let Some((literal, _)) = after_eq.literal()
            {
                let literal = literal.to_string();
                let path = literal.trim_matches('"').to_string();
                return Ok(((method.to_string(), path), syn::buffer::Cursor::empty()));
            }
            rest = next;
        }
        Err(cursor.error("no `path = \"…\"`"))
    })
}

/// Whether a type mentions a path segment named exactly `JsonBody`.
#[derive(Default)]
struct NamesJsonBody {
    found: bool,
}

impl<'ast> Visit<'ast> for NamesJsonBody {
    fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
        if segment.ident == "JsonBody" {
            self.found = true;
        }
        visit::visit_path_segment(self, segment);
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let path = entry.expect("read_dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn annotated_handlers() -> Vec<Annotated> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut scan = Handlers::default();
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let file = syn::parse_file(&source).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        scan.file = path.strip_prefix(&src).unwrap().display().to_string();
        scan.visit_file(&file);
    }
    scan.found
}

fn document() -> Value {
    serde_json::to_value(ApiDoc::openapi()).expect("the document serializes")
}

/// Whether the operation lists `status` with an `ErrorBody` answer.
fn lists_error(operation: &Value, status: &str) -> bool {
    operation["responses"][status]["content"]["application/json"]["schema"]["$ref"]
        == "#/components/schemas/ErrorBody"
}

/// Every `#[utoipa::path]` handler is in the document; it declares a JSON request body exactly when
/// it takes `JsonBody`, and then lists each status a `JsonBody` rejection answers.
#[test]
fn a_json_body_is_declared_exactly_where_jsonbody_answers_and_lists_its_rejections() {
    let doc = document();
    let handlers = annotated_handlers();
    // The scan reaches the route handlers, both kinds of them.
    assert!(
        handlers
            .iter()
            .any(|h| h.handler == "auth.rs::login_handler" && h.takes_json_body)
            && handlers.iter().any(|h| !h.takes_json_body),
        "scan lost the route handlers: {handlers:?}"
    );
    let mut violations = Vec::new();
    for handler in &handlers {
        let operation = &doc["paths"][&handler.path][&handler.method];
        if operation.is_null() {
            violations.push(format!(
                "{}: `{} {}` is not in `ApiDoc`'s paths",
                handler.handler, handler.method, handler.path
            ));
            continue;
        }
        let declares_json_body =
            operation["requestBody"]["content"]["application/json"].is_object();
        if declares_json_body != handler.takes_json_body {
            violations.push(format!(
                "{}: takes JsonBody = {}, declares a JSON request body = {declares_json_body}",
                handler.handler, handler.takes_json_body
            ));
        }
        if handler.takes_json_body {
            for status in ["400", "413", "415", "422"] {
                if !lists_error(operation, status) {
                    violations.push(format!(
                        "{}: takes JsonBody but does not list {status} ErrorBody",
                        handler.handler
                    ));
                }
            }
        }
    }
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

/// Whether the document requires the session of this operation: its own `security` replaces the
/// document's, and an empty requirement (`security(())`) lets an anonymous request through.
fn requires_session(doc: &Value, operation: &Value) -> bool {
    let anyof = match operation.get("security") {
        Some(own) => own,
        None => &doc["security"],
    };
    let anyof = anyof.as_array().map(Vec::as_slice).unwrap_or_default();
    !anyof.is_empty()
        && !anyof
            .iter()
            .any(|requirement| requirement.as_object().is_some_and(|r| r.is_empty()))
}

async fn answer(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    origin: Option<&str>,
) -> StatusCode {
    let mut request = Request::builder()
        .method(method.to_uppercase().as_str())
        .uri(uri)
        .header(header::HOST, HOST);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    if let Some(origin) = origin {
        request = request.header(header::ORIGIN, origin);
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

/// Through the production router, an anonymous request is refused 401 exactly by the operations
/// the document requires the session of, and each of those lists 401; each such write, with a
/// session and a foreign `Origin`, is refused 403 before its handler runs, and lists 403.
#[tokio::test]
async fn the_session_is_required_exactly_where_the_router_gates_it_and_its_refusals_are_listed() {
    let doc = document();
    let (app, cookie) = app_and_cookie(live_auth_state("alice", "pw")).await;
    let mut violations = Vec::new();
    let mut gated = 0;
    let mut open = 0;
    for (path, item) in doc["paths"].as_object().expect("paths") {
        let uri = path
            .split('/')
            .map(|segment| {
                if segment.starts_with('{') {
                    "x"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        for (method, operation) in item.as_object().expect("path item") {
            if !["get", "put", "post", "delete", "patch"].contains(&method.as_str()) {
                continue;
            }
            let requires = requires_session(&doc, operation);
            let anonymous = answer(&app, method, &uri, None, None).await;
            if (anonymous == StatusCode::UNAUTHORIZED) != requires {
                violations.push(format!(
                    "{method} {path}: requires the session = {requires}, anonymous answered {anonymous}"
                ));
            }
            if !requires {
                open += 1;
                continue;
            }
            gated += 1;
            if !lists_error(operation, "401") {
                violations.push(format!(
                    "{method} {path}: requires the session, lists no 401"
                ));
            }
            if method == "get" {
                continue;
            }
            let foreign = answer(
                &app,
                method,
                &uri,
                Some(&cookie),
                Some("http://192.168.1.5:4050"),
            )
            .await;
            if foreign != StatusCode::FORBIDDEN {
                violations.push(format!(
                    "{method} {path}: a cross-origin write answered {foreign}, not 403"
                ));
            }
            if !lists_error(operation, "403") {
                violations.push(format!(
                    "{method} {path}: a write that requires the session lists no 403"
                ));
            }
        }
    }
    assert!(gated > 0 && open > 0, "gated {gated}, open {open}");
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}
