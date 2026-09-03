use std::collections::BTreeSet;
use std::path::PathBuf;

use syn::visit::Visit;
use syn::{Item, ItemMod, UseTree, Visibility};

const EXPORTED_ENTRY: &str = "guard_forked_blocks";
const PRIVATE_IMPL: &str = "guard_forked_blocks_impl";

/// #1252 S2 — the structural door of the report write boundary, and the struct
/// carrying its whole argument set.
const STRUCTURAL_DOOR: &str = "structural_init_report_tx";
const STRUCTURAL_TARGET: &str = "InitialReportTarget";

/// Every name that must not appear anywhere in the structural door's signature
/// or in the struct that carries its arguments.
///
/// Both spellings of each concept, because the mutation this list exists to
/// catch can arrive as either: the type (`EditAuthor`) or the parameter /
/// field name it would land under (`author`). A future rename that keeps the
/// concept has to be added here, which is the point — the list is the
/// statement of what this door may not be able to say.
const FORBIDDEN_IN_THE_DOOR: &[&str] = &[
    // #1115 — attribution in any shape. `Option<EditAuthor>` is the same hole
    // with a nullable type, and both halves of it are named here.
    "EditAuthor",
    "author",
    "WriteAttribution",
    "attribution",
    // The runtime enum this door was explicitly ruled not to take, plus the
    // value it would be matched out of.
    "WritePolicy",
    "WriteOrigin",
    "policy",
    "origin",
    // Q12 — this door cannot emit, and cannot attribute.
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

/// Every identifier appearing anywhere under a piece of syntax.
///
/// `syn`'s `printing` feature would let this be a string compare over rendered
/// tokens; walking idents instead is both narrower and harder to fool with
/// whitespace, and the `visit` feature is already enabled for this crate's dev
/// build.
#[derive(Default)]
struct Idents(BTreeSet<String>);

impl<'ast> Visit<'ast> for Idents {
    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.0.insert(ident.to_string());
    }
}

/// #1252 S2 — the six absences on `track_report::write::structural_init_report_tx`
/// are the whole content of that door, so they are a gate rather than a comment.
///
/// The door writes a forked or templated report onto the report card inside the
/// track-creation transaction. What makes it safe is not something it does; it
/// is the set of things it has **no way to say**: it cannot name an author
/// (#1115 — so `guard_task_declarations` is unreachable from it, because there
/// is nothing to pass), it cannot emit (Q12 — no `EventBus`, no event in the
/// return type, so no `track.report_edited` and no `card.updated`), it cannot
/// compare-and-swap (the row was INSERTed by the same transaction), and it has
/// no lifecycle, auto-promote or recorder-probe leg.
///
/// Prose says that; this test is what makes it fail to build a green run.
/// Add any one of `author: EditAuthor`, `attribution: WriteAttribution`,
/// `events: &EventBus` or `if_doc_rev: u64` to the signature — or smuggle the
/// same field into `InitialReportTarget` — and this goes red on the exact name.
///
/// It reads the file rather than the compiled crate on purpose: the door is
/// `pub(crate)`, so no integration test can name it, and the property under
/// test is the *shape of the signature*, which is a syntactic fact.
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

    // The parameter list itself, by name. This is the assertion that bites on
    // an added argument even if its type is spelled in a way the name list
    // below has never heard of.
    let parameters: Vec<String> = door
        .sig
        .inputs
        .iter()
        .map(|input| match input {
            syn::FnArg::Receiver(_) => panic!("{STRUCTURAL_DOOR} must be a free function"),
            syn::FnArg::Typed(typed) => match &*typed.pat {
                syn::Pat::Ident(ident) => ident.ident.to_string(),
                _ => panic!(
                    "{STRUCTURAL_DOOR}: every parameter must be a plain `name: Type` binding"
                ),
            },
        })
        .collect();
    assert_eq!(
        parameters,
        vec!["tx".to_string(), "target".to_string()],
        "the structural door takes the transaction and its target, and nothing else"
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

    // Field names, pinned as a set for the same reason: an added field is how
    // an argument arrives once the parameter list is pinned.
    let fields: BTreeSet<String> = target
        .fields
        .iter()
        .map(|field| {
            field
                .ident
                .as_ref()
                .unwrap_or_else(|| panic!("{STRUCTURAL_TARGET} must be a named struct"))
                .to_string()
        })
        .collect();
    let expected_fields: BTreeSet<String> = [
        "report_card_id",
        "track_id",
        "payload",
        "doc",
        "declarations",
        "diagnostics",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(
        fields, expected_fields,
        "the structural door's argument struct carries report content and two ids — nothing that \
         names a writer, an authority or a prior revision"
    );

    // And the name check over both, which is what catches the same concept
    // arriving under a field this test already expects (a `payload` typed
    // `WritePolicy`, say) or inside the return type.
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

/// Issue #1115 / #1252 S2 — the release belt stays in `prepare_fork_report`,
/// upstream of the shared structural door.
///
/// # Why this is a syntactic assertion and not a behavioural one
///
/// Because a behavioural one does not exist, and that was established by
/// running it rather than by argument. **Measured**: deleting the
/// `guard_forked_blocks(&blocks)?` call from `prepare_fork_report` leaves the
/// whole `calm-server` package green — 1259 tests, 0 failures. That is not a
/// coverage hole to be patched with a better fixture; it is what the belt *is*.
/// `normalize_task_privilege_fields` runs over every block first and strips
/// `released_by_user` from every live task, and the tombstone arm's residues are
/// refused earlier still by `validate_payload`, so no production input reaches
/// the belt with the flag set. `fork_guard.rs` says this in its own words: "that
/// no-op is the intended steady state, not evidence the rule is vacuous."
///
/// What the belt buys is measurable, just not from its own call site: delete the
/// `payload.remove("released_by_user")` in `normalize_task_privilege_fields` and
/// a fork sent with **no `X-Calm-Actor` header** — the #1115 accident's original
/// shape, a browser fork — answers 400 carrying the belt's own message, red in
/// `track_report_fork::forked_task_does_not_inherit_the_source_users_release`.
/// That is the belt firing for a `User` author, which is the whole difference
/// from §3.7 Rule 5. Re-branching it on the author instead reds
/// `routes::tracks::fork_guard::tests::
/// fork_guard_exempts_rule_one_but_belts_release_for_every_author`.
///
/// So the two failure modes with no behavioural detector are the two this test
/// takes: the call disappearing, and the belt migrating onto the shared door.
/// The second is the one #1252 S2 makes newly possible and explicitly vetoed —
/// `track_report::write::structural_init_report_tx` serves `TrackInit::Template`
/// as well as `TrackInit::Fork`, so a belt hung there would give template
/// instantiation a guard it has never had, and would separate the belt from the
/// normalization it belts. Both are one-line edits; both are red here.
///
/// It reads the source rather than linking against the symbols because both are
/// module-private (`pub(in crate::routes::tracks)` and `pub(crate)`), which is
/// the same reason the sibling test in this file parses `write.rs`.
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
    let mut prepare_names = Idents::default();
    prepare_names.visit_block(&prepare.block);
    for required in [EXPORTED_ENTRY, "normalize_task_privilege_fields"] {
        assert!(
            prepare_names.0.contains(required),
            "`prepare_fork_report` must still call `{required}`: the belt and the normalization \
             it belts are only meaningful adjacent to each other"
        );
    }

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

/// INV-1110-005 (partial, S5): `TemplateDescriptor` is an id handle. Do not
/// grow a public descriptor body (plan_template / gates / planner_instructions /
/// card_kinds / leftover input_schema) or add sibling public template types.
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
        // Both spellings: #1268 renamed the type, but the ban has to keep
        // catching a type reintroduced under the retired prefix — otherwise
        // the rename would have quietly reopened the hole this test closes.
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
