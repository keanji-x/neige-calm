// @vitest-environment jsdom
//
// #1234 S1b-4a / S1b-4b — the projection marker channels this primitive opened,
// the two wording channels beside them (the pointer tooltip, and the row's
// accessible description since S1b-4b), and `onSelect` becoming optional.
//
// Every channel gets two assertions and needs both: that the marker lands on the
// **right element** (a `data-nc-row` on the title span instead of the `<li>`
// would satisfy an "the attribute is somewhere" check and break the
// projection's scoping), and that **omitting the prop leaves no attribute at
// all**. The second is not symmetry for its own sake: `app/shell`'s area and
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
import { useLayoutEffect } from 'react';
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
 * its Areas list and an area's Tracks list, the track page's Outline rows (parent
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

/*
 * #1234 S1b-4b — the accessible-description channel.
 *
 * **The element it lands on is the whole point.** Astryx's `Item` spreads rest
 * props onto the root `<li>` and gives the invisible `<button>` nothing from
 * outside, so `aria-describedby={id}` written as an ordinary prop would sit on
 * the container and never reach the control a reader focuses. These cases
 * therefore read the attribute off the **button**, and assert it is *not* on the
 * `<li>`: an implementation that took the easy route passes an "it is somewhere"
 * check and delivers nothing.
 */
describe('MobileListItem accessible description', () => {
  /** The description a reader would actually get: follow the reference from the
   *  focused control to the node it names. */
  const describedText = (host: Element | null | undefined): string | null => {
    const id = host?.getAttribute('aria-describedby') ?? null;
    if (id === null) return null;
    return host!.ownerDocument.getElementById(id)?.textContent ?? null;
  };

  it('describes the generated control, not the li', () => {
    const { container } = render(row({
      onSelect: vi.fn(),
      meta: <span>failed</span>,
      accessibleDescription: 'failed — not a git repository',
    }));
    const button = container.querySelector('button');
    expect(describedText(button)).toBe('failed — not a git repository');
    expect(container.querySelector('li')?.hasAttribute('aria-describedby')).toBe(false);
  });

  /* The name is untouched — the description is *on top of* it, which is the
     whole reason this is not an `aria-label`. */
  it('leaves the row’s visible name as the accessible name', () => {
    const { container } = render(row({
      onSelect: vi.fn(),
      accessibleDescription: 'failed — not a git repository',
    }));
    expect(container.querySelector('button')?.hasAttribute('aria-label')).toBe(false);
    expect(container.querySelector('button')?.textContent).toBe('Build log');
  });

  it('emits neither the attribute nor a carrier when the prop is omitted', () => {
    const { container } = render(row({ onSelect: vi.fn(), meta: <span>failed</span> }));
    expect(container.querySelector('[aria-describedby]')).toBeNull();
    expect(container.querySelector('button')?.hasAttribute('aria-describedby')).toBe(false);
    /* And no carrier was left behind either — an empty description node is one
       a screen reader still walks into. The carrier is the only node in this
       row that has an `id`, so its absence is observable without naming a
       class. */
    expect(container.querySelector('li [id]')).toBeNull();
  });

  /* A row with no `onSelect` generates no control at all, so the container is
     the only host there is. Asserted rather than left to chance: silently
     dropping the description on a non-interactive row would be a hole the
     positive case above cannot see. */
  it('falls back to the li when the row generates no control', () => {
    const { container } = render(row({ accessibleDescription: 'failed — not a git repository' }));
    expect(container.querySelector('button')).toBeNull();
    expect(describedText(container.querySelector('li'))).toBe('failed — not a git repository');
  });

  /*
   * #1234 S1b-4b — **the two cases below are the ones "Astryx changing shape
   * turns this red" did not actually cover.**
   *
   * The cases above read the description off the first direct-child control, and
   * so did the implementation. That pair catches a control moving *deeper* — the
   * selector finds nothing, the description lands on the `<li>`, and the
   * assertion that it is on the button fails. It does not catch a control being
   * *joined*: with `<li><button>row</button><button>action</button></li>` the
   * first match is still described, every assertion above still passes, and the
   * second focusable control silently has no description. Multiplicity, not
   * nesting, is the blind spot.
   *
   * The counterexample is built by putting the extra control into the rendered
   * `<li>` directly — the markup Astryx would produce, without waiting for an
   * Astryx that produces it — and then changing the description so the effect
   * re-runs against it. What must happen is a throw, not a best guess: a
   * described first button looks identical, from the DOM, to a correct row.
   */
  const withExtraControl = (
    props: Partial<Parameters<typeof MobileListItem>[0]>,
  ): (() => void) => {
    const { container, rerender } = render(row({ ...props, accessibleDescription: 'first' }));
    container.querySelector('li')!.append(document.createElement('button'));
    /* A different description, so the effect's deps change and it re-runs over
       the markup as it now stands. */
    return () => { rerender(row({ ...props, accessibleDescription: 'second' })); };
  };

  it('refuses to choose when an interactive row holds two direct-child controls', () => {
    const rerenderWithTwo = withExtraControl({ onSelect: vi.fn() });
    expect(rerenderWithTwo).toThrow(/an interactive row expects 1 control .* rendered 2/s);
  });

  /* The inert row's own direction of the same check. It is not symmetry for its
     own sake: a row with no `onSelect` that somehow has a focusable control is a
     row whose description sits on the `<li>` while the thing a reader focuses
     has none — the same hole, reached from the other side. */
  it('refuses to fall back to the li when an inert row holds a control', () => {
    const rerenderWithOne = withExtraControl({});
    expect(rerenderWithOne).toThrow(/a non-interactive row expects 0 control .* rendered 1/s);
  });

  /*
   * #1234 S1b-4b review — **and in production it degrades instead.**
   *
   * The two cases above run with `import.meta.env.DEV` true, which is the tier
   * the throw is for: an Astryx upgrade that changed the markup is looked at in
   * development and in CI. Production is a different trade. This component
   * renders inside the `/track/$trackId` route; neither the route nor the router
   * configures an `errorComponent`, so a throw out of a layout effect is caught
   * by the global `CatchBoundary` above `Matches` and replaces the entire match
   * — app shell and navigation with it — with a bare error page. Losing the
   * whole surface is a worse outcome than an under-described row, so the
   * production build logs and falls back to the selection this guard replaced.
   *
   * **What is asserted is the fallback, not merely the absence of a throw.** A
   * production branch that swallowed the mismatch *and* dropped the description
   * would satisfy "it did not throw" and deliver nothing; so the description is
   * followed from the host that is left, and it must be the old choice — the
   * first direct-child control, the `<li>` when there is none.
   */
  describe('outside development', () => {
    const describedTextOn = (host: Element | null | undefined): string | null => {
      const id = host?.getAttribute('aria-describedby') ?? null;
      if (id === null) return null;
      return host!.ownerDocument.getElementById(id)?.textContent ?? null;
    };

    afterEach(() => { vi.unstubAllEnvs(); vi.restoreAllMocks(); });

    it('logs and describes the first control when an interactive row holds two', () => {
      vi.stubEnv('DEV', false);
      const logged = vi.spyOn(console, 'error').mockImplementation(() => {});
      const { container, rerender } = render(
        row({ onSelect: vi.fn(), accessibleDescription: 'first' }),
      );
      const first = container.querySelector('button')!;
      container.querySelector('li')!.append(document.createElement('button'));
      rerender(row({ onSelect: vi.fn(), accessibleDescription: 'second' }));

      expect(logged).toHaveBeenCalledTimes(1);
      expect(String(logged.mock.calls[0][0]))
        .toMatch(/an interactive row expects 1 control .* rendered 2/s);
      /* The old selection, still doing its job: the row a reader focuses is
         described, and the appended control — which is not a row of Astryx's —
         is not. */
      expect(describedTextOn(first)).toBe('second');
      expect(container.querySelectorAll('[aria-describedby]').length).toBe(1);
    });

    it('logs and describes the unexpected first control when an inert row holds one', () => {
      vi.stubEnv('DEV', false);
      const logged = vi.spyOn(console, 'error').mockImplementation(() => {});
      const { container, rerender } = render(row({ accessibleDescription: 'first' }));
      const injected = document.createElement('button');
      container.querySelector('li')!.append(injected);
      rerender(row({ accessibleDescription: 'second' }));

      expect(logged).toHaveBeenCalledTimes(1);
      expect(String(logged.mock.calls[0][0]))
        .toMatch(/a non-interactive row expects 0 control .* rendered 1/s);
      /* `controls[0] ?? root` — the injected control is the only one there is,
         so the fallback lands on it rather than dropping the description. */
      expect(describedTextOn(injected)).toBe('second');
      expect(container.querySelectorAll('[aria-describedby]').length).toBe(1);
    });
  });
});

/*
 * #1234 S1b-4b — **the description and its carrier have to arrive and leave in
 * the same commit.**
 *
 * The carrier `<span id>` is declarative, so React puts it in the DOM during the
 * commit; the IDREF that points at it is written from an effect. If that effect
 * is *passive*, the two halves are apart for a window: a row that just gained a
 * description paints with the span present and no `aria-describedby`, and a row
 * that just lost one paints with the attribute still naming a node React has
 * already removed — a dangling IDREF, which a reader resolves to nothing.
 * Concurrent rendering can stretch that window arbitrarily.
 *
 * **Why the cases above cannot see it.** `render()` and `rerender()` run inside
 * `act`, which flushes passive effects before returning, so every assertion made
 * after them sees the converged state whichever effect tier wrote it. The window
 * is real and the whole existing block is blind to it.
 *
 * So these cases observe from *inside the commit*: `CommitProbe` is a later
 * sibling of the row, and React runs layout effects in tree order, so its
 * `useLayoutEffect` fires in the same commit, right after the row's own. What it
 * records is therefore a precisely located snapshot: the DOM during the
 * layout-effect phase, after the row's own layout effect and before any passive
 * effect of that commit has run. It is not "the first painted frame" — jsdom
 * never paints, and even in a browser React may run passive effects of an
 * interactive update before the paint that follows them.
 */
describe('MobileListItem accessible description, mid-commit', () => {
  type Snapshot = Readonly<{
    hostTag: string | null;
    describedText: string | null;
    hostCount: number;
    carrierCount: number;
  }>;

  /** The DOM as it stands at this instant: who is described, by what, and how
   *  many of each exist. */
  const snapshot = (): Snapshot => {
    const host = document.body.querySelector('[aria-describedby]');
    const id = host?.getAttribute('aria-describedby') ?? null;
    return {
      hostTag: host === null ? null : host.tagName.toLowerCase(),
      describedText: id === null ? null : document.getElementById(id)?.textContent ?? null,
      hostCount: document.body.querySelectorAll('[aria-describedby]').length,
      /* The carrier is the only node inside a row that has an `id`. */
      carrierCount: document.body.querySelectorAll('li [id]').length,
    };
  };

  /* No dependency array on purpose: every commit is recorded, so the assertions
     can read the last one rather than guessing which commit mattered. */
  function CommitProbe({ record }: Readonly<{ record: (seen: Snapshot) => void }>) {
    useLayoutEffect(() => { record(snapshot()); });
    return null;
  }

  const probed = (
    props: Partial<Parameters<typeof MobileListItem>[0]>,
    record: (seen: Snapshot) => void,
  ) => (
    <MobileList>
      <MobileListItem title="Build log" {...props} />
      <CommitProbe record={record} />
    </MobileList>
  );

  const onSelect = vi.fn();
  const phrase = 'failed — not a git repository';

  it('attaches the reference in the commit that adds the carrier', () => {
    const seen: Snapshot[] = [];
    const { rerender } = render(probed({ onSelect }, seen.push.bind(seen)));
    expect(seen.at(-1)?.hostTag).toBeNull();
    rerender(probed({ onSelect, accessibleDescription: phrase }, seen.push.bind(seen)));
    /* Not "eventually": at this point in the commit the span already exists, so
       a control without the attribute is a row a reader would have read
       undescribed. */
    expect(seen.at(-1)?.hostTag).toBe('button');
    expect(seen.at(-1)?.describedText).toBe(phrase);
  });

  it('removes the reference in the commit that removes the carrier', () => {
    const seen: Snapshot[] = [];
    const { rerender } = render(probed({ onSelect, accessibleDescription: phrase }, seen.push.bind(seen)));
    expect(seen.at(-1)?.describedText).toBe(phrase);
    rerender(probed({ onSelect }, seen.push.bind(seen)));
    /* A dangling IDREF is worse than no description: the attribute is there and
       resolves to nothing. */
    expect(seen.at(-1)?.hostTag).toBeNull();
    expect(seen.at(-1)?.hostCount).toBe(0);
    expect(seen.at(-1)?.carrierCount).toBe(0);
  });

  /* Making a described row non-interactive destroys the control the reference
     was on and moves the host to the `<li>`. Both halves are asserted: the new
     host is described, and there is exactly one described element — a stale
     attribute left on a detached button would be invisible to the first
     assertion alone. */
  it('moves the reference to the li when the row stops being interactive', () => {
    const seen: Snapshot[] = [];
    const { container, rerender } = render(
      probed({ onSelect, accessibleDescription: phrase }, seen.push.bind(seen)),
    );
    expect(seen.at(-1)?.hostTag).toBe('button');
    rerender(probed({ accessibleDescription: phrase }, seen.push.bind(seen)));
    expect(container.querySelector('button')).toBeNull();
    expect(seen.at(-1)?.hostTag).toBe('li');
    expect(seen.at(-1)?.describedText).toBe(phrase);
    expect(seen.at(-1)?.hostCount).toBe(1);
  });

  it('moves the reference to the control when the row becomes interactive', () => {
    const seen: Snapshot[] = [];
    const { rerender } = render(probed({ accessibleDescription: phrase }, seen.push.bind(seen)));
    expect(seen.at(-1)?.hostTag).toBe('li');
    rerender(probed({ onSelect, accessibleDescription: phrase }, seen.push.bind(seen)));
    expect(seen.at(-1)?.hostTag).toBe('button');
    expect(seen.at(-1)?.describedText).toBe(phrase);
    /* The `<li>` is still in the tree, so a cleanup that forgot it would leave
       two described elements rather than a detached one. */
    expect(seen.at(-1)?.hostCount).toBe(1);
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
