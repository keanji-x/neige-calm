// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { useState } from '../../ui/state/public.ts';
import { MobileAreas } from './mobile-areas.tsx';

afterEach(cleanup);

const area: Area = {
  id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0,
};
const track: Track = {
  id: 'w1', areaId: 'c1', title: 'Responsive mobile UI', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
  ...NEUTRAL_ACTIVITY,
};

/*
 * The drill-in is the shell's state now (#1191 §2.2), so a caller has to be
 * supplied. This stand-in is the *shape* of one transition — id and motion move
 * together — which is the property the shell is what actually proves
 * (`mobile-report-navigation.test.tsx` drives the real one).
 */
function AreasHarness({ onOpenTrack }: { onOpenTrack: (trackId: string) => void }) {
  const [selection, setSelection] = useState<{ areaId: string | null; motion: 'none' | 'forward' | 'back' }>(
    { areaId: null, motion: 'none' },
  );
  return (
    <MobileAreas
      areas={[area]}
      tracksByArea={new Map([['c1', [track]]])}
      selectedAreaId={selection.areaId}
      motion={selection.motion}
      onSelectArea={(areaId) => setSelection({ areaId, motion: 'forward' })}
      onBack={() => setSelection({ areaId: null, motion: 'back' })}
      onOpenTrack={onOpenTrack}
    />
  );
}

describe('MobileAreas', () => {
  it('navigates list → area Track list → Report without a desktop tree', async () => {
    const onOpenTrack = vi.fn();
    render(<AreasHarness onOpenTrack={onOpenTrack} />);

    expect(screen.getByRole('heading', { name: 'Areas' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: /Responsive mobile UI/ })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: /Product/ }));

    expect(screen.getByRole('heading', { name: 'Product' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: /Responsive mobile UI/ }));
    expect(onOpenTrack).toHaveBeenCalledWith('w1');

    await userEvent.click(screen.getByRole('button', { name: 'Back to Areas' }));
    expect(screen.getByRole('heading', { name: 'Areas' })).toBeTruthy();
  });
});
