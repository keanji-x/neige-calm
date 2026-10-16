import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { commands } from 'vitest/browser';
import { Pane } from './follow-scroll-fixture.tsx';
import { restoreDrawerFocus } from './restore-focus.ts';
import { withScrollRestoration } from './follow-scroll.ts';

const pane = (name = 'pane') => [...document.querySelectorAll<HTMLElement>('[data-testid]')].find(element => element.dataset.testid === name)!;
const link = (name = 'pane', tail = false) => tail
  ? pane(name).querySelector<HTMLAnchorElement>('a[href="#tail"]')!
  : pane(name).querySelector<HTMLAnchorElement>('a[href="#history"]')!;
const remaining = (name = 'pane') => pane(name).scrollHeight - pane(name).scrollTop - pane(name).clientHeight;
const visible = (name = 'pane') => link(name).getBoundingClientRect().bottom > pane(name).getBoundingClientRect().top
  && link(name).getBoundingClientRect().top < pane(name).getBoundingClientRect().bottom;
function select(element: Element, collapsed = false) {
  const range = document.createRange(); range.selectNodeContents(element);
  if (collapsed) range.collapse(true);
  const selection = window.getSelection()!; selection.removeAllRanges(); selection.addRange(range);
}
function end(name = 'pane') {
  (pane(name).previousElementSibling as HTMLButtonElement).click();
}
async function frames() {
  for (let index = 0; index < 4; index++) await act(async () => {
    await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  });
}
afterEach(() => { cleanup(); window.getSelection()?.removeAllRanges(); });

it('preserves selected history when ordinary content grows', async () => {
  const view = render(<Pane />); await frames();
  select(link()); link().scrollIntoView({ block: 'center' }); await frames();
  expect(visible()).toBe(true);
  view.rerender(<Pane growth={400} />); await frames();
  expect(visible()).toBe(true);
});

it('treats selected tail text as reading before the next resize', async () => {
  const view = render(<Pane />); await frames();
  const top = pane().scrollTop;
  select(link('pane', true));
  view.rerender(<Pane growth={400} />); await frames();
  expect(pane().scrollTop).toBe(top);
  expect(remaining()).toBeGreaterThan(300);
});

it('lets explicit return win over a still-held selection and its queued event', async () => {
  const view = render(<Pane />); await frames();
  select(link()); link().scrollIntoView({ block: 'center' });
  end();
  view.rerender(<Pane growth={400} />); await frames();
  expect(window.getSelection()!.isCollapsed).toBe(false);
  expect(remaining()).toBeLessThanOrEqual(1);
});

it('rearms after an observed clear and protects a newly selected extent', async () => {
  const view = render(<Pane />); await frames();
  select(link()); link().scrollIntoView({ block: 'center' }); await frames();
  end(); await frames();
  window.getSelection()!.removeAllRanges(); await frames();
  select(link()); link().scrollIntoView({ block: 'center' }); await frames();
  view.rerender(<Pane growth={400} />); await frames();
  expect(visible()).toBe(true);
});

it('acknowledges restoration-created selection before its real queued delivery', async () => {
  const view = render(<Pane />); await frames();
  withScrollRestoration([pane()], () => { select(link()); link().scrollIntoView({ block: 'center' }); });
  view.rerender(<Pane growth={400} />); await frames();
  expect(window.getSelection()!.isCollapsed).toBe(false);
  expect(remaining()).toBeLessThanOrEqual(1);
});

it('does not let restoration exit overwrite an explicit return made inside it', async () => {
  const view = render(<Pane />); await frames();
  select(link()); link().scrollIntoView({ block: 'center' }); await frames();
  withScrollRestoration([pane()], () => {
    end();
    withScrollRestoration([pane()], () => select(link('pane', true)));
  });
  view.rerender(<Pane growth={400} />); await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
});

it('does not reinterpret direction-only changes as a new selected extent', async () => {
  const view = render(<Pane />); await frames();
  select(link()); await frames();
  end(); await frames();
  const selected = window.getSelection()!.getRangeAt(0);
  // Reverse the same DOM points; selecting a different container is a new extent.
  window.getSelection()!.setBaseAndExtent(selected.endContainer, selected.endOffset, selected.startContainer, selected.startOffset);
  view.rerender(<Pane growth={400} />); await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
});

it('reconciles a pending reader selection before an unrelated restoration can acknowledge it', async () => {
  const view = render(<Pane />); await frames();
  select(link()); link().scrollIntoView({ block: 'center' });
  restoreDrawerFocus(link('pane', true));
  view.rerender(<Pane growth={400} />); await frames();
  expect(visible()).toBe(true);
});

it('reads existing selection before the initial attachment writes the tail', async () => {
  render(<Pane selectAtAttach />); await frames();
  expect(visible()).toBe(true);
});

it('does not let collapsed or outside selection cancel following', async () => {
  const view = render(<><Pane /><p data-testid="outside">Other content</p></>); await frames();
  select(document.querySelector('[data-testid="outside"]')!);
  view.rerender(<><Pane growth={200} /><p data-testid="outside">Other content</p></>); await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
  select(link(), true);
  view.rerender(<><Pane growth={400} /><p data-testid="outside">Other content</p></>); await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
});

it('keeps a sibling pane following while this pane retains selected history', async () => {
  const view = render(<><Pane name="a" /><Pane name="b" /></>); await frames();
  select(link('b')); link('b').scrollIntoView({ block: 'center' }); await frames();
  view.rerender(<><Pane name="a" growth={400} /><Pane name="b" growth={400} /></>); await frames();
  expect(remaining('a')).toBeLessThanOrEqual(1);
  expect(visible('b')).toBe(true);
});

it('compares local extent when a cross-pane range only moves its other endpoint', async () => {
  const view = render(<><Pane name="a" /><Pane name="b" /></>); await frames();
  const selection = window.getSelection()!;
  selection.setBaseAndExtent(link('a').firstChild!, 0, link('b').firstChild!, 3); await frames();
  end('a'); await frames();
  selection.setBaseAndExtent(link('a').firstChild!, 0, link('b').firstChild!, 8); await frames();
  view.rerender(<><Pane name="a" growth={400} /><Pane name="b" /></>); await frames();
  expect(remaining('a')).toBeLessThanOrEqual(1);
});

it('resumes after a native downward return while the old selection remains held', async () => {
  const view = render(<Pane />); await frames();
  select(link()); link().scrollIntoView({ block: 'center' }); await frames();
  await commands.wheelScroll('[data-testid="pane"]', 20000);
  await expect.poll(remaining).toBeLessThanOrEqual(1); await frames();
  view.rerender(<Pane growth={400} />); await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
});

it.each(['start', 'end'] as const)('keeps return acknowledged when an outside %s moves to the exact content edge', async edge => {
  const view = render(<><Pane name="a" /><Pane name="b" /></>); await frames();
  const selection = window.getSelection()!;
  selection.setBaseAndExtent(link('a').firstChild!, 0, link('b').firstChild!, 8); await frames();
  const name = edge === 'start' ? 'b' : 'a';
  const content = pane(name).firstElementChild!;
  end(name); await frames();
  const range = selection.getRangeAt(0).cloneRange();
  if (edge === 'start') range.setStart(content, 0);
  else range.setEnd(content, content.childNodes.length);
  selection.removeAllRanges(); selection.addRange(range); await frames();
  view.rerender(<><Pane name="a" growth={400} /><Pane name="b" growth={400} /></>); await frames();
  expect(remaining(name)).toBeLessThanOrEqual(1);
  // Retain the newly observed internal point when children are appended later.
  const addition = document.createElement('div'); addition.style.height = '400px';
  content.append(addition); await frames();
  expect(remaining(name)).toBeLessThanOrEqual(1);
});

it('retains an acknowledged internal endpoint when its old content edge becomes interior', async () => {
  render(<Pane />); await frames();
  const content = pane().firstElementChild!;
  const range = document.createRange(); range.setStart(link().firstChild!, 0); range.setEnd(content, content.childNodes.length);
  const selection = window.getSelection()!; selection.removeAllRanges(); selection.addRange(range); await frames();
  end(); await frames();
  const addition = document.createElement('div'); addition.style.height = '400px';
  content.append(addition); await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
});

it('pauses again when the acknowledged local endpoint actually moves inside the pane', async () => {
  const view = render(<><Pane name="a" /><Pane name="b" /></>); await frames();
  const selection = window.getSelection()!;
  selection.setBaseAndExtent(link('a').firstChild!, 0, link('b').firstChild!, 8); await frames();
  end('a'); await frames();
  const top = pane('a').scrollTop;
  selection.setBaseAndExtent(link('a').firstChild!, 0, link('a').firstChild!, 8);
  view.rerender(<><Pane name="a" growth={400} /><Pane name="b" /></>); await frames();
  expect(pane('a').scrollTop).toBe(top);
  expect(remaining('a')).toBeGreaterThan(300);
});

it('pauses when an internal endpoint expands outside after same-task child insertion', async () => {
  const view = render(<><Pane name="a" /><Pane name="b" /></>); await frames();
  const content = pane('a').firstElementChild!;
  const range = document.createRange(); range.setStart(link('a').firstChild!, 0); range.setEnd(content, content.childNodes.length);
  const selection = window.getSelection()!; selection.removeAllRanges(); selection.addRange(range); await frames();
  end('a'); await frames();
  const top = pane('a').scrollTop;
  const addition = document.createElement('div'); addition.style.height = '400px';
  content.append(addition);
  selection.setBaseAndExtent(link('a').firstChild!, 0, link('b').firstChild!, 8);
  view.rerender(<><Pane name="a" growth={400} /><Pane name="b" /></>); await frames();
  expect(pane('a').scrollTop).toBe(top);
  expect(remaining('a')).toBeGreaterThan(700);
});

it('retains an acknowledged external endpoint as the content grows', async () => {
  render(<><Pane name="a" /><Pane name="b" /></>); await frames();
  const selection = window.getSelection()!;
  selection.setBaseAndExtent(link('a').firstChild!, 0, link('b').firstChild!, 8); await frames();
  end('a'); await frames();
  const addition = document.createElement('div'); addition.style.height = '400px';
  pane('a').firstElementChild!.append(addition); await frames();
  expect(remaining('a')).toBeLessThanOrEqual(1);
});
