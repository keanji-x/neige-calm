use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use quote::ToTokens;
use syn::visit::Visit;
use syn::{Item, ItemMod, UseTree, Visibility};

const EXPORTED_ENTRY: &str = "guard_forked_blocks";
const PRIVATE_IMPL: &str = "guard_forked_blocks_impl";

/// The structural door of the report write boundary, and the struct carrying its argument set.
const STRUCTURAL_DOOR: &str = "structural_init_report_tx";
const STRUCTURAL_TARGET: &str = "InitialReportTarget";

/// The structural door's parameter list, `(name, type-as-written)`, in order. Types are pinned
/// too: a newtype under an unchanged parameter name would otherwise pass.
const DOOR_PARAMETERS: &[(&str, &str)] = &[
    ("tx", "&mut sqlx::Transaction<'_, sqlx::Sqlite>"),
    ("target", "InitialReportTarget<'_>"),
];

/// The structural door's return type, as written.
const DOOR_RETURN: &str = "Result<(Card, TaskProjectionOutcome), CalmError>";

/// Every field of the door's argument struct, `name -> type-as-written`.
const DOOR_TARGET_FIELDS: &[(&str, &str)] = &[
    ("report_card_id", "&'a str"),
    ("track_id", "&'a str"),
    ("payload", "&'a TrackReportPayload"),
    ("doc", "&'a mut ReportDoc"),
    (
        "declarations",
        "&'a [calm_types::report_blocks::tasks::TaskDeclaration]",
    ),
    (
        "diagnostics",
        "&'a [Vec<calm_types::report_blocks::tasks::Diagnostic>]",
    ),
];

/// Every name that must not appear anywhere in the structural door's signature or its argument
/// struct; both the type and the parameter/field name each concept would land under.
const FORBIDDEN_IN_THE_DOOR: &[&str] = &[
    // Attribution in any shape; `Option<EditAuthor>` is the same hole with a nullable type.
    "EditAuthor",
    "author",
    "WriteAttribution",
    "attribution",
    "WritePolicy",
    "WriteOrigin",
    "policy",
    "origin",
    // No bus and no actor in the signature; the `kernel_events` vector in `TaskProjectionOutcome`
    // is refused by the door's own body when non-empty.
    "EventBus",
    "Event",
    "events",
    "ActorId",
    "actor",
    // No CAS input: the row was INSERTed by this same transaction.
    "if_doc_rev",
    "expected_rev",
    "if_rev",
    // No lifecycle leg and no draft promotion: the track is being created.
    "TrackLifecycle",
    "lifecycle",
    "auto_promote_draft",
    // No recorder gate: there is no agent principal on a create request.
    "RecorderShadowProbe",
    "RecorderShadowDecisionKind",
    "recorder_shadow",
    "probe",
];

/// For every spelling the tables above use, the canonical path it means. `write.rs` carries a
/// `let _: fn(canonical) -> spelling = identical;` line per row, so each compiles only while the
/// two sides are the same type — a local `struct TrackReportPayload` is a compile error.
const RESOLUTION_ANCHORS: &[(&str, &str)] = &[
    (
        "::sqlx::Transaction<'static, ::sqlx::Sqlite>",
        "sqlx::Transaction<'static, sqlx::Sqlite>",
    ),
    ("::core::result::Result<u8, u8>", "Result<u8, u8>"),
    ("crate::model::Card", "Card"),
    (
        "crate::db::sqlite::TaskProjectionOutcome",
        "TaskProjectionOutcome",
    ),
    ("crate::error::CalmError", "CalmError"),
    ("&'static ::core::primitive::str", "&'static str"),
    (
        "::calm_types::track_report::TrackReportPayload",
        "TrackReportPayload",
    ),
    ("crate::track_report_doc::ReportDoc", "ReportDoc"),
    ("::std::vec::Vec<u8>", "Vec<u8>"),
    (
        "::calm_types::report_blocks::tasks::TaskDeclaration",
        "calm_types::report_blocks::tasks::TaskDeclaration",
    ),
    (
        "::calm_types::report_blocks::tasks::Diagnostic",
        "calm_types::report_blocks::tasks::Diagnostic",
    ),
];

/// Every identifier appearing anywhere under a piece of syntax.
#[derive(Default)]
struct Idents(BTreeSet<String>);

impl<'ast> Visit<'ast> for Idents {
    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.0.insert(ident.to_string());
    }
}

/// Every identifier under a *type*, lifetimes excluded.
#[derive(Default)]
struct TypeIdents(BTreeSet<String>);

impl<'ast> Visit<'ast> for TypeIdents {
    fn visit_lifetime(&mut self, _: &'ast syn::Lifetime) {}

    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.0.insert(ident.to_string());
    }
}

/// Every name a file's top-level items bind, `use` renames included — the set that can shadow a glob import.
fn top_level_bindings(items: &[Item]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in items {
        match item {
            Item::Struct(declared) => {
                names.insert(declared.ident.to_string());
            }
            Item::Enum(declared) => {
                names.insert(declared.ident.to_string());
            }
            Item::Union(declared) => {
                names.insert(declared.ident.to_string());
            }
            Item::Type(declared) => {
                names.insert(declared.ident.to_string());
            }
            Item::Trait(declared) => {
                names.insert(declared.ident.to_string());
            }
            Item::TraitAlias(declared) => {
                names.insert(declared.ident.to_string());
            }
            Item::Fn(declared) => {
                names.insert(declared.sig.ident.to_string());
            }
            Item::Const(declared) => {
                names.insert(declared.ident.to_string());
            }
            Item::Static(declared) => {
                names.insert(declared.ident.to_string());
            }
            Item::Mod(declared) => {
                names.insert(declared.ident.to_string());
            }
            Item::Macro(declared) => {
                if let Some(ident) = &declared.ident {
                    names.insert(ident.to_string());
                }
            }
            Item::Use(item_use) => {
                let mut paths = Vec::new();
                collect_use_paths(&item_use.tree, &mut Vec::new(), &mut paths);
                for (_, bound) in paths {
                    names.insert(bound);
                }
            }
            _ => {}
        }
    }
    names
}

/// Every free function *called* under a piece of syntax; `let _ = f;` is a mention, not a call.
#[derive(Default)]
struct Calls(BTreeSet<String>);

impl<'ast> Visit<'ast> for Calls {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*call.func
            && let Some(last) = path.path.segments.last()
        {
            self.0.insert(last.ident.to_string());
        }
        syn::visit::visit_expr_call(self, call);
    }
}

/// The functions `body` calls in the shape `f(args)?;` — an unconditional statement of `body`
/// itself, `?` applied, at least one argument, and no empty literal slice argument. Each clause
/// refuses a hollowed-out shape (`let _ = f;`, `if false { f()? }`, `let _ = f(&[]);`, `f(&[])?;`).
fn unconditional_checked_calls(body: &syn::Block) -> BTreeSet<String> {
    fn peel(expr: &syn::Expr) -> &syn::Expr {
        match expr {
            syn::Expr::Paren(inner) => peel(&inner.expr),
            syn::Expr::Group(inner) => peel(&inner.expr),
            other => other,
        }
    }

    fn is_empty_literal_slice(expr: &syn::Expr) -> bool {
        match peel(expr) {
            syn::Expr::Reference(reference) => is_empty_literal_slice(&reference.expr),
            syn::Expr::Array(array) => array.elems.is_empty(),
            _ => false,
        }
    }

    let mut called = BTreeSet::new();
    for statement in &body.stmts {
        let syn::Stmt::Expr(expr, Some(_)) = statement else {
            continue;
        };
        let syn::Expr::Try(tried) = peel(expr) else {
            continue;
        };
        let syn::Expr::Call(call) = peel(&tried.expr) else {
            continue;
        };
        let syn::Expr::Path(path) = &*call.func else {
            continue;
        };
        let Some(last) = path.path.segments.last() else {
            continue;
        };
        if call.args.is_empty() || call.args.iter().any(is_empty_literal_slice) {
            continue;
        }
        called.insert(last.ident.to_string());
    }
    called
}

/// A piece of syntax as written, with every space removed, so expected spellings survive rustfmt
/// wrapping. This compares spelling, not resolved meaning.
fn rendered(tokens: &impl ToTokens) -> String {
    tokens
        .to_token_stream()
        .to_string()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

/// The door's argument set is pinned by name AND written type, plus the return type, so it can
/// name no author, bus, actor, prior revision, lifecycle, or recorder probe. Reads the file
/// rather than the compiled crate because the door is `pub(crate)`.
#[test]
fn the_structural_door_cannot_name_an_author_an_actor_or_a_revision() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/track_report/write.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));

    let door = syntax
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(function) if function.sig.ident == STRUCTURAL_DOOR => Some(function),
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{STRUCTURAL_DOOR}` vanished from {}", path.display()));

    // The parameter list itself, name *and* written type.
    let parameters: Vec<(String, String)> = door
        .sig
        .inputs
        .iter()
        .map(|input| match input {
            syn::FnArg::Receiver(_) => panic!("{STRUCTURAL_DOOR} must be a free function"),
            syn::FnArg::Typed(typed) => match &*typed.pat {
                syn::Pat::Ident(ident) => (ident.ident.to_string(), rendered(&*typed.ty)),
                _ => panic!(
                    "{STRUCTURAL_DOOR}: every parameter must be a plain `name: Type` binding"
                ),
            },
        })
        .collect();
    let expected_parameters: Vec<(String, String)> = DOOR_PARAMETERS
        .iter()
        .map(|(name, ty)| {
            (
                (*name).to_string(),
                rendered(&syn::parse_str::<syn::Type>(ty).unwrap()),
            )
        })
        .collect();
    assert_eq!(
        parameters, expected_parameters,
        "the structural door takes the transaction and its target, and nothing else — neither \
         under a new name nor under a new type wearing one of these two names"
    );

    // The return type: a third tuple member is how an event vector leaves without any parameter changing.
    let return_type = match &door.sig.output {
        syn::ReturnType::Default => "()".to_string(),
        syn::ReturnType::Type(_, ty) => rendered(ty),
    };
    assert_eq!(
        return_type,
        rendered(&syn::parse_str::<syn::Type>(DOOR_RETURN).unwrap()),
        "the structural door returns the written card and the projection outcome, and nothing \
         else: a widened return is how this door would come to emit"
    );

    let target = syntax
        .items
        .iter()
        .find_map(|item| match item {
            Item::Struct(item_struct) if item_struct.ident == STRUCTURAL_TARGET => {
                Some(item_struct)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{STRUCTURAL_TARGET}` vanished from {}", path.display()));

    // Fields, pinned name-to-written-type.
    let fields: BTreeMap<String, String> = target
        .fields
        .iter()
        .map(|field| {
            let name = field
                .ident
                .as_ref()
                .unwrap_or_else(|| panic!("{STRUCTURAL_TARGET} must be a named struct"))
                .to_string();
            (name, rendered(&field.ty))
        })
        .collect();
    let expected_fields: BTreeMap<String, String> = DOOR_TARGET_FIELDS
        .iter()
        .map(|(name, ty)| {
            (
                (*name).to_string(),
                rendered(&syn::parse_str::<syn::Type>(ty).unwrap()),
            )
        })
        .collect();
    assert_eq!(
        fields, expected_fields,
        "the structural door's argument struct carries report content and two ids — nothing that \
         names a writer, an authority or a prior revision, and nothing that wraps one of these \
         six types around something that does"
    );

    // The name check over both catches the same concept arriving under an already-expected field.
    let mut names = Idents::default();
    names.visit_signature(&door.sig);
    names.visit_item_struct(target);
    let smuggled: Vec<&str> = FORBIDDEN_IN_THE_DOOR
        .iter()
        .copied()
        .filter(|forbidden| names.0.contains(*forbidden))
        .collect();
    assert!(
        smuggled.is_empty(),
        "the structural door must not be able to name {smuggled:?}. Each of these is absent for a \
         stated reason (see the door's own doc comment); reintroducing one is a design decision, \
         not a signature tweak."
    );
}

/// A `struct TrackReportPayload` defined in `write.rs` shadows the glob import and renders
/// byte-for-byte the same, so: no top-level binding of `write.rs` may be spelled like a pinned
/// type, and the resolution anchors must be present for every pinned spelling.
#[test]
fn the_pinned_spellings_resolve_to_the_types_they_name() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/track_report/write.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));

    let mut pinned = TypeIdents::default();
    for (_, written) in DOOR_PARAMETERS.iter().chain(DOOR_TARGET_FIELDS) {
        pinned.visit_type(&syn::parse_str::<syn::Type>(written).unwrap());
    }
    pinned.visit_type(&syn::parse_str::<syn::Type>(DOOR_RETURN).unwrap());
    pinned.0.remove(STRUCTURAL_TARGET);

    let shadows: Vec<String> = top_level_bindings(&syntax.items)
        .into_iter()
        .filter(|name| pinned.0.contains(name))
        .collect();
    assert!(
        shadows.is_empty(),
        "`{}` binds {shadows:?} at its top level, and every one of those spellings appears in a \
         pinned signature. A same-named item defined or imported here shadows what `use super::*` \
         provides, which leaves the pinned text identical while the door's argument set becomes \
         something else.",
        path.display()
    );

    // rustfmt wraps the longer anchors, and a wrapped `fn(A,) -> B` renders with a trailing comma;
    // dropping `,)` is the whole normalization needed.
    let anchors: String = syntax
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Const(constant) if constant.ident == "_" => {
                Some(rendered(constant).replace(",)", ")"))
            }
            _ => None,
        })
        .collect();
    for (canonical, spelling) in RESOLUTION_ANCHORS {
        let line = rendered(
            &syn::parse_str::<syn::Type>(&format!("fn({canonical}) -> {spelling}")).unwrap(),
        )
        .replace(",)", ")");
        assert!(
            anchors.contains(&line),
            "`{}` must anchor `{spelling}` to `{canonical}`: a `let _: fn({canonical}) -> \
             {spelling} = identical;` line in its `const _` block. Without it that spelling is \
             pinned as text only, and a shadowing item in this file satisfies the text.",
            path.display()
        );
    }

    let mut anchored = TypeIdents::default();
    for (_, spelling) in RESOLUTION_ANCHORS {
        anchored.visit_type(&syn::parse_str::<syn::Type>(spelling).unwrap());
    }
    let unanchored: Vec<&String> = pinned.0.difference(&anchored.0).collect();
    assert!(
        unanchored.is_empty(),
        "the pinned signature names {unanchored:?}, which no row of RESOLUTION_ANCHORS covers. \
         Every spelling the tables pin has to be anchored to a canonical path, or it is pinned as \
         text only."
    );
}

#[test]
fn fork_rule_one_exemption_has_one_structural_entry() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/routes/tracks/fork_guard.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    let items = &syntax.items;

    let entry = items.iter().find_map(|item| match item {
        Item::Fn(function) if function.sig.ident == EXPORTED_ENTRY => Some(function),
        _ => None,
    });
    let entry = entry.expect("fork guard entry vanished");
    assert!(
        is_not_wider_than_tracks(&entry.vis, 0),
        "{EXPORTED_ENTRY} must not be visible beyond crate::routes::tracks"
    );

    let mut exported_entries = BTreeSet::new();
    collect_module_exports(items, "fork_guard", &mut exported_entries);
    collect_parent_reexports(&mut exported_entries);
    let expected_entries = if matches!(entry.vis, Visibility::Inherited) {
        BTreeSet::new()
    } else {
        BTreeSet::from([format!("fork_guard::{EXPORTED_ENTRY}")])
    };
    assert_eq!(
        exported_entries, expected_entries,
        "the exemption module must export exactly its fork-shaped entry"
    );

    let implementation = items.iter().find_map(|item| match item {
        Item::Fn(function) if function.sig.ident == PRIVATE_IMPL => Some(function),
        _ => None,
    });
    let implementation = implementation.expect("private fork guard implementation vanished");
    assert!(
        matches!(implementation.vis, Visibility::Inherited),
        "{PRIVATE_IMPL} must stay module-private"
    );

    assert!(
        items.iter().all(|item| !matches!(item, Item::Enum(_))),
        "the exemption module must not reintroduce a constructible exemption enum"
    );
}

/// The release belt stays in `prepare_fork_report`, upstream of the shared structural door.
/// Syntactic on purpose: `normalize_task_privilege_fields` strips the flag first, so deleting the
/// belt call leaves the whole package green; a belt on the shared door would also guard template
/// instantiation, which has never had one.
#[test]
fn the_release_belt_stays_next_to_the_normalization_it_belts() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let tracks_path = manifest.join("src/routes/tracks.rs");
    let tracks_source = std::fs::read_to_string(&tracks_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", tracks_path.display()));
    let tracks = syn::parse_file(&tracks_source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", tracks_path.display()));
    let prepare = tracks
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(function) if function.sig.ident == "prepare_fork_report" => Some(function),
            _ => None,
        })
        .expect("`prepare_fork_report` vanished from routes/tracks.rs");
    // The belt: `guard_forked_blocks(<something not an empty literal>)?;` as an unconditional statement.
    assert!(
        unconditional_checked_calls(&prepare.block).contains(EXPORTED_ENTRY),
        "`prepare_fork_report` must call `{EXPORTED_ENTRY}(…)?` from an unconditional statement \
         of its own body, on arguments that are not an empty literal slice: the belt and the \
         normalization it belts are only meaningful adjacent to each other, and a call that is \
         merely mentioned, nested under a condition, has its `Err` discarded, or is handed an \
         empty slice is not the belt a fork passes through"
    );

    // The normalization runs per block, inside the loop, so the statement-level rule does not apply.
    let mut prepare_calls = Calls::default();
    prepare_calls.visit_block(&prepare.block);
    assert!(
        prepare_calls.0.contains("normalize_task_privilege_fields"),
        "`prepare_fork_report` must still call `normalize_task_privilege_fields`: the belt is a \
         belt over that normalization, and means nothing without it"
    );

    let door_path = manifest.join("src/track_report/write.rs");
    let door_source = std::fs::read_to_string(&door_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", door_path.display()));
    let door = syn::parse_file(&door_source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", door_path.display()));
    let mut door_names = Idents::default();
    door_names.visit_file(&door);
    assert!(
        !door_names.0.contains(EXPORTED_ENTRY),
        "the fork belt must not move onto the shared structural door: that door also serves \
         `TrackInit::Template`, which has never carried this guard, and hanging it there \
         separates the belt from the normalization in `prepare_fork_report` that it exists to \
         catch a regression in"
    );
}

/// `TemplateDescriptor` is an id handle: no public descriptor body and no sibling public template types.
#[test]
fn template_descriptor_surface_is_id_only() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/plugin_host/manifest.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));

    let descriptor = syntax.items.iter().find_map(|item| match item {
        Item::Struct(item_struct) if item_struct.ident == "TemplateDescriptor" => Some(item_struct),
        _ => None,
    });
    let descriptor = descriptor.expect("TemplateDescriptor vanished from the manifest parser");
    assert!(
        matches!(descriptor.vis, Visibility::Public(_)),
        "TemplateDescriptor must stay pub so track-create can resolve plugin_scope"
    );

    let mut expected_entries = BTreeSet::new();
    expected_entries.insert("id".to_string());
    let mut fields = BTreeSet::new();
    for field in &descriptor.fields {
        let name = field
            .ident
            .as_ref()
            .expect("TemplateDescriptor must be a named struct")
            .to_string();
        fields.insert(name);
    }
    assert_eq!(
        fields, expected_entries,
        "TemplateDescriptor must stay {{ id }} (#1110 S5)"
    );

    let mut public_template_types = BTreeSet::new();
    for item in &syntax.items {
        let (ident, vis) = match item {
            Item::Struct(item_struct) => (&item_struct.ident, &item_struct.vis),
            Item::Enum(item_enum) => (&item_enum.ident, &item_enum.vis),
            Item::Type(item_type) => (&item_type.ident, &item_type.vis),
            _ => continue,
        };
        if matches!(vis, Visibility::Inherited) {
            continue;
        }
        let name = ident.to_string();
        if name == "TemplateDescriptor" {
            continue;
        }
        // Both spellings, so a type reintroduced under the retired `Workflow` prefix is still caught.
        if name.contains("Workflow") || name.contains("Template") || name.ends_with("Descriptor") {
            public_template_types.insert(name);
        }
    }
    assert!(
        public_template_types.is_empty(),
        "new public template-descriptor types must not appear: {public_template_types:?}"
    );
}

fn is_not_wider_than_tracks(visibility: &Visibility, inline_depth: usize) -> bool {
    let Visibility::Restricted(restricted) = visibility else {
        return matches!(visibility, Visibility::Inherited);
    };
    let segments: Vec<_> = restricted
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    match segments.first().map(String::as_str) {
        Some("self") => true,
        Some("super") => {
            segments.iter().all(|segment| segment == "super") && segments.len() <= inline_depth + 1
        }
        Some("crate") => segments.starts_with(&[
            "crate".to_string(),
            "routes".to_string(),
            "tracks".to_string(),
        ]),
        _ => false,
    }
}

fn collect_module_exports(items: &[Item], module: &str, exports: &mut BTreeSet<String>) {
    for item in items {
        match item {
            Item::Fn(function) if !matches!(function.vis, Visibility::Inherited) => {
                exports.insert(format!("{module}::{}", function.sig.ident));
            }
            Item::Use(item_use) if !matches!(item_use.vis, Visibility::Inherited) => {
                collect_use_exports(&item_use.tree, module, exports);
            }
            Item::Mod(item_mod) if !is_cfg_test_module(item_mod) => {
                if let Some((_, items)) = &item_mod.content {
                    collect_module_exports(
                        items,
                        &format!("{module}::{}", item_mod.ident),
                        exports,
                    );
                }
            }
            _ => {}
        }
    }
}

fn is_cfg_test_module(module: &ItemMod) -> bool {
    module.ident == "tests"
        && module.attrs.iter().any(|attribute| {
            attribute.path().is_ident("cfg")
                && attribute
                    .parse_args::<syn::Ident>()
                    .is_ok_and(|argument| argument == "test")
        })
}

fn collect_parent_reexports(exports: &mut BTreeSet<String>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/routes/tracks.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    for item in &syntax.items {
        let Item::Use(item_use) = item else {
            continue;
        };
        if matches!(item_use.vis, Visibility::Inherited) {
            continue;
        }
        let mut paths = Vec::new();
        collect_use_paths(&item_use.tree, &mut Vec::new(), &mut paths);
        for (path, exported_name) in paths {
            if path.iter().any(|segment| segment == "fork_guard") {
                exports.insert(format!("tracks::{exported_name}"));
            }
        }
    }
}

fn collect_use_exports(tree: &UseTree, module: &str, exports: &mut BTreeSet<String>) {
    match tree {
        UseTree::Path(path) => collect_use_exports(&path.tree, module, exports),
        UseTree::Name(name) => {
            exports.insert(format!("{module}::{}", name.ident));
        }
        UseTree::Rename(rename) => {
            exports.insert(format!("{module}::{}", rename.rename));
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use_exports(tree, module, exports);
            }
        }
        UseTree::Glob(_) => {
            exports.insert(format!("{module}::*"));
        }
    }
}

fn collect_use_paths(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    paths: &mut Vec<(Vec<String>, String)>,
) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_use_paths(&path.tree, prefix, paths);
            prefix.pop();
        }
        UseTree::Name(name) => {
            let mut path = prefix.clone();
            path.push(name.ident.to_string());
            paths.push((path, name.ident.to_string()));
        }
        UseTree::Rename(rename) => {
            let mut path = prefix.clone();
            path.push(rename.ident.to_string());
            paths.push((path, rename.rename.to_string()));
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use_paths(tree, prefix, paths);
            }
        }
        UseTree::Glob(_) => paths.push((prefix.clone(), "*".into())),
    }
}
