// @vitest-environment jsdom
//
// Renders the entry's `component` for real. Both review channels flagged that
// nothing did: `claude.test.ts` covers resolution, registration and the
// partition, and `register.contract.test.ts` states outright that it never
// executes a component — so replacing Claude's `component` with `() => null`,
// or dropping the `fallbackTitle` that stops the card announcing itself as a
// terminal, was a green mutation. The board would open onto a blank card and
// every existing test would still pass.

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
        card={{ type: 'claude', id: 'c1', title: null, terminalId: 't1', sessionState: 'running' }}
        host={fakeHost()}
      />,
    );

    expect(screen.getByText('claude')).toBeTruthy();
    // `LetterAvatar` derives the provider avatar from that same head string;
    // its own module test owns the colour, this owns the string reaching it.
    expect(screen.getByText('C')).toBeTruthy();
    // A resolved terminal id is what makes the card live, which is the whole
    // point of opening it. `data-nc-terminal-id` is the locator for that state
    // (`no-class-dom-query` forbids reaching for `.term.live`).
    expect(document.querySelector('[data-nc-terminal-id="t1"]')).not.toBeNull();
  });

  it('prefers the kernel row title when there is one', () => {
    const Component = CLAUDE_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'claude', id: 'c1', title: 'tencent-valuation', terminalId: 't1', sessionState: 'running' }}
        host={fakeHost()}
      />,
    );
    expect(screen.getByText('tencent-valuation')).toBeTruthy();
    expect(screen.queryByText('claude')).toBeNull();
  });

  it('says the agent is starting, not "terminal", before the id is projected', () => {
    // `fromKernel` explicitly models this window (the kernel projects
    // `terminal_id` on read), so it is a state users really see.
    const Component = CLAUDE_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'claude', id: 'c1', title: null, terminalId: null, sessionState: 'starting' }}
        host={fakeHost()}
      />,
    );
    expect(screen.getByText('Starting claude…')).toBeTruthy();
    // Empty locator, not a missing one: the card is mounted but not live.
    expect(document.querySelector('[data-nc-terminal-id=""]')).not.toBeNull();
    expect(document.querySelector('[data-nc-terminal-id="t1"]')).toBeNull();
  });

  it('leaves the terminal card wearing its own name', () => {
    // The shared renderer gained a parameter; the existing card must not have
    // moved. This is the mutation guard on `fallbackTitle`'s default.
    const Component = TERMINAL_CARD_ENTRY.component;
    render(
      <Component
        card={{ type: 'terminal', id: 't-card', title: null, terminalId: null, sessionState: 'starting' }}
        host={fakeHost()}
      />,
    );
    expect(screen.getByText('terminal')).toBeTruthy();
    expect(screen.getByText('Starting terminal…')).toBeTruthy();
  });
});
