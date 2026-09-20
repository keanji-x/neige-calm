/* The mobile Task row's strike-through, measured: a class assertion cannot say whether the word is struck. `.mobileRowStruck` lives inside `@media (width < 60rem)`, evaluated against the iframe the suite renders into. */
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

/* The whole cascade, and before the CSS Module. */
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

/** A withdrawn declaration and an ordinary one, in that order. */
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
    /* Narrow enough for `@media (width < 60rem)`; at 1024 the whole mobile surface is collapsed. */
    await browserPage.viewport(420, 900);
    render(
      <TrackPage
        mobilePanelObscured={false}
        track={track}
        cards={[]}
        tasks={TASKS}
        openableCards={new Set()}
        panel="tasks"
        onOpenTask={vi.fn()}
        canResumeTrack={false}
        onRenameTrack={vi.fn()}
        onResumeTrack={vi.fn()}
        onDeleteTrack={vi.fn()}
      />,
    );

    const panel = document.querySelector('[data-nc-mobile-panel]');
    expect(panel, 'the mobile panel must be on the page').not.toBeNull();
    for (const summary of panel!.querySelectorAll<HTMLElement>('details:not([open]) > summary')) summary.click();
    const badges = Array.from(panel!.querySelectorAll('[data-nc-badge="declaration"]'));
    expect(badges.map((badge) => badge.textContent)).toEqual(['Unreadable', 'Withdrawn']);

    /* Premise: these words are laid out; a collapsed panel would give computed styles off a box nobody can see. */
    for (const badge of badges) {
      expect(badge.getBoundingClientRect().width).toBeGreaterThan(0);
    }

    expect(getComputedStyle(badges[1]).textDecorationLine).toBe('line-through');
    expect(getComputedStyle(badges[0]).textDecorationLine).toBe('none');
  });
});
