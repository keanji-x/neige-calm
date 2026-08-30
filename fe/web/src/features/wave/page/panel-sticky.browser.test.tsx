/*
 * The panel card stays while the document scrolls under it.
 *
 * `page.module.css` says so in one word — `position: sticky` — and that word was
 * true and did nothing. Sticky may only travel inside its own containing block,
 * and the containing block was a `minmax(0, 1fr)` grid row: in a grid whose
 * height is definite, a `1fr` row is *exactly* the leftover and never grows, so
 * the box the card could move in stayed one viewport tall while the report grew
 * to three or four. The card held for about one screen and then slid off the top
 * like ordinary content. Measured on a real wave before the fix: y=68 at
 * scrollTop 400, y=−280 at 900, y=−1380 at 2000.
 *
 * **Only an engine can answer this.** jsdom computes no layout: `position` is a
 * string it stores, `getBoundingClientRect()` is zeroes, and nothing scrolls. A
 * unit test asserting `position: sticky` is in the stylesheet would have been
 * green for the entire life of the bug — the declaration was never the thing
 * that was missing.
 *
 * The fixture is the page's own three-level nesting built from the *production*
 * class names (`.page` → `.workspace` → `.content` → `.doc` / `.panel`), because
 * the defect is not in any one of them: it is in the chain, and each level
 * re-pinned to a viewport independently. A fixture that flattened it would be
 * measuring a page this app does not render.
 */
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade, and before the CSS Module — see the long note on import
   order in `features/chat/thread/thread.browser.test.tsx`. A module that
   declares `@layer features` of its own registers that layer first if it is
   imported first, and every override in the app then loses. */
import '../../../styles/entry.css';

import pageHeader from '../../../ui/page-header/page-header.module.css';
import styles from './page.module.css';

afterEach(() => { document.body.replaceChildren(); });

/** The wave page's skeleton: a header, then the document/panel pair, inside the
 *  page's own scrollport. `report` is a block tall enough to scroll. */
function Page({ documentHeight }: { documentHeight: number }) {
  return (
    /* `.main` publishes `--panel-span`, which `.content`'s trailing track reads.
       Its own height is what `.page`'s `flex: 1 1 auto` fills. */
    <div style={{ blockSize: 600, display: 'flex', flexDirection: 'column' }}>
      <section className={styles.page} data-testid="page">
        {/* The real page-header class, not a spacer div: `--header-band` and
            `--header-h` — which `.panel`'s sticky offset is a `calc()` of — are
            published by `ui/page-header`'s own `:has(> .header)` rule, and an
            unset custom property makes that `calc()` invalid, which silently
            turns the offset into `auto` and unsticks the card. The seam is part
            of what is under test. */}
        <div className={pageHeader.header} data-testid="header">Wave</div>
        <div className={styles.workspace}>
          <div className={styles.content}>
            <div className={styles.doc} data-testid="doc">
              <article style={{ blockSize: documentHeight }}>Report</article>
            </div>
            <aside className={styles.panel} data-nc-panel="" data-testid="panel">
              <div style={{ blockSize: 200 }}>Cards</div>
            </aside>
          </div>
        </div>
      </section>
    </div>
  );
}

const scroller = () => document.querySelector<HTMLElement>('[data-testid="page"]')!;
const panel = () => document.querySelector<HTMLElement>('[data-testid="panel"]')!;
const panelTop = () => Math.round(panel().getBoundingClientRect().top);

describe('the panel card, against a scrolling report', () => {
  it('holds its place for the whole scroll, not just the first screen', async () => {
    await browserPage.viewport(1200, 600);
    render(<Page documentHeight={4000} />);

    /* The premise, stated: this page really does scroll, and by much more than
       one viewport. Without it the assertions below would pass on a page with
       nothing to scroll — which is the shape the bug produced at the *bottom*
       of the range and would have hidden it. */
    const range = scroller().scrollHeight - scroller().clientHeight;
    expect(range).toBeGreaterThan(2000);

    const resting = panelTop();

    for (const scrollTop of [200, 600, 1200, 2400, range]) {
      scroller().scrollTop = scrollTop;
      /* The scroll really happened. Without this the case is green on a page
         that cannot scroll at all — take `overflow: auto` off `.page` and every
         assignment below is a no-op, every rect is unchanged, and "the panel
         held its place" is true for the wrong reason. */
      expect(scroller().scrollTop, 'the page did not scroll').toBe(scrollTop);
      /* Sticky is resolved during layout, so reading a rect is enough — there is
         no scroll event to wait for. */
      expect(
        panelTop(),
        `panel drifted at scrollTop ${scrollTop} (was ${resting} at rest)`,
      ).toBe(resting);
    }
  });

  /*
   * The eight-row cap, which had no layout assertion at all — `max-block-size`
   * and `overflow-y` are exactly the pair jsdom stores and never applies, so
   * every unit test of a long list was green with the rule and green without.
   *
   * A wave with thirty cards pushed TASKS and CONVERSATIONS below the fold; the
   * panel's whole point is that its modules are readable at once. The number is
   * `features/chat/list`'s, which had the treatment already.
   */
  it('caps a long list at eight rows and scrolls it, rather than growing', async () => {
    await browserPage.viewport(1200, 900);
    render(
      <div style={{ blockSize: 900, display: 'flex', flexDirection: 'column' }}>
        <section className={styles.page}>
          <div className={pageHeader.header}>Wave</div>
          <div className={styles.workspace}>
            <div className={styles.content}>
              <div className={styles.doc} />
              <aside className={styles.panel} data-nc-panel="">
                <ul className={styles.cards} data-testid="cards">
                  {Array.from({ length: 30 }, (_, index) => (
                    <li key={index}>
                      <button type="button" className={styles.cardRow}>card {index}</button>
                    </li>
                  ))}
                </ul>
              </aside>
            </div>
          </div>
        </section>
      </div>,
    );

    const list = document.querySelector<HTMLElement>('[data-testid="cards"]')!;
    /* The premise: there really is more content than the cap, so a list that
       ignored the cap would be visibly taller. */
    expect(list.scrollHeight).toBeGreaterThan(list.clientHeight * 2);

    /* Eight `--row-h-sm` rows plus seven `--space-1` gaps. Read off the tokens
       rather than hard-coded, so a change to either is a change to the cap and
       not a broken test. */
    const probe = document.createElement('div');
    probe.style.blockSize = 'calc(var(--row-h-sm) * 8 + var(--space-1) * 7)';
    document.body.append(probe);
    expect(Math.round(list.getBoundingClientRect().height))
      .toBe(Math.round(probe.getBoundingClientRect().height));
    probe.remove();

    /* And it is the list that scrolls, not the panel it sits in. */
    list.scrollTop = 200;
    expect(list.scrollTop).toBe(200);
  });

  /*
   * The other half, and the reason the fix is `flex: 1 0 auto` rather than a
   * `max-content` track: a short document must still *fill* the page. The
   * original `1fr` was there for that — an empty document has to be able to
   * centre itself in the space it owns rather than ending where the panel card
   * ends — and a fix that bought the scroll travel by giving that up would have
   * traded one visible defect for another.
   */
  it('still fills the page when the document is shorter than the window', async () => {
    await browserPage.viewport(1200, 600);
    render(<Page documentHeight={50} />);

    expect(scroller().scrollHeight).toBeLessThanOrEqual(scroller().clientHeight + 1);

    /* The document column reaches the bottom of the page rather than stopping at
       the panel card's own height. */
    const doc = document.querySelector<HTMLElement>('[data-testid="doc"]')!;
    expect(Math.round(doc.getBoundingClientRect().height))
      .toBeGreaterThan(Math.round(panel().getBoundingClientRect().height));
  });
});
