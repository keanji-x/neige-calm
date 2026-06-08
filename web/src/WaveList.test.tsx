// Component-level tests for `WaveList` — the keyboard-canonical alternative
// to WaveGrid added in Slice 9 of issue #56.
//
// What we lock in here:
//
//   1. **Cards render as a semantic <ul> in sort order.** Each <li> wraps
//      a WaveCard with a roving-tabindex setup; only the active one is in
//      the Tab order.
//   2. **Arrow keys move focus between rows.** ArrowDown / ArrowUp /
//      Home / End — the WAI-ARIA listbox model from `useRovingTabindex`.
//   3. **Alt+ArrowUp / Alt+ArrowDown swap card sort values via the
//      `updateCard` API.** Two mutations are issued, one per card, with
//      the other's sort value. The component does NOT optimistically
//      reorder its own props (that's `useUpdateCardMutation`'s job inside
//      the cache); we only assert on the API calls.
//   4. **Delete removes the focused card.** Mirrors the `×` button.
//
// The `useUpdateCardMutation` and `useOverlayState` hooks are not stubbed —
// we mock `api/calm.ts` at the module boundary (same pattern as
// `WaveGrid.test.tsx`) and use a real QueryClient.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, fireEvent, render, waitFor, cleanup, screen } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useEffect, type ReactNode } from 'react';
import { ThemeProvider } from './app/theme';

vi.mock('./api/calm', () => ({
  listOverlays: vi.fn(),
  upsertOverlay: vi.fn(),
  updateCard: vi.fn(),
  getTerminalForCard: vi.fn().mockRejectedValue(new Error('no terminal seed')),
  listDir: vi.fn().mockResolvedValue({ path: '/repo', parent: null, entries: [] }),
  readFile: vi.fn(),
  readFileRaw: vi.fn((path: string) => `/api/fs/read?path=${encodeURIComponent(path)}`),
  gitStatus: vi.fn().mockResolvedValue({ repo_root: '/repo', files: [] }),
  gitDiff: vi.fn(),
  toolCallFromIframe: vi.fn(),
}));

vi.mock('./api/events', () => ({
  sharedEventStream: vi.fn(() => ({
    addTopic: () => {},
    on: () => () => {},
  })),
}));

vi.mock('./XtermView', async () => {
  const React = await vi.importActual<typeof import('react')>('react');
  const XtermView = React.forwardRef(
    (
      props: { terminalId: string },
      ref: React.Ref<{ refresh(): void }>,
    ) => {
      React.useImperativeHandle(ref, () => ({ refresh: () => {} }), []);
      return React.createElement('div', {
        'data-testid': 'xterm-view-stub',
        'data-terminal-id': props.terminalId,
      });
    },
  );
  return { XtermView };
});

// xterm.js + the codex / terminal card components pull in heavy modules
// (XtermView) at lazy-import time. WaveList renders WaveCards directly,
// and the spec-card fixture below mounts the codex terminal surface, so
// we stub XtermView at the module boundary.

import * as api from './api/calm';
import { registerBuiltins } from './cards/builtins';
import { registerCard, type CardEntry } from './cards/registry';
import { __resetCardEntryResolverRegistryForTest } from './cards/resolver';
import { WaveList } from './WaveList';
import type { WaveCardSlot, WaveCardData } from './types';

declare module './types' {
  interface WaveCardDataMap {
    'list-visibility-test': ListVisibilityCardData;
  }
}

interface ListVisibilityCardData {
  type: 'list-visibility-test';
  id: string;
}

const originalIntersectionObserver = globalThis.IntersectionObserver;

class FakeIntersectionObserver {
  static instances: FakeIntersectionObserver[] = [];

  private readonly callback: IntersectionObserverCallback;
  observed = new Set<Element>();
  observe = vi.fn((target: Element) => {
    this.observed.add(target);
  });
  unobserve = vi.fn((target: Element) => {
    this.observed.delete(target);
  });
  disconnect = vi.fn(() => {
    this.observed.clear();
  });
  takeRecords = vi.fn(() => []);

  constructor(callback: IntersectionObserverCallback) {
    this.callback = callback;
    FakeIntersectionObserver.instances.push(this);
  }

  fire(entries: Array<Partial<IntersectionObserverEntry> & { target: Element }>) {
    this.callback(
      entries as IntersectionObserverEntry[],
      this as unknown as IntersectionObserver,
    );
  }
}

registerBuiltins();

function slot(
  id: string,
  sort: number,
  kind: 'terminal' | 'codex' = 'terminal',
): WaveCardSlot {
  const data: WaveCardData =
    kind === 'codex'
      ? { type: 'codex', id }
      : { type: 'terminal', id, title: id, lines: [], terminalId: undefined };
  return { kind: 'card', card: data, sort };
}

function visibilitySlot(id: string, sort: number): WaveCardSlot {
  return {
    kind: 'card',
    sort,
    card: { type: 'list-visibility-test', id },
  };
}

function registerVisibilityEntry({
  onVisibleChange = vi.fn(),
  onUnmount = vi.fn(),
}: {
  onVisibleChange?: (visible: boolean) => void;
  onUnmount?: () => void;
}) {
  function VisibilityCard() {
    useEffect(() => () => onUnmount());
    return <div data-testid="list-visibility-card">visible</div>;
  }

  registerCard({
    type: 'list-visibility-test',
    Component: VisibilityCard,
    defaultSize: { w: 4, h: 3, minW: 2, minH: 2 },
    title: () => 'visibility',
    accessibleName: () => 'Visibility test card',
    create: { mode: 'kernel-minted-only' },
    createController: () => ({ onVisibleChange }),
  } as CardEntry<ListVisibilityCardData>);
}

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0, staleTime: 0 },
      mutations: { retry: false },
    },
  });
}

function Wrapper({
  client,
  children,
}: {
  client: QueryClient;
  children: ReactNode;
}) {
  return (
    <ThemeProvider>
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    </ThemeProvider>
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  __resetCardEntryResolverRegistryForTest();
  FakeIntersectionObserver.instances = [];
  globalThis.IntersectionObserver =
    FakeIntersectionObserver as unknown as typeof IntersectionObserver;
  cleanup();
});

afterEach(() => {
  if (originalIntersectionObserver) {
    globalThis.IntersectionObserver = originalIntersectionObserver;
  } else {
    const mutableGlobal = globalThis as {
      IntersectionObserver?: typeof IntersectionObserver;
    };
    delete mutableGlobal.IntersectionObserver;
  }
  __resetCardEntryResolverRegistryForTest();
});

describe('WaveList — rendering + accessibility', () => {
  it('renders cards as <li> items inside a labeled <ul>', () => {
    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[slot('a', 10), slot('b', 20)]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    // The wave-list role is `list` (UL default); accessible name comes
    // from the aria-label.
    const list = screen.getByRole('list', { name: /wave cards/i });
    expect(list).toBeTruthy();

    // Two list items, each with a per-card aria-label derived from the
    // card's title.
    const items = screen.getAllByRole('listitem');
    expect(items.length).toBe(2);
    expect(items[0].getAttribute('aria-label')).toMatch(/terminal:\s*a/i);
    expect(items[1].getAttribute('aria-label')).toMatch(/terminal:\s*b/i);
  });

  it('applies roving tabindex — first item is in the Tab order, others are -1', () => {
    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[slot('a', 10), slot('b', 20), slot('c', 30)]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    const items = screen.getAllByRole('listitem');
    expect(items[0].getAttribute('tabindex')).toBe('0');
    expect(items[1].getAttribute('tabindex')).toBe('-1');
    expect(items[2].getAttribute('tabindex')).toBe('-1');
  });

  it('exposes the documented keyboard shortcuts via aria-keyshortcuts', () => {
    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[slot('a', 10)]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );
    const item = screen.getAllByRole('listitem')[0];
    const ks = item.getAttribute('aria-keyshortcuts') ?? '';
    // The exact string is in the slice 9 contract; we assert each
    // documented shortcut appears.
    expect(ks).toMatch(/ArrowUp/);
    expect(ks).toMatch(/ArrowDown/);
    expect(ks).toMatch(/Alt\+ArrowUp/);
    expect(ks).toMatch(/Alt\+ArrowDown/);
    expect(ks).toMatch(/Home/);
    expect(ks).toMatch(/End/);
    expect(ks).toMatch(/Delete/);
  });

  it('uses entry accessibleName metadata for iframe, plugin, and file-viewer rows', () => {
    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[
            {
              kind: 'card',
              card: {
                type: 'iframe',
                id: 'iframe_1',
                url: 'https://example.com',
              },
              sort: 10,
            },
            {
              kind: 'card',
              card: {
                type: 'plugin',
                id: 'plugin_1',
                resource_uri: 'ui://hello/main',
              },
              sort: 20,
            },
            {
              kind: 'card',
              card: {
                type: 'file-viewer',
                id: 'file_1',
                path: '/repo',
              },
              sort: 30,
            },
          ]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    expect(
      screen.getByRole('listitem', { name: 'Web page: https://example.com' }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole('listitem', { name: 'Plugin: ui://hello/main' }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole('listitem', { name: 'File: /repo' }),
    ).toBeInTheDocument();
  });

  it('does not let iframe head action key events reach the row roving handler', () => {
    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[
            {
              kind: 'card',
              card: {
                type: 'iframe',
                id: 'iframe_1',
                url: 'https://example.com',
              },
              sort: 10,
            },
          ]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    const reload = screen.getByRole('button', { name: 'Reload' });
    const event = new KeyboardEvent('keydown', {
      key: 'Enter',
      bubbles: true,
      cancelable: true,
    });

    expect(reload.dispatchEvent(event)).toBe(true);
    expect(event.defaultPrevented).toBe(false);
  });

  it('routes row intersection changes to the card controller without unmounting', async () => {
    const onVisibleChange = vi.fn();
    const onUnmount = vi.fn();
    registerVisibilityEntry({ onVisibleChange, onUnmount });

    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[visibilitySlot('list_visibility', 10)]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    const shell = screen
      .getByTestId('list-visibility-card')
      .closest<HTMLElement>('[data-card-id]');
    expect(shell).not.toBeNull();
    await waitFor(() =>
      expect(FakeIntersectionObserver.instances[0]?.observe).toHaveBeenCalledWith(
        shell,
      ),
    );

    act(() => {
      FakeIntersectionObserver.instances[0]!.fire([
        { target: shell!, isIntersecting: false },
      ]);
    });
    expect(onVisibleChange).toHaveBeenCalledWith(false);
    expect(onUnmount).not.toHaveBeenCalled();
    expect(screen.getByTestId('list-visibility-card')).toBeInTheDocument();

    act(() => {
      FakeIntersectionObserver.instances[0]!.fire([
        { target: shell!, isIntersecting: true },
      ]);
    });
    expect(onVisibleChange).toHaveBeenLastCalledWith(true);
  });
});

describe('WaveList — keyboard navigation', () => {
  it('ArrowDown moves focus to the next row; ArrowUp moves back', async () => {
    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[slot('a', 10), slot('b', 20), slot('c', 30)]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    const items = screen.getAllByRole('listitem');
    items[0].focus();
    expect(document.activeElement).toBe(items[0]);

    fireEvent.keyDown(items[0], { key: 'ArrowDown' });
    await waitFor(() => expect(document.activeElement).toBe(items[1]));

    fireEvent.keyDown(items[1], { key: 'ArrowDown' });
    await waitFor(() => expect(document.activeElement).toBe(items[2]));

    fireEvent.keyDown(items[2], { key: 'ArrowUp' });
    await waitFor(() => expect(document.activeElement).toBe(items[1]));
  });

  it('Home and End jump to first / last row', async () => {
    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[slot('a', 10), slot('b', 20), slot('c', 30)]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    const items = screen.getAllByRole('listitem');
    items[0].focus();
    fireEvent.keyDown(items[0], { key: 'End' });
    await waitFor(() => expect(document.activeElement).toBe(items[2]));
    fireEvent.keyDown(items[2], { key: 'Home' });
    await waitFor(() => expect(document.activeElement).toBe(items[0]));
  });

});

describe('WaveList — reorder via Alt+ArrowUp/Down', () => {
  it('Alt+ArrowDown calls updateCard for both cards with swapped sort values', async () => {
    (api.updateCard as ReturnType<typeof vi.fn>).mockImplementation(
      async (id: string, body: unknown) => ({
        id,
        wave_id: 'w1',
        kind: 'terminal',
        title: id,
        sort: (body as { sort?: number }).sort ?? 0,
        payload: null,
        updated_at: Date.now(),
        created_at: 0,
      }),
    );

    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[slot('a', 10), slot('b', 20)]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    const items = screen.getAllByRole('listitem');
    items[0].focus();
    act(() => {
      fireEvent.keyDown(items[0], { key: 'ArrowDown', altKey: true });
    });

    // Two mutations: card 'a' gets sort 20 (was b's), card 'b' gets sort
    // 10 (was a's). Both must be invoked. The sequential-vs-concurrent
    // contract is locked separately in the next test ("swap waits for the
    // first mutation to resolve before firing the second").
    await waitFor(() =>
      expect((api.updateCard as ReturnType<typeof vi.fn>).mock.calls.length).toBe(2),
    );
    const calls = (api.updateCard as ReturnType<typeof vi.fn>).mock.calls;
    const seen: Record<string, number> = {};
    for (const [id, body] of calls) {
      seen[id as string] = (body as { sort: number }).sort;
    }
    expect(seen).toEqual({ a: 20, b: 10 });
  });

  it('swap waits for the first mutation to resolve before firing the second', async () => {
    // The two updateCard calls MUST be sequential, not Promise.all. Concurrent
    // mutations race their onMutate cache snapshots and the second optimistic
    // write shadows the first, leaving a brief equal-sort UI rendering. A
    // future refactor to Promise.all would silently re-introduce that race;
    // this test fails loudly if anyone tries.
    let resolveFirst: (value: unknown) => void = () => {};
    const firstPending = new Promise((r) => {
      resolveFirst = r;
    });
    (api.updateCard as ReturnType<typeof vi.fn>)
      .mockImplementationOnce(() => firstPending)
      .mockResolvedValueOnce({
        id: 'b',
        wave_id: 'w1',
        kind: 'terminal',
        sort: 10,
        payload: null,
        updated_at: Date.now(),
        created_at: 0,
      });

    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[slot('a', 10), slot('b', 20)]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    const items = screen.getAllByRole('listitem');
    items[0].focus();
    act(() => {
      fireEvent.keyDown(items[0], { key: 'ArrowDown', altKey: true });
    });

    // Let any synchronous + microtask work flush. Under Promise.all both
    // mutations would already be invoked here; under sequential await only
    // the first call is in flight.
    await new Promise((r) => setTimeout(r, 20));
    expect((api.updateCard as ReturnType<typeof vi.fn>).mock.calls.length).toBe(1);
    expect((api.updateCard as ReturnType<typeof vi.fn>).mock.calls[0][0]).toBe('a');

    // Release the first mutation; the second should now fire.
    act(() => {
      resolveFirst({
        id: 'a',
        wave_id: 'w1',
        kind: 'terminal',
        sort: 20,
        payload: null,
        updated_at: Date.now(),
        created_at: 0,
      });
    });

    await waitFor(() =>
      expect((api.updateCard as ReturnType<typeof vi.fn>).mock.calls.length).toBe(2),
    );
    expect((api.updateCard as ReturnType<typeof vi.fn>).mock.calls[1][0]).toBe('b');
  });

  it('Alt+ArrowUp on the first card is a no-op (no mutations)', async () => {
    (api.updateCard as ReturnType<typeof vi.fn>).mockResolvedValue({});

    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[slot('a', 10), slot('b', 20)]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    const items = screen.getAllByRole('listitem');
    items[0].focus();
    fireEvent.keyDown(items[0], { key: 'ArrowUp', altKey: true });
    // Give any stray async work a tick to settle.
    await new Promise((r) => setTimeout(r, 10));
    expect(api.updateCard).not.toHaveBeenCalled();
  });

  it('Alt+ArrowDown on the last card is a no-op', async () => {
    (api.updateCard as ReturnType<typeof vi.fn>).mockResolvedValue({});

    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[slot('a', 10), slot('b', 20)]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    const items = screen.getAllByRole('listitem');
    items[1].focus();
    fireEvent.keyDown(items[1], { key: 'ArrowDown', altKey: true });
    await new Promise((r) => setTimeout(r, 10));
    expect(api.updateCard).not.toHaveBeenCalled();
  });
});

describe('WaveList — remove via Delete', () => {
  it('Delete on the focused row calls onRemoveCard with its index', () => {
    const onRemoveCard = vi.fn();
    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[slot('a', 10), slot('b', 20)]}
          onRemoveCard={onRemoveCard}
        />
      </Wrapper>,
    );

    const items = screen.getAllByRole('listitem');
    items[1].focus();
    fireEvent.keyDown(items[1], { key: 'Delete' });
    expect(onRemoveCard).toHaveBeenCalledWith(1);
  });

  it('Remove × button click also fires onRemoveCard', () => {
    const onRemoveCard = vi.fn();
    render(
      <Wrapper client={makeClient()}>
        <WaveList
          waveId="w1"
          cards={[slot('a', 10)]}
          onRemoveCard={onRemoveCard}
        />
      </Wrapper>,
    );

    const closeBtn = screen.getByRole('button', { name: /remove terminal:\s*a/i });
    fireEvent.click(closeBtn);
    expect(onRemoveCard).toHaveBeenCalledWith(0);
  });
});
