## Outcome

Refactor PR #1770 so platform/kernel code owns a single inert presentation contract, while paper-trading owns calculations, risk/approval policies and semantic labels.

## Design

- `calm-types` owns native presentation DTOs, structural JSON Schema and TypeScript declarations. Generate schema/bindings with its existing utoipa and ts-rs dependencies; MCP discovery consumes the crate-owned schema. Frontend shape decoding consumes the generated contract, with independent read-side resource and relational checks. No kernel import of the frontend tree.
- Inline `view` and overlay `view.live` use the same composition and NativeReportView renderer. Live reference contains only source and version; remove overview/activity/cards/details preset dispatch and duplicate rich renderer. Add domain-free bars/meter primitives and text metric values needed to preserve current plugin displays.
- Generic record fields: id, subtitle, title, summary, badges[{label,value,tone}], facts[{label,value}], sections[{label,body}], disclosures[{id,label,body,note,tone}]. No status/handling/evidence workflow requirement or fixed business labels. Empty explicit arrays are valid. Record summary/section/disclosure bodies support 8,000 code points; up to100 records preserves activity history. Badge semantics belong to publishers.
- Snapshot keeps required id, observedAt and producedAt; timestamps may explicitly be null when genuinely unknown. No zero or default backfill. Values support known/unknown/text.
- Keep exact 256 KiB canonical persisted-block admission in kernel unchanged. Live transport remains capped at4 MiB compact UTF-8; full structural presentation validation uses the same DTO validator, without imposing the persisted canonical cap on overlays. Frontend uses a4 MiB decoded JSON resource budget and bounded shape, never imitates serde formatting.
- Existing released table/chart/app/preview contracts, migrations, account isolation and human broker authority remain unchanged. New view contracts exist only on this unmerged PR: explicitly regenerate preview/Recipe/demo data to the unified shape before release; no read-time conversion, alias sniffing or silent fallback. Legacy table overlays remain published/readable.

## Acceptance

- Neutral records with zero, one and three badges render without fabricated workflow fields; long live review bodies remain complete.
- Actual plugin projections and static demo both validate against the same contract and use the same renderer.
- No backend reference to fe/core/domain/report-view.schema.json; real generator + check mode, schema/TS type drift checks and registry golden are current.
- Static/live persisted round trips, exact canonical write-size limits, read-only overlay scope/version/resource fences, table compatibility and malformed presentation rejection are covered at production entry points.
- Focused Rust/Python/frontend tests first; critical fence mutation; text ratchets, quick Rust gates, frontend lint/build/tests/browser, complete two-channel fresh review, CI.
- Update existing PR and isolated preview after verification. Do not merge or touch4140.

Related: #1769, #1595; PR #1770.
