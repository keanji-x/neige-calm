// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

import type { ReportFileLinkTarget } from '../../../../../core/domain/report-file.ts';
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import type { ReportBlock, TrackReport } from '../../../../../core/domain/report.ts';
import { SOURCE_PANEL_COPY } from '../source/public.tsx';
import { initialBody, splitInitialBody } from './kernel-initial-body.ts';
import { ReportDocument } from './public.tsx';

afterEach(cleanup);

const EMPTY = <p>Nothing yet.</p>;

/** A v1 report: flat body, no blocks and therefore no anchors. */
function flat(body: string): TrackReport {
  return { summary: '', body, blocks: null };
}

function blocked(...blocks: ReportBlock[]): TrackReport {
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
    const { container } = render(<ReportDocument report={flat('# Goal\n\nBody text.')} empty={EMPTY} />);
    expect(container.querySelectorAll('h1').length).toBe(0);
    expect(screen.getByRole('heading', { level: 2 }).textContent).toBe('Goal');
  });

  it('renders a tight list item inline, so its text sits on the marker line', () => {
    const { container } = render(<ReportDocument report={flat('- one\n- two')} empty={EMPTY} />);
    const items = container.querySelectorAll('li');
    expect(items.length).toBe(2);
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
        <ReportDocument report={flat('See [the planner](https://example.com/planner) for details.')} empty={EMPTY} />,
      );
      expect(container.querySelectorAll('a').length).toBe(0);
      expect(container.textContent).toContain('the planner');
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
      expect(onOpenLink).toHaveBeenCalledWith({ trackId: 'w-2', blockId: 'b-3' });
    });

    it('routes a workspace-relative file through a button and a typed callback', () => {
      const onOpenFileLink = vi.fn<(target: ReportFileLinkTarget) => void>();
      const { container } = render(
        <ReportDocument
          report={flat('Inspect [the transport](./fe/web/src/app/providers/transport.ts).')}
          empty={EMPTY}
          onOpenFileLink={onOpenFileLink}
          fileRoot="/repo"
        />,
      );
      expect(container.querySelectorAll('a').length).toBe(0);
      screen.getByRole('button', { name: 'the transport' }).click();
      expect(onOpenFileLink).toHaveBeenCalledWith({ path: 'fe/web/src/app/providers/transport.ts' });
    });

    it('does not turn an escaping relative path into a file control', () => {
      const onOpenFileLink = vi.fn();
      const { container } = render(
        <ReportDocument
          report={flat('Do not open [outside](../secret.txt).')}
          empty={EMPTY}
          onOpenFileLink={onOpenFileLink}
          fileRoot="/repo"
        />,
      );
      expect(container.querySelectorAll('button').length).toBe(0);
      expect(container.textContent).toContain('outside');
      expect(onOpenFileLink).not.toHaveBeenCalled();
    });

    it('accepts an absolute file link only when it is inside the injected workspace root', () => {
      const onOpenFileLink = vi.fn();
      render(<ReportDocument
        report={flat('[inside](/repo/src/main.rs:12) and [outside](/etc/passwd).')}
        empty={EMPTY}
        onOpenFileLink={onOpenFileLink}
        fileRoot="/repo"
      />);
      screen.getByRole('button', { name: 'inside' }).click();
      expect(onOpenFileLink).toHaveBeenCalledWith({ path: 'src/main.rs' });
      expect(screen.queryByRole('button', { name: 'outside' })).toBeNull();
    });

    it('resolves links relative to the Markdown file being rendered', () => {
      const onOpenFileLink = vi.fn<(target: ReportFileLinkTarget) => void>();
      render(<ReportDocument
        report={flat('[sibling](./spec.md) and [parent](../README.md).')}
        empty={EMPTY}
        onOpenFileLink={onOpenFileLink}
        fileRoot="/repo"
        fileBasePath="docs"
      />);
      screen.getByRole('button', { name: 'sibling' }).click();
      screen.getByRole('button', { name: 'parent' }).click();
      expect(onOpenFileLink.mock.calls.map(([target]) => target)).toEqual([
        { path: 'docs/spec.md' }, { path: 'README.md' },
      ]);
    });

    it('routes a source citation through a button and a typed callback, never an anchor', () => {
      const onOpenSourceLink = vi.fn<(target: ReportSourceLinkTarget) => void>();
      const { container } = render(
        <ReportDocument
          report={flat('据 [Mikko 日志](neige://source/src_2c9e0a1b#q1) 与 [年报](neige://source/src_0badf00d)。')}
          empty={EMPTY}
          onOpenSourceLink={onOpenSourceLink}
        />,
      );
      expect(container.querySelectorAll('a').length).toBe(0);
      expect(container.innerHTML).not.toContain('neige://source');
      screen.getByRole('button', { name: 'Mikko 日志' }).click();
      screen.getByRole('button', { name: '年报' }).click();
      expect(onOpenSourceLink.mock.calls.map(([target]) => target)).toEqual([
        { destination: 'neige://source/src_2c9e0a1b#q1', sourceId: 'src_2c9e0a1b', quoteId: 'q1' },
        { destination: 'neige://source/src_0badf00d', sourceId: 'src_0badf00d', quoteId: null },
      ]);
      expect(container.querySelectorAll('[data-nc-report-source-link]').length).toBe(2);
    });

    it('keeps a malformed source citation clickable so the panel can say it is missing', () => {
      const onOpenSourceLink = vi.fn<(target: ReportSourceLinkTarget) => void>();
      render(
        <ReportDocument
          report={flat('见 [坏链接](neige://source/src_dead#q0)。')}
          empty={EMPTY}
          onOpenSourceLink={onOpenSourceLink}
        />,
      );
      screen.getByRole('button', { name: '坏链接' }).click();
      expect(onOpenSourceLink).toHaveBeenCalledWith({
        destination: 'neige://source/src_dead#q0', sourceId: null, quoteId: null,
      });
    });

    it('does not hand a source citation to the track or file callbacks', () => {
      const onOpenLink = vi.fn();
      const onOpenFileLink = vi.fn();
      const onOpenSourceLink = vi.fn();
      render(
        <ReportDocument
          report={flat('[x](neige://source/src_2c9e0a1b#q1)')}
          empty={EMPTY}
          onOpenLink={onOpenLink}
          onOpenFileLink={onOpenFileLink}
          onOpenSourceLink={onOpenSourceLink}
          fileRoot="/repo"
        />,
      );
      screen.getByRole('button', { name: 'x' }).click();
      expect(onOpenSourceLink).toHaveBeenCalledTimes(1);
      expect(onOpenLink).not.toHaveBeenCalled();
      expect(onOpenFileLink).not.toHaveBeenCalled();
    });

    it('renders a source citation as a badge plus label, not a control, when no handler is injected', () => {
      const { container } = render(
        <ReportDocument report={flat('据 [Mikko 日志](neige://source/src_2c9e0a1b#q1)。')} empty={EMPTY} />,
      );
      expect(container.querySelectorAll('button, a').length).toBe(0);
      const citation = container.querySelector('[data-nc-report-source-citation]');
      expect(citation).not.toBeNull();
      expect(citation?.textContent).toBe(`${SOURCE_PANEL_COPY.citationBadge}Mikko 日志`);
      expect(container.innerHTML).not.toContain('neige://source');
    });

    it('hands a table cell citation the same source handler as the prose', () => {
      const onOpenSourceLink = vi.fn<(target: ReportSourceLinkTarget) => void>();
      const { container } = render(
        <ReportDocument
          report={blocked(
            prose('b-1', '据 [Mikko 日志](neige://source/src_2c9e0a1b#q1)。'),
            {
              id: 'b-2', kind: 'table',
              payload: {
                columns: [{ key: 'metric', label: '指标' }, { key: 'source', label: '来源' }],
                rows: [{ metric: '布伦特收盘', source: '[AP](neige://source/src_ddef99cc#q1)' }],
              },
            },
          )}
          empty={EMPTY}
          onOpenSourceLink={onOpenSourceLink}
        />,
      );
      expect(container.querySelectorAll('a').length).toBe(0);
      screen.getByRole('button', { name: 'AP' }).click();
      expect(onOpenSourceLink).toHaveBeenCalledWith({
        destination: 'neige://source/src_ddef99cc#q1', sourceId: 'src_ddef99cc', quoteId: 'q1',
      });
      expect(container.querySelectorAll('td [data-nc-report-source-link]').length).toBe(1);
    });

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
    const body = `${'> '.repeat(80)}too deep`;
    const { container } = render(<ReportDocument report={flat(body)} empty={EMPTY} />);
    expect(container.querySelector('pre')?.textContent).toContain('too deep');
  });

  describe('the reference appendix', () => {
    function task(id: string, key: string): ReportBlock {
      return {
        id,
        kind: 'task',
        payload: { key, kind: 'codex', declared_by: 'spec', ready: true, goal: `goal for ${key}` },
      };
    }

    it('lifts task blocks out of the document flow, and only those', () => {
      const { container } = render(<ReportDocument
        report={blocked(
          prose('b-1', '# Conclusion'),
          task('b-2', 'alpha'),
          { id: 'b-3', kind: 'table', payload: { caption: 'Comparables', columns: [{ key: 'k', label: 'K' }], rows: [{ k: 'v' }] } },
        )}
        empty={EMPTY}
      />);
      const reference = container.querySelector('[data-nc-report-reference]')!;
      expect(reference).toBeTruthy();
      expect(reference.contains(container.querySelector('#b-2'))).toBe(true);

      expect(reference.contains(container.querySelector('#b-1'))).toBe(false);
      expect(reference.contains(container.querySelector('#b-3'))).toBe(false);
    });

    it('comes after the document, not before it', () => {
      const { container } = render(<ReportDocument
        report={blocked(prose('b-1', '# Conclusion'), task('b-2', 'alpha'), prose('b-3', '# Next'))}
        empty={EMPTY}
      />);
      const article = container.querySelector('[data-nc-report]')!;
      const reference = container.querySelector('[data-nc-report-reference]')!;
      const referenceRow = reference.closest('div')!;
      const rows = [...article.children];
      expect(rows.indexOf(referenceRow)).toBe(rows.length - 1);
      expect(referenceRow.compareDocumentPosition(container.querySelector('#b-3')!))
        .toBe(Node.DOCUMENT_POSITION_PRECEDING);
    });

    it('lifts a task whose payload did not parse, which degrades to unsupported', () => {
      const { container } = render(<ReportDocument
        report={blocked(prose('b-1', '# Conclusion'), { id: 'b-2', kind: 'unsupported', declaredKind: 'task' })}
        empty={EMPTY}
      />);
      const reference = container.querySelector('[data-nc-report-reference]')!;
      expect(reference).not.toBeNull();
      expect(reference.contains(container.querySelector('#b-2'))).toBe(true);
    });

    it('leaves an unsupported block of some other kind in the flow', () => {
      const { container } = render(<ReportDocument
        report={blocked(prose('b-1', '# Conclusion'), { id: 'b-2', kind: 'unsupported', declaredKind: 'chart.sankey' })}
        empty={EMPTY}
      />);
      expect(container.querySelector('[data-nc-report-reference]')).toBeNull();
      expect(container.querySelector('#b-2')).toBeTruthy();
    });

    it('starts closed, and is one fold for all of them', () => {
      const { container } = render(<ReportDocument
        report={blocked(task('b-1', 'alpha'), task('b-2', 'beta'))}
        empty={EMPTY}
      />);
      const reference = container.querySelector<HTMLDetailsElement>('[data-nc-report-reference]')!;
      expect(reference.open).toBe(false);
      expect(reference.querySelectorAll('[id]').length).toBe(2);
    });

    it('is a heading at the same level as the report\'s own sections', () => {
      const { container } = render(<ReportDocument report={blocked(task('b-1', 'alpha'))} empty={EMPTY} />);
      const summary = container.querySelector('[data-nc-report-reference] > summary')!;
      const heading = summary.querySelector('h2');
      expect(heading).not.toBeNull();
      expect(heading!.textContent).toContain('Reference');
      expect(summary.children.length).toBe(1);
      expect(summary.firstElementChild).toBe(heading);
    });

    it('says how many are behind it, and counts one in the singular', () => {
      const { container } = render(<ReportDocument report={blocked(task('b-1', 'alpha'))} empty={EMPTY} />);
      expect(container.querySelector('[data-nc-report-reference] summary')?.textContent)
        .toContain('1 task');
      cleanup();
      const two = render(<ReportDocument
        report={blocked(task('b-1', 'alpha'), task('b-2', 'beta'))}
        empty={EMPTY}
      />);
      expect(two.container.querySelector('[data-nc-report-reference] summary')?.textContent)
        .toContain('2 tasks');
    });

    it('is absent, not empty, when the report declares no tasks', () => {
      const { container } = render(
        <ReportDocument report={blocked(prose('b-1', '# Conclusion'))} empty={EMPTY} />,
      );
      expect(container.querySelector('[data-nc-report-reference]')).toBeNull();
    });

    it('keeps each block id, so a citation still has something to land on', () => {
      const { container } = render(<ReportDocument report={blocked(task('b-2', 'alpha'))} empty={EMPTY} />);
      expect(container.querySelector('#b-2')).toBeTruthy();
    });
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

    it('renders a chart.series block through the injected resolver, by block id and rev', () => {
      const block: ReportBlock = { id: 'b-1', kind: 'chart.series', rev: 3, payload: {
        source: 'neige://plugin/dev-neige-market/market.series', series: ['US:NVDA', 'HK:9988'], range: '6M', caption: 'Big tech',
      } };
      const { container } = render(<ReportDocument report={blocked(block)} empty={EMPTY} />);
      expect(screen.getByRole('note').textContent).toContain('this view does not carry its data');
      expect(screen.getByText('Big tech')).toBeTruthy();
      expect(container.textContent).not.toContain('unsupported block kind');
      cleanup();

      const asked: [string, number][] = [];
      render(<ReportDocument report={blocked(block)} empty={EMPTY} resolveSeries={(blockId, rev) => {
        asked.push([blockId, rev]);
        return { status: 'pending', reason: 'plugin dev-neige-market is not running', view: 'line', field: 'close', period: 'day', range: '6M' };
      }} />);
      expect(asked).toEqual([['b-1', 3]]);
      expect(screen.getByRole('note').textContent).toContain('Pending — plugin dev-neige-market is not running');
    });

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

  describe('a document that carries its own maintenance contract (#1185)', () => {
    /* Read in `beforeAll`, not at module scope: `kernel-initial-body.ts` must never touch the filesystem at import time. */
    let CONTRACT: string;
    let SECTIONS: string[];
    beforeAll(() => {
      [CONTRACT, ...SECTIONS] = splitInitialBody();
    });

    it('the fixture really is the kernel skeleton', () => {
      expect(CONTRACT.startsWith('<!-- neige:contract ')).toBe(true);
      expect(CONTRACT).toContain('<!-- 报告维护契约');
      expect(CONTRACT.endsWith('-->\n\n')).toBe(true);
      expect(CONTRACT).toContain('散文正文');
      expect(SECTIONS.map((s) => s.split('\n')[0]))
        .toEqual(['# 概要', '# 待你定', '# 已完成', '# 决策']);
    });

    it('renders neither the contract nor a row for its block', () => {
      const { container } = render(<ReportDocument
        report={blocked(prose('b_1', CONTRACT), prose('b_2', `${SECTIONS[0]}本轮结论。\n`))}
        empty={EMPTY}
      />);
      expect(container.textContent).not.toContain('报告维护契约');
      expect(container.innerHTML).not.toContain('报告维护契约');
      expect(container.innerHTML).not.toContain('neige:contract');
      expect(container.textContent).not.toContain('散文正文');
      expect(container.innerHTML).not.toContain('散文正文');
      expect(container.querySelector('#b_1')?.childNodes.length).toBe(0);
      expect(container.textContent).toContain('概要');
      expect(container.textContent).toContain('本轮结论。');
    });

    it('drops the contract on the v1 flat-body path too', () => {
      const { container } = render(<ReportDocument report={flat(initialBody())} empty={EMPTY} />);
      expect(container.textContent).not.toContain('报告维护契约');
      expect(container.innerHTML).not.toContain('报告维护契约');
      expect(container.innerHTML).not.toContain('neige:contract');
      expect(container.textContent).not.toContain('散文正文');
      expect(container.innerHTML).not.toContain('散文正文');
      expect(screen.getAllByRole('heading', { level: 2 }).map((h) => h.textContent))
        .toEqual(['概要', '待你定', '已完成', '决策']);
    });
  });
});
