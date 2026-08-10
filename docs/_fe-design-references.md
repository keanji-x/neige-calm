# Design references for the neige-calm frontend rewrite

Written 2026-08-10. Network on this box is unreliable; procedure was search-first, fetch only for exact
normative figures, one retry max per source.

**Provenance tags — every external claim carries one:**
- `[verified]` — I read the primary source this session (fetched the page, quoted its text).
- `[search]` — search-result snippet or secondary write-up. Directionally reliable, wording may be paraphrased.
- `[unverified]` — my prior knowledge, NOT confirmed this session. Treat exact numbers as hypotheses to
  measure, not specs to copy. Where an `[unverified]` number is load-bearing I say how to check it locally.

Sources that failed this session and were dropped after one retry: `carbondesignsystem.com/components/button/usage/`
(fetch returned truncated content twice-equivalent). Carbon claims below are therefore `[search]`, drawn from
snippets of that same page and the v10/v11 mirrors.

---

## 1. Action hierarchy

### 1.1 The ladder as the major systems publish it

| Level | Carbon (IBM) `[search]` | Material 3 `[search]` | Primer (GitHub) `[unverified]` | What it means for us |
|---|---|---|---|---|
| Highest | **Primary** — filled | **Filled** — "most visual impact after the FAB… important, final actions that complete a flow, like Save, Join now, Confirm" | `primary` — filled green | Filled accent. One per surface. |
| High-mid | **Secondary** — filled, lower-contrast fill | **Filled tonal** — "middle ground between filled and outlined… a lower-priority button requires slightly more emphasis than an outline would give" | (no tonal tier) | Optional. Skip it; see 1.5. |
| Mid | **Tertiary** — outlined/ghost-with-border | **Outlined** — "medium emphasis… important, but aren't the primary action" | `default` — bordered neutral | Bordered neutral. The workhorse. |
| Low | **Ghost** — text-only, no border | **Text** — "lowest priority actions, especially when presenting multiple options" | `invisible` — text-only | Toolbar/row/card actions. |
| Destructive | **Danger**, in three styles: `danger` (primary), `danger--tertiary`, `danger--ghost` | (no separate ladder; `error` colour role applied) | `danger` variant of default and invisible | Danger is an **orthogonal axis**, not a fifth rung. |

The single most useful published idea here is Carbon's: **danger is a modifier on the emphasis ladder, not a
level of it.** `[search]` — "The danger button has three different styles: primary, tertiary, and ghost…
destructive actions that are a required or primary step in a workflow should use the primary danger button
style. However, if a destructive action is just one of several actions a user could choose from, then a lower
emphasis style like the tertiary danger button or the ghost danger button may be more appropriate."
(https://carbondesignsystem.com/components/button/usage/) Material 3 has no equivalent statement; it just has
an `error` colour role, which is why M3 products routinely ship an unmissable red filled "Delete" sitting next
to a grey "Cancel" — exactly the failure mode we are trying to avoid in reverse.

**Take a side.** Adopt Carbon's two-axis model (emphasis × tone), reject M3's five-button-type taxonomy.
M3's five types (elevated / filled / filled-tonal / outlined / text) are five *appearances* mapped loosely to
emphasis, and they include `elevated`, which M3 itself hedges on `[search]`: "To prevent shadow creep, only use
them when absolutely necessary." A tool with shadows on buttons reads as a consumer app; a dense workspace
should carry elevation on *panels*, not on controls.

### 1.2 "One primary per surface" — who actually says it

| Source | Wording | Tag |
|---|---|---|
| Material Design | "a layout should contain a single high-emphasis button that makes it clear that other buttons have less importance in the hierarchy" | `[search]` (https://m3.material.io/components/all-buttons) |
| Carbon | "Use only one primary button per view, unless multiple actions are equally important." Also: "due to the visual weight of the secondary button, it's recommended to use tertiary or ghost buttons in layouts with more than three calls to action." | `[search]` (https://carbondesignsystem.com/components/button/usage/) |
| Refactoring UI | "Emphasize by de-emphasizing" — when the primary won't stand out, lower the competing elements rather than raising the primary | `[search]` (https://www.refactoringui.com/, notes mirror: https://gist.github.com/selcukcihan/b9418596a98abfcd4bbc622550820cc5) |

Note the scoping word: Material says *layout*, Carbon says *view*. Neither says "per screen" in the sense of a
whole browser window. For an app like ours the correct unit is **per independently-scannable surface**: the
Today page header is one surface, each card's header is another, a drawer is another, a modal is another. A
wave page with 12 cards legitimately has 12 card-level primaries — but each card gets at most one, and none of
them may use the page-level filled treatment. Encode this as: *filled accent is reserved for page/dialog-level
primary; card-level primaries are the bordered tier.*

### 1.3 Destructive: placement, and colour-at-rest vs colour-on-intent

The two published positions differ, and this is the live question for us.

| Question | Carbon | NN/g | Verdict for neige-calm |
|---|---|---|---|
| Colour-coded at rest? | Yes — danger variants are red-tinted at rest, including tertiary (red border + red label) and ghost (red label) | Doesn't legislate colour; legislates *separation* and *redundant signalling*: "Confirmatory and destructive actions should be far apart from each other; use additional redundant visual signals to differentiate them and avoid user errors" `[search]` (https://www.nngroup.com/articles/proximity-consequential-options/) | **Colour at rest for anything already visible; colour + separation in menus.** Colour only on hover is a trap: keyboard users and scanners never see it, and it violates NN/g's redundancy point because the signal is absent exactly when the decision is made. |
| Placement | Danger sits at low emphasis when it is one option among many | "Dangerous UX: Consequential Options Close to Benign Options" — proximity is the hazard `[search]` | Destructive actions live **last, behind a separator, in an overflow menu** for row/card scope; and **left of the confirm button, or as the confirm button itself in a danger dialog** for dialog scope. Never adjacent to a benign action without a separator. |
| Focus target in a danger dialog | Carbon a11y issue #10914: "Focus is automatically set to the first focusable element inside the dialog, which is the 'No' button. This is the least destructive action, so focusing 'No' helps prevent users from accidentally confirming the destructive 'Discard' action, which cannot be undone." `[search]` (https://github.com/carbon-design-system/carbon/issues/10914) | — | **Initial focus goes to the safe action, always.** This is mechanically checkable in a test. |

So: destructive is red **at rest**, but red at *low chroma* when at tertiary/ghost emphasis — a red text label
and, at most, a red border. A filled red button is reserved for the confirm step of a dialog, or for a
workflow whose entire purpose is the deletion. If our build ever shows two filled red buttons on one screen,
that is a bug.

### 1.4 Confirming a destructive action — the accepted ladder

NN/g's guidance is a ladder, not a single pattern `[search]`
(https://www.nngroup.com/articles/confirmation-dialog/, https://www.nngroup.com/articles/user-mistakes/):

1. **Undo beats confirm.** "An even better design would provide the user with the opportunity to undo this
   destructive action… a mistake is low cost and can be easily fixed." Prefer optimistic action + a toast with
   Undo for anything reversible.
2. **Plain confirmation dialog** for irreversible-but-cheap. Verb-specific button labels ("Delete wave", not
   "OK"). NN/g separately warns about Cancel-vs-Close ambiguity
   (https://www.nngroup.com/articles/cancel-vs-close/) `[search]`.
3. **Non-standard confirmation** (type the name to confirm) reserved for the rare and catastrophic: "For
   particularly dangerous operations, require a nonstandard action from the user to confirm, such as typing a
   word into a box, as MailChimp requires before deleting a mailing list, rather than simply clicking an OK
   button which risks becoming automated behavior… Such nonstandard response options have to be reserved for
   the most dangerous and rare actions, because if they're used too frequently, they become a new standard."
   `[search]`
4. Confirmation is never the *only* protection: "confirmation dialogs should not be used as the sole error
   prevention method." `[search]`

Mapping to our domain: killing a terminal card → undo-less but cheap, plain confirm or even no confirm with a
5-second undo. Deleting a wave (with its report document) → plain confirm. Deleting a cove (cascades to all
waves) → type-to-confirm. That is exactly one type-to-confirm in the whole product, which is the right budget.

### 1.5 Icon-only actions

- Every icon-only control needs a programmatic accessible name. Order of preference: **visible text >
  `aria-label` > `aria-labelledby` > visually-hidden text** `[search]`
  (https://www.sarasoueidan.com/blog/accessible-icon-buttons/). Decorative SVG gets `aria-hidden="true"` and
  `focusable="false"`.
- A `title` attribute or a custom tooltip is **not** an acceptable substitute for the accessible name
  `[search]` (same source; also https://accessibilityinsights.io/info-examples/web/aria-tooltip-name/). Ship
  both: `aria-label` for the a11y tree, a real tooltip for sighted discoverability.
- Empty accessible names are one of the most common WCAG failures in the wild — reported at ~27.7% of home
  pages tested `[search]` (https://www.levelaccess.com/blog/aria-labels-and-accessible-names-a-developers-guide/).
  This is a lint rule, not a review item.
- Carbon on icon usage: "icons should be used sparingly, as overuse can create visual noise and make an
  experience less usable"; in an icon-only button the icon is centred, in a labelled button it is right-aligned
  `[search]`.
- Discoverability rule for a dense tool: icon-only is legitimate for **repeated, per-row/per-card** actions
  where the label would be repeated N times; it is illegitimate for **once-per-page** actions, which should
  carry text. A toolbar of five icon-only glyphs with no text anywhere on screen is a memory test.
- Size: see §3.3 — icon-only buttons are the single most common WCAG 2.5.8 violation, because a 16px glyph
  with 2px padding is a 20×20 target.

---

## 2. Typographic hierarchy in dense professional tools

### 2.1 How many levels, and how they differ from consumer scales

| System | Published levels | Base size | Ratio | Tag |
|---|---|---|---|---|
| Material 3 | 15 named styles (display/headline/title/body/label × L/M/S) | 16sp body (M3 Expressive nudges to 18sp) | roughly 1.125–1.25 between adjacent, larger at display end | `[search]` (https://m3.material.io/styles/typography/type-scale-tokens) |
| Apple HIG | 11 text styles (largeTitle…caption2) × 12 Dynamic Type sizes | Body 17pt at default; Large Title 34pt | ~1.125 near body, wider at top | `[search]` (https://developer.apple.com/design/human-interface-guidelines/typography) |
| Dense-tool lineage (Linear/Raycast/VS Code/Figma) | effectively **4–6** used sizes, clustered in an 11–22px band | 13px UI text is the centre of gravity | ~1.1, i.e. barely a ratio at all | `[unverified]` for the specific numbers; `[search]` that Linear/Vercel/Raycast share an approach and favour system/Inter-class faces for readability over brand expression (https://typescale.app/typescales/typescale-and-typography-system-of-linear, https://oh-my-design.kr/design-systems/raycast) |

The structural difference is not "dense tools use smaller type." It is:

- **Consumer scales are multiplicative; dense scales are additive.** A 1.25 modular scale from 16px gives
  16/20/25/31/39 — the top of that range is a marketing headline and has no referent in an IDE. Dense tools
  step 11 → 12 → 13 → 15 → 18 → 22: differences of 1–4px, chosen so that *two adjacent levels can sit on the
  same row without changing the row's height*.
- **Consumer scales spend the size channel; dense scales hoard it.** In a wave page there is one h1 (the
  report title) and then everything else is body-or-smaller. The scale's job is to distinguish
  label-vs-value-vs-metadata inside a 20px row, not to distinguish hero-vs-subhead.
- **Consumer scales are viewport-responsive; dense scales are density-responsive.** M3/HIG scale type with
  Dynamic Type / breakpoints. A dense tool instead offers a global density/zoom control and keeps ratios fixed.

A defensible published scale for us, stated as design intent (numbers `[unverified]`, derived from the
lineage above rather than quoted from it):

| Token | px / line-height | Weight | Use |
|---|---|---|---|
| `text-xs` | 11 / 16 | 500 | badges, axis ticks, keycap hints |
| `text-sm` | 12 / 18 | 400–500 | metadata, timestamps, card subtitles |
| `text-base` | 13 / 20 | 400 | the default. rail items, table cells, agenda rows |
| `text-md` | 15 / 22 | 500–600 | card titles, section headers, wave list rows |
| `text-lg` | 18 / 26 | 600 | page title (Today, cove name) |
| `text-xl` | 22 / 30 | 600 | the clock, and nothing else |
| `mono-base` | 12 / 18 | 400 | terminal, code, IDs |

Six levels. Note that a 5-level product is also defensible; anything past seven is unused surface area.

### 2.2 When weight substitutes for size

Primer states this as policy `[search]` (https://primer.style/foundations/color/overview/,
https://primer.github.io/design/foundations/typography/): "When establishing hierarchy for GitHub products,
designers stress efficient, clean reading experiences and refrain from utilizing color as a primary method of
emphasis. Instead, font weight is adjusted to add emphasis and differentiate content hierarchy."

Refactoring UI, on the same channel `[search]`: "two font weights are usually enough: a normal font weight
(400 or 500) for most text and a heavier font weight (600 or 700) for text you want to emphasize"; and "use
color and weight to create hierarchy instead of size."

Concretely, for a codebase that currently uses `font-weight: 600` twice in 1511 lines:

1. **Set a weight ladder of exactly three and use all three.** 400 = body/value; 500 = the default for UI
   chrome (rail items, buttons, tabs, column headers); 600 = titles and the single emphasised item in any
   group. Do not add 700 — at 13px the difference between 600 and 700 is invisible on most displays and the
   two will drift into being used interchangeably.
2. **Weight is the *primary* differentiator inside a row; size is the primary differentiator between
   regions.** A card header at 13/600 beside a value at 13/400 is a full hierarchy step at zero vertical cost.
   That is the whole trick a dense tool is built on and it is the channel our build has unused.
3. **State is a weight change, not only a colour change.** Active rail item = 500 → 600 plus the colour
   change. This survives colour-blindness and grayscale screenshots, and it is the reason Linear-class UIs
   still read correctly at 50% brightness.
4. **Never use weight and size together for the same distinction** unless you also change region. Stacking
   channels is how a 6-level scale becomes visually 3 blunt levels — see §4.
5. Practical caveat `[unverified]`: variable-font weights (Inter's 450/510/550) are attractive but every
   intermediate weight you add is a weight that must be justified. Ship 400/500/600; only reach for
   `font-variation-settings` if a specific face's 500 is too light against dark backgrounds — which it usually
   is, so an *optional* rule is "dark theme shifts the whole ladder +25 to +50 units" (measure, don't assume).

### 2.3 Line-height ladder

Line-height in a dense tool is a *row-height* decision, not a reading-comfort decision.

| Content | Ratio | Rationale |
|---|---|---|
| Single-line UI rows (rail, agenda, table cells) | 1.4–1.55, snapped to an even px so rows land on the 4px grid | Prose ratios (1.5–1.6) applied to 13px give 20.8px — round it to 20 and the whole grid is integral `[unverified]`, standard practice |
| Report body prose | 1.6–1.7 | Long-form reading; this is the one place a consumer ratio is correct |
| Titles ≥18px | 1.25–1.35 | Large type needs proportionally less leading |
| Mono/terminal | 1.4–1.5 | Below 1.35 and stack traces become unreadable |

Rule: line-height decreases as size increases; never specify unitless line-heights that produce fractional
pixels in dense rows.

### 2.4 Numerals

- **Use `font-variant-numeric: tabular-nums`** (OpenType `tnum`) wherever a number can change in place or is
  stacked in a column `[verified via MDN definition in search results]`
  (https://developer.mozilla.org/en-US/docs/Web/CSS/font-variant-numeric): tabular-nums "activates a set of
  figures where numbers are all of the same size, allowing them to be easily aligned like in tables."
- Rationale that matters here `[search]`: "when a number flips from 11:11 to 12:23, the whole string can shift
  horizontally, which is terrible UX for timers, leaderboards, financial tables, and live prices."
  Our Today page has a **clock** and our cards have **elapsed-time / token-count / cost** readouts that tick.
  Every one of them is a horizontal-jitter bug today unless tabular figures are on.
- **Do not make it global** `[search]`: "Use tabular figures only where alignment matters… For body copy,
  proportional digits usually look better." Correct scoping: a `.tnum` utility applied to the clock, week
  calendar day numbers, durations, counts, byte sizes, and any table column; NOT applied to the report body.
- Alignment: numeric columns right-align (or decimal-align); the header aligns with the column, not with the
  label `[search]` — "consider using tabular-nums alongside text-right to align numerical content neatly."
- Two further rules that avoid re-work `[unverified]`:
  - Durations render in a fixed shape (`0:04:31`, not `4m 31s`) when they live in a column; the human-readable
    form is fine for inline prose.
  - Counts that can be zero render as `0`, never as an empty cell — an empty cell means "unknown", and in an
    agent workspace that distinction is real.

### 2.5 Why this differs from M3/HIG at all

M3 and HIG are optimising for **unknown content on unknown screens read at arm's length by a first-time
user**: big ratios, generous leading, many named roles so a designer never picks a raw px. Dense tools
optimise for **known content, on a large screen, read for eight hours by the same person**: minimal ratios,
tight leading, a small scale that a single developer can hold in their head. Adopting M3's 15 roles for
neige-calm would mean 9 roles are permanently unused and the remaining 6 are all too large.

---

## 3. Focus and state

### 3.1 What WCAG 2.2 actually requires — cited by number

All quotes in this subsection are `[verified]` from https://www.w3.org/TR/WCAG22/ and
https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html, fetched this session.

| SC | Level | Normative text | Practical threshold |
|---|---|---|---|
| **1.4.11 Non-text Contrast** | AA | "Visual information required to identify user interface components and states, except for inactive components or where the appearance of the component is determined by the user agent and not modified by the author" must have "a contrast ratio of at least 3:1 against adjacent color(s)." | **3:1** for the focus ring, borders that carry meaning, checkbox/toggle states, and the visual distinguishing a selected row. |
| **2.4.11 Focus Not Obscured (Minimum)** | **AA** | "When a user interface component receives keyboard focus, the component is not entirely hidden due to author-created content." | Sticky headers/footers, our left rail, the right drawer, and toasts must not fully cover the focused element. **This is AA — it is a hard requirement, and it is a real risk for us** given a sticky page header plus a drawer. |
| **2.4.12 Focus Not Obscured (Enhanced)** | AAA | "When a user interface component receives keyboard focus, no part of the component is hidden by author-created content." | Nothing may be even partially covered. Aim for it via `scroll-margin-top` = sticky-header height. |
| **2.4.13 Focus Appearance** | AAA | "When the keyboard focus indicator is visible, an area of the focus indicator meets all the following: — is at least as large as the area of a 2 CSS pixel thick perimeter of the unfocused component or sub-component, and — has a contrast ratio of at least 3:1 between the same pixels in the focused and unfocused states." | **2px ring, 3:1 focused-vs-unfocused.** Note the contrast is measured between the *same pixels* in the two states, which is why a ring drawn *outside* the component (outline-offset) is the easy way to pass: those pixels were background before and ring colour now. |
| **2.5.8 Target Size (Minimum)** | **AA** | "The size of the target for pointer inputs is at least 24 by 24 CSS pixels, except when: **Spacing:** Undersized targets… are positioned so that if a 24 CSS pixel diameter circle is centered on the bounding box of each, the circles do not intersect another target or the circle for another undersized target; **Equivalent:** The function can be achieved through a different control on the same page that meets this criterion; **Inline:** The target is in a sentence or its size is otherwise constrained by the line-height of non-target text; **User Agent Control:** The size of the target is determined by the user agent and is not modified by the author; **Essential:** A particular presentation of the target is essential or is legally required." | **24×24 CSS px**, or 24px centre-to-centre spacing. Padding counts toward the target. Note 2.5.5 Target Size (Enhanced) remains 44×44 at AAA `[unverified — not re-read this session]`. |

The AA set that binds us: **1.4.11, 2.4.11, 2.5.8** (plus 2.4.7 Focus Visible from 2.0, `[unverified]` wording).
2.4.13 is AAA but is cheap to meet and is the only criterion that gives a *number* for ring thickness, so treat
it as our internal standard.

Density consequence worth stating plainly: a 24px minimum target with a 24px spacing escape hatch is
**compatible** with a dense UI. A 20px-tall icon button is legal if nothing else is within a 24px circle of it.
A row of five 20px icon buttons packed at 4px gaps is **not** legal. This is the rule our card headers will hit.

### 3.2 The two-tone focus ring

The problem: a single-colour ring fails over some background, always — a blue ring over a blue-tinted selected
row, a white ring over a light card. The published fix is a two-layer ring `[search]`
(https://piccalil.li/blog/taking-a-shot-at-the-double-focus-ring-problem-using-modern-css/,
https://www.deque.com/blog/give-site-focus-tips-designing-usable-focus-indicators/,
https://darekkay.com/blog/accessible-focus-indicator/): "By sandwiching two high-contrast colors, such as
black on white, you guarantee that at least half of the indicator remains visible regardless of the colour or
complexity of the image behind it." The simplest reliable form is "an inner ring in the surface colour and an
outer ring in the focus colour, with the inner ring acting as a separator regardless of what's behind it."

Reference implementation from the same lineage `[search]`:

```css
:root {
  --focus-inner: var(--surface-0);   /* the separator: matches page surface */
  --focus-outer: var(--accent-500);  /* the ring proper; ≥3:1 vs both surfaces */
}
:focus-visible {
  outline: 2px solid var(--focus-outer);
  outline-offset: 1px;
  box-shadow: 0 0 0 1px var(--focus-inner);  /* inner separator */
  border-radius: inherit;
}
```

Design notes:

- **`outline`, not `box-shadow`, for the visible ring** `[search]`: "Modern browsers respect `border-radius` on
  outlines, and outlines have one critical advantage: they don't get clipped by `overflow: hidden` parents,
  avoiding an entire category of bug." Our board of cards and the scrollable rail are exactly the
  `overflow:hidden` ancestors that break `box-shadow` rings.
- **`:focus-visible`, not `:focus`** `[search]`: "Focus rings only appear when the user is navigating by
  keyboard, not when they click, so mouse users never see them." Never `outline: none` without an immediate
  replacement in the same rule block.
- **One global rule, not per-component rules.** Our build's single `:focus-visible` rule across 11 stylesheets
  is not "one too few" — the correct count is close to one *base* rule plus a small number of overrides for
  components that need an inset ring (full-bleed rows, the terminal surface) because an outset ring would be
  clipped by the viewport edge.
- **Ring colour must clear 3:1 against every surface it can land on**, both themes. If the accent fails on one
  surface, do not tune the accent — add a `--focus-outer` override on that surface token.
- **Focus must be scrolled into view fully**, per 2.4.11/2.4.12: give focusable rows `scroll-margin-block:
  var(--sticky-header-h) var(--sticky-footer-h)`.

### 3.3 The full state matrix

`[unverified]` as a specific matrix — this is my synthesis of the sources above, not a quotation. Each cell is
a token change, not an ad-hoc value.

| State | Background | Foreground | Border | Ring | Notes |
|---|---|---|---|---|---|
| **rest** | `--surface-*` or transparent | `--fg-default` | `--border-default` or none | none | Ghost/tertiary controls are transparent at rest. |
| **hover** | +1 step of surface tint (dark: lighter; light: darker) | unchanged | may appear on ghost controls | none | Hover is *not* a hierarchy signal; it must never be stronger than selected. Hover on a non-interactive element is a bug. |
| **active / pressed** | +2 steps, or the hover tint darkened | unchanged | unchanged | none | Should be visibly distinct from hover for the ~120ms it exists; no transition on the way *in*, short transition on the way out. |
| **focus-visible** | unchanged from whatever state it's in | unchanged | unchanged | 2px outer + 1px inner (§3.2) | Composes with hover/selected — it never replaces them. |
| **disabled** | unchanged or muted surface | `--fg-disabled` (≈ 38–45% of default) | muted | **none, and not focusable** | 1.4.11 explicitly exempts inactive components from the 3:1 rule, so low contrast is permitted here — but see the rejection list: prefer *not* disabling. |
| **selected** | `--surface-selected` (accent-tinted, low chroma) | `--fg-default`, weight 500→600 | left accent bar 2px, or full border | none unless also focused | Must be distinguishable from hover **and** must survive when the window is unfocused. Two channels minimum (tint + weight or tint + bar) because tint alone at 3–5% opacity is invisible on some panels. |
| **selected + inactive window** | tint desaturated to neutral | unchanged | unchanged | none | Classic IDE behaviour; tells you where you are without pretending the app has focus. |
| **loading / busy** | rest | rest | rest | rest | `aria-busy`; never swap to disabled (see rejections). |
| **error / invalid** | rest | rest | `--border-danger` | ring may take danger colour | Colour never alone — pair with an icon or message (WCAG 1.4.1 Use of Color, `[unverified]` wording). |

Two invariants worth turning into tests:
1. `selected` and `hover` never render identically for any component (compare computed styles).
2. Every interactive element has a `:focus-visible` style that differs from its rest style in ≥2 CSS pixels of
   perimeter.

---

## 4. Hierarchy channels and how systems ration them

Channels available: **size, weight, colour/tone, spacing, position, surface (elevation/fill), border,
alignment**, plus (in a live tool) *motion* and *density*.

| Channel | Cost when spent | Who warns about it |
|---|---|---|
| Size | Highest — changes row height, reflows everything | Dense tools implicitly, by hoarding it `[unverified]` |
| Weight | Near-zero — no layout change | Primer *recommends* it as the primary channel `[search]` |
| Tone (fg colour) | Near-zero, but a limited budget: 3–4 tones total | Refactoring UI: "A dark color for primary content, a grey for secondary content, and a lighter grey for tertiary content" `[search]` |
| Spacing | Cheap in a sparse layout, expensive in a dense one | Atlassian ships explicit dense (0–8px) vs comfortable (12–24px) token bands `[search]` (https://atlassian.design/foundations/spacing) |
| Surface/fill | Expensive — creates a "card" reading whether you meant it or not | M3 hedges on `elevated` buttons: shadow creep `[search]` |
| Border | Cheap, but borders accumulate into a grid that reads as noise | — |
| Position | Free, strongest signal available, and the one most often ignored | NN/g proximity article `[search]` |

**The rationing rule:** to make one thing more important, change **one** channel by a lot, or **two** channels
by a little — never three. Refactoring UI states the inverse constructively as "emphasize by de-emphasizing"
`[search]`: the fix for a weak primary is usually to lower everything around it (one channel, applied to the
many) rather than to raise it (three channels, applied to the one).

Refactoring UI also gives a specific trap we will hit in dark mode `[search]`: "Making text a lighter grey is
a great way to de-emphasize it on white backgrounds, but it doesn't look so great on colored backgrounds…
hand-pick a color with the same hue, adjust saturation and lightness." Our accent-tinted selected rows are
exactly "coloured backgrounds"; secondary text inside them needs its own token, not the global grey.

---

## 5. Content emphasis and de-emphasis

**Four foreground tones, and no more.** Primer's shipped set is the model `[search]`
(https://primer.style/foundations/color/overview/): `--fgColor-default` = primary text, `--fgColor-muted` =
secondary text, plus `subtle`/`disabled` tiers, all under a `fgColor` namespace with matching `bgColor` and
`borderColor` families, each having `muted` and `emphasis` options.

| Tone | Contrast target | Semantics — what is allowed to use it |
|---|---|---|
| `fg-default` | ≥ 7:1 (aim AAA for the thing you read for eight hours) | The content itself: report prose, card titles, values, row primary labels |
| `fg-muted` | ≥ 4.5:1 | Secondary but still content: descriptions, second line of a row, column headers |
| `fg-subtle` | ≥ 3:1, **never used for anything a user must read** | Metadata: timestamps, counts, IDs, keyboard hints, separators, unit suffixes |
| `fg-disabled` | no floor (1.4.11 exempts inactive components `[verified]`) | Disabled controls only |

Rules that keep these honest:

- **Metadata is `fg-subtle` + `text-sm`, and that is the only combination it gets.** If a timestamp needs to
  be more prominent, it is not metadata — it is a value, and it gets promoted in the data model, not restyled.
- **Empty states are content, not metadata.** They are `fg-muted` at `text-base`, and they carry an action.
  An empty state rendered in `fg-subtle` italic reads as "loading" or "broken". For an agent workspace the
  empty state of the agenda is a *frequent* state, not an edge case — design it as a first-class screen.
- **Placeholder text must never be the label.** Placeholders are `fg-subtle`; the field's label is separate and
  persistent. Placeholder-as-label fails the moment the user types, and it is a known a11y defect
  `[unverified — well established, not re-cited this session]`.
- **Colour is not the emphasis channel.** Primer again `[search]`: "refrain from utilizing color as a primary
  method of emphasis. Instead, font weight is adjusted." Reserve hue for *semantics* (accent = interactive,
  danger, warning, success) and use the neutral tone ladder for *importance*.
- Status colours need a non-colour partner in a workspace full of agent states: a running card, a blocked card
  and a failed card must differ by icon/shape as well as hue.

---

## 6. Published density numbers

| System | Number | Tag |
|---|---|---|
| Ant Design compact algorithm | `controlHeight` 32px → **28px**; `controlHeightSM` = 28 × 0.75 = **21px** | `[search]` (https://github.com/ant-design/ant-design/pull/58411) |
| Carbon data table row heights | 4 heights in v10; v11 adds a 5th (Medium **40px**); pagination bar aligned at **32px** | `[search]` (https://github.com/carbon-design-system/carbon/issues/8874) |
| Atlassian spacing bands | `space.0`–`space.100` = 0–8px for "small and compact pieces of UI"; `space.150`–`space.300` = 12–24px for "larger and less dense pieces" | `[search]` (https://atlassian.design/foundations/spacing) |
| Practitioner consensus | 28–32px for genuinely dense views; 36–40px middle ground; 48–52px comfortable. "Below 28px you start losing usable click targets" | `[search]` (https://artofstyleframe.com/blog/dashboard-data-density-patterns/) |
| VS Code list row | 22px at default zoom | `[unverified]` — measure in devtools before quoting |

Reconciling with WCAG 2.5.8: a **28px** row height for the rail/agenda is the sweet spot — comfortably above
the 24px target minimum, so a full-row click target passes without invoking the spacing exception, while
still reading as dense. Reserve 24px for non-interactive dense lists and 32px for anything with inline
controls (which need their own 24px targets inside the row).

Suggested ladder for us `[unverified, synthesised]`: spacing scale 2/4/6/8/12/16/24/32; rail row 28px; agenda
row 32px; card header 32px; button heights 24 (xs, icon-only in card headers, spaced per 2.5.8) / 28 (default)
/ 32 (page primary).

---

## 7. Dark mode that is not an inversion

All `[search]`, from Material's dark theme guidance and its codelab
(https://codelabs.developers.google.com/codelabs/design-material-darktheme) plus the practitioner write-ups
found alongside it:

1. **Base surface is near-black, not black**: "#121212 as the primary background colour instead of pure black…
   Dark grey reduces eye strain by lowering the amount of contrast between the surface and components…
   Near-black also lets you show elevation with subtle lighter layers, which pure black cannot."
2. **Elevation is expressed by lightening the surface, not by shadow**: "in dark theme… elevation level is
   also expressed by adjusting the colour of surface: higher elevation the lighter colour of surface."
   Practical consequence: our card stack needs 4–5 surface steps in dark (`surface-0..4`) where light mode may
   need only 2 plus borders.
3. **Desaturate the accent**: "Desaturate primary colors in order to make the contrast enough against the dark
   surface. More saturated colors tend to visually 'vibrate' against darker backgrounds." A light-mode accent
   reused verbatim in dark mode is the single most common dark-theme defect.
4. **No pure-white text**: "Pure whites for text and icons gives harsh contrast in dark mode; stick to
   transparent whites or light greys." Prefer *opaque* light greys over `rgba(255,255,255,.87)` in our case,
   because translucent foregrounds over translucent surfaces compound unpredictably and break contrast
   calculation.
5. **Light and dark are two designs sharing a token contract, not one design with a filter.** The mechanical
   form of this: semantic tokens (`--surface-1`, `--fg-muted`, `--border-default`) are the only things
   components reference; each theme supplies its own raw values, and the two themes are permitted to differ in
   *how many* raw steps they use.
6. Corollary specific to us `[unverified]`: in dark mode, borders do more work than shadows and less work than
   surface steps. Prefer `surface step + 1px hairline` for card edges; avoid large blurred shadows, which read
   as haze on OLED and on WebView compositing.

---

## 8. Ten adoptable decisions

| # | Decision | Source | Why it fits a dense agent workspace | Mechanically checkable? |
|---|---|---|---|---|
| 1 | **Two-axis button model: emphasis (filled / bordered / ghost) × tone (neutral / danger).** No tonal tier, no elevated tier. | Carbon `[search]` | Cards, rows, headers and dialogs all need the same four emphases at different scales; a 5-type taxonomy would double the component surface for no gain. | Yes — lint that every `<Button>` has `variant ∈ {filled,bordered,ghost}` and `tone ∈ {neutral,danger}`, and that no other button styling exists in CSS. |
| 2 | **One filled button per surface; card-level primaries max out at `bordered`.** | Material "single high-emphasis button per layout" + Carbon "one primary per view" `[search]` | A wave page shows 12 cards; without the scoping rule the page has 13 primaries and therefore none. | Partly — a DOM test can assert ≤1 `variant="filled"` per `[data-surface]` subtree. |
| 3 | **Destructive is red at rest, at the lowest emphasis that fits, placed last behind a separator; a filled red button exists only as the confirm action of a danger dialog.** | Carbon danger variants + NN/g proximity `[search]` | Agent workspaces are full of irreversible verbs (kill, delete wave, discard report). Hover-only red hides the signal exactly when the pointer is already on the item. | Yes — assert at most one `variant=filled tone=danger` per dialog and zero outside dialogs; assert destructive menu items are last and preceded by a separator. |
| 4 | **Confirmation ladder: undo > plain confirm > type-to-confirm, with type-to-confirm used exactly once (delete cove).** Initial focus always on the safe action. | NN/g `[search]`; Carbon a11y #10914 `[search]` | Preserves flow for the 99% while genuinely gating the cascade delete. Overusing type-to-confirm destroys its power ("becomes a new standard"). | Yes — test that a danger dialog's initially-focused element is the cancel action; count type-to-confirm usages in the codebase (must be 1). |
| 5 | **Global `:focus-visible` two-tone ring: `outline: 2px solid var(--focus-outer); outline-offset: 1px;` plus a 1px inner separator; `outline` not `box-shadow`.** | WCAG 2.4.13 `[verified]`; double-ring practice `[search]` | The board and rail are `overflow:hidden` ancestors that clip shadow rings; the two-tone ring survives on accent-tinted selected rows and on the terminal's black surface alike. | Yes — a11y test asserting a visible, ≥2px, ≥3:1 focus indicator on every focusable role; plus a CSS lint banning bare `outline: none`. |
| 6 | **24×24 CSS px minimum for every pointer target, satisfied by padding; icon-only buttons in card headers get 24px boxes with ≥24px centre spacing.** | WCAG 2.5.8 AA `[verified]`, incl. the Spacing exception's 24px-diameter-circle wording | Our densest surface (card headers with 3–5 glyph actions) is precisely where this breaks, and it is an AA obligation, not a nicety. | Yes — automated bounding-box audit in a browser test; it is the cheapest high-value gate we can add. |
| 7 | **Six-step type scale (11/12/13/15/18/22) with 13px as the default, and a three-step weight ladder (400/500/600) where weight is the in-row hierarchy channel and size is the between-region channel.** | Dense-tool lineage `[unverified numbers]`; Primer "font weight is adjusted to add emphasis" `[search]`; Refactoring UI "two font weights are usually enough" `[search]` | Fixes the stated defect directly: the weight channel is unused, so every distinction currently has to be paid for in size or colour, which a dense layout cannot afford. | Yes — lint that `font-size` and `font-weight` only take token values, and count distinct values (≤6 and ≤3). |
| 8 | **`tabular-nums` on the clock, week calendar, durations, counts, costs and all table columns — and nowhere else.** | MDN definition `[verified via snippet]`; scoping advice `[search]` | The Today clock and live card metrics tick every second; proportional digits make the whole layout twitch, which in a long-session tool is genuinely fatiguing. | Yes — assert `font-variant-numeric` on the clock/duration components; assert it is *absent* from report prose. |
| 9 | **Four foreground tones (`default/muted/subtle/disabled`) with fixed semantics; metadata is locked to `subtle + text-sm`; empty states are `muted + text-base` with an action; placeholders are never labels.** | Primer fgColor family `[search]`; Refactoring UI tone ladder `[search]` | The agenda is often empty and often full of timestamps; without locked semantics, metadata creeps up in prominence until every row looks equally urgent. | Partly — lint that raw colours never appear on text (tokens only); tone-vs-role pairing needs review. |
| 10 | **Dark mode is a second design over one token contract: near-black base, 4–5 lightening surface steps for elevation, desaturated accent, no pure white text, hairline borders instead of large shadows.** | Material dark theme `[search]` | WebView compositing plus OLED plus eight-hour sessions; and the board's stacked cards need elevation the shadow channel cannot supply in dark. | Partly — assert every component references only semantic tokens (no raw hex outside theme files); contrast ratios per theme are automatable. |

Bonus, mechanically checkable, cheap: **28px rail rows / 32px rows containing inline controls** (§6), and
**`scroll-margin-block` on every focusable row equal to the sticky header/footer heights** so 2.4.11 holds.

---

## 9. What I would reject

1. **Material 3's five button types and its 15-role type scale.** Both are calibrated for unknown content on
   unknown screens. Nine of the fifteen roles would be dead code, and `elevated` buttons contradict a flat,
   panel-based workspace. M3's *ideas* (emphasis ladder, one high-emphasis button) survive; its taxonomy
   doesn't. `[search]` for the taxonomy, judgement mine.
2. **Apple HIG's Dynamic Type ladder as a source of sizes.** 17pt body is correct for a phone at arm's length
   and 30% too large for a desktop workspace. Take the *principle* (one neutral system typeface, size classes
   rather than ad-hoc px) and reject the numbers. `[search]`
3. **Consumer modular scales (1.25×, 1.333×) applied to UI chrome.** They generate sizes with no referent in
   this product and push adjacent levels far enough apart that they can't share a row. Dense tools step
   additively.
4. **Colour as the primary emphasis channel.** Explicitly rejected by Primer for the same reason it should be
   rejected here `[search]`: with dozens of live agent status colours already competing, spending hue on
   *importance* leaves nothing to spend on *meaning*.
5. **Destructive styling that appears only on hover.** Widespread (it keeps lists calm), but it removes the
   warning from the keyboard path entirely and from the mouse path until the pointer is already there. NN/g's
   redundancy principle argues the opposite way. `[search]` + judgement.
6. **Confirmation dialogs as the default protection for everything.** NN/g is explicit that they should not be
   the sole error-prevention method and that over-used non-standard confirmations lose their power. In an
   agent workspace where the user performs the same destructive verb dozens of times a day, dialogs become
   muscle memory within a week. Undo is the correct default. `[search]`
7. **Disabled buttons as the standard way to express "not yet".** Disabled controls are exempt from contrast
   requirements `[verified: 1.4.11 excludes inactive components]`, are typically not focusable, and give no
   reason. Prefer an enabled control that explains why it can't proceed, or a `aria-busy` loading state that
   keeps focus. (Judgement; widely argued in a11y practice `[unverified]`.)
8. **`44×44` everywhere.** That is 2.5.5 at AAA and a touch-first number; applying it to a desktop keyboard-
   driven workspace would destroy the density that is the product's point. 2.5.8's 24px AA floor plus the
   spacing exception is the correct target. `[verified]` for the 24px figure.
9. **Pure black (#000) dark theme and pure white text.** Material's guidance is direct on both, and both are
   actively harmful over an eight-hour session. `[search]`
10. **A global `tabular-nums`.** Tempting given how numeric this app is, but it makes report prose look like a
    spreadsheet; the sources scope it to alignment-critical contexts. `[search]`
11. **Per-component focus styling.** Eleven stylesheets with their own focus opinions is how you end up with
    the one-rule situation we have now, inverted. One base rule plus a documented, short override list.
12. **Hover states on non-interactive rows.** In a dense read-heavy view, hover feedback on things you cannot
    click trains the user to distrust the signal — and it competes with `selected`, which must always be the
    strongest row state.

---

## Source list

Primary (fetched and read this session):
- WCAG 2.2, W3C Recommendation — https://www.w3.org/TR/WCAG22/
- Understanding SC 2.5.8 Target Size (Minimum) — https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html

Secondary (search snippets):
- Carbon Button usage — https://carbondesignsystem.com/components/button/usage/ (and v10/v11 mirrors)
- Carbon a11y issue: focus the least destructive action — https://github.com/carbon-design-system/carbon/issues/10914
- Carbon data table row heights — https://github.com/carbon-design-system/carbon/issues/8874
- Material 3 all buttons — https://m3.material.io/components/all-buttons
- Material dark theme codelab — https://codelabs.developers.google.com/codelabs/design-material-darktheme
- Apple HIG Typography — https://developer.apple.com/design/human-interface-guidelines/typography
- Primer UI color system — https://primer.style/foundations/color/overview/
- Primer Typography — https://primer.github.io/design/foundations/typography/
- NN/g Confirmation Dialogs Can Prevent User Errors — https://www.nngroup.com/articles/confirmation-dialog/
- NN/g Preventing User Errors: Avoiding Conscious Mistakes — https://www.nngroup.com/articles/user-mistakes/
- NN/g Dangerous UX: Consequential Options Close to Benign Options — https://www.nngroup.com/articles/proximity-consequential-options/
- NN/g Cancel vs Close — https://www.nngroup.com/articles/cancel-vs-close/
- Sara Soueidan, Accessible Icon Buttons — https://www.sarasoueidan.com/blog/accessible-icon-buttons/
- Level Access, ARIA Labels and Accessible Names — https://www.levelaccess.com/blog/aria-labels-and-accessible-names-a-developers-guide/
- Piccalilli, the double focus ring problem — https://piccalil.li/blog/taking-a-shot-at-the-double-focus-ring-problem-using-modern-css/
- Deque, Designing Usable Focus Indicators — https://www.deque.com/blog/give-site-focus-tips-designing-usable-focus-indicators/
- Darek Kay, Implementing an accessible focus indicator — https://darekkay.com/blog/accessible-focus-indicator/
- MDN font-variant-numeric — https://developer.mozilla.org/en-US/docs/Web/CSS/font-variant-numeric
- Atlassian spacing — https://atlassian.design/foundations/spacing
- Ant Design compact control height PR — https://github.com/ant-design/ant-design/pull/58411
- Dashboard data density patterns — https://artofstyleframe.com/blog/dashboard-data-density-patterns/
- Refactoring UI notes mirror — https://gist.github.com/selcukcihan/b9418596a98abfcd4bbc622550820cc5
- Linear typography analysis — https://typescale.app/typescales/typescale-and-typography-system-of-linear
- Raycast design system breakdown — https://oh-my-design.kr/design-systems/raycast
