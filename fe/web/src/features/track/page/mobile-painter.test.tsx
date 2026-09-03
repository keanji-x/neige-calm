// @vitest-environment jsdom
//
// #1234 S1b-4a / S1b-4b — the mobile painter, against `checkProjection`'s
// synthetic mount.
//
// **This file checks the painter, not the page.** It paints with the painter it
// checks, which is the one thing `checkProjection` guarantees and
// `checkProjectionIn` cannot. Whether `public.tsx` renders through this painter
// at all is `mobile-entry.test.tsx`'s claim, with `mobile-projection.test.tsx`
// checking the resulting real DOM against the view model.
//
// The capability cases are here because support is a *painter* fact, and on this
// surface it is the deliberate inconsistency itself (D1 / D7): the two card
// actions are not offered, and each says why.

import { render } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { checkProjection } from '../../../../../tools/projection/public.ts';
import type { ProjectionNode } from '../../../../../tools/projection/public.ts';
import { makeMobilePainter, paintMobileModule, type MobileLeaf } from './mobile-painter.tsx';

/** The one mount every case here uses: it unwraps each module leaf and renders
 *  exactly what the painter painted, nothing fabricated. */
const mount = (painted: readonly MobileLeaf[]): ProjectionNode =>
  render(<>{painted.map((leaf) => {
    if (leaf.slot !== 'module') throw new Error(`checkProjection handed back a ${leaf.slot} leaf`);
    return leaf.node;
  })}</>).container;

/** A titled, deletable card: `title !== kind`, so both fields have a carrier of
 *  their own, and the derivation offers the two actions this surface refuses. */
const titled: PanelRow = {
  id: 'card-1',
  title: 'Build log',
  kind: 'terminal',
  badges: [],
  status: null,
  actions: [
    { kind: 'open-card', cardId: 'card-1', label: null, hint: null },
    { kind: 'delete-card', cardId: 'card-1', label: 'Delete card Build log', hint: 'Delete card' },
  ],
};

/** Untitled and kernel-owned: the derived name *is* the kind, so there is no
 *  separate `kind` carrier — the row that used to print the word twice. */
const untitled: PanelRow = {
  id: 'card-2',
  title: 'harness',
  kind: null,
  badges: [{ id: 'kernel-owned', text: 'kernel-owned', struck: false }],
  status: null,
  actions: [{ kind: 'open-card', cardId: 'card-2', label: null, hint: null }],
};

const cardsModule: RowModuleView = {
  key: 'cards', title: 'Cards', empty: 'No cards yet.', rows: [titled, untitled],
};
const emptyCards: RowModuleView = { ...cardsModule, rows: [] };

/* ── Task rows (S1b-4b) ─────────────────────────────────────────────────────
 *
 * Shapes written the way `core/view/track-page.ts` derives them, so the fixture
 * cannot offer a combination the derivation never produces: `declaration` is
 * null once there is a `status`, and `open-card` exists only when
 * `kind !== null && workerCardId !== null`. `mobile-projection.test.tsx` runs
 * the real derivation over real task rows; this file is the painter in
 * isolation, so it spells the view model out.
 */

/** Ready and undispatched: no declaration word, no run — the row `ready` used
 *  to print `Ready` for on this surface and nowhere else (D8). */
const ready: PanelRow = {
  id: 'block-1',
  title: 'alpha-impl',
  kind: 'codex',
  badges: [],
  status: null,
  actions: [{ kind: 'reveal-block', blockId: 'block-1', label: null, hint: 'Show alpha-impl in the report' }],
};

/** Dispatched, with a reason: a status supersedes the readiness word, and the
 *  worker card offers an `open-card` this surface refuses. */
const dispatched: PanelRow = {
  id: 'block-2',
  title: 'beta-gate',
  kind: 'terminal',
  badges: [],
  status: { token: 'failed', phrase: 'failed — track /tmp/alpha is not a git repository' },
  actions: [
    { kind: 'reveal-block', blockId: 'block-2', label: null, hint: 'Show beta-gate in the report' },
    { kind: 'open-card', cardId: 'card-9', label: null, hint: 'Open the worker card for beta-gate' },
  ],
};

/** Withdrawn: the struck declaration, and no kind — `deriveReportTasks` makes
 *  `kind` and `workerCardId` null together for exactly these rows. */
const withdrawn: PanelRow = {
  id: 'block-3',
  title: 'gamma-planner',
  kind: null,
  badges: [{ id: 'declaration', text: 'Withdrawn', struck: true }],
  status: null,
  actions: [{ kind: 'reveal-block', blockId: 'block-3', label: null, hint: 'Show gamma-planner in the report' }],
};

/** Unreadable: an ordinary, unstruck declaration beside the withdrawn one. */
const unreadable: PanelRow = {
  id: 'block-4',
  title: 'delta-doc',
  kind: null,
  badges: [{ id: 'declaration', text: 'Unreadable', struck: false }],
  status: null,
  actions: [{ kind: 'reveal-block', blockId: 'block-4', label: null, hint: 'Show delta-doc in the report' }],
};

/** Declared but not ready, and never dispatched: **the one row that carries a
 *  declaration badge and a kind at once**, which is what makes "each is its own
 *  leaf carrier" a claim with a witness rather than a rule about a shape no
 *  fixture reaches. Its readiness word survives D8 — the word stands down for a
 *  run, and there is none. */
const notReady: PanelRow = {
  id: 'block-5',
  title: 'epsilon-fix',
  kind: 'codex',
  badges: [{ id: 'declaration', text: 'Not ready', struck: false }],
  status: null,
  actions: [{ kind: 'reveal-block', blockId: 'block-5', label: null, hint: 'Show epsilon-fix in the report' }],
};

/**
 * A `reveal-block` that **names itself**, and a synthetic row on purpose.
 *
 * `deriveTrackPageView` words this action with `label: null` on every Task row it
 * produces today, so a painter that never read `RowAction.label` is green
 * against every derived fixture in this file — green by an invariant of
 * `core/view/track-page.ts` rather than by consuming the channel it is handed.
 * `RowAction` declares `label` as a channel of its own and the projection checks
 * it on both sides, so the painter owes it a case. `hint` is null here so the
 * two channels cannot cover for one another.
 */
const namedReveal: PanelRow = {
  id: 'block-6',
  title: 'zeta-audit',
  kind: null,
  badges: [],
  status: null,
  actions: [{
    kind: 'reveal-block', blockId: 'block-6', label: 'Reveal zeta-audit', hint: null,
  }],
};

const tasksModule: RowModuleView = {
  key: 'tasks',
  title: 'Tasks',
  empty: 'No tasks declared yet.',
  rows: [ready, dispatched, withdrawn, unreadable, notReady],
};
const emptyTasks: RowModuleView = { ...tasksModule, rows: [] };

/* ── Three boundaries of `PanelRow` the fixtures above cannot reach ──────────
 *
 * **Written at the painter level, and deliberately not as `deriveReportTasks`
 * output.** This file checks the painter against `PanelRow`, and `PanelRow` is
 * the contract it is handed — a fixture set that only ever contains shapes
 * today's derivation happens to emit makes the painter correct by an upstream
 * invariant rather than by consuming its input (the same defect `reveal()`'s
 * missing `label` was). Each row below is one boundary, and each has a mutation
 * that only it can see.
 */

/** **`phrase === token`** — a run the kernel gave no reason for. Every derived
 *  fixture above with a status has a `statusDetail`, so a painter that stopped
 *  writing `title` whenever the phrase added nothing would be green across all
 *  of them, and the row would lose its tooltip on the majority case. */
const bareStatus: PanelRow = {
  id: 'block-7',
  title: 'eta-run',
  kind: 'terminal',
  badges: [],
  status: { token: 'running', phrase: 'running' },
  actions: [{ kind: 'reveal-block', blockId: 'block-7', label: null, hint: 'Show eta-run in the report' }],
};

/** **A badge and a status at once.** `deriveTrackPageView` reads `declaration`
 *  and `status` independently and says so, so the combination is inside the
 *  contract even though D8's wording rule keeps it out of today's derived rows.
 *  Without it, a painter that dropped the badges of any row carrying a run is
 *  green: every other fixture has at most one of the two. */
const declaredAndRunning: PanelRow = {
  id: 'block-8',
  title: 'theta-check',
  kind: null,
  badges: [{ id: 'declaration', text: 'Not ready', struck: false }],
  status: { token: 'failed', phrase: 'failed — the worker exited before it reported' },
  actions: [{ kind: 'reveal-block', blockId: 'block-8', label: null, hint: 'Show theta-check in the report' }],
};

/** **An empty token.** `deriveReportTasks` folds `''` into `null` today, but
 *  `RowStatus` permits it and the projection's own generic checker exercises it
 *  (`desktop-projection.test.tsx` carries the same row), so the painter's
 *  contract fixture owes it one: an emptiness test in front of the status
 *  carrier would drop the element and nothing here would notice. The `phrase` is
 *  kept non-empty on purpose — it keeps `status-token` and `status-phrase`
 *  separable on this row rather than letting one empty string satisfy both. */
const emptyToken: PanelRow = {
  id: 'block-9',
  title: 'iota-probe',
  kind: 'claude',
  badges: [],
  status: { token: '', phrase: 'the kernel has not named this state' },
  actions: [{ kind: 'reveal-block', blockId: 'block-9', label: null, hint: 'Show iota-probe in the report' }],
};

const boundaryTasks: RowModuleView = {
  ...tasksModule,
  rows: [bareStatus, declaredAndRunning, emptyToken],
};

const painter = () => makeMobilePainter({ backLabel: 'Report', onBack: vi.fn() });

describe('the mobile painter’s capability table', () => {
  it('supports reveal-block', () => {
    expect(painter().action['reveal-block']).toEqual({ supported: true });
  });

  it('offers neither card action, and says why for each', () => {
    for (const kind of ['open-card', 'delete-card'] as const) {
      const support = painter().action[kind];
      expect(support.supported, kind).toBe(false);
      expect(support.supported === false && support.why.length > 0, kind).toBe(true);
    }
  });

  /* The table is not bound to a host prop on this surface, unlike the desktop's
     `delete-card`: not offering the card actions is a fact about the viewport. */
  it('says the same thing whatever the host passes', () => {
    const withHandlers = makeMobilePainter({ onOpenTask: vi.fn(), backLabel: 'Report', onBack: vi.fn() });
    expect(withHandlers.action).toEqual(makeMobilePainter({}).action);
  });
});

/*
 * The unknown-module guard, which nothing reached.
 *
 * `const unknown: never = moduleKey` is an **exhaustiveness check and nothing
 * more**: it fires when `RowModuleView['key']` gains a member and is erased
 * entirely at runtime. The `throw` beside it is the runtime half — and mutating
 * it into `return cardRow(row)` leaves `tsc` green and every legal fixture in
 * this file green with it, because no legal fixture can carry a third key.
 *
 * So the case is built with a forced cast, which is the only thing that can
 * produce the input the guard exists for: a module whose key is not in the
 * union, arriving from a derivation that has changed underneath this painter.
 * Painting it as a Cards row would report the fault far from its cause, which is
 * exactly what the throw prevents.
 */
describe('the mobile painter’s unknown-module guard', () => {
  it('throws on a module key it has no row for, rather than falling back', () => {
    const future = { ...tasksModule, key: 'future' } as unknown as RowModuleView;
    expect(() => paintMobileModule(painter(), future))
      .toThrowError('the mobile painter has no future row');
  });

  /* Not vacuous: the same module with a legal key paints. Without this the case
     above would also pass for a painter that threw on everything. */
  it('and paints the very same module once its key is legal again', () => {
    expect(() => paintMobileModule(painter(), tasksModule)).not.toThrow();
  });
});

describe('the mobile painter is a faithful projection of a Cards module', () => {
  it('with a titled row and an untitled kernel-owned one', () => {
    expect(checkProjection(painter(), [cardsModule], mount)).toEqual([]);
  });

  it('with zero rows', () => {
    expect(checkProjection(painter(), [emptyCards], mount)).toEqual([]);
  });
});

describe('what the painted Cards module puts on screen', () => {
  const paint = (module: RowModuleView) =>
    render(<>{paintMobileModule(painter(), module)}</>).container;

  /* The row is not a control: both of its actions were filtered away, so no
     `onSelect` was passed and Astryx generated no invisible button. The whole
     mobile Cards list therefore contains exactly one button — the header's
     Back. */
  it('renders no per-row control', () => {
    const container = paint(cardsModule);
    const buttons = Array.from(container.querySelectorAll('button'));
    expect(buttons.length).toBe(1);
    expect(buttons[0].getAttribute('aria-label')).toBe('Back to Report');
  });

  /* The regression this slice exists to remove: an untitled card's name *is*
     its kind, and the old mobile list printed it a second time in the meta
     lane. */
  it('prints an untitled card’s kind once, not twice', () => {
    const container = paint({ ...cardsModule, rows: [untitled] });
    const occurrences = (container.textContent ?? '').split('harness').length - 1;
    expect(occurrences).toBe(1);
  });

  it('prints a titled card’s name and its kind, and the ownership badge', () => {
    const container = paint(cardsModule);
    expect(container.textContent).toContain('Build log');
    expect(container.textContent).toContain('terminal');
    expect(container.textContent).toContain('kernel-owned');
  });

});

describe('the mobile painter is a faithful projection of a Tasks module', () => {
  it('across ready, dispatched, withdrawn and unreadable rows', () => {
    expect(checkProjection(painter(), [tasksModule], mount)).toEqual([]);
  });

  it('with zero rows', () => {
    expect(checkProjection(painter(), [emptyTasks], mount)).toEqual([]);
  });

  /* The three `PanelRow` boundaries no derived fixture reaches. */
  it('across a bare status, a badge beside a run, and an empty token', () => {
    expect(checkProjection(painter(), [boundaryTasks], mount)).toEqual([]);
  });

  /* The label channel, which no derived fixture reaches: `action-label` is
     checked on both sides, so a painter that dropped `RowAction.label` fails
     here and only here. */
  it('with a reveal that carries a label of its own', () => {
    expect(checkProjection(painter(), [{ ...tasksModule, rows: [namedReveal] }], mount)).toEqual([]);
  });
});

/*
 * The three boundaries, each read off the DOM as well as through the projection.
 *
 * The projection case above would catch all three, but it reports one list of
 * violation codes for the whole module — these say which row is which, so a
 * mutation names the boundary it broke.
 */
describe('what the painted Tasks module does at PanelRow’s boundaries', () => {
  const paint = (module: RowModuleView) =>
    render(<>{paintMobileModule(painter(), module)}</>).container;

  it('still writes the title when the phrase adds nothing to the token', () => {
    const status = paint(boundaryTasks).querySelector('[data-nc-row="block-7"] [data-nc-status]');
    expect(status?.getAttribute('data-nc-status')).toBe('running');
    /* Equal to the token, and still present: the tooltip is the phrase, not the
       part of the phrase the token did not already say. */
    expect(status?.getAttribute('title')).toBe('running');
  });

  it('keeps a declaration badge on a row that also carries a run', () => {
    const row = paint(boundaryTasks).querySelector('[data-nc-row="block-8"]');
    expect(row?.querySelector('[data-nc-badge="declaration"]')?.textContent).toBe('Not ready');
    expect(row?.querySelector('[data-nc-status]')?.getAttribute('data-nc-status')).toBe('failed');
  });

  it('draws a status carrier for an empty token rather than dropping it', () => {
    const row = paint(boundaryTasks).querySelector('[data-nc-row="block-9"]');
    const status = row?.querySelector('[data-nc-status]');
    expect(status, 'an empty token is still a status').not.toBeNull();
    expect(status?.getAttribute('data-nc-status')).toBe('');
    expect(status?.getAttribute('title')).toBe('the kernel has not named this state');
  });
});

/*
 * `RowAction.label`, in both directions.
 *
 * The projection case above is the mechanical half; these two say *where* the
 * name goes — the action host, which on this surface is the row's own `<li>` —
 * and that a null label leaves **no attribute at all** rather than an empty or
 * fabricated one.
 *
 * Neither case carries the pair alone, which is why there are two. A painter
 * that hard-codes `ariaLabel: undefined` — the shape this channel had before
 * the label was consumed at all — fails the first and passes the second; one
 * that names every row unconditionally fails the second and passes the first.
 * They are written against the same host element so that a painter which put
 * the name somewhere else could not satisfy both.
 */
describe('the mobile Task row’s action label', () => {
  const paint = (module: RowModuleView) =>
    render(<>{paintMobileModule(painter(), module)}</>).container;

  it('becomes the action host’s accessible name when the view model offers one', () => {
    const container = paint({ ...tasksModule, rows: [namedReveal] });
    const row = container.querySelector('[data-nc-row="block-6"]');
    expect(row?.getAttribute('data-nc-row-action')).toBe('reveal-block');
    expect(row?.getAttribute('aria-label')).toBe('Reveal zeta-audit');
  });

  it('and leaves no aria-label behind when the view model offers none', () => {
    const container = paint({ ...tasksModule, rows: [ready] });
    const row = container.querySelector('[data-nc-row="block-1"]');
    expect(row?.getAttribute('data-nc-row-action')).toBe('reveal-block');
    expect(row?.hasAttribute('aria-label')).toBe(false);
  });
});

describe('what the painted Tasks module puts on screen', () => {
  const paint = (module: RowModuleView) =>
    render(<>{paintMobileModule(painter(), module)}</>).container;

  /*
   * **`struck` has no carrier in the projection at all** — `checkBadges` reads a
   * badge's id, order and text and nothing else, so a painter that ignored or
   * inverted `struck` is green under every violation code. This is its only
   * carrier on this surface, and it is the same shape the desktop's lives in
   * (`public.test.tsx`'s `taskWithdrawn` assertion): both directions, because a
   * class applied unconditionally would satisfy the positive half alone.
   */
  it('strikes through a withdrawn declaration but not an ordinary one', () => {
    const container = paint(tasksModule);
    const struck = Array.from(container.querySelectorAll('[data-nc-badge="declaration"]'));
    expect(struck.map((element) => element.textContent))
      .toEqual(['Withdrawn', 'Unreadable', 'Not ready']);
    expect(struck[0].className).toContain('mobileRowStruck');
    expect(struck[1].className).not.toContain('mobileRowStruck');
    expect(struck[2].className).not.toContain('mobileRowStruck');
  });

  /* The row root is the action host, and the two markers share it. This is the
     first production shape to take `owned()`'s self-inclusion path (S1b-2), so
     it is asserted rather than left implied by the green run above. */
  it('hosts reveal-block on the row root, beside the row marker', () => {
    const container = paint(tasksModule);
    const rows = Array.from(container.querySelectorAll('[data-nc-row]'));
    expect(rows.length).toBe(tasksModule.rows.length);
    for (const row of rows) {
      expect(row.getAttribute('data-nc-row-action')).toBe('reveal-block');
    }
  });

  /* Two channels, both exact: the hint is the pointer text on the row root, and
     `label` is null so no accessible name may be fabricated over the visible
     one (WCAG 2.5.3). */
  it('puts the action hint on the row root and emits no aria-label', () => {
    const container = paint(tasksModule);
    const row = container.querySelector('[data-nc-row="block-1"]');
    expect(row?.getAttribute('title')).toBe('Show alpha-impl in the report');
    expect(row?.hasAttribute('aria-label')).toBe(false);
  });

  /*
   * D8, from the other side, and **scoped to the two rows it is about**: a
   * `ready` row carries no declaration at all (so the word this surface used to
   * print for it is simply gone), and a dispatched row's readiness word stood
   * down for the run. Asserting it over the whole module would be wrong rather
   * than merely weak: `notReady` legitimately prints its word, because nothing
   * has run.
   */
  it('prints no readiness word for a ready row or a dispatched one', () => {
    const text = paint({ ...tasksModule, rows: [ready, dispatched] }).textContent ?? '';
    expect(text).not.toContain('Ready');
    expect(text).not.toContain('Not ready');
    expect(text).toContain('failed');
  });

  /* And the words the derivation *does* produce all arrive. */
  it('prints every declaration the derivation kept', () => {
    const text = paint(tasksModule).textContent ?? '';
    expect(text).toContain('Withdrawn');
    expect(text).toContain('Unreadable');
    expect(text).toContain('Not ready');
  });

  /* The status carriers the projection reads, spelled out: the attribute holds
     the bare token, so the projection can compare it by exact equality, and
     `title` holds the phrase, which is strictly more — the kernel's reason is
     appended, never substituted. (Colour is a desktop-only affair: the
     stylesheet keys `.taskDot[data-nc-status=…]`, and this surface prints the
     word instead.) */
  it('writes the bare token into the marker and the whole phrase into the title', () => {
    const container = paint(tasksModule);
    const status = container.querySelector('[data-nc-status]');
    expect(status?.getAttribute('data-nc-status')).toBe('failed');
    expect(status?.getAttribute('title'))
      .toBe('failed — track /tmp/alpha is not a git repository');
  });

  /*
   * And the same phrase reaches the **control**, which the two carriers above do
   * not manage on their own: `data-nc-status` and `title` sit on a span in the
   * meta lane, and Astryx lays that lane out as a *sibling* of the invisible
   * button — so a reader on the button hears `beta-gate` and no reason at all.
   * The desktop's reveal button encloses its dot and therefore names the whole
   * phrase; this is that information, delivered as a description so the visible
   * key stays the name.
   */
  it('describes the row’s control with the whole status phrase', () => {
    const container = paint(tasksModule);
    const row = container.querySelector('[data-nc-row="block-2"]');
    const control = row?.querySelector('button');
    expect(control, 'the Task row must generate a control to describe').not.toBeNull();
    expect(control?.textContent).toBe('beta-gate');
    const described = control?.getAttribute('aria-describedby') ?? null;
    expect(described).not.toBeNull();
    /* Looked up by id the way a user agent resolves the reference, rather than
       by selector: `useId` spells ids with characters a CSS selector would have
       to escape. */
    expect(container.ownerDocument.getElementById(described!)?.textContent)
      .toBe('failed — track /tmp/alpha is not a git repository');
  });

  /* A row the kernel said nothing about is described by nothing: an empty
     description node is one a screen reader still walks into. */
  it('and describes a row with no status with nothing at all', () => {
    const container = paint({ ...tasksModule, rows: [ready] });
    expect(container.querySelector('[aria-describedby]')).toBeNull();
  });

  /* The kind is printed; what is not offered is the *action* on it (§3.6). The
     dispatched fixture's `open-card` was filtered away by the capability table,
     so the row carries exactly one action marker. */
  it('prints a worker task’s kind while offering no card action for it', () => {
    const container = paint(tasksModule);
    const row = container.querySelector('[data-nc-row="block-2"]');
    expect(row?.textContent).toContain('terminal');
    expect(row?.querySelectorAll('[data-nc-row-action]').length).toBe(0);
    expect(row?.getAttribute('data-nc-row-action')).toBe('reveal-block');
  });

  it('reveals the block the row names when the row is tapped', () => {
    const onOpenTask = vi.fn();
    const container = render(<>{paintMobileModule(
      makeMobilePainter({ onOpenTask, backLabel: 'Report', onBack: vi.fn() }),
      tasksModule,
    )}</>).container;
    const button = container.querySelector('[data-nc-row="block-3"] button')
      ?? container.querySelector('[data-nc-row="block-3"]');
    (button as HTMLElement).click();
    expect(onOpenTask).toHaveBeenCalledWith('block-3');
  });
});
