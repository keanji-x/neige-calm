/*
 * Two facts about Today that only a real engine can settle, both introduced by
 * #1253's document region and both invisible to jsdom, which loads no CSS:
 *
 *   1. the summary notice inherits from `.document`, so it is the one piece of
 *      text on this page that can silently take the prose rank;
 *   2. the agenda's empty line is a shared primitive (`PanelEmpty`) placed by
 *      this feature, so its inset depends on where this feature puts it.
 *
 * Computed values and box geometry, not class names: a rule that exists but is
 * overridden reads the same as a rule that works when you only inspect source.
 */
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

function area(): Area {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0 };
}

function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Open track', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: NOW - 3_600_000, updatedAt: NOW,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

describe('the summary notice answers a control, not the document', () => {
  it('stays at interface rank inside the document region', async () => {
    await page.viewport(1280, 800);
    const { container } = render(
      <TodayPage
        renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
        launchpad={{ track_id: 'lp', report_has_noninitial_content: false }}
        onWriteSummary={() => undefined}
        summaryNotice={<span data-nc-role="hint">Nothing happened today.</span>}
      />,
    );
    const notice = container.querySelector('[data-nc-role="hint"]');
    const trigger = container.querySelector('button[data-nc-action="tertiary"]');
    const region = container.querySelector('p')?.parentElement;
    expect(notice).not.toBeNull();
    expect(trigger).not.toBeNull();
    // The region really is at the prose rank — otherwise this test would pass
    // for the trivial reason that nothing here is enlarged at all.
    expect(getComputedStyle(region as Element).fontSize).toBe('18px');
    // …and the notice is not: 13px is the interface rank, which is what it read
    // at before the region gained its type, and what the button beside it is
    // sized against.
    expect(getComputedStyle(notice as Element).fontSize).toBe('13px');
    // Not the darkest text either: it is an aside about a press.
    expect(getComputedStyle(notice as Element).color)
      .not.toBe(getComputedStyle(region as Element).color);
  });
});

describe('the agenda empty line sits on the panel inset', () => {
  it('starts where the module title starts, not 4px left of it', async () => {
    await page.viewport(1280, 800);
    // A track that is alive nowhere near today, so both agenda sources are
    // empty and the module renders its empty line.
    const { container } = render(
      <TodayPage
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
