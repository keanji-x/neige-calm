/*
 * The cove page's panel card, against a scrolling report — the same claim, the
 * same defect and the same fix as `features/wave/page/panel-sticky.browser.test`,
 * where the mechanism is written out in full.
 *
 * It is a second file rather than a second case in that one because the two
 * pages are two stylesheets: the cove page hangs `.content` directly off
 * `.page` while the wave page interposes `.workspace`, and it was the *chain*
 * that pinned the sticky card to one viewport. A shared fixture would have had
 * to pick one shape, and the shape is the thing under test.
 */
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';

import pageHeader from '../../../ui/page-header/page-header.module.css';
import styles from './page.module.css';

afterEach(() => { document.body.replaceChildren(); });

function Page({ documentHeight }: { documentHeight: number }) {
  return (
    <div style={{ blockSize: 600, display: 'flex', flexDirection: 'column' }}>
      <div className={styles.page} data-testid="page">
        {/* The real header class: `.panel`'s sticky offset is a `calc()` of
            `--header-band` / `--header-h`, which `ui/page-header` publishes off
            `:has(> .header)` — unset, the `calc()` is invalid and the offset
            silently becomes `auto`. */}
        <div className={pageHeader.header}>Cove</div>
        <div className={styles.content} data-testid="content">
          <div className={styles.doc}>
            <article style={{ blockSize: documentHeight }}>Report</article>
          </div>
          <aside className={styles.panel} data-nc-panel="" data-testid="panel">
            <div style={{ blockSize: 200 }}>Conversations</div>
          </aside>
        </div>
      </div>
    </div>
  );
}

const scroller = () => document.querySelector<HTMLElement>('[data-testid="page"]')!;
const panelTop = () => Math.round(
  document.querySelector<HTMLElement>('[data-testid="panel"]')!.getBoundingClientRect().top,
);

describe('the cove page panel card', () => {
  it('holds its place for the whole scroll, not just the first screen', async () => {
    await browserPage.viewport(1200, 600);
    render(<Page documentHeight={4000} />);

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
      expect(panelTop(), `panel drifted at scrollTop ${scrollTop}`).toBe(resting);
    }
  });

  /*
   * **The name is "fills", so the assertion has to be about filling.** This
   * used to check only `scrollHeight <= clientHeight` — that the short page does
   * not scroll — which is true of a `.content` that has collapsed to its panel
   * card's height as well. Both review channels caught it independently:
   * removing the `flex: 1 0 auto` under test left the case green.
   */
  it('still fills the page when the document is shorter than the window', async () => {
    await browserPage.viewport(1200, 600);
    render(<Page documentHeight={50} />);
    expect(scroller().scrollHeight).toBeLessThanOrEqual(scroller().clientHeight + 1);

    /* The row reaches the bottom of the page rather than stopping at the taller
       of its two columns — which, with a 50px document and a 200px card, is the
       card. */
    const content = document.querySelector<HTMLElement>('[data-testid="content"]')!;
    expect(content.getBoundingClientRect().height).toBeGreaterThan(400);
  });
});
