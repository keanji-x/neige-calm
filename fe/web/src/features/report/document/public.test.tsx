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

  /*
   * ── The reference appendix ───────────────────────────────────────────────
   *
   * The report is meant to be a deliverable and was not reading as one:
   * measured on a real wave, 8141 characters of body across 11 blocks, of which
   * the prose a reader takes away was ~700 and seven `task` blocks — worker
   * prompts, acceptance criteria, gate shell commands — were the rest, set
   * between the paragraphs that were the actual conclusions.
   *
   * So process blocks leave the flow and go to one collapsed section at the
   * end. Each claim below is a separate way that can break.
   */
  describe('the reference appendix', () => {
    function task(id: string, key: string): ReportBlock {
      return {
        id,
        kind: 'task',
        payload: { key, kind: 'codex', declared_by: 'spec', ready: true, goal: `goal for ${key}` },
      };
    }

    /* The whole point: the reading column is the argument, and the machinery is
       not in it. Asserted by *position* — the task must not be among the
       article's own block rows — because "is on the page somewhere" is exactly
       what stayed true while the defect existed. */
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

      /*
       * The prose stays, and **so does the table** — the second half is the one
       * that was missing and it was caught by mutation, not by review: a
       * predicate of `kind !== 'prose'` swept every figure into the appendix and
       * this file was still green, because every case here paired prose with a
       * task and nothing asked where a table went.
       *
       * A table or a chart is *evidence*: the report cites it to make its point,
       * and a conclusion whose numbers are folded into an appendix is a
       * conclusion you have to take on trust. The split this section draws is
       * "argument / machinery", not "prose / everything else".
       */
      expect(reference.contains(container.querySelector('#b-1'))).toBe(false);
      expect(reference.contains(container.querySelector('#b-3'))).toBe(false);
    });

    /*
     * **At the end**, which is the claim the containment assertions above do not
     * make: a filter that put the appendix first would satisfy every one of
     * them and still leave the reader scrolling past the machinery to reach the
     * conclusions.
     */
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
      /* And after *both* prose blocks, not merely last among some subset. */
      expect(referenceRow.compareDocumentPosition(container.querySelector('#b-3')!))
        .toBe(Node.DOCUMENT_POSITION_PRECEDING);
    });

    /* A task block whose payload this build cannot parse degrades to
       `unsupported` — and it is still machinery. Keying the split on
       `kind === 'task'` alone left exactly those in the reading column, printing
       `unsupported block kind task` between two paragraphs of conclusions. */
    it('lifts a task whose payload did not parse, which degrades to unsupported', () => {
      const { container } = render(<ReportDocument
        report={blocked(prose('b-1', '# Conclusion'), { id: 'b-2', kind: 'unsupported', declaredKind: 'task' })}
        empty={EMPTY}
      />);
      const reference = container.querySelector('[data-nc-report-reference]')!;
      expect(reference).not.toBeNull();
      expect(reference.contains(container.querySelector('#b-2'))).toBe(true);
    });

    /* But not every unsupported block: one that declared some other kind is a
       figure this build cannot draw, which is a hole in the argument and has to
       stay where the argument is. */
    it('leaves an unsupported block of some other kind in the flow', () => {
      const { container } = render(<ReportDocument
        report={blocked(prose('b-1', '# Conclusion'), { id: 'b-2', kind: 'unsupported', declaredKind: 'chart.sankey' })}
        empty={EMPTY}
      />);
      expect(container.querySelector('[data-nc-report-reference]')).toBeNull();
      expect(container.querySelector('#b-2')).toBeTruthy();
    });

    /* Closed, so the reader who never opens it reads a clean document. One fold
       for the whole appendix, not one per task. */
    it('starts closed, and is one fold for all of them', () => {
      const { container } = render(<ReportDocument
        report={blocked(task('b-1', 'alpha'), task('b-2', 'beta'))}
        empty={EMPTY}
      />);
      const reference = container.querySelector<HTMLDetailsElement>('[data-nc-report-reference]')!;
      expect(reference.open).toBe(false);
      expect(reference.querySelectorAll('[id]').length).toBe(2);
    });

    /*
     * A heading, not a styled row: `Reference` sits at the same level in the
     * heading outline as the report's own numbered sections, so a reader
     * navigating by heading finds the appendix where they would look for a
     * section. `<h2>` is what those sections are too (`document`'s `.h1` class
     * is worn by an `<h2>`), and `<summary>`'s content model takes phrasing
     * content *or one heading element* — which is why the heading wraps the
     * chevron and the count rather than sitting beside them.
     */
    it('is a heading at the same level as the report\'s own sections', () => {
      const { container } = render(<ReportDocument report={blocked(task('b-1', 'alpha'))} empty={EMPTY} />);
      const summary = container.querySelector('[data-nc-report-reference] > summary')!;
      const heading = summary.querySelector('h2');
      expect(heading).not.toBeNull();
      expect(heading!.textContent).toContain('Reference');
      /* One heading element, and everything in the summary is inside it. */
      expect(summary.children.length).toBe(1);
      expect(summary.firstElementChild).toBe(heading);
    });

    /* The count is the only thing a closed section can say about what is behind
       it, so it is the only reason to open it. */
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

    /* §6.1 — a section with zero rows is not rendered. A wave that declared no
       tasks has no machinery to account for, and a permanent empty appendix
       would make the ordinary case look like a gap. */
    it('is absent, not empty, when the report declares no tasks', () => {
      const { container } = render(
        <ReportDocument report={blocked(prose('b-1', '# Conclusion'))} empty={EMPTY} />,
      );
      expect(container.querySelector('[data-nc-report-reference]')).toBeNull();
    });

    /* The id is what makes the move safe: a `neige://wave/x#b-2` link from
       another report, and the panel's TASKS inventory, both address the block,
       and `revealReportAnchor` unfolds the section on the way in. Lose the id
       and both land nowhere, silently. */
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

  /*
   * A report may carry its own maintenance contract as a leading HTML comment:
   * dropped where the document is rendered, readable to everything that reads
   * the body source (#1185). The fixture is deliberately multi-line and spans
   * blank lines — a CommonMark HTML block of type 2 does not end at one, and a
   * single-line fixture would not test the property the carrier relies on.
   *
   * **These two cases were already green before #1185 touched this front end.**
   * `sanitizeAstPolicy(_, { rawHtml: 'drop' })` has always removed the node;
   * nothing here measures a production change made by this PR, and they must
   * not be read as `fe/`'s evidence for it — that is
   * `carrier.browser.test.tsx`, in a real browser. They stay as a regression
   * fence: the day someone reaches for `rehype-raw` to make `<details>` work,
   * this is where the contract leak shows up first, cheaply, in jsdom.
   */
  describe('a document that carries its own maintenance contract (#1185)', () => {
    const CONTRACT = [
      '<!-- 报告维护契约（渲染时被丢弃，读 body 源码的主体看得到）',
      '',
      '这份报告自带的结构就是规则：维护它，不要重写它。',
      '',
      '写作方式：散文正文控制在 1000 字以内。',
      '-->',
      '',
    ].join('\n');

    it('renders neither the contract nor a row for its block', () => {
      const { container } = render(<ReportDocument
        report={blocked(prose('b_1', CONTRACT), prose('b_2', '# 概要\n\n本轮结论。\n'))}
        empty={EMPTY}
      />);
      expect(container.textContent).not.toContain('报告维护契约');
      expect(container.innerHTML).not.toContain('报告维护契约');
      expect(container.textContent).not.toContain('散文正文');
      expect(container.innerHTML).not.toContain('散文正文');
      // The slot stays in the DOM (it keeps the anchor and any backlink
      // sidenote); the block inside it renders nothing, and the row it sits on
      // is what `.row:has(> .block:empty)` then hides — which jsdom cannot see.
      expect(container.querySelector('#b_1')?.childNodes.length).toBe(0);
      expect(container.textContent).toContain('概要');
      expect(container.textContent).toContain('本轮结论。');
    });

    it('drops the contract on the v1 flat-body path too', () => {
      // `report.blocks === null` sends the whole body through one ProseBlock.
      const { container } = render(<ReportDocument
        report={flat(`${CONTRACT}# 概要\n\n本轮结论。\n\n# 决策\n\n定了的事。\n`)}
        empty={EMPTY}
      />);
      expect(container.textContent).not.toContain('报告维护契约');
      expect(container.innerHTML).not.toContain('报告维护契约');
      expect(container.textContent).not.toContain('散文正文');
      expect(container.innerHTML).not.toContain('散文正文');
      expect(screen.getAllByRole('heading', { level: 2 }).map((h) => h.textContent))
        .toEqual(['概要', '决策']);
    });
  });
});
