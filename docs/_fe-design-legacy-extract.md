# Legacy frontend — visual language extract (evidence-based)

Extracted 2026-08-10 for the FE rewrite design system spec.

## Sources & how to read the citations

| Tag | Meaning |
|---|---|
| `calm.css:NNNN` | `web/src/calm.css` in worktree `997-c1-today` (6834 lines; single stylesheet, no separate token file — the `:root` block lives at `calm.css:9-331`, the dark override at `calm.css:333-383`) |
| `computed, <theme>, <route>` | `getComputedStyle` from the live app at `http://localhost:4041/calm/`, Chromium 1440×900, `colorScheme` set on the context |
| `served.css` | the built bundle actually served at `/calm/assets/index-ZglSzD8S.css` (129 153 bytes) |

**Important caveat.** The running dev stack is built from a **newer** source than `web/src/calm.css` in either the worktree (6834 lines) or the primary repo (6810 lines). `served.css` contains selectors that exist in neither file — `.report-convo-tabs`, `.report-convo-tab`, `.report-event-line`, and a **rewritten `.report-page` grid** (rail on the *right*, `minmax(0,1fr) 280px`, collapsed `44px`) instead of the repo's left-rail `250px minmax(0,1fr) 0`. Token vocabulary is byte-identical between the two; only report-page component selectors drifted. **Where they disagree I take the running app as truth** and say so.

Screenshots (1440×900, `…/scratchpad/legacy/`):
`today-light.png` · `today-dark.png` · `cove-light.png` · `cove-dark.png` · `wave-light.png` · `wave-dark.png` · `settings-light.png` · `settings-dark.png`
Routes probed: `/calm/`, `/calm/cove/2bfee6e9cfea49909d4879a4a5799e67`, `/calm/wave/226397006aa04367808349e828b290c8`, `/calm/settings`.

---

## 1. Type scale

### 1a. The declared scale (`calm.css:55-62`, `72-93`)

8 size tokens, 5 leading tiers, 6 tracking tiers, 4 weights. All single-mode (no dark override).

| Token | Value | Declared purpose |
|---|---|---|
| `--text-xs` | 11px | small labels, counts, eyebrows |
| `--text-sm` | 12.5px | dense UI, hints, captions |
| `--text-base` | 13px | default body |
| `--text-md` | 15px | emphasized body, field labels, login |
| `--text-lg` | 18px | card titles |
| `--text-xl` | 22px | section heads |
| `--text-display-sm` | 26px | today-card large numerals |
| `--text-display` | 36px | h-display, login hero |

| Leading | Tracking | Weight |
|---|---|---|
| `none 1` · `tight 1.15` · `snug 1.3` · `base 1.5` · `loose 1.65` | `tighter -0.02em` · `tight -0.01em` · `normal 0` · `wide 0.02em` · `wider 0.06em` · `widest 0.08em` | `400 / 500 / 600 / 700` |

### 1b. What actually renders (computed, both themes, all four routes)

Every distinct `font-size / line-height / weight / letter-spacing` combination observed, sorted descending:

| px | line-height | weight | tracking | Where (computed) | Verdict |
|---|---|---|---|---|---|
| 36 | 36px (1.0) | 300 | −0.3px | `.today-clock-h/-m/-colon` — the wall clock, mono, weight **300** | **Weight 300 appears exactly once in the whole app** (`calm.css:5819`, `5823`). Not in the weight scale. |
| 36 | 41.4px (1.15) | 400 | −0.72px | `.h-display` (cove title), computed light `/calm/cove` | canonical display |
| 36 | 41.4px (1.15) | 500 | 0 | `.report-title` (wave report h1), computed dark `/calm/wave` | serif; same size, different weight+tracking than `.h-display` |
| **30** | **45px (1.5)** | **700** | normal | **`h1` on `/calm/settings` — no class at all**, computed light | **Bug.** Chromium UA default `2em` of the 15px body. See §10. |
| 26 | 33.8px (1.3) | 400 | −0.26px | `.synth` — the page *subtitle* paragraph on Settings and Today | 26px for a subtitle is the single loudest thing on the page (`settings-light.png`) |
| 26 | 29.9px (1.15) | 500 | 0 | `.calm-prose h1` inside `.report-block` | serif |
| 22 | — | 500 | −0.01em | `--text-xl`: `.attn h2`, `.calm-prose h3`, `.today-date` (`calm.css:1233`, `2008`, `6061`) | not present on any probed route — only reachable via attention cards |
| 18 | 20.7px (1.15) | 400 | −0.18px | `.today-clock-day` ("Monday") |  |
| 18 | 27px (1.5) | 500 | 0 | `.wave-title` (breadcrumb tail, header) |  |
| 18 | 29.7px (1.65) | 400 | 0 | `.calm-prose p / li` — **serif, 18px, 1.65** = the report reading body | signature |
| 18 | 18px (1.0) | 400 | normal | `.add-panel`, `.view-cycle`, `.wave-row-delete` — these are *icon glyph sizes*, not text | |
| 16 | 16px | 700 | normal | `.nav-label-add` (`+` in COVES header) — icon glyph | off-scale literal (`calm.css:717`) |
| 15 | 22.5px (1.5) | 400 | normal | `body` default — 152 elements, by far the most common | |
| 15 | 19.5px (1.3) | 700 | 0 | `.nav-item.nav-today` — the "Today" rail item (`--font-nav-primary`) | |
| 15 | 15px (1.0) | 400 | 0 | `.card-head` row (leading collapsed to 1 deliberately, `calm.css:4624`) | |
| 15 | 22.5px | 600 | normal | `.today-stat-n` (the 0 / 0 running·waiting counters) | |
| 15 | 15px | 400 | 0.3px | `.today-term-host` ("reset ↻") | |
| 14 | 14px | 400 | normal | `.side-collapse-toggle`, `.cove-row-delete`, `.side-wave-delete`, `.report-rail-toggle` | **off-scale literal**, 4 sites (`calm.css:554`, `856`, `1086`, `2800`) |
| **13.3333** | normal | 400 | normal | bare `<input>` (radio) on `/calm/settings` | UA default leaking |
| 13 | 16.9px (1.3) | 500 | 0 | `.cove-nav` — the cove row (`--font-nav-group`) | |
| 13 | 19.5px (1.5) | 400 | normal | `.wave-row .s`, `.settings-section-hint`, form buttons | |
| 13 | 13px (1.0) | 500 | 0.78px (0.06em) | `.card-head-title` — uppercase + wider tracking | signature |
| 13 | 21.45px (1.65) | 400 | normal | `.today-term-body` (terminal), mono | |
| 13 | 19.5px | 600 | 0.26px (0.02em) | `.settings-section-title` — uppercase | |
| 13 | 13px | 400 | normal | `.cove-row-chevron` glyph | |
| 12.5 | 18.75px (1.5) | 400 | normal | `.crumbs`, `.schema-form-label`, `.report-byline`, `.report-convo-tab` — 38 elements | |
| 12.5 | 16.25px (1.3) | 500/600 | 0 | `.side-wave` (rail wave rows) / `.who` (username) | |
| 12.5 | 12.5px | 600 | 1px (0.08em) | `.today-clock-ap` ("PM") | |
| 12.5 | 18.75px | 600 | normal | `.cal-month-label`, `.cal-agenda-head` | |
| 11 | 14.3px (1.3) | 700 | 0.66px (0.06em) | `.nav-label` ("COVES") — uppercase | signature |
| 11 | 16.5px | 700 | 0.66px | `.cal-week-dow` (M T W…) | |
| 11 | 16.5px | 600 | 0.22px | `.cal-toggle button` (Week / Month) | |
| 11 | 14.3px | 500 | normal | `.cove-nav-badge.muted` (wave count) | |
| 11 | 11px | 600 | normal | `.card-head-icon--letter` (avatar glyph) | |
| 11 | 18.15px | 400 | 0.66px | `.xterm-status` | |
| 12 | 12px | 400 | normal | `.side-wave-pin` glyph | off-scale literal, *documented* as deliberate (`calm.css:825-826`) |

Off-scale literals still living in `calm.css` after the #150 consolidation, with occurrence counts: **10px** (`.report-rail-head`, `.report-outline-number` — 2), **11.5px** (5 sites: `.report-activity-empty/-earlier`, `.report-prose ::before`, `.report-backlinks-*`, `.report-rail-files row`), **9.5px** (2: `.report-rail-count`, `.report-rail-toggle--show-all`), **12px** (2), **13px raw** (`.card-unknown`), **14px** (4), **16px** (1), **19px** (`.report-prose h3`), **23px** (`.report-prose h1/h2`), **7px** (`.agent-card-logo--codex`). All are in the report subsystem or in icon buttons.

**Near-duplicates to kill:** 12px vs 12.5px vs 11.5px vs 11px is a four-way split with no perceptual difference; 13px vs 13.3333px (UA leak); 23px vs 22px (`--text-xl`); 19px vs 18px (`--text-lg`); 9.5px vs 10px vs 11px in the rail.

**Sizes appearing exactly once (deletion candidates):** 36px/300 (clock), 30px/700 (the settings-h1 bug), 22px (`--text-xl` — *never renders on any probed route*), 7px, 9.5px, 16px, 19px, 23px.

---

## 2. Spacing rhythm

Base unit is **4px**, expressed as a 14-step ladder (`calm.css:239-252`):

`0, 1, 2, 4, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32`

Steps 1/2/4 are sub-grid (logarithmic small end); from `--space-4` (8px) upward it marches on 4px. Note `6px`, `10px`, `14px`, `20px`, `28px` are **not** multiples of 4 — the ladder is really a 2px grid pretending to be a 4px grid. That is the single largest inconsistency in the spacing system: **9 of 14 steps** are 2px-grid, only 5 are true 4px multiples.

Documented deliberate violations (each carries a comment justifying itself):

| Value | Site | `calm.css` |
|---|---|---|
| `3px` border-left | `.side-section.attn-zone` accent stripe | 616-617 |
| `9px` gap | `.cal-event` grid gap ("spec values happen to differ from `--space-5` by 1px") | 6342 |
| `−1px` margin | `.sr-only`, `.today-clock-colon`, `.report-convo-tabs` | 431, 5827 |
| `−4px` margin | `.cal-agenda` (bleed to card edge) | 6316 |
| `−100%` margin-inline-end | `.report-prose h1/h2::after` trailing rule | 2121 |
| `22px` / `20px` / `26px` / `28px` | icon-button hit targets, explicitly excluded from the grid | 311-313 |
| `60px` | `.side-wave-cove` max-width, "chosen empirically" | 786 |

Composite values (`calc(--space-12 + --space-4)` = 36px on `.report-doc`, `calc(--space-12 + --space-6)` = 44px on `.calm-prose h1`) effectively add 36px, 38px, 44px, 56px to the ladder without naming them.

**Observed layout paddings (computed):**

| Element | Padding | Route |
|---|---|---|
| `.side` rail | `6px 10px 14px 10px` | all |
| `.col` (editorial column) | `28px 32px 32px` | settings, cove |
| `.workbench` | `20px 32px 0` (report mode) | wave |
| `.today-page` | `24px 28px 28px` | today |
| `.report-doc` | `40px 40px 56px` | wave |
| `.report-convo-head-inner` | `14px 40px 0` | wave |
| `.card-head` | `10px 14px` + right reserve `8+22+8 = 38px` | today, wave |
| `.settings-section` | `20px` all round | settings |
| `.wave-row` | `14px 60px 14px 0` (right gutter = 2×28 + 4) | cove |

Gaps in use: `1px` (cal grids), `2px` (rail item stack), `4px`, `6px`, `8px`, `10px` (`--card-head-gap`), `12px`, `14px`, `20px`, `24px`.

---

## 3. Colour roles

Resolved to sRGB by converting the declared OKLCH through OKLab → linear sRGB → sRGB (probe script; declared values verified live via `getComputedStyle` on `:root`).

### 3a. Light

| Role | Token | OKLCH (`calm.css`) | sRGB |
|---|---|---|---|
| page bg | `--bg` | `oklch(98.8% .003 240)` :31 | `#f9fbfd` |
| paper (card) | `--paper` | `oklch(99.5% .002 240)` :32 | `#fcfeff` |
| rail | `--surface-rail` | `oklch(98% .003 240)` :143 | `#f7f9fa` |
| terminal | `--surface-terminal` | `oklch(99% .003 240)` :173 | `#fafcfe` |
| card head / inset | `--surface-card` | `oklch(96% .004 240)` :144 | `#eff2f4` |
| chip | `--surface-chip` | `oklch(95% .005 240)` :145 | `#eceff1` |
| panel head | `--surface-panel-head` | `oklch(98% .003 240)` :146 | `#f7f9fa` (**identical to `--surface-rail`**) |
| hairline | `--hairline` | `oklch(92% .005 240)` :33 | `#e2e5e8` |
| hairline strong | `--hairline-strong` | `oklch(86% .006 240)` :34 | `#ced1d4` |
| text 1 | `--text` | `oklch(20% .008 250)` :36 | `#13161a` |
| text 2 (`--text-label`) | `--text-2` | `oklch(45% .01 250)` :37 | `#51565b` |
| text 3 (`--text-meta`) | `--text-3` | `oklch(52% .01 250)` :38 | `#65696f` |
| text 4 (`--text-decorative`) | `--text-4` | `oklch(76% .008 250)` :39 | `#adb1b6` |
| accent | `--accent` | `oklch(52% .13 245)` :111 | `#036eae` |
| accent soft | `--accent-soft` | `oklch(95% .025 245)` :112 | `#e1f1ff` |
| warn | `--warn` | `oklch(58% .16 30)` :114 | `#c74c3d` |
| warn soft / border | `--warn-soft` / `--warn-border` | :115 / :162 | `#ffebe6` / `#edc2bb` |
| success | `--success` | `oklch(54% .14 145)` :126 | `#2d8336` |
| error | `--error` / `--error-text` | :127 / :161 | `#b84d49` / `#b32228` |
| market up / down | `--up` / `--down` | hex :134-135 | `#b3271c` / `#2ba471` (红涨绿跌) |
| scrim | `--overlay-scrim` | `rgba(20,30,55,.32)` :159 | — |

**Surface elevations, light: 7 named, 5 perceptually distinct.** `--bg #f9fbfd`, `--paper #fcfeff`, `--surface-rail`/`--surface-panel-head` `#f7f9fa` (duplicate), `--surface-terminal #fafcfe`, `--surface-card #eff2f4`, `--surface-chip #eceff1`. The spread from bg to paper is **ΔL = 0.7%** — invisible. `--surface-terminal` sits between them and is also invisible. Effectively there are **three** elevations a human can see: page (`#f9fbfd`/`#fcfeff`), inset (`#eff2f4`), chip (`#eceff1`).

**Text ramp: 4 tiers**, aliased semantically to `--text-label` (=2), `--text-meta` (=3), `--text-decorative` (=4) at `calm.css:45-47`.

### 3b. Dark (`calm.css:333-383`)

| Role | OKLCH | sRGB |
|---|---|---|
| `--bg` | `oklch(16% .008 245)` | `#0a0e11` |
| `--paper` | `oklch(19% .009 245)` | `#111418` |
| `--surface-rail` | `oklch(15% .008 245)` | `#080c0e` (**darker than the page**) |
| `--surface-terminal` | `oklch(18% .009 245)` | `#0e1215` |
| `--surface-panel-head` | `oklch(20% .009 245)` | `#13171a` |
| `--surface-card` | `oklch(21% .009 245)` | `#15191c` |
| `--surface-chip` | `oklch(24% .01 245)` | `#1b2024` |
| `--hairline` / `-strong` | `28%` / `36%` | `#25292e` / `#383e43` |
| `--text` / `-2` / `-3` / `-4` | `96/72/64/42%` | `#eff2f5` / `#9fa6ac` / `#868d93` / `#484e53` |
| `--accent` / `-soft` | `72% .14 245` / `28% .05 245` | `#4dacf6` / `#112b40` |
| `--warn` / `-soft` / `-border` | `70%` / `28%` / `40%` | `#f17260` / `#3e1f1a` / `#603d38` |
| `--success` / `--error` | both L=74% (standardised, :352-359) | `#6ec272` / `#f7857d` |
| `--up` / `--down` | hex | `#e94f42` / `#34a87e` |

**Dark inverts the elevation logic**: the rail is *darker* than the page (`#080c0e` vs `#0a0e11`) while cards are *lighter* (`#15191c`). Light does the opposite (rail slightly darker than paper but the page is darkest of the three light greys). That asymmetry is intentional and reads well; it is not a bug.

### 3c. WCAG contrast — computed, not eyeballed

Ratios are exact (relative-luminance formula). **Bold = fails.**

**Light — text on surface**

| surface | text | text-2 | text-3 | text-4 | accent | warn | success | error | error-text |
|---|---|---|---|---|---|---|---|---|---|
| `--bg` | 17.49 | 7.15 | 5.32 | **2.08** | 5.26 | 4.47 | 4.59 | 4.83 | 6.37 |
| `--paper` | 17.94 | 7.33 | 5.46 | **2.13** | 5.40 | 4.58 | 4.71 | 4.95 | 6.54 |
| `--surface-rail` | 17.18 | 7.02 | 5.23 | **2.04** | 5.17 | 4.39 | 4.51 | 4.74 | 6.26 |
| `--surface-card` | 16.14 | 6.59 | 4.91 | **1.92** | 4.86 | **4.12** | **4.23** | **4.45** | 5.88 |
| `--surface-chip` | 15.71 | 6.42 | 4.78 | **1.87** | 4.73 | **4.01** | **4.12** | **4.33** | 5.72 |
| `--warn-soft` | 15.79 | 6.45 | 4.81 | **1.88** | 4.75 | **4.04** | **4.14** | **4.36** | 5.75 |
| `--accent-soft` | 15.75 | 6.44 | 4.79 | **1.87** | 4.74 | **4.02** | **4.13** | **4.34** | 5.74 |

**Dark — text on surface**

| surface | text | text-2 | text-3 | text-4 | accent | warn | success | error |
|---|---|---|---|---|---|---|---|---|
| `--bg` | 17.24 | 7.87 | 5.76 | **2.30** | 7.89 | 6.75 | 8.88 | 7.94 |
| `--paper` | 16.44 | 7.50 | 5.49 | **2.19** | 7.52 | 6.43 | 8.46 | 7.57 |
| `--surface-rail` | 17.48 | 7.97 | 5.84 | **2.33** | 7.99 | 6.84 | 9.00 | 8.05 |
| `--surface-card` | 15.74 | 7.18 | 5.26 | **2.10** | 7.20 | 6.16 | 8.10 | 7.25 |
| `--surface-chip` | 14.62 | 6.67 | 4.88 | **1.95** | 6.68 | 5.72 | 7.53 | 6.73 |
| `--warn-soft` | 13.20 | 6.02 | **4.41** | **1.76** | 6.04 | 5.17 | 6.80 | 6.08 |
| `--accent-soft` | 12.95 | 5.91 | **4.33** | **1.73** | 5.92 | 5.07 | 6.67 | 5.96 |

**Failures that matter:**

1. `--text-4` / `--text-decorative` fails AA **everywhere**, in both themes (1.7–2.3:1). It is *declared* WCAG-exempt (`calm.css:47`) but it is used for real content: `.report-outline-list ul a` (nested outline links, 11.5px, `calm.css:2996`), `.report-rail-count`, `.rb-table thead th` (**table column headers**, `calm.css:2423`), `.report-convo-time`, `.report-convo-muted`, `.report-convo-system`, `.rb-fig-cap`. Those are not decorative.
2. `--warn` (`#c74c3d`) on light `--surface-card` / `--surface-chip` / `--warn-soft` / `--accent-soft`: **4.01–4.12**, short of 4.5. Every `.report-convo-chip--warn`, `.wave-fs-viewer-chip[data-tone=warning]`, `.report-convo-reset` is warn-on-warn-soft. This is exactly the pairing `calm.css:467-472` says they avoided in one place and then used in ten others.
3. `--success` and `--error` on light `--surface-card`/`--surface-chip`: 4.12–4.45. Marginal fails.
4. Dark `--text-3` on `--warn-soft` / `--accent-soft`: 4.33–4.41. Marginal fails — this is the exact class of bug the `#306` comment at `calm.css:2763-2774` already fixed once for exit badges but did not sweep.
5. **Hairlines fail the 3:1 non-text requirement by a mile**: `--hairline` on `--bg` is **1.22:1** (light) / **1.32:1** (dark). `--hairline-strong` is 1.48 / 1.79. `--warn-border` 1.55 / 2.05. Structurally this is *the* aesthetic of the app; formally, no border in this design meets WCAG 1.4.11.

Overlay tiers composited (light, over `--bg`): faint `#f3f5f7`, hover `#eff1f3`, strong `#ebedef`, active `#edeef0`. Note **hover-strong (`#ebedef`) is darker than active (`#edeef0`)** — the "strong hover" is visually heavier than the selected state. That is backwards.

---

## 4. Density (the thing to get right)

All computed from the running app at 1440×900.

### Rail

| Metric | Value | Source |
|---|---|---|
| `.side` width | **200px** expanded, **44px** collapsed | computed all routes; `calm.css:511`, `527` |
| `.side` padding | `6px 10px 14px 10px` | computed |
| inner content width | 179px (200 − 2×10 − 1px border) | computed |
| `.side-section` right padding | 8px (`--rail-scrollbar-gutter`) | `calm.css:255`, `603` |
| gap between rail items | **2px** (`--space-1`) | `calm.css:596` |

### Row heights — the real numbers

| Row | Height | Padding | Font |
|---|---|---|---|
| `.nav-item.nav-today` | **41.5px** | `10 10 12 10` | 15/1.3/700 |
| `.nav-label` ("COVES") | 36px | `12 10 4 10` | 11/1.3/700, uppercase, 0.06em |
| `.cove-nav` (group row) | **28.89px** | `6 10 6 28` | 13/1.3/500 |
| `.side-wave` (rail leaf row) | **28.25px** | `6 36 6 46` | 12.5/1.3/400 (500 active) |
| `.me-row` (account) | **42px** | `8 10` | 12.5/1.3/600 |
| `.wave-row` (cove page list row) | **72.25px** | `14 60 14 0` | title 15/1.5/500, sub 13/1.5/400 |
| `.report-rail-head` | ~44px | `14 16` | 12.5/600 |
| `.wave-report-files-row` | min 28px | `2 6 2 …` | 11 mono |
| `.file-viewer-entry` | min 30px | `4 8` | 12.5 |
| `.cal-event` (agenda row) | ~28px | `6 6` | 12.5/1.3/500 |
| `.card-head` | **41px** | `10 14`, right 38px | title 13/1.0/500 uppercase |

So the rail is a **28px row** system with a 41–42px cap at each end (Today at top, account at bottom), and the content pane is a **72px row** system. Two very different densities on one screen, deliberately.

### Control heights

| Control | Size |
|---|---|
| icon button, rail inline (`.cove-row-delete`, `.side-wave-pin`, `.side-wave-delete`, `.nav-label-add`) | **20×20** |
| icon button, header (`.add-panel`, `.view-cycle`, `.wave-back`, `.card-head-action`) | **26×26** |
| icon button, toggle (`.side-collapse-toggle`, `.report-rail-toggle`, `.cove-head-add`, `.wave-row-delete`) | **28×28** |
| card close `.card-grid-close` | 22×22, inset 8px |
| `.cal-nav` arrows | 22×22 |
| `.cal-week-date` | 24×24 circle |
| `.card-head-icon` avatar | 20×20, radius 4px |
| `.me` avatar | 26×26 pill |
| `.report-byline-avatar` | 22×22, radius 6px |
| primary button `.go` | **36px** high, `0 16px`, radius 10px |
| ghost button `.go.ghost` | **30px**, `0 10px` |
| form buttons (`.schema-form-*`, `.new-task-form-*`, `.dirpicker-*`) | **32px**, `0 12–14px`, radius 6px |
| text input `.schema-form-input` | **36.5px** computed (`6 8` padding + 1px border on 15px/1.5 text) |
| `.login-input` | 40px, radius 10px |
| `.dirpicker-path-input`, `.iframe-url-input` | 30–32px |
| status dots | 6px (most), 7px (`.status-pill-dot`, `.attn .source .dot`), 9px (`.wave-cove-dot`), 3px (`.report-byline-sep`), 4px (`.cal-week-dot`), 3.5px (`.cal-md-dots i`) |

**Five distinct icon-button sizes (20/22/26/28/30) and four button heights (30/32/36/40).** That is the density mess.

### Content measures

| Measure | Value |
|---|---|
| `.col` editorial column | max-width **620px**, `.col.wide` **720px**, padding `28 32 32` |
| `.workbench` | max-width **1280px**, padding `20 32 28` |
| `.report-doc` container | **748px**, padding `40 40 56` |
| `.report-doc` prose measure | **616px** (blocks share one left edge; `.report-block--breakout` widens to 748) |
| `.report-page` rail (running build) | **280px**, collapsed **44px** — right side |
| `.report-page` rail (repo `calm.css:1712`) | 250px, **left** side, plus a 396px conversation drawer |
| `.today-page` | max-width **1180px**; grid `minmax(0,1fr) 308px`, gap 24px |
| `.modal-panel` | `min(620px, 100vw−32px)`, wide `min(820px, …)`; min-height `min(60vh,480px)` |

---

## 5. Borders, radii, shadows

### Radii (`calm.css:221-226`)

| Token | Value | Where |
|---|---|---|
| `--radius-xs` | 2px | inline `code`, `.cal-event-bar`, `.rb-sw`, tiny swatches |
| `--radius-sm` | 4px | icon-button hover targets, `.card-head-icon`, `.wave-cove-dot`, small inputs |
| `--radius-md` | 6px | **the workhorse** — rail rows, form fields, form buttons, `.cal-month-day`, toggles |
| `--radius-lg` | 8px | `.nav-item`, `.me-row`, `.cal-week-day`, `.rb-app`, `.report-activity-card` |
| `--radius-xl` | 10px | `.go`, `.login-input`, `.today-now-card`, `.report-convo-inputline` |
| `--radius-pill` | 999px | dots, badges, chips, avatars, scrollbar thumbs |
| `--r` (alias → xl) | 10px | **every card surface**: `.attn`, `.modal-panel`, `.codex-card`, `.term`, `.today-term`, `.today-card`, `.login-card`, `.settings-section`, `.add-panel-menu`, `.wave-list-item:focus-visible` (`calm.css:316-327`) |

One off-scale composite: `calc(--radius-xl + --space-1 + --space-px)` = 13px on `.report-activity-panel` (`calm.css:1790`).

### Borders

**Almost everything is 1px `--hairline`.** Non-1px borders in the entire stylesheet:

| Width | Site | `calm.css` |
|---|---|---|
| 3px left | `.side-section.attn-zone` warn stripe; `.rb-task--readonly` / `--draft`; `.cal-event` cove bar (as a grid column, not a border) | 617, 2255, 2259 |
| 2px left | `.calm-prose blockquote`, `.report-convo-entry--user` (accent), `--run/--tool/--reasoning/--edit` (hairline-strong) | 2058, 3376, 3414 |
| 1.5px | `.report-backlinks-group li` left edge; `.react-resizable-handle::after` corner | 3017, 6609 |
| 1.4px | `.rb-sw--box` legend swatch | 2392 |
| 1px dashed | `.card-unknown`, `.react-grid-placeholder` | 4921, 6562 |
| inset `0 0 0 1.5px` | `.cal-week-day.sel`, `.cal-month-day.sel` selection ring (box-shadow, not border) | 6219, 6285 |

### Shadows — **yes, but sparingly**

Three shadow tokens plus four one-offs:

| Token | Light | Dark | Used on |
|---|---|---|---|
| `--shadow` | `0 1px 2px rgba(20,30,55,.04), 0 12px 36px rgba(20,30,55,.06)` :328 | `0 1px 2px #0000004d, 0 12px 36px #0006` :380 | every card: `.attn`, `.codex-card`, `.term`, `.today-term`, `.login-card`, `.modal-panel`, `.add-panel-menu`, `.me-menu-popover`, `.rb-tip` |
| `--float` | 3-layer, `0 1px 1.5px / 0 5px 14px / 0 18px 44px` :330 | 3-layer heavier :382 | `.report-activity-panel` only |
| one-off | `0 1px 2px rgba(20,30,55,.05)` | — | `.cal-toggle button.on` (the segmented-control thumb) :6151 |
| one-off | `0 4px 14px .08, 0 22px 48px .12` | `.4 / .55` | `.react-grid-item.react-draggable-dragging` :6578 |
| ring | `0 0 0 3px var(--accent-soft)` | | focused inputs |
| ring | `0 0 0 2px var(--accent-soft)` | | focused rename targets, `.wave-list-item:focus-visible` |
| ring | `0 0 0 3px var(--warn-soft)` | | `.status-pill-dot.warn`, `.today-stat-dot.warn` — a *halo*, not a focus ring |
| inset | `inset 0 0 0 99px var(--accent-soft)` | | `.rb-table tr.rb-row-hi` (row highlight fill via shadow) :2449 |
| inset | `inset 0 -.42em 0 var(--accent-soft)` | | `.report-backlinks-quote b` — **highlighter-pen underline** :3038 |
| inset | `inset 0 1px 0 color-mix(text 5%)` | | `.report-activity-card + .report-activity-card` — hairline-as-shadow separator :1832 |

`--shadow` is a *very* low-contrast lift (0.04/0.06 alpha in light) — at card size it reads as a soft edge, not elevation. The design is **hairline-first, shadow-as-whisper**. `--veil` (62% light / 80% dark) + `backdrop-filter: blur(22px) saturate(170%)` is used exactly once, on `.report-activity-panel` (`calm.css:1791-1794`); `blur(14px) saturate(160%)` on `.rb-tip`; `blur(4px)` on `.modal-overlay`; `blur(2px)` on `.xterm-status-closed`.

---

## 6. Focus / hover / active / disabled

### Focus — inconsistent, and this is measurable

`:focus-visible` **is** used — 27 selectors in `calm.css`. The house ring is:

```
outline: 2px solid var(--accent); outline-offset: -2px;   /* inset — icon buttons, rail rows */
outline: 2px solid var(--accent); outline-offset:  2px;   /* outset — header buttons, links */
box-shadow: 0 0 0 3px var(--accent-soft); border-color: var(--accent);  /* text inputs */
box-shadow: 0 0 0 2px var(--accent-soft);                 /* rename targets, wave-list rows */
```

Four different focus treatments. But the real finding is what happens when you actually Tab (computed, both themes, `/calm/`, 14 Tab presses):

| Focused element | Ring rendered |
|---|---|
| `button.cove-row-delete`, `.side-wave-pin`, `.side-wave-delete`, `.me-row` | `2px solid oklch(.52 .13 245)` offset `-2px` — **the designed ring** |
| `button.cove-nav` | `1px auto rgb(16,16,16)` offset `0` — **Chromium UA default** |
| `button.side-wave` | `1px auto rgb(16,16,16)` — **UA default** |
| `button.today-term-host` ("reset") | **UA default** |
| `.cal-toggle button` (Week/Month) | **UA default** |
| `.cal-nav` arrows | **UA default** |

So **the two most-used navigation rows in the app (`.cove-nav`, `.side-wave`) have no designed focus ring at all** — they fall through to the browser's 1px black outline, which on `--bg #f9fbfd` is a hard black hairline and on dark is nearly invisible. `el.matches(':focus-visible')` was `true` in every case, so this is not a `:focus-visible` support issue — those selectors simply were never written.

### Hover — computed

| Element | Light hover bg | Dark hover bg |
|---|---|---|
| `.cove-nav` | `--overlay-hover-faint` (black 2.5%) → `#f3f5f7` | `--overlay-hover` (white 5%) |
| `.nav-item` | `--overlay-hover-strong` (black 5.5%) — *this is the `.active` value; the probed item was active* | white 6% |
| `.side-wave` / `.side-wave-row` | `--overlay-active` (black 5%) — **hover deliberately equals selected** (`calm.css:794`) | white 6% |
| `.me-row` | faint 2.5% | white 5% |
| `.cal-week-day`, `.cal-event`, `.cal-month-day`, `.wave-row`, `.today-now-card` | faint 2.5% | white 5–6% |
| `.side-collapse-toggle` | bg 4% **+ `border-color: --hairline-strong` + `color: --text`** | same |
| destructive icon buttons (`.cove-row-delete`, `.wave-row-delete`, `.card-grid-close`, `.side-wave-delete`) | `background: --warn-soft; color: --warn` | same |
| `.go` (primary) | `opacity: .88` | same |
| `.report-outline-list a`, `.report-rail-files row`, `.report-convo-close`, `.report-rail-open/close` | `background: --surface-card` | same |

Two parallel hover systems: the **overlay tiers** (rail, calendar, lists) and **`--surface-card`** (report subsystem). They don't produce the same value.

Hover-reveal is a core idiom: `opacity: 0 → 1` on `:hover`/`:focus-within` of the *wrapper*, for `.cove-row-delete`, `.side-wave-pin`, `.side-wave-delete`, `.wave-row-delete`, `.card-grid-close`, `.card-list-close`, `.cove-head-delete`, `.react-resizable-handle`. On `.cove-row` the badge **cross-fades out** as the `×` fades in (`calm.css:1091-1094`) — same x-column, opposite opacity.

### Active / pressed

`.nav-item:active` and `.side-wave:active` paint the selected background *before* navigation, so selection feels instant (`calm.css:807-808`). `.go:active { transform: scale(0.98) }` over `--motion-instant` (60ms) is the only transform-press in the app.

### Disabled

No single convention. Observed: `opacity: 0.5` (`.schema-form-submit`, `.new-task-form-submit`, `.dirpicker-select`), `0.4` (`.dirpicker-up`), `0.45` (`.card-head-action`, `.file-viewer-up`), `0.55` (`.report-convo-inputline--pending`), `0.6` (`.report-convo-stop/-reset/-load-earlier`), `0.68` (`.claude-restart-button`), `0.78` (`.rb-task--readonly`), `0.85` (`.report-convo-entry--compact`), and **colour-only** (`.report-convo-tab:disabled { color: --text-4 }`, `.fv-search-bar button:disabled { color: --text-3 }`). Cursor is `not-allowed` on forms, `default` on chips. **Nine disabled opacities.**

---

## 7. Motion

Six duration tokens (`calm.css:276-281`), all single-mode:

| Token | Value | Use |
|---|---|---|
| `--motion-instant` | 0.06s | `.go:active` press |
| `--motion-quick` | 0.1s | **default** — every hover/focus colour + background + opacity transition |
| `--motion-snappy` | 0.15s | `.react-grid-item` transform + box-shadow |
| `--motion-medium` | 0.24s | rail collapse (`width`, `padding`), chevron rotate, drawer, `grid-template-columns` |
| `--motion-slow` | 1s | `.conn-indicator-pulse`, `term-blink`, `.today-cursor` |
| `--motion-pulse` | 2.2s | `dot-pulse`, `clock-blink`, `.attn .label .pulse` |

Raw duration literals surviving in `served.css`: `1.8s` (`report-block-highlight`), `.9s` (`report-convo-dot-pulse`), `.12s`, `9999s` ×5 (the autofill suppression hack, `calm.css:413`), `.01ms` ×2 (reduced-motion).

Easing: `ease` (7), `ease-out` (11), `ease-in-out` (8), `steps(2, start)` for both cursor blinks, `cubic-bezier(.2,.8,.3,1)` once (`report-activity-drop`, `calm.css:1796`), `linear` once (`visibility 0s linear --motion-medium`, the drawer visibility delay trick). **No easing tokens exist** — keywords are inline everywhere (documented as deferred at `calm.css:266-267`).

Keyframes actually loaded in the running app (8): `conn-indicator-pulse`, `live-pulse`, `dot-pulse`, `pulse`, `report-block-highlight`, `report-convo-dot-pulse`, `term-blink`, `clock-blink`. (`calm.css` also declares `report-activity-drop`, `report-activity-breathe` for the panel not present in the running build.)

**`prefers-reduced-motion` is honoured, with a universal nuke** (`calm.css:6818-6827`): `animation-duration: 0.01ms !important; animation-iteration-count: 1 !important; transition-duration: 0.01ms !important; scroll-behavior: auto !important` on `*, ::before, ::after`. Two component-level overrides also exist (`.conn-indicator-dot`, `.report-convo-typing-dot` → `animation: none`), confirmed present in the live bundle. The comment justifies the nuke: nothing signals load state via motion alone, and no JS listens for `animationend`.

---

## 8. Component anatomy

**Rail section** — `.side-nav` / `.side-section`, flex column, `gap: 2px`, right padding 8px (scrollbar gutter). Header is `.nav-label`: 11px/700/uppercase/0.06em, `--text-2`, padding `12 10 4 10`, with an optional 20×20 `+` at the right edge (`.nav-label-row` / `.nav-label-add`).

**Rail group row** (`.cove-row` → `.cove-nav`) — 28.89px; padding `6 10 6 28`; radius 6px; 13/1.3/500; left 28px is a reserved column for the absolutely-positioned 20×20 chevron at `left: 6px` (rotates 90° on expand, 0.1s); right edge carries a 16×16 pill badge that cross-fades to a 20×20 `×` on row hover. Active: `--overlay-active` + weight 600 + `--text`. The whole group is wrapped in `.cove-block`, which paints a single `color-mix(cove-color 10%)` tint behind row + children so there is no seam.

**Rail leaf row** (`.side-wave-row` → `.side-wave`) — 28.25px; padding `6 36 6 46` (46 left = 6 + 22 chevron column + 6 + 12 swatch, computed to align with the group label above; 36 right reserves the delete slot); 12.5/1.3/400 → 500 + `--text` when active; radius 6px, `overflow: hidden` on the wrapper so children clip to it. Pin button 20×20 absolute at `left: 6px`, delete 20×20 absolute at `right: 10px`, both `opacity: 0` until row hover/focus-within.

**Content list row** (`.wave-row`) — `display: grid; grid-template-columns: 26px 1fr auto; gap: 14px`; padding `14px 60px 14px 0`; **`border-bottom: 1px --hairline`, no side or top borders, no radius, no background** — the row is defined purely by its baseline rule. Glyph cell is `place-items: center start` so the 7px status dot's *left edge* lands at x=0, aligned with the page `<h1>` (`calm.css:1325-1333`). Title 15/1.5/500 `--text`, sub-line 13/1.5 `--text-3`. Right gutter is exactly `2×28 + 4` for the two hover-revealed 28×28 buttons. Height at two lines: **72.25px**.

**Card** — `border: 1px --hairline; border-radius: 10px (--r); background: --paper; box-shadow: --shadow; overflow: hidden; height: 100%`. Applies verbatim to `.codex-card`, `.term`, `.file-viewer-card`, `.iframe-card`, `.today-term`, `.login-card`, `.modal-panel`, `.attn`. `.settings-section` and `.today-card` use the same shape **without** the shadow.

**Card head** (`.card-head`, shared slot component) — `padding: 10px 14px` with `padding-right: 8+22+8 = 38px` reserved for the absolute `.card-grid-close`; `gap: 10px`; `border-bottom: 1px --hairline`; `background: --surface-card`; **`line-height: 1`** so the row height is governed by the 20×20 icon, not by text leading (explicitly reasoned at `calm.css:2616-2624`). Computed height **41px**. Contents left→right: 20×20 `.card-head-icon` (rounded-4px letter avatar from an 8-hue L=62%/C=0.09 palette, `calm.css:4864-4871`), `.card-head-title` (13/1.0/500/uppercase/0.06em `--text`), then a right-aligned `.card-head-status` / `.card-head-actions` slot (26×26 icon buttons), then the hover-revealed 22×22 `×`. Doubles as the RGL drag handle (`cursor: grab` → `grabbing`).

**Badge / pill** — three families that never quite agree:
- **count badge**: 16×16 circle, 11/700, `font-variant-numeric: tabular-nums`; `.warn` = white on `--warn`, `.muted` = `--text-4` on transparent (`calm.css:1048-1064`).
- **chip**: `min-height: 18–22px`, `padding: 0 4–6px`, `border: 1px --hairline`, `border-radius: 999px`, 11/600, `line-height: 1`, tone variants swap border + bg + fg as a triple (`--accent-soft`/`--accent`, `--warn-soft`/`--warn`) — `.report-convo-chip`, `.report-convo-state`, `.wave-fs-viewer-chip`.
- **head pill**: `padding: 4px 6px`, 11/600/uppercase/0.06em, pill radius, `--warn-soft` bg — `.card-head-observing-pill`, `.card-head-exit-badge`.
- **status pill** (`.status-pill`) is *not* a pill at all: no background, no border, no padding — just a 7px `currentColor` dot + 12.5/500 text, coloured `--accent` when running / `--warn` when waiting.

**Icon button** — the canonical form: `display: grid; place-items: center; padding: 0; background: transparent; border: none; border-radius: 4–6px; color: --text-3; line-height: 1; transition: background/color 0.1s`. Hover → `--overlay-hover` + `--text`. Destructive variants hover → `--warn-soft` + `--warn`. Sizes 20 / 22 / 26 / 28. Glyphs are inline stroke SVGs at 10–16px, `stroke-width: 1.7–2.4`, `stroke: currentColor`, `fill: none`.

**Dialog** (`.modal-overlay` + `.modal-panel`) — scrim `rgba(20,30,55,.32)` light / `rgba(0,0,0,.55)` dark + `backdrop-filter: blur(4px)`, `z-index: 100`, 24px page padding. Panel: `min(620px, 100vw−32px)` × `min-height min(60vh,480px)` / `max-height 100vh−64px`, card chrome (10px radius, `--shadow`). Head: `padding 10px 12px`, `border-bottom` hairline, `background: --surface-card`, 13/500 — i.e. **deliberately the same head as an in-grid card**, so the modal reads as "a card detached from the grid" (`calm.css:3932-3935`). Body padding 14px. Wide variant `min(820px, …)` drops body padding entirely.

**Empty state** — no component; a bare paragraph at 12.5–13px `--text-3`, sometimes `font-style: italic` (`.cal-empty`, `.new-task-form-cove-resolving`), centred only in `.report-convo-empty` (padding `32px 0`, `text-align: center`). `.report-empty` / `.report-duplicate` do get card chrome (1px hairline, 8px radius, `--paper`, 16px padding).

**Report document** — 748px container, 616px prose measure, one shared left edge; figures/tables/apps "break out" to 748px instead of getting a card frame (`calm.css:1897-1900`). Headings carry a `counter(report-h2, decimal-leading-zero)` in `--accent` mono 11.5px before the text and a **full-width 1px rule after it** built from `::after { width: 100%; margin-inline-end: -100% }` — a zero-advance trailing rule (`calm.css:2114-2124`).

---

## 9. The five signature moves

1. **Hairline-only structure, shadow as a whisper.** Every boundary in the app is a 1px `oklch(92%)` line at 1.2:1 contrast. `--shadow` exists but at 0.04/0.06 alpha it does not read as elevation — it reads as a slightly softer edge. Rows are separated by a *single bottom border with no radius and no fill* (`.wave-row`, `.rb-table td`). A rewrite that reaches for `box-shadow` to indicate cards, or for filled row backgrounds, will feel like a different product immediately.

2. **The uppercase micro-label.** 11–13px, weight 600/700, `letter-spacing 0.06–0.08em`, uppercase, in `--text-2`/`--text-3`. It appears as `.nav-label` (COVES), `.card-head-title`, `.settings-section-title`, `.h-eyebrow`, `.cal-week-dow`, `.card-head-observing-pill`, `.report-convo-author`, `.rb-table thead th`, `.add-panel-menu-item`. It is the app's *only* decoration. Lose the tracking or the uppercase and every panel head goes flat.

3. **Hover-reveal with column alignment.** Destructive and secondary affordances are `opacity: 0` until the wrapper is hovered or focus-within, and they are absolutely positioned to land in a **shared x column** across row types — the pin, the chevron, the `×`, the count badge and the section `+` all line up vertically down the rail (`calm.css:811-812`, `1068-1072`). On `.cove-row` the badge fades *out* as the `×` fades *in*, in place. This is why the rail looks empty at rest and complete on hover.

4. **Selection is a 5% grey wash, and hover deliberately equals selection.** No accent fill, no left bar, no bold border — `.side-wave` hover and `.side-wave.active` are the *same* `--overlay-active`; only font-weight (400→500) and colour (`--text-2`→`--text`) change. `calm.css:794` states this as intent. It makes the rail feel like paper you're brushing rather than a menu you're clicking.

5. **Two densities on one screen, tuned by optical alignment not by a grid.** The 28px rail row and the 72px content row coexist; inside each, the left edges are hand-computed so the status dot's *ink*, the page `<h1>`'s first glyph, and the `+` of the new-row control all sit on x=0 (`calm.css:1325-1333`), and the rail's leaf-row 46px left padding is derived as `6 + 22 + 6 + 12` to land under the group label's text (`calm.css:1110-1112`). Plus the near-invisible surface ladder: bg → paper is ΔL 0.7%, terminal sits between them. The calm comes from *nothing being loud*, which is exactly what a rewrite loses by picking round numbers.

---

## 10. What is genuinely bad and should not be carried over

1. **`<h1>Settings</h1>` has no class.** It renders at Chromium's UA default 30px/45px/700 sans (computed, light, `/calm/settings`) next to a 36px/400 `.h-display` on `/calm/cove`. Two page titles, two sizes, two weights, one of them accidental. Visible in `settings-light.png` vs `cove-light.png`.

2. **`.synth` at 26px is a subtitle louder than most titles.** On `/calm/settings` the descriptive paragraph ("App-global preferences…") is 26px/1.3, larger than every heading in the report body except the H1. It buries the actual controls.

3. **`--text-4` fails AA everywhere (1.73–2.33:1) and is used for content, not decoration.** Table column headers (`.rb-table thead th`), nested outline links, timestamps, and system messages all sit on it. The "WCAG-exempt decorative" label at `calm.css:47` is not true of how it's used.

4. **`--warn` on `--warn-soft` fails AA in light (4.04:1)** and is the default chip pairing. `calm.css:467-472` explicitly avoids this pairing on `.conn-indicator` for exactly this reason, then uses it on `.report-convo-chip--warn`, `.report-convo-reset`, `.wave-fs-viewer-chip[data-tone=warning]`, `.report-convo-state[data-fsm=AwaitingInput]`. The same for `--text-3` on `--warn-soft`/`--accent-soft` in dark (4.33–4.41).

5. **Two of the three most-clicked rail rows have no focus ring.** `.cove-nav` and `.side-wave` fall through to Chromium's `1px auto rgb(16,16,16)` (measured by tabbing the live app). The stylesheet defines a house ring and applies it only to the small buttons *inside* those rows.

6. **`--overlay-hover-strong` is darker than `--overlay-active`** (composited: `#ebedef` vs `#edeef0` on light `--bg`). The "strong hover" outweighs the selected state. Four overlay tiers where two would do.

7. **Nine disabled opacities** (0.4 / 0.45 / 0.5 / 0.55 / 0.6 / 0.68 / 0.75 / 0.78 / 0.85) plus two colour-only disabled treatments. There is no disabled state; there are eleven.

8. **Five icon-button sizes and four button heights** with no rule for choosing between them. 20 vs 22 vs 26 vs 28 vs 30 px targets; 30 vs 32 vs 36 vs 40 px buttons. Every one of these is below the 44px touch minimum.

9. **The spacing ladder isn't a 4px grid.** 6, 10, 14, 20, 28 are 2px-grid values wearing 4px-grid names; then `calc()` composites silently add 36, 38, 44, 56. Either commit to 4px or admit it's 2px, but don't document one and ship the other.

10. **`--surface-panel-head` is byte-identical to `--surface-rail`** in both themes, and `--surface-terminal` is within 0.5% L of `--paper`. Three of the seven surface tokens carry no information.

11. **Two unreconciled hover systems.** The rail/calendar use `--overlay-*` alpha washes; the entire report subsystem uses `background: --surface-card`. They composite to different colours on the same page.

12. **The report subsystem re-litigated the whole type scale.** 9.5 / 10 / 11.5 / 12 / 19 / 23 px all appear only under `.report-*`, alongside `letter-spacing: .1em` and `.12em` literals that duplicate `--tracking-widest`. The #150/#165 consolidation never reached it.

13. **The 200px rail is too narrow for its own content.** `.side-wave-cove` is clamped to a "chosen empirically" 60px max-width and the wave title ellipsises at ~89px (computed). Titles in the rail are unreadable on both probed waves (`wave-dark.png`: "被引用方：…").

14. **Repo/build drift.** `web/src/calm.css` in both the worktree and the primary repo describes a **left** 250px report rail plus a 396px conversation drawer; the running build ships a **right** 280px rail with tabbed Report/Conversation and no drawer. Any spec written from the file alone will describe a layout that no longer exists.
