/* Facts about Today that only a real engine can settle: computed values and box geometry, not class names. */
import type { ReactNode } from 'react';
import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../styles/entry.css';

import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import type { Area } from '../../../../core/domain/area.ts';
import { TodayPage, type TodayPageProps } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

const NOW = new Date(2026, 7, 10, 15, 0, 0).getTime();

const renderTrackRow: TodayPageProps['renderTrackRow'] = (track) => (
  <span data-nc-role="row">{track.title}</span>
);

/** What one type token resolves to right now, as a computed `font-size`, via a probe that goes through the same resolution the page does. */
function fontSizeOf(token: '--text-lg' | '--text-base'): string {
  const probe = document.createElement('span');
  probe.style.fontSize = `var(${token})`;
  document.body.append(probe);
  const size = getComputedStyle(probe).fontSize;
  probe.remove();
  return size;
}

function area(): Area {
  return {
    id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0,
  };
}

function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Open track', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: NOW - 3_600_000, updatedAt: NOW,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

/** The two things `app/shell`'s `.main` provides that this page reads; without them `--document-start` is invalid and the gutter absent. */
function Main({ children, inlineSize = '1080px' }: { children: ReactNode; inlineSize?: string }) {
  return (
    <div style={{
      containerType: 'inline-size',
      ['--panel-span' as string]: 'max(15rem, 25cqi)',
      inlineSize,
      display: 'flex',
      flexDirection: 'column',
      blockSize: '760px',
    }}
    >
      {children}
    </div>
  );
}

/** The document region: the box the empty guide or written report lives in. */
function regionOf(container: Element): HTMLElement {
  const guide = container.querySelector('[aria-label="Getting started"]');
  expect(guide).toBeTruthy();
  return (guide as HTMLElement).parentElement as HTMLElement;
}

describe('the document’s action answers a control, not the document', () => {
  it('stays at interface rank inside the document region', async () => {
    await page.viewport(1280, 800);
    const { container } = render(
      <Main><TodayPage
      activityAvailable
        renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
        launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
        launchpadDocument={<p>the day&apos;s report</p>}
        documentAction={<button type="button" data-nc-action="destructive">Reset</button>}
      /></Main>,
    );
    const action = container.querySelector('button[data-nc-action="destructive"]');
    const region = container.querySelector('p')?.parentElement;
    expect(action).not.toBeNull();
    expect(region).not.toBeNull();
    const prose = fontSizeOf('--text-lg');
    const interfaceRank = fontSizeOf('--text-base');
    expect(prose).not.toBe(interfaceRank);
    expect(getComputedStyle(region as Element).fontSize).toBe(prose);
    expect(getComputedStyle(action as Element).fontSize).toBe(interfaceRank);
  });
});

describe('the document region owns the column the report will stand in', () => {
  function renderVacant() {
    return render(
      <Main><TodayPage
      activityAvailable
        renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
        launchpad={{ track_id: 'lp', report_has_noninitial_content: false }}
      /></Main>,
    );
  }

  it('publishes a real gutter, and stands the action on the document’s column', async () => {
    await page.viewport(1280, 800);
    const { container } = render(
      <Main><TodayPage
      activityAvailable
        renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
        launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
        launchpadDocument={<p>today’s report</p>}
        documentAction={<button type="button" data-nc-action="destructive">Reset</button>}
      /></Main>,
    );
    const action = container.querySelector('button[data-nc-action="destructive"]')
      ?.parentElement as HTMLElement;
    const column = action.parentElement?.parentElement as HTMLElement;
    const actionBox = action.getBoundingClientRect();
    const columnBox = column.getBoundingClientRect();

    /* Geometry, because a custom property cannot be read back: `getComputedStyle` returns the substituted token for `--document-start`, not the length it resolves to. */
    const leading = actionBox.left - columnBox.left;
    expect(leading).toBeGreaterThan(0);
  });

  it('centres the empty guide in the main column, not inside a start-aligned box', async () => {
    await page.viewport(1280, 800);
    const { container } = renderVacant();
    const region = regionOf(container);
    const guide = container.querySelector('[aria-label="Getting started"]') as HTMLElement;
    const column = region.parentElement as HTMLElement;
    const guideBox = guide.getBoundingClientRect();
    const columnBox = column.getBoundingClientRect();
    expect(Math.abs((guideBox.left + guideBox.right) / 2 - (columnBox.left + columnBox.right) / 2))
      .toBeLessThanOrEqual(1);
    expect(region.getBoundingClientRect().width).toBeGreaterThan(504);
  });

  it('clamps the document measure to a narrow desktop main column', async () => {
    await page.viewport(1024, 800);
    const { container } = render(
      <Main inlineSize="824px"><TodayPage
      activityAvailable
        renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
        launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
        launchpadDocument={<div data-document-measure="" style={{
          inlineSize: 'var(--document-measure)', blockSize: '1000px',
        }} />}
      /></Main>,
    );
    const measure = container.querySelector('[data-document-measure]') as HTMLElement;
    const mainColumn = measure.parentElement?.parentElement as HTMLElement;
    /* Chromium uses overlay scrollbars on some hosts. Force the classic gutter
       that Windows/Linux desktop browsers reserve so the cross-platform
       constraint is tested deterministically. */
    const scrollport = mainColumn.parentElement?.parentElement as HTMLElement;
    scrollport.style.scrollbarGutter = 'stable';
    scrollport.style.overflowY = 'scroll';
    expect(measure.getBoundingClientRect().width)
      .toBeLessThanOrEqual(mainColumn.getBoundingClientRect().width);
  });
});

describe('the agenda empty line sits on the panel inset', () => {
  it('starts where the module title starts, not 4px left of it', async () => {
    await page.viewport(1280, 800);
    // A track that is alive nowhere near today, so both agenda sources are
    // empty and the module renders its empty line.
    const { container } = render(
      <TodayPage
      activityAvailable
        renderTrackRow={renderTrackRow} areas={[area()]} nowMs={NOW}
        tracks={[track({
          lifecycle: 'done',
          createdAt: NOW - 40 * 86_400_000,
          terminalAt: NOW - 39 * 86_400_000,
          updatedAt: NOW - 39 * 86_400_000,
        })]}
      />,
    );
    const empty = [...container.querySelectorAll('p')]
      .find((node) => node.textContent === 'Nothing scheduled.');
    const title = [...container.querySelectorAll('h2')]
      .find((node) => node.textContent === 'Calendar');
    expect(empty).toBeTruthy();
    expect(title).toBeTruthy();
    expect((empty as HTMLElement).getBoundingClientRect().left)
      .toBe((title as HTMLElement).getBoundingClientRect().left);
  });
});
