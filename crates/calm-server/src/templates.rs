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
//! and the area default-template check all read one value. That value is
//! [`TemplateRoster::for_boot`]'s: the builtin entries, followed — when the
//! process was started with `--templates-dir` (#1635 S5) — by one entry per
//! `*.md` file in that directory, keyed `site/<stem>`. The readers do not
//! know which kind an entry is; the prefix is the only difference.
//!
//! #1635 S5 — the operator directory is read **once, at boot, fail-closed**:
//! a file that does not open, does not parse, declares an `id` other than its
//! stem, collides with an existing key, or whose body
//! `routes::tracks::compile_template` or the contract-header funnel
//! (`check_document`) refuses, stops the boot with an error naming the file. There is no "skip the bad file" arm, on purpose — a
//! shorter picker is a silent failure, and the directory is deployer-trusted
//! (§6 gap 2), so refusing is the operator's own feedback loop. A builtin id
//! cannot be overridden from the directory: every site key carries the
//! `site/` prefix and no file id may contain `/`, so the collision check is
//! asserted rather than relied on.
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
use std::path::{Path, PathBuf};
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

/// #1635 S5 — the key prefix of an operator-provided template: a file
/// `<dir>/<stem>.md` under `--templates-dir` is exposed as `site/<stem>`.
///
/// Composed here, by the loader, and never spelled in a file: a front matter
/// `id` may not contain `/` ([`front_matter`]), and neither may a plugin
/// manifest's `templates[].id` (`plugin_host::manifest::TemplateDescriptor`,
/// `^[a-z0-9][a-z0-9._-]{0,63}$`). So no builtin file and no plugin can claim
/// a `site/…` key, and no site file can claim a builtin one.
pub const SITE_PREFIX: &str = "site/";

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
/// the roster's own buffer rather than a copy of it. For the builtin entries
/// `body` is a slice of the `include_str!` source; `key` and `title` are the
/// front matter's decoded strings, leaked once per process when the roster is
/// built (#1635 §6 gap 8 — one roster per process, and tests share it). For a
/// `site/` entry (#1635 S5) the file's text is read and leaked once at boot,
/// `body` is a slice of it, and `key` is the leaked `site/<stem>` string.
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

/// The roster: every template `POST /api/tracks` admits, in picker order —
/// the builtin entries first, then the `site/` entries in file-name order.
///
/// Like [`Template`], constructible only inside this module's subtree: the
/// field is private and there is no public constructor, so a downstream module
/// cannot hand the routes a roster of its own. The one production value is
/// [`TemplateRoster::for_boot`]'s, carried on `RouteState.templates`; it *is*
/// [`TemplateRoster::builtin`] when no `--templates-dir` was given.
pub struct TemplateRoster {
    entries: Vec<Template>,
}

/// Why a set of template sources did not become a roster.
///
/// `pub(crate)` since #1635 S5: [`TemplateRoster::for_boot`] hands it to
/// `AppState::boot`, which turns it into the boot error `main` exits on.
#[derive(Debug)]
pub(crate) enum RosterError {
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
    /// #1635 S5 — `--templates-dir` could not be listed (missing, not a
    /// directory, unreadable).
    SiteDir { dir: PathBuf, error: std::io::Error },
    /// #1635 S5 — one file under `--templates-dir` did not become an entry.
    /// Always names the file: the operator's fix is an edit to it.
    SiteFile {
        path: PathBuf,
        reason: SiteFileError,
    },
}

/// #1635 S5 — the ways one `<dir>/<stem>.md` fails to load.
#[derive(Debug)]
pub(crate) enum SiteFileError {
    /// The file name's stem is not UTF-8, so no `site/<stem>` key can be made.
    StemNotUtf8,
    /// `read_to_string` failed: permissions, not UTF-8, a directory named
    /// `*.md`.
    Read(std::io::Error),
    /// The text is not a template file ([`front_matter::parse`]).
    FrontMatter(front_matter::FrontMatterError),
    /// The front matter's `id` is not the file stem — the one fact that ties
    /// "the key the picker shows" to "the file the operator edits".
    IdIsNotStem { id: String, stem: String },
    /// `site/<stem>` is already on the roster. Unreachable against a builtin
    /// key (the prefix) and against another file in one directory (stems are
    /// unique there); reachable through [`TemplateRoster::extend_with_site_files`]
    /// with two directories, and asserted regardless.
    Duplicate(String),
    /// The body does not compile (`routes::tracks::compile_template`): a
    /// malformed or schema-invalid `neige-block` fence, a `+++` body, a
    /// document the block layout refuses.
    Body(String),
    /// The body fails the contract-header funnel
    /// (`calm_types::report_contract::check_document`): a header that is not
    /// on line 1, not canonical, duplicated, or a block 0 that ends inside an
    /// HTML comment. The create path runs this check at persist time, so
    /// without it here a file the boot accepted would fail every create.
    ContractHeader(calm_types::report_contract::HeaderError),
}

impl std::fmt::Display for RosterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FrontMatter { index, error } => {
                write!(f, "template source #{index}: {error}")
            }
            Self::DuplicateId(id) => write!(f, "duplicate template id `{id}`"),
            Self::SiteDir { dir, error } => {
                write!(f, "templates dir {}: {error}", dir.display())
            }
            Self::SiteFile { path, reason } => {
                write!(f, "template file {}: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for RosterError {}

impl std::fmt::Display for SiteFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StemNotUtf8 => write!(f, "file stem is not UTF-8"),
            Self::Read(error) => write!(f, "{error}"),
            Self::FrontMatter(error) => write!(f, "{error}"),
            Self::IdIsNotStem { id, stem } => write!(
                f,
                "front matter id `{id}` must equal the file stem `{stem}` (the template is \
                 exposed as `{SITE_PREFIX}{stem}`)"
            ),
            Self::Duplicate(key) => write!(f, "template id `{key}` is already on the roster"),
            Self::Body(error) => write!(f, "body does not compile: {error}"),
            Self::ContractHeader(error) => write!(f, "report contract header: {error}"),
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

    /// #1635 S5 — the roster this boot serves: [`Self::builtin`] when no
    /// `--templates-dir` was given, otherwise the builtin entries followed by
    /// one `site/<stem>` entry per `*.md` file in `site_dir`, leaked once.
    ///
    /// The one production caller is `AppState::boot`, which runs this before
    /// it opens storage; `main` exits non-zero on `Err`. The `fixtures`-gated
    /// `AppState::with_templates_dir` is the test road onto the same function.
    /// `pub(crate)`, not `pub`: a downstream crate cannot construct or feed
    /// the roster except through the boot loader, which validates every file
    /// (`tests/ui/template_roster_constructors_are_private.rs`).
    ///
    /// Fail-closed, and every failure names its file: see [`RosterError`] and
    /// the module doc. There is deliberately no arm that skips a file. A
    /// successful load logs the site-entry count at `info` — zero is a
    /// legitimate state (a directory with no `*.md`), and the count is what
    /// makes it visible.
    pub(crate) fn for_boot(
        site_dir: Option<&Path>,
    ) -> Result<&'static TemplateRoster, RosterError> {
        let Some(dir) = site_dir else {
            return Ok(Self::builtin());
        };
        let builtin = Self::builtin();
        let mut roster = builtin.copy_of_entries();
        roster.extend_with_site_dir(dir)?;
        tracing::info!(
            site_templates = roster.entries.len() - builtin.entries.len(),
            dir = %dir.display(),
            "operator templates loaded"
        );
        Ok(Box::leak(Box::new(roster)))
    }

    /// A roster holding the same entries as `self` — the same `&'static str`
    /// buffers, nothing re-leaked. The builtin half of a merged roster is
    /// therefore pointer-identical to [`Self::builtin`]'s entries.
    ///
    /// Private, and not a `Clone` impl: the #1318 privacy story is that a
    /// `Template` cannot be *named* outside this subtree, and this is inside
    /// it. Nothing outside can reach a copy either — the only caller is
    /// [`Self::for_boot`], which leaks the result exactly once.
    fn copy_of_entries(&self) -> TemplateRoster {
        TemplateRoster {
            entries: self
                .entries
                .iter()
                .map(|template| Template {
                    key: template.key,
                    title: template.title,
                    body: template.body,
                })
                .collect(),
        }
    }

    /// Append every `*.md` file directly under `dir` (non-recursive; sorted by
    /// file name, so the picker order is deterministic across hosts) as a
    /// `site/<stem>` entry.
    fn extend_with_site_dir(&mut self, dir: &Path) -> Result<(), RosterError> {
        let mut paths: Vec<PathBuf> = Vec::new();
        let listing = std::fs::read_dir(dir).map_err(|error| RosterError::SiteDir {
            dir: dir.to_path_buf(),
            error,
        })?;
        for entry in listing {
            let path = entry
                .map_err(|error| RosterError::SiteDir {
                    dir: dir.to_path_buf(),
                    error,
                })?
                .path();
            // `read_dir` yields directories and non-template files too; only
            // `*.md` are templates. A directory named `x.md` is not skipped —
            // it reaches `read_to_string` and fails there, naming itself.
            if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
                paths.push(path);
            }
        }
        paths.sort();
        self.extend_with_site_files(&paths)
    }

    /// Append one `site/<stem>` entry per path, in the order given. Split from
    /// [`Self::extend_with_site_dir`] so the duplicate-key arm is reachable
    /// from a test (two directories, one stem); production always passes one
    /// directory's sorted listing.
    ///
    /// Every failure returns — the `?`s here are the fail-closed contract
    /// (§5: replacing one with `continue` is the named mutation).
    fn extend_with_site_files(&mut self, paths: &[PathBuf]) -> Result<(), RosterError> {
        for path in paths {
            let template = Self::load_site_file(path)?;
            if self.entries.iter().any(|entry| entry.key == template.key) {
                return Err(RosterError::SiteFile {
                    path: path.clone(),
                    reason: SiteFileError::Duplicate(template.key.to_string()),
                });
            }
            self.entries.push(template);
        }
        Ok(())
    }

    /// Read, parse, check and compile one operator file into an entry.
    ///
    /// The file's text is leaked (the body borrows it); `key` and `title` are
    /// leaked too. All three leaks happen once per file per boot — and on the
    /// error paths the process is about to exit, so nothing is retained.
    ///
    /// The body gets the two checks the create path runs on a roster body:
    /// `routes::tracks::compile_template` (the call `POST /api/tracks` and
    /// `GET /api/track-templates` make on every entry at request time) and
    /// `check_document` (the contract-header funnel
    /// `track_report::write::write_report_row_and_project_tx` runs at persist
    /// time; for the builtin files it is pinned by
    /// `tests::every_plan_template_carries_the_one_maintenance_contract`).
    /// So a `site/` entry that reaches the roster does not fail either check
    /// at request time. What is *not* run here is the task projection the
    /// create transaction performs after the persist; nothing about a
    /// template body is known to fail there that these two accept.
    fn load_site_file(path: &Path) -> Result<Template, RosterError> {
        let fail = |reason: SiteFileError| RosterError::SiteFile {
            path: path.to_path_buf(),
            reason,
        };
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| fail(SiteFileError::StemNotUtf8))?;
        let text: &'static str = String::leak(
            std::fs::read_to_string(path).map_err(|error| fail(SiteFileError::Read(error)))?,
        );
        let (front, body) =
            front_matter::parse(text).map_err(|error| fail(SiteFileError::FrontMatter(error)))?;
        if front.id != stem {
            return Err(fail(SiteFileError::IdIsNotStem {
                id: front.id,
                stem: stem.to_string(),
            }));
        }
        let template = Template {
            key: String::leak(format!("{SITE_PREFIX}{stem}")),
            title: String::leak(front.title),
            body,
        };
        let compiled = crate::routes::tracks::compile_template(&template)
            .map_err(|error| fail(SiteFileError::Body(error.to_string())))?;
        calm_types::report_contract::check_document(compiled.body())
            .map_err(|error| fail(SiteFileError::ContractHeader(error)))?;
        Ok(template)
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
mod site_dir_tests {
    //! #1635 S5 — the `--templates-dir` loader, on hand-built directories.
    //!
    //! Every refusal case asserts two things: `Err`, and that the error names
    //! the offending file (or directory) — an operator reads this message
    //! once, at boot, and the file path is the whole of the fix instruction.
    //! The three named mutations (§5): a `?` in `extend_with_site_files`
    //! replaced by `continue` reddens the refusal cases; the `id == stem`
    //! check dropped reddens `an_id_that_is_not_the_stem_is_refused`; the
    //! `site/` prefix dropped reddens `a_site_file_named_like_a_builtin_…`.

    use super::*;
    use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
    use calm_types::report_blocks::{KIND_TASK, render_fence};
    use calm_types::report_contract::canonical_line;
    use calm_types::track_report::work_brief_header;
    use clap::Parser;
    use serde_json::json;
    use tempfile::TempDir;

    /// A body every check accepts: canonical work-brief header on line 1, a
    /// closed contract comment, prose, one canonical `task` fence.
    fn valid_body() -> String {
        let mut body = canonical_line(&work_brief_header());
        body.push_str("\n<!-- site template: closed contract comment -->\n\n# Plan\n\n");
        body.push_str("Operator prose.\n\n");
        body.push_str(&render_fence(
            KIND_TASK,
            &json!({
                "key": "site-task",
                "kind": "codex",
                "goal": "Do the operator's thing.",
                "acceptance": "It is done.",
                "declared_by": PLANNER_DECLARATION_AUTHOR,
                "depends_on": [],
                "no_gate_reason": "site fixture",
                "ready": false
            }),
        ));
        body
    }

    fn file(id: &str, title: &str, body: &str) -> String {
        format!("+++\nid = \"{id}\"\ntitle = \"{title}\"\n+++\n{body}")
    }

    /// Write `(file name, contents)` pairs into a fresh directory.
    fn site_dir(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        for (name, contents) in files {
            std::fs::write(dir.path().join(name), contents).expect("write template file");
        }
        dir
    }

    /// The production entry point on one directory: `for_boot(Some(dir))`,
    /// exactly what `AppState::boot` calls. Leaks one roster per call — fine
    /// for a test, and the reason production calls it once.
    fn load(dir: &Path) -> Result<&'static TemplateRoster, RosterError> {
        TemplateRoster::for_boot(Some(dir))
    }

    /// `Err`, and its message names `path`.
    #[track_caller]
    fn assert_refused_naming<T>(result: Result<T, RosterError>, path: &Path) -> String {
        let error = match result {
            Ok(_) => panic!("expected a refusal naming {}, got Ok", path.display()),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains(&path.display().to_string()),
            "the error must name {}; got: {message}",
            path.display()
        );
        message
    }

    #[test]
    fn site_files_become_site_entries_after_the_builtins_in_file_name_order() {
        let body = valid_body();
        let dir = site_dir(&[
            ("b.md", &file("b", "Site B", &body)),
            ("a.md", &file("a", "Site A", &body)),
            ("notes.txt", "not a template; not `.md`, so not read"),
        ]);
        let roster = load(dir.path()).expect("valid site dir");
        let builtin = TemplateRoster::builtin();
        let keys: Vec<&str> = roster.entries().iter().map(Template::key).collect();
        let mut expected: Vec<&str> = builtin.entries().iter().map(Template::key).collect();
        expected.extend(["site/a", "site/b"]);
        assert_eq!(
            keys, expected,
            "builtin first, then site files sorted by name"
        );

        for (key, title) in [("site/a", "Site A"), ("site/b", "Site B")] {
            let template = roster.get(key).expect("site entry admits");
            assert_eq!(template.title(), title);
            let recipe = template.recipe();
            assert_eq!(recipe.summary, title);
            assert_eq!(
                recipe.body, body,
                "{key}: the recipe body is the file after the front matter"
            );
            assert!(
                std::ptr::eq(template.key().as_ptr(), template.key.as_ptr()),
                "{key}: key() is the entry's own buffer"
            );
        }
        // The unprefixed spelling is not a key: a request for `a` is a 400.
        assert!(roster.get("a").is_none());
        assert!(roster.get("b").is_none());

        // The builtin half is the builtin roster's own entries — same buffers,
        // nothing re-leaked (§6 gap 8).
        for (merged, original) in roster.entries().iter().zip(builtin.entries()) {
            assert!(std::ptr::eq(merged.key.as_ptr(), original.key.as_ptr()));
            assert!(std::ptr::eq(merged.title.as_ptr(), original.title.as_ptr()));
            assert!(std::ptr::eq(merged.body.as_ptr(), original.body.as_ptr()));
        }
    }

    #[test]
    fn for_boot_without_a_dir_is_the_builtin_roster_itself() {
        let roster = TemplateRoster::for_boot(None).expect("no dir is fine");
        assert!(std::ptr::eq(roster, TemplateRoster::builtin()));
    }

    #[test]
    fn for_boot_with_a_dir_serves_builtin_and_site_entries() {
        let body = valid_body();
        let dir = site_dir(&[("x.md", &file("x", "Site X", &body))]);
        let roster = TemplateRoster::for_boot(Some(dir.path())).expect("valid site dir");
        assert!(!std::ptr::eq(roster, TemplateRoster::builtin()));
        assert_eq!(
            roster.entries().len(),
            TemplateRoster::builtin().entries().len() + 1
        );
        assert!(roster.get(ISSUE_DEVELOPMENT).is_some());
        assert_eq!(roster.get("site/x").expect("site/x").title(), "Site X");
    }

    /// `Config { templates_dir: Some(bad) }` → the boot's roster construction
    /// is `Err`; `AppState::boot` propagates it and `main` exits non-zero.
    /// (`main.rs`'s `a_bad_templates_dir_fails_the_boot_before_storage_exists`
    /// drives `AppState::boot` itself and asserts no database was created.)
    #[test]
    fn a_config_with_a_bad_templates_dir_fails_for_boot() {
        let dir = site_dir(&[("x.md", "# no front matter\n")]);
        let cfg = crate::config::Config::parse_from([
            "calm-server",
            "--templates-dir",
            dir.path().to_str().expect("utf-8 tempdir"),
        ]);
        assert_eq!(cfg.templates_dir.as_deref(), Some(dir.path()));
        let error = TemplateRoster::for_boot(cfg.templates_dir.as_deref())
            .err()
            .expect("a bad templates dir must fail the boot");
        assert!(
            error.to_string().contains("x.md"),
            "the boot error must name the file: {error}"
        );
    }

    #[test]
    fn a_dir_with_no_md_files_is_ok_and_adds_nothing() {
        let dir = site_dir(&[("README.txt", "nothing here")]);
        let roster = load(dir.path()).expect("an empty site set is not an error");
        assert_eq!(
            roster.entries().len(),
            TemplateRoster::builtin().entries().len()
        );
    }

    #[test]
    fn a_missing_or_unlistable_dir_is_refused_naming_it() {
        let parent = TempDir::new().expect("tempdir");
        let missing = parent.path().join("does-not-exist");
        let message = assert_refused_naming(load(&missing), &missing);
        assert!(message.starts_with("templates dir "), "{message}");

        // A file where a directory was expected.
        let not_a_dir = parent.path().join("file-not-dir");
        std::fs::write(&not_a_dir, "x").unwrap();
        assert_refused_naming(load(&not_a_dir), &not_a_dir);
    }

    #[test]
    fn a_file_without_front_matter_is_refused_naming_it() {
        let dir = site_dir(&[("x.md", "# no front matter at all\n")]);
        let path = dir.path().join("x.md");
        let message = assert_refused_naming(load(dir.path()), &path);
        assert!(
            message.contains("must open with a `+++`"),
            "the front matter parser's own reason: {message}"
        );
    }

    #[test]
    fn a_file_with_unparsable_front_matter_is_refused_naming_it() {
        let dir = site_dir(&[
            ("ok.md", &file("ok", "Fine", &valid_body())),
            ("x.md", "+++\nid = x\ntitle = \"X\"\n+++\n# body\n"),
        ]);
        let path = dir.path().join("x.md");
        let message = assert_refused_naming(load(dir.path()), &path);
        assert!(message.contains("front matter"), "{message}");
    }

    #[test]
    fn an_id_that_is_not_the_stem_is_refused_naming_it() {
        let dir = site_dir(&[("x.md", &file("y", "Y", &valid_body()))]);
        let path = dir.path().join("x.md");
        let message = assert_refused_naming(load(dir.path()), &path);
        assert!(
            message.contains("id `y` must equal the file stem `x`"),
            "{message}"
        );
        assert!(
            message.contains("site/x"),
            "names the key it would have had: {message}"
        );
    }

    /// Two files whose front matter says the same id: at most one of them has
    /// that id as its stem, so the other is refused by the stem rule before
    /// any duplicate could exist.
    #[test]
    fn two_files_declaring_one_id_are_refused_by_the_stem_rule() {
        let body = valid_body();
        let dir = site_dir(&[
            ("x.md", &file("x", "X", &body)),
            ("y.md", &file("x", "X again", &body)),
        ]);
        let path = dir.path().join("y.md");
        let message = assert_refused_naming(load(dir.path()), &path);
        assert!(
            message.contains("must equal the file stem `y`"),
            "{message}"
        );
    }

    /// The duplicate arm itself, reached the one way it can be: the same
    /// stem in two directories, fed through `extend_with_site_files`.
    #[test]
    fn one_stem_from_two_directories_is_a_duplicate_naming_the_second() {
        let body = valid_body();
        let first = site_dir(&[("x.md", &file("x", "First", &body))]);
        let second = site_dir(&[("x.md", &file("x", "Second", &body))]);
        let paths = [first.path().join("x.md"), second.path().join("x.md")];
        let mut roster = TemplateRoster::builtin().copy_of_entries();
        let error = roster
            .extend_with_site_files(&paths)
            .expect_err("one key twice");
        let message = error.to_string();
        assert!(
            message.contains(&paths[1].display().to_string()),
            "{message}"
        );
        assert!(
            message.contains("template id `site/x` is already on the roster"),
            "{message}"
        );
        assert!(
            matches!(
                error,
                RosterError::SiteFile {
                    reason: SiteFileError::Duplicate(_),
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn an_unreadable_file_is_refused_naming_it() {
        // A directory named `x.md`: `read_to_string` fails on it, and the
        // extension filter deliberately does not skip it.
        let dir = site_dir(&[]);
        let as_dir = dir.path().join("x.md");
        std::fs::create_dir(&as_dir).unwrap();
        assert_refused_naming(load(dir.path()), &as_dir);

        // A regular file with no read permission. Only meaningful when the
        // test process is not privileged; a root run reads it regardless and
        // this half says so instead of asserting on a value it cannot produce.
        let dir = site_dir(&[("y.md", &file("y", "Y", &valid_body()))]);
        let unreadable = dir.path().join("y.md");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&unreadable).is_ok() {
            eprintln!("privileged test process: the mode-000 half of this case is skipped");
        } else {
            assert_refused_naming(load(dir.path()), &unreadable);
        }
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn a_body_with_a_malformed_task_fence_is_refused_naming_it() {
        let mut body = valid_body();
        body.push_str("\n```neige-block task\nnot json\n```\n");
        let dir = site_dir(&[("x.md", &file("x", "X", &body))]);
        let path = dir.path().join("x.md");
        let message = assert_refused_naming(load(dir.path()), &path);
        assert!(message.contains("body does not compile"), "{message}");
    }

    #[test]
    fn a_body_with_a_schema_invalid_task_fence_is_refused_naming_it() {
        let mut body = valid_body();
        body.push_str(&render_fence(KIND_TASK, &json!({ "key": "Not A Key" })));
        let dir = site_dir(&[("x.md", &file("x", "X", &body))]);
        let path = dir.path().join("x.md");
        let message = assert_refused_naming(load(dir.path()), &path);
        assert!(message.contains("body does not compile"), "{message}");
    }

    /// The persist-time funnel, run at boot: a header that is not canonical
    /// (here: spaces inside the JSON) or not on line 1 would otherwise pass
    /// `compile_template` and then fail every create from this template.
    #[test]
    fn a_body_failing_the_contract_header_funnel_is_refused_naming_it() {
        let canonical = canonical_line(&work_brief_header());
        let spaced = canonical.replacen("\"version\":1", "\"version\": 1", 1);
        assert_ne!(spaced, canonical, "the fixture must be non-canonical");
        let body = format!("{spaced}\n\n# Plan\n\nprose\n");
        let dir = site_dir(&[("x.md", &file("x", "X", &body))]);
        let path = dir.path().join("x.md");
        let message = assert_refused_naming(load(dir.path()), &path);
        assert!(message.contains("report contract header"), "{message}");

        let misplaced = format!("# Plan\n\n{canonical}\n\nprose\n");
        let dir = site_dir(&[("x.md", &file("x", "X", &misplaced))]);
        let path = dir.path().join("x.md");
        let message = assert_refused_naming(load(dir.path()), &path);
        assert!(message.contains("report contract header"), "{message}");
    }

    /// The stem alphabet is the id alphabet, enforced by `id == stem` plus
    /// the front matter's own id rule: an upper-case or underscored file name
    /// cannot become a key.
    #[test]
    fn a_stem_outside_the_id_alphabet_is_refused_naming_it() {
        for name in ["Site-X.md", "a_b.md", "x y.md"] {
            let stem = name.strip_suffix(".md").unwrap();
            let dir = site_dir(&[(name, &file(stem, "X", &valid_body()))]);
            let path = dir.path().join(name);
            let message = assert_refused_naming(load(dir.path()), &path);
            assert!(message.contains("must match"), "{name}: {message}");
        }
    }

    /// A site file named like a builtin is a *different* entry, keyed under
    /// `site/`, and the builtin keeps its key and its buffers. Dropping the
    /// prefix would make this a duplicate-key refusal.
    #[test]
    fn a_site_file_named_like_a_builtin_is_a_distinct_site_entry() {
        let name = format!("{ISSUE_DEVELOPMENT}.md");
        let dir = site_dir(&[(
            name.as_str(),
            &file(
                ISSUE_DEVELOPMENT,
                "Operator's issue development",
                &valid_body(),
            ),
        )]);
        let roster = load(dir.path()).expect("a builtin-named site file is not a collision");
        let builtin = TemplateRoster::builtin()
            .get(ISSUE_DEVELOPMENT)
            .expect("builtin");
        let kept = roster
            .get(ISSUE_DEVELOPMENT)
            .expect("the builtin key still admits");
        assert!(
            std::ptr::eq(kept.key.as_ptr(), builtin.key.as_ptr()),
            "the builtin entry must be untouched, not overridden"
        );
        assert_eq!(kept.title(), builtin.title());
        let site_key = format!("{SITE_PREFIX}{ISSUE_DEVELOPMENT}");
        let site = roster
            .get(&site_key)
            .expect("the site entry admits under site/");
        assert_eq!(site.title(), "Operator's issue development");
        assert_ne!(site.recipe().body, builtin.recipe().body);
        assert_eq!(
            roster
                .entries()
                .iter()
                .filter(|t| t.key().ends_with(ISSUE_DEVELOPMENT))
                .count(),
            2,
            "one builtin, one site entry"
        );
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
