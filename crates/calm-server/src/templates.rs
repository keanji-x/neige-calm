//! The template roster — the report a `template_id` create starts from. Builtin entries are
//! `templates/builtin/*.md` compiled in with `include_str!`; `--templates-dir` files are read
//! once, at boot, fail-closed, and keyed `site/<stem>`.

use crate::track_report::TrackReportPayload;
#[cfg(test)]
use calm_types::report_blocks::{KIND_TASK, parse_fence, split_body};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The `+++` TOML front matter a template file opens with.
pub mod front_matter;

// Protocol, not data: other code names these ids.
pub const ISSUE_DEVELOPMENT: &str = "issue-development";
pub const SMALL_CHANGE: &str = "small-change";
pub const INVESTIGATION: &str = "investigation";
/// A report-only template: no pre-set `task` blocks.
pub const INVESTMENT_RESEARCH: &str = "investment-research";

/// Key prefix of an operator-provided template (`<dir>/<stem>.md` → `site/<stem>`). A front
/// matter `id` may not contain `/`, so no builtin file or plugin can claim a `site/…` key.
pub const SITE_PREFIX: &str = "site/";

/// The builtin template files, in picker order. Parsed at first use; a file that does not
/// parse is a panic there, deliberately.
static BUILTIN_SOURCES: [&str; 4] = [
    include_str!("../templates/builtin/issue-development.md"),
    include_str!("../templates/builtin/small-change.md"),
    include_str!("../templates/builtin/investigation.md"),
    include_str!("../templates/builtin/investment-research.md"),
];

/// One roster entry. Every field is private and there is no constructor/`Clone`/`Default`, so in
/// safe Rust a `&'static Template` can only be borrowed from the roster. Accessors hand back
/// `&'static str`: downstream stores the roster's own buffer, not a copy.
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

    /// The uncompiled recipe; a fresh `TrackReportPayload` on every call.
    pub fn recipe(&self) -> TrackReportPayload {
        TrackReportPayload::new(self.title, self.body)
    }
}

/// Every template `POST /api/tracks` admits, in picker order. Constructible only inside this
/// module's subtree.
pub struct TemplateRoster {
    entries: Vec<Template>,
}

/// Why a set of template sources did not become a roster.
#[derive(Debug)]
pub(crate) enum RosterError {
    /// Source number `index` (0-based, in [`BUILTIN_SOURCES`] order) did not
    /// parse as a template file.
    FrontMatter {
        index: usize,
        error: front_matter::FrontMatterError,
    },
    /// Two sources declare the same `id`.
    DuplicateId(String),
    /// `--templates-dir` could not be listed.
    SiteDir { dir: PathBuf, error: std::io::Error },
    /// One file under `--templates-dir` did not become an entry; always names the file.
    SiteFile {
        path: PathBuf,
        reason: SiteFileError,
    },
}

/// The ways one `<dir>/<stem>.md` fails to load.
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
    /// `site/<stem>` is already on the roster; only reachable through
    /// [`TemplateRoster::extend_with_site_files`] with two directories.
    Duplicate(String),
    /// The body does not compile (`routes::tracks::compile_template`).
    Body(String),
    /// The body fails the contract-header funnel; without this check a file the boot accepted
    /// would fail every create.
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
    /// The built-in roster, parsed once. Panics at first use on a bad builtin file: a build
    /// defect, and the process must not come up advertising a partial roster.
    pub fn builtin() -> &'static TemplateRoster {
        static ROSTER: OnceLock<TemplateRoster> = OnceLock::new();
        ROSTER.get_or_init(|| Self::load(&BUILTIN_SOURCES))
    }

    /// [`Self::from_sources`], panicking on failure.
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

    /// The roster this boot serves: builtin, plus one `site/<stem>` entry per `*.md` in `site_dir`.
    /// Fail-closed; there is deliberately no arm that skips a file.
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

    /// Same entries, same `&'static str` buffers, nothing re-leaked. Not a `Clone` impl on purpose.
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
            // A directory named `x.md` is not skipped — it reaches `read_to_string` and fails there, naming itself.
            if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
                paths.push(path);
            }
        }
        paths.sort();
        self.extend_with_site_files(&paths)
    }

    /// Split from [`Self::extend_with_site_dir`] so the duplicate-key arm is reachable from a test.
    /// Every failure returns — the `?`s are the fail-closed contract.
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

    /// Read, parse, check and compile one operator file into an entry. Runs the two checks the
    /// create path runs (`compile_template`, `check_document`) so a `site/` entry never fails them at request time.
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

    /// The roster's single fallible lookup; `POST /api/tracks` admits an id iff this returns `Some`.
    /// The answer is a borrow into the roster, never a value derived from the argument.
    pub fn get(&self, key: &str) -> Option<&Template> {
        self.entries.iter().find(|template| template.key == key)
    }
}

/// Read the task blocks back out of a rendered template body as whole payloads (not
/// `PlanTaskInput`, whose `deny_unknown_fields` would silently drop `refs`/`tombstone`).
/// Test-only independent reader; lenient about fence shape, which is why it is not the production reader.
#[cfg(test)]
pub fn template_task_payloads_from_body(body: &str) -> Vec<Value> {
    split_body(body)
        .iter()
        .filter_map(|slice| parse_fence(&slice.raw))
        .filter(|fence| fence.kind == KIND_TASK)
        .map(|fence| fence.payload)
        .collect()
}

/// The picker projection of one task payload: `key` and its display instruction, or `None`
/// for a tombstone. Filtering happens here, never in the reader that produced the payloads.
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

    /// The three plan templates; `investment-research` is the one report-only entry.
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

    #[test]
    #[should_panic(expected = "duplicate template id `twin`")]
    fn duplicate_ids_panic_at_first_use() {
        TemplateRoster::load(&[
            "+++\nid = \"twin\"\ntitle = \"A\"\n+++\n# A\n",
            "+++\nid = \"twin\"\ntitle = \"B\"\n+++\n# B\n",
        ]);
    }

    #[test]
    #[should_panic(expected = "template source #1: template file must open with a `+++`")]
    fn a_source_without_front_matter_panics_at_first_use() {
        TemplateRoster::load(&[
            "+++\nid = \"fine\"\ntitle = \"Fine\"\n+++\n# A\n",
            "# no front matter\n",
        ]);
    }

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

    /// Asserted by data-pointer identity: an equality assertion would pass for a copy too.
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

    /// Accessor-against-private-field comparison; only expressible in the defining module.
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

    /// Block 0 is one identical text across the three plan templates, and `default.md`'s block 0
    /// is a prefix of it.
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
        // It reads as written because of its birth summary, not its body: the same body with an
        // empty summary is structurally unwritten.
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

    /// Identity on the payload, not merely agreement on the fields some struct models.
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
    //! The `--templates-dir` loader, on hand-built directories. Every refusal case asserts `Err`
    //! and that the error names the offending file.

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

    /// The production entry point on one directory, exactly what `AppState::boot` calls.
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

        // The builtin half is the builtin roster's own entries — same buffers, nothing re-leaked.
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

    /// The duplicate arm is only reachable with the same stem in two directories.
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
        // A directory named `x.md`: the extension filter deliberately does not skip it.
        let dir = site_dir(&[]);
        let as_dir = dir.path().join("x.md");
        std::fs::create_dir(&as_dir).unwrap();
        assert_refused_naming(load(dir.path()), &as_dir);

        // Only meaningful when the test process is not privileged; a root run reads it regardless.
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

    /// A non-canonical or misplaced header passes `compile_template` and would fail every create.
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

    /// `PlanTaskInput` is `deny_unknown_fields`; a well-formed fence carrying `refs` must survive the read.
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

    /// The guard's removal check is gated on `!is_tombstone(old)`, so nothing else stops a
    /// rewrite from erasing a tombstone.
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
