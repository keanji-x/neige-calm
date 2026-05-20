# Neige Calm — Design Language

A descriptive reference of the visual vocabulary used throughout the
`web/` app. Read this once before building a new card, page, or plugin
iframe: every claim here has a `file:line` citation so you can verify
the live source.

The single source of truth is `web/src/calm.css`. There is **no** other
stylesheet — react-grid-layout's bundled CSS is the only third-party
import, and we override its hot spots from `calm.css` itself.

This is a normative document. When it says "we use X" or "do not do Y",
that is a rule, not a suggestion. Drift gets reverted.

---

## 0. Identity

**Editorial-quiet: OKLCH cool palette, hairline architecture, monospace
meta, status-as-dots — restraint as a position, not an absence.**

The aesthetic direction is committed. The design system is small on
purpose; every additional token, color, or font is a tax on coherence.
A new plugin that follows these six commitments will visually disappear
into the host, which is the goal.

- **One palette, locked.** Cool-neutral OKLCH greys with a single
  accent blue and a single warn amber. No purple, no teal, no per-
  feature hue.
- **Three font families, system-only.** Sans for UI, serif for
  editorial display, mono for meta and numbers. No web font imports,
  no Inter/Roboto/Space Grotesk.
- **Hairlines, not boxes.** Surfaces are separated by 1px borders and
  one shared shadow — never by background-color blocks or heavy strokes.
- **Status is a dot.** 8×8px, accent for live, warn for "needs you",
  grey for idle. Never a pill, never a badge with a solid fill.
- **Mono-numeric everywhere.** Any digit that updates carries
  `tabular-nums` so the layout does not jitter.
- **Density is calibrated, not crowded.** 13–15px body, ~1.5
  line-height, 14px card radius, ~18–24px card padding. Whitespace is
  load-bearing.

---

## 0.1 Anti-patterns

These are things calm rejects. If your patch reaches for one of these,
stop and reach for the documented alternative instead.

- **Generic UI fonts** (Inter, Roboto, Arial, Open Sans, Space Grotesk).
  We use the system stack + a serif display face. Distinction comes from
  the sans/serif/mono triad and the tabular-nums discipline, not from a
  Google Font.
- **Purple gradients, "AI dashboard" palettes, dark-glassmorphism.**
  We commit to OKLCH cool greys + one accent. No gradients in chrome.
- **Badges and pills with solid color fills** for status. Status is a
  dot (§6 vocabulary). A solid pill says "this matters more than the
  surrounding card" — it never should.
- **Exclamation icons, alert triangles, emoji in chrome.** Use the warn
  dot. Emoji are allowed inside user content (e.g. a doc body) but
  never in app chrome, button labels, or empty states.
- **Drop shadows that announce themselves.** There is exactly one
  `--shadow` token. Use it as-is; do not stack a second shadow on top.
- **Multiple status colors competing.** Accent blue *or* warn amber,
  never both lit at once in the same surface. If a card is both
  "running" and "needs you", warn wins.
- **Inventing a new color token because the design "needs" one.**
  There is deliberately no `--danger`, `--success`, `--info`. Diff
  green and fail red exist as inline OKLCH literals in §2 — they are
  contextual, not tokens. Adding a token is a design decision; raise
  it before you ship.
- **Dashed borders outside the "create / drop here" affordance.** The
  AddPanel and the RGL drop placeholder are the only dashed borders in
  the app. A dashed border anywhere else reads as "broken".
- **Hand-rolled palettes inside plugin iframes.** The hex table in §8
  is canonical. `#2a2f3a` is not `--text`; near-matches drift the
  iframe cooler or warmer than its host shell and the eye notices.
- **Hover states that move geometry.** Hover changes color, opacity,
  background — never position or size. Cards do not lift on hover.

---

## 1. Philosophy

> Restrained, editorial. Quiet by default; communicates state through
> subtle dots and hairlines instead of badges or color blocks.

- Cards are one hairline border + the single `var(--shadow)` token on
  `var(--paper)` (`calm.css:24-27, 348-356`). No solid color chrome.
- Headers are short, low-contrast, often uppercased tracking-out
  labels (`calm.css:529-536, 1180-1187`). Avoid title case for chrome.
- Color is data, not decoration: accent blue = "live / focused",
  warn amber = "needs you", everything else is a neutral grey scale.
  No third hue.
- Serif (`--font-serif`) is reserved for editorial display copy
  (synthesis paragraph, page H1, doc-card titles); UI text is sans;
  metadata is monospace (`calm.css:3-5`). Do not mix.
- Naming uses one-syllable semantic surfaces: `calm`, `surf`, `wave`,
  `cove`, `attn`, `term`, `doc`, `git`, `diff`, `plan`, `note` —
  *not* BEM components. New surfaces follow the same convention
  (`calm.css` section banners throughout).

---

## 2. Color tokens

All tokens are defined on `:root` (light) at `calm.css:7-27` and
overridden in `[data-theme="dark"]` at `calm.css:29-43`. We use OKLCH
in source; the iframe table below pre-converts to hex for plugin CSP
contexts where OKLCH support is uneven.

| Token             | Light (OKLCH)         | Dark (OKLCH)         | Use for                                                                                  |
| ----------------- | --------------------- | -------------------- | ---------------------------------------------------------------------------------------- |
| `--bg`            | `98.8% 0.003 240`     | `16% 0.008 245`      | Page background. (`calm.css:8, 30`)                                                      |
| `--paper`         | `99.5% 0.002 240`     | `19% 0.009 245`      | Card / surface fill. (`calm.css:9, 31`)                                                  |
| `--hairline`      | `92% 0.005 240`       | `28% 0.01 245`       | Default 1px border between sections. (`calm.css:10, 32`)                                 |
| `--hairline-strong` | `86% 0.006 240`     | `36% 0.012 245`      | Inputs, dashed AddPanel border, button outlines. (`calm.css:11, 33`)                     |
| `--text`          | `20% 0.008 250`       | `96% 0.005 245`      | Primary text + the "solid dark" button fill. (`calm.css:13, 34`)                         |
| `--text-2`        | `45% 0.01 250`        | `72% 0.012 245`      | Body copy in cards / secondary text. (`calm.css:14, 35`)                                 |
| `--text-3`        | `62% 0.01 250`        | `56% 0.012 245`      | Muted: timestamps, meta, eyebrow labels. (`calm.css:15, 36`)                             |
| `--text-4`        | `76% 0.008 250`       | `42% 0.012 245`      | Faint: separator glyphs, disabled, dim digits. (`calm.css:16, 37`)                       |
| `--accent`        | `54% 0.13 245`        | `72% 0.14 245`       | Live state, primary highlight, focus ring color, "running" dot. (`calm.css:18, 38`)      |
| `--accent-soft`   | `95% 0.025 245`       | `28% 0.05 245`       | Halo fill for focus / drop-target placeholder. (`calm.css:19, 39`)                       |
| `--warn`          | `58% 0.16 30`         | `70% 0.16 30`        | "Needs you" + errors + danger affordance. (`calm.css:21, 40`)                            |
| `--warn-soft`     | `96% 0.03 30`         | `28% 0.05 30`        | Halo behind warn dots, hover-tint for destructive buttons. (`calm.css:22, 41`)           |
| `--shadow`        | `0 1px 2px / 0 12px 36px` (cool blue) | dark-tuned | Single canonical card shadow. Use as-is. (`calm.css:26, 42`) |
| `--r`             | `14px`                | (same)               | Canonical surface radius. Almost everything that's a card uses this. (`calm.css:25`)     |

### Hex mirrors for plugin iframes

Use these in iframes where OKLCH may not be reliable. Picked to match the
OKLCH tokens above to 1–2 perceptual units.

| Token             | Light hex   | Dark hex    |
| ----------------- | ----------- | ----------- |
| `--bg`            | `#fafbfc`   | `#1c1f25`   |
| `--paper`         | `#fdfdfe`   | `#23262d`   |
| `--hairline`      | `#e4e6eb`   | `#363a42`   |
| `--hairline-strong` | `#cfd2d9` | `#4a4e57`   |
| `--text`          | `#1f242e`   | `#eef0f4`   |
| `--text-2`        | `#5d626c`   | `#a8acb4`   |
| `--text-3`        | `#878b94`   | `#7a7e87`   |
| `--text-4`        | `#a9adb4`   | `#5a5e66`   |
| `--accent`        | `#3b6dbf`   | `#7aa6ea`   |
| `--accent-soft`   | `#e7edf7`   | `#2a3a52`   |
| `--warn`          | `#c25a2b`   | `#df8054`   |
| `--warn-soft`     | `#fbecdf`   | `#4a2e22`   |

### Semantic accents (no token)

There is **no `--danger`, `--success`, or per-FSM-state token**. The
existing code points at this on purpose (see `CardStatusDot.tsx:24-28`:
"Errored… the existing palette has no 'danger' token; reuse `--warn`").
A few inlined hues exist for diff/pass/fail; use them only in matching
contexts:

| Use case              | Light                | Dark                 | Where                          |
| --------------------- | -------------------- | -------------------- | ------------------------------ |
| Pass / diff-add text  | `oklch(54% 0.14 145)` | `oklch(72% 0.14 145)` | `calm.css:1289, 1449, 1773`   |
| Fail / diff-rm text   | `oklch(56% 0.14 25)`  | `oklch(72% 0.14 25)`  | `calm.css:1291, 1450, 1042`   |
| Diff-add row fill     | `oklch(96% 0.05 145 / 0.45)` | `oklch(28% 0.05 145 / 0.5)` | `calm.css:1475, 1482` |
| Codex "Working" amber | `oklch(74% 0.16 70)`  | (same)                | `calm.css:1123`                |

---

## 3. Typography

### Stacks (`calm.css:3-5`)

| Variable      | Stack |
| ------------- | ----- |
| `--font-sans` | `-apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", sans-serif` |
| `--font-serif` | `"New York", "Iowan Old Style", "Charter", "Georgia", serif` |
| `--font-mono` | `"SF Mono", ui-monospace, "Menlo", monospace` |

`body` font-size is 15px, line-height 1.55 (`calm.css:51-52`). No
imported web fonts — system stacks only.

### Size scale (collected from the stylesheet)

| Size | Weight | Family | Used for | Example |
| ---- | ------ | ------ | -------- | ------- |
| 36px / 500 | serif  | Page H1 (`.h-display`) | `calm.css:537-545` | Cove name |
| 26px / 400 | serif  | Synthesis paragraph (`.synth`) | `calm.css:335-344` | Today's headline |
| 22px / 500 | serif  | Attention card title (`.attn h2`) | `calm.css:374-382` | |
| 22px / 400 | serif  | "Today" date (`.surf-date`) | `calm.css:1828-1836` | |
| 18px / 500 | serif  | Wave header crumb (`.wave-title`) | `calm.css:587-594` | |
| 15px / 400-500 | sans | Body, wave-row title | `calm.css:51, 458` | |
| 14px / 600 | sans   | Primary button (`.go`) | `calm.css:408-417` | |
| 13.5px / 400 | sans/mono | Surf-term body, longer-form mono | `calm.css:1742, 1786` | |
| 13px / 400-600 | sans | Default UI text — nav, inputs, modal head, schema-form input | `calm.css:101, 786, 842, 880-887` | |
| 12.5px / 500 | sans  | Status pill, side-wave row, meta secondary | `calm.css:155, 600, 1879` | |
| 12px / 500 | sans/mono | Eyebrow timestamps, breadcrumb (`.crumbs`) | `calm.css:325-332, 627-635` | |
| 11.5px / 400 | sans  | Git/diff timestamps, faint meta | `calm.css:1419, 1471` | |
| 11px / 700 + 0.08em letter-spacing UPPERCASE | sans | The "eyebrow" pattern (`.h-eyebrow`, `.attn .label`, `.surf-now-label`, `.plan-card-head`) | `calm.css:362-367, 529-536, 1659-1667, 1501-1505` | |
| 10.5px / 700 UPPERCASE | sans | Sidebar section label (`.nav-label`) | `calm.css:141-148` | |
| 10px / 700 UPPERCASE | sans  | Calendar weekday strip | `calm.css:1963-1968, 2010-2014` | |

**Mono-numeric is everywhere.** Any text that shows a number (counts,
percentages, clock digits, timestamps) carries
`font-variant-numeric: tabular-nums` so digits don't jitter as they
update (`.num` helper at `calm.css:638`; ~15 in-line uses).

---

## 4. Spacing & shape

### Radius

There are effectively four radii.

| Token / value | Used for | Sample sites |
| ------------- | -------- | ------------ |
| `var(--r)` = 14px | Cards, modal panels, dashed AddPanel, login card | `calm.css:25, 353, 823, 755, 2171` |
| `10px` | Primary button (`.go`), surf-now-card | `calm.css:412, 1641` |
| `6–8px` | Sidebar rows, nav items, input fields, small buttons | `calm.css:99, 154, 886, 1009` |
| `4–5px` | Code chips inline, tiny gutters, calendar nav, swatch | `calm.css:219, 1759, 1926` |
| `999px` | Pills (sidebar count, fill bar) | `calm.css:136, 518` |

### Padding (cards & rows)

| Slot | Padding | Where |
| ---- | ------- | ----- |
| Card body, large (attn, note) | `18-22px / 22-24px` | `calm.css:353, 1310` |
| Card head, std (term, doc, git, diff, plan, codex) | `6-12px vertical / 12-16px horizontal` | `calm.css:1082, 1166, 1339, 1381, 1432, 1498` |
| Card head, codex variant | `6px 38px 6px 12px` — extra right pad reserves space for the `.card-grid-close` × button | `calm.css:1082` (see the inline comment) |
| Wave-row | `18px 4px` | `calm.css:443` |
| Sidebar nav item | `7px 10px` | `calm.css:97` |
| Schema-form input | `6px 8px` | `calm.css:882` |
| Modal head | `9px 12px`; modal body `14px` | `calm.css:840, 857` |
| Page column | `28px 32px 32px` (max-width 620, wide=720) | `calm.css:320-322` |
| Workbench page | `18px 32px 28px`, max-width 1280 | `calm.css:551` |

### Gap

`gap` is set per-context, not on a global scale. Common values: `6, 8,
10, 12, 14, 16, 24`. There is no `4px` micro-gap convention; the
tightest gaps are 6–8.

### Hairlines

| Width / style | Use | Example |
| -------------- | --- | ------- |
| `1px solid var(--hairline)` | Default separator between rows and inside cards | `calm.css:399, 444, 1083, 1167` |
| `1px solid var(--hairline-strong)` | Inputs, ghost buttons, modal menu | `calm.css:883, 924, 985` |
| `1.5px dashed var(--hairline-strong)` | The AddPanel button — the **only** place dashed borders appear in the app | `calm.css:752-760` |
| `1.5px solid <current>` | Plan-card status dot ring, select-day ring (`box-shadow: inset 0 0 0 1.5px var(--text)`) | `calm.css:1520, 1984, 2050` |
| `1px dashed var(--accent)` | Drop-target ghost only (RGL placeholder) | `calm.css:2261-2266` |

Dashed borders are reserved for "create / drop here" affordances. Use
solid hairlines for everything else. A dashed border on a non-CTA
surface is a bug.

---

## 5. Animations & transitions

Single shared motion vocabulary. Transitions are short (~100–150ms),
ease default. The only "looping" animation is the `dot-pulse` family.

| Name | Duration | Used by | Source |
| ---- | -------- | ------- | ------ |
| `dot-pulse` (opacity 1 ↔ 0.55) | 2.2s ease-in-out infinite | `.live-dot`, `.status-pill-dot.live-dot`, `.side-wave .swatch.pulse`, `.surf-now-dot.live`, `.surf-stat-dot.run` | `calm.css:300-303, 297-299, 611, 1676, 1870` |
| `pulse` (transform-scale halo) | 1.6s infinite | `.attn .label .pulse` only | `calm.css:368-372` |
| `codex-dot-pulse` (halo ring grow) | 1.6s ease-in-out infinite | Codex "Working" amber dot | `calm.css:1123-1130` |
| `term-blink` / `clock-blink` | 1.05s / 1.6s steps(2) | Terminal cursor, clock colon | `calm.css:1296-1303, 1602` |
| Hover / state transitions | `0.1s` (color, background, border) — sometimes `0.12s` for opacity reveals | All nav/button/row hovers | passim |

Mirror these durations in plugin iframes. Nothing snappier than 100ms,
nothing longer than ~250ms outside the named pulse loops. No spring
physics, no bounce, no parallax — calm does not "delight" through
motion.

---

## 6. Card chrome pattern

Every grid-cell card carries the same skeleton: head bar (which doubles
as the RGL drag handle), then body. Canonical examples:

- **Terminal card** — `web/src/cards/builtins/terminal.tsx:30-60`. The
  head is `<div className="term-head card-drag-handle">` and holds
  three faint dots + an UPPERCASE 11px title with optional `· live`
  pip in accent.
- **Codex card** — `web/src/cards/builtins/codex.tsx:84-107`. The head
  is `<div className="codex-card-head card-drag-handle">` and holds a
  13px medium title, then a status bar (`.codex-status-bar` →
  ellipsing `.codex-status-label` + `<CardStatusDot/>`).
- **Plugin iframe card** — `web/src/cards/plugin-iframe.tsx:269-291`.
  Same pattern: a host-rendered `.plugin-iframe-head card-drag-handle`
  div sits above the iframe.

Three rules every card must obey:

1. **The head bar must carry the `card-drag-handle` class** —
   `WaveGrid.tsx:191` registers it as the RGL drag handle. Without it,
   the card cannot be repositioned. (`calm.css:705-712` adds the
   `cursor: grab` / `grabbing`.)
2. **The head's right side leaves ~38px clear** for the hover-revealed
   `.card-grid-close` × button (`calm.css:716-742`). Codex's head pads
   `38px` on the right for exactly this — see the comment at
   `calm.css:1079-1081`.
3. **Status goes in the head bar, on the right.** The vocabulary is
   `CardStatusDot` (`web/src/shared/components/CardStatusDot.tsx`).

### Canonical card chrome (normative)

Three card types ship today (`.term`, `.codex-card`, `.plugin-iframe-card`)
and **all three must share** these declarations, in exactly this form:

```css
.<card>      { border: 1px solid var(--hairline); border-radius: var(--r);
               background: var(--paper); box-shadow: var(--shadow);
               display: flex; flex-direction: column; overflow: hidden; }
.<card>-head { padding: 6px 38px 6px 12px;
               border-bottom: 1px solid var(--hairline);
               background: oklch(96% 0.004 240);   /* light grey wash */
               font: 500 13px/1.4 var(--font-sans);
               min-height: 28px; display: flex; align-items: center; gap: 8px; }
[data-theme="dark"] .<card>-head { background: oklch(21% 0.009 245); }
```

Reference implementations: `.codex-card` (`calm.css:1067-1090`),
`.term` (`calm.css:1155-1170`), `.plugin-iframe-card`
(`calm.css:1154` block — added when the plugin chrome landed).
**Do not** invent a new card chrome (different radius, different head
background, white head); add a fourth entry to this family instead.

### Head label rules

- Title is short: one to three words. Sentence case. No prefix like
  `Plugin:` and no colon-joined identifiers — the raw `ui://<id>/<view>`
  is debug info, never user-facing chrome.
- For plugin iframes, the host reads `card.payload.title` (set by the
  plugin's `tools/call` return) and falls back to `view_id`. Plugins
  that want a different label set `structuredContent.title` in their
  tool result; they do **not** render their own head bar inside the
  iframe (the host already provides one — see §8).

### Status dot vocabulary (`CardStatusDot.tsx:15-46`)

| FSM state       | Color           | Halo                | Animation       |
| --------------- | --------------- | ------------------- | --------------- |
| `Starting`      | `var(--accent)` | none                | `.live-dot` pulse |
| `Working`       | `var(--accent)` | none                | `.live-dot` pulse |
| `AwaitingInput` | `var(--warn)`   | `0 0 0 4px var(--warn-soft)` | none |
| `Errored`       | `var(--warn)`   | `0 0 0 4px var(--warn-soft)` | none |
| `Idle`          | `var(--text-3)` | none                | none            |
| `Done`          | `var(--text-3)` | none                | opacity 0.55    |

Dot is 8×8px, `border-radius: 50%`. Plugin iframes that want a parallel
indicator should reuse these conventions: amber halo = "needs you",
accent pulse = "live".

### Card empty / loading state (gap noted)

There is **no shared empty-state component**. Each card improvises:

- Codex: `.codex-card-empty` — centered, `var(--text-3)`, 12px
  (`calm.css:1147-1152`). Copy is short and lowercase ("Codex is
  starting… waiting for PTY.").
- Terminal (static flavor): renders the same `.term-body` with stub
  `.term-line k-cursor` rows — implicit empty state.
- Doc / Git / Diff / Plan: no empty state at all; assume content.

When you build a new card, lean on the Codex pattern: full-card flex
center, `--text-3`, ~12–13px, short sentence, no glyph.

---

## 7. Forms & inputs

Single source of vocabulary: `SchemaForm` (`SchemaForm.tsx`) +
`AddPanel` (`AddPanel.tsx`). All styles live in `calm.css:876-902`.

### Field

```
.schema-form-field   flex-column, 4px gap
.schema-form-label   12px, var(--text-2)
.schema-form-input   1px solid var(--hairline-strong), 6px radius, 6px 8px padding, var(--paper) bg
```

`textarea.schema-form-input` adds `resize: vertical` (`calm.css:888`).
Focus styling isn't explicit; the `.dirpicker-field-open` rule
(`calm.css:930`) shows the canonical focus halo:
`box-shadow: 0 0 0 2px oklch(60% 0.04 245 / 0.18)` plus a stronger
border. Plugin iframes can match by using `--accent-soft` for the halo
+ `--accent` for the border on `:focus`.

### Button hierarchy

| Variant            | Style                                                      | Where |
| ------------------ | ---------------------------------------------------------- | ----- |
| **Primary**        | `bg: var(--text)`, `color: var(--paper)`, 36px tall, 10px radius, 14px/600 (`.go`) | `calm.css:408-420` |
| **Outline**        | `.go.outline` — transparent + inset 1px hairline-strong    | `calm.css:421-424` |
| **Warn**           | `.go.warn` — `bg: var(--warn)` + white text                | `calm.css:425` |
| **Ghost**          | `.go.ghost` — transparent, 30px, hover background `oklch(0% 0 0 / 0.04)` | `calm.css:426-430` |
| **Schema-form submit** | 32px / 13px / bg=`--text`, color=`--paper` (`.schema-form-submit`) | `calm.css:893-902` |
| **Schema-form cancel** | 32px / 13px / transparent + hairline-strong border        | `calm.css:893-900` |
| **Dashed call-to-action (`+ Add`)** | 28px, 1.5px dashed hairline-strong, 12.5px/500 muted | `calm.css:751-762` |
| **Per-row × delete** | 24×24, transparent, hover → `--warn-soft` bg + `--warn` color, **opacity 0 → 1 on row hover only** | `calm.css:487-510, 716-742` |

The "destructive on hover" pattern (transparent → warn-soft fill) is
the canonical delete affordance. Never add red borders, persistent red
text, or a trash-can icon stuck to the row at rest. Destructive intent
appears on hover and only on hover.

---

## 8. Plugin iframe styling rules

This is the actionable bit. The host **does not inject CSS into your
iframe** — you ship your own stylesheet, and you **must** mirror the
calm tokens exactly so your card visually disappears into the host.

**First check:** open your iframe in dark mode. If the contrast feels
wrong, off-hue, or "warmer than the rest of the page", you used the
wrong tokens. Re-paste the block below verbatim before doing anything
else.

### What the host gives you

- A wrapping `.plugin-iframe-card` + `.plugin-iframe-head
  card-drag-handle` rendered **outside** the iframe
  (`plugin-iframe.tsx:269-291`). You do **not** add `card-drag-handle`
  inside your iframe — the host already owns the head bar.
- An iframe with `sandbox="allow-scripts allow-same-origin
  allow-forms"` (`plugin-iframe.tsx:315`). Forms work; native submit
  works. Network writes are still gated by the manifest's
  `connect-src` CSP, so you can't `<link rel="stylesheet">` from a
  CDN — bundle everything inline.
- A `prefers-color-scheme` signal that follows the OS (the host's
  theme toggle is a separate concern; the contract today is "match
  the OS"). If the host later forwards theme via
  `host-context-changed`, we'll document a `data-theme` attribute
  contract here.

### Paste this at the top of your view

Paste this block. Do not invent your own palette. Do not "improve" the
hex values. Do not add a third theme. The hexes below are the OKLCH
tokens of §2 pre-converted for CSP contexts where OKLCH may not
resolve — they are the contract.

```css
:root {
  color-scheme: light dark;

  --bg: #fafbfc;
  --paper: #fdfdfe;
  --hairline: #e4e6eb;
  --hairline-strong: #cfd2d9;
  --text: #1f242e;
  --text-2: #5d626c;
  --text-3: #878b94;
  --text-4: #a9adb4;
  --accent: #3b6dbf;
  --accent-soft: #e7edf7;
  --warn: #c25a2b;
  --warn-soft: #fbecdf;

  --r: 14px;
  --font-sans: -apple-system, BlinkMacSystemFont, "SF Pro Text",
               "Helvetica Neue", sans-serif;
  --font-mono: "SF Mono", ui-monospace, "Menlo", monospace;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #1c1f25;
    --paper: #23262d;
    --hairline: #363a42;
    --hairline-strong: #4a4e57;
    --text: #eef0f4;
    --text-2: #a8acb4;
    --text-3: #7a7e87;
    --text-4: #5a5e66;
    --accent: #7aa6ea;
    --accent-soft: #2a3a52;
    --warn: #df8054;
    --warn-soft: #4a2e22;
  }
}
body {
  margin: 0;
  padding: 10px 12px;
  font: 13px/1.5 var(--font-sans);
  color: var(--text);
  background: var(--bg);
}
```

### Sizes & weights to use

- **Body / row text**: 13px / 400-500
- **Title / strong**: 13px / 600 (no heavier)
- **Meta / muted timestamps**: 11–12px `--font-mono`, `--text-3`
- **Eyebrow** (if you need one): 11px / 700 uppercase, 0.08em letter-
  spacing, `--text-3`

### What NOT to do

- **Never invent a palette.** `plugins/hello-world/views/status.html:6-21`
  hard-codes `#2a2f3a` / `#5b626d` / `#fff` that drift from
  `--text`/`--text-2`/`--bg`. Result: the iframe looks vaguely
  cooler-blue than its host shell. Use the table above, verbatim.
- **Never import fonts.** No `@import`, no `<link rel="stylesheet">`,
  no `@font-face` URLs. CSP blocks them and they would break the
  unified look. The system stack is the design — not a fallback.
- **Never import an icon library** (Heroicons, Lucide, Phosphor, FA).
  Match the stroke-icon vocabulary by inlining SVGs. The reference is
  `web/src/Icon.tsx`: 24×24 viewBox, `stroke="currentColor"`,
  `stroke-width: 1.6`, round caps/joins, no fill.
- **Never put a drag handle inside your iframe.** The host head bar
  owns dragging. Your iframe body starts below the head.
- **Never render multi-step wizards inside a card.** Forms are
  allowed; keep them inline and small (AddPanel's `+ Add` → schema
  form modal pattern). For deeper flows, surface a button that opens
  a host-level modal.
- **No bright borders. No badge pills with solid color fills. No
  emoji in chrome.** Status communicates via the dot vocabulary in §6.
- **No drop shadows of your own.** If you need lift, use a 1px
  `--hairline` border. The card's outer shadow is the host's job.

---

## 9. Worked example — `plugins/todo/views/list.html`

The todo plugin works but visibly drifts from the design language. The
specific places to fix (do not edit yet — that's the next task):

| Line(s) | Drift | What it should be |
| ------- | ----- | ----------------- |
| `list.html:9-26` | Hand-rolled palette (`--fg: #2a2f3a`, `--muted: #5b626d`, `--bg: #fff`, `--border: #e3e6ec`, `--danger: #b04141`). Each token is "near" a calm token but none match. | Replace with the §8 token block. Map `--fg → --text`, `--muted → --text-3`, `--border → --hairline`, `--danger → --warn`. |
| `list.html:31-40` | `body` is 13px on `--bg` with 10/12 padding — close, but no `font-feature-settings`/`line-height` to match calm body 1.5. | Use the §8 body block. |
| `list.html:88-98` | `button.del` uses 1px transparent border and switches `color` only on hover. Doesn't match the calm delete pattern (transparent → `--warn-soft` bg + `--warn` color). | Adopt the `.wave-row-delete` pattern at `calm.css:487-510`: 24×24, opacity 0 until row hover, background `--warn-soft` on hover. |
| `list.html:123-132` | "Add" button uses 1px solid `--muted` border, brightens on hover. This is roughly the `.go.outline` look but at the wrong height / weight / radius. | Either reuse the `.add-panel` dashed pattern (this card's primary CTA) or the `.schema-form-submit` solid-dark button. Pick one; don't invent. |
| `list.html:146-157` | `.status` block uses **dashed border** as a default state, and goes solid red on error. Dashed is reserved for "create / drop here" affordances (§4). Solid red is not in the vocabulary. | Make the default status a single 11px mono `--text-3` line, no border. Error: same line, color `--warn`, no border. Or, if it must be a callout, use `.attn .label` style — small UPPERCASE eyebrow + `--warn` text + 6px pulse dot. |

Other smaller items to fold in during the rewrite:

- `font-size: 10` for meta (`list.html:50, 138, 146`) → calm uses 11–12
  for mono meta; 10 only shows up on calendar weekday strips.
- `ul.items` uses `border-top` + `border-bottom` for the list frame —
  calm prefers a single `border-bottom: 1px solid var(--hairline)` per
  row (see `.wave-row` at `calm.css:444`). The outer frame is just the
  card itself.
- The empty state (`list.html:99-104`) is italic centered — fine, but
  match calm's hint pattern: 12.5px `--text-3`, italic, padded
  `4px 4px 2px` (mirror `.cal-empty` at `calm.css:2082-2087`).

---

## 10. Quick lookups

- **"What hex for muted text?"** → `--text-3`: `#878b94` (light),
  `#7a7e87` (dark).
- **"What's the card radius?"** → 14px (`--r`).
- **"What's the card border?"** → `1px solid var(--hairline)`.
- **"What's the standard shadow?"** → `var(--shadow)`. Don't mix
  custom shadows; use the token even when you only want a subtle lift.
- **"How do I show 'live'?"** → 8px dot, `background: var(--accent)`,
  add the `.live-dot` class for the 2.2s opacity pulse.
- **"How do I show 'needs attention'?"** → 8px dot, `background:
  var(--warn)`, `box-shadow: 0 0 0 4px var(--warn-soft)`. No pulse.
- **"Where does an 'Add' button go?"** → Wave header right edge,
  dashed 1.5px border style (`.add-panel`, `calm.css:751-762`).

---

This document is the project-specific application of the `frontend-design`
skill (Anthropic, `claude-plugins-official` marketplace). The skill picks
the aesthetic direction; this doc commits neige-calm to one ("editorial-
quiet") and locks the vocabulary.
