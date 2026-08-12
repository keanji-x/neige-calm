// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { WaveBacklink, WaveBacklinks } from '../../../../../core/domain/report.ts';
import { ReportBacklinks } from './public.tsx';

afterEach(cleanup);

function backlink(overrides: Partial<WaveBacklink> = {}): WaveBacklink {
  return {
    src_wave_id: 'w-1', src_wave_title: 'Reference resolver', src_block_id: 'b-open',
    dst_block_id: 'b-thesis', label: 'the referenced side',
    quote: {
      before: 'The valuation this hangs off is in ', label: 'the referenced side',
      after: ', and its table is here.', head_elided: true, tail_elided: false,
    },
    updated_at: 0,
    ...overrides,
  };
}

const PAGE: WaveBacklinks = { truncated: false, skipped_sources: 0, backlinks: [backlink()] };

describe('ReportBacklinks', () => {
  it('is one row per citing wave: its title, on one line', () => {
    render(<ReportBacklinks waveId="w-2" backlinks={PAGE} onOpen={vi.fn()} />);
    expect(screen.getByRole('button').textContent).toBe('Reference resolver');
  });

  /*
   * The kernel answers per link. Two links in one sentence are two backlinks
   * whose quotes are two overlapping slices of it — which is what made this
   * module print the same words twice. One mention is one source *block*.
   */
  it('counts mentions by source block, so two links in one paragraph are one', () => {
    render(<ReportBacklinks
      waveId="w-2"
      onOpen={vi.fn()}
      backlinks={{ ...PAGE, backlinks: [
        backlink(),
        backlink({ dst_block_id: 'b-comps', label: 'here' }),
        backlink({ src_block_id: 'b-other' }),
      ] }}
    />);
    const row = screen.getByRole('button');
    expect(row.textContent).toBe('Reference resolver2');
  });

  // A column of ones says nothing; the count only appears when it does.
  it('shows no count for a single mention', () => {
    render(<ReportBacklinks waveId="w-2" backlinks={PAGE} onOpen={vi.fn()} />);
    expect(screen.getByRole('button').textContent).toBe('Reference resolver');
  });

  // The sentence is still worth having — just not at three lines in a 280
  // column. It rides along where it costs nothing until it is asked for.
  it('keeps the sentence as the row’s tooltip', () => {
    render(<ReportBacklinks waveId="w-2" backlinks={PAGE} onOpen={vi.fn()} />);
    expect(screen.getByRole('button').getAttribute('title'))
      .toBe('…The valuation this hangs off is in the referenced side, and its table is here.');
  });

  it('opens the citing wave at the block the citation is written in', async () => {
    const onOpen = vi.fn();
    render(<ReportBacklinks waveId="w-2" backlinks={PAGE} onOpen={onOpen} />);
    await userEvent.click(screen.getByRole('button'));
    // `b-open` is where the sentence *is*, not `b-thesis` which is what it
    // points at — a backlink takes you to the citation, not back to yourself.
    expect(onOpen).toHaveBeenCalledWith('w-1', 'b-open');
  });

  it('names a self-reference rather than repeating this wave’s own title', () => {
    render(<ReportBacklinks waveId="w-1" backlinks={PAGE} onOpen={vi.fn()} />);
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
