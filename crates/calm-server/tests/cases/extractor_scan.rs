//! Source-scan guard: no function or closure in calm-server takes axum's `Json`, `Path` or `Query`
//! as an argument. Those extractors answer a rejection with axum's plain-text body; the ones in
//! `calm_server::extract` (`JsonBody<T>`, `Path<T>`, `Query<T>`) answer it as an `ErrorBody`
//! (#2132, #2175).
//!
//! `Json` is refused by name, so `Json<T>`, `axum::Json<T>` and `Option<Json<T>>` are all caught,
//! and so is an argument pattern that destructures a `Json` without a type (`|axum::Json(body)| …`);
//! returning `Json<T>` stays allowed. A name rule is only sound while `Json` has no other name, so
//! the scan also refuses every alias: a `use` that renames any `Json` (`use axum::Json as X`,
//! `use axum::{Json as X}`, or a local re-export renamed, `use self::a::Json as X`), and any `type`
//! alias whose aliased type names a `Json`.
//!
//! `Path` and `Query` cannot be refused by name: the crate's own extractors carry those names, so
//! utoipa still infers the documented parameters. They are refused by origin instead: an argument
//! or alias whose path runs through `axum` and ends in `Path` or `Query`, and every `use` that
//! brings either into scope from `axum`, including the module `axum::extract` itself (through which
//! `extract::Path` would no longer name `axum`), a glob over it, and a rename of `axum`.

use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

/// Every argument that names a refused extractor in its type or its pattern, as
/// `fn name: argument`, and every `use` or `type` that would hide one, as `use …` / `type …`.
#[derive(Default)]
struct RefusedExtractors {
    hits: Vec<String>,
}

impl RefusedExtractors {
    /// One hit per argument, whichever of its type and its pattern names a refused extractor.
    fn check(&mut self, owner: &str, pat: &syn::Pat, ty: Option<&syn::Type>) {
        let mut in_pattern = Names::in_pattern();
        in_pattern.visit_pat(pat);
        let mut in_type = Names::in_type();
        if let Some(ty) = ty {
            in_type.visit_type(ty);
        }
        if in_pattern.found || in_type.found {
            let argument = match ty {
                Some(ty) => format!(
                    "{}: {}",
                    quote::ToTokens::to_token_stream(pat),
                    quote::ToTokens::to_token_stream(ty)
                ),
                None => quote::ToTokens::to_token_stream(pat).to_string(),
            };
            self.hits.push(format!("{owner}: {argument}"));
        }
    }

    /// A `use`, at any depth of a grouped tree, that renames a `Json` (on any path: a module can
    /// re-export axum's under its own), or that brings axum's `Path`, `Query` or `extract` module
    /// into scope, or renames `axum` itself.
    fn check_use(&mut self, prefix: &mut Vec<String>, tree: &syn::UseTree) {
        let through_axum = |prefix: &[String]| prefix.iter().any(|segment| segment == "axum");
        let at = |prefix: &[String], leaf: &dyn std::fmt::Display| {
            let mut path = prefix.join("::");
            if !path.is_empty() {
                path.push_str("::");
            }
            format!("use {path}{leaf}")
        };
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.check_use(prefix, &path.tree);
                prefix.pop();
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.check_use(prefix, item);
                }
            }
            syn::UseTree::Rename(rename) => {
                let leaf = rename.ident.to_string();
                if leaf == "Json"
                    || leaf == "axum"
                    || (through_axum(prefix) && refused_axum_leaf(prefix, &leaf))
                {
                    let leaf = format!("{leaf} as {}", rename.rename);
                    self.hits.push(at(prefix, &leaf));
                }
            }
            syn::UseTree::Name(name) => {
                let leaf = name.ident.to_string();
                if through_axum(prefix) && refused_axum_leaf(prefix, &leaf) {
                    self.hits.push(at(prefix, &leaf));
                }
            }
            syn::UseTree::Glob(_) => {
                if through_axum(prefix)
                    && matches!(prefix.last().map(String::as_str), Some("axum" | "extract"))
                {
                    self.hits.push(at(prefix, &"*"));
                }
            }
        }
    }

    /// A `type` alias whose aliased type names a refused extractor: the alias would hide it.
    fn check_alias(&mut self, ident: &syn::Ident, ty: &syn::Type) {
        let mut names = Names::in_type();
        names.visit_type(ty);
        if names.found {
            self.hits.push(format!(
                "type {ident} = {}",
                quote::ToTokens::to_token_stream(ty)
            ));
        }
    }
}

/// Under a path through `axum`: the `Path` and `Query` extractors, and the `extract` module itself
/// (as a name, or as `self` in a group under it).
fn refused_axum_leaf(prefix: &[String], leaf: &str) -> bool {
    match leaf {
        "Path" | "Query" | "extract" => true,
        "self" => prefix.last().is_some_and(|last| last == "extract"),
        _ => false,
    }
}

impl<'ast> Visit<'ast> for RefusedExtractors {
    fn visit_signature(&mut self, signature: &'ast syn::Signature) {
        for input in &signature.inputs {
            if let syn::FnArg::Typed(argument) = input {
                self.check(
                    &format!("fn {}", signature.ident),
                    &argument.pat,
                    Some(&argument.ty),
                );
            }
        }
        visit::visit_signature(self, signature);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.check_use(&mut Vec::new(), &item.tree);
        visit::visit_item_use(self, item);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if item.ident == "axum" && item.rename.is_some() {
            self.hits.push("extern crate axum as …".into());
        }
        visit::visit_item_extern_crate(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        self.check_alias(&item.ident, &item.ty);
        visit::visit_item_type(self, item);
    }

    fn visit_impl_item_type(&mut self, item: &'ast syn::ImplItemType) {
        self.check_alias(&item.ident, &item.ty);
        visit::visit_impl_item_type(self, item);
    }

    fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
        for input in &closure.inputs {
            match input {
                syn::Pat::Type(argument) => {
                    self.check("closure", &argument.pat, Some(&argument.ty));
                }
                untyped => self.check("closure", untyped, None),
            }
        }
        visit::visit_expr_closure(self, closure);
    }
}

/// Whether a type or a pattern names a refused extractor: in a type, any path segment named
/// exactly `Json`; in a pattern, a tuple-struct destructure whose path ends in `Json` (`Json(body)`,
/// `axum::Json(body)`); in either, a path through `axum` that ends in `Path` or `Query`. At any
/// depth.
struct Names {
    pattern: bool,
    found: bool,
}

impl Names {
    fn in_type() -> Self {
        Self {
            pattern: false,
            found: false,
        }
    }

    fn in_pattern() -> Self {
        Self {
            pattern: true,
            found: false,
        }
    }
}

impl<'ast> Visit<'ast> for Names {
    fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
        if !self.pattern && segment.ident == "Json" {
            self.found = true;
        }
        visit::visit_path_segment(self, segment);
    }

    fn visit_pat_tuple_struct(&mut self, pat: &'ast syn::PatTupleStruct) {
        if pat
            .path
            .segments
            .last()
            .is_some_and(|last| last.ident == "Json")
        {
            self.found = true;
        }
        visit::visit_pat_tuple_struct(self, pat);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let through_axum = path.segments.iter().any(|segment| segment.ident == "axum");
        if through_axum
            && path
                .segments
                .last()
                .is_some_and(|last| last.ident == "Path" || last.ident == "Query")
        {
            self.found = true;
        }
        visit::visit_path(self, path);
    }
}

fn refused_extractors(source: &str) -> Vec<String> {
    let file = syn::parse_file(source).unwrap_or_else(|e| panic!("parse: {e}"));
    let mut scan = RefusedExtractors::default();
    scan.visit_file(&file);
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
fn no_calm_server_function_takes_an_axum_extractor_that_answers_plain_text() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    // The scan root is the whole crate source, handlers included.
    assert!(
        files.iter().any(|path| path.ends_with("routes/plugins.rs")),
        "scan root lost the route handlers: {src:?}"
    );
    let mut violations = Vec::new();
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        for hit in refused_extractors(&source) {
            violations.push(format!("{}:{hit}", path.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "extract with `crate::extract::{{JsonBody, Path, Query}}`, not axum's `Json`, `Path` or \
         `Query`:\n{}",
        violations.join("\n")
    );
}

/// Each shape the rule names is flagged on its own; the allowed shapes are not.
#[test]
fn the_scan_flags_every_refused_extractor_shape_and_nothing_else() {
    let flagged = [
        "async fn h(Json(body): Json<Body>) {}",
        "async fn h(axum::Json(body): axum::Json<Body>) {}",
        "async fn h(body: Option<Json<Body>>) {}",
        "impl X { async fn h(&self, Json(body): Json<Body>) {} }",
        "fn r() { let _ = post(|Json(body): Json<Body>| async move {}); }",
        // Untyped closure patterns: the destructure alone names the extractor.
        "fn r() { let _ = post(|axum::Json(body)| async move {}); }",
        "fn r() { let _ = post(|Json(body)| async move {}); }",
        "fn r() { let _ = post(|(State(s), Json(body))| async move {}); }",
        "async fn h(Json(body): BodyAlias) {}",
        // Aliases, each on its own: the alias line is the one hit, whatever uses it.
        "use axum::Json as BodyJson; async fn h(BodyJson(body): BodyJson<Body>) {}",
        "use axum::extract::Json as BodyJson;",
        "use axum::{Json as BodyJson, Router};",
        "use axum::{extract::{Json as BodyJson, State}, Router};",
        // A local re-export renamed: the path no longer runs through `axum`.
        "mod a { pub use axum::Json; } use self::a::Json as BodyJson;",
        "use self::a::{Json as BodyJson, Other};",
        "type BodyJson<T> = axum::Json<T>; async fn h(body: BodyJson<Body>) {}",
        "type BodyJson<T> = Json<T>;",
        "impl X for Y { type Body = axum::Json<Body>; }",
        // axum's `Path` and `Query`, by origin.
        "async fn h(axum::extract::Path(id): axum::extract::Path<String>) {}",
        "async fn h(q: Option<axum::extract::Query<Q>>) {}",
        "fn r() { let _ = get(|axum::extract::Path(id)| async move {}); }",
        "use axum::extract::Path;",
        "use axum::extract::{Query, State};",
        "use axum::{extract::{Path as Params, State}, Router};",
        "use axum::extract;",
        "use axum::extract::{self, State};",
        "use axum::extract::*;",
        "use axum as web;",
        "extern crate axum as web;",
        "type Params<T> = axum::extract::Path<T>;",
    ];
    for source in flagged {
        assert_eq!(refused_extractors(source).len(), 1, "{source}");
    }
    let allowed = [
        "async fn h(JsonBody(body): JsonBody<Body>) -> Json<Out> { todo!() }",
        "async fn h() -> Result<Json<Out>, CalmError> { todo!() }",
        "fn r() { let body: Json<Out> = Json(out); }",
        "fn r() { if let Some(Json(out)) = answer {} }",
        "fn r() { let _ = post(|JsonBody(body)| async move {}); }",
        "mod a { pub use axum::Json; } use self::a::Json;",
        "use axum::Json;",
        "use axum::{Json, Router};",
        "use crate::extract::JsonBody as Body;",
        "use serde_json::Value as Json;",
        "type Answer = Result<Out, CalmError>;",
        "use crate::extract::{JsonBody, Path, Query};",
        "use crate::extract::Path as RoutePath;",
        "async fn h(Path(id): Path<String>, Query(q): Query<Q>) {}",
        "fn r() { let _ = axum::extract::Path::<String>::from_request_parts(parts, state); }",
        "use axum::extract::{FromRef, State, rejection::JsonRejection};",
        "use std::path::Path;",
    ];
    for source in allowed {
        assert!(refused_extractors(source).is_empty(), "{source}");
    }
}
