import '../../styles/entry.css';
import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { commands, page } from 'vitest/browser';
import { HoverPreview } from './public.tsx';
import { notifyAncestorVisualChange } from './visual-change.ts';

const frames = async (n = 5) => { for (let i = 0; i < n; i++) await new Promise(requestAnimationFrame); };
afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });

it('proves transform displacement does not notify ResizeObserver', async () => {
  const element = document.createElement('div');
  Object.assign(element.style, { width: '40px', height: '40px' });
  document.body.append(element);
  let calls = 0;
  const observer = new ResizeObserver(() => { calls++; }); observer.observe(element);
  try {
    await frames(); expect(calls).toBeGreaterThan(0);
    const before = calls; const top = element.getBoundingClientRect().top;
    element.style.transform = 'translateY(60px)'; await frames();
    expect(element.getBoundingClientRect().top - top).toBe(60);
    expect(calls).toBe(before);
  } finally { observer.disconnect(); element.remove(); }
});

it('invalidates only admitted ancestor dependencies and propagates through the existing nested port', async () => {
  await page.viewport(1440, 1000); await commands.emulateReducedMotion(true);
  const activate: (() => void)[] = [];
  const view = render(<><div data-testid="panel" style={{ position: 'fixed', left: 20, top: 100 }}>
    <HoverPreview title="moving" trigger={open => { activate[0] = open; return <button>moving</button>; }}>
      <HoverPreview title="nested" trigger={open => { activate[1] = open; return <button>nested</button>; }}>Nested content</HoverPreview>
    </HoverPreview></div><div data-testid="footer" style={{ position: 'fixed', left: 20, top: 700 }}>
      <HoverPreview title="stationary" trigger={open => { activate[2] = open; return <button>stationary</button>; }}>Footer content</HoverPreview>
    </div></>);
  for (const open of activate) { act(open); await frames(); }
  const moving = await page.getByRole('dialog', { name: 'Preview: moving' }).findElement();
  const nested = await page.getByRole('dialog', { name: 'Preview: nested' }).findElement();
  const stationary = await page.getByRole('dialog', { name: 'Preview: stationary' }).findElement();
  const panel = view.getByTestId('panel');
  const tops = [moving, nested, stationary].map(node => node.getBoundingClientRect().top);
  panel.style.transform = 'translateY(80px)'; await frames();
  expect(moving.getBoundingClientRect().top).toBe(tops[0]);
  act(() => notifyAncestorVisualChange(panel)); await frames();
  expect(moving.getBoundingClientRect().top - tops[0]).toBe(80);
  expect(nested.getBoundingClientRect().top - tops[1]).toBe(80);
  expect(stationary.getBoundingClientRect().top).toBe(tops[2]);
  panel.inert = true; await frames(); panel.inert = false;
  act(() => notifyAncestorVisualChange(panel)); await frames();
  expect(moving.hasAttribute('data-nc-link-preview')).toBe(false);
  expect(nested.hasAttribute('data-nc-link-preview')).toBe(false);
  view.unmount(); act(() => notifyAncestorVisualChange(panel)); await frames();
  expect(document.querySelector('[data-nc-link-preview]')).toBeNull();
});
