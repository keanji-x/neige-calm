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

/**
 * What one type token resolves to right now, as a computed `font-size`.
 *
 * A probe element rather than reading the custom property off `:root`: the
 * property's value is a token, and the thing the assertions compare against is
 * the *resolved* `font-size` string the engine reports, so the probe has to go
 * through the same resolution the page does.
 */
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
    /*
     * Compared against the tokens as the engine resolves them, not against
     * `18px` / `13px`.
     *
     * What this locks is the size each element ends up at: the notice computes
     * to whatever `--text-base` resolves to right now and the document region
     * to `--text-lg`, with the two ranks asserted distinct first so they cannot
     * pass by collapsing into each other. It does not lock which token the CSS
     * *names* — a rule written as a literal would pass too, as long as the
     * number still matches the token's current value. Comparing against probes
     * rather than `18px` / `13px` is what keeps a legitimate global retune of
     * either token from failing this test while the implementation stays
     * correct.
     */
    const prose = fontSizeOf('--text-lg');
    const interfaceRank = fontSizeOf('--text-base');
    expect(prose).not.toBe(interfaceRank);
    // The region really is at the prose rank — otherwise this test would pass
    // for the trivial reason that nothing here is enlarged at all.
    expect(getComputedStyle(region as Element).fontSize).toBe(prose);
    // …and the notice is not: it reads at the rank it had before the region
    // gained its type, and the rank the button beside it is sized against.
    expect(getComputedStyle(notice as Element).fontSize).toBe(interfaceRank);
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
