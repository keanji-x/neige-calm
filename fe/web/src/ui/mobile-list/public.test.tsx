// @vitest-environment jsdom
//
// #1234 S1b-4a — the five projection marker channels this primitive opened, the
// tooltip channel beside them, and `onSelect` becoming optional.
//
// Every channel gets two assertions and needs both: that the marker lands on the
// **right element** (a `data-nc-row` on the title span instead of the `<li>`
// would satisfy an "the attribute is somewhere" check and break the
// projection's scoping), and that **omitting the prop leaves no attribute at
// all**. The second is not symmetry for its own sake: `app/shell`'s cove and
// page lists, and this page's Outline and Conversations drill-downs, render
// through these same primitives and are not row modules — they must stay
// unmarked, or a faithful painter's tree holds module and row markers a view
// model never named.
//
// **The `onSelect` pair is the load-bearing one.** Astryx's `Item` computes
// `isInteractive = onClick != null` and, when interactive, wraps the label in an
// invisible `<button>`. So `onClick={() => onSelect?.()}` — the obvious way to
// make the prop optional — keeps every row a button, and a mobile Cards row
// would still be a control that does nothing. The presence and absence of that
// generated `<button>` is therefore the mechanical observation of whether
// `onClick` reached Astryx at all.

import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import styles from './mobile-list.module.css';
import { MobileList, MobileListEmpty, MobileListItem, MobileListPage } from './public.tsx';

afterEach(cleanup);

const row = (props: Partial<Parameters<typeof MobileListItem>[0]> = {}) => (
  <MobileList><MobileListItem title="Build log" {...props} /></MobileList>
);

describe('MobileListItem interactivity', () => {
  it('is a control that fires onSelect when one is supplied', async () => {
    const onSelect = vi.fn();
    render(row({ onSelect }));
    const button = screen.getByRole('button');
    await userEvent.click(button);
    expect(onSelect).toHaveBeenCalledOnce();
  });

  it('renders no control at all when onSelect is omitted', () => {
    const { container } = render(row());
    /* Astryx generates the invisible button from `onClick` alone, so its
       absence is the proof that no `onClick` was passed — the exact defect a
       `() => onSelect?.()` wrapper would hide. */
    expect(screen.queryByRole('button')).toBeNull();
    expect(container.querySelector('button')).toBeNull();
    expect(container.querySelector('a')).toBeNull();
  });

  it('marks a non-interactive row so the stylesheet can withhold hover', () => {
    const { container } = render(row());
    const item = container.querySelector('li');
    expect(item?.className.split(' ')).toContain(styles.itemStatic);
  });

  it('does not mark an interactive row static', () => {
    const { container } = render(row({ onSelect: vi.fn() }));
    const item = container.querySelector('li');
    expect(item?.className.split(' ')).not.toContain(styles.itemStatic);
  });

  /* `nested` survived the className rewrite that the static class arrived in —
     it is what indents an Outline child row. */
  it('still nests a second-level row', () => {
    const { container } = render(row({ nested: true, onSelect: vi.fn() }));
    expect(container.querySelector('li')?.className.split(' ')).toContain(styles.itemNested);
  });
});

describe('MobileListItem hint', () => {
  it('puts the pointer tooltip on the root li', () => {
    const { container } = render(row({ hint: 'Show alpha-gate in the report' }));
    expect(container.querySelector('li')?.getAttribute('title')).toBe('Show alpha-gate in the report');
  });

  it('emits no title attribute when the prop is omitted', () => {
    const { container } = render(row());
    expect(container.querySelector('li')?.hasAttribute('title')).toBe(false);
  });

  /* The visible name and the tooltip are two channels: a row's `title` prop is
     the words on screen, and must not become a tooltip by itself. */
  it('is not the visible title prop', () => {
    const { container } = render(row({ title: 'Build log' }));
    expect(container.textContent).toContain('Build log');
    expect(container.querySelector('li')?.hasAttribute('title')).toBe(false);
  });
});

/*
 * #1234 S1b-4a/4b did not change this rule — and that is exactly why it is
 * pinned here. `MobileListItem` has seven call sites: `app/shell`'s Pages list,
 * its Coves list and a cove's Waves list, the wave page's Outline rows (parent
 * and child), and the Cards and Tasks rows the painter now composes. Five of the
 * seven pass a string meta, so the composed name is live behaviour, not a
 * corner. Those two slices reworked the primitive around them — `onSelect`
 * became optional, `hint` and three marker channels arrived — and "the
 * accessible name still composes the way it always did" was, until this block,
 * held up by reading the diff. On a shared primitive that is not enough, so the
 * composition is asserted in all four of its cases.
 *
 * The two painted rows are the other case: their meta lane is an element, not a
 * string, so `metaLabel` is null and the row carries no `aria-label` at all —
 * the row's name is the visible text, which is what the projection compares. On
 * a Task row that is load-bearing rather than incidental: the row is the
 * `reveal-block` host, its action's `label` is null, and an `aria-label`
 * composed from the meta lane would both override the visible text (WCAG 2.5.3)
 * and go red as `action-label`.
 */
describe('MobileListItem accessible name', () => {
  const label = (container: HTMLElement) => container.querySelector('li')?.getAttribute('aria-label');

  it('prefers an explicit ariaLabel over the composed name', () => {
    const { container } = render(row({ ariaLabel: 'Open the build log', meta: 'terminal' }));
    expect(label(container)).toBe('Open the build log');
  });

  it('composes title and a string meta', () => {
    const { container } = render(row({ meta: 'terminal' }));
    expect(label(container)).toBe('Build log, terminal');
  });

  it('composes title and a numeric meta', () => {
    const { container } = render(row({ meta: 3 }));
    expect(label(container)).toBe('Build log, 3');
  });

  /* A ReactNode meta has no string form to append, so nothing is composed —
     rather than an `aria-label` of `Build log, [object Object]`. */
  it('emits no aria-label when meta is a node rather than a string or number', () => {
    const { container } = render(row({ meta: <span>terminal</span> }));
    expect(container.querySelector('li')?.hasAttribute('aria-label')).toBe(false);
    expect(container.querySelector('[aria-label]')).toBeNull();
    /* The meta is still on screen — the row is named by what it shows. */
    expect(container.textContent).toContain('terminal');
  });

  it('emits no aria-label when neither ariaLabel nor meta is given', () => {
    const { container } = render(row());
    expect(container.querySelector('li')?.hasAttribute('aria-label')).toBe(false);
    expect(container.querySelector('[aria-label]')).toBeNull();
  });

  /* The carrier is the `<li>` in both shapes: making a row interactive does not
     move the composed name onto the generated control, which keeps only the
     visible label. Pinned so a future `onSelect` change cannot relocate it
     silently. */
  it('keeps the composed name on the li when the row is interactive', () => {
    const { container } = render(row({ meta: 'terminal', onSelect: vi.fn() }));
    expect(label(container)).toBe('Build log, terminal');
    expect(container.querySelector('button')?.hasAttribute('aria-label')).toBe(false);
  });
});

describe('MobileListItem markers', () => {
  it('puts the row marker on the root li', () => {
    const { container } = render(row({ rowMarker: 'card-1' }));
    const item = container.querySelector('li');
    expect(item?.getAttribute('data-nc-row')).toBe('card-1');
    expect(container.querySelectorAll('[data-nc-row]').length).toBe(1);
  });

  it('emits no row attribute when the prop is omitted', () => {
    const { container } = render(row());
    expect(container.querySelector('[data-nc-row]')).toBeNull();
    expect(container.querySelector('li')?.hasAttribute('data-nc-row')).toBe(false);
  });

  it('puts the title field marker on the visible title span, not on the li', () => {
    const { container } = render(row({ titleFieldMarker: 'title' }));
    const carrier = container.querySelector('[data-nc-field]');
    /* The carrier owes an exact string, so it must be the element whose whole
       text is the name — and it must not be the `<li>`, which already carries
       the row marker and may hold only one content marker. */
    expect(carrier?.textContent).toBe('Build log');
    expect(carrier?.tagName).toBe('SPAN');
    expect(container.querySelector('li')?.hasAttribute('data-nc-field')).toBe(false);
    expect(container.querySelectorAll('[data-nc-field]').length).toBe(1);
  });

  it('emits no field attribute when the prop is omitted', () => {
    const { container } = render(row());
    expect(container.querySelector('[data-nc-field]')).toBeNull();
  });

  /* #1234 S1b-4b — the row-action channel. It **shares the `<li>` with the row
     marker on purpose**: on this surface the whole row is the tappable control,
     and `data-nc-row-action` is a host annotation rather than a content marker,
     so the one-content-marker-per-element rule is not engaged. */
  it('puts the row-action marker on the root li, beside the row marker', () => {
    const { container } = render(row({ rowMarker: 'block-1', rowActionMarker: 'reveal-block' }));
    const item = container.querySelector('li');
    expect(item?.getAttribute('data-nc-row')).toBe('block-1');
    expect(item?.getAttribute('data-nc-row-action')).toBe('reveal-block');
    expect(container.querySelectorAll('[data-nc-row-action]').length).toBe(1);
  });

  it('emits no row-action attribute when the prop is omitted', () => {
    const { container } = render(row({ rowMarker: 'card-1' }));
    expect(container.querySelector('[data-nc-row-action]')).toBeNull();
    expect(container.querySelector('li')?.hasAttribute('data-nc-row-action')).toBe(false);
  });

  it('the two channels are independent', () => {
    const { container } = render(row({ rowMarker: 'card-1' }));
    expect(container.querySelector('li')?.getAttribute('data-nc-row')).toBe('card-1');
    expect(container.querySelector('[data-nc-field]')).toBeNull();
  });

  /* The title carrier stays a leaf even when the row shows meta beside it: the
     meta lane is a sibling of the label, so nothing the painter puts there can
     land inside the string the projection compares. */
  it('keeps the title carrier free of the meta lane', () => {
    const { container } = render(row({
      titleFieldMarker: 'title',
      meta: <span data-nc-field="kind">terminal</span>,
    }));
    const carrier = container.querySelector('[data-nc-field="title"]');
    expect(carrier?.textContent).toBe('Build log');
    expect(carrier?.querySelector('[data-nc-field]')).toBeNull();
  });
});

describe('MobileListPage markers', () => {
  it('puts the module marker on the page container', () => {
    const { container } = render(
      <MobileListPage title="Cards" moduleMarker="cards">rows</MobileListPage>,
    );
    const page = container.firstElementChild;
    expect(page?.getAttribute('data-nc-module')).toBe('cards');
    expect(container.querySelectorAll('[data-nc-module]').length).toBe(1);
  });

  it('emits no module attribute when the prop is omitted', () => {
    const { container } = render(<MobileListPage title="Outline">rows</MobileListPage>);
    expect(container.querySelector('[data-nc-module]')).toBeNull();
    expect(container.firstElementChild?.hasAttribute('data-nc-module')).toBe(false);
  });

  it('puts the title field marker on the heading that carries the title', () => {
    const { container } = render(
      <MobileListPage title="Cards" titleFieldMarker="module-title">rows</MobileListPage>,
    );
    const heading = container.querySelector('h2');
    expect(heading?.getAttribute('data-nc-field')).toBe('module-title');
    expect(heading?.textContent).toBe('Cards');
    expect(container.querySelectorAll('[data-nc-field]').length).toBe(1);
  });

  it('emits no field attribute when the prop is omitted', () => {
    const { container } = render(<MobileListPage title="Cards">rows</MobileListPage>);
    expect(container.querySelector('[data-nc-field]')).toBeNull();
    expect(container.querySelector('h2')?.hasAttribute('data-nc-field')).toBe(false);
  });

  it('the two channels are independent', () => {
    const { container } = render(
      <MobileListPage title="Cards" moduleMarker="cards">rows</MobileListPage>,
    );
    expect(container.firstElementChild?.getAttribute('data-nc-module')).toBe('cards');
    expect(container.querySelector('h2')?.hasAttribute('data-nc-field')).toBe(false);
  });
});

describe('MobileListEmpty field marker', () => {
  it('puts the value on the element that carries the sentence', () => {
    const { container } = render(<MobileListEmpty fieldMarker="empty">No cards yet.</MobileListEmpty>);
    const paragraph = container.querySelector('p');
    expect(paragraph?.getAttribute('data-nc-field')).toBe('empty');
    expect(paragraph?.textContent).toBe('No cards yet.');
    expect(container.querySelectorAll('[data-nc-field]').length).toBe(1);
  });

  it('emits no attribute when the prop is omitted', () => {
    const { container } = render(<MobileListEmpty>No cards yet.</MobileListEmpty>);
    expect(container.querySelector('[data-nc-field]')).toBeNull();
    expect(container.querySelector('p')?.hasAttribute('data-nc-field')).toBe(false);
  });
});
