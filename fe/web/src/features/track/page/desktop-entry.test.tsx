// @vitest-environment jsdom
//
// #1234 S1b-3b — the **entry** oracle: it catches a page that **discards the
// painter's whole return value**, and it catches a **marker-bearing bypass**
// drawn beside it. It does *not* prove "the page can only put content on screen
// through the painter" — see the honest limit at the end of this head.
//
// **Why this file exists.** `desktop-projection.test.tsx` checks that the
// rendered desktop panel is a faithful projection of the view model, and
// `checkProjectionIn` takes the painter on trust — it cannot prove the DOM it
// reads came from that painter. The argument that used to close that gap was a
// **source scan**: `public.tsx` spells none of `MARKER`'s attribute names, so
// the only writer of those attributes into the panel is the painter. That scan
// is worth keeping and it is not a closed proof, because it is bound to the
// *spelling* of the markers rather than to the concept, and at least four ways
// past it need no literal at all:
//
//   * a computed property — `{...{[MARKER.module]: 'cards'}}`;
//   * assembly — `'data-' + 'nc-module'`;
//   * a marker-channel prop that spells the attribute somewhere else
//     (`ui/panel-card`'s `moduleMarker` / `titleFieldMarker` do exactly this,
//     legitimately);
//   * importing a component from another file that carries markers of its own.
//
// So the scan's honest claim is narrow: **the page does not rewrite a marker
// literal in place.** What this file adds is an oracle whose load-bearing
// assertions do not depend on how a marker is spelled — it holds the *call*,
// and it reads a tag and the fixture's own text, so no spelling can go round
// those.
//
// **Two obligations, and the second is the one that bites.** "It was called" is
// satisfied by a page that calls the painter and then throws the result away
// and draws its own panel beside it. So the mock's return value is tagged, and
// the tag has to be *in the desktop panel subtree*; and in the third case the
// mock returns the tag **instead of** the painted modules, which leaves the
// subtree with neither a projection marker **nor any of the fixture's own
// module titles and row text** in it unless something other than the painter
// put them there. The text half is what closes the cheapest bypass the marker
// counts alone would miss: a page that renders the painter's output *and*
// hand-builds a second, entirely unmarked copy of the same modules beside it
// would satisfy every marker count here, because in `replace` the painter
// contributes no markers and the hand-built copy carries none either — but it
// cannot avoid printing `Cards` / `Build log` / `alpha-gate`.
//
// **The honest limit.** Even so, this file does not close "all Cards/Tasks
// row-module content comes from the painter" — which is the statement at stake,
// not "all content in the desktop panel", since the panel legitimately holds
// `Referenced by` and `Conversations` composed by the page itself. The text
// assertions are bound to *this
// fixture's* strings, so a bypass that renders different content, or content
// only for inputs this fixture does not use, is outside them. As for spelling:
// the **call**, **tag** and **text** assertions here — the view equality, the
// tag's presence in the subtree, and `PAINTED_TEXT` surviving in `wrap` and
// vanishing in `replace` — name no marker and are spelling-independent. It is
// the **marker-count** assertions that read `MARKER.module` / `MARKER.row`
// directly, and those inherit the same spelling-boundness the source scan has.
//
// **Why a whole file for it.** `vi.mock` is module-wide, so arming this one in
// `desktop-projection.test.tsx` would put a mock underneath that suite's
// faithful-projection cases too. The two files divide the work: the projection
// suite renders the page with nothing mocked, this one holds the entry.

import { cleanup } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import type { CardWire } from '../../../../../core/domain/track.ts';
import { MARKER } from '../../../../../core/view/panel.ts';
import type { RowPainter, TrackPageView } from '../../../../../core/view/panel.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import type { DesktopLeaf } from './desktop-painter.tsx';
import { card, renderPage } from './test-fixtures.tsx';

/** Every call the page made, in order. */
const calls: { painter: RowPainter<DesktopLeaf>; view: TrackPageView }[] = [];

/**
 * What the mock hands back.
 *
 *  - `wrap` — the painter's own modules, with the tag in front. The page under
 *    test is unchanged apart from one extra node.
 *  - `replace` — the tag **and nothing else**, which is what makes "the page
 *    draws no panel of its own" observable.
 */
let mode: 'wrap' | 'replace' = 'wrap';

vi.mock('./desktop-painter.tsx', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./desktop-painter.tsx')>();
  return {
    ...actual,
    paintDesktopPanel: (painter: RowPainter<DesktopLeaf>, view: TrackPageView): readonly ReactNode[] => {
      calls.push({ painter, view });
      /* The tag is not a projection marker and deliberately shares no prefix
         with one: it must not be something `checkProjectionIn`'s selectors or
         the page's own source scan could ever read as a marker. */
      const tag = <div key="painted-here" data-entry-oracle-tag="" />;
      return mode === 'replace' ? [tag] : [tag, ...actual.paintDesktopPanel(painter, view)];
    },
  };
});

afterEach(() => {
  cleanup();
  calls.length = 0;
  mode = 'wrap';
});

const CARDS: readonly CardWire[] = [
  card({ id: 'card-1', kind: 'terminal', title: 'Build log', deletable: true }),
  card({ id: 'card-2', kind: 'harness', title: null, deletable: false }),
];

const TASKS: readonly ReportTaskRow[] = [
  {
    blockId: 'block-1', key: 'alpha-gate', state: 'ready', declaration: null,
    status: 'running', statusDetail: 'step 2 of 3', kind: 'codex', workerCardId: 'card-1',
  },
  {
    blockId: 'block-2', key: 'beta-gate', state: 'withdrawn', declaration: 'Withdrawn',
    status: null, statusDetail: null, kind: null, workerCardId: null,
  },
];

/**
 * Strings only the painted row modules can put on screen: the two module
 * titles, both card names (`card-2` is untitled, so its name is its kind) and
 * both task keys.
 *
 * These are what make the `replace` case bite against a **hand-built, unmarked**
 * second copy of the panel: such a copy contributes no `data-nc-*` attribute,
 * so the marker counts stay at zero, but it still has to print the text.
 *
 * Read as substrings of the subtree's `textContent`, deliberately: the point is
 * "this content is not on screen at all", not which element carries it — that is
 * `desktop-projection.test.tsx`'s question.
 */
const PAINTED_TEXT: readonly string[] = [
  'Cards', 'Tasks', 'Build log', 'harness', 'alpha-gate', 'beta-gate',
];

/** The desktop panel subtree — the same root `desktop-projection.test.tsx`
 *  scopes to, and for the same reason: the mobile surface is a sibling that is
 *  in the DOM at the same time. */
function desktopPanel(container: Element): Element {
  const root = container.querySelector('[data-nc-desktop-panel]');
  expect(root, 'the desktop panel surface must be findable').not.toBeNull();
  return root!;
}

describe('the page paints its desktop panel through paintDesktopPanel', () => {
  it('calls it once, with the whole derived view and both modules', () => {
    renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });

    expect(calls.length, 'paintDesktopPanel calls').toBe(1);
    /* The **whole** view, not a slice of it: an equality against the derivation
       run independently here catches a page that filtered a module away, or
       reordered them, or paints from something else entirely. */
    expect(calls[0].view).toEqual(deriveTrackPageView({ cards: CARDS, tasks: TASKS }));
    /* Spelled out too, because the equality above would also be satisfied by a
       derivation that had itself lost a module. */
    expect(calls[0].view.rowModules.map((module) => module.key)).toEqual(['cards', 'tasks']);
    expect(calls[0].view.rowModules.map((module) => module.rows.length))
      .toEqual([CARDS.length, TASKS.length]);
  });

  it('renders what it handed back, inside the desktop panel', () => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });
    const root = desktopPanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length, 'the painter’s node').toBe(1);
    /* Not vacuous: in `wrap` the painted modules are there as well, which is
       the state the `replace` case below removes. */
    expect(MARKER.module).toBe('data-nc-module');
    expect(MARKER.row).toBe('data-nc-row');
    expect(root.querySelectorAll('[data-nc-module]').length).toBe(2);
    expect(root.querySelectorAll('[data-nc-row]').length).toBe(CARDS.length + TASKS.length);
    /* And the text those modules print is on screen — which is what makes its
       absence in `replace` below a real observation rather than a string that
       was never rendered on this page in the first place. */
    for (const text of PAINTED_TEXT) {
      expect(root.textContent, `wrap renders ${text}`).toContain(text);
    }
  });

  it('and draws no Cards/Tasks row-module of its own beside it, for this fixture', () => {
    mode = 'replace';
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });
    const root = desktopPanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length).toBe(1);
    /* The page composed the **row-module** part of the panel card out of the
       painter's return value and nothing else. The panel legitimately holds
       more than that: `Referenced by` / `Conversations` are `PanelModule`s of
       the page's own and are correctly unmarked (`ui/panel-card`'s channels are
       opt-in), so zero *marked* modules is the right number here and not an
       accident of scoping — and it is a claim about Cards/Tasks only. */
    expect(root.querySelectorAll('[data-nc-module]').length, 'row modules the page drew itself').toBe(0);
    expect(root.querySelectorAll('[data-nc-row]').length, 'rows the page drew itself').toBe(0);
    /* The markers are gone **and so is the content** — for this fixture's
       strings. Without this second half a page that renders the painter's
       return value and hand-builds an unmarked duplicate of the Cards/Tasks
       modules beside it stays green: the duplicate carries no `data-nc-*`, so
       both counts above are still zero. A duplicate printing *other* content
       is still outside this case; see the honest limit in this file's head. */
    for (const text of PAINTED_TEXT) {
      expect(root.textContent, `no unmarked copy of ${text} survives`).not.toContain(text);
    }
  });
});
