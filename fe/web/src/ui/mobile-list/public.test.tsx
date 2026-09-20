// @vitest-environment jsdom
// Astryx's `Item` wraps the label in an invisible `<button>` only when `onClick != null`, so that button's presence is the observation of whether `onClick` reached Astryx.

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
    /* Astryx generates the button from `onClick` alone; a `() => onSelect?.()` wrapper would keep every row a button. */
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

  /* A row's `title` prop is the words on screen and must not become a tooltip by itself. */
  it('is not the visible title prop', () => {
    const { container } = render(row({ title: 'Build log' }));
    expect(container.textContent).toContain('Build log');
    expect(container.querySelector('li')?.hasAttribute('title')).toBe(false);
  });
});

/* Painted rows pass an element meta, so `metaLabel` is null and the row carries no `aria-label`; a composed one would override the visible text (WCAG 2.5.3). */
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

  it('emits no aria-label when meta is a node rather than a string or number', () => {
    const { container } = render(row({ meta: <span>terminal</span> }));
    expect(container.querySelector('li')?.hasAttribute('aria-label')).toBe(false);
    expect(container.querySelector('[aria-label]')).toBeNull();
    expect(container.textContent).toContain('terminal');
  });

  it('emits no aria-label when neither ariaLabel nor meta is given', () => {
    const { container } = render(row());
    expect(container.querySelector('li')?.hasAttribute('aria-label')).toBe(false);
    expect(container.querySelector('[aria-label]')).toBeNull();
  });

  /* The carrier is the `<li>` in both shapes; the generated control keeps only the visible label. */
  it('keeps the composed name on the li when the row is interactive', () => {
    const { container } = render(row({ meta: 'terminal', onSelect: vi.fn() }));
    expect(label(container)).toBe('Build log, terminal');
    expect(container.querySelector('button')?.hasAttribute('aria-label')).toBe(false);
  });
});

/* Astryx spreads rest props onto the `<li>`, so an `aria-describedby` prop would never reach the control a reader focuses; read it off the button and assert it is not on the `<li>`. */
describe('MobileListItem accessible description', () => {
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
    /* An empty description node is one a screen reader still walks into; the carrier is the only node in the row with an `id`. */
    expect(container.querySelector('li [id]')).toBeNull();
  });

  /* A row with no `onSelect` generates no control, so the container is the only host. */
  it('falls back to the li when the row generates no control', () => {
    const { container } = render(row({ accessibleDescription: 'failed — not a git repository' }));
    expect(container.querySelector('button')).toBeNull();
    expect(describedText(container.querySelector('li'))).toBe('failed — not a git repository');
  });

  /* Multiplicity, not nesting, is the blind spot of the cases above: a second direct-child control would silently go undescribed. The extra control is appended to the rendered `<li>` and the description changed so the effect re-runs. */
  const withExtraControl = (
    props: Partial<Parameters<typeof MobileListItem>[0]>,
  ): (() => void) => {
    const { container, rerender } = render(row({ ...props, accessibleDescription: 'first' }));
    container.querySelector('li')!.append(document.createElement('button'));
    return () => { rerender(row({ ...props, accessibleDescription: 'second' })); };
  };

  it('refuses to choose when an interactive row holds two direct-child controls', () => {
    const rerenderWithTwo = withExtraControl({ onSelect: vi.fn() });
    expect(rerenderWithTwo).toThrow(/an interactive row expects 1 control .* rendered 2/s);
  });

  /* The inert row's direction of the same check: a focusable control with the description on the `<li>`. */
  it('refuses to fall back to the li when an inert row holds a control', () => {
    const rerenderWithOne = withExtraControl({});
    expect(rerenderWithOne).toThrow(/a non-interactive row expects 0 control .* rendered 1/s);
  });

  /* In production a throw out of a layout effect reaches the global `CatchBoundary` and replaces the whole match, so it logs and falls back instead. The fallback itself is asserted, not merely the absence of a throw. */
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
      expect(describedTextOn(injected)).toBe('second');
      expect(container.querySelectorAll('[aria-describedby]').length).toBe(1);
    });
  });
});

/* The carrier `<span id>` is committed declaratively and the IDREF written from an effect; a passive effect leaves the two apart for a window that `act` hides. `CommitProbe` is a later sibling, so its layout effect fires in the same commit right after the row's. */
describe('MobileListItem accessible description, mid-commit', () => {
  type Snapshot = Readonly<{
    hostTag: string | null;
    describedText: string | null;
    hostCount: number;
    carrierCount: number;
  }>;

  const snapshot = (): Snapshot => {
    const host = document.body.querySelector('[aria-describedby]');
    const id = host?.getAttribute('aria-describedby') ?? null;
    return {
      hostTag: host === null ? null : host.tagName.toLowerCase(),
      describedText: id === null ? null : document.getElementById(id)?.textContent ?? null,
      hostCount: document.body.querySelectorAll('[aria-describedby]').length,
      carrierCount: document.body.querySelectorAll('li [id]').length,
    };
  };

  /* No dependency array on purpose: every commit is recorded. */
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
    expect(seen.at(-1)?.hostTag).toBe('button');
    expect(seen.at(-1)?.describedText).toBe(phrase);
  });

  it('removes the reference in the commit that removes the carrier', () => {
    const seen: Snapshot[] = [];
    const { rerender } = render(probed({ onSelect, accessibleDescription: phrase }, seen.push.bind(seen)));
    expect(seen.at(-1)?.describedText).toBe(phrase);
    rerender(probed({ onSelect }, seen.push.bind(seen)));
    expect(seen.at(-1)?.hostTag).toBeNull();
    expect(seen.at(-1)?.hostCount).toBe(0);
    expect(seen.at(-1)?.carrierCount).toBe(0);
  });

  /* Both halves: the new host is described, and exactly one element is — a stale attribute on a detached button would be invisible to the first alone. */
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
    /* The carrier owes an exact string, and the `<li>` already carries the row marker and may hold only one content marker. */
    expect(carrier?.textContent).toBe('Build log');
    expect(carrier?.tagName).toBe('SPAN');
    expect(container.querySelector('li')?.hasAttribute('data-nc-field')).toBe(false);
    expect(container.querySelectorAll('[data-nc-field]').length).toBe(1);
  });

  it('emits no field attribute when the prop is omitted', () => {
    const { container } = render(row());
    expect(container.querySelector('[data-nc-field]')).toBeNull();
  });

  /* The row-action marker shares the `<li>` on purpose: it is a host annotation, not a content marker. */
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

  /* The meta lane is a sibling of the label, so nothing the painter puts there lands inside the compared string. */
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
  it('forwards a header action without turning page content into the control', async () => {
    const onClick = vi.fn();
    render(
      <MobileListPage
        title="Areas"
        actions={<button type="button" onClick={onClick}>New area</button>}
      >
        rows
      </MobileListPage>,
    );
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    expect(onClick).toHaveBeenCalledOnce();
    expect(screen.getByText('rows').closest('button')).toBeNull();
  });

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
