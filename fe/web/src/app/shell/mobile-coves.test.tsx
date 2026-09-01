// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove } from '../../../../core/domain/cove.ts';
import { NEUTRAL_ACTIVITY, type Wave } from '../../../../core/domain/wave.ts';
import { useState } from '../../ui/state/public.ts';
import { MobileCoves } from './mobile-coves.tsx';

afterEach(cleanup);

const cove: Cove = {
  id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0,
};
const wave: Wave = {
  id: 'w1', coveId: 'c1', title: 'Responsive mobile UI', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
  ...NEUTRAL_ACTIVITY,
};

/*
 * The drill-in is the shell's state now (#1191 §2.2), so a caller has to be
 * supplied. This stand-in is the *shape* of one transition — id and motion move
 * together — which is the property the shell is what actually proves
 * (`mobile-report-navigation.test.tsx` drives the real one).
 */
function CovesHarness({ onOpenWave }: { onOpenWave: (waveId: string) => void }) {
  const [selection, setSelection] = useState<{ coveId: string | null; motion: 'none' | 'forward' | 'back' }>(
    { coveId: null, motion: 'none' },
  );
  return (
    <MobileCoves
      coves={[cove]}
      wavesByCove={new Map([['c1', [wave]]])}
      selectedCoveId={selection.coveId}
      motion={selection.motion}
      onSelectCove={(coveId) => setSelection({ coveId, motion: 'forward' })}
      onBack={() => setSelection({ coveId: null, motion: 'back' })}
      onOpenWave={onOpenWave}
    />
  );
}

describe('MobileCoves', () => {
  it('navigates list → cove Wave list → Report without a desktop tree', async () => {
    const onOpenWave = vi.fn();
    render(<CovesHarness onOpenWave={onOpenWave} />);

    expect(screen.getByRole('heading', { name: 'Coves' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: /Responsive mobile UI/ })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: /Product/ }));

    expect(screen.getByRole('heading', { name: 'Product' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: /Responsive mobile UI/ }));
    expect(onOpenWave).toHaveBeenCalledWith('w1');

    await userEvent.click(screen.getByRole('button', { name: 'Back to Coves' }));
    expect(screen.getByRole('heading', { name: 'Coves' })).toBeTruthy();
  });
});
