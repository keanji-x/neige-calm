/* A reply as it streams (#1923 S2), as a finger gets it (`vitest.config.ts`: `pointer: coarse`, touch): the pane
   follows its growth only for a reader at the end, and a narrow pane takes a growing code block without overflowing. */
import { cleanup, act, fireEvent, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';

import { ChatThread } from './public.tsx';
import type { Conversation, ConversationTurn, TranscriptEntry } from '../../../../../core/domain/conversation.ts';
import drawerStyles from '../../../ui/drawer/drawer.module.css';

afterEach(() => { cleanup(); document.body.replaceChildren(); });

const conversation: Conversation = { id: 'c1', trackId: 'w1', title: 'Review', kind: 'codex', state: 'running', updatedAt: 0 };
const LINE = 'The reply runs on for a few lines so the pane has something to scroll. ';

const history: readonly ConversationTurn[] = Array.from({ length: 6 }).flatMap((_unused, index) => [
  { id: `you-${index}`, author: 'you' as const, text: `Ask ${index}`, atMs: index * 2_000 },
  { id: `agent-${index}`, author: 'agent' as const, text: `Answer ${index}. ${LINE.repeat(8)}`, atMs: index * 2_000 + 1 },
]);
const asked: ConversationTurn = { id: 'you-now', author: 'you', text: 'Explain the change', atMs: 20_000 };
/** The live copy as the router draws it, and the stored row that replaces it. */
const live = (text: string): ConversationTurn => ({ id: 'live-T-m', author: 'agent', text, atMs: 20_001 });
const stored = (text: string): ConversationTurn => ({ id: '42', author: 'agent', text, atMs: 20_002 });

function Pane({ turns, width = 640 }: { turns: readonly TranscriptEntry[]; width?: number }) {
  return (
    <div className={drawerStyles.drawer} data-nc-drawer="" style={{ animation: 'none', transition: 'none', position: 'relative', inlineSize: width, blockSize: 448 }}>
      <div className={drawerStyles.scroll} data-nc-drawer-scroll="" style={{ blockSize: 400, flex: 'none' }}>
        <div className={drawerStyles.bodyInner}>
          <ChatThread canContinue={false} cards={{}} stalled={false} pending conversation={conversation} turns={turns} />
        </div>
      </div>
    </div>
  );
}

const pane = () => document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
const replies = () => [...document.querySelectorAll<HTMLElement>('[data-nc-turn="agent"]')];
const atEnd = () => pane().scrollHeight - pane().clientHeight;

async function frames() {
  for (let index = 0; index < 2; index += 1) {
    await act(async () => { await new Promise((resolve) => { requestAnimationFrame(() => { resolve(null); }); }); });
  }
}

describe('a streamed reply under a coarse pointer', () => {
  it('keeps the newest text in view while the reader is following', async () => {
    const { rerender } = render(<Pane turns={[...history, asked, live('First words')]} />);
    await frames();
    pane().scrollTop = atEnd();
    await frames();
    for (const length of [4, 10, 18]) {
      rerender(<Pane turns={[...history, asked, live(`First words. ${LINE.repeat(length)}`)]} />);
      await frames();
      expect(pane().scrollTop).toBe(atEnd());
    }
  });

  it('never moves the pane while the reader is reading history', async () => {
    const { rerender } = render(<Pane turns={[...history, asked, live('First words')]} />);
    await frames();
    fireEvent.wheel(pane(), { deltaY: 120 - pane().scrollTop });
    pane().scrollTop = 120;
    await frames();
    for (const length of [4, 10, 18]) {
      rerender(<Pane turns={[...history, asked, live(`First words. ${LINE.repeat(length)}`)]} />);
      await frames();
      expect(pane().scrollTop).toBe(120);
    }
  });

  it('grows Markdown and an open code block in a phone-width pane without overflowing, and lands the stored row without a jump', async () => {
    /* Each poll, and the last words it must already show. */
    const polls: readonly (readonly [string, string])[] = [
      ['Here is the fix:\n\n```ts\nconst answer = compute(', 'compute('],
      ['Here is the fix:\n\n```ts\nconst answer = compute(input);\nexport default answer;\n', 'export default answer;'],
      ['Here is the fix:\n\n```ts\nconst answer = compute(input);\nexport default answer;\n```\n\nIt **keeps** the old', 'It keeps the old'],
      ['Here is the fix:\n\n```ts\nconst answer = compute(input);\nexport default answer;\n```\n\nIt **keeps** the old behaviour.', 'the old behaviour.'],
    ];
    const { rerender } = render(<Pane width={358} turns={[asked, live(polls[0][0])]} />);
    await frames();
    for (const [text, newest] of polls) {
      rerender(<Pane width={358} turns={[asked, live(text)]} />);
      /* No typewriter: every character the poll brought is on the page in the same commit. */
      const [reply] = replies();
      expect(reply.textContent).toContain(newest);
      expect(reply.querySelector('pre, [role="textbox"][aria-readonly="true"]')).not.toBeNull();
      await frames();
      expect(pane().scrollWidth).toBeLessThanOrEqual(pane().clientWidth);
    }
    const final = polls[polls.length - 1][0];
    const [streamed] = replies();
    expect(streamed.textContent).toContain('It keeps the old behaviour.');
    const box = streamed.getBoundingClientRect();
    const before = { height: box.height, top: box.top, text: streamed.innerText };

    /* The row can land while the turn still runs (a tool call may follow the reply). */
    rerender(<Pane width={358} turns={[asked, stored(final)]} />);
    await frames();
    const [landed] = replies();
    expect(replies()).toHaveLength(1);
    expect(landed.innerText).toBe(before.text);
    expect(landed.getBoundingClientRect().height).toBe(before.height);
    expect(landed.getBoundingClientRect().top).toBe(before.top);
  });
});
