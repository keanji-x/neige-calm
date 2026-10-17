import { useState } from '../state/public.ts';
import { act, cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { ListText } from './public.tsx';

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it('remeasures nested text and style changes without a parent render and releases observers', async () => {
  const long = 'A nested label that is much wider than its bounded text host';
  function Label() {
    const [expanded, setExpanded] = useState(false);
    return <button type="button" onClick={() => setExpanded(previous => !previous)}>{expanded ? long : 'Short'}</button>;
  }
  const view = render(<ListText tone="primary" fadeOverflow style={{ display: 'block', inlineSize: 180, overflow: 'hidden', whiteSpace: 'nowrap' }}><Label /></ListText>);
  const title = view.container.firstElementChild as HTMLElement;
  const mask = () => getComputedStyle(title).maskImage;
  await expect.poll(mask).toBe('none');
  await page.getByRole('button', { name: 'Short', exact: true }).click();
  await expect.poll(mask).toContain('linear-gradient');
  await page.getByRole('button', { name: long, exact: true }).click();
  await expect.poll(mask).toBe('none');
  const child = title.firstElementChild as HTMLElement;
  child.style.letterSpacing = '60px';
  await expect.poll(mask).toContain('linear-gradient');
  child.style.letterSpacing = '';
  await expect.poll(mask).toBe('none');
  title.style.inlineSize = '20px';
  await expect.poll(mask).toContain('linear-gradient');
  let detachedReads = 0;
  const nativeWidth = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollWidth')!;
  vi.spyOn(Element.prototype, 'scrollWidth', 'get').mockImplementation(function (this: Element) {
    if (this === title) detachedReads++;
    return nativeWidth.get!.call(this) as number;
  });
  view.unmount();
  // Detached descendants may still mutate; no observer may revive the host.
  child.textContent = long;
  await act(async () => { await new Promise<void>(resolve => requestAnimationFrame(() => resolve())); });
  expect(detachedReads).toBe(0);
  expect(title.isConnected).toBe(false);
});
