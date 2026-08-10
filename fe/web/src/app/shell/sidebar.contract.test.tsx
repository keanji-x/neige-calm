// @vitest-environment jsdom
// Invariants for the workspace rail.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove } from '../../../../core/domain/cove.ts';
import type { Wave } from '../../../../core/domain/wave.ts';
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
});
