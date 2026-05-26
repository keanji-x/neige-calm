# Spike Day 1 Findings: Plate AI-first editor playground

**Status**: build + types verified locally; runtime verification gated on user hands-on test of `/editor-spike` (per [[feedback-ui-design-preview-before-merge]]).

## 1. Dependency footprint

Installed (pinned semver in `web/package.json`, resolved versions match latest published):

- `platejs` — `^53.0.5` (52 KB unpacked)
- `@platejs/ai` — `^53.0.4`
- `@platejs/basic-nodes` — `^53.0.0`
- `@platejs/code-block` — `^53.0.0`
- `@platejs/link` — `^53.0.3`
- `@platejs/list` — `^53.0.2`
- `@platejs/markdown` — `^53.0.4`
- `@platejs/selection` — `^53.0.0`
- `ai` — `^6.0.191` (7.8 MB — includes Vercel AI SDK core; the bulk is provider adapters we don't load)
- `@ai-sdk/react` — `^3.0.193` (3.3 MB)

Per-namespace footprint: `@platejs/*` = 2.2 MB, `platejs` = 52 KB, `@ai-sdk/*` = 3.3 MB, `ai` = 7.8 MB. Total package-lock entries grew by ~250 net (lockfile total now 795 resolved entries).

Bundle impact: lazy-loaded `EditorSpikePage` chunk = **881 KB** raw / **272 KB** gzip. This is the spike-route-only chunk — main app bundle is unchanged (`index-*.js` = 670 KB, same as before). Confirms route-level code-splitting works; Plate doesn't leak into the home-page tree.

**Note**: codex initially pinned all `@platejs/*` packages at `^53.0.5`, which fails npm resolution because the sub-packages are not synchronized — `@platejs/ai` is on `53.0.4`, `@platejs/basic-nodes` is on `53.0.0`, etc. The actual install was driven from the main shell (codex's sandbox blocked `127.0.0.1:10809` localhost proxy needed for the registry mirror) using `npm install --save` to let npm pick correct semver. Real lesson: don't hand-pin Plate sub-package versions; let npm resolve.

## 2. Streaming partial-AST (goal a)

API used: `streamInsertChunk(editor, chunk, { textProps })` from `@platejs/ai/react`, paired with manual `AIChatPlugin` internal state management (`streaming`, `_blockChunks`, `_blockPath`, `_mdxName`) on enter/exit.

Wiring is build-clean — TS types match, no Plate API misuse caught by the compiler. The spike route streams 10 canned markdown chunks at 220 ms intervals (mix of token-sized fragments and syntax-boundary chunks for bold, lists, fenced code).

Verdict: **PASS — build/types**. Runtime behavior (flicker, focus loss, malformed-intermediate-MD rendering during fence streaming) is gated on user click-through.

## 3. Accept/reject preview (goal b)

Transforms used:
- `editor.getTransforms(BaseAIPlugin).ai.beginPreview({ originalBlocks })` — stash rollback
- `editor.getTransforms(BaseAIPlugin).ai.acceptPreview()` — commit
- `editor.getTransforms(BaseAIPlugin).ai.cancelPreview()` — revert

**Important Plate v53 API correction**: `cancelPreview()` is the reject path (restores rollback content); `discardPreview()` only clears preview bookkeeping while leaving content as-is. The issue context originally wrote "discardPreview" — corrected here for downstream usage.

Spike UX: top toolbar has `Preview rewrite` → replaces the first paragraph with an AI-marked block; `Accept` commits; `Reject` restores.

Verdict: **PASS — build/types**. Rollback round-trip (type X → preview Y → reject → undo) needs runtime confirmation.

## 4. Markdown round-trip (goal c)

API:
- `editor.getApi(MarkdownPlugin).markdown.deserialize(input)` — MD → Plate value
- `editor.getApi(MarkdownPlugin).markdown.serialize()` — Plate value → MD

The spike route has an Import MD / Serialize MD panel + an auto-generated round-trip table that probes the serialized output for expected fragments (paragraph, H1–H3, bullet/ordered lists, fenced code with lang, inline code, link, bold, italic, mixed-list-with-code-inside). The table fills in real PASS/PARTIAL verdicts at runtime — the user can paste in their own sample wave-report content and inspect.

Verdict: **PASS — build/types; runtime verification is the value here**. Per Plate v53 docs the list plugin uses flat indentation metadata rather than classic nested list nodes — nested-list + embedded code block is the highest-risk row.

## 5. Human editing sanity (goal d)

`PlateContent` is the editor surface; same `editor` instance accepts AI streaming, markdown import, and manual typing. Toolbar `Undo` / `Redo` wire to `editor.tf.undo()` / `editor.tf.redo()`. Bold/italic plugins registered for keyboard shortcuts.

Verdict: **PASS — build/types**. Runtime confirmation that typing + format shortcuts + undo all work normally on the AI-modified value is the user's preview-gate job.

## 6. Friction / surprises

- **`@platejs/*` sub-package version skew** — they are not on synchronized releases (`@platejs/ai` 53.0.4, `@platejs/basic-nodes` 53.0.0, etc). Hand-pinning identical versions across the namespace fails. Use `npm install --save` once and let npm resolve.
- **Plate v53 AI preview API** — `cancelPreview()` (not `discardPreview()`) is the reject path. Issue #330 originally documented `discardPreview` — that's wrong.
- **`useState` import** — the spike uses `web/src/shared/state`'s persistence-wrapped `useState`, not React's. This is the project convention; codex picked it up automatically. Fine for the spike but production editor state will need to opt out of persistence for transient UI.
- **Sandbox network isolation** — codex's exec sandbox cannot reach `127.0.0.1:10809` (the localhost npm proxy). For dependency installs in future codex-driven worktrees, install from the parent shell first, then re-launch codex for the verify/commit step. Documented for [[feedback-worktree-isolation]] flow.
- **Bundle size** — lazy-loaded spike chunk is 881 KB (272 KB gzip). For a future production WaveReportEditor we should consider trimming `@ai-sdk/*` if we don't use it (the `ai` package is the bulk at 7.8 MB unpacked).
- **No backend wiring yet** — `MarkdownPlugin.markdown.serialize()` is sync; `tf.ai.*` preview transforms are local. The MCP-tool surface + WS push-AI-edits-into-editor plumbing is the Spike Day 2 work.

## 7. Recommendation

**Provisional GO for Plate adoption — pending user runtime verification.**

The four target capabilities all have a clean Plate v53 API surface, all imports + types check, lint + build pass with the spike route lazy-loaded. The hard part shifts from "can Plate do this?" (answer looks like yes) to "what shims do we own?":

1. **Stable block IDs** across streaming + markdown import + preview accept/reject. Plate's `serialize({ withBlockId: true })` is the hook; we'll need a normalizer plugin that assigns + persists IDs on AST nodes that don't have them.
2. **AI as CRDT peer** — the MCP tool surface + WS-relay → editor `tf.*` op dispatcher (Day 2 scope).
3. **Markdown serializer fidelity** for our actual wave-report corpus — the spike's auto-table will catch the easy gaps; need a real-content pass next.

Spike Day 2-3 scope, ordered by risk:

- **Day 2a** — runtime verify spike (user clicks through `/editor-spike`, fills round-trip table for our wave-report samples, identifies elements that need a custom serializer).
- **Day 2b** — stable block-id middleware proof-of-concept; AST-with-IDs round-trip through markdown.
- **Day 3** — WS → `tf.*` op dispatcher prototype on a stub MCP tool, prove the "AI as CRDT peer" model with a real round-trip.

If user runtime verification surfaces blocker-level issues (e.g., streaming flicker is too severe, markdown round-trip loses code-block languages, preview reject doesn't restore correctly), we revisit the Plate decision before committing to Day 2+.
