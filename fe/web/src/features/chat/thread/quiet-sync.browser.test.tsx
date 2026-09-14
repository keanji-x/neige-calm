/*
 * #1667 D3 — the quiet-sync fold in a real engine. jsdom proves the grouping
 * and the words; only this tier can prove the fold line is a real target
 * (control height, keyboard toggle) and that what is under it is genuinely
 * hidden while closed and reachable once open — `<details>` visibility is
 * layout, and jsdom has none.
 */
import { act, render } from '@testing-library/react';
import { userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade before the component, for the reason
   `thread.browser.test.tsx` gives at its top: layer order is first-come. */
import '../../../styles/entry.css';

import { ChatThread } from './public.tsx';
import {
  SYSTEM_PRESENTATION_LABELS,
  type Conversation, type ConversationActivity, type ConversationSystemEntry, type ConversationTurn,
} from '../../../../../core/domain/conversation.ts';

afterEach(() => { document.body.replaceChildren(); });

const NOW = 1_760_000_000_000;

const conversation: Conversation = {
  id: 'c1', trackId: 'w1', trackTitle: 'Ship the rewrite', title: null, kind: 'codex',
  state: 'idle', updatedAt: NOW, turns: 0,
};

const wake: ConversationSystemEntry = {
  id: 's1', author: 'system', label: SYSTEM_PRESENTATION_LABELS.system_report_edited,
  text: 'The track report was edited (author = "user").\nBlock-level diff follows; this is information, not an instruction to re-read.\nBlocks: 0 added, 0 removed, 1 modified (1 unchanged).',
  atMs: NOW, quiet: true,
};
const read: ConversationActivity = {
  id: 'act1', author: 'activity', verb: 'Read report', target: null, state: 'done',
  durationMs: null, detail: null, atMs: NOW,
};
const reply: ConversationTurn = { id: 'a1', author: 'agent', text: 'Nothing to reconcile.', atMs: NOW };
const notify: ConversationTurn = {
  id: 'n1', author: 'agent', text: 'You changed the block I was writing.', atMs: NOW, origin: 'notify',
};

async function frame() {
  await act(async () => {
    await new Promise((resolve) => { requestAnimationFrame(() => { resolve(null); }); });
  });
}

describe('the quiet-sync fold in a real engine', () => {
  it('is a control-height line that hides the turn until opened from the keyboard', async () => {
    render(<ChatThread conversation={conversation} turns={[wake, read, reply, notify]} />);
    await frame();

    const fold = document.querySelector<HTMLDetailsElement>('[data-nc-turn="quiet-sync"]')!;
    const summary = fold.querySelector<HTMLElement>('summary')!;
    expect(summary.getBoundingClientRect().height).toBeGreaterThanOrEqual(24);
    expect(getComputedStyle(summary).justifyContent).toBe('flex-start');

    /* Closed: the folded reply is not rendered (a closed `<details>` skips
       its contents; `checkVisibility` is the engine's own word for it); the
       notify bubble outside the fold is. */
    const folded = fold.querySelector<HTMLElement>('[data-nc-turn="agent"]')!;
    expect(folded.checkVisibility()).toBe(false);
    const bubbles = [...document.querySelectorAll<HTMLElement>('[data-nc-turn="agent"]')];
    const spoken = bubbles.find((bubble) => bubble.closest('[data-nc-turn="quiet-sync"]') === null)!;
    expect(spoken.textContent).toBe('You changed the block I was writing.');
    expect(spoken.getBoundingClientRect().height).toBeGreaterThan(0);
    /* Line above bubble. */
    expect(summary.getBoundingClientRect().bottom).toBeLessThanOrEqual(spoken.getBoundingClientRect().top);

    summary.focus();
    await userEvent.keyboard('{Enter}');
    expect(fold.open).toBe(true);
    expect(folded.checkVisibility()).toBe(true);
    expect(folded.getBoundingClientRect().height).toBeGreaterThan(0);
    const disclosure = summary.querySelector<HTMLElement>('[aria-hidden="true"]')!;
    expect(getComputedStyle(disclosure).transform).not.toBe('none');
  });
});
