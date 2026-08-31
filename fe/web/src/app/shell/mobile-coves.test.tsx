// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove } from '../../../../core/domain/cove.ts';
import { NEUTRAL_ACTIVITY, type Wave } from '../../../../core/domain/wave.ts';
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

describe('MobileCoves', () => {
  it('navigates list → cove Wave list → Report without a desktop tree', async () => {
    const onOpenWave = vi.fn();
    render(<MobileCoves coves={[cove]} wavesByCove={new Map([['c1', [wave]]])} onOpenWave={onOpenWave} />);

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
