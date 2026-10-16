import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import '../../../styles/entry.css';
import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
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
afterEach(async () => { cleanup(); vi.restoreAllMocks(); await page.viewport(1280, 800); });

it('finds the active exchange without measuring every earlier marker on scroll', async () => {
  await page.viewport(1400, 900);
  render(<div style={{ height: 600, width: 900, position: 'relative' }}>
    <Drawer open title="Hotspots" onClose={() => {}}><ChatThread conversation={conversation}
      turns={turns(256)} cards={{}} stalled={false} canContinue={false} /></Drawer>
  </div>);
  await frames();
  const scroller = document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  const marker = document.querySelector<HTMLElement>('[data-nc-exchange="you-200"]')!;
  const top = scroller.scrollTop + marker.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
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
