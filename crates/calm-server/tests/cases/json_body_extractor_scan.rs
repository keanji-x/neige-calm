//! Source-scan guard: no function or closure in calm-server takes `axum::Json` as an argument.
//! A handler that extracts its body with `Json<T>` answers a rejection with axum's plain-text body;
//! `calm_server::json_body::JsonBody<T>` answers the same rejection as an `ErrorBody` (#2132).
//! The rule is by name, so `Json<T>`, `axum::Json<T>` and `Option<Json<T>>` are all caught;
//! returning `Json<T>` stays allowed. A name rule is only sound while `Json` has no other name, so
//! the scan also refuses every alias: a `use` that renames axum's `Json` (`use axum::Json as X`,
//! `use axum::extract::Json as X`, `use axum::{Json as X}`), and any `type` alias whose aliased
//! type names a `Json`.

use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

/// Every argument type that names `Json` anywhere in it, as `fn name: type`, and every alias of a
/// `Json`, as `use …` / `type …`.
#[derive(Default)]
struct JsonArguments {
    hits: Vec<String>,
}

impl JsonArguments {
    fn check(&mut self, owner: &str, ty: &syn::Type) {
        let mut names = NamesJson::default();
        names.visit_type(ty);
        if names.found {
            self.hits
                .push(format!("{owner}: {}", quote::ToTokens::to_token_stream(ty)));
        }
    }
}

impl JsonArguments {
    /// A `use` that renames `Json` under a path through `axum`, at any depth of a grouped tree.
    fn check_use(&mut self, prefix: &mut Vec<String>, tree: &syn::UseTree) {
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
                if rename.ident == "Json" && prefix.iter().any(|segment| segment == "axum") {
                    self.hits.push(format!(
                        "use {}::Json as {}",
                        prefix.join("::"),
                        rename.rename
                    ));
                }
            }
            syn::UseTree::Name(_) | syn::UseTree::Glob(_) => {}
        }
    }

    /// A `type` alias whose aliased type names a `Json`: the alias would hide it from the name rule.
    fn check_alias(&mut self, ident: &syn::Ident, ty: &syn::Type) {
        let mut names = NamesJson::default();
        names.visit_type(ty);
        if names.found {
            self.hits.push(format!(
                "type {ident} = {}",
                quote::ToTokens::to_token_stream(ty)
            ));
        }
    }
}

impl<'ast> Visit<'ast> for JsonArguments {
    fn visit_signature(&mut self, signature: &'ast syn::Signature) {
        for input in &signature.inputs {
            if let syn::FnArg::Typed(argument) = input {
                self.check(&format!("fn {}", signature.ident), &argument.ty);
            }
        }
        visit::visit_signature(self, signature);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.check_use(&mut Vec::new(), &item.tree);
        visit::visit_item_use(self, item);
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
            if let syn::Pat::Type(argument) = input {
                self.check("closure", &argument.ty);
            }
        }
        visit::visit_expr_closure(self, closure);
    }
}

/// Whether a type mentions a path segment named exactly `Json`.
#[derive(Default)]
struct NamesJson {
    found: bool,
}

impl<'ast> Visit<'ast> for NamesJson {
    fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
        if segment.ident == "Json" {
            self.found = true;
        }
        visit::visit_path_segment(self, segment);
    }
}

fn json_arguments(source: &str) -> Vec<String> {
    let file = syn::parse_file(source).unwrap_or_else(|e| panic!("parse: {e}"));
    let mut scan = JsonArguments::default();
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
fn no_calm_server_function_takes_axum_json_as_an_argument() {
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
        for hit in json_arguments(&source) {
            violations.push(format!("{}:{hit}", path.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "extract a JSON body with `crate::json_body::JsonBody<T>`, not `axum::Json<T>`:\n{}",
        violations.join("\n")
    );
}

/// Each shape the rule names is flagged on its own; the allowed shapes are not.
#[test]
fn the_scan_flags_every_json_argument_shape_and_nothing_else() {
    let flagged = [
        "async fn h(Json(body): Json<Body>) {}",
        "async fn h(axum::Json(body): axum::Json<Body>) {}",
        "async fn h(body: Option<Json<Body>>) {}",
        "impl X { async fn h(&self, Json(body): Json<Body>) {} }",
        "fn r() { let _ = post(|Json(body): Json<Body>| async move {}); }",
        // Aliases, each on its own: the alias line is the one hit, whatever uses it.
        "use axum::Json as BodyJson; async fn h(BodyJson(body): BodyJson<Body>) {}",
        "use axum::extract::Json as BodyJson;",
        "use axum::{Json as BodyJson, Router};",
        "use axum::{extract::{Json as BodyJson, State}, Router};",
        "type BodyJson<T> = axum::Json<T>; async fn h(body: BodyJson<Body>) {}",
        "type BodyJson<T> = Json<T>;",
        "impl X for Y { type Body = axum::Json<Body>; }",
    ];
    for source in flagged {
        assert_eq!(json_arguments(source).len(), 1, "{source}");
    }
    let allowed = [
        "async fn h(JsonBody(body): JsonBody<Body>) -> Json<Out> { todo!() }",
        "async fn h() -> Result<Json<Out>, CalmError> { todo!() }",
        "fn r() { let body: Json<Out> = Json(out); }",
        "use axum::Json;",
        "use axum::{Json, Router};",
        "use crate::json_body::JsonBody as Body;",
        "use serde_json::Value as Json;",
        "type Answer = Result<Out, CalmError>;",
    ];
    for source in allowed {
        assert!(json_arguments(source).is_empty(), "{source}");
    }
}
