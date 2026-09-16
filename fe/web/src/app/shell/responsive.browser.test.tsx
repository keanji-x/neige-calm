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
        <section data-testid="areas-list" style={{ inlineSize: '100%' }}>Areas</section>
      </div>
    </div>
    <main />

  </div>;
}

afterEach(() => { document.body.replaceChildren(); });

describe('responsive shell layout', () => {
  it('renders the brand mark as a crisp theme-owned square mask', () => {
    render(<span data-testid="brand-mark" className={styles.brandMark} />);
    const mark = document.querySelector('[data-testid="brand-mark"]')!;
    const box = mark.getBoundingClientRect();
    const style = getComputedStyle(mark);

    expect(box.width).toBe(16);
    expect(box.height).toBe(16);
    expect(style.maskImage).not.toBe('none');
    expect(style.backgroundColor).toBe(style.color);
  });

  it('uses one content column below the compact boundary and preserves desktop rail choices above it', async () => {
    render(<><ShellProbe state="auto" /><ShellProbe state="expanded" /><ShellProbe state="collapsed" /></>);

    await page.viewport(900, 700);
    expect(getComputedStyle(document.querySelector('[data-testid="auto"]')!).gridTemplateColumns).toBe('900px');
    expect(getComputedStyle(document.querySelector('[data-testid="expanded"]')!).gridTemplateColumns).toBe('900px');
    expect(getComputedStyle(document.querySelector('[data-testid="collapsed"]')!).gridTemplateColumns).toBe('900px');

    await page.viewport(390, 844);
    expect(getComputedStyle(document.querySelector('[data-testid="auto"]')!).gridTemplateColumns).toBe('390px');

    await page.viewport(1400, 900);
    expect(getComputedStyle(document.querySelector('[data-testid="auto"]')!).gridTemplateColumns).toBe('240px 1160px');
    expect(getComputedStyle(document.querySelector('[data-testid="expanded"]')!).gridTemplateColumns).toBe('240px 1160px');
    expect(getComputedStyle(document.querySelector('[data-testid="collapsed"]')!).gridTemplateColumns).toBe('44px 1356px');
  });

  it('gives the workspace navigation the full viewport without reserving a dock', async () => {
    render(<MobileNavigationProbe />);
    await page.viewport(390, 844);
    const list = document.querySelector('[data-testid="areas-list"]')!.getBoundingClientRect();
    const panel = document.querySelector('[data-testid="mobile-panel"]')!.getBoundingClientRect();
    expect(panel.left).toBe(0);
    expect(panel.right).toBe(window.innerWidth);
    expect(panel.width).toBe(list.width);
    expect(panel.bottom).toBe(window.innerHeight);
    expect(getComputedStyle(document.querySelector('[data-testid="mobile-panel"]')!).paddingBlockEnd).toBe('0px');
  });
});
