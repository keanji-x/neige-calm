// @vitest-environment jsdom
// Invariants for the workspace rail.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove } from '../../../../core/domain/cove.ts';
import { NEUTRAL_ACTIVITY, type Wave } from '../../../../core/domain/wave.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

function memoryStorage() {
  const values = new Map<string, string>();
  return { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}

function cove(overrides: Partial<Cove> = {}): Cove {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function wave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1', coveId: 'c1', title: 'Task', sort: 1, lifecycle: 'draft', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

function renderSidebar(props: Partial<Parameters<typeof Sidebar>[0]> = {}) {
  const waves = props.waves ?? [];
  return render(
    <ThemeProvider storage={memoryStorage()}>
      <Sidebar
        coves={props.coves ?? [cove()]}
        wavesByCove={props.wavesByCove ?? new Map([['c1', waves]])}
        waves={waves}
        currentPath={props.currentPath ?? '/'}
        onGo={props.onGo ?? vi.fn()}
        onCreateCove={props.onCreateCove ?? vi.fn()}
        onDeleteCove={props.onDeleteCove ?? vi.fn()}
        onSetPinned={props.onSetPinned ?? vi.fn()}
        onDeleteWave={props.onDeleteWave ?? vi.fn()}
        collapsed={props.collapsed ?? false}
        onToggleCollapsed={props.onToggleCollapsed ?? vi.fn()}
        onOpenSettings={props.onOpenSettings ?? vi.fn()}
        onSignOut={props.onSignOut ?? vi.fn()}
      />
    </ThemeProvider>,
  );
}

describe('INV-SIDEBAR-007 three sections, and pinning is not relocation', () => {
  const pinnedAndBlocked = wave({ id: 'both', title: 'Both', lifecycle: 'blocked', pinnedAt: 10 });

  it('renders Waiting on you, then Pinned, then Coves', () => {
    renderSidebar({ waves: [pinnedAndBlocked] });
    const headings = screen.getAllByRole('heading').map((node) => node.textContent);
    expect(headings).toEqual(['Waiting on you', 'Pinned', 'Coves']);
  });

  it('keeps a pinned wave in its cove list as well as in the Pinned section', () => {
    renderSidebar({ waves: [wave({ id: 'p', title: 'Pinned task', pinnedAt: 10 })] });
    // One row under Pinned (carries the cove name) and one inside the cove list.
    expect(screen.getAllByRole('button', { name: /^Wave Pinned task/ })).toHaveLength(2);
  });

  it('surfaces a pinned attention wave in Waiting on you too — three rows, not one', () => {
    renderSidebar({ waves: [pinnedAndBlocked] });
    expect(screen.getAllByRole('button', { name: /^Wave Both/ })).toHaveLength(3);
  });

  it('drops a section entirely when it is empty rather than reordering the rest', () => {
    renderSidebar({ waves: [wave()] });
    expect(screen.getAllByRole('heading').map((node) => node.textContent)).toEqual(['Coves']);
  });
});

describe('INV-SIDEBAR-012 the pin button is always in the accessibility tree', () => {
  // The *visual* reveal (opacity 0 until hover, opacity 1 once pinned) is CSS in
  // `features/wave/row/row.module.css` and is a `browser`-tier concern: jsdom
  // does not apply CSS Modules, so this test cannot prove it. What it can prove
  // — and what actually breaks touch users if it regresses — is that the control
  // exists and is reachable in both states, carrying its pressed state.
  it('exposes a pressed-state pin control for pinned and unpinned waves alike', () => {
    renderSidebar({
      waves: [wave({ id: 'u', title: 'Loose' }), wave({ id: 'p', title: 'Stuck', pinnedAt: 10 })],
    });
    const unpinned = screen.getByRole('button', { name: 'Pin Loose' });
    expect(unpinned.getAttribute('aria-pressed')).toBe('false');
    // The pinned wave renders twice (Pinned section + cove list); both carry it.
    const pinned = screen.getAllByRole('button', { name: 'Unpin Stuck' });
    expect(pinned).toHaveLength(2);
    for (const node of pinned) expect(node.getAttribute('aria-pressed')).toBe('true');
  });
});

describe('E2E-INV-SHELL-003 the kernel system cove never reaches the rail', () => {
  it('renders zero cove rows for a workspace whose only cove is a system cove', () => {
    renderSidebar({
      coves: [cove({ id: 'sys', name: 'System', kind: 'system' })],
      waves: [wave({ id: 'k', coveId: 'sys', title: 'Kernel' })],
      wavesByCove: new Map([['sys', [wave({ id: 'k', coveId: 'sys', title: 'Kernel' })]]]),
    });
    expect(screen.queryByRole('button', { name: /^System/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /^Wave Kernel/ })).toBeNull();
    // §5.3's strongest rule: when a region's emptiness has exactly one remedy,
    // render that remedy's own interface where the content would have been. So
    // there is no "No coves yet." sentence pointing at a button elsewhere —
    // the create field is already open in the first row's place.
    expect(screen.queryByText(/no coves/i)).toBeNull();
    expect(screen.getByRole('textbox', { name: 'Cove name' })).toBeTruthy();
  });
});

describe('INV-A11Y-058 there is intentionally no skip link', () => {
  it('exposes no skip-to-content affordance', () => {
    const { container } = renderSidebar({ waves: [wave()] });
    expect(screen.queryByRole('link', { name: /skip/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /skip/i })).toBeNull();
    expect(container.textContent).not.toMatch(/skip to/i);
  });
});

describe('INV-A11Y-061 navigation shape', () => {
  it('uses buttons for every navigation row and emits no native links', () => {
    const onGo = vi.fn();
    const { container } = renderSidebar({ waves: [wave({ id: 'w9', title: 'Row' })], onGo });
    expect(container.querySelectorAll('a').length).toBe(0);
    for (const node of screen.getAllByRole('button')) expect(node.tagName).toBe('BUTTON');
  });

  it('routes cove and wave rows through onGo with structured targets', async () => {
    const onGo = vi.fn();
    renderSidebar({ waves: [wave({ id: 'w9', title: 'Row' })], onGo });
    await userEvent.click(screen.getByRole('button', { name: /^Work/ }));
    await userEvent.click(screen.getByRole('button', { name: /^Wave Row/ }));
    const targets: unknown[] = onGo.mock.calls.map((call) => (call as unknown[])[0]);
    expect(targets).toEqual([
      { name: 'cove', coveId: 'c1' },
      { name: 'wave', waveId: 'w9' },
    ]);
  });
});

describe('active row', () => {
  it('marks the open cove and the open wave with aria-current', () => {
    renderSidebar({ waves: [wave({ id: 'w9', title: 'Row' })], currentPath: '/wave/w9' });
    expect(screen.getByRole('button', { name: /^Wave Row/ }).getAttribute('aria-current')).toBe('page');
    expect(screen.getByRole('button', { name: /^Work/ }).getAttribute('aria-current')).toBeNull();
  });

  /*
   * The open wave is marked in **one** place, however many sections it appears
   * in. "Waiting on you" and "Pinned" are shortcuts into the tree; the cove
   * list is the tree, and a location is shown where the thing lives. A wave
   * that is open, pinned and blocked renders three rows here — this pins that
   * exactly one of them claims to be the current page.
   */
  it('marks the open wave once, in its cove, not in the shortcut sections', () => {
    const open = wave({ id: 'w9', title: 'Row', lifecycle: 'blocked', pinnedAt: 10 });
    renderSidebar({ waves: [open], currentPath: '/wave/w9' });
    const rows = screen.getAllByRole('button', { name: /^Wave Row/ });
    expect(rows).toHaveLength(3);
    expect(rows.filter((row) => row.getAttribute('aria-current') === 'page')).toHaveLength(1);
  });
});
