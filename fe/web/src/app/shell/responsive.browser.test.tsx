import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';
import styles from './shell.module.css';
import { setup } from './mobile-browser-fixture.tsx';
import { playSpring, playTrajectory, type Playback } from '../../ui/motion/spring.ts';
vi.mock('../../ui/motion/spring.ts', { spy: true });

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

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.clearAllMocks(); document.body.replaceChildren(); });

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
    await Promise.all(document.querySelector('[data-testid="mobile-panel"]')!.getAnimations().map((animation) => animation.finished));
    const list = document.querySelector('[data-testid="areas-list"]')!.getBoundingClientRect();
    const panel = document.querySelector('[data-testid="mobile-panel"]')!.getBoundingClientRect();
    expect(panel.left).toBe(0);
    expect(panel.right).toBe(window.innerWidth);
    expect(panel.width).toBe(list.width);
    expect(panel.bottom).toBe(window.innerHeight);
    expect(getComputedStyle(document.querySelector('[data-testid="mobile-panel"]')!).paddingBlockEnd).toBe('0px');
  });
});

// Running clocks exercise physical continuity; these are controlled model oracles,
// separate from the natural route screenshots in mobile.browser.test.tsx.
it.each(['Shell', 'Tracks'] as const)('preserves %s paint contribution and velocity across resize and reversal', async layer => {
  await page.viewport(390, 844);
  setup('/track/w1');
  await page.getByRole('button', { name: 'Open workspace' }).click();
  const host = await page.getByRole('dialog', { name: 'Tracks and settings' }).findElement() as HTMLElement;
  if (layer === 'Tracks') {
    await Promise.all(host.getAnimations().map(animation => animation.finished));
    await page.getByRole('button', { name: 'Product', exact: true }).click();
  }
  const node = layer === 'Shell' ? host : document.querySelector<HTMLElement>('[data-nc-workspace-page="tracks"]')!;
  const old = node.getAnimations()[0];
  expect(old).toBeDefined();
  await old.ready;
  old.playbackRate = 0.5;
  old.currentTime = 60;
  old.startTime = Number(document.timeline.currentTime) - 120;
  const playback = (vi.mocked(playTrajectory).mock.results.at(-1)?.value ?? vi.mocked(playSpring).mock.results.at(-1)!.value) as Playback<number>;
  let sample: { value: unknown; velocity: unknown } | undefined;
  vi.spyOn(old, 'cancel').mockImplementation(function (this: Animation) {
    sample = playback.sample();
    Animation.prototype.cancel.call(this);
  });
  await page.viewport(520, 844);
  await expect.poll(() => node.getAnimations().some(animation => animation !== old)).toBe(true);
  expect(old.playState).toBe('idle');
  expect(sample).toBeDefined();
  const rebased = vi.mocked(playTrajectory).mock.calls.at(-1)![1].sample(0);
  expect(rebased.value).toBeCloseTo(sample!.value as number, 6);
  expect(rebased.velocity).toBeCloseTo(sample!.velocity as number, 6);
  expect(Math.abs(rebased.velocity as number)).toBeGreaterThan(1);
  const active = node.getAnimations()[0];
  await active.ready;
  active.playbackRate = 1;
  active.currentTime = 60;
  active.startTime = Number(document.timeline.currentTime) - 60;
  const forward = vi.mocked(playTrajectory).mock.calls.at(-1)![1];
  let reversal: { value: unknown; velocity: unknown } | undefined;
  vi.spyOn(active, 'cancel').mockImplementation(function (this: Animation) {
    reversal = forward.sample(Number(this.currentTime));
    Animation.prototype.cancel.call(this);
  });
  await page.getByRole('button', { name: layer === 'Shell' ? 'Back to workspace' : 'Back to Areas', exact: true }).click();
  const reverse = vi.mocked(playTrajectory).mock.calls.at(-1)![1].sample(0);
  expect(reverse.value).toBeCloseTo(reversal!.value as number, 6);
  expect(reverse.velocity).toBeCloseTo(reversal!.velocity as number, 6);
});
