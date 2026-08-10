# Per-page information hierarchy — `/`, `/cove/$coveId`, `/wave/$waveId`, `/settings`

**Status**: proposal, round 1
**Scope**: `fe/web/src/{features,app}/**` on `design/fe-rewrite-architecture` (worktree `997-c1-today`)
**Governing spec**: `docs/_fe-design-system.md` — rule IDs (`DS-*`), roles (`T-01`…`T-21`), tiers and TCR numbering are cited, not re-derived.
**Evidence**: `docs/_fe-design-current-audit.md` (measured on the running build), `docs/_fe-design-legacy-extract.md` (measured on the legacy app), `fe/web/src/styles/tokens.css` (FROZEN).
**Token discipline**: every size/weight/tone below names a token. Tokens that do not exist are filed in §C-5, never used silently. Weight literals are written `--weight-*` and depend on **TCR-001**.

The problem this document solves, stated in measured terms:

| Symptom | Measurement | Source |
|---|---|---|
| Nothing reads as more important than anything else | `font-weight: 600` appears **2×** in 1511 lines of module CSS; everything else inherits 400 from the UA | audit §7.3, DS-ANTI-001 |
| The page has no visible structure | rail↔main background contrast **1.02:1** (light) / **1.01:1** (dark); card↔page **1.09:1** | audit §6.2 |
| The loudest element is the least actionable | Today clock renders at **36px**, 2.8× the 13px base and the only `--text-display` on the page | audit §7.1, `today.module.css` |
| Two thirds of the viewport is dead | main-column whitespace **66% / 86% / 79% / 54%** on Today / Cove / Wave / Settings; content bottom edge at y=317 / 135 / 198 / 425 of 900 | audit §8 |
| Developer scaffolding renders as UI | `today .placeholderBody` measures **748px wide @ 12.5px mono ≈ 100+ chars/line**; `wave .cardNote` = `"Card runtime lands in a later slice."` at `--text-4` (**1.91:1**) | audit §7.5, §6.1 |

---

## C-0. How to read the hierarchy tables

| Tier | Meaning | Budget | Enforced by |
|---|---|---|---|
| **P0** | The one thing the page is about | exactly one per page | DS-HIER-001 (checkable core), review |
| **P1** | Directly supports the P0 decision | a handful | DS-PRIN-002 |
| **P2** | Context — scannable, not read | — | DS-TYPE-007 |
| **P3** | Metadata — present but recessive | — | §8.2 tone ramp |
| **—** | Drop, defer, or move | — | — |

**A necessary clarification to DS-HIER-001.** The rule says the surface's primary emphasis is "the element with the largest `--text-*`". On all four of these pages the largest text is the **page title**, and the page title is *chrome*, not the P0. Three of the four pages are list-driven: their P0 is a *region* (a list of rows), and DS-HIER-005 forbids size from carrying hierarchy inside a row. So:

> **P0 is designated by position (C4) + density + being the only place semantic colour appears on the page.** The page title satisfies DS-HIER-001 mechanically (one element at the top size) and is deliberately identical on all four pages so that it recedes into the frame instead of competing. DS-HIER-001 remains necessary — a *second* 18px element would be a defect — but it is not sufficient, and no checker should be read as certifying that a page has hierarchy.

This is the single most important sentence in this document. Everything else follows from it.

**Channel budget applied here.** Of the eight channels (DS §2.1) this design spends, per page: **C4 position** on everything (it is free — DS-HIER-008); **C2 weight + C3 tone** inside every row and header bar (DS-HIER-005 forbids C1 there); **C1 size** exactly once per page, on the title; **C8 accent/semantic** on at most one element plus the selected row (DS-COLOR-010). C6 surface separates regions only (DS-HIER-007); C7 border never carries importance (DS-HIER-006).

**Distinct chrome sizes per page: 3** — `--text-xs` (11), `--text-sm` (12.5), `--text-lg` (18) — against the cap of 4 (DS-TYPE-010). `--text-base` (13) appears only in prose/terminal content, which DS-TYPE-010 excludes. `--text-md` is banned in chrome (DS-TYPE-008). `--text-xl`+ is discussed in §C-6/A-1.

---

# 1. `/` — Today

## A. The job

> A user opens Today to find out **whether any wave needs them right now**, and to re-enter whatever they were last working on.

**The question the page must answer in two seconds:** *Is anything waiting on me?* — and if not, *what moved while I was away?*

The current build answers a different question — *what time is it* — with the largest, brightest element on the page.

## B. Information inventory

| Item | Currently | Belongs |
|---|---|---|
| Wall clock (h:mm + AM/PM) | `--text-display` 36px, 3 elements | yes, demoted |
| Weekday name | `--text-lg` 18px | yes, promoted to page title |
| Running count | 12.5px + 8px dot | yes |
| Waiting count | 12.5px + 8px dot | yes |
| **Waves waiting on the user** | **absent from the main column** (rail only) | **yes — this is the P0** |
| **Waves running now** | absent | yes |
| **Recently touched waves** | absent | yes |
| Week calendar strip (7 days, per-day dots) | present, right column | yes |
| Selected-day agenda (wave rows) | present, right column | yes |
| Month/year label + week nav | present | yes |
| Today terminal card | **scaffolding paragraph** | region reserved, unbuilt treatment |
| `"The default Today terminal lands with features/today/terminal…"` | 748px of mono prose | **REMOVE** |
| `"Nothing scheduled."` empty text | `--text-3` | yes |
| Date of each agenda row's lifecycle | `--text-4` (1.86–2.07:1) | yes, at `--text-3` |
| Cove colour bar on agenda rows | 3px grid column | yes, as a 6px dot |
| Per-day dots ×4 | 5px (a third dot size) | yes, at `--dot-sm` |

## C. Hierarchy

| Tier | Item | Size | Weight | Tone | Position | Reason |
|---|---|---|---|---|---|---|
| **P0** | **"Waiting on you" wave list** (rows) | — | — | — | primary column, first, directly under the header | The one thing the page is about. Carried by position + being the only `--warn` pixels on the page (DS-COLOR-012), **not** by size. Section renders only when non-empty. |
| P1 | Wave row title (inside P0) | `--text-sm` | `--weight-medium` | `--text` | row line 1, start | T-10. Weight, not size — DS-HIER-005. |
| P1 | Attention glyph on a waiting row | `--dot-sm` | — | `--warn` | row, leading 6px column | DS-COLOR-009: colour is never the sole carrier — the title also takes `--warn-text` (TCR-005). Two channels, both non-size → legal under DS-HIER-002. |
| P1 | "Running now" wave list | — | — | — | primary column, second section | Same row spec; glyph `--accent` + `--motion-pulse` (the app's only loop, DS-MOT-005). |
| P1 | Waiting / running counters | `--text-sm` | `--weight-medium` | `--text-2` | header, title line, after the title | Two numbers that summarise the two P0 sections. `.tnum` (DS-NUM-005). Numeral is medium; the word "waiting" stays 400/`--text-3`. |
| P1 | Selected-day agenda rows | `--text-sm` | `--weight-normal` | `--text` | panel column, below the calendar | Same row atom at `--row-h` (single line) — the panel is narrow, so the meta line is dropped, not shrunk. |
| P2 | Page title — weekday + date (`Monday, 10 Aug`) | `--text-lg` | `--weight-semibold` | `--text` | header line 1, start | T-03. The page's only 18px element (DS-HIER-001). Same treatment on all four pages — see §C-3. |
| P2 | Week calendar strip | `--text-sm` (number) / `--text-xs` (day letter) | `--weight-normal` / `--weight-medium` on today | `--text-2` / `--text-3` | panel column, top | Navigation, not content. Today's cell is the panel's single selected element (`--accent-soft` + `--accent` border). |
| P2 | "Recent waves" list | — | — | — | primary column, last, fills remaining height | DS-LAY-010: a page is never only a header. There is always history. Rows are `--row-h` compact, no progress track. |
| P2 | Today terminal region | — | — | — | primary column, between "Running" and "Recent" | Reserved at real geometry; unbuilt treatment **U-1** (§C-1.5) until the card runtime lands. |
| **P3** | **Clock** (`4:05 PM`) | `--text-sm` | `--weight-normal` | `--text-3` | header, title line, pushed to the inline-end edge | **Demoted from `--text-display` 36px.** It is ambient, not actionable. `--font-numeric` + `tabular-nums` (DS-NUM-001) so the minute tick causes no jitter. Position (C4) is its whole signal — it costs zero size, weight or colour. See §C-6 A-1 for the DS-TYPE-009 amendment this requires. |
| P3 | Section labels ("WAITING ON YOU", "RUNNING", "RECENT") | `--text-xs` | `--weight-semibold` | `--text-3` | above each section | T-09, uppercase + `--tracking-wider` (DS-TYPE-004). Weight + tracking + caps substitute for size (DS-TYPE-006). |
| P3 | Month + year label | `--text-xs` | `--weight-medium` | `--text-2` | panel, calendar head, centre | |
| P3 | Agenda row lifecycle phrase | `--text-xs` | `--weight-normal` | `--text-3` | agenda row, line 2 | Was `--text-4` at 1.86:1 (audit §6.1). DS-COLOR-001. |
| P3 | Cove identity dot on agenda rows | `--dot-sm` | — | cove colour | row, leading | Identity, never state (DS-COLOR-015). Replaces the 3px bar. |
| P3 | Per-day dots in the calendar | `--dot-sm` | — | cove colour | day cell, block-end | Was 5px — the third of five dot sizes (DS-ANTI-017). |
| — | Scaffolding paragraph about `features/today/terminal` | — | — | — | — | **Delete.** Developer prose is not UI (§C-6 A-4). |
| — | `AM`/`PM` as a separately styled 12.5px/600/0.08em element | — | — | — | — | Fold into the clock string. It was a third clock element earning its own type spec. |

**Channel audit — the clock.** It moves from {C1 size 36px + C3 tone `--text` + C4 first position} to {C4 position only}. Nothing else on the page gains a channel to compensate; per DS-HIER-009 the correct move is downward.

**Channel audit — any element stacking ≥3 channels.** The only one is the *waiting* wave row: C2 weight (500), C3/C8 tone (`--warn-text` title), C8 colour (`--warn` glyph). Three channels, no size, no border → satisfies DS-HIER-003's checkable core and is justified because it is the P0 and must be findable pre-attentively from across the room. It is also the only element on the page permitted `--warn`.

## D. Layout — 1440×900

```
0        200                                    1440
├─ rail ─┼──────────────── main (1240) ──────────────┤
│        │←24 page padding→│                 │←24→│
│        │                 │
│        │  ┌ content 1180 (start-aligned, --measure-page) ─────────────┐
│  200px │  │ primary column 848             │ 24 │ panel 308 (--panel-w)
│  --rail│  ├────────────────────────────────┤    ├─────────────────────┤
│  -w    │  │ Monday, 10 Aug   2 waiting · 1 running        4:05 PM  ⟵ header, y=20..48
│        │  ├────────────────────────────────────────────────────────────┤ (hairline on scroll)
│  ┌───┐ │  │                                     │    │  ‹  August 2026  › │
│  │Wait│ │  │ WAITING ON YOU              (T-09)  │    │  M  T  W  T  F  S  S│  28px
│  │ing │ │  │ ●  引用方：本轮修复      blocked 2h  │    │ [10] 11 12 13 14 15 │  48px cells
│  │ ▪ │ │  │    双链演示 · awaiting your review   │    │  ··                 │
│  └───┘ │  │ ────────────────────────────── 48px  │    │                     │
│  COVES │  │ ●  被引用方：估值结论    blocked 40m  │    │  TODAY        (T-09)│
│   ▸ ▪  │  │    双链演示 · needs a decision       │    │  ▪ 引用方：本轮修复  │ 28px
│   ▸ ▪  │  │                                     │    │    draft            │
│        │  │ RUNNING                     (T-09)  │    │  ▪ 被引用方：估值结论 │ 28px
│        │  │ ◉  <none — section not rendered>    │    │    draft            │
│        │  │                                     │    │                     │
│        │  │ ┌ ~ / neige · today ──────────────┐ │    │                     │
│        │  │ │                                 │ │    │                     │
│        │  │ │   Terminal is not wired up yet. │ │    │  (panel ends; no     │
│        │  │ │        (dashed, --text-3)       │ │    │   filler below)      │
│        │  │ │                            240px│ │    │                     │
│        │  │ └─────────────────────────────────┘ │    │                     │
│        │  │                                     │    │                     │
│        │  │ RECENT                      (T-09)  │    │                     │
│        │  │ ▪ …                            28px │    │                     │
│        │  │ ▪ …                            28px │    │                     │
│        │  │ ▪ …  (fills to the block-end edge)  │    │                     │
│  ┌──┐  │  └────────────────────────────────────┘    └─────────────────────┘
│  │YO│  │                                                          ↓ 28px bottom padding
└────────┴───────────────────────────────────────────────────────────────────┘
```

**Where the whitespace goes.** Not into a 764px void at the bottom. The vertical order is *decisions first, ambient last*: the two attention sections consume as much height as they need, the terminal takes a fixed 240px slot, and **"Recent" absorbs all remaining height** — which is why the page is never only a header (DS-LAY-010) and never shows a contiguous empty rectangle taller than 240px inside the primary column (DS-LAY-007, as scoped in §C-1.6).

Horizontal slack (1192 usable − 1180 content = 12px, plus whatever the 848 column does not fill) is a **gutter**, not a void. The 848/24/308 split is the same on Today, Cove and Wave — see §C-1.

## E. States

| State | Treatment |
|---|---|
| **Loading** (< 200ms) | Render nothing. No spinner, no flash (DS-LOAD-002). |
| **Loading** (> 200ms) | The header renders immediately (title + clock need no fetch). Each section shows one line of T-14 `--text-3` in place: `Loading waves…` (DS-LOAD-003). |
| **Refetch** | Stale rows stay on screen, unmoved; no skeleton, no fade (DS-LOAD-004, DS-MOT-004). |
| **Empty — nothing waiting** | The "WAITING ON YOU" section **is not rendered at all** — no label, no dashed box. Absence is the message. "RUNNING" likewise. The page then opens on "RECENT", which is never empty in a workspace that has ever been used. |
| **Empty — brand-new workspace** (no coves, no waves) | Page empty state (DS §11.13 "Page" variant): one T-02 line at `--text-2` — `Nothing here yet.` — plus **one primary action**, `New cove`, and the rail's inline cove-creation input already focused (DS-LAY-008, §12.4). No illustration (DS-EMPTY-003). |
| **Empty — agenda** | Inline variant inside the panel: one T-11 line, `--text-3`, in a `--row-h` box with `1px dashed var(--hairline)` (DS-EMPTY-002) — `Nothing today.` The panel does **not** collapse, because the calendar above it is never empty. |
| **Error** | Region-scoped, never a page banner: 6px `--error` dot + T-14 message at `--error-text` + tertiary `Retry`, in a `--warn-soft` box (DS §11.15). The calendar failing does not blank the wave sections. |
| **Not built yet** — terminal | Treatment **U-1** (§C-1.5): the region at its real geometry, `1px dashed var(--hairline)`, no fill, one centred line at T-14 `--text-3`: **`Terminal is not wired up yet.`** Nothing else. No module path, no resolve order, no README reference, no slice name. |

## F. What Today deliberately does NOT show

| Not shown | Lives instead |
|---|---|
| The full cove/wave tree | The rail — it is on every page (§C-2). Duplicating it on Today would make the rail decorative. |
| Wave `cwd`, ids, branches | The wave page's identity line (T-16, mono). |
| A lifecycle pill on every row | 6px dot + tone only. A coloured pill per row makes the list permanently multicolour and destroys "colour = attention" (DS-PRIN-005; explicitly rejected from Linear, DS §17). |
| Month view | The week strip only. A month grid is a navigation surface for a scheduling feature that does not exist. |
| Per-cove statistics, charts, streaks | Nowhere. This is a workbench, not a dashboard (DS-PRIN-001). |
| Seconds on the clock | Removed. It is the third-hand of a wall clock in an app the user re-enters hundreds of times a day (DS-PRIN-003). Minute resolution, tabular. |

---

# 2. `/cove/$coveId` — one cove

## A. The job

> A user opens a cove to **pick the wave they need**, or to **start a new one**.

**Two-second question:** *Which of this cove's waves is moving, which is stuck, and where do I add one?*

## B. Information inventory

| Item | Currently | Belongs |
|---|---|---|
| Cove identity swatch | 12px (vs 8px in the rail, 6px in a wave row — audit §4 C1) | yes, at `--dot-md` |
| Cove name (editable) | `--text-xl` 22px | yes, at `--text-lg` |
| Wave count (`2 waves`) | 12.5px, beside the title | yes, as trailing metadata |
| `+ New wave` button | `--accent-soft` fill (= the selection colour, DS-ANTI-006), hover turns it grey (DS-ANTI-005) | yes, as the page's single primary action |
| `Delete` cove | byte-identical to `+ New wave` except two colours (DS-ANTI-004) | yes, demoted to destructive-at-rest-is-tertiary |
| Wave rows (title, lifecycle) | 1128px wide, lifecycle pushed to x=1379 by `margin-inline-start:auto` — **~950px of nothing between them** (audit §8.2) | yes, at `--measure-list` |
| Wave activity (`now`, `eta`, `progress`) | **not rendered** — the fields exist on `Wave` (`core/domain/wave.ts`) and are dropped | **yes** — this is what makes a row scannable |
| Pin / remove row actions | hover-revealed, 22px | yes, at `--control-h-sm` |
| Cove `cwd` default, created date | not rendered | yes, panel |
| Lifecycle breakdown (n draft / n working / n blocked) | not rendered | yes, panel `[assumption]` |
| Empty list state | dashed box + a button elsewhere | **replaced by the composer itself** |
| Cove colour dot inside each wave row | present (`row.module.css:104`) | **REMOVE** — every row in this list has the same cove |

## C. Hierarchy

| Tier | Item | Size | Weight | Tone | Position | Reason |
|---|---|---|---|---|---|---|
| **P0** | **The wave list** | — | — | — | primary column, immediately under the header, capped at `--measure-list` | The page is a list. Everything else is a label for it. |
| P1 | Wave row title | `--text-sm` | `--weight-medium` | `--text` | row line 1, start | T-10. |
| P1 | Wave row activity line (`now` text, or the cove-relative phrase) | `--text-xs` | `--weight-normal` | `--text-3` | row line 2, start | T-11. This is the field the current build drops; without it a two-line row is not worth 48px. |
| P1 | Status glyph | `--dot-sm` | — | `--warn` waiting / `--accent` running / `--text-4` otherwise | row, leading 6px column | `--text-4` is legal here: a dot renders no text (DS-COLOR-001). |
| P1 | `+ New wave` | `--text-sm` | `--weight-normal` | `--text-on-accent` (TCR-007) | header, inline-end of the title line | The page's **one** primary action (DS-ACT-001), `data-action="primary"`, solid `--accent` fill — the only solid accent in the app (DS-ACT-003). |
| P2 | Cove name (page title, editable) | `--text-lg` | `--weight-semibold` | `--text` | header line 1, start | T-03, `--tracking-tight`. Down from `--text-xl` 22px: 22/13 = 1.7× reads as a landing page; 18/13 = 1.38× is the dense-tool ratio (DS §3.4). |
| P2 | Lifecycle label per row | `--text-xs` | `--weight-normal` | `--text-3`, or `--warn-text` when waiting | row, inline-end of line 1 | T-15. Right edge = status (C4). Was `--text-4` at 2.07:1. |
| P2 | Progress hairline | 3px | — | `--accent` on `--surface-chip` | row, block-end edge, full-bleed | Steps; never animates its width (DS-MOT-003). |
| P3 | Cove identity swatch | `--dot-md` | — | cove colour | header, before the title | One size everywhere (§C-3). |
| P3 | Wave count (`2 waves`) | `--text-xs` | `--weight-normal` | `--text-3` | header, after the title, before the spacer | T-15, `.tnum`. It is a count, not a heading (DS-NUM-003). |
| P3 | `Delete` cove | `--text-sm` | `--weight-normal` | `--text-2` | header, inline-end, **after `--space-6` of separation** from `+ New wave` (DS-ACT-008) | `data-action="destructive"`. **No colour at rest** (DS-ACT-006) — red appears only on hover/focus. |
| P3 | Pin / remove per row | `--control-h-sm` | — | `--text-3` | row, inline-end, hover-revealed with space reserved | DS-ACT-012; visible on `:focus-within` (DS-FOCUS-007); a pinned row's pin stays at opacity 1 forever (DS-ACT-013 / INV-SIDEBAR-012). |
| P3 | Cove `cwd`, created, lifecycle counts | `--text-xs` | 400 / 500 for the numeral | `--text-3` | panel column | Context you consult, not scan. |
| — | Cove colour dot inside each row | — | — | — | — | **Delete on this page.** Redundant identity ×N rows; the header swatch already says which cove this is. (`compact` rail variant keeps it — there the rows span coves.) |
| — | Breadcrumb line | — | — | — | — | **Not rendered.** A cove has no domain ancestor; the rail shows where it sits. See §C-3 for the rule. |

**Channel audit.** `+ New wave` is the one element on the page stacking size-neutral + weight-neutral + **fill** + **border** — i.e. C8 + C7. That is 2 channels, legal under DS-HIER-002, and DS-HIER-006 is not violated because the border here is co-extensive with the fill (it is the button's edge, not an importance marker). Nothing else on the page carries a fill.

## D. Layout — 1440×900

```
0        200                                                            1440
├─ rail ─┼───────────────────── main (1240) ────────────────────────────┤
│        │←24→│                                                    │←24→│
│        │  ┌ content 1180 ──────────────────────────────────────────┐
│        │  │ primary column 848              │ 24 │ panel 308        │
│        │  ├─────────────────────────────────┤    ├──────────────────┤
│        │  │ ▪ 双链演示   2 waves     [+ New wave]   Delete     ⟵ header line 1 (28px)
│        │  │ /tmp/demo-b                          (T-16, mono)  ⟵ identity line
│        │  ├──────────────────────────────────────────────────────────┤
│        │  │ ┌ list, --measure-list 720 ─────┐ │    │ COVE      (T-09)│
│        │  │ │ ● 被引用方：估值结论  blocked │ │    │ waves        2  │ 28px
│        │  │ │   waiting on your decision·40m│ │48px│ working      0  │ 28px
│        │  │ │ ▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁ progress│ │    │ blocked      2  │ 28px
│        │  │ ├───────────────────────────────┤ │    │                 │
│        │  │ │ ● 引用方：本轮修复      draft │ │48px│ CWD       (T-09)│
│        │  │ │   created 10d ago             │ │    │ /tmp/demo-b     │
│        │  │ └───────────────────────────────┘ │    │                 │
│        │  │  ↑ 720px measure — the lifecycle  │    │ (panel ends)    │
│        │  │    label lands 720px from the     │    │                 │
│        │  │    title, not 950px               │    │                 │
│        │  │                                   │    │                 │
│        │  │ (list fills; no filler below)     │    │                 │
│        │  └───────────────────────────────────┘    └─────────────────┘
└────────┴──────────────────────────────────────────────────────────────┘
```

**The 950px hole, fixed by a number.** The row is capped at `--measure-list` **720px** (TCR-010, measured from legacy `.col.wide` — the legacy cove screenshot shows the wave row spanning x=492→1147 = **655px**, inside a 720px column). The current build's row is **1128px**. That single cap is what removes the "eyes have to sweep the whole screen to read one line" defect (audit §8.2).

**Whitespace.** The primary column is 848 but the list is 720 — the 128px trailing strip is where the hover-revealed row actions live and where a title that ellipsises at 720 has somewhere to *not* go. Vertically the list starts at the top and stops; below it is the page's block-end padding. It is legitimate for a 2-wave cove to be short — but the panel column gives the page a second vertical anchor so it does not read as one band pinned to the top-left corner (the current 86% whitespace figure).

## E. States

| State | Treatment |
|---|---|
| **Loading** | Header renders from the rail's cached cove; list shows one T-14 `--text-3` line, `Loading waves…`. |
| **Empty — no waves** | **The new-wave composer, already expanded, focused, at the first row's position and size** (DS-LAY-008, DS §12.4). Not a dashed box plus a button in the header. Creating a wave is the only thing you can do here; making the user click to reveal the form wastes a step *and* a screen. The `+ New wave` header button remains (it is how you add a second one) but is not the empty state. |
| **Error — cove load failed** | Page-level, because the whole page failed: `--warn-soft` box at `--measure-list`, T-14 `--error-text`, tertiary `Retry`. |
| **Error — a mutation failed** (rename, delete) | Inline beside the control that failed; the confirm dialog stays mounted with Cancel live (INV-CONFIRM-001, already correct in `page/public.tsx`). |
| **Not built yet** | Nothing on this page is unbuilt. The panel's lifecycle breakdown is new work, not a placeholder — if it is deferred, the panel column is simply **not rendered** and the primary column keeps its 848 width. A deferred region is absent, never a labelled empty box. |

## F. What the cove page deliberately does NOT show

| Not shown | Lives instead |
|---|---|
| Wave cards / terminals | The wave page. A cove is an index. |
| The cove colour on rows, headers or backgrounds | The 8px swatch only (DS-COLOR-015). Tinting the page turns identity into state. |
| Archived waves | Out of scope; when it lands it is a filter on this list, not a second list. |
| A per-wave "open" button | The row is the target (DS-ACT-011: never make a row's own action an icon). |
| Cove settings / renaming as a dialog | The title is edited in place (`ui/editable-title`), already correct. |

---

# 3. `/wave/$waveId` — one wave

## A. The job

> A user opens a wave to **see what the agent has done and is doing**, and to **unblock it**.

**Two-second question:** *What state is this wave in, and does it want something from me?*

This is the page where the product actually lives, and it is the page the rewrite has least of. The card runtime is a later slice. That is a fact to be *rendered honestly*, not narrated in body text.

## B. Information inventory

| Item | Currently | Belongs |
|---|---|---|
| Back control | 24px `←` box | yes, `--control-h` icon button |
| Breadcrumb `Today / cove` | `--text-xs`, separator at `--text-4` (2.07:1) | yes |
| Wave title (editable) | `--text-xl` 22px | yes, at `--text-lg` |
| Lifecycle badge | `--warn` on `--warn-soft` = **4.01:1** (DS-ANTI-016) | yes, with `--warn-text` (TCR-005) |
| `cwd` | `--text-4` mono, 2.07:1 | yes, at `--text-3` |
| `Delete` wave | 11px, hover changes only the border | yes, unified with cove's Delete |
| `Cards` section label | `--text-sm`/600/not-uppercase — a third section-label spec (audit §4 C8) | yes, T-09 |
| Card rows (kind, title, `kernel-owned`) | 1128×28px rows with the note pushed to the far edge | yes, in the panel at `--row-h` |
| `"Card runtime lands in a later slice."` ×N rows | `--text-4` on `--surface-card` = **1.91:1**, repeated per card | **REMOVE (all N copies)** |
| The report document | absent | **yes — this is the P0 slot** |
| Event line / activity | absent | yes, panel |
| Files | absent | deferred; legacy had it (`legacy/wave-light.png`) |
| Wave activity (`progress`, `eta`, `now`) | absent | yes, header identity line |

## C. Hierarchy

| Tier | Item | Size | Weight | Tone | Position | Reason |
|---|---|---|---|---|---|---|
| **P0** | **The wave body** — the report document when one exists, otherwise the card board | prose: T-04…T-07 | — | `--text` | primary column, `--measure-prose` 616 inside a 748 container | This is what the wave *is*. Until the card runtime lands the slot renders unbuilt treatment **U-1** at its real geometry, so the document's shape is visible before it has content (DS §12.4). |
| P1 | Wave title (editable) | `--text-lg` | `--weight-semibold` | `--text` | header line 2, start | T-03, `--tracking-tight`. The page's only 18px element. |
| P1 | Lifecycle badge | `--text-xs` | `--weight-medium` | per state, `--warn-text` for attention | header line 2, immediately after the title | T-17. Status is a *different kind* of thing, carried by shape (pill + 6px dot) + semantic tone, never size — which is why it can sit beside an 18px title without competing (DS §3.4). This is the page's single accent-filled element (DS-COLOR-010). |
| P1 | Activity line (`now` · `eta` · progress %) | `--text-xs` | `--weight-normal` | `--text-3`, numerals `.tnum` | header line 3, after the cwd | The answer to "is it moving". Currently not rendered at all despite existing on `Wave`. |
| P1 | Card inventory rows | `--text-xs` | `--weight-normal` | `--text` | panel column, `--row-h` 28px | Moved out of the 1128px primary column. A card is an object you open, not a paragraph. |
| P2 | Breadcrumb ancestors (`Today`) | `--text-xs` | `--weight-normal` | `--text-3` | header line 1, start | T-19. |
| P2 | Breadcrumb current (cove name + dot) | `--text-xs` | `--weight-medium` | `--text-2` | header line 1, after `/` | T-20. "You are here" gets exactly one notch — weight, not size (DS-HIER-005). |
| P2 | Event line / recent activity | `--text-xs` | `--weight-normal` | `--text-3` | panel column, below the cards | The panel's second section. Live-updating text; never animates in (DS-MOT-004). |
| P3 | Back control | `--control-h` | — | `--text-3` | header line 1, leading | A gesture, not content. Leading position is its whole signal. |
| P3 | `cwd` | `--text-xs` | `--weight-normal` | `--text-3` | header line 3, start | T-16, `--font-mono`. The *font family* announces "literal string" — zero size/weight/colour spend. |
| P3 | `kernel-owned` | `--text-xs` | `--weight-normal` | `--text-3` | card row, inline-end | T-15. Was `--text-4` (1.91:1). |
| P3 | `Delete` wave | `--text-sm` | `--weight-normal` | `--text-2` | header line 2, far inline-end, past `--space-6` | Identical to the cove page's Delete (§C-3). Tertiary at rest. |
| P3 | Section labels (`CARDS`, `ACTIVITY`) | `--text-xs` | `--weight-semibold` | `--text-3` | above each panel section | T-09. |
| — | `"Card runtime lands in a later slice."` × every card | — | — | — | — | **Delete all copies.** One unbuilt region says it once, in the P0 slot, at U-1. Repeating it per row turns a project status into page furniture. |
| — | Card `kind` rendered twice (mono chip + title fallback) | — | — | — | — | Collapse to one: the mono `kind` is the identity, the title is the label; when `title` is absent, show the kind once. |

**Channel audit — the lifecycle badge.** It stacks C8 fill (`--warn-soft`) + C7 border (`--warn-border`) + C3/C8 text tone (`--warn-text`) + shape (pill + dot). That is the most decorated element in the entire design, and it is justified explicitly: it is the **only** element on the page whose whole job is to be read pre-attentively from a peripheral glance, it sets **no** `font-size` (so DS-HIER-003's four-channel prohibition — which requires size *and* weight ≥600 *and* colour *and* border — is not tripped), and DS §11.6 already specifies this anatomy. No other element on this page may carry a fill.

## D. Layout — 1440×900

```
0        200                                                            1440
├─ rail ─┼───────────────────── main (1240) ────────────────────────────┤
│        │  ┌ content 1180 ──────────────────────────────────────────┐
│        │  │ primary column 848              │ 24 │ panel 308        │
│        │  ├─────────────────────────────────┤    ├──────────────────┤
│        │  │ ← Today / ▪双链演示                              ⟵ crumbs (16px)
│        │  │ 被引用方：估值结论  (● Draft)              Delete ⟵ title (28px)
│        │  │ /tmp/demo-b · 40% · eta 2h · writing report   ⟵ identity (16px)
│        │  ├──────────────────────────────────────────────────────────┤
│        │  │ ┌ document container 748 ───────┐ │    │ CARDS     (T-09)│
│        │  │ │ ┌ prose measure 616 ────────┐ │ │    │ ▫ wave-report   │ 28px
│        │  │ │ │                           │ │ │    │ ▫ codex         │ 28px
│        │  │ │ │  ┌ dashed, U-1 ─────────┐ │ │ │    │                 │
│        │  │ │ │  │                      │ │ │ │    │ ACTIVITY  (T-09)│
│        │  │ │ │  │  No report yet.      │ │ │ │    │ Nothing yet.    │
│        │  │ │ │  │                      │ │ │ │    │  (--text-3)     │
│        │  │ │ │  │   (the document's    │ │ │ │    │                 │
│        │  │ │ │  │    shape, empty)     │ │ │ │    │                 │
│        │  │ │ │  │                 ~480 │ │ │ │    │                 │
│        │  │ │ │  └──────────────────────┘ │ │ │    │                 │
│        │  │ │ └───────────────────────────┘ │ │    │                 │
│        │  │ └───────────────────────────────┘ │    │                 │
│        │  └───────────────────────────────────┘    └─────────────────┘
└────────┴──────────────────────────────────────────────────────────────┘
```

**Measures are evidence, not taste.** 748px container / 616px prose are the legacy `.report-doc` numbers (legacy extract §4), and the legacy wave screenshot confirms them. The current build's 1128px card rows with a note at the far edge are the exact opposite: two words nailed to the two ends of a 1128px line (audit §8.2).

**Whitespace.** The document slot fills the primary column's height — a document that is empty still occupies a document's worth of space, because that is what teaches its shape. The panel is short and stops; a short panel beside a tall document is not a void, it is a margin.

## E. States

| State | Treatment |
|---|---|
| **Loading** | Header renders from the rail's cached wave (title, lifecycle, cove) with no flash; the document slot shows `Loading…` at T-14 after 200ms. |
| **Empty — no cards** | The board's ghost geometry: card-kind tiles at the board's real tile size, `1px dashed var(--hairline)`, label T-14, clicking one creates that card in that slot (DS §12.4). Until the board exists, U-1 covers it. |
| **Empty — no report** | The report surface (`--paper`, `--measure-prose`) rendered empty with its editing affordance, so the shape is visible before the content (DS §12.4). |
| **Empty — activity** | Inline: `Nothing yet.` at T-11 `--text-3` in a `--row-h` dashed box. |
| **Error** | Region-scoped. A failed card fetch shows the error box in the panel; the header and document stay. A failed *wave* fetch is page-level. |
| **Not built yet** | U-1, **exactly once**, in the P0 slot: `No report yet.` — and, when the board lands but the runtime does not, `Cards are not wired up yet.` The current build says it N+1 times (once per card row plus once in the empty state) in a tone that fails contrast. |

## F. What the wave page deliberately does NOT show

| Not shown | Lives instead |
|---|---|
| The cove's other waves | The rail (the wave's cove is auto-expanded) and the cove page. |
| Raw ids, plugin ids, overlay payloads | Nowhere in chrome. A `Details` disclosure inside an error, `--font-mono`/`--text-xs` (DS §11.15). |
| A second primary action | There is **no** primary action on this page (zero is allowed and common — DS-ACT-001). Creating a card is a board gesture; renaming is in-place; deleting is destructive-tertiary. |
| Progress as an animated bar | A stepped 3px hairline and a tabular `40%`. It updates while being read (DS-MOT-003). |
| Conversation | Legacy put Report/Conversation in tabs; when the conversation lands it is the panel column or a drawer at `--drawer-w` 396px — **not** a second thing competing with the document for the primary column. |

---

# 4. `/settings`

## A. The job

> A user opens Settings to **change one preference and confirm it stuck**.

**Two-second question:** *Where is the setting I came for?*

Nobody browses settings. The page's only job is fast targeting and unambiguous confirmation.

## B. Information inventory

| Item | Currently | Belongs |
|---|---|---|
| Breadcrumb `Today / Settings` | `--text-sm` (the wave page uses `--text-xs` for the same thing — audit §4 C4) | yes, unified at `--text-xs` |
| `Settings` title | `--text-xl` 22px | yes, at `--text-lg` |
| `NETWORK` / `APPEARANCE` section labels | `--text-sm`/`--text-3`/uppercase | yes, T-09 at `--text-xs` |
| HTTP proxy / HTTPS proxy fields | input on `--surface-bg` inside a `--surface-card` card — inverts between themes (DS-ANTI-010) | yes, on `--paper` |
| `Save` / `Reset` | **no `font-size` at all → render at UA 16px**, visibly larger than anything else on the page (audit §7.2) | yes, primary + secondary at `--text-sm` |
| `Saved.` confirmation | `role="status"`, 4s | yes |
| Light / Dark / System radios | 16px, `--accent-soft` fill on the selected one | yes, as a segmented control |
| `Appearance is stored on this device only.` | `--text-4`, **1.91:1** | yes, at `--text-3` |
| Two `.card` boxes (border + radius + surface + padding) | present | **REMOVE the boxes** |
| App version / build / data dir | not rendered | **yes** — it is the thing people actually come to Settings to read |
| Load / save error | `role="alert"` | yes |

## C. Hierarchy

| Tier | Item | Size | Weight | Tone | Position | Reason |
|---|---|---|---|---|---|---|
| **P0** | **The field group the user came for** — the settings form itself | — | — | — | primary column, `--measure-form` 544, start-aligned | The page is a form. Its P0 is the form; there is nothing else. |
| P1 | Field label | `--text-xs` | `--weight-medium` | `--text-2` | above its input | T-13. |
| P1 | Field value (input text) | `--text-sm` | `--weight-normal` | `--text` | inside a `--control-h` input on `--paper` | The chosen value is content (DS §11.10). `--paper` is one of only two directionally stable surfaces — it is raised in both themes, which fixes DS-ANTI-010. |
| P1 | `Save` | `--text-sm` | `--weight-normal` | `--text-on-accent` | actions row, first | The page's one primary action, `data-action="primary"`. |
| P1 | Appearance segmented control | `--text-sm` | `--weight-normal` (500 on the selected) | `--text-2` (`--text` selected) | Appearance section | Tab matrix (DS §4.3): the selected segment takes weight + tone, and a 2px `--accent` indicator — **not** an `--accent-soft` fill, which is reserved for selection in *lists* (DS-ACT-004). |
| P2 | Page title `Settings` | `--text-lg` | `--weight-semibold` | `--text` | header line 2 | T-03. The page's only 18px element. |
| P2 | Section labels `NETWORK` / `APPEARANCE` / `ABOUT` | `--text-xs` | `--weight-semibold` | `--text-3` | above each group, `--space-8` above / `--space-4` below | T-09. **These labels replace the card boxes.** Per DS-SURF-003's ladder, step 1 (can `gap` alone group this?) answers yes: a label plus `--space-8` separates two groups as well as a border + radius + surface + padding does, using one channel instead of four (DS-ANTI-002). |
| P3 | Breadcrumbs | `--text-xs` | 400 / 500 current | `--text-3` / `--text-2` | header line 1 | T-19 / T-20, identical to the wave page (§C-3). |
| P3 | `Reset` | `--text-sm` | `--weight-normal` | `--text` on `--surface-chip` | actions row, after `Save` | Secondary. It must be findable without reading, and it sits beside a primary (DS-ACT-005). |
| P3 | Field hint (`Appearance is stored on this device only.`) | `--text-xs` | `--weight-normal` | `--text-3` | under its group | T-14. Was `--text-4` at 1.91:1. |
| P3 | `Saved.` | `--text-xs` | `--weight-medium` | `--success-text` (TCR-006) | actions row, after `Reset` | The one green pixel in the app, for 4 seconds, then gone. Never `--success` as text (DS-COLOR-005). |
| P3 | About: version · build · data dir | `--text-xs` | `--weight-normal` | `--text-3`, mono for the path | last section | T-16. Real information that currently has no home. |
| — | The two `.card` boxes | — | — | — | — | **Delete.** Four separators (border + radius + surface + padding) for one boundary. |
| — | `.crumbSep` at `--text-4` | — | — | — | — | Legal as a glyph, but unify with the wave page's `.crumbSeparator` — one component, not two class names (audit §4 C4). |

**Disabled states.** `Save`/`Reset` disable when the form is clean. Disabled = `color: var(--text-4)` + `--surface-chip` fill + `cursor: default` — **never** `opacity: 0.5` (DS-STATE-003; the current build uses it in 3 places, DS-ANTI-009). This is the one sanctioned use of `--text-4` as a text colour (DS-COLOR-001).

## D. Layout — 1440×900

```
0        200                                                            1440
├─ rail ─┼───────────────────── main (1240) ────────────────────────────┤
│        │←24→│
│        │  ┌ content 1180 (start-aligned) ─────────────────────────────┐
│        │  │ Today / Settings                          ⟵ crumbs (16px) │
│        │  │ Settings                                  ⟵ title  (28px) │
│        │  ├──────────────────────────────────────────────────────────┤
│        │  │ ┌ --measure-form 544 ───────────┐                         │
│        │  │ │ NETWORK                (T-09) │   ← 648px of page       │
│        │  │ │ HTTP proxy                    │     gutter. Deliberate: │
│        │  │ │ [                          ]  │28px a form is capped at │
│        │  │ │ HTTPS proxy                   │     544 and a second    │
│        │  │ │ [                          ]  │28px column would be     │
│        │  │ │ [Save] [Reset]   Saved.       │28px invented work.      │
│        │  │ │                               │                         │
│        │  │ │ APPEARANCE             (T-09) │  ← --space-8 between    │
│        │  │ │ ( Light │ Dark │ System )     │28px   sections; no      │
│        │  │ │ Stored on this device only.   │       card boxes        │
│        │  │ │                               │                         │
│        │  │ │ ABOUT                  (T-09) │                         │
│        │  │ │ version   0.x.y               │                         │
│        │  │ │ build     abc1234             │                         │
│        │  │ │ data dir  ~/.neige/calm       │ (mono)                  │
│        │  │ └───────────────────────────────┘                         │
│        │  └──────────────────────────────────────────────────────────┘
└────────┴──────────────────────────────────────────────────────────────┘
```

**Why this page is legitimately sparse.** A form is capped at `--measure-form` 544 (legacy `.col` was 620). The 648px to its right is a **page gutter**, not a void — see the DS-LAY-007 scoping amendment in §C-1.6. Filling it with a second column would mean inventing settings to put there. The honest fix for the current 54% whitespace figure is *more real content in the same column* — which is what the **About** section is — not a wider layout.

`[assumption]` Settings will grow (agent defaults, MCP servers, kernel paths). At ~5 sections a left index column at `--rail-w` 200 inside the page becomes worth it. Below 5 it is chrome for its own sake. What would settle it: the backlog of settings the kernel already exposes.

## E. States

| State | Treatment |
|---|---|
| **Loading** | Section labels render; each group shows one T-14 `--text-3` line, `Loading settings…`. The form is **not** rendered empty and then filled — that is a layout jump on a surface the user is about to type into. |
| **Saving** | `Save` disables and **keeps its label and its width** — the label swaps to `Saving…` in place; no spinner, no resize (DS-LOAD-005). The current build already swaps the label; keep it, add the fixed width. |
| **Saved** | `Saved.` at `--success-text`, `role="status"`, 4s. |
| **Empty** | Not applicable — a form is never empty. If a whole section's data is unavailable, the section renders its label plus one error line, not a dashed box. |
| **Error — load** | Inline at the top of the affected section: 6px `--error` dot + T-14 at `--error-text` + tertiary `Retry`, in a `--warn-soft` box (DS §11.15). Not a page banner — Appearance still works when Network fails. |
| **Error — save** | Same box, directly under the actions row, so it is adjacent to the control that produced it. |
| **Not built yet** | Nothing. If a settings group is not implemented, **the group is absent.** A settings page that lists sections it cannot honour is worse than a short one. |

## F. What Settings deliberately does NOT show

| Not shown | Lives instead |
|---|---|
| Theme preview / swatches | The app itself repaints instantly on selection — that is the preview (DS-COLOR-014: no component knows the theme). |
| A description paragraph under the title | Deleted. The legacy `.synth` subtitle rendered at 26px, **louder than most headings on the page** (legacy extract §10.2). A settings page does not need to explain itself. |
| Per-setting "learn more" links | Nowhere. One-sentence hints at T-14 or nothing. |
| Account / session management | The rail's account menu (already there: Settings, Sign out). |
| Destructive workspace actions (reset, wipe) | Not yet built; when they land they go in a final `DANGER` section with `--space-12` above it and confirm dialogs (DS-ACT-007). |

---

# C. Cross-cutting

## C-1. The shared page frame

**This frame does not exist today, and that is the single biggest reason the build looks unfinished.** Four pages currently declare their own padding (`12px` on all four), their own root `gap` (`--space-6` on two, `--space-5` on the other two, "with no reason" — audit §3.1), and no width constraint at all except one `40rem` on a settings card.

### C-1.1 The shell

| Property | Value | Rule |
|---|---|---|
| Shell grid | `grid-template-columns: var(--rail-w) minmax(0,1fr)` | DS-DENS-004; **200px**, down from `17rem`/272px = 36% wider than legacy with no more content in it |
| Shell height | `block-size: 100dvh` — **not** `min-height` | DS-LAY-001 / DS-ANTI-011. The current `min-height` makes the whole app one scrolling document, which is why the header can't stick, the rail scrolls away, and a short page leaves a full-viewport grey field |
| Scroll containers | exactly two: the rail and `<main>`; `<body>` never scrolls | DS-LAY-002 |
| Below ~864px | rail collapses to `--rail-w-collapsed` 44px icon strip | DS-DENS-006 / DS-ANTI-012 (currently it vanishes below 960px) |
| Below 640px | mobile end takes over (deferred) | — |

### C-1.2 The page

| Property | Value | Rule |
|---|---|---|
| Page grid | `grid-template-rows: auto minmax(0,1fr)` — header row, content row that **fills** | DS-LAY-003 |
| Page padding | inline `--space-10` (24), block-start `--space-9` (20), block-end `--space-11` (28) | DS-SPACE-008. Legacy `.today-page` = 24/28/28. Current build: **12px on all four sides** |
| Content cap | `--measure-page` 1180, **start-aligned** | DS-LAY-004 — with a persistent left rail, centring detaches content from navigation |
| Content grid | `minmax(0,1fr)` + `--space-10` + `--panel-w` (308) when a panel exists | 848/24/308 at 1440 on Today, Cove and Wave |
| Inner measures | list `--measure-list` 720 (TCR-010) · prose `--measure-prose` 616 · form `--measure-form` 544 · document container 748 | DS-DENS-005: prose is capped, boards and terminals are not |
| Section gap | `--space-8` (16) | DS-SPACE-003/005 |
| Row pitch | `--row-h` 28 or `--row-h-lg` 48, `gap: --space-1` (2) → 30px / 50px pitch, never mixed within one list | DS-SPACE-006, DS-DENS-001 |

### C-1.3 The header pattern — identical on all four pages

```
line 1  [back]  ancestor / ancestor            ← --text-xs, 16px tall, optional
line 2  [dot] Title  [badge]      [spacer]  [primary] [dstr]   ← --text-lg/600, 28px
line 3  machine identity · activity                            ← --text-xs mono, 16px, optional
────────────────────────────────────────────  ← hairline, appears only on [data-scrolled]
```

- Gap between lines `--space-3` (6); `--space-8` (16) below the header before content.
- Sticky at `--z-sticky`, background `--bg`; the block-end hairline appears only when scrolled, transitioning `border-color` only, at `--motion-quick` (DS §11.4).
- **The title line is never omitted and contains exactly one T-03 element** (DS-LAY-006: exactly one `data-page-title` per route).
- Line 1 renders **the entity's domain ancestors**: a wave's ancestor is its cove (`Today / cove`); a cove has none (omitted); Settings' ancestor is the workspace (`Today / Settings`); Today has none. This is a rule, not a per-page choice.
- Never shrinks on scroll — a shrinking header reflows the content the user is reading (DS §11.4).

### C-1.4 Vertical rhythm

| Distance | Token | Where |
|---|---|---|
| between rows in a list | `--space-1` (2) | every list |
| between a section label and its first row | `--space-2` (4) | every section |
| between sections | `--space-8` (16) | every page |
| between major regions (primary ↔ panel) | `--space-10` (24) | Today, Cove, Wave |
| inside a row | only `--space-1,2,3,4,6` | DS-SPACE-002 |

### C-1.5 Unbuilt-region treatment `U-1` — the honest alternative to scaffolding prose

The current build renders, as UI, a 748px-wide mono paragraph describing a resolve order and a README, plus `"Card runtime lands in a later slice."` once per card row. That is a defect of the same class as a `TODO` shipped in a title bar.

> **U-1.** A region whose implementation has not landed renders **at the geometry the real content will occupy**, with `1px dashed var(--hairline)`, no fill, and **one** line of text: T-14 (`--text-xs` / `--weight-normal` / `--text-3`), centred on both axes, of the form `<Noun> is not wired up yet.` or `No <noun> yet.` — **at most six words**. Nothing else. No module path, no file name, no slice name, no README reference, no explanation of a contract, no apology, no icon, no action.

Rationale: the *shape* is the useful information (it teaches the layout the user will get), the *sentence* is a one-time acknowledgement, and everything beyond that is a note from one developer to another that happens to be rendered in a product. The dashed border is what distinguishes "a container with nothing in it" from "a container with something in it" (DS-EMPTY-002); `--text-3` is what makes it read as *nothing here* rather than as content (DS-EMPTY-001).

**Applied:** Today terminal → `Terminal is not wired up yet.` · Wave document → `No report yet.` · Wave board → `Cards are not wired up yet.` Every other scaffolding string is deleted outright.

### C-1.6 Two scoping amendments this frame needs

| Request | Rule | Change | Why |
|---|---|---|---|
| **A-2** | **DS-LAY-007** ("no contiguous empty rectangle taller than 240px within `<main>`") | Scope the measurement to the **primary content column**, not to `<main>`'s full width. | Otherwise a 544px form on a 1192px page fails the rule with no honest fix — the only way to pass would be to invent content. The rule's real target is the current build's *vertical* void (content ending at y=135 of 900), and column-scoping still catches that exactly. |
| **A-3** | **DS-SURF-005** (a hairline and a surface change may not both mark one boundary unless Δ < 1.0 L) | Raise the threshold to **Δ < 3.0 L**. | See §C-2. At the measured Δ 0.8 L / 1.02:1 the rail is invisible on its own; at the proposed Δ 2.4 L it is still marginal and the hairline is still doing real work. Under the current threshold, raising the surface delta would *forbid* the border, which is backwards. |

## C-2. The rail — present on every page

| Property | Value | Evidence |
|---|---|---|
| Width | `--rail-w` **200px** expanded, `--rail-w-collapsed` **44px** | legacy `.side`, measured (legacy §4). Current: 272px |
| Padding | `--space-3` block-start / `--space-5` inline / `--space-7` block-end (legacy `6 10 14 10`) | legacy §2 |
| Row gap | `--space-1` (2px) | legacy §4 |
| Section gap | `--space-6` (12px) | DS §11.1 |
| Surface | `--surface-rail` (revised — TCR-011) + `1px solid var(--hairline)` inline-end | audit §6.2: current delta is **1.02:1** |
| Scroll | its own, full `100dvh` | DS-LAY-002 |

### Hierarchy inside the rail

| Tier | Element | Height | Size | Weight | Tone |
|---|---|---|---|---|---|
| **P0** | **The current position** (active cove row or active wave row) | — | — | `--weight-semibold` (up from 500) | `--text` on `--accent-soft` + `1px --accent` |
| P1 | "Waiting on you" section rows | `--row-h-sm` 24 | `--text-sm` | `--weight-medium` | title `--warn-text`, glyph `--warn` |
| P1 | Cove rows | `--row-h` 28 | `--text-sm` | `--weight-medium` | `--text` |
| P2 | "Pinned" section rows | `--row-h-sm` 24 | `--text-sm` | `--weight-normal` | `--text-2` |
| P2 | Wave rows under a cove (compact) | `--row-h-sm` 24 | `--text-sm` | `--weight-normal` | `--text-2` |
| P3 | Section labels (`WAITING ON YOU` / `PINNED` / `COVES`) | one `--row-h-sm` slot | `--text-xs` | `--weight-semibold` | `--text-3` (was `--text-4`, **2.03:1**) |
| P3 | Cove identity swatch | `--dot-md` 8 | — | — | cove colour, unchanged in every state |
| P3 | Wave count per cove | — | `--text-xs` | `--weight-normal` | `--text-3` (was `--text-4`, **2.03:1**; on a selected row **1.86:1**) |
| P3 | Chevron, delete `×`, pin | `--control-h-sm` 20 | — | — | `--text-3`; hover-revealed with space reserved |
| P3 | Brand, account row | `--control-h` 28 | `--text-sm` | `--weight-medium` | `--text-2` |

**Section ordering is fixed** (INV-SIDEBAR-007) and pinning is not relocation. Sections with zero rows are **not rendered** — no label, no dashed box. That is why the rail looks empty at rest and complete when there is work, which is exactly the legacy behaviour (legacy §9.3).

**One emphasis rule for the rail:** only the current position may take `--accent`. "Waiting on you" is the only place `--warn` appears. Everything else is a grey ramp. A rail with three coloured states is a status board, not navigation.

### How the rail separates from the main column

Four devices, in order of how much work each does:

1. **Content rhythm.** Rail type is uniformly 11–12.5px at 24–28px pitch; main opens with an 18px/600 title. The two regions are different densities, which is the strongest boundary signal on the screen and costs nothing (legacy §9.5 calls this out as a signature).
2. **Asymmetric gutter.** Rail rows sit 10px from the boundary; main content starts 24px from it. A 34px gap with text on both sides at different insets reads as an edge.
3. **1px `--hairline` inline-end border** (1.20:1 — decorative, exempt under DS-COLOR-003, and never the *sole* carrier of the boundary, which is exactly why (1) and (2) come first).
4. **Surface delta.** Currently **Δ 0.8 L / 1.02:1** — below the threshold of perception. TCR-011 raises it to Δ ~2.4 L.

**Honest statement of the ceiling:** no pair of adjacent near-white or near-black greys can reach 3:1, and WCAG 1.4.11 does not apply to a decorative region fill. The target is **perceptual separation (ΔL ≥ 2)**, not a contrast ratio. Anyone who reads the audit's "≥ 3:1 for `--surface-rail` on `--bg`" as a requirement will end up with a rail that looks like a different application.

## C-3. Cross-page consistency

Same conceptual element → same treatment. Every cell reads `size · weight · tone · position`. **Divergences are marked ⚠ and justified in the cell.**

| Element | Today | Cove | Wave | Settings |
|---|---|---|---|---|
| **Page title** | `--text-lg`·600·`--text`·header line 2 (date) | same (cove name, editable) | same (wave title, editable) | same (`Settings`) |
| **Breadcrumb ancestor** | ⚠ absent — Today is the root | ⚠ absent — a cove has no domain ancestor | `--text-xs`·400·`--text-3`·header line 1 | same as Wave |
| **Breadcrumb current** | — | — | `--text-xs`·500·`--text-2` | same |
| **Count** | `--text-sm`·500·`--text-2`·header, after title (running/waiting) ⚠ promoted one tone step: these two counts *are* the P0 summary | `--text-xs`·400·`--text-3`·after title | `--text-xs`·400·`--text-3`·panel section head | `--text-xs`·400·`--text-3`·About |
| **Timestamp / duration** | `--text-xs`·400·`--text-3`·`.tnum`·row inline-end | same | same (header line 3) | same (About) |
| **Wave row (full)** | `--row-h-lg` 48, 2 lines, progress track | identical | — | — |
| **Wave row (compact)** | `--row-h` 28, 1 line (agenda, recent) | — | — | — |
| **Wave row (rail)** | `--row-h-sm` 24, 1 line, no track — ⚠ deliberately tighter: the rail is a 24–28px density, the content pane a 48px one; two densities on one screen is a legacy signature (legacy §9.5), not drift | same | same | same |
| **Lifecycle** | 6px dot + `--text-xs`·400·`--text-3` (row) | same | ⚠ **pill badge** (T-17) in the header — exactly one per page, and only here; a pill on every row would make the list permanently multicolour (DS-PRIN-005) | — |
| **Cove identity dot** | `--dot-sm` 6 (agenda row, calendar) | `--dot-md` 8 (header) ⚠ larger because it is the page's subject, not a row marker; ⚠ **absent from rows** — redundant when every row shares one cove | `--dot-sm` 6 (breadcrumb) | — |
| **Section label** | `--text-xs`·600·`--text-3`·uppercase·`--tracking-wider` | same | same | same |
| **Primary action** | ⚠ none at rest (`New cove` appears only in the brand-new-workspace empty state) | `+ New wave`, solid `--accent`, `--control-h` | ⚠ none — DS-ACT-001 permits zero | `Save`, solid `--accent` |
| **Secondary action** | — | — | — | `Reset`, `--surface-chip` + `--hairline-strong` |
| **Destructive action** | — | `Delete` — `--text-sm`·400·`--text-2`·transparent, far inline-end past `--space-6`; red **only** on hover/focus | identical | ⚠ absent today; when it lands, identical, in a final `DANGER` section |
| **Machine identity (path)** | — | `--text-xs`·400·`--text-3`·**mono**·header line 3 | identical | identical (About data dir) |
| **Empty (inline)** | `--text-3` in a `--row-h` dashed box | replaced by the composer (DS-LAY-008) | `--text-3` dashed box | n/a |
| **Unbuilt region** | U-1 | none | U-1 | none |
| **Selected row** | `--accent-soft` + `1px --accent` + weight 500→600 | same | same | ⚠ segmented control uses a 2px `--accent` indicator instead — `--accent-soft` is list-selection only (DS-ACT-004) |
| **Hover on a row** | `--overlay-hover` | same | same | same |
| **Focus ring** | `2px solid --accent`, offset `+2` (or `-2` inset on rows/tabs/menu items) | same | same | same, plus the input variant (`box-shadow: 0 0 0 3px --accent-soft`) |
| **Page padding** | 24 / 20 / 28 | same | same | same |
| **Content cap** | 1180, start-aligned | same | same | same |

Four divergences are marked; each names its reason. Everything else is identical by construction, which is the point — **the frame must be boring so the content can be legible.**

## C-4. Ranked implementation order

Ordered by visible improvement per unit of work. Each row states what a screenshot looks like afterward.

| # | Change | Size | Visible result |
|---|---|---|---|
| **1** | **Write the `reset` + `base` layers** (DS §13.2 R1–R10, B1–B9). Both layers are declared in `entry.css` and **empty**. | ~50 lines, 2 files | Kills the 8px white frame around the dark-mode app, the permanent scrollbar (content-box + `min-height:100%`), Times New Roman, and the 16px UA leak into 31 elements. Critically it **wires up the weight channel** — the mechanical reason nothing in the app has hierarchy is that 14 modules each declare `font: inherit` and never set a weight (DS-ANTI-013 → DS-ANTI-001). Nothing in this document is implementable before this lands. |
| **2** | **The shared page frame** (§C-1): shell `block-size:100dvh`, rail 200px, page grid `auto/1fr`, padding 24/20/28, `--measure-page` cap, one header component. | ~120 lines, 5 files | Removes the 66/86/79/54% whitespace figures and the 950px horizontal holes in one change. This is what makes the four pages look like one application. |
| **3** | **Weight + tone pass**: T-03 titles at 18/600, row titles at 12.5/500, all metadata to `--text-3`; **delete every `color: var(--text-4)` on text** (24 sites). | ~40 single-value edits | Closes all **28 WCAG failures** (14 light + 14 dark, audit §6.1) and is the first change after which the pages have visible ranking rather than one grey texture. |
| **4** | **Delete the scaffolding**: the Today terminal paragraph, all N+1 copies of `Card runtime lands in a later slice.`; apply U-1 (§C-1.5) to the three unbuilt regions. | ~15 lines removed, ~20 added | The most embarrassing thing on the screen stops being on the screen. |
| **5** | **Density tokens** (TCR-002): `--row-h-sm/​--row-h/​--row-h-lg` = 24/28/48, `--control-h-sm/​--control-h/​--control-h-lg` = 20/28/32; collapse the measured **11 control heights** (12/17/18/18.5/20/22/24/25/28/29/38/43/47) to three and the **5 dot sizes** to two. | new tokens + ~35 replacements | The eye gets a rhythm to lock onto. Rows of different content lengths stop being different heights in the same list (DS-ANTI-008). |
| **6** | **Action hierarchy**: `data-action` on every button (DS-ACT-002); `+ New wave`/`Save` become solid-accent primary; `--accent-soft` stops being a button fill; destructive goes colourless at rest; fix the backwards hover. | ~60 lines, 5 files | Create and destroy stop looking identical (DS-ANTI-004). Buttons stop going *backwards* on hover (DS-ANTI-005). |
| **7** | **Today re-rank** (§1): clock 36px → `--text-sm` at the header's inline-end; "Waiting on you" / "Running" / "Recent" become the primary column. | ~1 file, moderate | The page starts answering the question it exists to answer. Highest *product* value on this list; ranked 7th only because 1–3 are prerequisites. |
| **8** | **The `--panel-w` 308 column** on Cove and Wave; card inventory moves into it; `--measure-list` 720 caps the wave list. | ~2 files | The 86% / 79% whitespace figures resolve; the two-word-nailed-to-two-ends rows disappear. |
| **9** | **Focus + motion**: the single `:focus-visible` recipe in `base` (25 controls currently fall through to Chromium's `1px auto rgb(16,16,16)`); `transition: background-color/color/border-color var(--motion-quick)` on interactive elements (currently **0** transitions app-wide); the reduced-motion killswitch. | ~10 lines | Closes an a11y defect and removes the main source of "cheap" feel — 21 hover states that snap with no acknowledgement (DS-ANTI-018). |
| **10** | **Rail surface + section suppression** (§C-2): TCR-011 value, empty sections not rendered, `--text-4` out of the section labels and counts. | ~15 lines | The rail becomes a region rather than a lighter patch of the same page. |

**Steps 1–4 are ~125 lines net and cover the majority of what the owner is reacting to.** Steps 5–8 are the structural work. Steps 9–10 are completion.

## C-5. Token change requests

`tokens.css` is **FROZEN**. This document depends on DS §14's requests and adds three. Nothing below was used silently anywhere above.

### Depended on, already filed in `docs/_fe-design-system.md` §14

| ID | Token | Needed by |
|---|---|---|
| **TCR-001** | `--weight-normal/​-medium/​-semibold` = 400/500/600 | Every hierarchy table in this document. Weight is the primary channel inside rows and headers, where DS-HIER-005 forbids size. Without it there is no hierarchy to design. ⚠ the `500` value needs a browser check on the Linux WebView — if `-apple-system` has no medium face there, every `--weight-medium` cell falls back to tone-only and the tables need a revision pass. |
| **TCR-002** | `--row-h-sm/​--row-h/​--row-h-lg`, `--control-h-sm/​--control-h/​--control-h-lg` | §C-1.4, every row spec, implementation step 5 |
| **TCR-003** | `--rail-w` 200, `--rail-w-collapsed` 44, `--panel-w` 308, `--drawer-w` 396 | §C-1.1, §C-2, every wireframe |
| **TCR-004** | `--measure-prose` 616, `--measure-form` 544, `--measure-page` 1180, `--measure-board` 1280 | §C-1.2, all four layouts |
| **TCR-005** | `--warn-text` | Waiting row titles, lifecycle badge (the current pairing measures **4.01:1**) |
| **TCR-006** | `--success-text` | The `Saved.` confirmation |
| **TCR-007** | `--text-on-accent` | `+ New wave`, `Save` |
| **TCR-008** | `--shadow-float` | Menus and the confirm dialog only; not used by any page surface here |

### New

| ID | Token | Light | Dark | Justification | Priority |
|---|---|---|---|---|---|
| **TCR-009** | `--dot-sm`<br>`--dot-md` | `6px`<br>`8px` | same | Nine independent dot implementations at **five sizes** (5/6/6/6/6/6/8/8/12px) for two meanings — status and identity (audit §2.2, DS-ANTI-017). The same cove's dot is 8px in the rail, 12px on the cove page, 6px in a wave row and 6px in a breadcrumb. This design assigns `--dot-sm` to status glyphs and row-level identity, `--dot-md` to a cove's own header swatch, and nothing else. Values are the two the codebase already converged on. | **High** |
| **TCR-010** | `--measure-list` | `720px` | same | DS §6.1 caps prose (616) and boards (1280) but names no measure for a **list**, so the current build runs wave rows to 1128px and then pushes the lifecycle label to the far edge with `margin-inline-start:auto`, leaving a measured **~950px gap between the title and its own status** (audit §8.2). 720px is legacy `.col.wide`; the legacy cove screenshot measures the rendered row at **655px** inside it. Without this token every list re-decides its width and DS-DENS-005 is unenforceable for lists. | **High** |
| **TCR-011** | `--surface-rail` (**value change**, not a new name) | `oklch(96.4% 0.004 240)`<br>(from `98%`) | `oklch(13% 0.008 245)`<br>(from `15%`) | The rail↔main boundary measures **1.02:1** light / **1.01:1** dark — below the threshold of perception (audit §6.2), so the region that is on every page has no visible edge. The proposed values give ΔL **2.4** (light, vs `--bg` 98.8) and **3.0** (dark, vs 16), matching the delta `--surface-card` already carries in light. **Honest limits**: this reaches ≈1.06:1, not 3:1 — no adjacent-grey pair can, and WCAG 1.4.11 does not apply to a decorative region fill; the target is perceptual separation, not a ratio. It must be browser-verified in both themes (DS-COLOR-013), and every `--text-*` on the new value must be re-measured (`--text-3` on the current `--surface-rail` is 5.20 light, with ~0.5 of headroom). Requires amendment **A-3** (§C-1.6) so keeping the hairline stays legal. | **High** |
| **TCR-012** | `--glyph-sm`<br>`--glyph` | `14px`<br>`16px` | same | DS §11.7 specifies "a single 14px or 16px glyph" inside icon buttons but names no token, so the build has chevrons at 18px, `×` at 20px and back-arrows at 24px sized by their *box* instead of their glyph. Low priority — it only matters once the icon set is real SVG rather than text glyphs (`«`, `▾`, `×`, `←`). | Low |

**Deliberately NOT requested:** a breakpoint token (media queries cannot read custom properties — the two hand-written `60rem` values become one documented constant in `styles/`, not a token); a border-width token (36 sites, all `1px`, and a second width would be a design change not a token); a per-page spacing token of any kind.

## C-6. Amendments to the design system this design requires

Filed explicitly rather than quietly diverged from.

| ID | Rule | Requested change | Why |
|---|---|---|---|
| **A-1** | **DS-TYPE-009** — `--text-xl`, `--text-display-sm`, `--text-display` may appear only in **the Today clock** and empty-state heroes | Narrow the allowlist to **empty-state heroes only**. The clock moves to `--text-sm`. | The owner's stated defect is that the largest, brightest element on Today is the least actionable information on it, and the measurement agrees: 36px is 2.8× the base and the only `--text-display` on the page. DS-HIER-001 says a surface has exactly one primary emphasis; DS-PRIN-002 says making a second thing important lowers the first. A clock cannot be the primary emphasis of a page whose job is "does anything need me". The spec itself flagged this as provisional (**P-3**). Consequence: `--text-display` (36) and `--text-display-sm` (26) become **unused** — which is correct, and matches the audit's finding that `--text-display-sm` is already unused and 8 size tokens are servicing 6 real roles. |
| **A-2** | **DS-LAY-007** — no contiguous empty rectangle > 240px within `<main>` | Scope to the **primary content column**. | §C-1.6. |
| **A-3** | **DS-SURF-005** — hairline + surface change may not mark one boundary unless Δ < 1.0 L | Raise to Δ < **3.0 L**. | §C-1.6, TCR-011. |
| **A-4** | *(new)* **DS-EMPTY-005** | *"Text that names a module path, a file, a slice, a contract, a ticket or a README may not be rendered as UI on any surface. A region whose implementation has not landed uses treatment U-1 (≤ 6 words, no references)."* `machine-checkable` — grep feature/ui modules' rendered string literals for `/`, `.tsx`, `README`, `slice`, `contract`. | The current build renders 748px of mono prose about `features/today/terminal`'s resolve order, plus one copy of `Card runtime lands in a later slice.` per card row at 1.91:1 contrast. This is a class of defect, not two instances, and it will recur on every future slice boundary unless it is a rule. |

## C-7. Assumptions

| ID | Assumption | What would settle it |
|---|---|---|
| `[assumption]` | The owner wants the clock demoted rather than deleted. Ambient time-of-day is a "calm" product signature (legacy rendered it at 36px/300 with a blinking colon), so this design keeps it — at metadata weight. | One sentence from the owner: keep at `--text-sm`, or remove entirely. Removal is strictly simpler. |
| `[assumption]` | The cove panel's lifecycle breakdown (n draft / n working / n blocked) is worth building. It exists to give the cove page a second vertical anchor and to answer "is this cove healthy" without reading every row. | Whether the owner scans coves for health, or only ever drills straight to a wave. If the latter, drop the panel and let the cove page be a 720px list with a gutter. |
| `[assumption]` | The `progress` / `eta` / `now` overlay fields are actually populated in production. They exist on `Wave` and are decoded (`waveActivityFrom`), and the wave row's second line and the wave header's activity line both depend on them. If no plugin writes them, both lines are permanently empty and `--row-h-lg` 48 is not earned. | One production read of the overlays table. If they are empty, the wave row drops to `--row-h` 28 single-line and the wave header loses line 3's tail. |
| `[assumption]` | Settings stays under ~5 sections for now. | The backlog of kernel-exposed settings. At 5+, add a 200px in-page section index. |
| `[assumption]` | `--row-h-lg` = 48px is right for the two-line wave row (DS §18 P-1 is still open; legacy measured **72.25px**). This design uses 48 and it is what the wireframes are drawn to. | Measuring a 48px row with a real title + activity line + progress track. If the progress track and the two lines do not breathe at 48, the answer is **56**, not 66 — and only the Cove and Today wireframes' row heights change. |

## C-8. Where this design can be checked

Not a gate spec — a pointer for the agent who writes one. The claims in this document that are decidable:

- **One 18px element per page** — DS-HIER-001, browser-tier. Catches a second title.
- **Three distinct chrome font sizes per page** — DS-TYPE-010 (cap 4), browser-tier.
- **Zero `color: var(--text-4)` outside `:disabled` / dot elements** — DS-COLOR-001, machine-checkable. This is the single check that closes 28 measured contrast failures.
- **Exactly one `[data-action="primary"]` per page root; zero is legal** — DS-ACT-001, browser-tier.
- **Every row sets `min-block-size` to a `--row-h*` token** — DS-DENS-001.
- **Page padding, content cap and header shape are identical across the four routes** — one snapshot per route of `<main>`'s computed padding + `max-inline-size` + header line count.
- **No rendered string literal contains `/`, `.tsx`, `README`, `slice` or `contract`** — A-4 / DS-EMPTY-005.
- **The largest empty rectangle in the primary column is ≤ 240px tall** — DS-LAY-007 as amended.

What is **not** checkable, and must be a human read: whether the P0 of each page is the right P0. No checker that reports zero violations should be read as certifying that a page has hierarchy (DS §16's honest note applies verbatim here).
