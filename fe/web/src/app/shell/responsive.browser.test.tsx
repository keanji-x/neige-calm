import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../styles/tokens.css';
import styles from './shell.module.css';

function ShellProbe({ state }: { state: 'auto' | 'expanded' | 'collapsed' }) {
  const modifier = state === 'expanded' ? styles.shellExpanded
    : state === 'collapsed' ? styles.shellCollapsed : '';
  return <div data-testid={state} className={`${styles.shell} ${modifier}`}><aside /><main /></div>;
}

function MenuProbe() {
  return <div className={styles.shell}><aside data-testid="rail" className={`${styles.rail} ${styles.railCollapsed}`}>
    <div className={styles.menuWrap}><div data-testid="menu" className={styles.menu}>Account menu</div></div>
  </aside><main /></div>;
}

afterEach(() => { document.body.replaceChildren(); });

describe('responsive shell layout', () => {
  it('computes the auto and both explicit rail widths at narrow and wide viewports', async () => {
    render(<><ShellProbe state="auto" /><ShellProbe state="expanded" /><ShellProbe state="collapsed" /></>);

    await page.viewport(900, 700);
    expect(getComputedStyle(document.querySelector('[data-testid="auto"]')!).gridTemplateColumns).toBe('44px 856px');
    expect(getComputedStyle(document.querySelector('[data-testid="expanded"]')!).gridTemplateColumns).toBe('200px 700px');
    expect(getComputedStyle(document.querySelector('[data-testid="collapsed"]')!).gridTemplateColumns).toBe('44px 856px');

    await page.viewport(1400, 900);
    expect(getComputedStyle(document.querySelector('[data-testid="auto"]')!).gridTemplateColumns).toBe('200px 1200px');
    expect(getComputedStyle(document.querySelector('[data-testid="expanded"]')!).gridTemplateColumns).toBe('200px 1200px');
    expect(getComputedStyle(document.querySelector('[data-testid="collapsed"]')!).gridTemplateColumns).toBe('44px 1356px');
  });

  it('keeps the account menu outside the narrow rail clipping edge', async () => {
    render(<MenuProbe />);
    await page.viewport(900, 700);
    const rail = document.querySelector('[data-testid="rail"]')!.getBoundingClientRect();
    const menu = document.querySelector('[data-testid="menu"]')!.getBoundingClientRect();
    expect(menu.top).toBeGreaterThanOrEqual(rail.top);
    expect(menu.left).toBeGreaterThanOrEqual(rail.left);
    expect(menu.right).toBeGreaterThan(rail.right);
    expect(menu.right).toBeLessThanOrEqual(window.innerWidth);
    expect(menu.bottom).toBeLessThan(rail.bottom);
  });
});
