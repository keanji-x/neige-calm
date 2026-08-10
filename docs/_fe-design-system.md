# neige-calm 前端设计系统规范 (Design System Spec)

**Status**: proposal, round 1
**Scope**: `fe/web` rewrite (`.claude/worktrees/997-c1-today/fe/web/src`)
**Token authority**: `web/src/styles/tokens.css` — **FROZEN**. Every value below is expressed in an existing token, or filed in §14 Token Change Requests.
**Layer authority**: `@layer reset, vendor, tokens, base, astryx, ui, features, overrides` — **FROZEN**.

Every rule carries an ID (`DS-<AREA>-<NNN>`) and a tier:

| Tier | Meaning | Who enforces |
|---|---|---|
| `machine-checkable` | Decidable from source text / AST / CSS parse, no rendering | stylelint, eslint, contract test |
| `browser-tier` | Requires a rendered DOM — computed style, measured geometry, measured contrast | Playwright / browser-mode vitest |
| `review-only` | Requires human or model judgment; no honest proxy exists | design review gate |

I have not invented proxy checks for judgment calls. Where a rule is `review-only` I say so, and where a rule has a *checkable core* plus a judgment remainder, I split it into two rules.

---

## 0. Provenance and provisional status

Peer documents `docs/_fe-design-legacy-extract.md` and `docs/_fe-design-current-audit.md` **did not exist** when this was written. To avoid writing a mood board, I measured both inputs myself:

| Input | How obtained | Confidence |
|---|---|---|
| Legacy visual language | Direct measurement of `web/src/calm.css` (6810 lines, the legacy app's only stylesheet) | High — numbers, counts, selectors |
| Current rewrite state | Read all 14 CSS Modules under `fe/web/src` end-to-end | High — complete, not sampled |
| Contrast ratios | Computed oklch→sRGB→WCAG for every token pair (script in scratchpad) | Medium — arithmetic is exact, but browser oklch rendering must confirm (§7) |

**Provisional, pending the peer docs** — these are the only places where a peer input could overturn me:

| ID | Choice | Why provisional |
|---|---|---|
| P-1 | `--row-h-lg: 48px` for the two-line wave row | Legacy measures **66px**. I am deliberately tightening. If the legacy-extract argues the 66px was load-bearing (touch, drag-target, progress bar), revert to 56px. |
| P-2 | Primary button height 28px | Legacy primary `.go` is **36px** with 15px/600 text. I judge that oversized against a 13px base; legacy may have had a reason. |
| P-3 | Reserving `--text-xl` and above for the Today clock and hero empty states | Legacy uses `--text-xl` 4× and `--text-display` 4×; I have not seen all four sites. |
| P-4 | Dropping Astryx (§13) | Decided on evidence (§13), but if a peer doc shows a committed Astryx migration plan, this is a project decision, not a design one. |

Everything else is either measured, computed, or a stated design decision with a stated reason.

---

## 1. Principles

Six principles. Each has a consequence you can point at in code, and a counter-example that is *tempting*, not a strawman.

### DS-PRIN-001 — The row is the atom, not the card. `review-only`

This app's primary act is scanning a list of waves for the one that needs you. The unit of information is a **row on a shared background**, separated by rhythm and hairlines — not a card with its own padding, border, radius and surface.

- **Consequence**: lists render as rows at a fixed `--row-h`, with `gap: var(--space-1)` or less and no per-row border; the container carries the border, if anything does.
- **Counter-example (tempting, wrong)**: giving each wave in the cove list `border: 1px solid var(--hairline); border-radius: var(--radius-md); background: var(--surface-card); padding: var(--space-3)`. Twelve waves then cost twelve borders, twelve radii and 12×2 padding edges of visual noise, and the eye has to re-acquire the left text edge on every row. The current build does exactly this on the wave page card list.
- **This app is NOT** a dashboard of tiles.

### DS-PRIN-002 — Hierarchy is spent, not sprinkled. `review-only` (checkable core: DS-HIER-004)

Emphasis is a budget. Every surface has exactly one thing that is most important. Making a second thing "also important" does not raise it — it lowers the first.

- **Consequence**: one primary emphasis per surface, one primary action per surface, one accent-filled element per surface.
- **Counter-example**: the current cove page header, where the title (`--text-xl`), the count, "New wave" and "Delete" are all rendered at similar visual weight, so the page has no focal point and the destructive action is as loud as the creative one.
- **This app is NOT** a marketing page where every section fights for attention.

### DS-PRIN-003 — Calm under continuous change. `review-only`

Agents write to this UI while the human reads it. Anything that moves, flashes, or reflows on data arrival costs the user their place in a document they are actively reading.

- **Consequence**: state changes are expressed by a **static** change of tone/dot/badge, transitioned over `--motion-quick` at most; only one thing in the entire app may loop (`--motion-pulse` on the running indicator). No entrance animations, no staggered reveals, no skeleton shimmer.
- **Counter-example**: animating a wave row's height when its progress bar appears, or fading in the agenda list on every refetch. Both are standard "polish" and both are wrong here — the user re-enters this app hundreds of times a day, and a 300ms entrance is 300ms of nothing, hundreds of times.
- **This app is NOT** a demo. It is furniture.

### DS-PRIN-004 — Density is a promise, not a side effect. `machine-checkable` via DS-DENS-001..004

Row heights, control heights and rail width are **declared numbers**, not whatever padding plus line-height happens to produce.

- **Consequence**: rows set `min-block-size: var(--row-h)`; controls set `block-size: var(--control-h)`; padding is then chosen to centre within that box, not to create it.
- **Counter-example**: the legacy app arrived at ~28px rows in the rail, the calendar and the file list through **three independent padding decisions** that happened to converge. That is luck, and the rewrite has already lost it — its rows have no height rule at all, so a wave row with a two-line body and one with a one-line body are different heights in the same list.
- **This app is NOT** flow-typeset. It is gridded.

### DS-PRIN-005 — Colour carries meaning or it carries nothing. `review-only` (checkable core: DS-COLOR-010)

There is exactly one accent and it means *"this, here, now"*: selection, focus, the running state, the single primary action. Semantic colour (`--warn`, `--error`, `--success`) means a state the user must act on. Nothing is coloured for decoration.

- **Consequence**: a screenshot of a resting page should be near-monochrome, with accent pixels well under 2% of the surface.
- **Counter-example**: giving each cove a coloured identity swatch *and* colouring its rows *and* tinting its page header. The swatch alone does the job; the rest turns identity into noise and destroys the "accent = attention" contract.
- **This app is NOT** colour-coded.

### DS-PRIN-006 — Light and dark are two designs, not one design with a filter. `browser-tier`

Every surface, contrast and elevation claim must hold in both. Several token relationships **invert** between themes (§6), which means "card floats above background" is simply false in one of them.

- **Consequence**: elevation is never expressed by lightness direction; it is expressed by the surface's *name* and by hairlines.
- **Counter-example**: writing `background: var(--surface-card)` on a panel and calling it "raised". In light mode `--surface-card` (L 96%) is **darker** than `--bg` (L 98.8%) — it is recessed. In dark mode (L 21% vs 16%) it is raised. Same token, opposite reading.
- **This app is NOT** dark-mode-first with a light afterthought, nor the reverse.

---

## 2. Hierarchy: the channel budget

This is the section that fixes the current build's core failure: **everything is 400-weight, `--text` or `--text-2`, at 12.5px, on a transparent background** — so nothing reads as more important than anything else.

### 2.1 The channels

There are eight channels available. Each has a **cost** (how much attention it takes to notice) and a **reversibility** (whether it survives being scanned peripherally).

| # | Channel | Token vocabulary | Strength | Peripheral? | Primary use |
|---|---|---|---|---|---|
| C1 | **Size** | `--text-*` | Strongest | Yes | Page/document titles only |
| C2 | **Weight** | 400 / 500 / 600 (→ TCR-001) | Strong | Yes | Row titles, current-position, section labels |
| C3 | **Tone** | `--text` → `--text-2` → `--text-3` | Medium | Weak | Content vs. supporting vs. metadata |
| C4 | **Position** | order, `margin-inline-start: auto` | Medium | Yes | First = most important; right edge = status |
| C5 | **Spacing** | `--space-*` | Medium | Yes | Group membership; section separation |
| C6 | **Surface** | `--surface-*` | Medium | Yes | Region identity (rail vs. main vs. panel) |
| C7 | **Border / hairline** | `--hairline`, `--hairline-strong` | Weak | No | Boundary of an interactive box; region edges |
| C8 | **Accent / semantic colour** | `--accent`, `--warn`, `--error` | Strongest | Yes | Selection, focus, states needing action |

### 2.2 The budget rules

| ID | Rule | Tier |
|---|---|---|
| **DS-HIER-001** | A surface (a page, a dialog, a rail, a panel) has **exactly one** primary emphasis: the element with the largest `--text-*` on that surface. There is never a tie. | `browser-tier` — query the surface, group text nodes by computed `font-size`, assert the max bucket has exactly one element |
| **DS-HIER-002** | **At most two channels may stack on one element.** A heading earns emphasis from size **+** tone, or weight **+** tone — never size + weight + colour + border together. | `review-only`; checkable core in DS-HIER-003 |
| **DS-HIER-003** | Checkable core of DS-HIER-002: no single element may simultaneously set a non-inherited `font-size`, a `font-weight` ≥ 600, a `color` other than `--text`/`--text-2`, **and** a non-transparent `border-color`. Four channels on one element is always a defect. | `machine-checkable` (CSS Module rule-block analysis) |
| **DS-HIER-004** | **Level budget per surface**: at most **3** hierarchy levels among text roles (primary / supporting / metadata) and at most **4** distinct `--text-*` sizes. See DS-TYPE-010. | `browser-tier` |
| **DS-HIER-005** | C1 (size) may only carry hierarchy at the **page/document** level. Inside a row, a card, a header bar or a form, hierarchy is carried by C2 (weight) and C3 (tone) only. Rationale: size differences inside a 28px row cause baseline drift and break the horizontal scan line. | `machine-checkable` — no `--text-lg` or larger inside a component whose root sets `min-block-size: var(--row-h*)` |
| **DS-HIER-006** | C7 (border) never carries hierarchy. A border says "this is an interactive box" or "this region ends here". It never says "this is more important". | `review-only` |
| **DS-HIER-007** | C6 (surface) never carries hierarchy *within* a region — only between regions. Two sibling elements on the same surface may not be differentiated by giving one a `--surface-*` background at rest. Selection (`--accent-soft`) and hover (`--overlay-*`) are states, not hierarchy. | `machine-checkable` — sibling selectors in one module may not both set different `--surface-*` at rest |
| **DS-HIER-008** | C4 (position) is the **free** channel and should be used first. Anything that can be expressed by "put it first" or "push it to the right edge" must not also consume a colour or weight channel. | `review-only` |
| **DS-HIER-009** | De-emphasis is preferred to emphasis. To make X stand out, first try lowering everything around it one tone step; only then consider raising X. Rationale: a dense surface has far more non-important elements than important ones, so the cheap move is downward. | `review-only` |

---

## 3. Typography — the primary hierarchy carrier

Base is `--text-base` = 13px. The workhorses in the legacy app were `--text-sm` (69 uses), `--text-xs` (66) and `--text-base` (34) — 82% of all type. That distribution is correct and is preserved here.

### 3.1 The role table

Every text role in the app. Triple = **size / weight / tone**. Weight values pending TCR-001 (`--weight-normal|medium|semibold` = 400/500/600).

| ID | Role | Size | Weight | Tone | Leading | Tracking | Font |
|---|---|---|---|---|---|---|---|
| T-01 | Clock (Today, hero only) | `--text-display` | 500 | `--text` | `--leading-none` | `--tracking-tighter` | `--font-numeric` |
| T-02 | Empty-state hero | `--text-xl` | 500 | `--text-2` | `--leading-tight` | `--tracking-tight` | `--font-sans` |
| T-03 | Page title | `--text-lg` | 600 | `--text` | `--leading-tight` | `--tracking-tight` | `--font-sans` |
| T-04 | Document (report) H1 | `--text-lg` | 600 | `--text` | `--leading-tight` | `--tracking-tight` | `--font-sans` |
| T-05 | Document H2 | `--text-md` | 600 | `--text` | `--leading-snug` | `--tracking-normal` | `--font-sans` |
| T-06 | Document H3 | `--text-base` | 600 | `--text` | `--leading-snug` | `--tracking-normal` | `--font-sans` |
| T-07 | Document body / prose | `--text-base` | 400 | `--text` | `--leading-loose` | `--tracking-normal` | `--font-sans` |
| T-08 | Panel / card title | `--text-sm` | 600 | `--text` | `--leading-tight` | `--tracking-normal` | `--font-sans` |
| T-09 | Section label (rail groups, "CARDS") | `--text-xs` | 600 | `--text-3` | `--leading-none` | `--tracking-wider` + `uppercase` | `--font-sans` |
| T-10 | Row title (primary content in a row) | `--text-sm` | 500 | `--text` | `--leading-tight` | `--tracking-normal` | `--font-sans` |
| T-11 | Row secondary line | `--text-xs` | 400 | `--text-3` | `--leading-snug` | `--tracking-normal` | `--font-sans` |
| T-12 | Control label (button, tab, menu item) | `--text-sm` | 400 | `--text` / `--text-2` | `--leading-none` | `--tracking-normal` | `--font-sans` |
| T-13 | Form field label | `--text-xs` | 500 | `--text-2` | `--leading-none` | `--tracking-normal` | `--font-sans` |
| T-14 | Form hint / description | `--text-xs` | 400 | `--text-3` | `--leading-snug` | `--tracking-normal` | `--font-sans` |
| T-15 | Metadata (timestamps, counts, durations) | `--text-xs` | 400 | `--text-3` | `--leading-none` | `--tracking-normal` | `--font-numeric` + `tabular-nums` |
| T-16 | Machine identity (paths, ids, cwd, branch) | `--text-xs` | 400 | `--text-3` | `--leading-snug` | `--tracking-normal` | `--font-mono` |
| T-17 | Badge / pill label | `--text-xs` | 500 | per state | `--leading-none` | `--tracking-wide` | `--font-sans` |
| T-18 | Code block / terminal | `--text-sm` | 400 | `--text` | `--leading-base` | `--tracking-normal` | `--font-code` |
| T-19 | Breadcrumb (ancestor) | `--text-xs` | 400 | `--text-3` | `--leading-none` | `--tracking-normal` | `--font-sans` |
| T-20 | Breadcrumb (current) | `--text-xs` | 500 | `--text-2` | `--leading-none` | `--tracking-normal` | `--font-sans` |
| T-21 | Disabled text (any role) | inherit | inherit | `--text-4` | inherit | inherit | inherit |

### 3.2 Typography rules

| ID | Rule | Tier |
|---|---|---|
| **DS-TYPE-001** | Only three weights exist in the entire app: **400, 500, 600**. 300, 700, 800, `bold`, `bolder`, `lighter` are forbidden. | `machine-checkable` |
| **DS-TYPE-002** | Every `font-size` declaration must be a `var(--text-*)` token. No raw `px`/`rem`/`em` font sizes. (Legacy leaked 30 raw sizes including 7× `11.5px`, an **off-scale** value between `--text-xs` and `--text-sm`.) | `machine-checkable` |
| **DS-TYPE-003** | Every `line-height` must be a `var(--leading-*)` token or the literal `1` where it is a control box (prefer `--leading-none`). | `machine-checkable` |
| **DS-TYPE-004** | Every element that sets `text-transform: uppercase` must also set `letter-spacing: var(--tracking-wider)` or `--tracking-widest`. Uppercase at 11px without tracking is unreadable. | `machine-checkable` |
| **DS-TYPE-005** | `--tracking-tighter` / `--tracking-tight` may only be used at `--text-lg` and above. Negative tracking below 18px damages legibility at this density. | `machine-checkable` |
| **DS-TYPE-006** | **Weight substitutes for size, never adds to it.** If two elements are the same size, they may differ by weight. If they differ by size, they must **not** also differ by weight in the same direction. (T-03 at `--text-lg`/600 vs. T-08 at `--text-sm`/600 is legal — same weight, different size. `--text-lg`/600 vs `--text-sm`/400 for two peers on one surface is not.) | `review-only`; partial check in DS-HIER-003 |
| **DS-TYPE-007** | **Tone alone does the work** whenever the distinction is "same kind of thing, less important" — a secondary line, a hint, a timestamp. Reach for size or weight only when the distinction is "different kind of thing". | `review-only` |
| **DS-TYPE-008** | `--text-md` (15px) is reserved for document H2 (T-05). It may not appear in application chrome. Rationale: 15px sits 2px from body and 3px from `--text-lg`; used in chrome it reads as an accident. | `machine-checkable` |
| **DS-TYPE-009** | `--text-xl`, `--text-display-sm`, `--text-display` may appear **only** in the Today clock and empty-state heroes. *(Provisional — P-3.)* | `machine-checkable` (allowlist of files) |
| **DS-TYPE-010** | **Hard cap: at most 4 distinct computed `font-size` values may render on one surface simultaneously** (a page's `<main>`, or a dialog). Terminal/editor/document content is excluded — it is a viewport onto foreign text, not chrome. | `browser-tier` |
| **DS-TYPE-011** | No component may set `font-family` to a raw string. Only `var(--font-*)`. | `machine-checkable` |
| **DS-TYPE-012** | `font: inherit` is forbidden in `ui`/`features` modules. The `base` layer establishes inheritance for `button/input/select/textarea` once (DS-BASE-005). Currently 14 modules repeat it. | `machine-checkable` |

### 3.3 Numerals, times, durations

This app shows counts, clock times, elapsed durations and progress percentages that **change in place**. Proportional figures cause horizontal jitter on every tick.

| ID | Rule | Tier |
|---|---|---|
| **DS-NUM-001** | Any element whose text content can contain a digit that changes without a layout change must set `font-variant-numeric: tabular-nums` and `font-family: var(--font-numeric)`. Covers: counts, clocks, durations, percentages, byte sizes, dates in tables. | `machine-checkable` (class/attribute contract) + `browser-tier` (computed style on live counters) |
| **DS-NUM-002** | Numeric columns in any tabular layout are **right-aligned**; their headers are right-aligned with them. Text columns are start-aligned. Never centre a number. | `review-only` |
| **DS-NUM-003** | A count that trails a row (e.g. wave count on a cove row) is pushed with `margin-inline-start: auto` and takes tone `--text-3` (T-15). It never takes `--text-4` (§7 contrast) and never takes accent unless it is itself the attention signal, in which case it takes the semantic tone. | `machine-checkable` |
| **DS-NUM-004** | Durations and times use a fixed-width format chosen so the string length does not change (`04:07`, not `4:07`; `1h 02m`, not `1h 2m`). This is a formatting rule in `core`, not CSS. | `machine-checkable` (unit test on the formatter) |
| **DS-NUM-005** | A global `.tnum` utility is registered in `global-classes.yaml` and defined once in `base`. Components apply it rather than re-declaring the pair. | `machine-checkable` (manifest set-equality) |

### 3.4 Worked example — the wave page header

The current wave page header (`features/wave/page/page.module.css`) renders: back button, breadcrumbs, title, lifecycle badge, cwd path, Delete, and a "Cards" section label. Today they read as one undifferentiated block. Here is the exact assignment and the reason for each.

| Element | Role | Size | Weight | Tone | Font | Channels used | Why |
|---|---|---|---|---|---|---|---|
| Back (icon) | icon button `sm` | — | — | `--text-3` | — | C4 position | It is a *gesture*, not content. Leading position + smallest control size. Gets no tone above `--text-3`. |
| Breadcrumb ancestors | T-19 | `--text-xs` | 400 | `--text-3` | sans | C3 tone | Navigational context. Metadata tone: present, not competing. |
| Breadcrumb current | T-20 | `--text-xs` | 500 | `--text-2` | sans | C2 weight + C3 tone | "You are here" needs exactly one notch. **Weight, not size** — DS-HIER-005 forbids size inside a header bar. |
| **Wave title** | T-03 | **`--text-lg`** | **600** | **`--text`** | sans, `--tracking-tight` | C1 size + C3 tone | **The single primary emphasis of this surface** (DS-HIER-001). Size does the work; 600 is the *baseline* weight for T-03, not a third stacked channel. |
| Lifecycle badge | T-17 | `--text-xs` | 500 | per state | sans, `--tracking-wide` | C8 colour + shape | Status is a *different kind* of thing. It is carried by **shape** (pill + 6px dot) and semantic tone — never by size, so it can sit beside an 18px title without competing. |
| cwd path | T-16 | `--text-xs` | 400 | `--text-3` | **mono** | C3 tone + font family | Machine identity. The *font family* announces "this is a literal string"; tone pushes it down. Zero size/weight spend. |
| Delete | tertiary-destructive | `--text-sm` | 400 | `--text-2` | sans | C4 position (far right) | See DS-ACT-006: no colour at rest. Being on the far right, past a `--space-6` gap, is its whole signal. |
| "CARDS" section label | T-09 | `--text-xs` | 600 | `--text-3` | sans, `--tracking-wider`, uppercase | C2 weight + C5 spacing | It must be *findable* while scrolling but must never compete with the title. Weight + tracking + caps substitute for size (DS-TYPE-006), and it is the smallest size on the page. |

**Distinct sizes on this surface**: `--text-xs`, `--text-sm`, `--text-lg` = **3**, under the cap of 4 (DS-TYPE-010).
**Elements at the top size**: 1 (DS-HIER-001 satisfied).
**Deliberate change from the current build**: title drops from `--text-xl` (22px) to `--text-lg` (18px). 22px against a 13px base is a 1.7× jump that makes the header feel like a landing page; 18px (1.38×) is the ratio that dense tools use, and it leaves `--text-xl`+ genuinely reserved for the Today clock.

---

## 4. Action hierarchy

The largest gap in the current build. Measured fact: on the cove page, `.newWave` and `.delete` are byte-for-byte identical apart from two colour declarations — same border, radius, padding, font-size. In `new-wave`, `.cancel` and `.submit` are the same rule. "Create a thing" and "destroy a thing" are visually equal.

### 4.1 The four levels

| Level | Fill | Border | Text | Height | Padding-inline | Radius |
|---|---|---|---|---|---|---|
| **Primary** | `var(--accent)` | `1px solid var(--accent)` | `var(--bg)` | `--control-h` | `--space-6` | `--radius-sm` |
| **Secondary** | `var(--surface-chip)` | `1px solid var(--hairline-strong)` | `var(--text)` | `--control-h` | `--space-6` | `--radius-sm` |
| **Tertiary** | `transparent` | `1px solid transparent` | `var(--text-2)` | `--control-h` | `--space-4` | `--radius-sm` |
| **Destructive** | `transparent` | `1px solid transparent` | `var(--text-2)` | `--control-h` | `--space-4` | `--radius-sm` |

Note that all four have **identical geometry**. The differences are exactly two: fill and text tone. This is deliberate — a row of buttons must sit on one baseline with one height, and hierarchy must be legible in greyscale-plus-one-hue, not in shape.

Every level carries a transparent border at rest so that adding a border on hover/focus never changes layout.

### 4.2 Rules

| ID | Rule | Tier |
|---|---|---|
| **DS-ACT-001** | **At most one primary action per surface.** A surface is: a page's `<main>`, a dialog, a popover, or an inline form. Zero is allowed and common. | `browser-tier` — count `[data-action="primary"]` within each surface root, assert ≤ 1 |
| **DS-ACT-002** | Action level is declared by a `data-action="primary\|secondary\|tertiary\|destructive"` attribute on the control, not by an ad-hoc class name. This is what makes DS-ACT-001 and the state matrix checkable at all. | `machine-checkable` (every `<button>` in `ui`/`features` has the attribute) |
| **DS-ACT-003** | Primary is the **only** control permitted a solid `--accent` fill. Nothing else in the app may set `background: var(--accent)`. | `machine-checkable` |
| **DS-ACT-004** | `--accent-soft` as a *button* fill is forbidden. It is reserved for **selection state** (`[aria-selected]`, `[data-state=selected]`, `.rowActive`). The current build uses it for `.newWave` and `.submit`, which is why selection and "there is a button here" are indistinguishable. | `machine-checkable` |
| **DS-ACT-005** | The default level is **tertiary**. Secondary is used only when a control must be findable without reading (toolbar affordances, "Cancel" beside a primary). A page whose every button is secondary has no hierarchy — see DS-ANTI-004. | `review-only` |
| **DS-ACT-006** | **Destructive is not coloured at rest.** At rest it is visually identical to tertiary (`--text-2`, transparent). On `:hover` and `:focus-visible` it becomes `color: var(--error-text); border-color: var(--warn-border); background: var(--warn-soft)`. Rationale: a red control at rest on a page you visit 50× a day is an alarm that never stops ringing, and it teaches the user to ignore red. Colour must appear at the moment of intent. | `machine-checkable` (rest-state rule may not set a semantic colour) + `browser-tier` (hover computed style) |
| **DS-ACT-007** | A destructive action that is **irreversible** must be confirmed in a dialog. The dialog's confirm button is the **only** solid-danger control in the app: `background: var(--error); color: var(--bg); border-color: var(--error)`. Measured contrast 4.82 (light) / 7.93 (dark). | `machine-checkable` (allowlist: `ui/confirm-dialog` only) |
| **DS-ACT-008** | A destructive action is separated from any non-destructive action in the same group by at least `var(--space-6)`, and is never the first control in a group in reading order. | `machine-checkable` (declared gap) + `review-only` (ordering) |
| **DS-ACT-009** | In a dialog footer, order is `[Cancel] [Confirm]` with Confirm last (right, in LTR). Confirm is primary, or solid-danger for destructive. There is never a third button. | `browser-tier` |
| **DS-ACT-010** | Buttons never carry an icon *and* a label *and* a colour *and* a border weight change. Icon + label is the maximum decoration for primary/secondary; tertiary is label-only or icon-only. | `review-only` |
| **DS-ACT-011** | **Icon-only discoverability**: an icon button must (a) have a non-empty `aria-label`, (b) have an accessible tooltip on hover *and* focus, (c) change background on hover so its hit area is revealed, and (d) if it is destructive or otherwise not reversible by an obvious inverse, be duplicated in a menu with a text label. | (a) `machine-checkable`; (b)(c) `browser-tier`; (d) `review-only` |
| **DS-ACT-012** | **Hover-revealed actions** (`opacity: 0` until `:hover`/`:focus-within`) are permitted **only** for row-scoped shortcuts that are also reachable another way (context menu, or the item's own page). They must become visible on `:focus-within` (the current build does this correctly) and must reserve their space at rest so the row does not reflow. | `machine-checkable` (every `opacity: 0` reveal has a `:focus-within` sibling rule in the same file) + `review-only` (the "reachable another way" half) |
| **DS-ACT-013** | A pinned/toggled-on hover-revealed action stays at `opacity: 1` permanently, because otherwise the *undo* is undiscoverable. (Already encoded as `INV-SIDEBAR-012` in the current build — keep it.) | `browser-tier` |
| **DS-ACT-014** | Hovering a control never *removes* an existing background. `.newWave:hover { background: var(--overlay-hover) }` in the current build replaces the accent-soft fill with grey — the button goes **backwards** on hover. Hover always moves in one direction: toward more contrast. | `machine-checkable` (a `:hover` rule may not set `background` to an `--overlay-*` token when the rest rule sets a non-transparent background) |

### 4.3 State matrix — all control families

Required for every family. `—` means "no change from rest".

**Focus ring is the same everywhere (DS-FOCUS-001) and is omitted from the cells below.**

#### Row (rail cove row, rail wave row, wave list row, agenda event, menu-like list rows)

| State | Background | Text | Border | Other |
|---|---|---|---|---|
| rest | `transparent` | `--text` | `1px solid transparent` | — |
| hover | `--overlay-hover` | — | — | reveal row actions |
| active (pressed) | `--overlay-active` | — | — | — |
| focus-visible | — | — | — | ring, `outline-offset: -2px` |
| selected | `--accent-soft` | `--text` | `1px solid var(--accent)` | title weight 500→600 |
| selected + hover | `--accent-soft` | — | — | reveal row actions (background does **not** change — DS-ACT-014) |
| disabled | — | `--text-4` | — | `cursor: default`, actions hidden |
| attention (waiting) | — | title `--warn-text` | — | 6px dot `--warn` |

#### Icon button

| State | Background | Icon colour | Border |
|---|---|---|---|
| rest | `transparent` | `--text-3` | `1px solid transparent` |
| hover | `--overlay-hover-strong` | `--text` | — |
| active | `--overlay-active` | `--text` | — |
| focus-visible | — | — | ring |
| disabled | `transparent` | `--text-4` | — |
| selected / toggled on | `--accent-soft` | `--accent` | `1px solid var(--accent)` |

#### Primary button

| State | Background | Text | Border |
|---|---|---|---|
| rest | `--accent` | `--bg` | `--accent` |
| hover | `--accent` + `--overlay-active` composited over it (use a `::after` overlay, not a colour swap) | — | — |
| active | as hover, `translate: 0 var(--space-px)` | — | — |
| focus-visible | — | — | ring at `outline-offset: 2px` |
| disabled | `--surface-chip` | `--text-4` | `--hairline` |
| selected | n/a | | |

#### Secondary button

| State | Background | Text | Border |
|---|---|---|---|
| rest | `--surface-chip` | `--text` | `--hairline-strong` |
| hover | `--overlay-hover-strong` over chip | — | — |
| active | `--overlay-active` over chip | — | — |
| focus-visible | — | — | ring |
| disabled | `--surface-chip` | `--text-4` | `--hairline` |

#### Tertiary / destructive button

| State | Tertiary | Destructive |
|---|---|---|
| rest | transparent / `--text-2` / transparent border | **identical to tertiary** |
| hover | `--overlay-hover` / `--text` | `--warn-soft` / `--error-text` / `--warn-border` |
| active | `--overlay-active` | `--warn-soft` + `--overlay-active` |
| focus-visible | ring | ring + the hover treatment |
| disabled | `--text-4` | `--text-4` |

#### Text input / textarea

| State | Background | Text | Border |
|---|---|---|---|
| rest | `--paper` | `--text` | `--hairline-strong` |
| hover | — | — | `--text-4` |
| focus-visible | — | — | `--accent` + `box-shadow: 0 0 0 3px var(--accent-soft)` (ring, not elevation — DS-SURF-007) |
| disabled | `--surface-chip` | `--text-4` | `--hairline` |
| invalid | — | — | `--warn-border`, plus a message at `--error-text` |
| placeholder | — | `--text-3` | — |

#### Select

Same matrix as text input, plus: the chevron is `--text-3` at rest, `--text-2` on hover; `[data-state=open]` takes the focus border without the ring.

#### Menu item

| State | Background | Text |
|---|---|---|
| rest | `transparent` | `--text` |
| hover / roving-focus | `--overlay-hover` | `--text` |
| active | `--overlay-active` | — |
| focus-visible | ring, `outline-offset: -2px` | — |
| disabled | `transparent` | `--text-4` |
| selected / checked | `--accent-soft` | `--text`, leading check glyph in `--accent` |
| destructive item | `transparent` at rest; hover → `--warn-soft` / `--error-text` | |

#### Tab

| State | Background | Text | Indicator |
|---|---|---|---|
| rest | `transparent` | `--text-3` | none |
| hover | `--overlay-hover-faint` | `--text-2` | none |
| focus-visible | — | — | ring, `outline-offset: -2px` |
| selected | `transparent` | `--text`, weight 500 | 2px `--accent` bar on the inline-end edge of the tab strip |
| disabled | `transparent` | `--text-4` | none |

| ID | Rule | Tier |
|---|---|---|
| **DS-STATE-001** | Every control family above must implement **every** row of its matrix. A missing `:disabled` or `:focus-visible` rule is a defect, not an omission. | `browser-tier` — drive each primitive through all states and snapshot computed styles |
| **DS-STATE-002** | State is expressed by `data-state` / `aria-*` attributes (`aria-selected`, `aria-disabled`, `data-state="open|selected|checked"`), never by a bare class, so the matrix is checkable by attribute selector. | `machine-checkable` |
| **DS-STATE-003** | `opacity` is **never** used to express disabled. It compounds unpredictably when nested and silently drops metadata below any contrast floor. Disabled = `color: var(--text-4)` + `--surface-chip` fill + `cursor: default`. The current build uses `opacity: 0.5` in three places. | `machine-checkable` |
| **DS-STATE-004** | Hover and selection are **different channels** and must be simultaneously legible: hover is an `--overlay-*` composite, selection is `--accent-soft` + `--accent` border. A selected row that is also hovered shows both. | `browser-tier` |
| **DS-STATE-005** | No `:hover` rule may exist without the corresponding `:focus-visible` rule producing an equivalent or stronger affordance. Keyboard users must not see less than mouse users. | `machine-checkable` |

---

## 5. Spacing and rhythm

Base unit is **2px** (`--space-1`). The scale is 0, 1px, 2, 4, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32.

| ID | Rule | Tier |
|---|---|---|
| **DS-SPACE-001** | Every `padding`, `margin`, `gap`, `inset` and `translate` distance is a `var(--space-*)` token or a `calc()` over one. No raw `px`/`rem`. Exceptions: `1px` borders (use `--space-px` where it is a length, plain `1px` in `border` shorthand), and geometry set by density tokens (§6). | `machine-checkable` |
| **DS-SPACE-002** | **Row-level context** (anything inside a component whose root sets a `--row-h*` or `--control-h*`) may use only `--space-1, 2, 3, 4, 6`. Larger steps inside a 28px row are not spacing, they are a bug. | `machine-checkable` |
| **DS-SPACE-003** | **Section-level context** (gaps between sibling sections of a page, page padding) may use only `--space-6, 8, 9, 10, 11, 12`. | `machine-checkable` |
| **DS-SPACE-004** | `--space-5, 7` (10px, 14px) are **legacy-inherited odd steps**. They are permitted only in `overrides` and in components that must align with third-party geometry (xterm, react-grid-layout). New code uses the even ladder. | `machine-checkable` |
| **DS-SPACE-005** | **Inline gap ≤ section gap, always, on the same surface.** The gap between items in a list is strictly smaller than the gap between that list and the next section. Concretely: list item gap `--space-1`, list-internal group gap `--space-4`, section gap `--space-8`, major region gap `--space-10`. | `machine-checkable` (per-file ordering assertion) |
| **DS-SPACE-006** | Vertical rhythm for stacked rows: rows are `--row-h` tall with `gap: var(--space-1)` (2px), giving a 30px pitch. A list of rows must not mix pitches; a group header inside a list occupies exactly one `--row-h` slot. | `browser-tier` — measure successive row offsets, assert constant delta |
| **DS-SPACE-007** | Horizontal padding inside a row is `--space-3` (6px) at the inline edges when the row sits inside a bordered container, `--space-4` (8px) when it sits directly on a page surface. Rationale (measured): legacy converged on `6px 10px` for rail rows; 6px inline is the value that lets a 6px status dot + `--space-2` gap land the text at a 14px left edge that matches the section label's `--space-3` inline padding. | `machine-checkable` |
| **DS-SPACE-008** | Page padding is `--space-10` (24px) inline, `--space-9` (20px) block-start, `--space-11` (28px) block-end. The asymmetry is deliberate: extra bottom padding keeps the last row clear of the viewport edge when scrolled to the end. (Legacy: `.today-page` = 24/28/28, `.workbench` = 20/32/28.) | `machine-checkable` |
| **DS-SPACE-009** | Vertical margins are forbidden on content elements. All vertical spacing comes from the parent's `gap`. Rationale: margins collapse, `gap` does not, and margin-based rhythm cannot be verified by measuring one container. Exception: `.calm-prose` document flow, where margins are the correct model. | `machine-checkable` |
| **DS-SPACE-010** | No component sets both `gap` and per-child `margin` in the same axis. | `machine-checkable` |

---

## 6. Density

The rewrite currently has **no density discipline**: not one `min-block-size` on a row, not one fixed control height. Every number below is a declared value.

### 6.1 The numbers (→ TCR-002, TCR-003, TCR-004)

| Token | Value | Applies to |
|---|---|---|
| `--row-h-sm` | `24px` | Nested rail wave rows (compact), menu items, tree rows |
| `--row-h` | `28px` | Default single-line row: rail cove row, agenda event, file row, calendar day |
| `--row-h-lg` | `48px` | Two-line row: wave list row (title + meta) *(provisional P-1; legacy was 66px)* |
| `--control-h-sm` | `20px` | Icon buttons **inside a row** |
| `--control-h` | `28px` | Default: buttons, inputs, selects, tabs, chrome icon buttons |
| `--control-h-lg` | `32px` | Page-header primary action, login/dialog inputs |
| `--rail-w` | `200px` | Left rail, expanded (measured from legacy `.side`) |
| `--rail-w-collapsed` | `44px` | Left rail, icon strip (measured from legacy `.side--collapsed`) |
| `--panel-w` | `308px` | Right-hand panels: Today agenda column, activity panel (legacy `.today-grid`) |
| `--drawer-w` | `396px` | Conversation drawer (legacy `.report-page`) |
| `--measure-prose` | `616px` | Report body text, any paragraph run (legacy `.report-block`) |
| `--measure-form` | `544px` | Form/settings card content |
| `--measure-page` | `1180px` | Centred page content max-width (legacy `.today-page`) |
| `--measure-board` | `1280px` | Card board / workbench max-width (legacy `.workbench`) |

### 6.2 Density rules

| ID | Rule | Tier |
|---|---|---|
| **DS-DENS-001** | Every list row component sets `min-block-size` to one of `--row-h-sm\|--row-h\|--row-h-lg`. Height is never left to padding + line-height. | `machine-checkable` + `browser-tier` (measured heights are within 1px of the token) |
| **DS-DENS-002** | Every interactive control sets `block-size` (not `min-block-size`) to one of `--control-h-sm\|--control-h\|--control-h-lg`, and its `padding-block` is `0`. Vertical centring is by flex/grid, never by padding. | `machine-checkable` |
| **DS-DENS-003** | Exactly **three** row heights and **three** control heights exist app-wide. A fourth is a spec change, not a component decision. (Legacy drifted to six icon-button sizes: 20, 22, 24, 26, 28, 32.) | `machine-checkable` |
| **DS-DENS-004** | The rail is `--rail-w` / `--rail-w-collapsed` and nothing else. The current build's `17rem` (272px) is 36% wider than the legacy rail with no more content in it. | `machine-checkable` |
| **DS-DENS-005** | **Prose is capped at `--measure-prose` (616px). Tables, boards, terminals and code are not capped** — they fill their region. Applying a prose measure to a board is the single most common way to waste a wide screen. | `machine-checkable` (a container with `--measure-prose` may not contain a `[role=grid]`/board/terminal) |
| **DS-DENS-006** | Below `--rail-w + --measure-prose + 2×page-padding` ≈ **864px** the rail collapses to the icon strip rather than disappearing. It disappears only below 640px, where the mobile end takes over (deferred). The current build hides the rail entirely below 60rem/960px, which loses navigation on a perfectly usable 900px window. | `browser-tier` |
| **DS-DENS-007** | Hit targets: `--control-h-sm` (20px) is permitted **only** for controls inside a row that also has a larger primary target (the row itself). Standalone controls are ≥ `--control-h` (28px). This is a desktop WebView with a mouse; the 44px mobile guidance does not apply and would destroy the density. | `review-only` |
| **DS-DENS-008** | A row's content columns are declared with `grid-template-columns`, not flex + margins, so that rows in a list align on a shared column grid. Status glyph column is `6px`; the trailing metadata column is `auto`. | `review-only` |

---

## 7. Surfaces and elevation

### 7.1 The measured inversion

I computed each surface token's lightness against `--bg` in both themes:

| Token | Light L | Δ vs `--bg` (98.8) | Dark L | Δ vs `--bg` (16) | Consistent? |
|---|---|---|---|---|---|
| `--paper` | 99.5 | **+0.7** (lighter) | 19 | **+3.0** (lighter) | ✅ raised in both |
| `--surface-terminal` | 99 | +0.2 | 18 | +2.0 | ✅ raised in both |
| `--surface-rail` | 98 | **−0.8** (darker) | 15 | **−1.0** (darker) | ✅ recessed in both |
| `--surface-panel-head` | 98 | −0.8 | 20 | **+4.0** | ❌ **inverts** |
| `--surface-card` | 96 | **−2.8** | 21 | **+5.0** | ❌ **inverts** |
| `--surface-chip` | 95 | **−3.8** | 24 | **+8.0** | ❌ **inverts** |

**Consequence (DS-SURF-001)**: `--surface-card`, `--surface-chip` and `--surface-panel-head` **do not encode elevation**. They encode *"a bounded region of different material"*. Only `--paper` (up) and `--surface-rail` (down) are directionally stable. Any code comment or component name claiming a card is "raised" is wrong in one of the two themes.

### 7.2 The four levels

There are exactly four surface levels, and their meaning is **semantic**, not spatial:

| Level | Token | Meaning | Where |
|---|---|---|---|
| L0 **Ground** | `--bg` | The application floor. Everything sits on it. | `<body>`, page backgrounds, `<main>` |
| L1 **Recessed chrome** | `--surface-rail` | Navigation and persistent chrome — "not the content" | left rail, collapsed strip |
| L2 **Document** | `--paper` | Reading and writing surfaces — "this is content you can edit" | report document, terminal (`--surface-terminal`), text inputs |
| L3 **Material** | `--surface-card` / `--surface-chip` | A bounded object with its own edges — "this is a thing, not a region" | cards, badges, chips, panel bodies, disabled fills |

### 7.3 Rules

| ID | Rule | Tier |
|---|---|---|
| **DS-SURF-001** | Elevation is never expressed by lightness direction (see 7.1). Component names, comments and docs may not describe `--surface-card`/`--surface-chip` as elevated. | `machine-checkable` (comment/name lint) + `review-only` |
| **DS-SURF-002** | **Maximum surface nesting depth is 2.** L0 → L3 is fine. L0 → L3 → L3 (a chip inside a card) is fine because the chip is a *different level of the same rank* and must then be separated by a hairline, not by its fill. L0 → L3 → L3 → L3 is forbidden. | `browser-tier` (walk the tree, count non-transparent background ancestors) |
| **DS-SURF-003** | **The hairline is the default separator.** Reach for a surface change only when the region has a *different function* (rail vs. main), and for a shadow never (DS-SURF-006). Decision ladder, in order: (1) can `gap` alone group this? use gap; (2) does it need a visible boundary? use `1px solid var(--hairline)`; (3) is it a different functional region? change surface; (4) does it float above unrelated content? see DS-SURF-006. | `review-only`; ladder step (1) partially checkable via DS-ANTI-002 |
| **DS-SURF-004** | `--hairline` for separators between peers (list dividers, section rules, region edges). `--hairline-strong` for the border of an interactive box (input, secondary button, tab strip). Never the reverse. | `machine-checkable` |
| **DS-SURF-005** | A hairline and a surface change may not both be used for the same boundary unless the surfaces are adjacent levels whose measured Δ is under 1.0 L in either theme (which, per 7.1, is `--paper`↔`--bg` and `--surface-rail`↔`--bg` — i.e. exactly the two that need it). | `browser-tier` |
| **DS-SURF-006** | **Shadows are not used for elevation.** The frozen `tokens.css` contains no shadow token at all, which makes this the status quo — I am ratifying it, not inventing it. Depth comes from surface + hairline. The **only** exceptions are the four genuinely floating surfaces (menu, popover, dialog, toast), which need TCR-008 because a menu over a list with only a hairline is genuinely ambiguous about which layer receives the click. | `machine-checkable` (allowlist of 4 files) |
| **DS-SURF-007** | `box-shadow` **is** permitted for non-elevation purposes, which are exhaustively: (a) a focus ring `0 0 0 <n>px <colour>` — zero offset, zero blur; (b) an inset hairline `inset 0 0 0 1px <colour>` where a real border would break layout. Any `box-shadow` with a non-zero blur **and** a non-zero y-offset is an elevation shadow and requires the DS-SURF-006 allowlist. (Legacy already lived by this distinction: 20 of its 48 shadows were rings or inset hairlines.) | `machine-checkable` |
| **DS-SURF-008** | Radius by level: L3 material `--radius-md` (6px); floating surfaces (menu/dialog) `--radius-lg` (8px); controls `--radius-sm` (4px); pills/dots `--radius-pill`. `--radius-xs` and `--radius-xl` are unused and new uses require justification. | `machine-checkable` |
| **DS-SURF-009** | A control never has a larger radius than the surface that contains it. | `machine-checkable` |

---

## 8. Colour and state

### 8.1 Measured contrast

Computed WCAG ratios, oklch → sRGB. **All must be confirmed in a real browser** (DS-COLOR-013) — browsers gamut-map out-of-sRGB oklch differently, and `-apple-system` antialiasing shifts apparent contrast.

**LIGHT** (the constrained theme):

| fg \ bg | `--bg` | `--paper` | `--surface-rail` | `--surface-card` | `--surface-chip` | `--accent-soft` | `--warn-soft` |
|---|---|---|---|---|---|---|---|
| `--text` | 17.49 | 17.84 | 17.09 | 16.12 | 15.65 | 15.68 | 15.74 |
| `--text-2` | 7.18 | 7.33 | 7.02 | 6.62 | 6.43 | 6.44 | 6.46 |
| `--text-3` | 5.32 | 5.43 | 5.20 | 4.90 | **4.76** | **4.77** | **4.79** |
| `--text-4` | **2.07** | **2.12** | **2.03** | **1.91** | **1.86** | **1.86** | **1.87** |
| `--accent` | 5.28 | 5.39 | 5.16 | 4.87 | **4.72** | **4.74** | 4.75 |
| `--warn` | **4.47** | 4.56 | **4.37** | **4.12** | **4.00** | **4.01** | **4.02** |
| `--success` | **4.61** | 4.70 | **4.50** | **4.25** | **4.12** | **4.13** | **4.15** |
| `--error` | 4.82 | 4.92 | 4.71 | **4.44** | **4.31** | **4.32** | **4.34** |
| `--error-text` | 6.37 | 6.50 | 6.23 | 5.87 | 5.70 | 5.71 | 5.73 |

**DARK** is comfortable throughout: the lowest cell in the same table is `--text-3` on `--accent-soft` at 4.33, and everything else clears 4.5 with margin. `--text-4` in dark peaks at 2.33.

Three findings drive the rules:

1. **`--text-4` never clears 2.4:1 anywhere, in either theme.** It cannot carry readable text. `public.ts` already names it `--text-decorative`. The current build uses it for rail wave counts, wave-row lifecycle labels, the cwd path, the calendar day names, settings hints, card notes and the rail empty state — all real information.
2. **`--warn` fails 4.5 as text on every surface except `--paper`.** The lifecycle badge does exactly the failing thing: `background: var(--warn-soft); color: var(--warn)` at 11px = **4.01**.
3. **`--success` and `--error` are marginal in light**, failing on `--surface-card`/`--surface-chip`. `--error-text` exists precisely because someone hit this. `--warn` and `--success` lack their siblings → TCR-005, TCR-006.

### 8.2 The tone ramp — semantics

| Tone | Semantic role | What it is | Examples |
|---|---|---|---|
| `--text` | **Primary content** | The thing the user came for | row titles, page title, document body, input values, values in a key/value pair |
| `--text-2` | **Supporting content** | Real content, one level down; still meant to be read | form labels, secondary button text, "you are here" crumb, panel body text |
| `--text-3` | **Metadata** | True but incidental; read on demand, not on scan | timestamps, counts, durations, paths, hints, breadcrumb ancestors, placeholder text, **empty-state text** |
| `--text-4` | **Decoration & disabled** | Never carries information the user must read | separator glyphs (`›`, `·`), inactive dots, resting icon strokes, and text **only** on a `[disabled]`/`[aria-disabled=true]` element |

| ID | Rule | Tier |
|---|---|---|
| **DS-COLOR-001** | `--text-4` may set `color` **only** within a rule whose selector includes `:disabled`, `[disabled]` or `[aria-disabled="true"]`, or on an element that renders no text (dots, bars, separators). Everywhere else it is a defect. Rationale: measured 1.86–2.33:1. | `machine-checkable` |
| **DS-COLOR-002** | **Contrast floor: 4.5:1** for all text regardless of size. The WCAG large-text exemption (≥24px, or ≥18.66px bold) is **not** claimed anywhere — the app's largest chrome text is 18px/600, which does not qualify, and the Today clock is a glanceable number where 4.5 is trivially met. | `browser-tier` |
| **DS-COLOR-003** | **Non-text floor: 3:1** for anything that conveys state or bounds a control: status dots, progress fills, focus rings, the border of an input. Decorative hairlines (`--hairline` measures 1.22 light / 1.33 dark) are **exempt** because they are redundant with a surface or gap change — a hairline may never be the *sole* carrier of a meaning. | `browser-tier` + `review-only` (the "sole carrier" half) |
| **DS-COLOR-004** | `--warn` may not be used as `color` on text. Use `--warn-text` (TCR-005). `--warn` remains correct for dots, bars, borders and fills (non-text, ≥3:1). | `machine-checkable` |
| **DS-COLOR-005** | `--success` may not be used as `color` on text. Use `--success-text` (TCR-006). | `machine-checkable` |
| **DS-COLOR-006** | `--error` may not be used as `color` on text; `--error-text` exists for that. `--error` is for the solid-danger confirm fill (DS-ACT-007) and for dots/borders. | `machine-checkable` |
| **DS-COLOR-007** | These pairs **must be verified in a real browser, not by eye and not by this table**, because they are within 0.5 of the floor: `--text-3` on `--surface-chip`/`--accent-soft`/`--warn-soft` (light), `--accent` on `--surface-chip`/`--accent-soft` (light), every proposed `--warn-text`/`--success-text` value, `--bg` on `--accent` (primary button), `--bg` on `--error` (danger confirm), and every `--text-*` on `--surface-terminal`. | `browser-tier` |
| **DS-COLOR-008** | The contrast test enumerates the **cartesian product of every text tone × every surface token that is actually used as a background in the codebase**, in both themes, and asserts the floor. It does not spot-check. | `browser-tier` |
| **DS-COLOR-009** | Colour is never the **sole** carrier of a state. Every semantically coloured element also carries a shape or text difference: the running state is `--accent` **plus** a pulsing dot; attention is `--warn` **plus** the word; a destructive hover is red **plus** a border appearing. | `review-only` |
| **DS-COLOR-010** | **Accent budget**: at most **one** accent-*filled* element (solid `--accent` or `--accent-soft` background) per surface at rest, plus the currently selected row, plus the focus ring. Accent as `color` is limited to the selected item's leading glyph and inline links. A resting page screenshot should be < 2% accent pixels. | `browser-tier` for the "one filled element" core; `review-only` for the pixel budget |
| **DS-COLOR-011** | `--accent-soft` is **selection only** (see DS-ACT-004). It is not a button fill, not an info banner, not a highlight. | `machine-checkable` |
| **DS-COLOR-012** | Warn vs. error: **`--warn` = the system is waiting for the human** (wave needs input, approval pending, quota near). **`--error` = an operation failed and something must be repaired.** A waiting wave is never red; a crashed wave is never amber. | `review-only` |
| **DS-COLOR-013** | No raw colour literals in `ui`/`features`. Every `color`, `background`, `border-color`, `outline-color`, `fill`, `stroke` resolves to a `var(--*)` token from `public.ts`. | `machine-checkable` |
| **DS-COLOR-014** | Theme switching must not require any component to know the theme. No `[data-theme="dark"]` selector may appear outside `styles/tokens.css`. | `machine-checkable` |
| **DS-COLOR-015** | Cove identity swatches draw from a fixed, tested palette that is **not** `--accent` and **not** any semantic colour, so that identity never reads as state. Their only use is the 8px dot; they never tint a row, a header or a page. | `machine-checkable` (swatch colours appear only in the swatch component) |

---

## 9. Focus and keyboard

The legacy app got this right and the rewrite has thrown it away: legacy has ~40 `:focus-visible` blocks and 16 `outline: none` declarations **every one of which is paired with a replacement ring**. The rewrite has **one** `:focus-visible` rule in the entire codebase (a settings input).

### The single recipe

```css
/* in @layer base, declared exactly once */
:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
  border-radius: inherit;
}
:focus:not(:focus-visible) { outline: none; }
```

Two sanctioned variants, and no others:

| Variant | Recipe | When |
|---|---|---|
| **Inset** | `outline-offset: -2px` | The element is a full-width row, a tab, a menu item, or is otherwise clipped by an `overflow: hidden` ancestor. Legacy used this 18×. |
| **Input** | `outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-soft);` | Text inputs, textareas, selects — where a 2px outline outside a 1px border reads as a double border. Permitted under DS-SURF-007(a). |

| ID | Rule | Tier |
|---|---|---|
| **DS-FOCUS-001** | There is exactly one focus colour (`--accent`) and one focus width (2px) in the app. | `machine-checkable` |
| **DS-FOCUS-002** | **No file may contain `outline: none` / `outline: 0` without a replacement focus affordance defined in the same file for the same selector.** This is the rule the rewrite most needs. | `machine-checkable` |
| **DS-FOCUS-003** | Every interactive element must show a visible focus indicator. Verified by tabbing the whole surface and asserting a computed style delta at each stop. | `browser-tier` |
| **DS-FOCUS-004** | Focus ring contrast against **both** the element's own background and the surrounding surface is ≥ 3:1. `--accent` measures 5.28 (light) / 7.91 (dark) on `--bg`; the risky case is the ring against `--accent-soft` (selected row) — measured 4.74/5.92, passing, but must be browser-verified. | `browser-tier` |
| **DS-FOCUS-005** | Focus is never trapped except in a modal dialog, and a modal dialog always traps it, always restores it to the invoking element on close, and always closes on `Escape`. | `browser-tier` |
| **DS-FOCUS-006** | Lists and menus use **roving tabindex** (one tab stop for the list, arrows within), never one tab stop per row. A rail with 40 waves must not cost 40 tab presses. `ui/roving` exists for this. | `browser-tier` |
| **DS-FOCUS-007** | Anything reachable by hover must be reachable by keyboard. Every `:hover` reveal has a `:focus-within` twin (already true in the current build's row actions — preserve it). | `machine-checkable` |
| **DS-FOCUS-008** | Tab order follows visual order. No positive `tabindex` anywhere. | `machine-checkable` |
| **DS-FOCUS-009** | The focus ring is never animated or transitioned in (it must be instantly present when tabbing fast). `outline-color` may transition; `outline-width` and `outline-offset` may not. | `machine-checkable` |

---

## 10. Motion

### The ladder

| Token | Value | Sanctioned use | Properties |
|---|---|---|---|
| `--motion-instant` | 0.06s | Press feedback | `background-color`, `translate` |
| `--motion-quick` | **0.1s** | **The default.** Hover, tone change, opacity reveal | `background-color`, `color`, `border-color`, `opacity`, `outline-color` |
| `--motion-snappy` | 0.15s | Menu/popover open-close, disclosure chevron rotate | `opacity`, `transform`, `rotate` |
| `--motion-medium` | 0.24s | Dialog enter, drawer slide, rail collapse | `opacity`, `transform`, `grid-template-columns` (rail only) |
| `--motion-slow` | 1s | Indeterminate progress sweep | `transform` |
| `--motion-pulse` | 2.2s | **The single looping animation in the app**: the "running" indicator | `opacity` |

Legacy used `--motion-quick` for 38 of its 44 transitions. That is the correct concentration; this ladder ratifies it.

| ID | Rule | Tier |
|---|---|---|
| **DS-MOT-001** | Every `transition-duration` and `animation-duration` is a `var(--motion-*)` token. | `machine-checkable` |
| **DS-MOT-002** | **Animatable properties are exhaustively**: `opacity`, `color`, `background-color`, `border-color`, `outline-color`, `transform`, `translate`, `rotate`, `scale`, `fill`, `stroke`. Everything else is forbidden. | `machine-checkable` |
| **DS-MOT-003** | **Never animate**: `height`, `width`, `inline-size`, `block-size`, `margin`, `padding`, `top/left/right/bottom`, `font-size`, `flex-basis`, `gap`. These force layout on a surface that is receiving live agent output. Sole exception: the rail's `grid-template-columns` on explicit user collapse, at `--motion-medium` (legacy did this and it is a deliberate, user-initiated, once-per-session move). | `machine-checkable` |
| **DS-MOT-004** | **No entrance animation on route change, mount, or data arrival.** No staggered reveals, no fade-in lists, no skeleton shimmer. Rationale: DS-PRIN-003 — this app is re-entered constantly, and an entrance is a tax charged every time. | `machine-checkable` (no `animation` on a `:not(:hover):not(:focus)` root selector) + `review-only` |
| **DS-MOT-005** | At most **one** looping animation may be visible per surface, and it is `--motion-pulse` on the running indicator. Terminals, progress bars and log tails do not animate; they update. | `browser-tier` |
| **DS-MOT-006** | Transitions never apply to more than 200 simultaneously-rendered elements. A virtualized 1000-row list does not carry per-row transitions. | `review-only` |
| **DS-MOT-007** | **Every file containing `transition` or `animation` must contain a `@media (prefers-reduced-motion: reduce)` block that neutralizes it.** | `machine-checkable` |
| **DS-MOT-008** | In addition, `base` carries a global reduced-motion killswitch using `!important`. This is a **deliberate** use of the layer-inversion quirk (§4.1④ of the architecture doc): `!important` in an early layer beats later layers, so `base`'s killswitch cannot be defeated by any `features` module. | `machine-checkable` |
| **DS-MOT-009** | Reduced motion means *reduced*, not *broken*: state changes still happen instantly and remain legible; nothing becomes invisible because its reveal animation was suppressed. | `browser-tier` |
| **DS-MOT-010** | Easing: `ease-out` for anything appearing, `ease-in` for anything leaving, `ease-in-out` for the pulse only. No `cubic-bezier` literals, no spring, no bounce. | `machine-checkable` |

---

## 11. Component specs

Format for each: **anatomy → dimensions → states → what it deliberately does not do.**
Every component's state matrix is the one from §4.3 for its family; only deviations are restated.

### 11.1 Rail section

- **Anatomy**: section label (T-09) + optional trailing action (icon button `sm`) → list of rows.
- **Dimensions**: label occupies one `--row-h-sm` slot; `padding-inline: var(--space-3)`; `gap: var(--space-1)` between rows; `--space-6` between sections.
- **States**: label is not interactive. The trailing action follows the icon-button matrix and is visible at rest (not hover-revealed) because a section-level action has no row to be discovered from.
- **Deliberately does not**: collapse/expand (sections are stable, and a collapsed section hides state the user is monitoring); show a count badge (counts live on rows); use a divider (spacing does the work — DS-SURF-003 step 1).

### 11.2 Cove row

- **Anatomy**: `[disclosure chevron 16px] [identity dot 8px] [label, ellipsized] [count, tabular]`.
- **Dimensions**: `--row-h`; `grid-template-columns: 16px 8px minmax(0,1fr) auto`; `gap: var(--space-2)`; `padding-inline: var(--space-3)`; `--radius-sm`.
- **States**: Row matrix (§4.3). Selected shows `--accent-soft` + `--accent` border and label weight 500→600. The identity dot keeps its own colour in all states — it is identity, not state (DS-COLOR-015).
- **Deliberately does not**: tint its background with the cove identity colour; grow when it has children; show the delete action at rest (hover-revealed per DS-ACT-012, duplicated in the context menu).

### 11.3 Wave row

- **Anatomy**: `[status glyph 6px] [ title (T-10) · lifecycle (T-15, right) / meta line: cove tag · current activity (T-11) ] [progress hairline]` + hover-revealed pin/remove.
- **Dimensions**: `--row-h-lg` (48px); two-line body with `gap: var(--space-1)`; `padding-inline: var(--space-4)`; progress track `block-size: 3px` pinned to the block-end edge, full-bleed, `--surface-chip` track / `--accent` fill.
- **States**: Row matrix. Attention state colours the **title** `--warn-text` and the glyph `--warn` — two channels, both permitted since neither is size (DS-HIER-002). Running state colours the glyph `--accent` and pulses it.
- **Compact variant**: `--row-h`, single line, drops the meta line and the progress track. Used in the rail only.
- **Deliberately does not**: use a card border or a per-row surface (DS-PRIN-001); animate the progress fill width (DS-MOT-003 — it steps); show more than one hover action pair; wrap the title (always ellipsized, full text in `title`).

### 11.4 Page header

- **Anatomy**: `[back icon-btn] [breadcrumbs T-19/T-20]` / `[title T-03] [badge] [spacer] [actions]` / `[machine identity T-16]`.
- **Dimensions**: `gap: var(--space-3)` between the three lines; `--space-6` between the title block and the action group; `--space-8` below the header before content. Sticky to the top of `<main>`'s scroll container at `--z-sticky`, background `--bg`, with a `--hairline` block-end border that appears only when `[data-scrolled]`.
- **States**: sticky/scrolled (border appears, `--motion-quick` on `border-color` only).
- **Deliberately does not**: change size on scroll (no shrinking headers — they reflow the content the user is reading); carry more than one primary action; centre anything; use a surface different from the page.

### 11.5 Card / panel

- **Anatomy**: optional header (`--surface-panel-head`, T-08 title + trailing controls) → body (`--surface-card` or `--paper` for editable content) → optional footer.
- **Dimensions**: header `block-size: var(--control-h-lg)` (32px), `padding-inline: var(--space-4)`, `--hairline` block-end border; body `padding: var(--space-4)`; card `--radius-md`, `1px solid var(--hairline)`.
- **States**: rest / focus-within (border → `--hairline-strong`) / selected on a board (border → `--accent`) / dragging (border → `--accent`, `--motion-instant`).
- **Deliberately does not**: cast a shadow (DS-SURF-006); nest another card (DS-SURF-002); animate its resize; use `--radius-lg` (reserved for floating surfaces).

### 11.6 Badge / pill

- **Anatomy**: `[6px dot] [label T-17]`.
- **Dimensions**: `block-size: var(--control-h-sm)` (20px); `padding-inline: var(--space-2)`; `gap: var(--space-2)`; `--radius-pill`; `1px solid`.
- **States** (not interactive): neutral (`--surface-chip` / `--hairline` / `--text-3` / dot `--text-4`), attention (`--warn-soft` / `--warn-border` / **`--warn-text`** / dot `--warn`), running (`--accent-soft` / `--accent` / `--text` / dot `--accent`), error (`--warn-soft` / `--warn-border` / `--error-text` / dot `--error`).
- **Deliberately does not**: appear on every row in a list (DS-PRIN-005 — rows get a 6px dot + tone; the pill is for the detail header); become a button; use `--text-4` for its label (measured failure in the current build).

### 11.7 Icon button

- **Anatomy**: a single 14px or 16px glyph, optically centred, in a square box.
- **Dimensions**: `--control-h-sm` (20px) inside a row, `--control-h` (28px) in chrome; `--radius-sm`; `display: grid; place-items: center`; `padding: 0`.
- **States**: Icon button matrix (§4.3). Requires `aria-label` + tooltip (DS-ACT-011).
- **Deliberately does not**: exist without an accessible name; be the only route to a destructive action; sit at `--control-h-sm` outside a row; change its glyph on hover (only its box and stroke change).

### 11.8 Buttons — primary / secondary / tertiary / danger

Anatomy, dimensions and states are §4.1 and §4.3. Additional:

- **Anatomy**: `[optional 14px leading icon] [label T-12]`, `gap: var(--space-2)`.
- **Deliberately does not**: use a gradient; use a shadow; grow on hover; use `--radius-pill` (buttons are `--radius-sm`; pills are badges and the two must never be confusable); carry a loading spinner that changes the button's width (the label is replaced in place at fixed width, or the button is disabled with unchanged text).

### 11.9 Text input

- **Anatomy**: `[label T-13] [input] [hint T-14 | error T-14@--error-text]`, `gap: var(--space-1)`; field group `gap: var(--space-4)`.
- **Dimensions**: `block-size: var(--control-h)` (28px), or `--control-h-lg` in dialogs; `padding-inline: var(--space-3)`; `padding-block: 0`; `--radius-sm`; `1px solid var(--hairline-strong)`; background `--paper`.
- **Textarea**: `block-size: auto`, `min-block-size: calc(var(--control-h) * 3)`, `padding-block: var(--space-2)`, `resize: vertical`, `font-family: var(--font-mono)` when it holds a prompt or path.
- **States**: Input matrix (§4.3). Invalid also renders the message; the message is `--error-text`, never `--error`.
- **Deliberately does not**: use a floating/animated label; use a placeholder as a label (placeholder is `--text-3` and may only hold an example, never the field's name); use `--surface-bg` as its fill (the current settings input does, which makes the input lighter than its card in light mode and darker in dark — the recessed/raised reading inverts).

### 11.10 Select

- **Anatomy**: input anatomy + trailing 14px chevron, `padding-inline-end: var(--space-8)`.
- **Dimensions**: identical to text input — a select and an input in the same form must be pixel-identical in height and border.
- **States**: input matrix + `[data-state=open]`.
- **Deliberately does not**: use the native popup styling on desktop where a menu primitive is available; differ in height from a sibling input; show its value in `--text-3` (a chosen value is content: `--text`; only the unset placeholder is `--text-3`).

### 11.11 Menu

- **Anatomy**: floating surface → optional section label (T-09) → menu items → `--hairline` separator → destructive item last.
- **Dimensions**: `min-inline-size: 180px`, `max-inline-size: 320px`; `padding: var(--space-1)`; item `block-size: var(--row-h-sm)` (24px), `padding-inline: var(--space-3)`, `--radius-sm`; surface `--paper`, `1px solid var(--hairline)`, `--radius-lg`, `--z-overlay`, float shadow (TCR-008).
- **States**: Menu item matrix (§4.3). Roving tabindex; opens at `--motion-snappy` (opacity + 2px translate); closes instantly.
- **Deliberately does not**: nest more than one submenu level; contain form controls; scroll past 12 items without a search field; use hover-open on desktop click menus (click to open, hover to move between items once open).

### 11.12 Dialog

- **Anatomy**: scrim (`--overlay-scrim`) → panel: `[title T-08] [body] [footer: Cancel, Confirm]`.
- **Dimensions**: panel `--paper`, `1px solid var(--hairline)`, `--radius-lg`, `--z-modal`, `inline-size: min(var(--measure-form), calc(100vw - var(--space-12)))`; padding `--space-8`; footer `gap: var(--space-4)`, `margin-block-start: var(--space-8)`, right-aligned.
- **States**: enter at `--motion-medium` (opacity + `scale(0.98)`), exit instantly. Focus trapped, restored on close, `Escape` closes (DS-FOCUS-005).
- **Deliberately does not**: stack (one dialog at a time; a dialog that needs a dialog is a design failure); scroll the page behind it; exceed `--measure-form` in width; have three footer buttons (DS-ACT-009).

### 11.13 Empty state

Three sizes, chosen by how much room there is and by whether an action is possible.

| Variant | Where | Anatomy | Tone |
|---|---|---|---|
| **Inline** | An empty list inside a populated page | one line of T-11 text inside a `1px dashed var(--hairline)` box at `--row-h` | `--text-3` |
| **Region** | A whole panel/column is empty | T-14 line + one tertiary action | `--text-3` |
| **Page** | A whole page has no content | T-02 hero line + one **primary** action + the creation surface itself (see §12.4) | `--text-2` hero, `--text-3` sub |

| ID | Rule | Tier |
|---|---|---|
| **DS-EMPTY-001** | Empty-state and placeholder text is `--text-3`, never `--text` or `--text-2`. It must read as *"nothing here"*, not as content. | `machine-checkable` |
| **DS-EMPTY-002** | An empty container is drawn with a **dashed** `--hairline` border, distinguishing "a container with nothing in it" from "a container with something in it". Never a solid border, never a filled surface. | `machine-checkable` |
| **DS-EMPTY-003** | Empty states have **no illustration and no icon**. They have one sentence and, where an action is possible, that action. | `review-only` |
| **DS-EMPTY-004** | An empty state that has an obvious single next action is replaced by the affordance for that action, not by a description of it (§12.4). | `review-only` |

- **Deliberately does not**: apologise; explain the feature; occupy more vertical space than the content it replaces would.

### 11.14 Loading state

| ID | Rule | Tier |
|---|---|---|
| **DS-LOAD-001** | **No skeleton screens and no shimmer.** Rationale: skeletons are a bet that the layout is predictable, and this app's rows have variable content; a shimmering ghost of the wrong shape is worse than nothing, and it violates DS-PRIN-003. | `machine-checkable` (no `animation` on a `[data-loading]`/skeleton class) |
| **DS-LOAD-002** | Under ~200ms: render **nothing**. No spinner, no flash. | `review-only` |
| **DS-LOAD-003** | Over ~200ms: a single line of T-14 text (`--text-3`) in the region, e.g. `Loading waves…`. Reuse the empty-state inline variant's box. | `review-only` |
| **DS-LOAD-004** | **Refetching existing data shows the stale data plus a `--text-3` indicator, never a loading state.** The content does not disappear and does not move. | `browser-tier` |
| **DS-LOAD-005** | A control performing an action disables itself and keeps its label and its width; it does not swap to a spinner that resizes the button. | `browser-tier` |

- **Deliberately does not**: use a progress bar for unknown-duration work; block the whole page for a region-scoped fetch.

### 11.15 Error state

- **Anatomy**: `[6px dot --error] [message T-14 @ --error-text] [retry, tertiary]` in a `--warn-soft` box with `1px solid var(--warn-border)`, `--radius-md`, `padding: var(--space-3) var(--space-4)`.
- **Dimensions**: inline within the region that failed. Never a page-level banner unless the whole page failed.
- **States**: static; retry follows the tertiary matrix.
- **Deliberately does not**: use a toast for an error that has a place on screen (toasts are for the result of a completed action the user navigated away from); show a stack trace inline (a "Details" disclosure holds it, in `--font-mono` at `--text-xs`); use `--error` as the message colour (measured 4.31–4.44 on tinted surfaces — DS-COLOR-006).

---

## 12. Layout

### 12.1 The page frame

```
┌─────────────┬──────────────────────────────────────┐
│  rail       │  main                                 │
│ --rail-w    │  ┌────────────────────────────────┐  │
│ --surface-  │  │ page header (sticky, --z-sticky)│  │
│   rail      │  ├────────────────────────────────┤  │
│ own scroll  │  │ content (fills, own scroll)     │  │
│             │  └────────────────────────────────┘  │
└─────────────┴──────────────────────────────────────┘
   100dvh                    100dvh
```

| ID | Rule | Tier |
|---|---|---|
| **DS-LAY-001** | The shell is `display: grid; grid-template-columns: var(--rail-w) minmax(0,1fr); block-size: 100dvh` — a fixed viewport, **not** `min-height`. The current build uses `min-height: 100dvh`, which lets the whole app scroll as one document and breaks the sticky header and the independent rail scroll. | `machine-checkable` |
| **DS-LAY-002** | Exactly two scroll containers at the shell level: the rail and `<main>`. `<body>` never scrolls. Any additional scroll container must be inside a panel with a declared height. | `browser-tier` — assert `document.body.scrollHeight === clientHeight` |
| **DS-LAY-003** | Every page is `grid-template-rows: auto minmax(0, 1fr)` — header row, content row. The content row **fills**; a short page does not leave the header floating in a tall grey field. | `machine-checkable` |
| **DS-LAY-004** | Page content is capped at `--measure-page` (1180px) and **start-aligned**, not centred, when the viewport is wider — except boards, which run to `--measure-board` (1280px). Rationale: with a persistent left rail, centring the content column detaches it from the navigation. | `machine-checkable` |
| **DS-LAY-005** | Content columns declare `min-inline-size: 0` on every grid/flex child that can contain ellipsized text. (This is the bug that makes long wave names blow out a rail.) | `machine-checkable` |

### 12.2 The header pattern

One pattern, used by every page (§11.4): crumbs line → title line → identity line. A page with no ancestors omits the crumbs line; a page with no machine identity omits the third. The **title line is never omitted** and always contains exactly one T-03 element.

**DS-LAY-006** `machine-checkable` — every route component renders exactly one element with `data-page-title`.

### 12.3 Filling the viewport

**DS-LAY-007** `browser-tier` — **No page may render a contiguous empty rectangle taller than 240px** (≈ 5 rows at the standard pitch) within `<main>`'s visible area. This is the rule that catches the current build's huge grey void. It is measurable: rasterize `<main>`, find the largest axis-aligned rectangle containing no rendered element, assert its height.

### 12.4 What a sparse page shows instead

This is a design decision, not a filler policy. **When a region has nothing in it, it shows the affordance for putting something in it — at the size and position the content would occupy.** The empty state *is* the creation surface.

| Surface | Currently | Should be |
|---|---|---|
| **Cove page, no waves** | Empty dashed box + a "New wave" button in the header | The new-wave composer, **already expanded inline** where the first wave row would be, focused. Creating a wave is the only thing you can do on this page; making the user click a button to reveal the form is a wasted step *and* a wasted screen. |
| **Wave page, no cards** | Dashed "no cards" box; rest of the page grey | A ghost board: card-kind tiles (terminal / editor / chart) laid out on the board's actual grid, at the board's actual tile size, `1px dashed var(--hairline)`, label T-14. Clicking one creates that card in that slot. The empty board teaches the board's geometry. |
| **Wave page, no report yet** | — | The report document surface (`--paper`, `--measure-prose`) rendered empty with its editing affordance, so the document's shape is visible before it has content. |
| **Today, empty agenda** | Empty agenda column beside a small calendar; large void below | The agenda column collapses; the week calendar expands to full width; below it, "Recent waves" fills the remaining height with the wave-row list. There is always something to show on Today, because there is always history. |
| **Rail, no coves** | "No coves" in `--text-4` | Inline cove-creation input, already present, at the first row position. |

| ID | Rule | Tier |
|---|---|---|
| **DS-LAY-008** | A region whose emptiness has exactly one resolving action renders that action's affordance in place, at content size and position — not a description plus a button elsewhere. | `review-only` |
| **DS-LAY-009** | When a secondary column has no content, it collapses and its space is redistributed to the primary column, rather than remaining as an empty column. | `browser-tier` |
| **DS-LAY-010** | A page must never be *only* a header. If there is no primary content, the page shows the next-most-relevant list (recent waves, recent activity) rather than empty space. | `review-only` |

---

## 13. The `reset` and `base` layers, and Astryx

### 13.1 The missing reset/base is a real gap — and it is the proximate cause of the current ugliness

The app renders on browser defaults. The evidence is in the components themselves:

- **14 CSS Modules** each declare `font: inherit` on their controls, because nothing establishes it once.
- Nearly every `<button>` rule re-declares `background: none; border: none; padding: 0; text-align: start; cursor: pointer` — five declarations × ~25 buttons ≈ **125 lines of duplicated reset** living in feature files.
- Because each module re-establishes typography independently, **weight is never set anywhere** — `font: inherit` inherits 400 from `<body>`'s UA default, then `font-size` is overridden alone. That is the mechanical reason nothing in the app has hierarchy: the weight channel was never wired up.
- One `:focus-visible` rule exists in the whole app.

So the gap is not cosmetic; it is why §2's channel budget currently has only one usable channel.

### 13.2 What belongs where

Split rule: **`reset` may not use `var()`; `base` must.** (`reset` is element normalization; `base` is token application. This is checkable and it keeps the two from blurring.)

**`@layer reset`** — no `var()`:

| # | Content |
|---|---|
| R1 | `*, *::before, *::after { box-sizing: border-box }` |
| R2 | `html { -webkit-text-size-adjust: 100%; text-size-adjust: 100% }` |
| R3 | `body { margin: 0 }` |
| R4 | `h1,h2,h3,h4,h5,h6,p,figure,blockquote,dl,dd { margin: 0 }` |
| R5 | `ul,ol { margin: 0; padding: 0; list-style: none }` (restored inside `.calm-prose`) |
| R6 | `button,input,select,textarea { font: inherit; color: inherit; margin: 0 }` |
| R7 | `button { background: none; border: 0; padding: 0; text-align: start; cursor: pointer }` |
| R8 | `img,svg,video,canvas { display: block; max-inline-size: 100% }` |
| R9 | `[hidden] { display: none !important }` |
| R10 | `table { border-collapse: collapse }` |

**`@layer base`** — must use `var()`:

| # | Content |
|---|---|
| B1 | `body { font-family: var(--font-sans); font-size: var(--text-base); line-height: var(--leading-base); color: var(--text); background: var(--bg); -webkit-font-smoothing: antialiased }` |
| B2 | The single focus recipe (§9) |
| B3 | `::selection { background: var(--accent-soft); color: var(--text) }` |
| B4 | `::placeholder { color: var(--text-3); opacity: 1 }` (Firefox defaults placeholder opacity to 0.54 — without this, placeholders fall below any contrast floor) |
| B5 | `:root { color-scheme: light }` / `[data-theme=dark] :root { color-scheme: dark }` — so native scrollbars, form controls and the WebView chrome match |
| B6 | Scrollbar styling: thin, `--hairline-strong` thumb, transparent track |
| B7 | `.tnum` global utility (DS-NUM-005) |
| B8 | `.calm-prose` — the report document's typography (T-04..T-07, restored list markers, `max-inline-size: var(--measure-prose)`, `--font-code` for `code`/`pre` on `--surface-code`) |
| B9 | The reduced-motion killswitch with `!important` (DS-MOT-008) |

| ID | Rule | Tier |
|---|---|---|
| **DS-BASE-001** | `reset` and `base` layers exist and are non-empty. | `machine-checkable` |
| **DS-BASE-002** | No `var()` in `reset`; `base` rules that set a colour, font or size must use `var()`. | `machine-checkable` |
| **DS-BASE-003** | Every global class defined in `base` is registered in `global-classes.yaml`, with bidirectional set-equality. (Currently `[]` — it will hold `.tnum`, `.calm-prose`, and the CodeMirror ancestor hooks that §4.2 of the architecture doc warns must exist *before* the CM unlayered exceptions land.) | `machine-checkable` |
| **DS-BASE-004** | `ui`/`features` modules may not restyle a bare element selector. They style classes and attributes only. | `machine-checkable` |
| **DS-BASE-005** | After `base` lands, `font: inherit`, `background: none`, `border: none`, `padding: 0` and `cursor: pointer` on `button` are forbidden in `ui`/`features` — they are `base`'s job. | `machine-checkable` |
| **DS-BASE-006** | `!important` is permitted **only** in `reset`/`base` (where the layer inversion makes it a real killswitch) and in registered `overrides` entries. Never in `ui`/`features`. | `machine-checkable` |

### 13.3 Astryx: **drop it**

`@astryxdesign/core@0.1.3` is a dependency; `styles/vendor.css` imports its CSS into the `astryx` layer; **no component in the app uses it.**

Evidence gathered:

| Finding | Detail |
|---|---|
| Size | `dist/astryx.css` is **122 KB** of CSS shipped for zero components |
| Surface | ~120 components (`Button`, `Table`, `Calendar`, `CommandPalette`, `TreeList`, …) — a full competing design system |
| Vocabulary | Tailwind-shaped and **disjoint** from ours: `--font-size-base`, `--radius-element`, `--text-heading-1-size`, `--font-weight-semibold`, `--radius-chat`. Adoption means maintaining a hand-written bridge from our 8 type tokens and 6 radii to ~60 of theirs, in both themes, forever |
| Latent collision | `src/tailwind-theme.css` defines `--text-base` and `--radius-md` — **both of which are our token names**. They are not in `dist/astryx.css` today, so there is no live collision, but the frozen layer order puts `astryx` **after** `tokens`, so if a future release ships them, Astryx silently wins our two most-used tokens with no error |
| It ships its own reset | `src/reset.css` declares `@layer reset`. We are about to write our own (§13.2). Two resets is one too many, and the architecture doc's §4.1① records that the *last* reset-vs-component collision cost a spike to diagnose |

**Decision: drop the dependency; keep the `astryx` layer name in the frozen order as an empty slot.**

Reasons, in order of weight:

1. **The migration cost is zero today and monotonically increasing.** Zero components use it. Every week we keep it, the argument for keeping it gets more expensive to test and no more true.
2. **Its density is not our density.** A general-purpose component library targets a ~16px base and ~40px controls. This app is 13px base / 28px controls / 200px rail. Adopting Astryx means overriding geometry on every component — which is precisely the specificity war the layer architecture was introduced to end.
3. **We would own the same amount of CSS either way, plus a bridge.** Restyling 120 foreign components through `overrides` is more code than writing the ~14 primitives this app actually needs, and it leaves us unable to change a primitive without checking a vendor changelog.
4. **It is the only thing standing between us and a `reset`/`base` layer we fully control.** §13.2 is blocked on knowing whether a second reset is in the cascade.
5. **An unused dependency that ships CSS is a live hazard, not dead weight.** Its rules apply to our elements right now with nothing opting in.

**What we keep from it**: nothing in code, one idea in process — its component *inventory* is a good checklist of what a complete system needs (`EmptyState`, `Skeleton`, `Kbd`, `Timestamp`, `StatusDot`, `Toolbar`). Use it to audit coverage, not to import.

| ID | Rule | Tier |
|---|---|---|
| **DS-VEND-001** | `@astryxdesign/core` is removed from `package.json`, and its `@import` is removed from `styles/vendor.css`. The `astryx` layer name stays in the frozen `@layer` statement (removing it is a layer-order change, which is out of scope) and remains empty. | `machine-checkable` |
| **DS-VEND-002** | No new component-library dependency may be added without a written comparison against this spec's density numbers (§6) and token vocabulary. Behaviour-only libraries (focus management, floating positioning, virtualization) are exempt — they ship no CSS. | `review-only` |
| **DS-VEND-003** | Third-party CSS enters only via `@import ... layer(vendor)`. JS-side `import 'pkg/style.css'` is forbidden (it silently escapes the layer order — the architecture doc §4.2 flags three existing instances). | `machine-checkable` |

---

## 14. Token change requests

`tokens.css` is frozen; these are the genuinely missing values. Each states the exact name, both theme values, and the justification. **None of the rules above may be implemented by inventing a token silently.**

| ID | Token | Light | Dark | Justification | Priority |
|---|---|---|---|---|---|
| **TCR-001** | `--weight-normal`<br>`--weight-medium`<br>`--weight-semibold` | `400`<br>`500`<br>`600` | same | §2 identifies weight as the primary hierarchy channel *inside* rows and headers, where size is forbidden (DS-HIER-005). `tokens.css` has **zero** weight tokens, so the two existing weight uses in the app are raw `600` literals and DS-TYPE-001 ("only three weights exist") is unenforceable. ⚠️ `500` must be browser-verified: with the `-apple-system` stack it renders as a true medium on macOS/Windows, but may snap to 400 or 700 on Linux WebView depending on installed faces — if it snaps, T-10/T-20 fall back to tone-only. | **High** |
| **TCR-002** | `--row-h-sm` / `--row-h` / `--row-h-lg`<br>`--control-h-sm` / `--control-h` / `--control-h-lg` | `24px` / `28px` / `48px`<br>`20px` / `28px` / `32px` | same | The entire density section turns on these. Without tokens every module writes literals and DS-DENS-001..003 cannot be checked. Values are not invented: the legacy app converged on 28px rows in three independent subsystems (rail items, calendar events, file rows) and clustered its icon buttons at 20/28px. This makes the convergence a contract. | **High** |
| **TCR-003** | `--rail-w`<br>`--rail-w-collapsed`<br>`--panel-w`<br>`--drawer-w` | `200px`<br>`44px`<br>`308px`<br>`396px` | same | Measured from legacy `.side`, `.side--collapsed`, `.today-grid`, `.report-page`. The rewrite currently hardcodes `17rem` in one file and `22rem` in another for the same conceptual column. | **High** |
| **TCR-004** | `--measure-prose`<br>`--measure-form`<br>`--measure-page`<br>`--measure-board` | `616px`<br>`544px`<br>`1180px`<br>`1280px` | same | Measured from legacy `.report-block` / `.today-page` / `.workbench`. DS-DENS-005 (prose is capped, boards are not) is only checkable if the two have different token names. | Medium |
| **TCR-005** | `--warn-text` | `oklch(45% 0.16 30)` | `oklch(78% 0.13 30)` | **Measured failure.** `--warn` as text scores **4.00** on `--surface-chip`, **4.01** on `--warn-soft` and **4.12** on `--surface-card` in light mode — below the 4.5 floor. The lifecycle badge's attention variant does exactly this today. `--error-text` already exists for precisely this reason; `--warn` lacks its sibling. Proposed values measure 7.80 / 7.02 / 7.19 (light) and 9.62 / 7.36 / 8.78 (dark). | **High** |
| **TCR-006** | `--success-text` | `oklch(45% 0.14 145)` | `oklch(78% 0.13 145)` | Same failure class: `--success` as text measures **4.12** on `--surface-chip`, **4.25** on `--surface-card` (light). Proposed values measure 6.77 / 6.24 (light), 10.16 / 9.27 (dark). | Medium |
| **TCR-007** | `--text-on-accent` | `var(--bg)` | `var(--bg)` | The primary button needs a foreground for a solid `--accent` fill and no token names one. **`var(--bg)` already works** — measured 5.28 (light) / 7.91 (dark) — so this request is for *intent legibility*, not correctness. If declined, DS-ACT-001 uses `var(--bg)` with a comment. | Low |
| **TCR-008** | `--shadow-float` | `0 1px 2px oklch(0% 0 0 / 0.05), 0 12px 32px oklch(0% 0 0 / 0.08)` | `0 1px 2px oklch(0% 0 0 / 0.3), 0 12px 32px oklch(0% 0 0 / 0.45)` | DS-SURF-006 bans elevation shadows, with one carve-out: menus, popovers, dialogs and toasts float over unrelated content, and in dark mode a `--paper` menu over a `--surface-card` panel differs by 2% lightness — the boundary is genuinely ambiguous about which layer receives a click. Legacy had `--shadow` and used it on exactly these surfaces (modal panel, menu, login card). The rewrite's frozen tokens dropped it, which is why menus in the current build have no separation at all. Scoped to four components by allowlist. | Medium |

**Deliberately NOT requested**, so the next reader doesn't re-litigate:

- **Focus ring width/offset tokens** — the ring is declared exactly once, in `base`. A single-use value is not a token.
- **A shadow scale** (`--shadow-sm/md/lg`) — there is one float level, not a ladder. A scale invites elevation-by-depth, which §7.1 shows is not expressible in this palette.
- **More surface levels** — four is enough (§7.2) and a fifth would need a lightness step the palette does not have room for in light mode.
- **A second accent** — DS-PRIN-005.

---

## 15. Anti-patterns — what produced the current ugliness

Each of these was observed directly in `fe/web/src`, with the file. These are not hypotheticals.

| ID | Anti-pattern | Where | Why it looks bad | Tier |
|---|---|---|---|---|
| **DS-ANTI-001** | **No weight channel anywhere.** `font-weight: 600` appears exactly twice in 1511 lines of CSS Modules. Everything else inherits 400. | app-wide | This is *the* reason nothing has hierarchy. Titles differ from metadata only by a 5.5px size step and a tone step; at a glance the page is one grey texture. | `machine-checkable` |
| **DS-ANTI-002** | **Card-ing everything.** `today.module.css` `.card`, `wave/page` `.card`, `settings` `.card` — every group gets `border + radius + surface + padding`. | 3 files | Four separators for one boundary. Twelve of them on one page is a quilt. Per DS-SURF-003 most of these need only a `gap`. | `review-only` (core: DS-SURF-003) |
| **DS-ANTI-003** | **`--text-4` used for real information.** rail wave counts, wave-row lifecycle label, cwd path, calendar day names, settings hints, card notes, rail empty state. | 6 files | Measured **1.86–2.33:1**. It is not "subtle", it is unreadable — and it makes the page look faded rather than calm. | `machine-checkable` |
| **DS-ANTI-004** | **Every button is the same button.** `.newWave` and `.delete` differ by two colour declarations; `.submit` and `.cancel` are one shared rule. | `cove/page`, `cove/new-wave` | Create and destroy carry equal weight. The user cannot pre-attentively find the action they want, so every action costs a read. | `machine-checkable` (DS-ACT-002) |
| **DS-ANTI-005** | **Hover runs backwards.** `.newWave:hover { background: var(--overlay-hover) }` replaces the `--accent-soft` fill with grey. Same in `.submit:hover`. | `cove/page`, `cove/new-wave` | The button gets *less* prominent when you point at it. | `machine-checkable` (DS-ACT-014) |
| **DS-ANTI-006** | **`--accent-soft` used as a button fill.** | `.newWave`, `.submit` | It is the selection colour. Once buttons wear it, a selected row and a button are the same object. | `machine-checkable` (DS-ACT-004) |
| **DS-ANTI-007** | **No focus rings.** One `:focus-visible` rule exists, on a settings input. ~25 buttons have none. | app-wide | Keyboard navigation is invisible. The legacy app had ~40 such rules — this is a regression, not an omission. | `machine-checkable` (DS-FOCUS-003) |
| **DS-ANTI-008** | **No row heights.** Zero `min-block-size` on any row. Heights are padding + line-height accidents. | all list components | A one-line and a two-line row in the same list are different heights; the eye has no rhythm to lock onto, which reads as "unfinished" more than any single wrong value would. | `machine-checkable` (DS-DENS-001) |
| **DS-ANTI-009** | **`opacity: 0.5` as disabled.** 3 sites. | `new-wave`, `settings` ×2 | Compounds when nested; drives `--text-3` text to ~2.4:1; and it fades the *border* too, so the control loses its shape as well as its label. | `machine-checkable` (DS-STATE-003) |
| **DS-ANTI-010** | **Surface inversion between themes.** `settings .input` sits on `--surface-bg` inside a `--surface-card` card. | `settings` | In light the input is *lighter* than the card (reads raised); in dark it is *darker* (reads recessed). The same component means two opposite things. | `browser-tier` |
| **DS-ANTI-011** | **`min-height: 100dvh` shell instead of `block-size: 100dvh`.** | `shell.module.css` | The whole app becomes one scrolling document: the rail scrolls away, no header can be sticky, and a short page leaves a full-viewport grey field below the fold. Directly responsible for the "huge empty area". | `machine-checkable` (DS-LAY-001) |
| **DS-ANTI-012** | **Rail hidden entirely below 60rem.** | `shell.module.css` | On a 900px window — a completely normal split-screen width — navigation vanishes with no replacement. Should collapse to the 44px icon strip. | `browser-tier` (DS-DENS-006) |
| **DS-ANTI-013** | **`font: inherit` in 14 modules; `background: none; border: none; padding: 0` on ~25 buttons.** | app-wide | ~125 lines of reset living in feature files. Beyond duplication: it means each component's typography is established independently, which is *why* DS-ANTI-001 happened. | `machine-checkable` (DS-BASE-005) |
| **DS-ANTI-014** | **Raw px for repeated geometry.** `6px` dots ×6, `2px` gaps ×4, `28px`/`22px`/`24px`/`18px`/`20px` control sizes, `26px` magic offset in `row.module.css`. | 8 files | Five icon-button sizes emerged with no decision behind any of them. The `inset-inline-end: 26px` is derived from another component's width by hand and will silently desync. | `machine-checkable` (DS-SPACE-001, DS-DENS-003) |
| **DS-ANTI-015** | **Uppercase micro-labels at inconsistent tracking and tone.** `.sectionTitle` is `--text-xs`/`--tracking-wider`/`--text-4`; `.cardTitle` is `--text-sm`/`--tracking-wide`/`--text-3`. | `shell`, `today`, `settings` | Two labels doing the same job at two sizes, two trackings and two tones. Per T-09 there is one section-label style. | `machine-checkable` |
| **DS-ANTI-016** | **Semantic colour used as text on tinted fills.** `.badge.attention` = `--warn` on `--warn-soft` = **4.01**. `.eventLifecycleAttention`, `.delete:hover`, `.coveDelete:hover` likewise. | `lifecycle-badge`, `today`, `wave/page`, `shell` | Fails the contrast floor, and low-contrast red/amber reads as "muddy" rather than "urgent" — the state loses the very salience it was coloured for. | `machine-checkable` (DS-COLOR-004) + `browser-tier` |
| **DS-ANTI-017** | **Two dot sizes for the same meaning.** Status dots at 8px (`today .dot`), 6px (`wave/row .glyph`, `badge .dot`, `page .coveDot`) and 5px (`today .dayDot`). | 4 files | Three sizes, one meaning. | `machine-checkable` |
| **DS-ANTI-018** | **No transitions at all** (one keyframe animation aside). | app-wide | Hover states snap with no acknowledgement, which reads as unresponsive rather than fast. `--motion-quick` (0.1s) on background/colour is the whole fix. | `machine-checkable` |

---

## 16. Rule index by tier

For the gate-building agent. Counts are of the numbered rules above.

| Tier | Rules |
|---|---|
| **machine-checkable** | DS-HIER-003, 005, 007 · DS-TYPE-001..005, 008, 009, 011, 012 · DS-NUM-001, 003, 004, 005 · DS-SPACE-001..005, 007..010 · DS-DENS-001..005 · DS-SURF-004, 006..009 · DS-COLOR-001, 004..006, 011, 013..015 · DS-ACT-002..004, 006..008, 012, 014 · DS-STATE-002, 003, 005 · DS-FOCUS-001, 002, 007..009 · DS-MOT-001..004, 007, 008, 010 · DS-EMPTY-001, 002 · DS-LOAD-001 · DS-LAY-001, 003..006 · DS-BASE-001..006 · DS-VEND-001, 003 · DS-ANTI-001, 003..009, 011, 013..018 |
| **browser-tier** | DS-PRIN-006 · DS-HIER-001, 004 · DS-TYPE-010 · DS-NUM-001 (computed) · DS-SPACE-006 · DS-DENS-001 (measured), 006 · DS-SURF-002, 005 · DS-COLOR-002, 003, 007, 008, 010 · DS-ACT-001, 006 (hover), 009, 011, 013 · DS-STATE-001, 004 · DS-FOCUS-003..006 · DS-MOT-005, 009 · DS-LOAD-004, 005 · DS-LAY-002, 007, 009 · DS-ANTI-010, 012, 016 |
| **review-only** | DS-PRIN-001..005 · DS-HIER-002, 006, 008, 009 · DS-TYPE-006, 007 · DS-NUM-002 · DS-DENS-007, 008 · DS-SURF-001, 003 · DS-COLOR-003 (partial), 009, 012 · DS-ACT-005, 010, 011 (partial), 012 (partial) · DS-MOT-004 (partial), 006 · DS-EMPTY-003, 004 · DS-LOAD-002, 003 · DS-LAY-008, 010 · DS-VEND-002 · DS-ANTI-002 |

**Honest note on the review-only set**: the hierarchy and action-priority rules (DS-PRIN-002, DS-HIER-002/008/009, DS-TYPE-006/007, DS-ACT-005/010) are the heart of this spec and are the *least* checkable. I have extracted a checkable core from each where one exists — DS-HIER-003 (four-channel stacking), DS-ACT-001/002 (one primary per surface, declared by attribute), DS-HIER-001 (one top size per surface), DS-TYPE-010 (four sizes per surface) — and left the remainder honestly unautomated rather than inventing a proxy that would pass while the page still looks flat. A checker that reports "0 violations" on a page that has no hierarchy would be worse than no checker.

---

## 17. Comparative grounding — what is borrowed, and what is rejected

Decisions, with the reason each fits or does not fit *this* app.

### Borrowed

| Source | What is borrowed | Why it fits here |
|---|---|---|
| **Linear** | (a) The row-as-atom model: full-width rows on a shared background, hover-revealed row actions, ~28px pitch. (b) Compressed type scale — 13px body, 18px page title, a 1.38× ratio rather than the 1.5–2× a content site uses. | Our primary act is scanning a wave list for the one that needs attention. Linear solved exactly this for issues. The compressed ratio is what lets a page title coexist with a dense list without the header dominating. |
| **VS Code** | (a) A **recessed** navigation rail (`--surface-rail` darker than `--bg` in *both* themes — one of only two directionally stable surfaces we have). (b) Uppercase 11px tracked section labels as the rail's only structural device. (c) Collapse-to-icon-strip rather than hide. | The rail is permanent chrome, not content; recessing it is the cheapest way to say so without a border. The 44px collapsed strip is measured from our own legacy, which independently converged on VS Code's model. |
| **Raycast** | The discipline that **hover ≠ selection**: hover is a neutral overlay, selection is an accent tint plus an accent border. Both must be simultaneously legible. | In a keyboard-driven list, the user's pointer is often resting somewhere unrelated. If hover and selection use the same treatment, "where am I" becomes ambiguous — DS-STATE-004. |
| **Bloomberg-class terminals** | (a) Tabular figures everywhere a digit can change in place. (b) Right-aligned numeric columns. (c) Monospace as a *semantic* marker for machine-literal strings (paths, ids, cwd, branches) rather than as a style choice. | The app displays live counts, elapsed durations and progress that tick while being read. Proportional figures cause per-second horizontal jitter. Monospace-as-semantics also means we spend a *font-family* channel instead of a colour channel on "this is a literal" (see the cwd row in §3.4). |
| **GitHub Primer** | (a) The text-tone ramp semantics (`fg.default` / `muted` / `subtle` → our `--text` / `--text-2` / `--text-3`) with **assigned meanings**, not just decreasing greys. (b) Danger buttons that are tertiary at rest and coloured only on hover/focus. | (a) is what turns a 4-step grey ramp into a hierarchy channel — §8.2. (b) is DS-ACT-006, and Primer's rationale is ours: on a page visited constantly, a permanently red control is an alarm that has stopped meaning anything. |
| **Radix / shadcn** | The **behaviour** contracts only: focus trap, roving tabindex, and `data-state="open\|selected\|checked"` as the styling hook. We already have `ui/dialog`, `ui/menu`, `ui/focus`, `ui/roving`. | Adopting the attribute convention (DS-STATE-002) is what makes the entire §4.3 state matrix machine-checkable by attribute selector instead of by className archaeology. We take the API shape and write our own CSS. |
| **IBM Carbon** | The idea that density is a **named, explicit variant** (Carbon's `sm`/`md`/`lg` row heights) rather than an emergent property of padding. | This is precisely the discipline our legacy lacked — it hit 28px rows three separate times by accident. DS-DENS-003 makes three heights a contract with a spec-change cost to add a fourth. |
| **Apple HIG** | (a) The system font stack (already `-apple-system` first in `--font-sans`). (b) The principle that the focus indicator is system-consistent and never removed without replacement. | (a) In a WebView, the system font is the one that renders correctly at 11–13px on the target platform. (b) is DS-FOCUS-002 — and our legacy already lived by it (16 `outline: none`, all paired). |

### Rejected

| Source | What is rejected | Why |
|---|---|---|
| **Material 3** | Rejected **wholesale**: elevation-by-shadow, 48px touch targets, ripple/state-layer motion, tonal-palette surface generation, FABs. | This is the system whose defaults leak into generated frontend code, so it is worth naming explicitly. Every one of its core mechanics is wrong here: shadows can't express elevation in our palette (§7.1); 48px targets would cut our rows-per-screen by 40%; ripples animate on every click in an app where the user clicks constantly; and MD3's tonal surfaces assume a generated palette, while ours is frozen and hand-tuned. |
| **Linear** | Its coloured status pill on **every** row. | Our lifecycle states are long-lived — a wave can sit in one state for hours. A coloured pill on every row means a permanently multicolour list, which destroys the "colour = attention" contract (DS-PRIN-005). We use a 6px dot plus text tone in rows, and reserve the pill for the wave page header where there is exactly one. |
| **VS Code** | Its zero-radius, zero-padding chrome aesthetic. | We are a document-and-report app as much as a tool. `--radius-sm/md` and real padding are correct on the report and board surfaces; a fully squared-off UI would make the report read as a config panel. |
| **Raycast** | Its dark-only commitment and its heavy use of translucency/blur. | Light and dark are equal citizens here (DS-PRIN-006). Backdrop blur is also a per-frame GPU cost on a surface that is already compositing terminals and charts. |
| **Bloomberg-class terminals** | Multi-hue text as the primary signal channel. | Their sessions are minutes of intense scanning; ours are hours of ambient monitoring. Saturation that reads as "information-dense" in a 10-minute session reads as "exhausting" in a 6-hour one. Our accent budget (DS-COLOR-010) is deliberately far tighter. |
| **Apple HIG** | Its spacing generosity and its 44pt minimum targets. | Desktop WebView with a mouse. DS-DENS-007 explicitly declines the touch-target guidance and says why. |
| **shadcn/ui** | Its CSS (Tailwind utilities + its own token names). | Same argument as Astryx (§13.3): a second token vocabulary that must be bridged to ours forever. We take the behaviour, not the styles. |
| **Every system above** | Skeleton loaders. | Universally recommended, and wrong here — DS-LOAD-001. Skeletons bet on predictable layout; our rows have variable content, and a shimmering ghost of the wrong shape during a 150ms fetch is worse than the 150ms. |

---

## 18. Open questions for the review round

1. **P-1** — is 48px right for the two-line wave row, or was legacy's 66px load-bearing (drag target, progress affordance)?
2. **TCR-001 weight 500** — needs a browser check on the Linux WebView build. If `-apple-system` has no medium face there, T-10/T-20 lose their channel and must fall back to tone-only, which weakens §2 materially.
3. **DS-COLOR-002** — I set the floor at 4.5 with no large-text exemption. If the Today clock or a future hero legitimately needs a tone that only clears 3:1, that is a spec amendment, not a component decision.
4. **DS-SURF-006 / TCR-008** — is the four-component float-shadow carve-out acceptable, or should menus and dialogs separate by hairline + `--paper` alone? Dark mode is the hard case (2% lightness delta).
5. **DS-LAY-007's 240px threshold** — chosen as ≈5 row-pitches. Needs one pass over real pages to confirm it does not false-positive on the board surface.
6. **Astryx removal** — a design recommendation with a dependency-management consequence. Needs the project owner's sign-off, not just design review.
