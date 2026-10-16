import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { useLayoutEffect, useRef } from 'react';
import { useState } from '../state/public.ts';
import { createScrollFollower } from './follow-scroll.ts';
import { restoreDrawerFocus } from './restore-focus.ts';

function Pane({ growth = 0, restoreFocus = false }: { growth?: number; restoreFocus?: boolean }) {
  const pane = useRef<HTMLDivElement | null>(null);
  const content = useRef<HTMLDivElement | null>(null);
  const [follower] = useState(() => createScrollFollower({ bottomSlack: 64, onAwayChange: () => {} }));
  useLayoutEffect(() => follower.attach(pane.current!, content.current!), [follower]);
  useLayoutEffect(() => {
    if (restoreFocus) restoreDrawerFocus(content.current!.querySelector<HTMLAnchorElement>('a')!);
  }, [growth, restoreFocus]);
  return <>
    <button onClick={() => follower.followToEnd()}>Return to latest</button>
    <div ref={pane} data-testid="pane" data-nc-drawer-scroll="" style={{ height: 300, width: 400, overflow: 'auto' }}>
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
afterEach(cleanup);

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

it('keeps following when the owner explicitly restores focus', async () => {
  const view = render(<Pane />);
  await frames();
  restoreDrawerFocus(oldLink());
  expect(remaining()).toBeLessThanOrEqual(1);
  view.rerender(<Pane growth={400} />);
  await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
});

it('keeps following when content grows before layout-effect focus restoration', async () => {
  const view = render(<Pane />);
  await frames();
  view.rerender(<Pane growth={400} restoreFocus />);
  await frames();
  expect(document.activeElement).toBe(oldLink());
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
