use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use syn::parse::Parser;
use syn::visit::{self, Visit};
use syn::{Expr, Item, ItemEnum, ItemFn, ItemImpl, ItemMod, ItemType, ItemUse, Meta, UseTree};

use crate::deferred_write_tx_invariant::rust_files;

const EXEMPTION_TYPE: &str = "TaskGuardRule1Exemption";

fn ident_name(ident: &syn::Ident) -> String {
    let name = ident.to_string();
    name.strip_prefix("r#").unwrap_or(&name).to_string()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CfgValue {
    AlwaysFalse,
    AlwaysTrue,
    Unknown,
}

fn cfg_value(meta: &Meta) -> CfgValue {
    match meta {
        Meta::Path(path) if path.is_ident("test") => CfgValue::AlwaysFalse,
        Meta::Path(_) | Meta::NameValue(_) => CfgValue::Unknown,
        Meta::List(list) => {
            let Ok(children) = list.parse_args_with(
                syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
            ) else {
                return CfgValue::Unknown;
            };
            if list.path.is_ident("all") {
                if children
                    .iter()
                    .any(|child| cfg_value(child) == CfgValue::AlwaysFalse)
                {
                    CfgValue::AlwaysFalse
                } else if children
                    .iter()
                    .all(|child| cfg_value(child) == CfgValue::AlwaysTrue)
                {
                    CfgValue::AlwaysTrue
                } else {
                    CfgValue::Unknown
                }
            } else if list.path.is_ident("any") {
                if children
                    .iter()
                    .any(|child| cfg_value(child) == CfgValue::AlwaysTrue)
                {
                    CfgValue::AlwaysTrue
                } else if children
                    .iter()
                    .all(|child| cfg_value(child) == CfgValue::AlwaysFalse)
                {
                    CfgValue::AlwaysFalse
                } else {
                    CfgValue::Unknown
                }
            } else if list.path.is_ident("not") && children.len() == 1 {
                match cfg_value(&children[0]) {
                    CfgValue::AlwaysFalse => CfgValue::AlwaysTrue,
                    CfgValue::AlwaysTrue => CfgValue::AlwaysFalse,
                    CfgValue::Unknown => CfgValue::Unknown,
                }
            } else {
                CfgValue::Unknown
            }
        }
    }
}

fn is_test_gated(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<Meta>()
                .is_ok_and(|meta| cfg_value(&meta) == CfgValue::AlwaysFalse)
    })
}

fn collect_use_aliases(tree: &UseTree, aliases: &mut HashSet<String>) {
    match tree {
        UseTree::Path(path) => collect_use_aliases(&path.tree, aliases),
        UseTree::Name(name) if ident_name(&name.ident) == EXEMPTION_TYPE => {
            aliases.insert(EXEMPTION_TYPE.into());
        }
        UseTree::Rename(rename) if ident_name(&rename.ident) == EXEMPTION_TYPE => {
            aliases.insert(ident_name(&rename.rename));
        }
        UseTree::Group(group) => {
            for item in &group.items {
                collect_use_aliases(item, aliases);
            }
        }
        UseTree::Name(_) | UseTree::Rename(_) | UseTree::Glob(_) => {}
    }
}

struct AliasCollector {
    aliases: HashSet<String>,
}

impl<'ast> Visit<'ast> for AliasCollector {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if !is_test_gated(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_use(&mut self, node: &'ast ItemUse) {
        if !is_test_gated(&node.attrs) {
            collect_use_aliases(&node.tree, &mut self.aliases);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        if !is_test_gated(&node.attrs) {
            visit::visit_item_fn(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        if !is_test_gated(&node.attrs) {
            visit::visit_item_impl(self, node);
        }
    }

    fn visit_item_type(&mut self, node: &'ast ItemType) {
        if is_test_gated(&node.attrs) {
            return;
        }
        if let syn::Type::Path(path) = &*node.ty
            && path
                .path
                .segments
                .last()
                .is_some_and(|segment| self.aliases.contains(&ident_name(&segment.ident)))
        {
            self.aliases.insert(ident_name(&node.ident));
        }
        visit::visit_item_type(self, node);
    }
}

struct GuardAstScan<'a> {
    rel: &'a str,
    aliases: &'a HashSet<String>,
    in_exemption_impl: usize,
    enum_variants: Option<BTreeSet<String>>,
    fork_calls: Vec<String>,
    exemption_uses: Vec<String>,
    saw_regular_guard_call: bool,
}

impl GuardAstScan<'_> {
    fn path_is_exemption_type(&self, path: &syn::Path) -> bool {
        path.segments
            .last()
            .is_some_and(|segment| self.aliases.contains(&ident_name(&segment.ident)))
    }
}

impl<'ast> Visit<'ast> for GuardAstScan<'_> {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if !is_test_gated(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        if !is_test_gated(&node.attrs) {
            visit::visit_item_fn(self, node);
        }
    }

    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        if is_test_gated(&node.attrs) {
            return;
        }
        if ident_name(&node.ident) == EXEMPTION_TYPE {
            assert!(
                self.enum_variants.is_none(),
                "duplicate {EXEMPTION_TYPE} enum"
            );
            self.enum_variants = Some(
                node.variants
                    .iter()
                    .map(|variant| ident_name(&variant.ident))
                    .collect(),
            );
        }
        visit::visit_item_enum(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        if is_test_gated(&node.attrs) {
            return;
        }
        let targets_exemption = match &*node.self_ty {
            syn::Type::Path(path) => self.path_is_exemption_type(&path.path),
            _ => false,
        };
        self.in_exemption_impl += usize::from(targets_exemption);
        visit::visit_item_impl(self, node);
        self.in_exemption_impl -= usize::from(targets_exemption);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Expr::Path(path) = &*node.func
            && let Some(function) = path.path.segments.last()
        {
            match ident_name(&function.ident).as_str() {
                "guard_fork_task_declarations" => self.fork_calls.push(self.rel.into()),
                "guard_task_declarations" if self.rel == "src/wave_report.rs" => {
                    self.saw_regular_guard_call = true;
                }
                _ => {}
            }
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        let segments: Vec<_> = node.path.segments.iter().collect();
        let variant = segments.last().map(|segment| ident_name(&segment.ident));
        let qualifier = segments
            .get(segments.len().saturating_sub(2))
            .map(|segment| ident_name(&segment.ident));
        let qualified = qualifier
            .as_ref()
            .is_some_and(|name| self.aliases.contains(name));
        let self_qualified = self.in_exemption_impl > 0 && qualifier.as_deref() == Some("Self");
        if (qualified || self_qualified)
            && let Some(variant) = variant
        {
            self.exemption_uses.push(format!("{variant}@{}", self.rel));
        }
        visit::visit_expr_path(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        // `matches!(self, Self::Fork)` stores its arguments as tokens in the
        // outer AST. Re-parse expression-shaped macro arguments with syn so
        // qualified variants inside them remain syntax, not searched text.
        let parser = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated;
        if let Ok(expressions) = parser.parse2(node.tokens.clone()) {
            for expression in &expressions {
                self.visit_expr(expression);
            }
        }
        visit::visit_macro(self, node);
    }
}

#[test]
fn task_guard_rule_one_exemption_has_exactly_one_production_call_site() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.join("src");
    assert!(root.is_dir(), "calm-server source root vanished: {root:?}");

    let mut scanned = 0usize;
    let mut saw_guard_module = false;
    let mut saw_wave_route = false;
    let mut saw_regular_guard_call = false;
    let mut fork_calls = Vec::new();
    let mut exemption_uses = Vec::new();
    let mut enum_variants = None;

    for path in rust_files(&root) {
        scanned += 1;
        let rel = path
            .strip_prefix(&manifest_dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        saw_guard_module |= rel == "src/wave_report_task_guard.rs";
        saw_wave_route |= rel == "src/routes/waves.rs";
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {rel}: {error}"));
        let syntax =
            syn::parse_file(&source).unwrap_or_else(|error| panic!("parse {rel}: {error}"));
        let mut aliases = HashSet::from([EXEMPTION_TYPE.to_string()]);
        // Resolve `use ... as Alias` (and simple `type Alias = ...`) before
        // scanning expressions; Rust item order does not constrain aliases.
        loop {
            let before = aliases.len();
            let mut collector = AliasCollector { aliases };
            collector.visit_file(&syntax);
            aliases = collector.aliases;
            if aliases.len() == before {
                break;
            }
        }
        let mut file_scan = GuardAstScan {
            rel: &rel,
            aliases: &aliases,
            in_exemption_impl: 0,
            enum_variants: None,
            fork_calls: Vec::new(),
            exemption_uses: Vec::new(),
            saw_regular_guard_call: false,
        };
        file_scan.visit_file(&syntax);
        if file_scan.enum_variants.is_some() {
            assert!(
                enum_variants.is_none(),
                "duplicate {EXEMPTION_TYPE} definition"
            );
            enum_variants = file_scan.enum_variants;
        }
        fork_calls.extend(file_scan.fork_calls);
        exemption_uses.extend(file_scan.exemption_uses);
        saw_regular_guard_call |= file_scan.saw_regular_guard_call;
    }

    assert!(
        scanned > 100,
        "scan looks vacuous: only {scanned} Rust files"
    );
    assert!(saw_guard_module, "guard module was outside the scan");
    assert!(saw_wave_route, "wave route was outside the scan");
    assert_eq!(
        enum_variants,
        Some(BTreeSet::from(["Fork".to_string(), "None".to_string()])),
        "{EXEMPTION_TYPE} definition must contain exactly None and Fork"
    );
    assert!(
        saw_regular_guard_call,
        "ordinary guard call in wave_report.rs disappeared from the production AST"
    );
    assert_eq!(
        fork_calls.len(),
        1,
        "fork rule-1 exemption must have exactly one production caller: {fork_calls:?}"
    );
    assert_eq!(
        fork_calls[0], "src/routes/waves.rs",
        "the sole fork exemption caller must be the wave-create route: {fork_calls:?}"
    );
    assert_eq!(
        exemption_uses.len(),
        2,
        "exemption variant uses must contain only Self::Fork and the one qualified Fork call: {exemption_uses:?}"
    );
    assert!(
        exemption_uses
            .iter()
            .all(|usage| usage == "Fork@src/wave_report_task_guard.rs"),
        "unexpected exemption variant use: {exemption_uses:?}"
    );
}

struct CallNames {
    names: Vec<String>,
}

impl<'ast> Visit<'ast> for CallNames {
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Expr::Path(path) = &*node.func
            && let Some(function) = path.path.segments.last()
        {
            self.names.push(ident_name(&function.ident));
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_closure(&mut self, _node: &'ast syn::ExprClosure) {}
}

struct ForkWriteBranch {
    call_orders: Vec<(usize, usize)>,
}

impl<'ast> Visit<'ast> for ForkWriteBranch {
    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        let mut cache_write = None;
        let mut projection = None;
        for (statement_index, statement) in node.then_branch.stmts.iter().enumerate() {
            let mut calls = CallNames { names: Vec::new() };
            calls.visit_stmt(statement);
            if calls
                .names
                .iter()
                .any(|name| name == "card_update_with_crdt_tx")
            {
                assert!(
                    cache_write.replace(statement_index).is_none(),
                    "duplicate cache write"
                );
            }
            if calls.names.iter().any(|name| name == "project_tasks_tx") {
                assert!(
                    projection.replace(statement_index).is_none(),
                    "duplicate projection"
                );
            }
        }
        if let (Some(cache_write), Some(projection)) = (cache_write, projection) {
            self.call_orders.push((cache_write, projection));
        }
        visit::visit_expr_if(self, node);
    }
}

#[test]
fn fork_report_cache_write_precedes_task_projection() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let route = manifest_dir.join("src/routes/waves.rs");
    let source = std::fs::read_to_string(&route)
        .unwrap_or_else(|error| panic!("read {}: {error}", route.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", route.display()));
    let create = syntax.items.iter().find_map(|item| match item {
        Item::Fn(function) if function.sig.ident == "create_wave_with_spec_harness" => {
            Some(function)
        }
        _ => None,
    });
    let create = create.expect("create_wave_with_spec_harness function vanished");
    let mut branch = ForkWriteBranch {
        call_orders: Vec::new(),
    };
    branch.visit_block(&create.block);
    assert_eq!(
        branch.call_orders.len(),
        1,
        "cache write and task projection must coexist in exactly one statement block: {:?}",
        branch.call_orders
    );
    let (cache_write, projection) = branch.call_orders[0];
    assert!(
        cache_write < projection,
        "fork task projection ran before its reference-existence cache write"
    );
}
