import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page } from 'vitest/browser';
import '../../../styles/entry.css';
import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
import { scrollGeometry } from '../../../ui/drawer/scroll-geometry.ts';
import { Drawer } from '../../../ui/drawer/public.tsx';
import { ChatThread } from './public.tsx';

const conversation: Conversation = { provider: 'codex', id: 'hotspots', trackId: 'track', title: 'Hotspots', kind: 'codex', state: 'idle', updatedAt: 1 };
function turns(count: number): ConversationTurn[] {
  return Array.from({ length: count }, (_, index) => [
    { id: `you-${index}`, author: 'you' as const, text: `Question ${index}`, atMs: 1_760_000_000_000 + index * 2 },
    { id: `agent-${index}`, author: 'agent' as const, text: `Reply ${index}. A paragraph of ordinary stored text.`, atMs: 1_760_000_000_001 + index * 2 },
  ]).flat();
}
async function frames(count = 4) {
  for (let i = 0; i < count; i++) await act(async () => {
    await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  });
}
afterEach(async () => { cleanup(); vi.restoreAllMocks(); await commands.emulateReducedMotion(false); await page.viewport(1280, 800); });

it('finds the active exchange without measuring every earlier marker on scroll', async () => {
  await page.viewport(1400, 900);
  render(<div style={{ height: 600, width: 900, position: 'relative' }}>
    <Drawer open title="Hotspots" onClose={() => {}}><ChatThread conversation={conversation}
      turns={turns(256)} cards={{}} stalled={false} canContinue={false} /></Drawer>
  </div>);
  for (const animation of document.querySelector<HTMLElement>('[data-nc-drawer]')!.getAnimations()) { animation.pause(); animation.currentTime = 0; }
  await frames();
  const scroller = document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  const marker = document.querySelector<HTMLElement>('[data-nc-exchange="you-200"]')!;
  const top = scroller.scrollTop + scrollGeometry(scroller).y(marker.getBoundingClientRect().top);
  let reads = 0;
  const native = Reflect.get(HTMLElement.prototype, 'getBoundingClientRect');
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
    if (this.hasAttribute('data-nc-exchange')) reads++;
    return native.call(this);
  });
  act(() => {
    fireEvent.wheel(scroller, { deltaY: -1 });
    scroller.scrollTop = top;
    fireEvent.scroll(scroller);
  });
  await frames();
  expect(document.querySelector('[data-nc-rail-track] [aria-current="true"]')?.getAttribute('aria-label')).toContain('Question 200');
  // A deterministic work budget, not a machine-speed threshold.
  expect(reads).toBeLessThanOrEqual(64);
});

it('updates the exchange rail without rereading unchanged history or disturbing native selection', async () => {
  await page.viewport(1400, 900);
  const history = turns(450);
  let reads = 0;
  const text = history[0].text;
  Object.defineProperty(history[0], 'text', { get: () => { reads++; return text; }, enumerable: true });
  render(<div style={{ height: 600, width: 900, position: 'relative' }}>
    <Drawer open title="Hotspots" onClose={() => {}}><ChatThread conversation={conversation}
      turns={history} cards={{}} stalled={false} canContinue={false} /></Drawer>
  </div>);
  for (const animation of document.querySelector<HTMLElement>('[data-nc-drawer]')!.getAnimations()) { animation.pause(); animation.currentTime = 0; }
  await frames();
  const scroller = document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  const scrollTo = async (id: string) => {
    const marker = [...document.querySelectorAll<HTMLElement>('[data-nc-exchange]')]
      .find(candidate => candidate.dataset.ncExchange === id)!;
    act(() => {
      fireEvent.wheel(scroller, { deltaY: -1 });
      scroller.scrollTop += scrollGeometry(scroller).y(marker.getBoundingClientRect().top);
      fireEvent.scroll(scroller);
    });
    await frames();
  };
  // Settle the independent transition from following to reading earlier text.
  await scrollTo('you-100');
  const entries = [...document.querySelectorAll('[data-nc-entry]')];
  expect(entries).toHaveLength(900);
  const question = document.createTreeWalker(entries[0], NodeFilter.SHOW_TEXT).nextNode()!;
  const answer = document.createTreeWalker(entries[1], NodeFilter.SHOW_TEXT).nextNode()!;
  const range = document.createRange();
  range.setStart(question, 0); range.setEnd(answer, 5);
  const selection = window.getSelection()!;
  selection.removeAllRanges(); selection.addRange(range);
  const selectedText = selection.toString();
  expect(selectedText).toContain('Question 0');
  reads = 0;
  act(() => { document.querySelector<HTMLElement>('[data-nc-rail-track] [aria-label="Jump to exchange 201: Question 200"]')!.click(); });
  await frames();
  expect(document.querySelector('[data-nc-rail-track] [aria-current="true"]')?.getAttribute('aria-label')).toContain('Question 200');
  const currentEntries = [...document.querySelectorAll('[data-nc-entry]')];
  expect(currentEntries).toHaveLength(entries.length);
  for (const [index, entry] of currentEntries.entries()) expect(entry).toBe(entries[index]);
  expect(selection.toString()).toBe(selectedText);
  expect(selection.anchorNode).toBe(question);
  expect(selection.focusNode).toBe(answer);
  expect(reads).toBe(0);
});

it.each(['entry 0', 'entry 40', 'entry 100', 'reversal', 'rest', 'reduced'])(
  'aligns real rail jumps at %s', async phase => {
  await page.viewport(1400, 900);
  if (phase === 'reduced') await commands.emulateReducedMotion(true);
  const history = turns(450);
  const content = (open: boolean) => <div style={{ height: 600, width: 900, position: 'relative' }}>
    <Drawer open={open} title="Hotspots" onClose={() => {}}><ChatThread conversation={conversation}
      turns={history} cards={{}} stalled={false} canContinue={false} /></Drawer>
  </div>;
  const view = render(content(true));
  const panel = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
  const sample = (time: number) => {
    for (const surface of [panel, document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!]) {
      for (const animation of surface.getAnimations()) { animation.pause(); animation.currentTime = time; }
    }
  };
  if (phase === 'rest') {
    await expect.poll(() => panel.getAnimations().length).toBe(0);
  } else if (phase === 'reversal') {
    sample(40);
    view.rerender(content(false)); sample(40);
    view.rerender(content(true)); sample(40);
  } else if (phase.startsWith('entry')) sample(Number(phase.split(' ')[1]));
  await frames();
  const scroller = document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  const markers = [...scroller.querySelectorAll<HTMLElement>('[data-nc-exchange]')];
  const rail = document.querySelector<HTMLElement>('[data-nc-rail-track]')!;
  const dots = [...rail.querySelectorAll<HTMLElement>('button')];
  for (const index of [100, 200, 448, 449, 0, 200]) {
    const marker = markers.find(candidate => candidate.dataset.ncExchange === `you-${index}`)!;
    const dot = dots.find(candidate => candidate.getAttribute('aria-label') === `Jump to exchange ${index + 1}: Question ${index}`)!;
    act(() => { dot.click(); });
    await frames();
    const pane = scroller.getBoundingClientRect();
    const target = marker.getBoundingClientRect();
    // A target near the tail clamps to the scroll extent, but remains visible.
    const remaining = scroller.scrollHeight - scroller.clientHeight - scroller.scrollTop;
    if (remaining > 1) expect(Math.abs(target.top - pane.top)).toBeLessThanOrEqual(1);
    else { expect(target.top).toBeGreaterThanOrEqual(pane.top); expect(target.top).toBeLessThan(pane.bottom); }
    expect(document.querySelector('[data-nc-rail-track] [aria-current="true"]')?.getAttribute('aria-label')).toBe(remaining <= 1 ? 'Jump to exchange 450: Question 449' : `Jump to exchange ${index + 1}: Question ${index}`);
    const track = dot.closest<HTMLElement>('[data-nc-rail-track]')!.getBoundingClientRect();
    expect(dot.getBoundingClientRect().top).toBeGreaterThanOrEqual(track.top - 1);
    expect(dot.getBoundingClientRect().bottom).toBeLessThanOrEqual(track.bottom + 1);
  }
});

it('keeps the sliding active edge in layout pixels near the end during entry', async () => {
  await page.viewport(1400, 900);
  render(<div style={{ height: 600, width: 900, position: 'relative' }}>
    <Drawer open title="Hotspots" onClose={() => {}}><ChatThread conversation={conversation}
      turns={turns(450)} cards={{}} stalled={false} canContinue={false} /></Drawer>
  </div>);
  for (const animation of document.querySelector<HTMLElement>('[data-nc-drawer]')!.getAnimations()) {
    animation.pause(); animation.currentTime = 0;
  }
  await frames();
  const pane = document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  const marker = document.querySelector<HTMLElement>('[data-nc-exchange="you-445"]')!;
  act(() => { fireEvent.wheel(pane, { deltaY: -1 }); pane.scrollTop = 0; });
  const position = scrollGeometry(pane).y(marker.getBoundingClientRect().top);
  const maximum = pane.scrollHeight - pane.clientHeight;
  // Put this marker immediately above the declared sliding edge: as scroll
  // advances one pixel, that edge advances two through the final pane-height.
  act(() => {
    pane.scrollTop = Math.ceil((position + maximum - pane.clientHeight - 4) / 2);
    fireEvent.scroll(pane);
  });
  expect(maximum - pane.scrollTop).toBeGreaterThan(100);
  expect(maximum - pane.scrollTop).toBeLessThan(pane.clientHeight);
  await frames();
  expect(document.querySelector('[data-nc-rail-track] [aria-current="true"]')?.getAttribute('aria-label')).toBe('Jump to exchange 446: Question 445');
});

it('formats compact timestamps without creating a locale formatter for each reply', async () => {
  await page.viewport(390, 900);
  const history = turns(64);
  const expected = history.filter(turn => turn.author === 'agent').map(turn => new Date(turn.atMs).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false }));
  const expensive = vi.spyOn(Date.prototype, 'toLocaleTimeString');
  const NativeFormatter = Intl.DateTimeFormat;
  const constructors = vi.spyOn(Intl, 'DateTimeFormat').mockImplementation(function (locales, options) {
    return new NativeFormatter(locales, options);
  });
  render(<ChatThread conversation={conversation} turns={history} cards={{}} stalled={false} canContinue={false} />);
  await frames();
  expect([...document.querySelectorAll('time')].map(time => time.textContent)).toEqual(expected);
  expect(expensive.mock.calls.length).toBeLessThanOrEqual(1);
  expect(constructors.mock.calls.length + expensive.mock.calls.length).toBeLessThanOrEqual(4);
});

it('keeps stored compact messages memoized when only the last reply grows', async () => {
  await page.viewport(390, 900);
  const history = turns(3);
  const view = render(<ChatThread conversation={conversation} turns={history} cards={{}} stalled={false} canContinue={false} />);
  await frames();
  const storedTime = document.querySelector('time');
  const renderDates = vi.spyOn(Date.prototype, 'toISOString');
  view.rerender(<ChatThread conversation={conversation} turns={history.map((turn, index) => index === history.length - 1 ? { ...turn, text: 'The last reply grows.' } : { ...turn })} cards={{}} stalled={false} canContinue={false} />);
  await frames();
  expect(document.querySelector('time')).toBe(storedTime);
  expect(renderDates).toHaveBeenCalledTimes(1);
});

it('preserves local gap labels and invalid-date display with shared formatters', async () => {
  await page.viewport(1400, 900);
  const history = turns(3).map((turn, index) => ({ ...turn, atMs: 1_760_000_000_000 + index * 3_600_000 }));
  const expected = history.slice(1).map(turn => new Date(turn.atMs).toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' }));
  const view = render(<ChatThread conversation={conversation} turns={history} cards={{}} stalled={false} canContinue={false} />);
  await frames();
  for (const label of expected) expect(document.body.textContent).toContain(label);
  expect(document.querySelectorAll('time')).toHaveLength(0);
  view.rerender(<ChatThread conversation={conversation} turns={[
    { ...history[0], atMs: 0 }, { ...history[1], atMs: 10_000_000_000_000_000 },
  ]} cards={{}} stalled={false} canContinue={false} />);
  expect(document.body.textContent).toContain('Invalid Date');
});

it('does not reformat stored compact history when only the last reply grows', async () => {
  await page.viewport(390, 900);
  const NativeFormatter = Intl.DateTimeFormat;
  let formats = 0;
  vi.spyOn(Intl, 'DateTimeFormat').mockImplementation(function (locales, options) {
    const formatter = new NativeFormatter(locales, options);
    const format = formatter.format.bind(formatter);
    Object.defineProperty(formatter, 'format', { value: (value: number | Date | undefined) => { formats++; return format(value); } });
    return formatter;
  });
  const history = turns(64);
  const view = render(<ChatThread conversation={conversation} turns={history} cards={{}} stalled={false} canContinue={false} />);
  await frames(); formats = 0;
  view.rerender(<ChatThread conversation={conversation} turns={history.map((turn, index) => index === history.length - 1 ? { ...turn, text: 'Only this reply changes.' } : { ...turn })} cards={{}} stalled={false} canContinue={false} />);
  await frames();
  expect(formats).toBeLessThanOrEqual(1);
});

it('invalidates compact clocks when the browser timezone changes on a later render', async () => {
  await page.viewport(390, 900);
  const NativeFormatter = Intl.DateTimeFormat;
  let timeZone = 'UTC';
  vi.spyOn(Intl, 'DateTimeFormat').mockImplementation(function (locales, options) {
    const formatter = new NativeFormatter(locales, options);
    if (locales === undefined && options === undefined) {
      const resolved = formatter.resolvedOptions();
      Object.defineProperty(formatter, 'resolvedOptions', { value: () => ({ ...resolved, timeZone }) });
    }
    return formatter;
  });
  const history = turns(2);
  const expected = (zone: string) => history.filter(turn => turn.author === 'agent').map(turn => new NativeFormatter('zh-CN', {
    hour: '2-digit', minute: '2-digit', hour12: false, timeZone: zone,
  }).format(turn.atMs));
  const labels = () => [...document.querySelectorAll('time')].map(time => time.textContent);
  const view = render(<ChatThread conversation={conversation} turns={history} cards={{}} stalled={false} canContinue={false} />);
  await frames(); expect(labels()).toEqual(expected('UTC'));
  timeZone = 'America/New_York';
  view.rerender(<ChatThread conversation={conversation} turns={[...history]} cards={{}} stalled={false} canContinue={false} />);
  await frames(); expect(labels()).toEqual(expected(timeZone));
  await page.viewport(1400, 900); await frames(); expect(labels()).toEqual([]);
  await page.viewport(390, 900); await frames(); expect(labels()).toEqual(expected(timeZone));
});
