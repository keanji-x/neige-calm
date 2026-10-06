//! Source-scan guard (#2132, #2175) against reintroducing axum's `Json`, `Path` or `Query` (or
//! axum-extra's): they answer a request rejection in plain text, where `calm_server::extract`'s
//! (`JsonBody`, `Path`, `Query` to extract, `Json` to answer, which cannot extract) answer an
//! `ErrorBody`.
//!
//! Threat model: this catches the spellings a contributor would naturally write, on documented and
//! undocumented routes alike. Deliberately evasive code is out of scope. The behavioural proof for
//! every documented route is `request_rejections.rs`, which sends each one input its extractors
//! cannot read and requires an `ErrorBody`; clippy's `disallowed-types` (`clippy.toml`) refuses the
//! three types at every type position.
//!
//! What it checks, over each file's raw token stream at every depth (macro bodies and inputs
//! included), names compared without `r#`, everywhere in `src/` except `src/extract.rs`:
//! - a path through `axum` or `axum_extra` that reaches `Json`, `Path` or `Query`, with or without
//!   a leading `::`;
//! - a `use` import, read with its full path, of one of those three, or of the `extract` or
//!   `response` module that holds them; any rename or glob through either crate; an `extern crate`
//!   rename;
//! - either crate's name not at the head of a literal path (bare, or followed by `::$x`);
//! - `include!`, and a `#[path = …]` that leaves `src/` (`..` or an absolute path), which would
//!   bring in code from outside the scanned files.
//!
//! The manifests are parsed as TOML: no dependency gives `axum` or `axum-extra` another name.

use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use syn::ext::IdentExt;

const CRATES: [&str; 2] = ["axum", "axum_extra"];
const WRAPPED: [&str; 3] = ["Json", "Path", "Query"];
/// The modules that hold them: importing one would let `extract::Path` name axum's without `axum`.
const MODULES: [&str; 2] = ["extract", "response"];

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

/// The rest of an attribute after its `#`, when it is `[path = "…"]` or `![path = "…"]`: the
/// literal it names.
fn path_attribute(rest: &[TokenTree]) -> Option<String> {
    let rest = if is_punct(rest.first(), '!') {
        &rest[1..]
    } else {
        rest
    };
    let Some(TokenTree::Group(group)) = rest.first() else {
        return None;
    };
    let inner: Vec<TokenTree> = group.stream().into_iter().collect();
    if group.delimiter() != proc_macro2::Delimiter::Bracket
        || inner.first().and_then(name).as_deref() != Some("path")
        || !is_punct(inner.get(1), '=')
    {
        return None;
    }
    Some(inner.get(2).map(ToString::to_string).unwrap_or_default())
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
                // Code from outside `src/**/*.rs` would escape the scan.
                Some("include") if is_punct(tokens.get(i + 1), '!') => {
                    self.hits.push("include!".into());
                }
                _ => {}
            }
            // A module file under `src/` is scanned like any other; one outside it is not.
            if is_punct(tokens.get(i), '#')
                && let Some(target) = path_attribute(&tokens[i + 1..])
                && (target.contains("..") || target.trim_matches('"').starts_with('/'))
            {
                self.hits.push(format!("#[path = {target}]"));
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

    /// A `use` item: each import it makes, with its full path, may not reach axum's (or
    /// axum-extra's) wrapped extractors or the modules that hold them, rename, or glob.
    fn use_item(&mut self, item: &[TokenTree]) {
        let body = &item[1..item.len().saturating_sub(1)];
        let mut found = Vec::new();
        imports(body, &mut Vec::new(), &mut found);
        for import in found {
            let Some(krate) = import.path.first() else {
                continue;
            };
            if !CRATES.contains(&krate.as_str()) {
                continue;
            }
            let last = import.path.last().map(String::as_str).unwrap_or_default();
            let module = import.path.len() == 2 && MODULES.contains(&last);
            if import.renamed || import.glob || WRAPPED.contains(&last) || module {
                self.hits.push(format!(
                    "use {}{}{}",
                    import.path.join("::"),
                    if import.glob { "::*" } else { "" },
                    if import.renamed { " as …" } else { "" }
                ));
            }
        }
    }
}

/// One import a `use` item makes: its full path, with `self` folded into its parent.
struct Import {
    path: Vec<String>,
    glob: bool,
    renamed: bool,
}

/// The imports of a use tree under `prefix`.
fn imports(tokens: &[TokenTree], prefix: &mut Vec<String>, out: &mut Vec<Import>) {
    let mut i = if path_sep(tokens, 0) { 2 } else { 0 };
    let depth = prefix.len();
    loop {
        match tokens.get(i) {
            Some(segment @ TokenTree::Ident(_)) => {
                let segment = name(segment).unwrap();
                if path_sep(tokens, i + 1) {
                    prefix.push(segment);
                    i += 3;
                    continue;
                }
                let mut path = prefix.clone();
                if segment != "self" {
                    path.push(segment);
                }
                let renamed = tokens.get(i + 1).and_then(name).as_deref() == Some("as");
                out.push(Import {
                    path,
                    glob: false,
                    renamed,
                });
            }
            Some(TokenTree::Punct(punct)) if punct.as_char() == '*' => out.push(Import {
                path: prefix.clone(),
                glob: true,
                renamed: false,
            }),
            Some(TokenTree::Group(group)) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                for tree in inner.split(|t| is_punct(Some(t), ',')) {
                    if !tree.is_empty() {
                        imports(tree, prefix, out);
                    }
                }
            }
            _ => {}
        }
        break;
    }
    prefix.truncate(depth);
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

/// Every dependency in a parsed manifest that names the `axum` or `axum-extra` package under
/// another key: such a key would be a crate name the scan does not know. Every dependency table is
/// read, target-specific and workspace ones included, whatever the TOML spelling.
fn renamed_axum_dependencies(manifest: &str) -> Vec<String> {
    let manifest: toml::Table = manifest.parse().unwrap_or_else(|e| panic!("manifest: {e}"));
    fn collect<'a>(owner: &str, table: &'a toml::Table, out: &mut Vec<(String, &'a toml::Table)>) {
        for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(deps) = table.get(kind).and_then(toml::Value::as_table) {
                out.push((format!("{owner}{kind}"), deps));
            }
        }
    }
    let mut tables = Vec::new();
    collect("", &manifest, &mut tables);
    if let Some(workspace) = manifest.get("workspace").and_then(toml::Value::as_table) {
        collect("workspace.", workspace, &mut tables);
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (target, table) in targets {
            if let Some(table) = table.as_table() {
                collect(&format!("target.{target}."), table, &mut tables);
            }
        }
    }
    let mut renamed = Vec::new();
    for (owner, deps) in tables {
        for (key, entry) in deps {
            let package = entry
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(key);
            if ["axum", "axum-extra"].contains(&package) && package != key {
                renamed.push(format!("{owner}.{key} = {package}"));
            }
        }
    }
    renamed
}

/// Neither calm-server's manifest nor the workspace's gives axum another crate name.
#[test]
fn no_manifest_renames_the_axum_dependencies() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for path in [
        manifest.join("Cargo.toml"),
        manifest.join("../../Cargo.toml"),
    ] {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let renamed = renamed_axum_dependencies(&text);
        assert!(renamed.is_empty(), "{}: {renamed:?}", path.display());
    }
    let refused = [
        "[dependencies]\nweb = { package = 'axum', version = '0.8' }",
        "[dependencies.web]\npackage = \"axum\"",
        "[dependencies]\nweb.package = \"axum\"",
        "[dev-dependencies]\n\"web\" = { package = \"axum\" }",
        "[target.'cfg(unix)'.dependencies]\nweb = { package = \"axum-extra\" }",
        "[workspace.dependencies]\nweb = { package = \"axum\" }",
    ];
    for manifest in refused {
        assert_eq!(renamed_axum_dependencies(manifest).len(), 1, "{manifest}");
    }
    let allowed = "[dependencies]\naxum = { version = \"0.8\" }\n\"axum-extra\" = \"0.10\"\n\
                   web = { package = \"tower-http\" }";
    assert!(renamed_axum_dependencies(allowed).is_empty());
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
        // Round 3: an unrenamed import of the module that holds them, so `extract::Path` would
        // name axum's without `axum`.
        "use axum::extract; fn r() { let _ = get(|p| async move { let extract::Path(id) = p; }); }",
        "use axum::response;",
        "use axum::extract::{self, State};",
        "use axum::{Router, extract::{self, State}};",
        "use axum_extra::extract;",
        "mod m { pub use axum::extract; } use m::*;",
        // Round 3: code pulled in from outside the scanned files.
        "include!(\"elsewhere.rs\");",
        "#[path = \"../elsewhere.rs\"] mod elsewhere;",
        "#[path = \"/tmp/elsewhere.rs\"] mod elsewhere;",
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
        // Round 3: a grouped import keeps each path's own ancestry.
        "use {axum::Router, std::path::Path};",
        "use {axum::extract::State, crate::extract::{Json, Query}};",
        "use axum;",
        "#[path = \"codex_appserver/tool_names_kernel_tests.rs\"] mod tests;",
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
