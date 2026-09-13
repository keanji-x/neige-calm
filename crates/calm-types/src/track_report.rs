//! Track-report payload vocabulary (#679 PR1).
//!
//! [`TrackReportPayload`] is the Tier-A persisted card payload + TS-exported
//! wire type, so it lives here. The persist boundary (`write::persist` and
//! its three entry points, CRDT plumbing, REST/MCP resolvers) stays in
//! calm-server's `track_report` module, which re-exports this type.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

use crate::report_contract::{ContractHeader, ContractSection, canonical_line};

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
// All four fragments are UNCLOSED (no `-->`) and therefore **private**.
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

/// #1571 — the investment-research contract, **self-contained**: it carries
/// its own copy of the two preamble paragraphs (render-time drop / no secrets,
/// "the structure is the rule") because [`CONTRACT_WRITING_RULES`] opens with
/// the work-brief comment header and its genre rules in one file, and cannot
/// be split into a shared preamble without rewriting the default contract.
/// Genre rules (thesis-first, sourced numbers, strongest counter-argument,
/// the 1500—2500 字 budget) and the seven-section list live together here
/// for the same reason [`CONTRACT_SECTION_RULES`] is not reusable: the
/// structure rule is worded as "the sections are defined by the list below".
/// Contains no `-->`.
const CONTRACT_RESEARCH_RULES: &str = include_str!("track_report_research_rules.md");

/// Closes the contract comment. Blank line after it so the first H1 starts
/// its own block (`split_body` splits at line-initial `# ` / `## `).
const CONTRACT_CLOSE: &str = "-->\n\n";

/// #1635 D2 — the work-brief contract header: the four default sections,
/// `待你定` omitted when empty. With [`research_header`] this is the source
/// the header lines are built from; `report/default.md` line 1 is
/// `canonical_line(&work_brief_header())`, the pin in
/// `initial_body_is_header_line_plus_legacy_v4` holds the two together, and
/// `default_h1s_are_the_work_brief_header_sections` ties the file's H1s to
/// these sections (calm-server's templates test does the same for the
/// research skeleton). S4 moves the names into template files.
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

/// #1635 D2 — the investment-research contract header (#1571): seven fixed
/// sections, `待你定` omitted when empty.
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

/// The default report body (#1635 S2b): `report/default.md`, byte for byte —
/// the canonical work-brief header line, then exactly the frozen
/// [`LEGACY_INITIAL_V4_BODY`] (writing rules + section rules, closed, then
/// the four empty H1s; sections are left empty on purpose — a `_待填_`
/// placeholder would render and the agent would read it as content to
/// delete). The file is the single source; `initial_body_is_header_line_plus_legacy_v4`
/// pins it as `canonical_line(&work_brief_header()) + "\n" + LEGACY_INITIAL_V4_BODY`
/// so neither side can drift alone.
fn initial_body() -> &'static str {
    include_str!("report/default.md")
}

/// #1635 S2a — the default report body exactly as shipped up to and including
/// `7754fd32` (no contract header line). Frozen as a file so the bytes survive
/// squash merges: the header-less compatibility fallback of
/// `report_startup_read_required` (S3) compares against THIS, and the
/// post-S2b default body is pinned as `header line + "\n" + this`.
/// Never edit the file; a change here is a change to what "unwritten" means
/// for every pre-header track in every database.
///
/// The file was generated, not typed, from the fragments `initial_body()`
/// concatenates at that commit — and the pin in `legacy_initial_v4_bytes_are_pinned`
/// is the output of the same command:
///
/// ```sh
/// { cat crates/calm-types/src/track_report_contract_rules.md \
///       crates/calm-types/src/track_report_section_rules.md; \
///   printf -- '-->\n\n# 概要\n\n# 待你定\n\n# 已完成\n\n# 决策\n'; } \
///   > crates/calm-types/src/report/legacy_initial_v4.md
/// sha256sum crates/calm-types/src/report/legacy_initial_v4.md
/// # 6cd893b62424185a842cccc790712a0c1d05151ec84cb6d9d85544f0d2e3f9f3
/// wc -c crates/calm-types/src/report/legacy_initial_v4.md
/// # 2647
/// ```
pub const LEGACY_INITIAL_V4_BODY: &str = include_str!("report/legacy_initial_v4.md");

/// Report-body prefix for the kernel's built-in templates: the canonical
/// work-brief header line (#1635 D2), then writing rules + section rules +
/// the pre-set section notes, **already closed**.
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
            "{}\n{CONTRACT_WRITING_RULES}{CONTRACT_SECTION_RULES}{CONTRACT_PLAN_NOTE}{CONTRACT_CLOSE}",
            canonical_line(&work_brief_header())
        )
    });
    &PREFIX
}

/// Which maintenance contract a template's report leads with (#1571).
///
/// The kernel still does not interpret sections; this only selects which
/// closed comment [`report_contract_prefix`] hands out. A template names its
/// contract here rather than concatenating fragments itself, so unclosed
/// text never leaves this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportContract {
    /// The default work-brief contract plus the pre-set plan note — what
    /// [`report_contract_prefix_for_template`] returns.
    WorkBrief,
    /// The investment-research contract: seven fixed sections, thesis-first
    /// prose, sourced numbers, a 1500—2500 字 budget.
    Research,
}

/// Report-body prefix for a template that names its contract, **already
/// closed**. [`ReportContract::WorkBrief`] is exactly
/// [`report_contract_prefix_for_template`]; [`ReportContract::Research`] is
/// the canonical research header line, then the research rules, closed.
pub fn report_contract_prefix(contract: ReportContract) -> &'static str {
    match contract {
        ReportContract::WorkBrief => report_contract_prefix_for_template(),
        ReportContract::Research => {
            static PREFIX: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
                format!(
                    "{}\n{CONTRACT_RESEARCH_RULES}{CONTRACT_CLOSE}",
                    canonical_line(&research_header())
                )
            });
            &PREFIX
        }
    }
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
    /// The body is a *structural skeleton*: the machine-readable contract
    /// header line (#1635 D2, `<!-- neige:contract … -->`), a maintenance
    /// contract carried in a second, prose HTML comment, then the four
    /// default H1 sections (#1185). Both comments are dropped when the
    /// document is rendered, so users never see them on the page — but they
    /// stay in the body source, which every source-reading subject reads
    /// (the planner agent, a worker's `neige cat report.md`, the REST read
    /// surface, the track's VCS diff). It is layout control, not access
    /// control: never put secrets in it.
    pub fn initial() -> Self {
        Self::new("", initial_body())
    }

    /// #1110 S3 — whether planner's first turn must `calm.report.read`.
    ///
    /// False only when `summary` is empty and `body` is byte-equal to
    /// [`Self::initial()`]'s body or to the frozen pre-header body
    /// [`LEGACY_INITIAL_V4_BODY`] (#1635 S2b: every track minted before the
    /// contract header still carries those bytes and must keep reading as
    /// unwritten — the launchpad empty state depends on it). `doc_rev` /
    /// `blocks` are ignored so a CRDT-materialized placeholder stays false.
    /// Forked or edited content is true. #1635 S3 replaces the byte
    /// comparison with the structural predicate (D3) and keeps both cells.
    pub fn report_startup_read_required(&self) -> bool {
        !(self.summary.is_empty()
            && (self.body == initial_body() || self.body == LEGACY_INITIAL_V4_BODY))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report_contract::{HEADER_OPEN, check_document, normalize_header};
    use std::borrow::Cow;

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
        // #1635 D2: the machine-readable header is line 1, the prose
        // contract comment starts on line 2.
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
        // Exactly one header line, it is line 1, and block 0 does not end
        // inside a comment (#1635 D2 (a)–(e)). `default.md` is pinned byte for
        // byte in `initial_body_is_header_line_plus_legacy_v4`, so a stray
        // `-->` in the file is caught there, not here.
        assert_eq!(
            body.lines().filter(|l| l.starts_with(HEADER_OPEN)).count(),
            1,
            "exactly one contract header line"
        );
        assert_eq!(check_document(&body), Ok(Some(work_brief_header())));
    }

    /// #1185 §1.5 B — the built-in templates must get the same
    /// writing policy, or #1146's guardrails silently vanish on them.
    #[test]
    fn the_template_prefix_is_closed_and_shares_the_default_contract() {
        let prefix = report_contract_prefix_for_template();
        let body = TrackReportPayload::initial().body;

        // #1635 D2: header line first, then the prose contract comment.
        assert!(prefix.starts_with(HEADER_OPEN));
        assert_eq!(
            prefix.lines().next(),
            Some(canonical_line(&work_brief_header()).as_str())
        );
        assert!(
            prefix
                .lines()
                .nth(1)
                .is_some_and(|line| line.starts_with("<!-- 报告维护契约"))
        );
        assert!(
            prefix.ends_with("-->\n\n"),
            "never hand out an unclosed comment"
        );
        // Exactly one header line, on line 1, and the block-0 scan closes.
        assert_eq!(
            prefix
                .lines()
                .filter(|l| l.starts_with(HEADER_OPEN))
                .count(),
            1
        );
        assert_eq!(check_document(prefix), Ok(Some(work_brief_header())));
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

    /// #1571 — the research contract is closed exactly once, names its seven
    /// sections, and shares the preamble the work-brief contract carries.
    #[test]
    fn the_research_prefix_is_closed_and_names_its_seven_sections() {
        let prefix = report_contract_prefix(ReportContract::Research);
        assert_eq!(
            report_contract_prefix(ReportContract::WorkBrief),
            report_contract_prefix_for_template(),
            "WorkBrief must be the existing template prefix, not a third text"
        );

        // #1635 D2: the research header line first, then the research
        // prose contract comment.
        assert!(prefix.starts_with(HEADER_OPEN));
        assert_eq!(
            prefix.lines().next(),
            Some(canonical_line(&research_header()).as_str())
        );
        assert!(
            prefix
                .lines()
                .nth(1)
                .is_some_and(|line| line.starts_with("<!-- 报告维护契约（投研报告版）"))
        );
        assert!(
            prefix.ends_with("-->\n\n"),
            "never hand out an unclosed comment"
        );
        // Exactly one header line, on line 1, and the block-0 scan closes.
        assert_eq!(
            prefix
                .lines()
                .filter(|l| l.starts_with(HEADER_OPEN))
                .count(),
            1
        );
        assert_eq!(check_document(prefix), Ok(Some(research_header())));
        assert!(
            !CONTRACT_RESEARCH_RULES.contains("-->"),
            "the fragment itself must stay unclosed"
        );
        assert!(!prefix.contains('\r'));
        assert_eq!(crate::report_blocks::split_body(prefix).len(), 1);
        assert!(
            !prefix
                .lines()
                .skip(1)
                .any(|l| l.starts_with("# ") || l.starts_with("## ")),
            "a heading inside the contract would split it into two blocks"
        );

        // The seven research H1 names, in the order the skeleton lists them.
        let mut last = 0;
        for section in [
            "结论 ——",
            "待你定 ——",
            "核心逻辑 ——",
            "关键数据 ——",
            "风险与证伪 ——",
            "催化剂与跟踪 ——",
            "来源与边界 ——",
        ] {
            let at = prefix
                .find(section)
                .unwrap_or_else(|| panic!("research contract must describe `{section}`"));
            assert!(
                at > last,
                "section descriptions out of order at `{section}`"
            );
            last = at;
        }
        // Not the work-brief sections — a template that got both lists would
        // carry two contradictory structure rules.
        for foreign in ["概要 ——", "已完成 ——", "决策 ——", "Plan —— 预置计划"]
        {
            assert!(
                !prefix.contains(foreign),
                "research contract must not carry the work-brief section `{foreign}`"
            );
        }

        // Shared preamble + the research genre rules this template exists for.
        let body = TrackReportPayload::initial().body;
        for shared in [
            "不要把秘密写进来",
            "这份报告自带的结构就是规则",
            "写产出，不写过程",
            "章节由下面这份清单定义",
            "没有就省略这个 section",
            "散文正文",
            "不计入",
        ] {
            assert!(
                prefix.contains(shared) && body.contains(shared),
                "both contracts must carry `{shared}`"
            );
        }
        for rule in [
            "投研报告",
            "论点先行",
            "口径、数据日期和来源",
            "1500—2500 字",
            "```neige-block table``` 块",
            "仅作研究，不构成交易建议",
        ] {
            assert!(
                prefix.contains(rule),
                "research contract must carry `{rule}`"
            );
        }
        assert!(
            !prefix.contains("优先用表格"),
            "the 关键数据 table is a requirement, not a preference"
        );
    }

    /// #1635 S2b — the structural pin: today's default body is exactly the
    /// canonical work-brief header line, a newline, and the frozen v4 bytes.
    /// `default.md` is the single source; this holds it to
    /// `work_brief_header()` on one side and `legacy_initial_v4.md` (itself
    /// sha-pinned below) on the other, so neither can drift alone and no
    /// fragment can smuggle a second `-->`. Do not weaken.
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

    /// #1635 S2b — the default body passes the funnel check as-is: one
    /// header, on line 1, canonical, block 0 does not end inside a comment
    /// (line-based scan). Birth
    /// bypasses the funnel (`card.rs` compares the whole payload to
    /// `initial()`), so this is where `initial()` is held to the funnel.
    #[test]
    fn initial_body_passes_the_funnel_check_unchanged() {
        let body = TrackReportPayload::initial().body;
        assert_eq!(check_document(&body), Ok(Some(work_brief_header())));
        assert!(
            matches!(normalize_header(&body), Ok(Cow::Borrowed(_))),
            "the shipped header is already canonical"
        );
        // Block 0 is the contract and nothing else — the S3 predicate.
        let slices = crate::report_blocks::split_body(&body);
        assert!(crate::report_contract::is_pure_comment_block(
            &slices[0].raw
        ));
    }

    /// #1635 S2b review — the first "header ↔ document shape" pin: the H1s
    /// `default.md` ships, in order, are exactly `work_brief_header()`'s
    /// sections. Rename one side and this goes red (the byte pin alone would
    /// not notice a header edit that is mirrored into the file). S3's
    /// structural predicate relies on this correspondence.
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

    /// The module comment's claim, pinned: none of the fragments closes the
    /// comment itself. The fragments still feed `report_contract_prefix`
    /// (`default.md` is a frozen file, not fragment output); a `-->` inside
    /// one would close a template's prose contract early and render its tail,
    /// and nothing else checks the plan-note fragment.
    #[test]
    fn contract_fragments_stay_unclosed() {
        for (name, fragment) in [
            ("contract_rules", CONTRACT_WRITING_RULES),
            ("section_rules", CONTRACT_SECTION_RULES),
            ("plan_note", CONTRACT_PLAN_NOTE),
            ("research_rules", CONTRACT_RESEARCH_RULES),
        ] {
            assert!(!fragment.contains("-->"), "{name} must stay unclosed");
        }
    }

    /// #1635 S2a — the bytes are pinned independently of `initial_body()`, so
    /// a fragment edit that drifts BOTH sides in step still goes red here.
    /// Length, no CR, and the sha256 printed by the generating command in the
    /// constant's doc comment.
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

    /// #1635 S2b — a pre-header track whose body is exactly the frozen bytes
    /// reads as unwritten, exactly as it did before the header existed:
    /// `initial()` grew a header line, the rows in every database did not.
    /// S3 replaces the byte comparison by the structural predicate (D3,
    /// `Ok(None) => body == LEGACY_INITIAL_V4_BODY`) and must keep this cell
    /// green. A non-empty summary or any other byte still reads as written.
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
