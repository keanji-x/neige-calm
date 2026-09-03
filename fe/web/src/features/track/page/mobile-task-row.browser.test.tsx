/*
 * The mobile Task row's strike-through, measured (#1234 S1b-4b review).
 *
 * **What the jsdom pair does not say.** `RowBadge.struck` has no carrier in the
 * projection framework at all, so the only thing holding it on this surface is
 * `mobile-painter.test.tsx`'s class assertion — and a class assertion answers
 * "did the painter write `.mobileRowStruck`", not "is the word struck through".
 * Delete `text-decoration: line-through` from the rule and keep the class: both
 * directions of that pair stay green, and the phone shows a withdrawn
 * declaration set exactly like a live one. That is the same defect
 * `task-row.browser.test.tsx` was written for on the desktop — a test that
 * checks `className` passes whether or not a single rule ever matched.
 *
 * So this reads `text-decoration-line` back out of the cascade, in an engine,
 * at a width where the rule is even eligible: `.mobileRowStruck` lives inside
 * `@media (width < 60rem)`, and it is the **iframe** the suite renders into that
 * the query is evaluated against — see `vitest.config.ts`'s note on the two
 * viewports.
 *
 * Both directions, because a declaration struck unconditionally would satisfy
 * the positive half alone.
 */
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

/* The whole cascade, and before the CSS Module — see the import-order note in
   `panel-sticky.browser.test.tsx`. */
import '../../../styles/entry.css';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../../core/domain/track.ts';
import { TrackPage } from './public.tsx';

afterEach(() => {
  document.body.replaceChildren();
});

const track: Track = {
  id: 'w1', areaId: 'c1', title: 'Alpha', sort: 1, lifecycle: 'working', cwd: '/tmp/alpha',
  archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
  ...NEUTRAL_ACTIVITY,
};

/** A withdrawn declaration and an ordinary one, in that order — the same pair
 *  the jsdom class assertion uses, so the two tests are about one row set. */
const TASKS: readonly ReportTaskRow[] = [
  {
    blockId: 'b-gone', key: 'gamma-planner', state: 'withdrawn', declaration: 'Withdrawn',
    status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null,
  },
  {
    blockId: 'b-doc', key: 'delta-doc', state: 'unreadable', declaration: 'Unreadable',
    status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null,
  },
];

describe('a withdrawn declaration on the mobile Tasks page', () => {
  it('is struck through, and an ordinary declaration beside it is not', async () => {
    /* Narrow enough for `@media (width < 60rem)`. This call is what makes the
       case *run*, not a guard against a false green — measured by putting 1024
       here instead: the whole mobile surface is collapsed at that width, both
       badges measure 0 wide, and the **width premise below** is what goes red,
       before either decoration is ever read. What excludes the false green is
       the positive assertion at the end: `line-through` is a value nothing but
       the rule produces. */
    await browserPage.viewport(420, 900);
    render(
      <TrackPage
        track={track}
        cards={[]}
        tasks={TASKS}
        panel="tasks"
        onOpenTask={vi.fn()}
        onRenameTrack={vi.fn()}
        onDeleteTrack={vi.fn()}
      />,
    );

    const panel = document.querySelector('[data-nc-mobile-panel]');
    expect(panel, 'the mobile panel must be on the page').not.toBeNull();
    const badges = Array.from(panel!.querySelectorAll('[data-nc-badge="declaration"]'));
    expect(badges.map((badge) => badge.textContent)).toEqual(['Withdrawn', 'Unreadable']);

    /* Premise: these words are laid out. A mobile panel the media query left
       collapsed would give every reading below a computed style off a box
       nobody can see, and the assertions would be about nothing. */
    for (const badge of badges) {
      expect(badge.getBoundingClientRect().width).toBeGreaterThan(0);
    }

    expect(getComputedStyle(badges[0]).textDecorationLine).toBe('line-through');
    expect(getComputedStyle(badges[1]).textDecorationLine).toBe('none');
  });
});
