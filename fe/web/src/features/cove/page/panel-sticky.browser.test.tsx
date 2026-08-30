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
        <div className={styles.content}>
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
      expect(panelTop(), `panel drifted at scrollTop ${scrollTop}`).toBe(resting);
    }
  });

  it('still fills the page when the document is shorter than the window', async () => {
    await browserPage.viewport(1200, 600);
    render(<Page documentHeight={50} />);
    expect(scroller().scrollHeight).toBeLessThanOrEqual(scroller().clientHeight + 1);
  });
});
