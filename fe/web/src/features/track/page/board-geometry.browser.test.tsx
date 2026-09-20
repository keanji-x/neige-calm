/* The card board is a viewport-sized overlay whatever the report's height: the grid's `inset: 0` overlay has `.workspace` as its containing block, so both files' classes are imported rather than restated. */
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';

import grid from '../grid/grid.module.css';
import { MobileHeader } from '../../../ui/mobile-header/public.tsx';
import { PageHeader } from '../../../ui/page-header/public.tsx';
import styles from './page.module.css';

afterEach(() => { document.body.replaceChildren(); });

const VIEWPORT = 600;

/** The track page's skeleton with the board slot filled, at the real nesting:
 *  `.page > .workspace > (.content, board)`. */
function Page({ documentHeight, boardOpen }: { documentHeight: number; boardOpen: boolean }) {
  return (
    <div style={{ blockSize: VIEWPORT, display: 'flex', flexDirection: 'column' }}>
      <section className={`${styles.page} ${boardOpen ? styles.pageBoard : ''}`} data-testid="page">
        <PageHeader title="Track" />
        <div className={styles.mobileTrackHeader}><MobileHeader title="Track" level={1} /></div>
        <div className={styles.workspace} data-testid="workspace">
          <div className={styles.content}>
            <div className={styles.doc}>
              <article data-testid="report-content" style={{ blockSize: documentHeight }}>Report</article>
            </div>
            <aside className={styles.panel} data-nc-panel="">
              <div style={{ blockSize: 200 }}>Cards</div>
            </aside>
          </div>
          {/* `features/track/grid`'s own class, not a hand-written `inset: 0`: a fixture that restates it measures itself. */}
          <div data-testid="board" className={boardOpen ? grid.open : grid.closed} />
        </div>
      </section>
    </div>
  );
}

const boxOf = (element: Element | null) => element!.getBoundingClientRect();
const pageBox = () => boxOf(document.querySelector('[data-testid="page"]'));
const boardBox = () => boxOf(document.querySelector('[data-testid="board"]'));
const workspaceBox = () => boxOf(document.querySelector('[data-testid="workspace"]'));
const scroller = () => document.querySelector<HTMLElement>('[data-testid="page"]')!;

describe('the card board overlay, against a long report', () => {
  it('is one viewport tall even when the report is many', async () => {
    await browserPage.viewport(1200, VIEWPORT);
    render(<Page documentHeight={4000} boardOpen />);

    expect(pageBox().height).toBeLessThan(VIEWPORT + 1);
    const board = boardBox();
    expect(board.height).toBeGreaterThan(200);
    expect(board.height).toBeLessThanOrEqual(VIEWPORT + 1);
  });

  it('is one viewport tall with the board closed, which is still mounted', async () => {
    await browserPage.viewport(1200, VIEWPORT);
    render(<Page documentHeight={4000} boardOpen={false} />);
    expect(workspaceBox().height).toBeLessThanOrEqual(VIEWPORT + 1);
    expect(boardBox().height).toBeLessThanOrEqual(VIEWPORT + 1);
  });

  it('moves into the visible content region when opened after the page has scrolled', async () => {
    await browserPage.viewport(1200, VIEWPORT);
    const view = render(<Page documentHeight={4000} boardOpen={false} />);
    const visibleContentTop = Math.round(workspaceBox().top);
    const reportEnd = scroller().scrollHeight - scroller().clientHeight;
    expect(reportEnd).toBeGreaterThan(2000);

    scroller().scrollTop = reportEnd;
    expect(scroller().scrollTop, 'the report did not scroll').toBe(reportEnd);
    expect(boardBox().bottom, 'the closed board should begin above the viewport')
      .toBeLessThan(visibleContentTop);

    view.rerender(<Page documentHeight={4000} boardOpen />);

    expect(scroller().scrollTop, 'opening the board changed the reader\'s place').toBe(reportEnd);
    expect(
      Math.abs(boardBox().top - visibleContentTop),
      'the open board did not return to the visible content edge',
    ).toBeLessThanOrEqual(1);
    expect(boardBox().bottom).toBeLessThanOrEqual(pageBox().bottom + 1);

    /* A box assertion cannot tell whether the report still paints in the trailing gap, so ask the browser what would receive a pointer there. */
    const report = boxOf(document.querySelector('[data-testid="report-content"]'));
    const trailingPaddingY = Math.floor((boardBox().bottom + pageBox().bottom) / 2);
    expect(pageBox().bottom - boardBox().bottom).toBeGreaterThan(2);
    expect(
      document.elementFromPoint(report.left + 1, trailingPaddingY)
        ?.closest('[data-testid="report-content"]')
        ?.getAttribute('data-testid') ?? null,
      `report content remained hit-testable below the board at y=${trailingPaddingY}`,
    ).toBeNull();
  });

  it('fits the compact content region on a direct cold open', async () => {
    await browserPage.viewport(390, VIEWPORT);
    const view = render(<Page documentHeight={4000} boardOpen={false} />);
    const visibleContentTop = workspaceBox().top;

    view.rerender(<Page documentHeight={4000} boardOpen />);

    expect(Math.abs(boardBox().top - visibleContentTop))
      .toBeLessThanOrEqual(1);
    expect(boardBox().bottom).toBeLessThanOrEqual(pageBox().bottom + 1);
    expect(boardBox().height).toBeLessThanOrEqual(VIEWPORT + 1);
  });
});
