//! Track-report payload vocabulary: the Tier-A persisted card payload + TS-exported wire type.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

use crate::report_blocks::{parse_fence, strip_markers_and_split};
use crate::report_contract::{
    ContractHeader, ContractSection, check_document, is_pure_comment_block,
};

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

/// The payload persisted in a track-report card's `payload` JSON column; `summary` is the one-line
/// preview, `body` the Markdown source the TrackReportCard renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase")]
pub struct TrackReportPayload {
    /// Tier A persistence contract; older rows remain readable and are lazily upgraded at the next persist.
    pub schema_version: u32,
    /// Document-wide optimistic-concurrency revision, mirrored from the authoritative CRDT root.
    #[serde(default)]
    #[schema(required = true)]
    pub doc_rev: u64,
    /// One-line summary used by sidebars / track-list previews; empty string is valid.
    pub summary: String,
    /// Markdown source. Sections are derived at render time by splitting at H1 (`^# `) headings.
    pub body: String,
    /// Block mirror of the authoritative CRDT block map; this field and `body` are both projections the
    /// persist boundary rewrites on every write. v1 rows may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<ReportBlock>>,
}

/// The work-brief contract header: the four default sections, `待你定` omitted when empty.
pub fn work_brief_header() -> ContractHeader {
    ContractHeader {
        version: 1,
        sections: vec![
            section("概要", false),
            section("待你定", true),
            section("已完成", false),
            section("决策", false),
        ],
    }
}

/// The investment-research contract header: seven fixed sections, `待你定` omitted when empty.
pub fn research_header() -> ContractHeader {
    ContractHeader {
        version: 1,
        sections: vec![
            section("结论", false),
            section("待你定", true),
            section("核心逻辑", false),
            section("关键数据", false),
            section("风险与证伪", false),
            section("催化剂与跟踪", false),
            section("来源与边界", false),
        ],
    }
}

fn section(h1: &str, omit_if_empty: bool) -> ContractSection {
    ContractSection {
        h1: h1.to_string(),
        omit_if_empty,
    }
}

/// The default report body: `report/default.md`, byte for byte. Sections are left empty on purpose:
/// a placeholder would render and the agent would read it as content to delete.
fn initial_body() -> &'static str {
    include_str!("report/default.md")
}

/// The default report body exactly as shipped before the contract header line, frozen as a file.
/// Never edit it: a change is a change to what "unwritten" means for every pre-header track.
pub const LEGACY_INITIAL_V4_BODY: &str = include_str!("report/legacy_initial_v4.md");

impl TrackReportPayload {
    /// Current schema version. Bumping this is a Tier A breaking change that must also extend the
    /// track-report card handler and the frontend zod schema.
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

    /// Canonical "track was just minted; planner hasn't run yet" payload. Its HTML comments are dropped
    /// on render but stay in the body source every source-reading subject reads: layout control, not access control — never put secrets in it.
    pub fn initial() -> Self {
        Self::new("", initial_body())
    }

    /// Whether planner's first turn must `calm.report.read`: `!self.is_unwritten()`.
    pub fn report_startup_read_required(&self) -> bool {
        !self.is_unwritten()
    }

    /// The structural "nothing has been written here" predicate (`true` = unwritten): `summary` empty
    /// and, on the marker-stripped body, either a headered pure-comment block 0 followed only by bare
    /// declared `# <h1>` blocks, or a headerless body byte-equal to [`LEGACY_INITIAL_V4_BODY`].
    /// A body the funnel check rejects reads as written (fail-closed).
    fn is_unwritten(&self) -> bool {
        if !self.summary.is_empty() {
            return false;
        }
        let marked = strip_markers_and_split(&self.body);
        match check_document(&marked.cleaned) {
            Ok(Some(header)) => marked.slices.iter().enumerate().all(|(index, slice)| {
                parse_fence(&slice.raw).is_none()
                    && if index == 0 {
                        is_pure_comment_block(slice.raw.trim())
                    } else {
                        header
                            .sections
                            .iter()
                            .any(|section| slice.raw.trim() == format!("# {}", section.h1))
                    }
            }),
            Ok(None) => self.body == LEGACY_INITIAL_V4_BODY,
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report_contract::{HEADER_OPEN, canonical_line, check_document, normalize_header};
    use std::borrow::Cow;

    #[test]
    fn report_startup_read_required_is_false_for_the_canonical_initial_payload() {
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

    #[test]
    fn report_startup_read_required_cell() {
        use crate::report_blocks::{KIND_TASK, render_fence};
        use crate::report_contract::HeaderError;

        let new = |summary: &str, body: String| TrackReportPayload::new(summary, body);
        let header = canonical_line(&work_brief_header());
        let block0 = format!("{header}\n<!-- 报告维护契约 -->\n\n");
        let headered = |sections: &str| format!("{block0}{sections}");
        let four = "# 概要\n\n# 待你定\n\n# 已完成\n\n# 决策\n";

        let initial = TrackReportPayload::initial();
        let mut materialized = initial.clone();
        materialized.doc_rev = 7;
        materialized.blocks = Some(vec![]);

        let non_canonical = headered(four).replacen(
            r#"{"h1":"概要"}"#,
            r#"{"h1":"概要","omit_if_empty":false}"#,
            1,
        );
        assert_ne!(non_canonical, headered(four), "the replacement must land");
        assert!(
            matches!(
                check_document(&non_canonical),
                Err(HeaderError::Internal(_))
            ),
            "a non-canonical header is the funnel's Internal error"
        );

        let task_fence = render_fence(
            KIND_TASK,
            &serde_json::json!({"title": "预置任务", "ready": false}),
        );
        assert!(parse_fence(&task_fence).is_some());

        let rows: Vec<(&str, TrackReportPayload, bool)> = vec![
            ("initial()", initial.clone(), false),
            (
                "initial() materialized by CRDT: doc_rev = 7, blocks = Some([])",
                materialized,
                false,
            ),
            (
                "legacy pre-header bytes (headerless → byte-equal fallback)",
                new("", LEGACY_INITIAL_V4_BODY.to_string()),
                false,
            ),
            (
                "minimal headered skeleton, the four declared H1s",
                new("", headered(four)),
                false,
            ),
            (
                "a SUBSET of the declared H1s (no `# 待你定`)",
                new("", headered("# 概要\n\n# 已完成\n\n# 决策\n")),
                false,
            ),
            (
                "the declared H1s in a different ORDER (membership, not sequence)",
                new("", headered("# 决策\n\n# 概要\n\n# 待你定\n\n# 已完成\n")),
                false,
            ),
            (
                "trailing spaces on a heading line (`str::trim` leniency)",
                new(
                    "",
                    headered("# 概要   \n\n# 待你定\n\n# 已完成\n\n# 决策\n"),
                ),
                false,
            ),
            (
                "whitespace-only lines under a heading (`str::trim` leniency)",
                new(
                    "",
                    headered("# 概要\n\n\n   \n# 待你定\n\n# 已完成\n\n# 决策\n"),
                ),
                false,
            ),
            (
                "a closed user comment in block 0 after the contract (issue §6.9)",
                new(
                    "",
                    format!("{header}\n<!-- 报告维护契约 -->\n<!-- User note -->\n\n{four}"),
                ),
                false,
            ),
            (
                "a non-empty summary over the initial body",
                new("fork source summary", initial.body.clone()),
                true,
            ),
            (
                "prose under a heading",
                new(
                    "",
                    headered("# 概要\n\n写了一句。\n\n# 待你定\n\n# 已完成\n\n# 决策\n"),
                ),
                true,
            ),
            (
                "an `![alt](x)` line",
                new(
                    "",
                    headered("# 概要\n\n![alt](x)\n\n# 待你定\n\n# 已完成\n\n# 决策\n"),
                ),
                true,
            ),
            (
                "a `<table>`",
                new(
                    "",
                    headered("# 概要\n\n<table></table>\n\n# 待你定\n\n# 已完成\n\n# 决策\n"),
                ),
                true,
            ),
            (
                "a `---` rule",
                new(
                    "",
                    headered("# 概要\n\n---\n\n# 待你定\n\n# 已完成\n\n# 决策\n"),
                ),
                true,
            ),
            (
                "a `## sub` heading",
                new(
                    "",
                    headered("# 概要\n\n## sub\n\n# 待你定\n\n# 已完成\n\n# 决策\n"),
                ),
                true,
            ),
            (
                "a task fence anywhere",
                new(
                    "",
                    headered(&format!(
                        "# 概要\n\n# 待你定\n\n# 已完成\n\n{task_fence}\n# 决策\n"
                    )),
                ),
                true,
            ),
            (
                "a closed HTML comment in a block ≥ 1 (issue §6.9 asymmetry)",
                new(
                    "",
                    headered("# 概要\n\n<!-- 已闭合 -->\n\n# 待你定\n\n# 已完成\n\n# 决策\n"),
                ),
                true,
            ),
            (
                "an H1 the header does not declare (`# Extra`)",
                new(
                    "",
                    headered("# 概要\n\n# 待你定\n\n# 已完成\n\n# 决策\n\n# Extra\n"),
                ),
                true,
            ),
            (
                "a leading space before `#` (not a heading to split_body; stays in block 0)",
                new("", headered(" # 概要\n\n# 待你定\n\n# 已完成\n\n# 决策\n")),
                true,
            ),
            (
                "prose in block 0 after the contract comment",
                new("", format!("{header}\n<!-- c -->\nstray text\n\n# 概要\n")),
                true,
            ),
            (
                "a `<!--` indented four columns in block 0 (CommonMark indented code: visible)",
                new(
                    "",
                    format!("{header}\n<!-- 报告维护契约 -->\n    <!-- visible note -->\n\n{four}"),
                ),
                true,
            ),
            (
                "a non-canonical header (funnel `Err(Internal)` → fail-closed)",
                new("", non_canonical),
                true,
            ),
            (
                "an empty body (headerless, not the legacy bytes)",
                new("", String::new()),
                true,
            ),
        ];
        assert!(
            rows.iter().any(|(_, _, expected)| *expected)
                && rows.iter().any(|(_, _, expected)| !*expected),
            "the cell must carry both polarities"
        );

        let mismatches: Vec<String> = rows
            .iter()
            .filter(|(_, payload, expected)| payload.report_startup_read_required() != *expected)
            .map(|(name, _, expected)| {
                format!("  [{name}] expected report_startup_read_required == {expected}")
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "{} row(s) off the D3 cell:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    #[test]
    fn initial_body_is_the_default_structural_skeleton() {
        let body = TrackReportPayload::initial().body;

        let slices = crate::report_blocks::split_body(&body);
        assert_eq!(
            slices.len(),
            5,
            "1 contract block + 4 sections; got {slices:#?}"
        );
        assert!(slices[0].raw.starts_with(HEADER_OPEN));
        assert_eq!(
            slices[0].raw.lines().next(),
            Some(canonical_line(&work_brief_header()).as_str()),
            "line 1 is the canonical work-brief header"
        );
        assert!(
            slices[0]
                .raw
                .lines()
                .nth(1)
                .is_some_and(|line| line.starts_with("<!-- 报告维护契约")),
            "the prose contract comment follows the header line"
        );
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

        assert!(
            !body.contains('\r'),
            "a CRLF checkout would silently change split_body's input"
        );
        assert!(body.ends_with('\n') && !body.ends_with("\n\n"));
        assert_eq!(
            body.lines().filter(|l| l.starts_with(HEADER_OPEN)).count(),
            1,
            "exactly one contract header line"
        );
        assert_eq!(check_document(&body), Ok(Some(work_brief_header())));
    }

    #[test]
    fn initial_body_is_header_line_plus_legacy_v4() {
        assert_eq!(
            TrackReportPayload::initial().body,
            format!(
                "{}\n{LEGACY_INITIAL_V4_BODY}",
                canonical_line(&work_brief_header())
            )
        );
    }

    #[test]
    fn initial_body_passes_the_funnel_check_unchanged() {
        let body = TrackReportPayload::initial().body;
        assert_eq!(check_document(&body), Ok(Some(work_brief_header())));
        assert!(
            matches!(normalize_header(&body), Ok(Cow::Borrowed(_))),
            "the shipped header is already canonical"
        );
        let slices = crate::report_blocks::split_body(&body);
        assert!(crate::report_contract::is_pure_comment_block(
            &slices[0].raw
        ));
    }

    #[test]
    fn default_h1s_are_the_work_brief_header_sections() {
        let body = TrackReportPayload::initial().body;
        let slices = crate::report_blocks::split_body(&body);
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
        let declared: Vec<String> = work_brief_header()
            .sections
            .into_iter()
            .map(|section| section.h1)
            .collect();
        assert_eq!(h1s, declared);
        assert_eq!(
            check_document(&body).unwrap().unwrap().sections,
            work_brief_header().sections,
            "and the header the file carries is the one the constructor builds"
        );
    }

    #[test]
    fn legacy_initial_v4_bytes_are_pinned() {
        use sha2::{Digest as _, Sha256};

        assert_eq!(LEGACY_INITIAL_V4_BODY.len(), 2647);
        assert!(
            !LEGACY_INITIAL_V4_BODY.contains('\r'),
            "a CRLF checkout would silently change the frozen bytes"
        );
        let digest = format!("{:x}", Sha256::digest(LEGACY_INITIAL_V4_BODY.as_bytes()));
        assert_eq!(
            digest,
            "6cd893b62424185a842cccc790712a0c1d05151ec84cb6d9d85544f0d2e3f9f3"
        );
    }

    #[test]
    fn legacy_initial_v4_reads_as_unwritten() {
        let payload = TrackReportPayload::new("", LEGACY_INITIAL_V4_BODY);
        assert!(
            !payload.report_startup_read_required(),
            "the frozen pre-header body must not require a startup read"
        );
        assert!(
            TrackReportPayload::new("fork source summary", LEGACY_INITIAL_V4_BODY)
                .report_startup_read_required(),
            "a non-empty summary is not the canonical placeholder"
        );
        assert!(
            TrackReportPayload::new("", format!("{LEGACY_INITIAL_V4_BODY}\n"))
                .report_startup_read_required(),
            "one byte off the frozen body is a written body"
        );
    }
}
