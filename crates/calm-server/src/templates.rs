//! The template roster — the report a `template_id` create starts from.
//!
//! #1635 S4 — the built-in templates are **files**: `templates/builtin/*.md`
//! at the crate root, each a `+++` TOML front matter (`id`, `title`; see
//! [`front_matter`]) followed by the report body — the canonical contract
//! header line, the prose maintenance contract, and, for the three plan
//! templates, the intro and one `task` fence per pre-set task. The bytes are
//! compiled in with `include_str!` and parsed once, at first use, into
//! [`TemplateRoster::builtin`]. Nothing in this module authors report text any
//! more; what Rust keeps is protocol — the front matter grammar, the id → entry
//! roster, and the privacy story on [`Template`].
//!
//! The roster reaches the routes as `RouteState.templates`
//! (`&'static TemplateRoster`), so `POST /api/tracks`, `GET /api/track-templates`
//! and the area default-template check all read one value. Today that value is
//! always [`TemplateRoster::builtin`]; #1635 S5 substitutes a roster merged
//! with an operator directory (`site/<stem>` ids) without touching the readers.
//!
//! #1321 S3 — the key→recipe association used to be a second `match` beside
//! the roster (`template_report`), plus a third `#[cfg(test)]` one
//! (`template_tasks`). Three tables keyed off the same constants is three
//! places to keep in sync; since S4 the association is the file itself: an
//! entry cannot be listed without the body it instantiates to, because both
//! come out of one `include_str!`.
//!
//! #1300 S2 — through #1110 S6 this module described the same three plans as
//! *seeded system-area template tracks*, discovered through an overlay payload
//! `{schemaVersion: 1, template_key}` and forked on create. All three of those
//! nouns are gone: no hidden track, no `template_key` writer, no fork. The
//! reason is #1300's own: seeding wrote those reports through `persist_report`
//! as `EditAuthor::User`, i.e. the kernel signing an edit as the user, which
//! was the last production path doing so.

use crate::track_report::TrackReportPayload;
// #1321 S3 — the lenient body reader that needed these went `#[cfg(test)]`
// with this slice; production reads the compiled blocks instead
// (`routes::tracks::InitialReportSnapshot::task_block_payloads`).
#[cfg(test)]
use calm_types::report_blocks::{KIND_TASK, parse_fence, split_body};
use serde_json::Value;
use std::sync::OnceLock;

/// #1635 S4 — the `+++` TOML front matter a template file opens with.
pub mod front_matter;

// The four built-in ids. Protocol, not data: other code names them (the
// plugin manifest's `templates[]` claims, the area default, tests), so they
// stay Rust constants — and `the_key_constants_are_exactly_the_builtin_file_ids`
// holds each to the `id` its file declares.
pub const ISSUE_DEVELOPMENT: &str = "issue-development";
pub const SMALL_CHANGE: &str = "small-change";
pub const INVESTIGATION: &str = "investigation";
/// #1571 — a report-only template: the investment-research contract and its
/// seven empty sections, no pre-set `task` blocks.
pub const INVESTMENT_RESEARCH: &str = "investment-research";

/// The builtin template files, in roster order — which is the order the
/// picker lists them (`GET /api/track-templates`).
///
/// `include_str!`, so a file that is missing or not UTF-8 is a compile error,
/// and an edit to one recompiles this crate. Its front matter and body are
/// only parsed at first use ([`TemplateRoster::builtin`]); a file that does
/// not parse is a panic there, deliberately — these are compile-time inputs
/// shipped inside the binary, and a bad one must be loud, not a shorter
/// picker.
static BUILTIN_SOURCES: [&str; 4] = [
    include_str!("../templates/builtin/issue-development.md"),
    include_str!("../templates/builtin/small-change.md"),
    include_str!("../templates/builtin/investigation.md"),
    include_str!("../templates/builtin/investment-research.md"),
];

/// One roster entry. **Constructible only inside this module and its
/// descendants — in safe Rust.**
///
/// #1318 S2 (第二轮评审 MAJOR) — every field is private and there is no
/// constructor, no `Clone`, no `Copy` and no `Default`, so a struct literal
/// written outside this module's subtree is `E0451` and `*template` cannot be
/// moved out of a borrow either. That is what the compiler checks about "a
/// `&'static Template` came from the roster": in **safe** Rust, outside this
/// module's subtree the only way to name a `Template` value at all is to
/// borrow one of [`TemplateRoster::entries`]. The cross-crate half of that
/// statement is pinned by `tests/cases/templates_privacy.rs` (trybuild).
///
/// #1318 S2 (第三轮评审) — the scope of that sentence is exactly *safe Rust
/// outside this subtree*, and no wider. This crate does not carry
/// `#![forbid(unsafe_code)]`, and `std::mem::transmute` does not consult field
/// visibility: a review channel compiled a forged entry from a leaked tuple of
/// `&'static str`s and `cargo clippy -D warnings` reported nothing. The forgery
/// relies on `repr(Rust)`'s unspecified layout, so it is not a *sound*
/// program — but the claim being made here was about what the compiler
/// rejects, and the compiler accepts it. See the `## KNOWN GAPS` block on
/// [`crate::routes::tracks::admit_template`] for the registered gaps.
///
/// This is load-bearing, not tidiness. While the fields were `pub`, the
/// sentence "a `&'static Template` can only come from the roster" was false
/// even in safe Rust — `Box::leak(Box::new(Template { key:
/// String::leak(caller.to_owned()), title: t.title, .. }))` compiled and
/// produced one from the caller's own spelling. Two independent review
/// channels built exactly that value and the whole suite stayed green, because
/// `routes::tracks` used the false sentence to *excuse* the plugin-binding
/// consumer from any test. Privacy removes that particular expression from
/// other modules; it does **not** restore the excuse, because the binding
/// decision never needed a forged `Template` in the first place — see the
/// `## KNOWN GAPS` block cited above and
/// [`crate::routes::tracks::resolve_template_binding`].
///
/// The accessors hand back `&'static str`, not `&'a str` tied to `&self`: the
/// bytes live for the whole process, and downstream (`TemplateAdmission::key`,
/// `TrackInit::Template`, the `tracks.template_id` column) depends on carrying
/// the roster's own buffer rather than a copy of it. For the builtin roster
/// `body` is a slice of the `include_str!` source; `key` and `title` are the
/// front matter's decoded strings, leaked once per process when the roster is
/// built (#1635 §6 gap 8 — one roster per process, and tests share it).
pub struct Template {
    key: &'static str,
    title: &'static str,
    /// The report body exactly as the file has it after the closing `+++`
    /// line: `Template::recipe` hands it out uncompiled and unmodified.
    body: &'static str,
}

impl Template {
    /// The roster's own `&'static str`, never a value derived from a caller.
    pub fn key(&self) -> &'static str {
        self.key
    }

    /// The picker's display title, and the summary an instantiated report
    /// starts with.
    pub fn title(&self) -> &'static str {
        self.title
    }

    /// This entry's recipe: the summary and body a `template_id` create
    /// instantiates from — `summary` is the front matter `title`, `body` the
    /// file's bytes after the front matter.
    ///
    /// A fresh `TrackReportPayload` on every call (two `String`s copied out of
    /// the `'static` bytes), so nothing a caller does to the returned payload
    /// is visible to the next caller.
    ///
    /// This is the *un*compiled recipe. `POST /api/tracks` and
    /// `GET /api/track-templates` both reach it through
    /// `routes::tracks::compile_template`, which validates the body and
    /// projects it; neither reads this directly.
    pub fn recipe(&self) -> TrackReportPayload {
        TrackReportPayload::new(self.title, self.body)
    }
}

/// The roster: every template `POST /api/tracks` admits, in picker order.
///
/// Like [`Template`], constructible only inside this module's subtree: the
/// field is private and there is no public constructor, so a downstream module
/// cannot hand the routes a roster of its own. The one production value is
/// [`TemplateRoster::builtin`], carried on `RouteState.templates`.
pub struct TemplateRoster {
    entries: Vec<Template>,
}

/// Why a set of template sources did not become a roster.
#[derive(Debug)]
enum RosterError {
    /// Source number `index` (0-based, in [`BUILTIN_SOURCES`] order) did not
    /// parse as a template file.
    FrontMatter {
        index: usize,
        error: front_matter::FrontMatterError,
    },
    /// Two sources declare the same `id`: #1321's "one key → recipe table"
    /// would otherwise be ambiguous, and `get` would silently answer with
    /// whichever came first.
    DuplicateId(String),
}

impl std::fmt::Display for RosterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FrontMatter { index, error } => {
                write!(f, "template source #{index}: {error}")
            }
            Self::DuplicateId(id) => write!(f, "duplicate template id `{id}`"),
        }
    }
}

impl TemplateRoster {
    /// The kernel's built-in roster: [`BUILTIN_SOURCES`], parsed once.
    ///
    /// `&'static` because the entries are: `TrackInit::Template { key }` and
    /// `TemplateAdmission` carry borrows into it across a create transaction,
    /// and `tracks.template_id` stores the borrowed key's bytes.
    ///
    /// # Panics
    ///
    /// At first use, if any builtin file fails [`front_matter::parse`] or two
    /// files declare the same `id`. The files are compiled into the binary, so
    /// this cannot be reached by any request — it is a build defect, and the
    /// process must not come up advertising a partial roster.
    pub fn builtin() -> &'static TemplateRoster {
        static ROSTER: OnceLock<TemplateRoster> = OnceLock::new();
        ROSTER.get_or_init(|| Self::load(&BUILTIN_SOURCES))
    }

    /// [`Self::from_sources`], panicking on failure — the builtin path's
    /// policy, factored out so a test can exercise it on hand-built sources.
    fn load(sources: &[&'static str]) -> TemplateRoster {
        Self::from_sources(sources)
            .unwrap_or_else(|error| panic!("builtin template roster: {error}"))
    }

    /// Parse each source as a template file and reject duplicate ids.
    ///
    /// `key` and `title` are leaked once here; `body` borrows the source.
    fn from_sources(sources: &[&'static str]) -> Result<TemplateRoster, RosterError> {
        let mut entries: Vec<Template> = Vec::with_capacity(sources.len());
        for (index, source) in sources.iter().enumerate() {
            let (front, body) = front_matter::parse(source)
                .map_err(|error| RosterError::FrontMatter { index, error })?;
            if entries.iter().any(|entry| entry.key == front.id) {
                return Err(RosterError::DuplicateId(front.id));
            }
            entries.push(Template {
                key: String::leak(front.id),
                title: String::leak(front.title),
                body,
            });
        }
        Ok(TemplateRoster { entries })
    }

    /// Every entry, in picker order.
    pub fn entries(&self) -> &[Template] {
        &self.entries
    }

    /// #1209 — the roster's single fallible lookup: "is this id a template,
    /// and if so which one". `POST /api/tracks` admits an id iff this returns
    /// `Some`, and the area default-template check asks the same question.
    ///
    /// It searches [`Self::entries`] rather than a second array of keys, so
    /// "the list the picker shows" and "the set create accepts" cannot drift:
    /// there is nothing to keep in sync. The second roster that used to exist
    /// — a key-array constant plus the predicate that walked it — was exactly
    /// that duplication and is gone since #1209.
    ///
    /// The answer is a borrow **into** the roster — `&'a Template` for
    /// `&'a self`, so `&'static Template` off [`Self::builtin`] — never a
    /// value derived from the argument. Pinned by pointer identity in
    /// `get_returns_the_rosters_own_borrow` (#1318 S2).
    pub fn get(&self, key: &str) -> Option<&Template> {
        self.entries.iter().find(|template| template.key == key)
    }
}

/// Read the task blocks back out of a rendered template report body, **as the
/// payloads they are** — `#[cfg(test)]` since #1321 S3.
///
/// ## What reads this, and what does not
///
/// Nothing in this crate outside `#[cfg(test)]` calls it. Method:
/// `grep -rn template_task_payloads_from_body crates/`, which is the whole
/// carrier set for a Rust caller — every workspace member in the root
/// `Cargo.toml` lives under `crates/`. The hits are this function, this
/// module's own tests, its `repro_1239` module, and one `#[cfg(test)]`
/// assertion in `routes::tracks`
/// (`every_recipe_instantiates_and_declares_its_tasks`).
/// Its production caller was `template_task_payloads`, which
/// `GET /api/track-templates` used to re-parse a rendered recipe body with;
/// #1321 S3 pointed that endpoint at `routes::tracks::compile_template`
/// instead, so the picker now projects from the same validated blocks the
/// create path builds rather than from a second, lenient parse of the same
/// bytes.
///
/// It stays as the *independent* reader the tests below compare that
/// projection against: an assertion that walks the body with `split_body` /
/// `parse_fence` is checking the compiled result against something, rather than
/// against itself.
///
/// ## Why this returns `Value` and not `PlanTaskInput`
///
/// The first cut deserialized each payload into `PlanTaskInput`. That was a
/// silent data-loss bug, not a typing preference: `PlanTaskInput` is
/// `#[serde(deny_unknown_fields)]`, and `refs`, `released_by_user`,
/// `tombstone`, `tombstoned_by` and `spawn` are all first-class task-block
/// vocabulary (`report_blocks::kinds`) that it does not carry. A **well-formed**
/// task fence using any of them failed to deserialize and was dropped by the
/// "lenient" filter — and the surviving list then drove a whole-document
/// rewrite. Two consequences, both reproduced before this was changed:
///
/// * a task carrying `refs` vanished from `GET /api/track-templates` (the exact
///   drift #1230 exists to remove) and made the template permanently unsavable,
///   because the rewrite dropped a live task block and
///   `guard_task_declarations` refuses that;
/// * a **tombstone** was erased by a save that only changed the title, silently
///   reversing a #1179-governed deletion — the guard cannot catch it, its
///   removal check is gated on `!is_tombstone(old)`.
///
/// Keeping the payload whole removes the failure mode rather than patching it:
/// there is no "unknown field" to lose, and the round trip is an identity on
/// everything this module does not deliberately restamp. Nothing here needs the
/// typed struct — the picker reads `key` and `goal`, and everything else is
/// carried whole.
///
/// #1300 S1 deleted the Settings editor this paragraph used to name as the
/// consumer. The reason to keep the payload whole outlived it, and outlives
/// this function's demotion to tests: the same "keep the whole payload"
/// property is what the create path's own reader (`ReportDoc::blocks_snapshot`,
/// via `routes::tracks::prepare_initial_report_payload`) provides, and a field
/// dropped there is a field an instantiated track never receives.
///
/// Still lenient in the one way `split_body` is: a slice that is not a
/// well-formed `task` fence — prose, another kind, unparseable JSON — is
/// skipped. That is leniency about *shape*, which the parser has already
/// decided, not about vocabulary. That leniency is exactly why this is not the
/// production reader: on this path an unparseable fence silently becomes prose
/// and its task disappears, whereas `compile_template` refuses the recipe.
#[cfg(test)]
pub fn template_task_payloads_from_body(body: &str) -> Vec<Value> {
    split_body(body)
        .iter()
        .filter_map(|slice| parse_fence(&slice.raw))
        .filter(|fence| fence.kind == KIND_TASK)
        .map(|fence| fence.payload)
        .collect()
}

/// The picker projection of one task payload: `key` and its display
/// instruction (`goal` for agents, `command` for terminals), or `None` for a
/// payload that has neither (a tombstone).
///
/// Used by the read side to answer "what tasks does this template pre-set" for
/// the New track picker (`routes::track_templates::current_definition`).
/// Tombstones are *not* tasks the picker should advertise, but they must still
/// survive the read untouched — which is why the filtering happens here, at the
/// projection, and never in the reader that produced the payloads.
pub fn task_payload_key_and_instruction(payload: &Value) -> Option<(String, String)> {
    if payload
        .get("tombstone")
        .is_some_and(|value| !value.is_null())
    {
        return None;
    }
    let key = payload.get("key")?.as_str()?.to_string();
    let instruction_field = if payload.get("kind").and_then(Value::as_str) == Some("terminal") {
        "command"
    } else {
        "goal"
    };
    let instruction = payload.get(instruction_field)?.as_str()?.to_string();
    Some((key, instruction))
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::report_blocks::render_fence;
    use calm_types::report_contract::check_document;
    use calm_types::track_report::{research_header, work_brief_header};
    use std::collections::BTreeSet;

    /// The three plan templates: work-brief contract, intro, `task` fences.
    /// `investment-research` is the one report-only entry and is pinned
    /// separately.
    const PLAN_TEMPLATES: [&str; 3] = [ISSUE_DEVELOPMENT, SMALL_CHANGE, INVESTIGATION];

    fn roster() -> &'static TemplateRoster {
        TemplateRoster::builtin()
    }

    fn body(key: &str) -> String {
        roster()
            .get(key)
            .unwrap_or_else(|| panic!("`{key}` is not on the builtin roster"))
            .recipe()
            .body
    }

    /// The four id constants are the four files' ids, in roster order. The
    /// constants are protocol (other code names them); the files are the
    /// roster; this is the only place the two are held together.
    #[test]
    fn the_key_constants_are_exactly_the_builtin_file_ids() {
        let ids: Vec<&str> = roster().entries().iter().map(Template::key).collect();
        assert_eq!(
            ids,
            [
                ISSUE_DEVELOPMENT,
                SMALL_CHANGE,
                INVESTIGATION,
                INVESTMENT_RESEARCH
            ]
        );
    }

    /// Directory ↔ roster: the stems of `templates/builtin/*.md` are exactly
    /// the roster's ids, every file's front-matter `id` is its stem, and the
    /// bytes on disk are the bytes the roster serves.
    ///
    /// The last clause is the oracle statement of `lib.rs`'s `templates`
    /// paragraph made executable: a template's recipe *is* its file after the
    /// front matter (`summary` = `title`). A file added to the directory
    /// without a `BUILTIN_SOURCES` entry, a source listed under a name whose
    /// stem is not its id, and a stale `include_str!` (impossible with cargo's
    /// dependency tracking, but cheap to hold) all land here.
    #[test]
    fn builtin_directory_and_roster_are_the_same_set() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/builtin");
        let mut stems = BTreeSet::new();
        for entry in std::fs::read_dir(&dir).expect("templates/builtin exists") {
            let path = entry.expect("read_dir entry").path();
            assert_eq!(
                path.extension().and_then(|e| e.to_str()),
                Some("md"),
                "only template files live in templates/builtin: {}",
                path.display()
            );
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("utf-8 stem")
                .to_string();
            let text = std::fs::read_to_string(&path).expect("read template file");
            let (front, file_body) = front_matter::parse(&text)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            assert_eq!(
                front.id,
                stem,
                "{}: front-matter id is its stem",
                path.display()
            );
            let template = roster()
                .get(&stem)
                .unwrap_or_else(|| panic!("{}: on disk but not on the roster", path.display()));
            assert_eq!(template.title(), front.title, "{stem}: title");
            assert_eq!(
                template.recipe().body,
                file_body,
                "{stem}: the recipe body is the file's bytes after the front matter"
            );
            assert!(stems.insert(stem));
        }
        let roster_ids: BTreeSet<String> = roster()
            .entries()
            .iter()
            .map(|template| template.key().to_string())
            .collect();
        assert_eq!(
            stems, roster_ids,
            "templates/builtin/*.md stems == roster ids"
        );
    }

    /// Two sources with one id do not become a roster: the builtin path
    /// panics at first use. Exercised on hand-built sources — the real files
    /// are distinct by `builtin_directory_and_roster_are_the_same_set`.
    #[test]
    #[should_panic(expected = "duplicate template id `twin`")]
    fn duplicate_ids_panic_at_first_use() {
        TemplateRoster::load(&[
            "+++\nid = \"twin\"\ntitle = \"A\"\n+++\n# A\n",
            "+++\nid = \"twin\"\ntitle = \"B\"\n+++\n# B\n",
        ]);
    }

    /// A source that is not a template file panics the same way, naming its
    /// position in the source list.
    #[test]
    #[should_panic(expected = "template source #1: template file must open with a `+++`")]
    fn a_source_without_front_matter_panics_at_first_use() {
        TemplateRoster::load(&[
            "+++\nid = \"fine\"\ntitle = \"Fine\"\n+++\n# A\n",
            "# no front matter\n",
        ]);
    }

    /// Every roster entry answers `get` with itself and carries a non-empty
    /// recipe; an unknown id answers `None`.
    #[test]
    fn known_keys_round_trip() {
        for (index, template) in roster().entries().iter().enumerate() {
            let key = template.key();
            let found = roster().get(key).expect("roster key admits");
            assert!(std::ptr::eq(found, &roster().entries()[index]));
            let recipe = template.recipe();
            assert_eq!(
                recipe.summary,
                template.title(),
                "{key}: summary is the title"
            );
            assert!(!recipe.summary.is_empty(), "{key}: empty recipe summary");
            assert!(!recipe.body.is_empty(), "{key}: empty recipe body");
        }
        assert!(roster().get("missing-template").is_none());
        assert!(roster().get("").is_none());
    }

    /// #1318 S2 — `get` hands back a borrow **into** the roster, never a value
    /// derived from the caller's argument.
    ///
    /// This is the source side of "`tracks.template_id` stores the roster's
    /// key": `create_track` writes `admission.key` onto the row, and that key
    /// is only worth writing if it is the roster's own `&'static str` rather
    /// than a copy of whatever the client sent. Asserted by data-pointer
    /// identity, which is the one form the caller's string cannot satisfy —
    /// `owned` below is a freshly allocated `String` with identical bytes, so
    /// an equality assertion would pass for both and discriminate nothing.
    ///
    /// The mutation this catches is not hypothetical: any case-folding or
    /// aliasing rule that reflects the caller's spelling back — e.g.
    /// `entries.iter().find(..).map(|t| &*Box::leak(Box::new(Template { key:
    /// String::leak(key.to_string()), title: t.title, body: t.body })))` —
    /// still returns an equal key and turns this test red. (The leak is not
    /// incidental: the signature returns a borrow, so a mutation that rebuilds
    /// the entry has to leak it to compile at all.)
    ///
    /// Since #1318 S2 (第二轮评审) that mutation is, in safe Rust, only
    /// *writable inside this module's subtree*: [`Template`]'s fields are
    /// private, so the same expression outside it is `E0451`.
    ///
    /// #1318 S2 (第三轮评审) — what this test guards is therefore **one return
    /// path**, [`TemplateRoster::get`]'s, and not "the module". A review
    /// channel added a *second* roster entry point in this same module (a
    /// case-insensitive find that leaked a rebuilt entry when the spelling
    /// differed), pointed `admit_template` at it, and
    /// `nextest -E 'test(admission) or test(template)'` ran **68 passed, 0
    /// failed**. The test is still worth keeping — it is the cheap
    /// unconditional guard on the path production uses — but it is not a guard
    /// on the class. See the `## KNOWN GAPS` block on
    /// [`crate::routes::tracks::admit_template`].
    #[test]
    fn get_returns_the_rosters_own_borrow() {
        for (index, template) in roster().entries().iter().enumerate() {
            let owned = String::from(template.key);
            assert_ne!(
                owned.as_ptr(),
                template.key.as_ptr(),
                "the fixture must not accidentally be the roster's own buffer"
            );
            let found = roster().get(owned.as_str()).expect("roster key admits");
            assert!(
                std::ptr::eq(found, &roster().entries()[index]),
                "get must borrow the roster entry, not rebuild one"
            );
            assert!(
                std::ptr::eq(found.key.as_ptr(), template.key.as_ptr()),
                "the admitted key must be the roster's &'static str, not the caller's"
            );
        }
    }

    /// #1318 S2 (第三轮评审) — [`Template::key`] / [`Template::title`] hand back
    /// **the entry's own buffer**, not merely a pointer-stable one, and
    /// [`Template::recipe`] copies the entry's own `body`.
    ///
    /// This closes a regression the 第二轮 refactor introduced. That round
    /// rewrote `routes::tracks`'s admission assertion so that *both* sides read
    /// through the accessor (`ptr::eq(admission.key().as_ptr(),
    /// template.key().as_ptr())`), which downgraded it from "the accessor is
    /// the roster buffer" to "the accessor is pointer-stable". A review channel
    /// made `key()` return an interned leak — same pointer on every call for a
    /// given entry, but *not* the field's bytes — and the whole selection ran
    /// **68 passed, 0 failed**, i.e. `tracks.template_id` and
    /// `TrackInit::Template` were no longer carrying the roster's bytes and
    /// nothing in the repository noticed.
    ///
    /// It has to live here, in the defining module, because the discriminating
    /// comparison is accessor-against-**private-field**: `t.key` is not
    /// nameable from `routes::tracks`, so no test over there can express it.
    #[test]
    fn the_accessors_hand_back_the_roster_fields_own_buffer() {
        for template in roster().entries() {
            assert!(
                std::ptr::eq(template.key().as_ptr(), template.key.as_ptr()),
                "`{}`: key() must be the `key` field's own buffer",
                template.key
            );
            assert!(
                std::ptr::eq(template.title().as_ptr(), template.title.as_ptr()),
                "`{}`: title() must be the `title` field's own buffer",
                template.key
            );
            let recipe = template.recipe();
            assert_eq!(
                recipe.summary, template.title,
                "`{}`: summary",
                template.key
            );
            assert_eq!(recipe.body, template.body, "`{}`: body", template.key);
        }
    }

    /// #1185 §1.5 B, restated structurally for files (#1635 S4): the three
    /// plan templates carry the work-brief maintenance contract — the same
    /// one `default.md` ships — as their block 0.
    ///
    /// Three clauses, each catching one way the wording could drift now that
    /// the contract is data in five files:
    ///
    ///   * `check_document(body) == Ok(Some(work_brief_header()))` — one
    ///     canonical header on line 1, every block-0 comment closed, the
    ///     declared sections are the default four (the funnel check the
    ///     persisted body must pass);
    ///   * block 0 is **one identical text** across the three — a plan note
    ///     deleted from one file, or a sentence reworded in one file only,
    ///     lands here;
    ///   * `default.md`'s block 0 minus its closing `-->` line is a **prefix**
    ///     of that block 0 — the shared contract wording (writing rules +
    ///     section list) cannot drift between the default skeleton and the
    ///     templates without this going red. What the templates add after
    ///     that prefix is the plan note; what they add after block 0 is the
    ///     intro and the fences.
    ///
    /// A wording change made consistently in all four files passes, by design:
    /// the file is the data, and that edit is reviewed as a diff of the file.
    #[test]
    fn every_plan_template_carries_the_one_maintenance_contract() {
        let mut block_0s: Vec<String> = Vec::new();
        for key in PLAN_TEMPLATES {
            let report = roster().get(key).expect("plan template").recipe();
            assert_eq!(
                check_document(&report.body),
                Ok(Some(work_brief_header())),
                "{key}: the body must pass the contract funnel check"
            );
            let slices = split_body(&report.body);
            assert!(
                slices[0].raw.ends_with("-->\n\n"),
                "{key}: the contract must be its own closed block, got {:?}",
                slices[0].raw
            );
            assert!(
                report.report_startup_read_required(),
                "{key} is not the default skeleton"
            );
            block_0s.push(slices[0].raw.clone());
        }
        for (key, block_0) in PLAN_TEMPLATES.iter().zip(&block_0s) {
            assert_eq!(
                block_0, &block_0s[0],
                "{key}: the three plan templates must share one block 0 (contract + plan note)"
            );
        }

        let default_body = TrackReportPayload::initial().body;
        let default_block_0 = split_body(&default_body)[0].raw.clone();
        let shared = default_block_0
            .strip_suffix("-->\n\n")
            .expect("default.md's block 0 is a closed comment");
        assert!(
            block_0s[0].starts_with(shared),
            "default.md's contract (minus its closing line) must be a prefix of the plan \
             templates' block 0; the two texts drifted:\n--- default ---\n{shared}\n--- template \
             ---\n{}",
            block_0s[0]
        );
        assert!(
            block_0s[0].len() > shared.len() + "-->\n\n".len(),
            "the plan templates add a plan note after the shared contract"
        );
    }

    /// #1635 S2b review — the "header ↔ document shape" pin for the research
    /// skeleton: the H1 lines `investment-research.md` ships, in order, are
    /// exactly `research_header()`'s sections. Rename one side and this goes
    /// red; nothing else ties the file to the header. (The work-brief twin
    /// lives in calm-types: `default_h1s_are_the_work_brief_header_sections`.)
    #[test]
    fn investment_research_h1s_are_the_research_header_sections() {
        let body = body(INVESTMENT_RESEARCH);
        let slices = split_body(&body);
        let h1s: Vec<String> = slices[1..]
            .iter()
            .map(|slice| {
                slice
                    .raw
                    .lines()
                    .next()
                    .and_then(|line| line.strip_prefix("# "))
                    .unwrap_or_else(|| panic!("block does not start with an H1: {:?}", slice.raw))
                    .to_string()
            })
            .collect();
        let declared: Vec<String> = research_header()
            .sections
            .into_iter()
            .map(|section| section.h1)
            .collect();
        assert_eq!(h1s, declared);
        assert_eq!(
            check_document(&body).unwrap().unwrap().sections,
            research_header().sections,
            "and the header the body carries is the one the constructor builds"
        );
    }

    /// #1571 — the `investment-research` recipe: research contract first, the
    /// seven H1s in contract order and nothing else, zero `task` blocks.
    #[test]
    fn investment_research_is_a_task_less_research_skeleton() {
        let report = roster()
            .get(INVESTMENT_RESEARCH)
            .expect("research template")
            .recipe();
        assert_eq!(report.summary, "Investment research");
        assert_eq!(
            check_document(&report.body),
            Ok(Some(research_header())),
            "must lead with the closed research contract, not the work-brief one"
        );
        assert!(report.report_startup_read_required());
        // #1635 v5 / §6.4 — it reads as written because of its birth summary,
        // not its body: the same body with an empty summary is structurally
        // unwritten under D3 (block 0 is the contract, then seven bare declared
        // H1s). If the empty research skeleton should ever read as unwritten,
        // that needs a birth-summary baseline — another design, not a tweak
        // to the predicate.
        assert!(
            !TrackReportPayload::new("", report.body.clone()).report_startup_read_required(),
            "the empty research skeleton is unwritten by shape; only the summary makes it written"
        );

        let slices = split_body(&report.body);
        assert!(
            slices[0].raw.ends_with("-->\n\n"),
            "the research contract must be its own closed block, got {:?}",
            slices[0].raw
        );
        let heads: Vec<&str> = slices[1..]
            .iter()
            .map(|slice| slice.raw.lines().next().unwrap_or(""))
            .collect();
        assert_eq!(
            heads,
            [
                "# 结论",
                "# 待你定",
                "# 核心逻辑",
                "# 关键数据",
                "# 风险与证伪",
                "# 催化剂与跟踪",
                "# 来源与边界",
            ],
            "exactly the seven research H1s, in order, after the contract block"
        );
        for slice in &slices[1..] {
            assert!(
                slice.raw.lines().skip(1).all(|line| line.trim().is_empty()),
                "sections start empty: {:?}",
                slice.raw
            );
        }
        assert!(
            slices.iter().all(|s| parse_fence(&s.raw).is_none()),
            "no fences of any kind in the skeleton"
        );
        assert!(template_task_payloads_from_body(&report.body).is_empty());
        assert!(!report.body.contains("# Plan"));
    }

    /// #1230 — reading a task block out of a body and rendering it back must be
    /// an **identity on the payload**, not merely agree on the fields some
    /// struct happens to model. The first cut deserialized into
    /// `PlanTaskInput` (`deny_unknown_fields`) and silently dropped any block
    /// carrying `refs` / `released_by_user` / `tombstone`; asserting identity is
    /// what makes that class impossible rather than fixed for the fields we
    /// happened to think of.
    ///
    /// Also the roster's honesty about which entries are plans: the three plan
    /// templates parse to at least one task fence each, `investment-research`
    /// to none.
    ///
    /// The whole-document version of this property — that a *save* preserves
    /// every block and its id — is an integration test
    /// (`a_save_preserves_blocks_it_does_not_edit`), because it is about the
    /// report's blocks and not about this module's files.
    #[test]
    fn parsing_a_task_fence_and_rendering_it_back_is_an_identity() {
        for template in roster().entries() {
            let key = template.key();
            let body = template.recipe().body;
            let payloads = template_task_payloads_from_body(&body);
            assert_eq!(
                payloads.is_empty(),
                key == INVESTMENT_RESEARCH,
                "{key}: task payloads parsed iff the recipe is a plan template"
            );
            for payload in &payloads {
                let fence = render_fence(KIND_TASK, payload);
                assert!(
                    body.contains(&fence),
                    "{key}: re-rendering a parsed payload did not reproduce its fence:\n{fence}"
                );
            }
        }
    }

    /// Prose the user added through the ordinary track report editor is not a
    /// task and must not be read as one — the lenient-read claim in the
    /// function's doc, exercised rather than asserted.
    #[test]
    fn body_prose_and_foreign_fences_are_skipped_not_parsed() {
        let mut body = body(SMALL_CHANGE);
        let before = template_task_payloads_from_body(&body).len();
        body.push_str("\n## Notes\n\nSomething the user typed.\n\n");
        body.push_str("```neige-block table\n{\n  \"rows\": []\n}\n```\n");
        body.push_str("```neige-block task\nnot json\n```\n");
        assert_eq!(template_task_payloads_from_body(&body).len(), before);
    }
}

#[cfg(test)]
mod repro_1239 {
    use super::*;
    use calm_types::report_blocks::render_fence;
    use serde_json::json;

    fn builtin_body(key: &str) -> String {
        TemplateRoster::builtin()
            .get(key)
            .expect("builtin key")
            .recipe()
            .body
    }

    /// Channel-B finding, reproduced before any fix.
    ///
    /// `PlanTaskInput` is `#[serde(deny_unknown_fields)]`, and `refs` /
    /// `released_by_user` / `tombstone` / `tombstoned_by` are all first-class
    /// task-block vocabulary it does not carry. A *well-formed* task fence
    /// using any of them therefore fails to deserialize and is dropped by the
    /// lenient filter — which is not leniency, it is silent data loss feeding a
    /// whole-document rewrite.
    #[test]
    fn a_wellformed_task_fence_with_task_block_vocabulary_is_silently_dropped() {
        let mut body = builtin_body(SMALL_CHANGE);
        body.push_str(&render_fence(
            KIND_TASK,
            &json!({
                "key": "with-refs",
                "kind": "codex",
                "goal": "A task that references a document.",
                "refs": ["neige://wave/w1#b_0001"],
                "ready": false,
                "declared_by": "user",
            }),
        ));
        let parsed = template_task_payloads_from_body(&body);
        let keys: Vec<&str> = parsed
            .iter()
            .filter_map(|p| p.get("key").and_then(Value::as_str))
            .collect();
        assert!(
            keys.contains(&"with-refs"),
            "a well-formed task fence carrying `refs` must survive the read; got {keys:?}"
        );
    }

    /// The same drop applied to a tombstone silently reverses a #1179-governed
    /// deletion: the guard's removal check is gated on `!is_tombstone(old)`, so
    /// nothing stops the rewrite from erasing it.
    #[test]
    fn a_task_tombstone_is_not_erased_by_the_read() {
        let mut body = builtin_body(INVESTIGATION);
        body.push_str(&render_fence(
            KIND_TASK,
            &json!({
                "key": "retired",
                "tombstone": { "reason": null },
                "declared_by": "user",
                "tombstoned_by": "user",
            }),
        ));
        let parsed = template_task_payloads_from_body(&body);
        let keys: Vec<&str> = parsed
            .iter()
            .filter_map(|p| p.get("key").and_then(Value::as_str))
            .collect();
        assert!(
            keys.contains(&"retired"),
            "a tombstone must survive the read; got {keys:?}"
        );
    }
}
