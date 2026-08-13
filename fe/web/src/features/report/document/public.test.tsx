// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportBlock, WaveReport } from '../../../../../core/domain/report.ts';
import { ReportDocument } from './public.tsx';

afterEach(cleanup);

const EMPTY = <p>Nothing yet.</p>;

/** A v1 report: flat body, no blocks and therefore no anchors. */
function flat(body: string): WaveReport {
  return { summary: '', body, blocks: null };
}

function blocked(...blocks: ReportBlock[]): WaveReport {
  return { summary: '', body: '', blocks };
}

function prose(id: string, markdown: string): ReportBlock {
  return { id, kind: 'prose', payload: { markdown } };
}

describe('ReportDocument', () => {
  it('renders the empty state when there is no report', () => {
    render(<ReportDocument report={null} empty={EMPTY} />);
    expect(screen.getByText('Nothing yet.')).toBeTruthy();
  });

  it('renders a summary-only legacy report instead of the empty state', () => {
    render(<ReportDocument
      report={{ summary: 'Agent finished the migration.', body: '', blocks: null }}
      empty={EMPTY}
    />);
    expect(screen.getByText('Agent finished the migration.')).toBeTruthy();
    expect(screen.queryByText('Nothing yet.')).toBeNull();
  });

  it('renders H1 as a section heading below the page title, never as an h1', () => {
    // The page title is the wave's name in the header. A document that emitted
    // its own h1 would give the page two, which is the heading-order failure
    // axe reports and the reason this maps depth 1 to h2.
    const { container } = render(<ReportDocument report={flat('# Goal\n\nBody text.')} empty={EMPTY} />);
    expect(container.querySelectorAll('h1').length).toBe(0);
    expect(screen.getByRole('heading', { level: 2 }).textContent).toBe('Goal');
  });

  it('renders a tight list item inline, so its text sits on the marker line', () => {
    const { container } = render(<ReportDocument report={flat('- one\n- two')} empty={EMPTY} />);
    const items = container.querySelectorAll('li');
    expect(items.length).toBe(2);
    // No block wrapper inside the item: that is what put every bullet's text on
    // the line below its marker.
    expect(items[0]?.querySelector('p')).toBeNull();
    expect(items[0]?.textContent).toBe('one');
  });

  it('keeps a task item checkbox disabled, because this surface does not write back', () => {
    render(<ReportDocument report={flat('- [x] done\n- [ ] open')} empty={EMPTY} />);
    const boxes = screen.getAllByRole('checkbox');
    expect(boxes.length).toBe(2);
    expect(boxes.every((box) => (box as HTMLInputElement).disabled)).toBe(true);
    expect((boxes[0] as HTMLInputElement).checked).toBe(true);
    expect((boxes[1] as HTMLInputElement).checked).toBe(false);
  });

  describe('INV-A11Y-061 — a report emits no native link', () => {
    it('keeps a link label and drops its destination', () => {
      const { container } = render(
        <ReportDocument report={flat('See [the spec](https://example.com/spec) for details.')} empty={EMPTY} />,
      );
      expect(container.querySelectorAll('a').length).toBe(0);
      expect(container.textContent).toContain('the spec');
      expect(container.innerHTML).not.toContain('example.com');
    });

    it('renders an image as its alt text and never requests the source', () => {
      const { container } = render(
        <ReportDocument report={flat('![a diagram](https://example.com/x.png)')} empty={EMPTY} />,
      );
      expect(container.querySelectorAll('img').length).toBe(0);
      expect(container.textContent).toContain('a diagram');
      expect(container.innerHTML).not.toContain('example.com');
    });

    it('routes a neige:// citation through a button and a callback, not an anchor', () => {
      const onOpenLink = vi.fn();
      const { container } = render(
        <ReportDocument
          report={flat('See [the model](neige://wave/w-2#b-3).')}
          empty={EMPTY}
          onOpenLink={onOpenLink}
        />,
      );
      expect(container.querySelectorAll('a').length).toBe(0);
      screen.getByRole('button', { name: 'the model' }).click();
      expect(onOpenLink).toHaveBeenCalledWith({ waveId: 'w-2', blockId: 'b-3' });
    });

    // Without a handler there is nowhere for the citation to go, and a button
    // that does nothing is worse than plain text.
    it('renders a citation as plain text when no handler is injected', () => {
      const { container } = render(
        <ReportDocument report={flat('See [the model](neige://wave/w-2#b-3).')} empty={EMPTY} />,
      );
      expect(container.querySelectorAll('button').length).toBe(0);
      expect(container.textContent).toContain('the model');
    });
  });

  it('drops raw HTML rather than rendering it', () => {
    const { container } = render(
      <ReportDocument report={flat('<script>alert(1)</script>\n\nAfter.')} empty={EMPTY} />,
    );
    expect(container.querySelectorAll('script').length).toBe(0);
    expect(container.innerHTML).not.toContain('alert(1)');
    expect(container.textContent).toContain('After.');
  });

  it('falls back to the source when the markdown will not parse', () => {
    // A report that exceeds the normalizer's limits is still what the agent
    // wrote; showing it beats showing an error about it.
    const body = `${'> '.repeat(80)}too deep`;
    const { container } = render(<ReportDocument report={flat(body)} empty={EMPTY} />);
    expect(container.querySelector('pre')?.textContent).toContain('too deep');
  });

  describe('typed blocks', () => {
    it('gives each block its id, so a citation has something to land on', () => {
      const { container } = render(<ReportDocument report={blocked(
        prose('b-1', '# One'),
        { id: 'b-2', kind: 'table', payload: { columns: [{ key: 'k', label: 'K' }], rows: [{ k: 'v' }] } },
      )} empty={EMPTY} />);
      expect(container.querySelector('#b-1')).toBeTruthy();
      expect(container.querySelector('#b-2')).toBeTruthy();
    });

    it('anchors headings on ids the outline can address', () => {
      // `<block id>-h<n>` is `reportHeadingIdPolicy`; the outline derives the
      // same ids from the same call, so this is the join between the two.
      const { container } = render(
        <ReportDocument report={blocked(prose('b-1', '# One\n\n## Two'))} empty={EMPTY} />,
      );
      expect(container.querySelector('#b-1-h1')?.textContent).toBe('One');
      expect(container.querySelector('#b-1-h2')?.textContent).toBe('Two');
    });

    it('renders every kind it knows', () => {
      render(<ReportDocument report={blocked(
        { id: 'b-1', kind: 'table', payload: { columns: [{ key: 'name', label: 'Name' }], rows: [{ name: 'Kweichow' }] } },
        { id: 'b-2', kind: 'chart.candles', payload: { symbol: '600519', candles: [[0, 1, 2, 0.5, 1.5], [86400000, 1.5, 2, 1, 1.2]] } },
        { id: 'b-3', kind: 'task', payload: { key: 't-1', kind: 'codex', goal: 'Ship it', ready: true, declared_by: 'spec' } },
      )} empty={EMPTY} />);
      expect(screen.getByText('Kweichow')).toBeTruthy();
      expect(screen.getByText('600519')).toBeTruthy();
      expect(screen.getByText('t-1')).toBeTruthy();
    });

    // The entrance fee of the block model, stated as a test: the reader keeps
    // the document even when the viewer cannot draw part of it.
    it('degrades one unreadable block and keeps the rest of the document', () => {
      const { container } = render(<ReportDocument report={blocked(
        { id: 'b-1', kind: 'unsupported', declaredKind: 'chart.sankey' },
        prose('b-2', 'Still readable.'),
      )} empty={EMPTY} />);
      expect(container.textContent).toContain('unsupported block kind chart.sankey');
      expect(container.textContent).toContain('Still readable.');
    });

    it('marks a cited block in the sidenote, and only a cited one', () => {
      const { container } = render(<ReportDocument
        report={blocked(prose('b-1', 'Cited.'), prose('b-2', 'Not cited.'))}
        backlinkCounts={new Map([['b-1', 3]])}
        empty={EMPTY}
      />);
      expect(container.textContent).toContain('◂ 3');
      expect(container.textContent?.match(/◂/g)?.length).toBe(1);
    });
  });
});
