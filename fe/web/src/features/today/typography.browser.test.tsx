/*
 * Facts about Today that only a real engine can settle, all of them invisible
 * to jsdom, which loads no CSS:
 *
 *   1. the document action sits inside `.document`, but remains at the
 *      interface type rank rather than inheriting the prose rank;
 *   2. the agenda's empty line is a shared primitive (`PanelEmpty`) placed by
 *      this feature, so its inset depends on where this feature puts it;
 *   3. the document region's own geometry — the gutter this page publishes for
 *      `features/report`'s three-column grid, and whether the empty day is
 *      centred in the space the report would fill.
 *
 * Computed values and box geometry, not class names: a rule that exists but is
 * overridden reads the same as a rule that works when you only inspect source.
 */
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

/**
 * The two things `app/shell`'s `.main` provides that this page reads.
 *
 * `--document-start` is a leftover computed against `100cqi` and
 * `--panel-span`, both of which come from that box; rendering `TodayPage` on
 * its own leaves the custom property invalid and the gutter simply absent —
 * which is the very defect these cases exist to catch, so it cannot also be the
 * conditions they run under. If the shell renames or re-derives either, this
 * host is where the mirror goes stale.
 */
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

/** The document region: the box the empty line or written report lives in. */
function regionOf(container: Element): HTMLElement {
  const line = [...container.querySelectorAll('p')]
    .find((node) => node.textContent === 'Nothing written today yet.');
  expect(line).toBeTruthy();
  return (line as HTMLElement).parentElement as HTMLElement;
}

describe('the document’s action answers a control, not the document', () => {
  it('stays at interface rank inside the document region', async () => {
    await page.viewport(1280, 800);
    const { container } = render(
      <Main><TodayPage
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
    /*
     * Compared against the tokens as the engine resolves them, not against
     * `18px` / `13px`.
     *
     * What this locks is the size each element ends up at: the control
     * computes to whatever `--text-base` resolves to right now and the
     * document region to `--text-lg`, with the two ranks asserted distinct
     * first so they cannot pass by collapsing into each other. It does not
     * lock which token the CSS *names* — a rule written as a literal would
     * pass too, as long as the number still matches the token's current value.
     * Comparing against probes rather than `18px` / `13px` is what keeps a
     * legitimate global retune of either token from failing this test while
     * the implementation stays correct.
     */
    const prose = fontSizeOf('--text-lg');
    const interfaceRank = fontSizeOf('--text-base');
    expect(prose).not.toBe(interfaceRank);
    // The region really is at the prose rank — otherwise this test would pass
    // for the trivial reason that nothing here is enlarged at all.
    expect(getComputedStyle(region as Element).fontSize).toBe(prose);
    // …and the control is not: `[data-nc-action]` declares its own size
    // (base.css §4.1) and a declaration beats an inherited value, so it reads
    // at interface rank beside a document rather than as part of the prose.
    expect(getComputedStyle(action as Element).fontSize).toBe(interfaceRank);
  });
});

/*
 * ── The document region's own geometry ────────────────────────────────────
 *
 * `features/report`'s `.doc` is a three-column grid whose first track is
 * `var(--document-start)` with no fallback, so a route that publishes nothing
 * loses the whole `grid-template-columns` — outline gutter, measure and
 * sidenote column at once. Today published neither variable and capped the
 * region at `--measure-prose` from the outside instead, which is what owner saw
 * on the 4140 preview: a narrow column pinned to the left margin, with the
 * empty day centred inside it rather than in the space the report would fill.
 */
describe('the document region owns the column the report will stand in', () => {
  function renderVacant() {
    return render(
      <Main><TodayPage
        renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
        launchpad={{ track_id: 'lp', report_has_noninitial_content: false }}
      /></Main>,
    );
  }

  it('publishes a real gutter, and stands the action on the document’s column', async () => {
    await page.viewport(1280, 800);
    const { container } = render(
      <Main><TodayPage
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

    /*
     * Geometry, because a custom property cannot be read back: `getComputedStyle`
     * returns the substituted *token* for `--document-start`, not the length it
     * resolves to, so `parseFloat` on it is `NaN` whether the page publishes a
     * good expression or nothing at all.
     *
     * What the boxes say instead is stronger. The action takes
     * `margin-inline-start: var(--document-start, 0px)`, so it lands on the
     * document's own column rather than flush against the main column. Delete
     * the publication and `var()` falls back to 0px.
     */
    const leading = actionBox.left - columnBox.left;
    expect(leading).toBeGreaterThan(0);
  });

  it('centres the empty day in the main column, not inside a 504px box', async () => {
    await page.viewport(1280, 800);
    const { container } = renderVacant();
    const region = regionOf(container);
    const line = [...container.querySelectorAll('p')]
      .find((node) => node.textContent === 'Nothing written today yet.') as HTMLElement;
    const column = region.parentElement as HTMLElement;
    const lineBox = line.getBoundingClientRect();
    const columnBox = column.getBoundingClientRect();
    /*
     * The sentence's centre against the MAIN COLUMN's, not against its own
     * region's. With the old cap the region was 504 wide and start-aligned, and
     * the sentence was perfectly centred inside it — so a region-relative
     * assertion passes on the exact layout owner reported. The column is what
     * the reader sees, and the document's own measure column is centred in it
     * too (`--document-start` is the leftover halved), so this is one axis for
     * the empty day and the report that replaces it.
     */
    expect(Math.abs((lineBox.left + lineBox.right) / 2 - (columnBox.left + columnBox.right) / 2))
      .toBeLessThanOrEqual(1);
    // And the region really did stop being 504 wide, so the centring above is
    // not being satisfied by a box that happens to sit mid-column.
    expect(region.getBoundingClientRect().width).toBeGreaterThan(504);
  });

  it('clamps the document measure to a narrow desktop main column', async () => {
    await page.viewport(1024, 800);
    const { container } = render(
      <Main inlineSize="824px"><TodayPage
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
