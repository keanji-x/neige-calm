// @vitest-environment jsdom
//
// Renders the entry's `component` for real: no other suite executes a component.

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { CardHostCapabilities } from '../contracts.ts';
import { CLAUDE_CARD_ENTRY } from './claude.ts';
import { TERMINAL_CARD_ENTRY } from './terminal.ts';

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

describe('claude card component', () => {
  it('renders a PTY surface headed "claude", not "terminal"', () => {
    const Component = CLAUDE_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'claude', id: 'c1', title: null, terminalId: 't1', sessionState: 'running', cwd: null, gateCwd: null }}
        host={fakeHost()}
        activity={null}
      />,
    );

    expect(screen.getByText('claude')).toBeTruthy();
    expect(screen.getByText('C')).toBeTruthy();
    // `data-nc-terminal-id` is the locator for the live state (`no-class-dom-query` forbids `.term.live`).
    expect(document.querySelector('[data-nc-terminal-id="t1"]')).not.toBeNull();
  });

  it('prefers the kernel row title when there is one', () => {
    const Component = CLAUDE_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'claude', id: 'c1', title: 'tencent-valuation', terminalId: 't1', sessionState: 'running', cwd: null, gateCwd: null }}
        host={fakeHost()}
        activity={null}
      />,
    );
    expect(screen.getByText('tencent-valuation')).toBeTruthy();
    expect(screen.queryByText('claude')).toBeNull();
  });

  it('says the agent is starting, not "terminal", before the id is projected', () => {
    const Component = CLAUDE_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'claude', id: 'c1', title: null, terminalId: null, sessionState: 'starting', cwd: null, gateCwd: null }}
        host={fakeHost()}
        activity={null}
      />,
    );
    expect(screen.getByText('Starting claude…')).toBeTruthy();
    expect(document.querySelector('[data-nc-terminal-id=""]')).not.toBeNull();
    expect(document.querySelector('[data-nc-terminal-id="t1"]')).toBeNull();
  });

  it('leaves the terminal card wearing its own name', () => {
    const Component = TERMINAL_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'terminal', id: 't-card', title: null, terminalId: null, sessionState: 'starting', cwd: null, gateCwd: null }}
        host={fakeHost()}
        activity={null}
      />,
    );
    expect(screen.getByText('terminal')).toBeTruthy();
    expect(screen.getByText('Starting terminal…')).toBeTruthy();
  });
});
