// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type {
  ReportSourceLinkTarget, SourceProvenance, TrackSourceDetail,
} from '../../../../../core/domain/report-source.ts';
import { SOURCE_PANEL_COPY, SOURCE_PROVENANCE_COPY } from './copy.ts';
import { ReportSourcePanel, reportSourcePanelTitle } from './public.tsx';

const scrollIntoView = vi.fn();
beforeEach(() => {
  Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', { configurable: true, value: scrollIntoView });
});
afterEach(() => { cleanup(); scrollIntoView.mockReset(); });

const BODY = '央行表示，9月加息概率接近九成。\n\n市场认为，9月加息概率接近九成的说法过于乐观。';

function row(overrides: Partial<TrackSourceDetail> = {}): TrackSourceDetail {
  return {
    source_id: 'src_2c9e0a1b',
    provenance: 'full_text',
    origin: { kind: 'plugin', plugin_id: 'mcp-wisburg', tool: 'get_article_detail', args_sha256: 'ab', args_canon: 'v1', content_id: '752972' },
    title: 'Mikko 全球市场日志 9-13',
    published_at: '2026-09-13',
    content_id: '752972',
    body_bytes: 100,
    body_sha256: 'cd',
    captured_at: '2026-09-14T08:00:00Z',
    quotes: [{ id: 'q1', text: '9月加息概率接近九成', start: 0, end: 0 }],
    body: BODY,
    ...overrides,
  };
}

function target(destination: string, sourceId: string | null, quoteId: string | null): ReportSourceLinkTarget {
  return { destination, sourceId, quoteId };
}

const WELL_FORMED = target('neige://source/src_2c9e0a1b#q1', 'src_2c9e0a1b', 'q1');
const NO_ANCHOR = target('neige://source/src_2c9e0a1b', 'src_2c9e0a1b', null);

describe('ReportSourcePanel', () => {
  /*
   * The owner signed these four strings (#1669 §2.5). Each provenance is
   * rendered on its own and the badge is read back by the enum value, so a
   * swapped pair — `summary` wearing `full_text`'s words — turns this red.
   */
  it.each(Object.keys(SOURCE_PROVENANCE_COPY) as SourceProvenance[])('names the %s provenance with the signed wording', (provenance) => {
    render(<ReportSourcePanel target={NO_ANCHOR} resolution={{ status: 'ok', source: row({ provenance }) }} onRetry={() => undefined} />);
    const badge = document.querySelector('[data-nc-report-source-provenance]');
    expect(badge?.getAttribute('data-nc-report-source-provenance')).toBe(provenance);
    expect(badge?.textContent).toBe(SOURCE_PROVENANCE_COPY[provenance]);
    expect(SOURCE_PROVENANCE_COPY[provenance]).toBe({
      full_text: '智堡全文', summary: '智堡摘要，非机构原文', web_page: '网页', manual: '手工录入，未经内核核验',
    }[provenance]);
  });

  it('paints the title, dates and origin as text, and the body as raw text in a <pre>', () => {
    const { container } = render(<ReportSourcePanel
      target={NO_ANCHOR}
      resolution={{ status: 'ok', source: row({ body: '看 [链接](https://example.com/x) 和 ![图](https://example.com/x.png) **不渲染**' }) }}
      onRetry={() => undefined}
    />);
    expect(screen.getByRole('heading', { level: 2 }).textContent).toBe('Mikko 全球市场日志 9-13');
    expect(container.textContent).toContain('2026-09-13');
    expect(container.querySelector('time')?.getAttribute('dateTime')).toBe('2026-09-14T08:00:00Z');
    expect(container.textContent).toContain('mcp-wisburg');
    expect(container.textContent).toContain('get_article_detail');
    expect(container.textContent).toContain('752972');
    // Raw: the Markdown is shown as written, nothing becomes an element.
    const body = container.querySelector('pre[data-nc-report-source-body]');
    expect(body?.textContent).toBe('看 [链接](https://example.com/x) 和 ![图](https://example.com/x.png) **不渲染**');
    expect(container.querySelectorAll('a, img, strong').length).toBe(0);
    expect(container.querySelector('mark')).toBeNull();
    expect(scrollIntoView).not.toHaveBeenCalled();
  });

  it('slices the first occurrence of the quote into a <mark> and scrolls it into view', () => {
    const { container } = render(<ReportSourcePanel target={WELL_FORMED} resolution={{ status: 'ok', source: row() }} onRetry={() => undefined} />);
    const body = container.querySelector('pre[data-nc-report-source-body]');
    const marks = container.querySelectorAll('mark');
    expect(marks.length).toBe(1);
    expect(marks[0]?.textContent).toBe('9月加息概率接近九成');
    expect(marks[0]?.getAttribute('data-nc-report-source-quote')).toBe('q1');
    // The text before the mark is exactly the prefix of the FIRST occurrence;
    // slicing the second one would put `市场认为` before it.
    expect(body?.childNodes[0]?.textContent).toBe('央行表示，');
    expect(body?.textContent).toBe(BODY);
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    expect(scrollIntoView.mock.instances[0]).toBe(marks[0]);
    expect(container.querySelector('[data-nc-report-source-anchor-missed]')).toBeNull();
  });

  it('shows the whole body and says so when the anchor cannot be placed', () => {
    const missingAnchor = target('neige://source/src_2c9e0a1b#q7', 'src_2c9e0a1b', 'q7');
    const { container } = render(<ReportSourcePanel target={missingAnchor} resolution={{ status: 'ok', source: row() }} onRetry={() => undefined} />);
    expect(container.querySelector('mark')).toBeNull();
    expect(container.querySelector('pre[data-nc-report-source-body]')?.textContent).toBe(BODY);
    expect(screen.getByRole('status').textContent).toBe(SOURCE_PANEL_COPY.anchorMissed);
    expect(scrollIntoView).not.toHaveBeenCalled();
  });

  it('says the anchor missed when the quote text is not in the body either', () => {
    const source = row({ quotes: [{ id: 'q1', text: '不在正文里', start: 0, end: 0 }] });
    const { container } = render(<ReportSourcePanel target={WELL_FORMED} resolution={{ status: 'ok', source }} onRetry={() => undefined} />);
    expect(container.querySelector('mark')).toBeNull();
    expect(container.querySelector('[data-nc-report-source-anchor-missed]')).not.toBeNull();
  });

  it('says the source is missing, with the destination, when the track has no such row (404)', () => {
    const { container } = render(<ReportSourcePanel target={WELL_FORMED} resolution={{ status: 'missing' }} onRetry={() => undefined} />);
    expect(screen.getByRole('heading', { level: 2 }).textContent).toBe(SOURCE_PANEL_COPY.missingTitle);
    expect(container.querySelector('[data-nc-report-source-missing]')?.getAttribute('data-nc-report-source-missing')).toBe('dangling');
    expect(container.textContent).toContain(SOURCE_PANEL_COPY.missingDangling);
    expect(container.querySelector('code')?.textContent).toBe('neige://source/src_2c9e0a1b#q1');
    expect(container.querySelector('pre')).toBeNull();
  });

  it('says the source is missing, with the destination, when the citation would not parse', () => {
    const malformed = target('neige://source/src_dead#q0', null, null);
    // Whatever the app's query says is irrelevant: nothing was fetched.
    const { container } = render(<ReportSourcePanel target={malformed} resolution={{ status: 'ok', source: row() }} onRetry={() => undefined} />);
    expect(screen.getByRole('heading', { level: 2 }).textContent).toBe(SOURCE_PANEL_COPY.missingTitle);
    expect(container.querySelector('[data-nc-report-source-missing]')?.getAttribute('data-nc-report-source-missing')).toBe('malformed');
    expect(container.textContent).toContain(SOURCE_PANEL_COPY.missingMalformed);
    expect(container.querySelector('code')?.textContent).toBe('neige://source/src_dead#q0');
    expect(container.querySelector('pre')).toBeNull();
  });

  it('shows the loading line, then the error box with a retry that repeats the read', () => {
    const onRetry = vi.fn();
    const { rerender } = render(<ReportSourcePanel target={WELL_FORMED} resolution={{ status: 'loading' }} onRetry={onRetry} />);
    expect(screen.getByRole('status').textContent).toBe(SOURCE_PANEL_COPY.loading);
    rerender(<ReportSourcePanel target={WELL_FORMED} resolution={{ status: 'error', message: 'boom' }} onRetry={onRetry} />);
    expect(screen.getByRole('alert').textContent).toContain('boom');
    screen.getByRole('button', { name: 'Retry' }).click();
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it('never emits a native link, even for a manual source with a URL', () => {
    const source = row({ provenance: 'manual', origin: { kind: 'manual', url: 'https://example.com/page' }, url: 'https://example.com/page', content_id: undefined });
    const { container } = render(<ReportSourcePanel target={NO_ANCHOR} resolution={{ status: 'ok', source }} onRetry={() => undefined} />);
    expect(container.textContent).toContain('https://example.com/page');
    expect(container.querySelectorAll('a').length).toBe(0);
    expect(container.querySelector('[data-nc-report-source-provenance="manual"]')?.textContent).toBe('手工录入，未经内核核验');
  });

  it('names the drawer after the row once it is known, and generically until then', () => {
    expect(reportSourcePanelTitle({ status: 'loading' })).toBe(SOURCE_PANEL_COPY.panelTitle);
    expect(reportSourcePanelTitle({ status: 'missing' })).toBe(SOURCE_PANEL_COPY.panelTitle);
    expect(reportSourcePanelTitle({ status: 'error', message: 'x' })).toBe(SOURCE_PANEL_COPY.panelTitle);
    expect(reportSourcePanelTitle({ status: 'ok', source: row() })).toBe('Mikko 全球市场日志 9-13');
  });
});
