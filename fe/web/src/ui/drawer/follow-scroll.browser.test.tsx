import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { restoreDrawerFocus } from './restore-focus.ts';
import { Pane } from './follow-scroll-fixture.tsx';


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
