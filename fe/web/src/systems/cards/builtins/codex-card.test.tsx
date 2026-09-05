// @vitest-environment jsdom
//
// Renders the entry's `component` for real. Nothing else does: `codex.test.ts`
// covers resolution, registration and the partition, and
// `register.contract.test.ts` states outright that it never executes a
// component — so replacing codex's `component` with `() => null`, or dropping
// the `fallbackTitle` that stops the card announcing itself as a terminal,
// would be a green mutation. The board would open onto a blank card and every
// other test would still pass.

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
        card={{ type: 'codex', id: 'x1', title: null, terminalId: 't1', sessionState: 'running', cwd: null, gateCwd: null }}
        host={fakeHost()}
      />,
    );

    expect(screen.getByText('codex')).toBeTruthy();
    expect(screen.queryByText('terminal')).toBeNull();
    // `LetterAvatar` derives the provider avatar from that same head string —
    // it special-cases the lowercased title `codex` into
    // `card-head-icon--codex`, so the fallback word is load-bearing beyond the
    // label. Read off the rendered node rather than queried by class
    // (`no-class-dom-query`): a wrong fallback still renders *a* letter, so the
    // semantic class is the assertion that has teeth.
    const avatar = screen.getByText('C');
    expect(avatar.className).toContain('card-head-icon--codex');
    expect(avatar.className).not.toContain('card-head-icon--claude');
    // A resolved terminal id is what makes the card live, which is the whole
    // point of opening it. `data-nc-terminal-id` is the locator for that state
    // (`no-class-dom-query` forbids reaching for `.term.live`).
    expect(document.querySelector('[data-nc-terminal-id="t1"]')).not.toBeNull();
  });

  it('prefers the kernel row title when there is one', () => {
    const Component = CODEX_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'codex', id: 'x1', title: 'tencent-valuation', terminalId: 't1', sessionState: 'running', cwd: null, gateCwd: null }}
        host={fakeHost()}
      />,
    );
    expect(screen.getByText('tencent-valuation')).toBeTruthy();
    expect(screen.queryByText('codex')).toBeNull();
  });

  it('says the agent is starting, not "terminal", before the id is projected', () => {
    // `fromKernel` explicitly models this window (the kernel projects
    // `terminal_id` on read), so it is a state users really see.
    const Component = CODEX_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'codex', id: 'x1', title: null, terminalId: null, sessionState: 'starting', cwd: null, gateCwd: null }}
        host={fakeHost()}
      />,
    );
    expect(screen.getByText('Starting codex…')).toBeTruthy();
    expect(screen.queryByText('Starting terminal…')).toBeNull();
    // Empty locator, not a missing one: the card is mounted but not live.
    expect(document.querySelector('[data-nc-terminal-id=""]')).not.toBeNull();
    expect(document.querySelector('[data-nc-terminal-id="t1"]')).toBeNull();
  });
});

describe('worker checkout visibility', () => {
  it('shows the actual worker directory from the kernel card', () => {
    const card = CODEX_CARD_ENTRY.fromKernel({
      id: 'checkout-worker', kind: 'codex',
      payload: { cwd: '/repo/.claude/worktrees/track/worker', gate_cwd: '/repo/gate-override', terminal_id: 'stale-pty' },
      runtime: { runtime_id: 'worker-runtime', kind: 'codex', status: 'exited' },
    });
    if (card === null) throw new Error('worker card must resolve');
    const Component = CODEX_CARD_ENTRY.component;
    render(<Component card={card} host={fakeHost()} />);
    expect(screen.getByText('/repo/.claude/worktrees/track/worker')).toBeTruthy();
    expect(screen.getByText('Working directory')).toBeTruthy();
    expect(screen.getByText('Gate working directory')).toBeTruthy();
    expect(screen.getByText('/repo/gate-override')).toBeTruthy();
    expect(screen.getByText('Session exited.')).toBeTruthy();
    expect(screen.queryByRole('img', { name: 'status Working' })).toBeNull();
    expect(document.querySelector('[data-nc-terminal-id="stale-pty"]')).toBeNull();
  });
});
