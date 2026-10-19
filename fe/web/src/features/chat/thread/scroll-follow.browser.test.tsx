import { act, cleanup, render } from '@testing-library/react';
import { commands, page, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';
import { ChatThread, type ChatThreadProps } from './public.tsx';
import type { Conversation, TranscriptEntry } from '../../../../../core/domain/conversation.ts';
import { Drawer } from '../../../ui/drawer/public.tsx';
import { useRef } from 'react';
import drawerStyles from '../../../ui/drawer/drawer.module.css';

declare module 'vitest/browser' {
  interface BrowserCommands { wheelScroll(selector: string, deltaY: number): Promise<void> }
}

const conversation: Conversation = { provider: 'codex' as const, id: 'scroll', trackId: 'track', title: 'Reading', kind: 'codex', state: 'idle', updatedAt: 0 };
const words = 'A reply with enough lines to keep the transcript taller than its pane. ';
const history: readonly TranscriptEntry[] = Array.from({ length: 8 }).flatMap((_, index) => [
  { id: `you-${index}`, author: 'you' as const, text: `Question ${index}`, atMs: index * 2000 },
  { id: `agent-${index}`, author: 'agent' as const, text: words.repeat(8), atMs: index * 2000 + 1 },
]);
const later: TranscriptEntry = { id: 'later', author: 'agent', text: words.repeat(4), atMs: 20000 };

function Pane({ height = 400, turns = history, historyPrefetch }: { height?: number; turns?: readonly TranscriptEntry[]; historyPrefetch?: ChatThreadProps['historyPrefetch'] }) {
  return <div className={drawerStyles.drawer} data-nc-drawer="" style={{ animation: 'none', position: 'relative', inlineSize: 396, blockSize: height + 48 }}>
    <div className={drawerStyles.scroll} data-nc-drawer-scroll="" style={{ blockSize: height, flex: 'none' }}>
      <div className={drawerStyles.bodyInner}>
        <ChatThread historyPrefetch={historyPrefetch} hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation} turns={turns} />
      </div>
    </div>
  </div>;
}
const pane = () => document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
const remaining = () => pane().scrollHeight - pane().scrollTop - pane().clientHeight;
async function frames() {
  for (let i = 0; i < 4; i++) await act(async () => {
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  });
}
afterEach(cleanup);

describe('chat reading intent across layout changes', () => {
  for (const offset of [0, 200]) for (const streaming of [false, true]) it(`holds the same message when older history is prepended at offset ${offset}, stream ${streaming}`, async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<Pane />);
    await frames();
    await commands.wheelScroll('[data-nc-drawer-scroll]', -100000);
    await expect.poll(() => pane().scrollTop).toBe(0);
    if (offset > 0) {
      await commands.wheelScroll('[data-nc-drawer-scroll]', offset);
      await expect.poll(() => pane().scrollTop).toBe(offset);
    }
    await frames();
    const anchor = document.querySelector<HTMLElement>('[data-nc-entry="agent-0"]')!;
    const y = () => anchor.getBoundingClientRect().top - pane().getBoundingClientRect().top;
    const before = y();
    const older = history.map(entry => ({ ...entry, id: `old-${entry.id}`, atMs: entry.atMs - 20000 }));
    rerender(<Pane turns={[...older, ...history, ...(streaming ? [later] : [])]} />);
    expect(Math.abs(y() - before)).toBeLessThanOrEqual(1);
    await frames();
    expect(Math.abs(y() - before)).toBeLessThanOrEqual(1);
  });

  for (const atTail of [false, true]) it(`keeps a cross-message selection and its text position through prepend and streaming, tail ${atTail}`, async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<Pane />);
    await frames();
    if (!atTail) {
      await commands.wheelScroll('[data-nc-drawer-scroll]', -100000);
      await expect.poll(() => pane().scrollTop).toBe(0);
    }
    const textOf = (id: string) => {
      const walker = document.createTreeWalker([...document.querySelectorAll('[data-nc-entry]')].find(entry => entry.getAttribute('data-nc-entry') === id)!, NodeFilter.SHOW_TEXT);
      let node = walker.nextNode();
      while (node !== null && !node.textContent?.includes('A reply')) node = walker.nextNode();
      if (node === null) throw new Error('Reply text missing');
      return node;
    };
    const first = textOf(atTail ? 'agent-6' : 'agent-0');
    const second = textOf(atTail ? 'agent-7' : 'agent-1');
    document.getSelection()!.setBaseAndExtent(first, 10, second, 20);
    await frames();
    const selection = document.getSelection()!;
    const text = selection.toString();
    const range = document.createRange();
    range.setStart(first, 10); range.setEnd(first, 11);
    const before = range.getBoundingClientRect().top;
    rerender(<Pane turns={[{ id: 'older', author: 'agent', text: words.repeat(10), atMs: -1 }, ...history, later]} />);
    await frames();
    expect(selection.toString()).toBe(text);
    expect(Math.abs(range.getBoundingClientRect().top - before)).toBeLessThanOrEqual(1);
    expect(remaining()).toBeGreaterThan(100);
    selection.removeAllRanges();
  });

  for (const streaming of [false, true]) it(`keeps the tail following during prepend, stream ${streaming}`, async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<Pane />);
    await frames();
    rerender(<Pane turns={[{ id: 'older', author: 'agent', text: words.repeat(10), atMs: -1 }, ...history, ...(streaming ? [later] : [])]} />);
    await frames();
    expect(remaining()).toBeLessThanOrEqual(1);
  });

  it('keeps following after the viewport shrinks without reader navigation', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<Pane />);
    await frames();
    expect(remaining()).toBeLessThanOrEqual(1);
    rerender(<Pane height={200} />);
    await frames();
    rerender(<Pane height={200} turns={[...history, later]} />);
    await frames();
    expect(remaining()).toBeLessThanOrEqual(1);
  });

  it('does not resume following when viewport growth brings history near the tail', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<Pane />);
    await frames();
    const before = pane().scrollTop;
    await commands.wheelScroll('[data-nc-drawer-scroll]', -300);
    await expect.poll(() => pane().scrollTop).toBe(before - 300);
    await frames();
    expect(remaining()).toBeCloseTo(300, 0);
    rerender(<Pane height={650} />);
    await frames();
    expect(remaining()).toBeCloseTo(50, 0);
    const parked = pane().scrollTop;
    rerender(<Pane height={650} turns={[...history, later]} />);
    await frames();
    expect(pane().scrollTop).toBe(parked);
  });

  it('resumes following after a native downward return to the tail', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<Pane />);
    await frames();
    await commands.wheelScroll('[data-nc-drawer-scroll]', -300);
    await expect.poll(remaining).toBe(300);
    await commands.wheelScroll('[data-nc-drawer-scroll]', 4000);
    await expect.poll(remaining).toBe(0);
    await frames();
    rerender(<Pane turns={[...history, later]} />);
    await frames();
    expect(remaining()).toBeLessThanOrEqual(1);
  });

  it('honors a slow native upward gesture inside the follow tolerance', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<Pane />);
    await frames();
    await commands.wheelScroll('[data-nc-drawer-scroll]', -2);
    await expect.poll(remaining).toBe(2);
    const parked = pane().scrollTop;
    rerender(<Pane turns={[...history, later]} />);
    await frames();
    expect(pane().scrollTop).toBe(parked);
  });

  it('does not treat a programmatic position change as reader navigation', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<Pane />);
    await frames();
    pane().scrollTop -= 300;
    await frames();
    rerender(<Pane turns={[...history, later]} />);
    await frames();
    expect(remaining()).toBeLessThanOrEqual(1);
  });

  it('follows disclosure growth without a new message or text delta', async () => {
    await page.viewport(1400, 900);
    render(<Pane turns={[...history, {
      id: 'details', author: 'system', label: 'Details', text: words.repeat(15), atMs: 20000,
    }]} />);
    await frames();
    const before = pane().scrollHeight;
    await page.getByText('· Details ·', { exact: true }).click();
    await frames();
    expect(pane().scrollHeight).toBeGreaterThan(before + 100);
    expect(remaining()).toBeLessThanOrEqual(1);
  });

});


function ResizablePane({ turns }: { turns: readonly TranscriptEntry[] }) {
  const host = useRef<HTMLDivElement | null>(null);
  return <div ref={host} style={{ position: 'relative', containerType: 'inline-size', inlineSize: 900, blockSize: 650 }}>
    <Drawer open title="Reading" onClose={() => {}} resize={{
      onPreview: (rem) => { host.current?.style.setProperty('--conversation-span', rem === null ? '396px' : `${rem}rem`); },
      onCommit: () => {},
    }}>
      <ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation} turns={turns} />
    </Drawer>
  </div>;
}

describe('chat intent through the real drawer resize path', () => {
  it('keeps following through a width change and a subsequent arrival', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<ResizablePane turns={history} />);
    await frames();
    await Promise.all(document.querySelector('[data-nc-drawer]')!.getAnimations().map((a) => a.finished));
    const edge = await page.getByRole('separator', { name: 'Resize conversation' }).findElement();
    (edge as HTMLElement).focus({ preventScroll: true });
    await userEvent.keyboard('{ArrowLeft}{ArrowLeft}{ArrowLeft}');
    await frames();
    expect(remaining()).toBeLessThanOrEqual(1);
    rerender(<ResizablePane turns={[...history, later]} />);
    await frames();
    expect(remaining()).toBeLessThanOrEqual(1);
  });

  it('preserves the words being read through rewrap and does not follow the next arrival', async () => {
    await page.viewport(1400, 900);
    const text = Array.from({ length: 1000 }, (_, i) => `w${i}`).join(' ');
    const turns: readonly TranscriptEntry[] = [{ id: 'long', author: 'agent', text, atMs: 0 }, ...history];
    const { rerender } = render(<ResizablePane turns={turns} />);
    await frames();
    await Promise.all(document.querySelector('[data-nc-drawer]')!.getAnimations().map((a) => a.finished));
    const walker = document.createTreeWalker(document.querySelector('[data-nc-turn="agent"]')!, NodeFilter.SHOW_TEXT);
    let node = walker.nextNode();
    while (node !== null && !node.textContent?.includes('w350')) node = walker.nextNode();
    expect(node).not.toBeNull();
    const paragraph = node!.parentElement!;
    const range = document.createRange();
    const index = node!.textContent!.indexOf('w350');
    range.setStart(node!, index);
    range.setEnd(node!, index + 4);
    const offset = () => range.getBoundingClientRect().top - pane().getBoundingClientRect().top;
    const delta = offset() - 12;
    const beforeScroll = pane().scrollTop;
    await commands.wheelScroll('[data-nc-drawer-scroll]', delta);
    await expect.poll(() => pane().scrollTop).toBeCloseTo(beforeScroll + delta, 0);
    await frames();
    const edge = await page.getByRole('separator', { name: 'Resize conversation' }).findElement();
    (edge as HTMLElement).focus({ preventScroll: true });
    const line = parseFloat(getComputedStyle(paragraph).lineHeight) + 1;
    for (const key of ['{ArrowLeft}{ArrowLeft}{ArrowLeft}', '{ArrowRight}{ArrowRight}{ArrowRight}']) {
      const before = offset();
      await userEvent.keyboard(key);
      await frames();
      expect(Math.abs(offset() - before)).toBeLessThanOrEqual(line);
    }
    const parked = pane().scrollTop;
    rerender(<ResizablePane turns={[...turns, later]} />);
    await frames();
    expect(pane().scrollTop).toBe(parked);
  });
});

it('prefetches only on real near-top upward input and keeps short content idle', async () => {
  const load = vi.fn(() => true);
  const historyPrefetch = { active: true, cursor: 100, busy: false, failed: false, load };
  const view = render(<Pane turns={history.slice(0, 1)} historyPrefetch={historyPrefetch} />);
  await frames();
  pane().dispatchEvent(new Event('scroll')); window.dispatchEvent(new Event('resize'));
  await frames(); expect(load).not.toHaveBeenCalled();
  await commands.wheelScroll('[data-nc-drawer-scroll]', -30);
  await expect.poll(() => load.mock.calls.length).toBe(1);
  await frames(); expect(load).toHaveBeenCalledTimes(1);
  view.rerender(<Pane turns={history} historyPrefetch={{ ...historyPrefetch, cursor: 40 }} />);
  await frames(); expect(load).toHaveBeenCalledTimes(1);
  await commands.wheelScroll('[data-nc-drawer-scroll]', -100000);
  await expect.poll(() => pane().scrollTop).toBe(0);
  await commands.wheelScroll('[data-nc-drawer-scroll]', -30);
  await expect.poll(() => load.mock.calls.length).toBe(2);
});

it('retains expanded cross-page tool groups and an opened detail in the real browser', async () => {
  const tool = (id: string): TranscriptEntry => ({ id, author: 'activity', verb: 'Ran', target: id,
    state: 'failed', durationMs: null, detail: `Detail ${id}`, tool: null, atMs: 0 });
  const view = render(<Pane turns={[tool('retained-a'), tool('retained-b'), ...history]} />);
  await frames();
  await commands.wheelScroll('[data-nc-drawer-scroll]', -100000);
  await expect.poll(() => pane().scrollTop).toBe(0);
  const group = page.getByRole('group', { name: '2 tool calls' }).element();
  await userEvent.click(group.querySelector<HTMLElement>(':scope > [role="button"]')!);
  await page.getByRole('button', { name: /retained-a/ }).click();
  await expect.element(page.getByText('Detail retained-a', { exact: true })).toBeVisible();
  const detail = document.querySelector('[data-nc-detail="retained-a"]');
  view.rerender(<Pane turns={[tool('older'), tool('retained-a'), tool('retained-b'), ...history]} />);
  await frames();
  expect(page.getByRole('group', { name: '3 tool calls' }).element()).toBe(group);
  expect(document.querySelector('[data-nc-detail="retained-a"]')).toBe(detail);
});

it('keeps the top reading mark when a prepended image arrives late', async () => {
  await commands.mockChatPerformanceImage(true);
  try {
    const view = render(<Pane />); await frames();
    await commands.wheelScroll('[data-nc-drawer-scroll]', -100000);
    await expect.poll(() => pane().scrollTop).toBe(0); await frames();
    const anchor = document.querySelector('[data-nc-entry="agent-0"]')!;
    const y = () => anchor.getBoundingClientRect().top - pane().getBoundingClientRect().top;
    const before = y();
    view.rerender(<Pane turns={[{ id: 'picture', author: 'agent', text: '![Probe image](https://chat-benchmark.invalid/image.svg)', atMs: -1 }, ...history]} />);
    expect(Math.abs(y() - before)).toBeLessThanOrEqual(1);
    await expect.poll(() => document.querySelector<HTMLImageElement>('img[alt="Probe image"]')?.naturalWidth).toBeGreaterThan(0);
    await frames(); expect(Math.abs(y() - before)).toBeLessThanOrEqual(1);
  } finally { await commands.mockChatPerformanceImage(false); }
});
