//! Source-scan guard (#2132, #2175): outside `src/extract.rs`, calm-server's source never names
//! axum's `Json`, `Path` or `Query`, or axum-extra's. Those answer a request rejection in plain
//! text; `calm_server::extract` wraps them (`JsonBody`, `Path`, `Query` to extract, `Json` to
//! answer, which is not an extractor) so a rejection answers an `ErrorBody`.
//!
//! The invariant, checked over each file's raw token stream at every depth (macro bodies and macro
//! inputs included), identifiers compared after stripping `r#`:
//! - no path that runs through `axum` or `axum_extra` reaches a segment `Json`, `Path` or `Query`,
//!   with or without a leading `::`, in any position: a type, a pattern, an expression, a generic
//!   argument, an attribute;
//! - no `use` item that names either crate imports `Json`, `Path` or `Query`, renames anything
//!   (`self` included), or globs; no `extern crate` renames either;
//! - either crate's name appears only as the first segment of a literal path (`axum::…`), never bare
//!   and never followed by a macro metavariable (`axum::$x`).
//!
//! With renames and globs refused, a reference to axum spells the literal path from its crate name,
//! so the first rule sees every one. `calm-server`'s and the workspace's `Cargo.toml` must not rename
//! the dependencies either. Not covered: a `macro_rules!` that assembles a path from fragments
//! passed separately (`$($seg)::*`). `crates/calm-server/clippy.toml` is the type-level backstop:
//! it disallows axum's three types at every type position, through any alias or macro.

use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use syn::ext::IdentExt;

const CRATES: [&str; 2] = ["axum", "axum_extra"];
const WRAPPED: [&str; 3] = ["Json", "Path", "Query"];

/// An identifier's name without `r#`.
fn name(token: &TokenTree) -> Option<String> {
    match token {
        TokenTree::Ident(ident) => Some(ident.unraw().to_string()),
        _ => None,
    }
}

fn is_punct(token: Option<&TokenTree>, ch: char) -> bool {
    matches!(token, Some(TokenTree::Punct(punct)) if punct.as_char() == ch)
}

/// `::` at `tokens[i]`.
fn path_sep(tokens: &[TokenTree], i: usize) -> bool {
    is_punct(tokens.get(i), ':') && is_punct(tokens.get(i + 1), ':')
}

/// Every token of `tokens`, groups opened, in order.
fn flatten(tokens: &[TokenTree], out: &mut Vec<TokenTree>) {
    for token in tokens {
        match token {
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                flatten(&inner, out);
            }
            other => out.push(other.clone()),
        }
    }
}

fn text(tokens: &[TokenTree]) -> String {
    tokens.iter().cloned().collect::<TokenStream>().to_string()
}

#[derive(Default)]
struct Scan {
    hits: Vec<String>,
}

impl Scan {
    /// One level of a token stream, then every group in it.
    fn level(&mut self, stream: TokenStream) {
        let tokens: Vec<TokenTree> = stream.into_iter().collect();
        let mut i = 0;
        while i < tokens.len() {
            match name(&tokens[i]).as_deref() {
                Some("use") => {
                    let end = (i..tokens.len())
                        .find(|&j| is_punct(tokens.get(j), ';'))
                        .unwrap_or(tokens.len() - 1);
                    self.use_item(&tokens[i..=end]);
                    i = end + 1;
                    continue;
                }
                Some("extern") if tokens.get(i + 1).and_then(name).as_deref() == Some("crate") => {
                    let krate = tokens.get(i + 2).and_then(name);
                    let renamed = tokens.get(i + 3).and_then(name).as_deref() == Some("as");
                    if krate.is_some_and(|k| CRATES.contains(&k.as_str())) && renamed {
                        self.hits.push(text(&tokens[i..(i + 5).min(tokens.len())]));
                    }
                    i += 3;
                    continue;
                }
                Some(krate) if CRATES.contains(&krate) => {
                    self.path(&tokens, i);
                }
                _ => {}
            }
            if let TokenTree::Group(group) = &tokens[i] {
                self.level(group.stream());
            }
            i += 1;
        }
    }

    /// The crate name at `tokens[start]`: it must lead a literal path (`axum::…`), whose segments
    /// never reach a wrapped extractor nor stop at a macro metavariable.
    fn path(&mut self, tokens: &[TokenTree], start: usize) {
        let mut end = start + 1;
        if !path_sep(tokens, end) {
            self.hits
                .push(format!("bare `{}`", text(&tokens[start..end])));
            return;
        }
        while path_sep(tokens, end) {
            match tokens.get(end + 2) {
                Some(segment @ TokenTree::Ident(_)) => {
                    end += 3;
                    if name(segment).is_some_and(|s| WRAPPED.contains(&s.as_str())) {
                        self.hits.push(text(&tokens[start..end]));
                        return;
                    }
                }
                Some(TokenTree::Punct(punct)) if punct.as_char() == '$' => {
                    let shown = (end + 4).min(tokens.len());
                    self.hits
                        .push(format!("macro-built path {}", text(&tokens[start..shown])));
                    return;
                }
                _ => return,
            }
        }
    }

    /// A `use` item, its groups opened: one that names either crate may not import a wrapped
    /// extractor, rename, or glob.
    fn use_item(&mut self, item: &[TokenTree]) {
        let mut flat = Vec::new();
        flatten(item, &mut flat);
        let Some(from) = flat
            .iter()
            .position(|t| name(t).is_some_and(|n| CRATES.contains(&n.as_str())))
        else {
            return;
        };
        let after = &flat[from..];
        let refused = after.iter().any(|t| {
            name(t).is_some_and(|n| n == "as" || WRAPPED.contains(&n.as_str()))
                || is_punct(Some(t), '*')
        });
        if refused {
            self.hits.push(text(item));
        }
    }
}

fn refused(source: &str) -> Vec<String> {
    let stream: TokenStream = source.parse().unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    let mut scan = Scan::default();
    scan.level(stream);
    scan.hits
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

#[test]
fn no_calm_server_source_names_an_axum_extractor_outside_extract() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    // The scan root is the whole crate source, handlers included, and the one exempt file exists.
    assert!(
        files.iter().any(|path| path.ends_with("routes/plugins.rs"))
            && files.iter().any(|path| path.ends_with("src/extract.rs")),
        "scan root lost the route handlers or `extract.rs`: {src:?}"
    );
    let mut violations = Vec::new();
    for path in files
        .iter()
        .filter(|path| !path.ends_with("src/extract.rs"))
    {
        let source = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        for hit in refused(&source) {
            violations.push(format!("{}: {hit}", path.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "name axum's extractors only in `src/extract.rs`; use `crate::extract::{{JsonBody, Path, \
         Query}}` to extract and `crate::extract::Json` to answer:\n{}",
        violations.join("\n")
    );
}

/// A dependency renamed in a manifest would give axum a name the scan does not know.
#[test]
fn no_manifest_renames_the_axum_dependencies() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for path in [
        manifest.join("Cargo.toml"),
        manifest.join("../../Cargo.toml"),
    ] {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let squeezed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        for package in ["package=\"axum\"", "package=\"axum-extra\""] {
            assert!(
                !squeezed.contains(package),
                "{}: a dependency renames `{package}`",
                path.display()
            );
        }
    }
}

/// Every form a review found, each refused; the crate's own extractors and response pass.
#[test]
fn the_scan_refuses_every_spelling_of_an_axum_extractor_and_nothing_else() {
    let refused_forms = [
        // Arguments, typed and untyped, and a response type.
        "async fn h(axum::Json(body): axum::Json<Body>) {}",
        "async fn h(::axum::Json(body): ::axum::Json<Body>) {}",
        "fn r() { let _ = post(|axum::Json(body)| async move {}); }",
        "fn r() -> axum::Json<Out> { todo!() }",
        "fn r() { let _ = axum::response::Json(out); }",
        "async fn h(axum::extract::Path(id): axum::extract::Path<String>) {}",
        "async fn h(q: Option<axum::extract::Query<Q>>) {}",
        "fn f(q: axum_extra::extract::Query<Q>) {}",
        // Codex round 2: an inferred closure argument, and a brace pattern.
        "fn r() { let _ = post(|body| async move { let _: axum::Json<V> = body; }); }",
        "fn r() { let _ = post(|axum::Json { 0: body }| async move {}); }",
        // Channel A round 2: raw identifiers.
        "async fn h(axum::r#Json(b): axum::r#Json<V>) {}",
        "use r#axum::r#Json as X;",
        // Channel A round 2: a generic handler routed with axum's type.
        "fn r() { let _ = post(generic::<axum::Json<V>>); }",
        // Channel A round 2: `let` patterns inside closures.
        "fn r() { let _ = post(|j| async move { let axum::Json(b) = j; }); }",
        "fn r() { let _ = get(|p| async move { let axum::extract::Path(id) = p; }); }",
        "fn r() { let _ = get(|q| async move { let axum::extract::Query(q) = q; }); }",
        // Channel A round 2: a macro_rules!-generated handler; its input, and its body.
        "macro_rules! mk { ($n:ident, $t:ty) => { async fn $n(b: $t) {} } } mk!(p, axum::Json<V>);",
        "macro_rules! mk { () => { async fn h(axum::Json(b): axum::Json<V>) {} } }",
        // A crate name that is not the head of a literal path.
        "fn r() { p!(axum); }",
        "macro_rules! m { ($x:ident) => { axum::$x } }",
        // `use` items: imports, renames, globs, re-exports.
        "use axum::Json;",
        "use axum::{Json, Router};",
        "use axum::extract::{Query, State};",
        "use axum::{Router, extract::{Path as Params, State}};",
        "mod a { pub use axum::Json; }",
        "use axum::{self as web}; async fn h(web::extract::Path(id): web::extract::Path<String>) {}",
        "use axum::{self as web}; fn r() { let _ = get(|web::extract::Query(q)| async move {}); }",
        "use axum::extract as ex;",
        "use axum::routing::get as route;",
        "use axum as web;",
        "extern crate axum as web;",
        "use axum::*;",
        "use axum::extract::*;",
        "type Params<T> = axum::extract::Path<T>;",
    ];
    let missed: Vec<&str> = refused_forms
        .into_iter()
        .filter(|source| refused(source).is_empty())
        .collect();
    assert!(missed.is_empty(), "not refused:\n{}", missed.join("\n"));
    let allowed = [
        "use crate::extract::{Json, JsonBody}; \
         async fn h(JsonBody(b): JsonBody<V>) -> Json<Out> { Json(out) }",
        "use crate::extract::{Path, Query}; async fn h(Path(id): Path<String>, Query(q): Query<Q>) {}",
        "use crate::extract::Path as RoutePath;",
        "use axum::{Router, extract::State, routing::post};",
        "use axum::extract::rejection::JsonRejection;",
        "use axum_extra::extract::cookie::{Cookie, SameSite};",
        "#[axum::debug_handler] async fn h() -> axum::response::Response { todo!() }",
        "use std::path::Path; use serde_json::Value as Json;",
        "/// `axum::Json` in a doc comment\nfn f() { let _ = \"axum::Json\"; }",
        "macro_rules! m { () => { crate::extract::Json(1) } }",
    ];
    let flagged: Vec<String> = allowed
        .into_iter()
        .filter_map(|source| {
            let hits = refused(source);
            (!hits.is_empty()).then(|| format!("{source}: {hits:?}"))
        })
        .collect();
    assert!(flagged.is_empty(), "refused:\n{}", flagged.join("\n"));
}
