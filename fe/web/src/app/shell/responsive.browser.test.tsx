import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../styles/entry.css';
import styles from './shell.module.css';

function ShellProbe({ state }: { state: 'auto' | 'expanded' | 'collapsed' }) {
  const modifier = state === 'expanded' ? styles.shellExpanded
    : state === 'collapsed' ? styles.shellCollapsed : '';
  return <div data-testid={state} className={`${styles.shell} ${modifier}`}><aside /><main /></div>;
}

function MobileNavigationProbe() {
  return <div className={styles.shell}>
    <div className={`${styles.navigation} ${styles.navigationOpen}`}>
      <div data-testid="mobile-panel" className={styles.navigationPanel}>
        <section data-testid="coves-list" style={{ inlineSize: '100%' }}>Coves</section>
      </div>
    </div>
    <main />
    <nav data-testid="dock" className={styles.mobileDock} aria-label="Primary">
      <div className={styles.mobileDockItem}>Pages</div>
      {['Today', 'Coves', 'Me'].map((label) => <button key={label} type="button" className={styles.mobileDockItem}>{label}</button>)}
    </nav>
  </div>;
}

afterEach(() => { document.body.replaceChildren(); });

describe('responsive shell layout', () => {
  it('uses one content column below the compact boundary and preserves desktop rail choices above it', async () => {
    render(<><ShellProbe state="auto" /><ShellProbe state="expanded" /><ShellProbe state="collapsed" /></>);

    await page.viewport(900, 700);
    expect(getComputedStyle(document.querySelector('[data-testid="auto"]')!).gridTemplateColumns).toBe('900px');
    expect(getComputedStyle(document.querySelector('[data-testid="expanded"]')!).gridTemplateColumns).toBe('900px');
    expect(getComputedStyle(document.querySelector('[data-testid="collapsed"]')!).gridTemplateColumns).toBe('900px');

    await page.viewport(390, 844);
    expect(getComputedStyle(document.querySelector('[data-testid="auto"]')!).gridTemplateColumns).toBe('390px');

    await page.viewport(1400, 900);
    expect(getComputedStyle(document.querySelector('[data-testid="auto"]')!).gridTemplateColumns).toBe('200px 1200px');
    expect(getComputedStyle(document.querySelector('[data-testid="expanded"]')!).gridTemplateColumns).toBe('200px 1200px');
    expect(getComputedStyle(document.querySelector('[data-testid="collapsed"]')!).gridTemplateColumns).toBe('44px 1356px');
  });

  it('gives the Coves list page the viewport above the persistent dock', async () => {
    render(<MobileNavigationProbe />);
    await page.viewport(390, 844);
    const list = document.querySelector('[data-testid="coves-list"]')!.getBoundingClientRect();
    const panel = document.querySelector('[data-testid="mobile-panel"]')!.getBoundingClientRect();
    const dock = document.querySelector('[data-testid="dock"]')!.getBoundingClientRect();
    expect(panel.left).toBe(0);
    expect(panel.right).toBe(window.innerWidth);
    expect(panel.width).toBe(list.width);
    expect(panel.bottom).toBe(dock.top);
    expect(dock.left).toBeGreaterThan(0);
    expect(dock.right).toBeLessThan(window.innerWidth);
    expect(dock.bottom).toBeLessThan(window.innerHeight);
  });
});
