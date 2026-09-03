/*
 * The card board is a **viewport-sized** overlay, whatever the report's height.
 *
 * `features/track/grid`'s overlay is `position: absolute; inset: 0` and its
 * containing block is `.workspace`, so `.workspace`'s height *is* the board's
 * height. That coupling is invisible from either file: the grid says `inset: 0`
 * and the page says how tall `.workspace` is, and nothing in between says they
 * are the same number. Both ends are therefore imported here — the grid's class
 * and the page's — because a fixture that restated either one would be
 * measuring itself.
 *
 * It was broken by the sticky fix in this same change. `.workspace` used to be
 * a `minmax(0, 1fr)` row — exactly one viewport — and became `flex: 1 0 auto`
 * so the panel card would have somewhere to travel. `flex-basis: auto` with
 * `flex-shrink: 0` sizes it to its content, so on a 4000px report the board
 * became a 4000px overlay inside a `.page` that `.pageBoard` gives
 * `overflow: hidden` — the top screenful visible, the rest unreachable. Worse
 * than a layout bug: `BoardHost` measures its container to pick terminal rows
 * and resizes the PTY to match, so the geometry is sent to a real process.
 *
 * Only an engine can answer this, and only with the real stylesheets: jsdom
 * gives every element a zero box, so the overlay and the viewport would agree
 * at zero and the test would pass on the broken CSS.
 */
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';

import grid from '../grid/grid.module.css';
import pageHeader from '../../../ui/page-header/page-header.module.css';
import styles from './page.module.css';

afterEach(() => { document.body.replaceChildren(); });

const VIEWPORT = 600;

/** The track page's skeleton with the board slot filled, at the real nesting:
 *  `.page > .workspace > (.content, board)`. */
function Page({ documentHeight, boardOpen }: { documentHeight: number; boardOpen: boolean }) {
  return (
    <div style={{ blockSize: VIEWPORT, display: 'flex', flexDirection: 'column' }}>
      <section className={`${styles.page} ${boardOpen ? styles.pageBoard : ''}`} data-testid="page">
        <div className={pageHeader.header}>Track</div>
        <div className={styles.workspace} data-testid="workspace">
          <div className={styles.content}>
            <div className={styles.doc}>
              <article style={{ blockSize: documentHeight }}>Report</article>
            </div>
            <aside className={styles.panel} data-nc-panel="">
              <div style={{ blockSize: 200 }}>Cards</div>
            </aside>
          </div>
          {/*
            * `features/track/grid`'s **own class**, not a hand-written
            * `inset: 0`. The claim under test is a product of two files — the
            * grid's positioning and this page's `.workspace` height — and a
            * fixture that restates one of them measures itself: change the grid
            * to `position: fixed`, or out of `absolute` altogether, and a
            * replica would stay green while the real overlay broke. That is
            * fail-open in exactly the direction of the defect this file exists
            * for, which is why it is imported.
            */}
          <div data-testid="board" className={boardOpen ? grid.open : grid.closed} />
        </div>
      </section>
    </div>
  );
}

/* Looked up by a static selector each: the architecture rule forbids building
   one at runtime, and it is right — a template-literal selector fails open. */
const boxOf = (element: Element | null) => element!.getBoundingClientRect();
const pageBox = () => boxOf(document.querySelector('[data-testid="page"]'));
const boardBox = () => boxOf(document.querySelector('[data-testid="board"]'));
const workspaceBox = () => boxOf(document.querySelector('[data-testid="workspace"]'));

describe('the card board overlay, against a long report', () => {
  /*
   * The regression, stated as the number that was wrong. `.workspace` is the
   * board's containing block, so it may not grow with the document — the panel
   * card's sticky travel has to come from somewhere that is not this box.
   */
  it('is one viewport tall even when the report is many', async () => {
    await browserPage.viewport(1200, VIEWPORT);
    render(<Page documentHeight={4000} boardOpen />);

    /* The premise: the report really is several viewports, so a workspace that
       tracked its content would be visibly wrong here. */
    expect(pageBox().height).toBeLessThan(VIEWPORT + 1);
    const board = boardBox();
    expect(board.height).toBeGreaterThan(200);
    expect(board.height).toBeLessThanOrEqual(VIEWPORT + 1);
  });

  /* And the same when it is closed, because a closed overlay is still mounted
     and still measured — `features/track/grid` says so in its own note. */
  it('is one viewport tall with the board closed, which is still mounted', async () => {
    await browserPage.viewport(1200, VIEWPORT);
    render(<Page documentHeight={4000} boardOpen={false} />);
    expect(workspaceBox().height).toBeLessThanOrEqual(VIEWPORT + 1);
  });
});
