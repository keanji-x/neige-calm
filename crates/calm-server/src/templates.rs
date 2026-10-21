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
pub const DEV: &str = "dev";
pub const SMALL_CHANGE: &str = "small-change";
pub const INVESTIGATION: &str = "investigation";
/// A report-only template: no pre-set `task` blocks.
pub const INVESTMENT_RESEARCH: &str = "investment-research";
/// Kernel-created daily report planning; hidden from public template creation.
pub const DAILY_PLANNER: &str = "daily-planner";

/// Key prefix of an operator-provided template (`<dir>/<stem>.md` → `site/<stem>`). A front
/// matter `id` may not contain `/`, so no builtin file or plugin can claim a `site/…` key.
pub const SITE_PREFIX: &str = "site/";

/// The builtin template files, in picker order. Parsed at first use; a file that does not
/// parse is a panic there, deliberately.
static BUILTIN_SOURCES: [&str; 6] = [
    include_str!("../templates/builtin/daily-planner.md"),
    include_str!("../templates/builtin/dev.md"),
    include_str!("../templates/builtin/small-change.md"),
    include_str!("../templates/builtin/investigation.md"),
    include_str!("../templates/builtin/investment-research.md"),
    include_str!("../templates/builtin/opencode-history-rebuild.md"),
];

mod objectives;

/// One roster entry. Every field is private and there is no constructor/`Clone`/`Default`, so in
/// safe Rust a `&'static Template` can only be borrowed from the roster. Accessors hand back
/// `&'static str`: downstream stores the roster's own buffer, not a copy.
pub struct Template {
    key: &'static str,
    title: &'static str,
    description: Option<&'static str>,
    instructions: Option<&'static str>,
    user_creatable: bool,
    capabilities: &'static [calm_types::track_capability::TrackCapability],
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

    pub fn description(&self) -> Option<&'static str> {
        self.description
    }

    pub fn user_creatable(&self) -> bool {
        self.user_creatable
    }

    pub fn capabilities(&self) -> &'static [calm_types::track_capability::TrackCapability] {
        self.capabilities
    }

    pub fn instructions(&self) -> Option<&'static str> {
        self.instructions
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
    ObjectiveBody {
        index: usize,
        error: String,
    },
    /// `--templates-dir` could not be listed.
    SiteDir {
        dir: PathBuf,
        error: std::io::Error,
    },
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
            Self::ObjectiveBody { index, error } => write!(f, "template source #{index}: {error}"),
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
            objectives::validate(body, &front.capabilities)
                .map_err(|error| RosterError::ObjectiveBody { index, error })?;
            entries.push(Template {
                key: String::leak(front.id),
                title: String::leak(front.title),
                description: front.description.map(|value| &*String::leak(value)),
                instructions: front.instructions.map(|value| &*String::leak(value)),
                user_creatable: front.user_creatable,
                capabilities: Box::leak(front.capabilities.into_boxed_slice()),
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
                    description: template.description,
                    instructions: template.instructions,
                    user_creatable: template.user_creatable,
                    capabilities: template.capabilities,
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
        objectives::validate(body, &front.capabilities)
            .map_err(|error| fail(SiteFileError::Body(error)))?;
        let template = Template {
            key: String::leak(format!("{SITE_PREFIX}{stem}")),
            title: String::leak(front.title),
            description: front.description.map(|value| &*String::leak(value)),
            instructions: front.instructions.map(|value| &*String::leak(value)),
            user_creatable: front.user_creatable,
            capabilities: Box::leak(front.capabilities.into_boxed_slice()),
            body,
        };
        let compiled = crate::routes::tracks::compile_template(&template)
            .map_err(|error| fail(SiteFileError::Body(error.to_string())))?;
        calm_types::report_contract::check_document(compiled.body())
            .map_err(|error| fail(SiteFileError::ContractHeader(error)))?;
        Ok(template)
    }

    /// Publicly creatable entries in picker order.
    pub fn user_entries(&self) -> impl Iterator<Item = &Template> {
        self.entries
            .iter()
            .filter(|template| template.user_creatable())
    }

    /// Every registered entry, including kernel-only templates.
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
mod tests;

#[cfg(test)]
mod site_dir_tests;

#[cfg(test)]
mod repro_1239;

#[cfg(test)]
mod objective_tests;
