// @vitest-environment jsdom
//
// The mobile painter against `checkProjection`'s synthetic mount: this file checks the painter, not the page.

import { render } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { checkProjection } from '../../../../../tools/projection/public.ts';
import type { ProjectionNode } from '../../../../../tools/projection/public.ts';
import { makeMobilePainter, paintMobileModule, type MobileLeaf } from './mobile-painter.tsx';

/** The one mount every case uses: it renders exactly what the painter painted. */
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
  activity: null,
  actions: [
    { kind: 'open-card', cardId: 'card-1', label: null, hint: null, description: null },
    {
      kind: 'delete-card', cardId: 'card-1', label: 'Delete card Build log',
      hint: 'Delete card', description: null,
    },
  ],
};

/** Untitled and kernel-owned: the derived name *is* the kind, so there is no separate `kind` carrier. */
const untitled: PanelRow = {
  id: 'card-2',
  title: 'harness',
  kind: null,
  badges: [{ id: 'kernel-owned', text: 'kernel-owned', struck: false }],
  status: null,
  activity: null,
  actions: [{ kind: 'open-card', cardId: 'card-2', label: null, hint: null, description: null }],
};

const cardsModule: RowModuleView = {
  key: 'cards', title: 'Cards', empty: 'No cards yet.', rows: [titled, untitled],
};
const emptyCards: RowModuleView = { ...cardsModule, rows: [] };

/* Task rows, shaped the way `core/view/track-page.ts` derives them: `declaration` is null once there is a `status`, and `open-card` exists only when `kind !== null && workerCardId !== null`. */

/** Ready and undispatched: no declaration word, no run. */
const ready: PanelRow = {
  id: 'block-1',
  title: 'alpha-impl',
  kind: 'codex',
  badges: [],
  status: null,
  activity: null,
  actions: [{
    kind: 'reveal-block', blockId: 'block-1', label: null, hint: null, description: null,
  }],
};

/** Dispatched, with a reason: a status supersedes the readiness word; the worker card offers an `open-card` this surface refuses. */
const dispatched: PanelRow = {
  id: 'block-2',
  title: 'beta-gate',
  kind: 'terminal',
  badges: [],
  status: { token: 'failed', phrase: 'failed — track /tmp/alpha is not a git repository' },
  activity: null,
  actions: [
    {
      kind: 'reveal-block', blockId: 'block-2', label: null,
      hint: 'Show beta-gate in the report',
      description: 'failed — track /tmp/alpha is not a git repository',
    },
    {
      kind: 'open-card', cardId: 'card-9', label: null,
      hint: 'Open the worker card for beta-gate', description: null,
    },
  ],
};

/** Withdrawn: the struck declaration, and no kind — `deriveReportTasks` nulls `kind` and `workerCardId` together for these rows. */
const withdrawn: PanelRow = {
  id: 'block-3',
  title: 'gamma-planner',
  kind: null,
  badges: [{ id: 'declaration', text: 'Withdrawn', struck: true }],
  status: null,
  activity: null,
  actions: [{
    kind: 'reveal-block', blockId: 'block-3', label: null, hint: null, description: null,
  }],
};

/** Unreadable: an ordinary, unstruck declaration beside the withdrawn one. */
const unreadable: PanelRow = {
  id: 'block-4',
  title: 'delta-doc',
  kind: null,
  badges: [{ id: 'declaration', text: 'Unreadable', struck: false }],
  status: null,
  activity: null,
  actions: [{
    kind: 'reveal-block', blockId: 'block-4', label: null, hint: null, description: null,
  }],
};

/** Declared but not ready, never dispatched: the one row carrying a declaration badge and a kind at once. */
const notReady: PanelRow = {
  id: 'block-5',
  title: 'epsilon-fix',
  kind: 'codex',
  badges: [{ id: 'declaration', text: 'Not ready', struck: false }],
  status: null,
  activity: null,
  actions: [{
    kind: 'reveal-block', blockId: 'block-5', label: null, hint: null, description: null,
  }],
};

/** A `reveal-block` that names itself — synthetic on purpose: the derivation words this action with `label: null` on every Task row today. `hint` is null so the two channels cannot cover for one another. */
const namedReveal: PanelRow = {
  id: 'block-6',
  title: 'zeta-audit',
  kind: null,
  badges: [],
  status: null,
  activity: null,
  actions: [{
    kind: 'reveal-block', blockId: 'block-6', label: 'Reveal zeta-audit', hint: null,
    description: null,
  }],
};

const hintedReveal: PanelRow = {
  ...namedReveal,
  id: 'block-6-hint',
  title: 'zeta-hinted',
  actions: [{
    kind: 'reveal-block', blockId: 'block-6-hint', label: null,
    hint: 'A pointer-only hint',
    description: null,
  }],
};

const tasksModule: RowModuleView = {
  key: 'tasks',
  title: 'Tasks',
  empty: 'No tasks declared yet.',
  rows: [ready, dispatched, withdrawn, unreadable, notReady],
};
const emptyTasks: RowModuleView = { ...tasksModule, rows: [] };

/* Three boundaries of `PanelRow` the derived fixtures cannot reach, written at the painter level. */

/** `phrase === token` — a run the kernel gave no reason for. */
const bareStatus: PanelRow = {
  id: 'block-7',
  title: 'eta-run',
  kind: 'terminal',
  badges: [],
  status: { token: 'running', phrase: 'running' },
  activity: null,
  actions: [{
    kind: 'reveal-block', blockId: 'block-7', label: null,
    hint: 'Show eta-run in the report', description: 'running',
  }],
};

/** A badge and a status at once: inside the `PanelRow` contract even though today's derivation never produces it. */
const declaredAndRunning: PanelRow = {
  id: 'block-8',
  title: 'theta-check',
  kind: null,
  badges: [{ id: 'declaration', text: 'Not ready', struck: false }],
  status: { token: 'failed', phrase: 'failed — the worker exited before it reported' },
  activity: null,
  actions: [{
    kind: 'reveal-block', blockId: 'block-8', label: null,
    hint: 'Show theta-check in the report',
    description: 'failed — the worker exited before it reported',
  }],
};

/** An empty token: `RowStatus` permits it. The `phrase` is kept non-empty so `status-token` and `status-phrase` stay separable. */
const emptyToken: PanelRow = {
  id: 'block-9',
  title: 'iota-probe',
  kind: 'claude',
  badges: [],
  status: { token: '', phrase: 'the kernel has not named this state' },
  activity: null,
  actions: [{
    kind: 'reveal-block', blockId: 'block-9', label: null,
    hint: 'Show iota-probe in the report', description: 'the kernel has not named this state',
  }],
};

/** A pending status plus the server-owned reason on the row action. The status
 * stays visible and described, but must not introduce a second native title. */
const pendingReason: PanelRow = {
  id: 'block-10',
  title: 'kappa-queued',
  kind: 'codex',
  badges: [],
  status: { token: 'pending', phrase: 'pending' },
  activity: null,
  actions: [{
    kind: 'reveal-block', blockId: 'block-10', label: null,
    hint: 'Queued 1/1', description: 'pending — Queued 1/1',
  }],
};

const notAdmittedReason: PanelRow = {
  id: 'block-11',
  title: 'lambda-rejected',
  kind: 'codex',
  badges: [],
  status: null,
  activity: null,
  actions: [{
    kind: 'reveal-block', blockId: 'block-11', label: null,
    hint: 'Not admitted · raise planner ceiling',
    description: 'Not admitted · raise planner ceiling',
  }],
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

/* `const unknown: never = moduleKey` is erased at runtime; the `throw` is the runtime half, and only a forced cast can produce the input it exists for. */
describe('the mobile painter’s unknown-module guard', () => {
  it('throws on a module key it has no row for, rather than falling back', () => {
    const future = { ...tasksModule, key: 'future' } as unknown as RowModuleView;
    expect(() => paintMobileModule(painter(), future))
      .toThrowError('the mobile painter has no future row');
  });

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

  /* Both actions were filtered away, so Astryx generated no invisible button: the only button is the header's Back. */
  it('renders no per-row control', () => {
    const container = paint(cardsModule);
    const buttons = Array.from(container.querySelectorAll('button'));
    expect(buttons.length).toBe(1);
    expect(buttons[0].getAttribute('aria-label')).toBe('Back to Report');
  });

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

  /* The label channel, which no derived fixture reaches. */
  it('with a reveal that carries a label of its own', () => {
    expect(checkProjection(painter(), [{ ...tasksModule, rows: [namedReveal] }], mount)).toEqual([]);
  });
});

/* The three boundaries, each read off the DOM as well: the projection reports one list for the whole module, these say which row is which. */
describe('what the painted Tasks module does at PanelRow’s boundaries', () => {
  const paint = (module: RowModuleView) =>
    render(<>{paintMobileModule(painter(), module)}</>).container;

  it('still writes the title when the phrase adds nothing to the token', () => {
    const status = paint(boundaryTasks).querySelector('[data-nc-row="block-7"] [data-nc-status]');
    expect(status?.getAttribute('data-nc-status')).toBe('running');
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

  it('uses only the row reason title when a pending task has one', () => {
    const container = paint({ ...tasksModule, rows: [pendingReason] });
    const row = container.querySelector('[data-nc-row="block-10"]')!;
    const status = row.querySelector('[data-nc-status="pending"]')!;
    expect(status.textContent).toBe('pending');
    expect(status.hasAttribute('title')).toBe(false);
    expect(Array.from(row.querySelectorAll('[title]'))).toEqual([]);
    expect(row.getAttribute('title')).toBe('Queued 1/1');
    expect(Array.from(container.querySelectorAll('[title]'))).toEqual([row]);
    const control = row.querySelector('button')!;
    const described = control.getAttribute('aria-describedby')!;
    expect(container.ownerDocument.getElementById(described)?.textContent)
      .toBe('pending — Queued 1/1');
  });

  it('describes a statusless not-admitted row without showing the reason in its body', () => {
    const container = paint({ ...tasksModule, rows: [notAdmittedReason] });
    const row = container.querySelector('[data-nc-row="block-11"]')!;
    const control = row.querySelector('button')!;
    const described = control.getAttribute('aria-describedby')!;
    expect(container.ownerDocument.getElementById(described)?.textContent)
      .toBe('Not admitted · raise planner ceiling');
    expect(control.textContent).toBe('lambda-rejected');
    expect(row.querySelector('[data-nc-status]')).toBeNull();
  });
});

/* `RowAction.label` in both directions, on the same host element: a null label leaves no attribute at all. */
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

  /* `struck` has no carrier in the projection (`checkBadges` reads id, order and text), so this is its only carrier on this surface. */
  it('strikes through a withdrawn declaration but not an ordinary one', () => {
    const container = paint(tasksModule);
    const struck = Array.from(container.querySelectorAll('[data-nc-badge="declaration"]'));
    expect(struck.map((element) => element.textContent))
      .toEqual(['Withdrawn', 'Unreadable', 'Not ready']);
    expect(struck[0].className).toContain('mobileRowStruck');
    expect(struck[1].className).not.toContain('mobileRowStruck');
    expect(struck[2].className).not.toContain('mobileRowStruck');
  });

  it('hosts reveal-block on the row root, beside the row marker', () => {
    const container = paint(tasksModule);
    const rows = Array.from(container.querySelectorAll('[data-nc-row]'));
    expect(rows.length).toBe(tasksModule.rows.length);
    for (const row of rows) {
      expect(row.getAttribute('data-nc-row-action')).toBe('reveal-block');
    }
  });

  /* `label` is null, so no accessible name may be fabricated over the visible one (WCAG 2.5.3). */
  it('puts the action hint on the row root and emits no aria-label', () => {
    const container = paint({ ...tasksModule, rows: [hintedReveal] });
    const row = container.querySelector('[data-nc-row="block-6-hint"]');
    expect(row?.getAttribute('title')).toBe('A pointer-only hint');
    expect(row?.hasAttribute('aria-label')).toBe(false);
  });

  /* Scoped to the two rows it is about: `notReady` legitimately prints its word, because nothing has run. */
  it('prints no readiness word for a ready row or a dispatched one', () => {
    const text = paint({ ...tasksModule, rows: [ready, dispatched] }).textContent ?? '';
    expect(text).not.toContain('Ready');
    expect(text).not.toContain('Not ready');
    expect(text).toContain('failed');
  });

  it('prints every declaration the derivation kept', () => {
    const text = paint(tasksModule).textContent ?? '';
    expect(text).toContain('Withdrawn');
    expect(text).toContain('Unreadable');
    expect(text).toContain('Not ready');
  });

  it('writes the bare token into the marker and the whole phrase into the title', () => {
    const container = paint(tasksModule);
    const status = container.querySelector('[data-nc-status]');
    expect(status?.getAttribute('data-nc-status')).toBe('failed');
    expect(status?.getAttribute('title'))
      .toBe('failed — track /tmp/alpha is not a git repository');
  });

  /* Astryx lays the meta lane out as a sibling of the invisible button, so the phrase reaches the control as a description; the visible key stays the name. */
  it('describes the row’s control with the whole status phrase', () => {
    const container = paint(tasksModule);
    const row = container.querySelector('[data-nc-row="block-2"]');
    const control = row?.querySelector('button');
    expect(control, 'the Task row must generate a control to describe').not.toBeNull();
    expect(control?.textContent).toBe('beta-gate');
    const described = control?.getAttribute('aria-describedby') ?? null;
    expect(described).not.toBeNull();
    /* Looked up by id, not selector: `useId` spells ids with characters a CSS selector would have to escape. */
    expect(container.ownerDocument.getElementById(described!)?.textContent)
      .toBe('failed — track /tmp/alpha is not a git repository');
  });

  /* A row the kernel said nothing about is described by nothing: an empty
     description node is one a screen reader still walks into. */
  it('and describes a row with no status with nothing at all', () => {
    const container = paint({ ...tasksModule, rows: [ready] });
    expect(container.querySelector('[aria-describedby]')).toBeNull();
  });

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
