import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { useLayoutEffect, useRef } from 'react';
import { useState } from '../state/public.ts';
import { createScrollFollower } from './follow-scroll.ts';

function Pane({ growth = 0 }: { growth?: number }) {
  const pane = useRef<HTMLDivElement | null>(null);
  const content = useRef<HTMLDivElement | null>(null);
  const [follower] = useState(() => createScrollFollower({ bottomSlack: 64, onAwayChange: () => {} }));
  useLayoutEffect(() => follower.attach(pane.current!, content.current!), [follower]);
  return <>
    <button onClick={() => follower.followToEnd()}>Return to latest</button>
    <div ref={pane} data-testid="pane" style={{ height: 300, width: 400, overflow: 'auto' }}>
      <div ref={content}>
        <p><a href="#history">A unique historical search target</a></p>
        {Array.from({ length: 60 }, (_, index) => <p key={index}>Paragraph {index}. {'A long passage of text. '.repeat(12)}</p>)}
        <a href="#tail">Latest link</a>
        <details><summary>Latest details</summary><p style={{ height: 400 }}>The detail grew.</p></details>
        <div style={{ height: growth }} />
      </div>
    </div>
    <input aria-label="Outside composer" />
  </>;
}
const pane = () => document.querySelector<HTMLElement>('[data-testid="pane"]')!;
const oldLink = () => document.querySelector<HTMLAnchorElement>('a[href="#history"]')!;
const remaining = () => pane().scrollHeight - pane().scrollTop - pane().clientHeight;
const visible = (element: Element) => element.getBoundingClientRect().bottom > pane().getBoundingClientRect().top
  && element.getBoundingClientRect().top < pane().getBoundingClientRect().bottom;
async function frames() {
  for (let index = 0; index < 4; index++) await act(async () => {
    await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  });
}
afterEach(() => { cleanup(); window.getSelection()?.removeAllRanges(); });

it('keeps a natively focused history link visible when content grows', async () => {
  const view = render(<Pane />);
  await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
  oldLink().focus();
  await frames();
  expect(visible(oldLink())).toBe(true);
  view.rerender(<Pane growth={400} />);
  await frames();
  expect(document.activeElement).toBe(oldLink());
  expect(visible(oldLink())).toBe(true);
  await page.getByRole('button', { name: 'Return to latest' }).click();
  await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
});

it('keeps a selected history range in view when content grows', async () => {
  const view = render(<Pane />);
  await frames();
  expect((window as unknown as Window & { find: (text: string) => boolean }).find('A unique historical search target')).toBe(true);
  // window.find selects in Chromium; revealing that range is a separate native
  // operation. This exercises selection navigation, not browser-chrome UI.
  oldLink().scrollIntoView({ block: 'center' });
  await frames();
  expect(visible(oldLink())).toBe(true);
  view.rerender(<Pane growth={400} />);
  await frames();
  expect(window.getSelection()!.toString()).toBe('A unique historical search target');
  expect(visible(oldLink())).toBe(true);
});

it('keeps following when focus restoration explicitly prevents scrolling', async () => {
  const view = render(<Pane />);
  await frames();
  oldLink().focus({ preventScroll: true });
  expect(remaining()).toBeLessThanOrEqual(1);
  view.rerender(<Pane growth={400} />);
  await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
});

it('keeps following after focusing a tail disclosure or the outside composer', async () => {
  const view = render(<Pane />);
  await frames();
  await page.getByText('Latest details', { exact: true }).click();
  await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
  await page.getByRole('textbox', { name: 'Outside composer' }).click();
  view.rerender(<Pane growth={400} />);
  await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
});
