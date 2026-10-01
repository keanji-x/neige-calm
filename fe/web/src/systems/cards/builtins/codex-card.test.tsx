// @vitest-environment jsdom
//
// Renders the entry's `component` for real: no other suite executes a component.

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { CardHostCapabilities } from '../contracts.ts';
import { CODEX_CARD_ENTRY } from './codex.ts';

afterEach(cleanup);

function fakeHost(): CardHostCapabilities {
  return {
    lifecycle: {
      getSnapshot: () => ({ visible: true, focused: false, geometry: { w: 0, h: 0 }, refresh: 0 }),
      subscribe: () => () => {},
    },
    slots: { use: () => [{ current: null }] },
    emit: vi.fn(),
  } as unknown as CardHostCapabilities;
}

describe('codex card component', () => {
  it('renders a PTY surface headed "codex", not "terminal"', () => {
    const Component = CODEX_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'codex', presentation: 'interactive_tui', id: 'x1', title: null, terminalId: 't1', sessionState: 'running', cwd: null, gateCwd: null }}
        host={fakeHost()}
        activity={null}
      />,
    );

    expect(screen.getByText('codex')).toBeTruthy();
    expect(screen.queryByText('terminal')).toBeNull();
    // Read off the rendered node rather than queried by class (`no-class-dom-query`): a wrong fallback still renders *a* letter, so the semantic class is the assertion with teeth.
    const avatar = screen.getByText('C');
    expect(avatar.className).toContain('card-head-icon--codex');
    expect(avatar.className).not.toContain('card-head-icon--claude');
    // `data-nc-terminal-id` is the locator for the live state (`no-class-dom-query` forbids `.term.live`).
    expect(document.querySelector('[data-nc-terminal-id="t1"]')).not.toBeNull();
  });

  it('prefers the kernel row title when there is one', () => {
    const Component = CODEX_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'codex', presentation: 'interactive_tui', id: 'x1', title: 'tencent-valuation', terminalId: 't1', sessionState: 'running', cwd: null, gateCwd: null }}
        host={fakeHost()}
        activity={null}
      />,
    );
    expect(screen.getByText('tencent-valuation')).toBeTruthy();
    expect(screen.queryByText('codex')).toBeNull();
  });

  it('says the agent is starting, not "terminal", before the id is projected', () => {
    const Component = CODEX_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'codex', presentation: 'interactive_tui', id: 'x1', title: null, terminalId: null, sessionState: 'starting', cwd: null, gateCwd: null }}
        host={fakeHost()}
        activity={null}
      />,
    );
    expect(screen.getByText('Starting codex…')).toBeTruthy();
    expect(screen.queryByText('Starting terminal…')).toBeNull();
    expect(document.querySelector('[data-nc-terminal-id=""]')).not.toBeNull();
    expect(document.querySelector('[data-nc-terminal-id="t1"]')).toBeNull();
  });
});

describe('worker checkout visibility', () => {
  it('shows the actual worker directory from the kernel card', () => {
    const card = CODEX_CARD_ENTRY.fromKernel({
      id: 'checkout-worker', kind: 'codex',
      payload: { cwd: '/repo/.claude/worktrees/track/worker', gate_cwd: '/repo/gate-override' },
    });
    if (card === null) throw new Error('worker card must resolve');
    const Component = CODEX_CARD_ENTRY.component;
    render(<Component card={card} host={fakeHost()} activity={null} />);
    expect(screen.getByText('/repo/.claude/worktrees/track/worker')).toBeTruthy();
    expect(screen.getByText('Working directory')).toBeTruthy();
    expect(screen.getByText('Gate working directory')).toBeTruthy();
    expect(screen.getByText('/repo/gate-override')).toBeTruthy();
  });
});

describe('native-only worker', () => {
  it('shows its goal and committed result without mounting a terminal', () => {
    const card = CODEX_CARD_ENTRY.fromKernel({
      id: 'readonly-review', kind: 'codex',
      payload: {
        worker_presentation: { kind: 'native_only' },
        worker_snapshot: {
          task_id: 'review', goal: 'Review cancellation lifetimes', status: 'done',
          report: { kind: 'reported', outcome: 'completed', result: 'Found one missing stop fence.' },
        },
        cwd: '/repo/track',
      },
    });
    if (card === null) throw new Error('native worker must resolve');
    const Component = CODEX_CARD_ENTRY.component;
    render(<Component card={card} host={fakeHost()} activity={null} />);
    expect(screen.getByText('Review cancellation lifetimes')).toBeTruthy();
    expect(screen.getByText('Found one missing stop fence.')).toBeTruthy();
    expect(screen.getByText('Read-only')).toBeTruthy();
    expect(document.querySelector('[data-nc-terminal-card]')).toBeNull();
    expect(screen.queryByRole('textbox')).toBeNull();
  });
});
