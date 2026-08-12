// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { WaveBacklinks } from '../../../../../core/domain/report.ts';
import { ReportBacklinks } from './public.tsx';

afterEach(cleanup);

const PAGE: WaveBacklinks = {
  truncated: false,
  skipped_sources: 0,
  backlinks: [{
    src_wave_id: 'w-1', src_wave_title: 'Reference resolver', src_block_id: 'b-open',
    dst_block_id: 'b-thesis', label: 'the referenced side',
    quote: {
      before: 'The valuation this hangs off is in ', label: 'the referenced side',
      after: ', and its table is here.', head_elided: true, tail_elided: false,
    },
    updated_at: 0,
  }],
};

describe('ReportBacklinks', () => {
  it('shows the sentence the citation is written in, with the linking words picked out', () => {
    const { container } = render(<ReportBacklinks waveId="w-2" backlinks={PAGE} onOpen={vi.fn()} />);
    expect(container.textContent).toContain('…The valuation this hangs off is in ');
    expect(container.querySelector('b')?.textContent).toBe('the referenced side');
  });

  it('opens the citing wave at the block the citation lives in', async () => {
    const onOpen = vi.fn();
    render(<ReportBacklinks waveId="w-2" backlinks={PAGE} onOpen={onOpen} />);
    await userEvent.click(screen.getByRole('button'));
    // `b-open` is where the sentence *is*, not `b-thesis` which is what it
    // points at — a backlink takes you to the citation, not back to yourself.
    expect(onOpen).toHaveBeenCalledWith('w-1', 'b-open');
  });

  it('names a self-reference rather than repeating this wave’s own title', () => {
    render(<ReportBacklinks
      waveId="w-1"
      backlinks={PAGE}
      onOpen={vi.fn()}
    />);
    expect(screen.getByText('This wave (self-reference)')).toBeTruthy();
  });

  // A citation list that is quietly short is worse than one that admits it.
  it('says so when the page is truncated or a source could not be read', () => {
    render(<ReportBacklinks
      waveId="w-2"
      backlinks={{ ...PAGE, truncated: true, skipped_sources: 2 }}
      onOpen={vi.fn()}
    />);
    expect(screen.getByText('Some backlinks are not shown.')).toBeTruthy();
    expect(screen.getByText(/2 source reports could not be read/)).toBeTruthy();
  });

  it('emits no native link', () => {
    const { container } = render(<ReportBacklinks waveId="w-2" backlinks={PAGE} onOpen={vi.fn()} />);
    expect(container.querySelectorAll('a').length).toBe(0);
  });
});
