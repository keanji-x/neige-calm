//! Track-report payload vocabulary (#679 PR1).
//!
//! [`TrackReportPayload`] is the Tier-A persisted card payload + TS-exported
//! wire type, so it lives here. The persist boundary (`write::persist` and
//! its three entry points, CRDT plumbing, REST/MCP resolvers) stays in
//! calm-server's `track_report` module, which re-exports this type.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

/// A derived, addressable slice of a track report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase")]
pub struct ReportBlock {
    pub id: String,
    pub kind: String,
    pub rev: u32,
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
}

/// The payload persisted in a track-report card's `payload` JSON column.
///
/// Wire shape (camelCase to match the rest of the kernel's payloads):
///
/// ```json
/// {
///   "schemaVersion": 4,
///   "docRev": 7,
///   "summary": "Refactored the dispatcher into a typed actor",
///   "body": "# Goal\n\nReplace the ad-hoc loop with…\n\n# Progress\n..."
/// }
/// ```
///
/// `summary` is the one-line previewable in sidebars / list views;
/// `body` is the Markdown source the TrackReportCard renders. The
/// frontend derives sections from `body` by splitting on H1 headings;
/// the storage layer does not impose a section vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase")]
pub struct TrackReportPayload {
    /// Tier A persistence contract — see
    /// `TRACK_REPORT_PAYLOAD_SCHEMA_VERSION` in calm-truth's
    /// `validation.rs`. `4` since #1456 discriminated terminal `command`
    /// from agent `goal`; blocks remain authoritative and `body` is their
    /// flat projection. Older rows remain readable and are lazily
    /// upgraded at the next persist via the CRDT-layer migrator
    /// (`ReportDoc::ensure_blocks_layout`).
    pub schema_version: u32,
    /// Document-wide optimistic-concurrency revision. This is mirrored
    /// from the authoritative CRDT root and increments after every
    /// successful report persist (whole-document or block-level).
    #[serde(default)]
    #[schema(required = true)]
    pub doc_rev: u64,
    /// One-line summary used by sidebars / track-list previews. Empty
    /// string is valid (means "planner agent has not produced a summary
    /// yet"); the field stays a required `String` per the
    /// [[required-over-option]] rule.
    pub summary: String,
    /// Markdown source. Sections are derived at render time by
    /// splitting at H1 (`^# `) headings; the kernel does not interpret
    /// the structure.
    pub body: String,
    /// Block mirror of the authoritative CRDT block map (#960 PR2).
    /// Since schema v2 the CRDT `blocks`/`order` layout is the source
    /// of truth; this JSON field and `body` are both projections the
    /// persist boundary rewrites on every write. v1 rows may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<ReportBlock>>,
}

// —— #1185: the report's maintenance contract travels with the document ——
//
// The kernel does not decide which sections a report has; the document
// carries its own rules in a leading HTML comment. The three fragments below
// are `include_str!` rather than escaped Rust literals because this text gets
// byte-reviewed routinely and 40 lines of `\n` escapes are unreadable. cargo
// tracks `include_str!` as a build dependency, so editing the .md recompiles.
//
// All three fragments are UNCLOSED (no `-->`) and therefore **private**.
// Handing out an unclosed comment fails silently and globally: a caller that
// forgets to append `-->` makes the comment swallow the whole document, and
// both frontends then render a completely blank report with no diagnostic
// (#1185 §1.5 B). Only the closed forms below leave this module.

/// Genre rules, independent of any section list: work-brief voice, reader
/// assumption, current-snapshot / REWRITE, outcomes-not-process, no long
/// quotes, the 1000-word soft target. Contains no `-->`.
const CONTRACT_WRITING_RULES: &str = include_str!("track_report_contract_rules.md");

/// Structure rules + the four section descriptions. The structure rule is
/// worded as "the sections are defined by the list below", so it cannot be
/// reused apart from that list — a template that got the structure rule
/// without the list would be told it may only ever have `# Plan`
/// (#1185 §1.5 B). Contains no `-->`.
const CONTRACT_SECTION_RULES: &str = include_str!("track_report_section_rules.md");

/// Template-only addendum: the pre-set plan sections hand their prose over
/// to the four report sections once tasks are activated. Contains no `-->`.
const CONTRACT_PLAN_NOTE: &str = include_str!("track_report_plan_note.md");

/// Closes the contract comment. Blank line after it so the first H1 starts
/// its own block (`split_body` splits at line-initial `# ` / `## `).
const CONTRACT_CLOSE: &str = "-->\n\n";

/// The default section skeleton. Sections are left empty on purpose: a
/// `_待填_` placeholder would render, and the agent would read it as content
/// to delete.
const DEFAULT_H1S: &str = "# 概要\n\n# 待你定\n\n# 已完成\n\n# 决策\n";

/// The default report body: writing rules + section rules, closed, then the
/// four empty H1s. `LazyLock` rather than a `format!` per call because
/// [`TrackReportPayload::initial`] is hot — it backs the birth hard-check in
/// calm-truth and every `report_startup_read_required` comparison.
fn initial_body() -> &'static str {
    static BODY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        format!("{CONTRACT_WRITING_RULES}{CONTRACT_SECTION_RULES}{CONTRACT_CLOSE}{DEFAULT_H1S}")
    });
    &BODY
}

/// Report-body prefix for the kernel's built-in templates: writing
/// rules + section rules + the pre-set section notes, **already closed**.
///
/// The templates build their body with [`TrackReportPayload::new`], bypassing
/// [`TrackReportPayload::initial`], so without this they would carry no
/// contract at all — losing not just the section list but the word budget,
/// the current-snapshot rule and the no-process-narration rule that #1146 S1
/// and #1172 put there (#1185 §1.5 B).
///
/// The return value is **closed**; unclosed fragments never leave this
/// module. See the module comment above for why.
pub fn report_contract_prefix_for_template() -> &'static str {
    static PREFIX: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        format!(
            "{CONTRACT_WRITING_RULES}{CONTRACT_SECTION_RULES}{CONTRACT_PLAN_NOTE}{CONTRACT_CLOSE}"
        )
    });
    &PREFIX
}

impl TrackReportPayload {
    /// Current schema version. Bumping this is a Tier A breaking
    /// change — the same PR must also extend
    /// [`crate::card_kind::TrackReportCardHandler`] and the matching
    /// frontend zod schema in
    /// `web/src/api/schemas.ts`.
    pub const SCHEMA_VERSION: u32 = 4;

    pub fn new(summary: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            doc_rev: 0,
            summary: summary.into(),
            body: body.into(),
            blocks: None,
        }
    }

    /// Canonical "track was just minted; planner hasn't run yet" payload.
    /// Used by `routes::tracks::create_track` (PR B). Historical
    /// migration seeds stay frozen; freshly-minted tracks use this copy.
    ///
    /// The body is a *structural skeleton*: a maintenance contract carried in
    /// a leading HTML comment, then the four default H1 sections (#1185).
    /// The comment is dropped when the document is rendered, so users never
    /// see it on the page — but it stays in the body source, which every
    /// source-reading subject reads (the planner agent, a worker's
    /// `neige cat report.md`, the REST read surface, the track's VCS diff).
    /// It is layout control, not access control: never put secrets in it.
    pub fn initial() -> Self {
        Self::new("", initial_body())
    }

    /// #1110 S3 — whether planner's first turn must `calm.report.read`.
    ///
    /// False only when `summary` and `body` equal [`Self::initial()`].
    /// `doc_rev` / `blocks` are ignored so a CRDT-materialized placeholder
    /// stays false. Forked or edited content is true.
    pub fn report_startup_read_required(&self) -> bool {
        let initial = Self::initial();
        self.summary != initial.summary || self.body != initial.body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_startup_read_required_is_false_only_for_canonical_initial_content() {
        let initial = TrackReportPayload::initial();
        assert!(
            !initial.report_startup_read_required(),
            "canonical initial payload must not require a startup read"
        );

        let mut materialized = initial.clone();
        materialized.doc_rev = 7;
        materialized.blocks = Some(vec![]);
        assert!(
            !materialized.report_startup_read_required(),
            "doc_rev/blocks must not flip the bit when summary/body are still initial"
        );

        assert!(
            TrackReportPayload::new("", "edited body\n").report_startup_read_required(),
            "an edited body is a pre-set plan"
        );
        assert!(
            TrackReportPayload::new("fork source summary", initial.body.clone())
                .report_startup_read_required(),
            "a non-empty summary is not the canonical placeholder"
        );
    }

    /// #1185 — the birth body is a structural skeleton, and this test is the
    /// one place in the repo that pins its shape.
    #[test]
    fn initial_body_is_the_default_structural_skeleton() {
        let body = TrackReportPayload::initial().body;

        // —— the contract block: first, and closed before the first H1 ——
        // `starts_with("<!--") + contains("-->")` is a vacuous pair: moving
        // `-->` below a heading satisfies both and destroys the rendering.
        // So assert the slice shape instead.
        let slices = crate::report_blocks::split_body(&body);
        assert_eq!(
            slices.len(),
            5,
            "1 contract block + 4 sections; got {slices:#?}"
        );
        assert!(slices[0].raw.starts_with("<!-- 报告维护契约"));
        assert!(
            slices[0].raw.ends_with("-->\n\n"),
            "the contract must close before the first H1"
        );
        for (slice, head) in
            slices[1..]
                .iter()
                .zip(["# 概要\n", "# 待你定\n", "# 已完成\n", "# 决策\n"])
        {
            assert!(
                slice.raw.starts_with(head),
                "section order is fixed: {head:?}"
            );
        }
        // No line-initial `# `/`## ` inside the contract — `split_body` would
        // cleave it into two blocks (#1185 §0(a)).
        assert!(
            !slices[0]
                .raw
                .lines()
                .skip(1)
                .any(|l| l.starts_with("# ") || l.starts_with("## ")),
            "a heading inside the contract would split it into two blocks"
        );
        assert!(
            !body.contains("# 进行中"),
            "#1172: the TASKS panel owns task runtime state"
        );

        // —— the policy really did move here (these strings used to live in
        // calm-server's planner_card.rs) ——
        for rule in [
            "写产出，不写过程",
            "散文正文",
            "1000 字",
            "不计入",
            "REWRITE",
            "没有就省略这个 section",
            "不要 append 后不删",
        ] {
            assert!(
                body.contains(rule),
                "maintenance contract must carry `{rule}`"
            );
        }

        // —— kernel carrier properties ——
        assert_eq!(crate::report_blocks::flatten(&slices), body);
        assert!(
            slices
                .iter()
                .all(|s| crate::report_blocks::parse_fence(&s.raw).is_none())
        );
        assert!(crate::report_blocks::check_prose_markdown(&body).is_ok());
        assert!(crate::report_blocks::invalid_neige_fences(&body).is_empty());
        assert_eq!(
            crate::report_blocks::strip_markers_and_split(&body).cleaned,
            body,
            "the marker stripper must not eat the contract comment"
        );

        // —— byte properties ——
        assert!(
            !body.contains('\r'),
            "a CRLF checkout would silently change split_body's input"
        );
        assert!(body.ends_with('\n') && !body.ends_with("\n\n"));
        // A second `-->` smuggled into a fragment closes the comment early and
        // the tail renders visibly.
        assert_eq!(body.matches("-->").count(), 1, "exactly one comment close");
    }

    /// #1185 §1.5 B — the built-in templates must get the same
    /// writing policy, or #1146's guardrails silently vanish on them.
    #[test]
    fn the_template_prefix_is_closed_and_shares_the_default_contract() {
        let prefix = report_contract_prefix_for_template();
        let body = TrackReportPayload::initial().body;

        assert!(prefix.starts_with("<!-- 报告维护契约"));
        assert!(
            prefix.ends_with("-->\n\n"),
            "never hand out an unclosed comment"
        );
        assert_eq!(prefix.matches("-->").count(), 1);
        assert!(!prefix.contains('\r'));

        // What is shared is the genre rules + the section list; the template
        // gets one extra `Plan` note. Comparing the fragment constants would
        // just restate the concatenation; asserting on sentences both sides
        // must carry is what catches one side being quietly edited.
        for shared in [
            "写产出，不写过程",
            "散文正文",
            "1000 字",
            "章节由下面这份清单定义",
            "没有就省略这个 section",
        ] {
            assert!(
                prefix.contains(shared) && body.contains(shared),
                "the writing rules and the section list must be one text: `{shared}`"
            );
        }
        // The template must be licensed to grow the four sections — otherwise
        // issue-development is stuck with `# Plan` forever (#1185 §1.5 B).
        for section in ["概要", "待你定", "已完成", "决策"] {
            assert!(
                prefix.contains(section),
                "template contract must license `{section}`"
            );
        }
        assert!(
            prefix.contains("Plan —— 预置计划"),
            "and must say what happens to # Plan"
        );
        assert!(
            !body.contains("Plan —— 预置计划"),
            "the default skeleton has no plan section"
        );

        // The contract must stay one block — fragment concatenation must not
        // introduce a line-initial H1/H2.
        assert_eq!(crate::report_blocks::split_body(prefix).len(), 1);
    }
}
