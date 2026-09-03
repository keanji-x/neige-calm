// @vitest-environment jsdom
//
// #1234 S1b-4a / S1b-4b — the mobile **entry** oracle, over both row modules: it
// catches a page that **discards the painter's whole return value**, and it
// catches a **bypass drawn beside it**, marked or not. It does *not* prove "the
// mobile panel can only put content on screen through the painter" — see the
// honest limit at the end of this head.
//
// **Why this file exists.** `mobile-projection.test.tsx` checks that a rendered
// mobile row-module page is a faithful projection of the view model, and
// `checkProjectionIn` takes the painter on trust — it cannot prove the DOM it
// reads came from that painter. "It was called" does not close that either: a
// page can call the painter, throw the result away and draw its own list beside
// it. So the mock's return value is tagged, the tag has to be **in the mobile
// panel subtree**, and in the third case the mock returns the tag **instead of**
// the painted module, which leaves the subtree with neither a projection marker
// **nor any of the fixture's own text** unless something other than the painter
// put it there. The text half is what closes the cheapest bypass the marker
// counts alone would miss: a hand-built, entirely unmarked copy of the drilled
// list contributes no `data-nc-*` at all, but it cannot avoid printing the
// module's own title and its rows' words.
//
// **The load-bearing assertions name no marker.** The call, the module equality,
// the tag and the fixture's strings are all spelling-independent; only the
// marker-*count* assertions read `MARKER.module` / `MARKER.row`, and those
// inherit the same spelling-boundness the desktop's source scan has.
//
// **The honest limit.** The text assertions are bound to *this fixture's*
// strings, so a bypass rendering different content, or content only for inputs
// this fixture does not use, is outside them. And the module *sequence* — that
// the mobile navigation offers Cards and Tasks, in that order — is still not this
// file's claim: it is `public.test.tsx`'s "the drill-down menu offers exactly the
// derived row modules" (Δ2), which reads the menu rather than a painter call.
//
// **Both row modules since S1b-4b.** The Tasks describe below is the same four
// assertions over the Tasks branch; the fixture there renders cards *as well*,
// so "it passed the right module" is a live question rather than one the input
// forecloses.
//
// **Why a whole file for it.** `vi.mock` is module-wide, so arming it in
// `mobile-projection.test.tsx` would put a mock underneath that suite's
// faithful-projection cases too.

import { cleanup } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import type { CardWire } from '../../../../../core/domain/track.ts';
import { MARKER } from '../../../../../core/view/panel.ts';
import type { RowModuleView, RowPainter } from '../../../../../core/view/panel.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import type { MobileLeaf } from './mobile-painter.tsx';
import { card, renderPage } from './test-fixtures.tsx';

/** Every call the page made, in order. */
const calls: { painter: RowPainter<MobileLeaf>; module: RowModuleView }[] = [];

/**
 * What the mock hands back.
 *
 *  - `wrap` — the painted module, with the tag in front. The page under test is
 *    unchanged apart from one extra node.
 *  - `replace` — the tag **and nothing else**, which is what makes "the page
 *    draws no Cards list of its own" observable.
 */
let mode: 'wrap' | 'replace' = 'wrap';

vi.mock('./mobile-painter.tsx', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./mobile-painter.tsx')>();
  return {
    ...actual,
    paintMobileModule: (painter: RowPainter<MobileLeaf>, module: RowModuleView): ReactNode => {
      calls.push({ painter, module });
      /* The tag is not a projection marker and deliberately shares no prefix
         with one: it must not be something `checkProjectionIn`'s selectors could
         ever read as a marker. */
      const tag = <div key="painted-here" data-entry-oracle-tag="" />;
      return mode === 'replace'
        ? tag
        : <>{tag}{actual.paintMobileModule(painter, module)}</>;
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

/**
 * Strings only the painted Cards module can put on this surface: the module
 * title, the titled card's name and the untitled one's kind — which is its
 * derived name.
 *
 * Read as substrings of the subtree's `textContent`, deliberately: the point is
 * "this content is not on screen at all", not which element carries it — that is
 * `mobile-projection.test.tsx`'s question.
 */
const PAINTED_TEXT: readonly string[] = ['Cards', 'Build log', 'harness'];

/** The Tasks fixture (S1b-4b). `beta-gate` carries a run, so its row prints a
 *  word only the painter can produce here — the page has no wording of its own
 *  left. */
const TASKS: readonly ReportTaskRow[] = [
  {
    blockId: 'block-1', key: 'alpha-impl', state: 'ready', declaration: null,
    status: null, statusDetail: null, kind: 'codex', workerCardId: null,
  },
  {
    blockId: 'block-2', key: 'beta-gate', state: 'not-ready', declaration: null,
    status: 'failed', statusDetail: 'not a git repository',
    kind: 'terminal', workerCardId: 'card-9',
  },
];

/** The module title, both task keys, and the status word. */
const PAINTED_TASK_TEXT: readonly string[] = ['Tasks', 'alpha-impl', 'beta-gate', 'failed'];

/** The mobile panel subtree — the same root `mobile-projection.test.tsx` scopes
 *  to, and for the same reason: the desktop surface is a sibling that is in the
 *  DOM at the same time and carries markers of its own. */
function mobilePanel(container: Element): Element {
  const root = container.querySelector('[data-nc-mobile-panel]');
  expect(root, 'the mobile panel surface must be findable').not.toBeNull();
  return root!;
}

describe('the page paints its mobile Cards page through paintMobileModule', () => {
  it('calls it once, with the Cards module of the derived view', () => {
    renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });

    expect(calls.length, 'paintMobileModule calls').toBe(1);
    /* Equality against the derivation run independently here: it catches a page
       that passed the Tasks module, a hand-built module, or a filtered copy of
       the right one. */
    const expected = deriveTrackPageView({ cards: CARDS, tasks: [] }).rowModules
      .find((module) => module.key === 'cards');
    expect(calls[0].module).toEqual(expected);
    /* Spelled out too, because the equality above would also be satisfied by a
       derivation that had itself lost every row. */
    expect(calls[0].module.key).toBe('cards');
    expect(calls[0].module.rows.map((row) => row.id)).toEqual(['card-1', 'card-2']);
  });

  it('does not call it while the panel is closed', () => {
    renderPage({ cards: CARDS });
    expect(calls.length).toBe(0);
  });

  it('renders what it handed back, inside the mobile panel', () => {
    const { container } = renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });
    const root = mobilePanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length, 'the painter’s node').toBe(1);
    /* Not vacuous: in `wrap` the painted module is there as well, which is the
       state the `replace` case below removes. */
    expect(MARKER.module).toBe('data-nc-module');
    expect(MARKER.row).toBe('data-nc-row');
    expect(root.querySelectorAll('[data-nc-module]').length).toBe(1);
    expect(root.querySelectorAll('[data-nc-row]').length).toBe(CARDS.length);
    for (const text of PAINTED_TEXT) {
      expect(root.textContent, `wrap renders ${text}`).toContain(text);
    }
  });

  it('and draws no Cards list of its own beside it, for this fixture', () => {
    mode = 'replace';
    const { container } = renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });
    const root = mobilePanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length).toBe(1);
    expect(root.querySelectorAll('[data-nc-module]').length, 'modules the page drew itself').toBe(0);
    expect(root.querySelectorAll('[data-nc-row]').length, 'rows the page drew itself').toBe(0);
    /* The markers are gone **and so is the content** — for this fixture's
       strings. Without this second half a page that renders the painter's return
       value and hand-builds an unmarked duplicate of the Cards list beside it
       stays green: the duplicate carries no `data-nc-*`, so both counts above
       are still zero. */
    for (const text of PAINTED_TEXT) {
      expect(root.textContent, `no unmarked copy of ${text} survives`).not.toContain(text);
    }
  });
});

describe('the page paints its mobile Tasks page through paintMobileModule', () => {
  it('calls it once, with the Tasks module of the derived view', () => {
    renderPage({ cards: CARDS, tasks: TASKS, panel: 'tasks', onOpenTask: vi.fn() });

    expect(calls.length, 'paintMobileModule calls').toBe(1);
    /* Equality against the derivation run independently here: it catches a page
       that passed the Cards module, a hand-built one, or a filtered copy. The
       page is rendered *with cards too*, so passing the wrong module is a live
       possibility rather than one the fixture forecloses. */
    const expected = deriveTrackPageView({ cards: CARDS, tasks: TASKS }).rowModules
      .find((module) => module.key === 'tasks');
    expect(calls[0].module).toEqual(expected);
    expect(calls[0].module.key).toBe('tasks');
    expect(calls[0].module.rows.map((row) => row.id)).toEqual(['block-1', 'block-2']);
  });

  it('renders what it handed back, inside the mobile panel', () => {
    const { container } = renderPage({
      cards: CARDS, tasks: TASKS, panel: 'tasks', onOpenTask: vi.fn(),
    });
    const root = mobilePanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length, 'the painter’s node').toBe(1);
    expect(MARKER.module).toBe('data-nc-module');
    expect(MARKER.row).toBe('data-nc-row');
    expect(root.querySelectorAll('[data-nc-module]').length).toBe(1);
    expect(root.querySelectorAll('[data-nc-row]').length).toBe(TASKS.length);
    for (const text of PAINTED_TASK_TEXT) {
      expect(root.textContent, `wrap renders ${text}`).toContain(text);
    }
  });

  /* "It was called" does not catch a page that calls the painter and draws its
     own Tasks list beside it — which is exactly the shape this branch had until
     this slice. With the painter's output replaced by a bare tag, neither a
     marker nor any of the fixture's own strings may survive. */
  it('and draws no Tasks list of its own beside it, for this fixture', () => {
    mode = 'replace';
    const { container } = renderPage({
      cards: CARDS, tasks: TASKS, panel: 'tasks', onOpenTask: vi.fn(),
    });
    const root = mobilePanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length).toBe(1);
    expect(root.querySelectorAll('[data-nc-module]').length, 'modules the page drew itself').toBe(0);
    expect(root.querySelectorAll('[data-nc-row]').length, 'rows the page drew itself').toBe(0);
    for (const text of PAINTED_TASK_TEXT) {
      expect(root.textContent, `no unmarked copy of ${text} survives`).not.toContain(text);
    }
    /* The wording the branch used to invent is gone with it: a hand-built copy
       that fell back to the old four words would still be caught above by the
       task keys, and this pins the words themselves. */
    for (const word of ['Ready', 'Not ready', 'Withdrawn', 'Unreadable']) {
      expect(root.textContent, `no ${word} survives`).not.toContain(word);
    }
  });
});
