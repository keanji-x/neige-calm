import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import '../../styles/entry.css';
import { mobileFontClassName } from './public.ts';

afterEach(cleanup);

it('resolves roles in the explicit font scope while preserving the desktop stack', async () => {
  render(<><span data-testid="desktop" style={{ font: 'var(--type-ui)' }}>Desktop</span>
    <div className={mobileFontClassName} data-testid="scope">
      <span data-testid="mobile" style={{ font: 'var(--type-ui)' }}>Mobile 中文</span>
      <span data-testid="family" style={{ fontFamily: 'var(--font-sans)' }}>Family 中文</span>
    </div></>);
  const desktop = document.querySelector<HTMLElement>('[data-testid="desktop"]')!;
  const scope = document.querySelector<HTMLElement>('[data-testid="scope"]')!;
  const mobile = document.querySelector<HTMLElement>('[data-testid="mobile"]')!;
  const family = document.querySelector<HTMLElement>('[data-testid="family"]')!;
  await document.fonts.load('14px "Neige Mobile Roboto"');
  expect(document.fonts.check('14px "Neige Mobile Roboto"')).toBe(true);
  expect(getComputedStyle(family).fontFamily).toContain('Neige Mobile Roboto');
  const desktopFamily = getComputedStyle(desktop).fontFamily;
  expect(getComputedStyle(mobile).fontFamily).toBe(getComputedStyle(family).fontFamily);
  scope.style.setProperty('--font-sans', 'monospace');
  expect(getComputedStyle(mobile).fontFamily).toBe('monospace');
  expect(getComputedStyle(desktop).fontFamily).toBe(desktopFamily);
});
