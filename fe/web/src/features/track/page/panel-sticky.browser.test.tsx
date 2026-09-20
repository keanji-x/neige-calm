/* The panel card stays while the document scrolls under it. Sticky may only travel inside its containing block, and jsdom computes no layout, so the fixture is the page's own production nesting. */
import { InventoryGroups } from './inventory-groups.tsx';
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade, and before the CSS Module: a module that declares `@layer features` of its own registers that layer first if imported first, and every override in the app then loses. */
import '../../../styles/entry.css';

import pageHeader from '../../../ui/page-header/page-header.module.css';
import styles from './page.module.css';

afterEach(() => { document.body.replaceChildren(); });

/** The track page's skeleton inside the page's own scrollport; `report` is a block tall enough to scroll. */
function Page({ documentHeight }: { documentHeight: number }) {
  return (
    <div style={{ blockSize: 600, display: 'flex', flexDirection: 'column' }}>
      <section className={styles.page} data-testid="page">
        {/* The real page-header class, not a spacer div: `.panel`'s sticky offset is a `calc()` of `--header-band` / `--header-h`, which `ui/page-header` publishes; unset, the `calc()` is invalid and the card unsticks. */}
        <div className={pageHeader.header} data-testid="header">Track</div>
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

    const range = scroller().scrollHeight - scroller().clientHeight;
    expect(range).toBeGreaterThan(2000);

    const resting = panelTop();

    for (const scrollTop of [200, 600, 1200, 2400, range]) {
      scroller().scrollTop = scrollTop;
      expect(scroller().scrollTop, 'the page did not scroll').toBe(scrollTop);
      /* Sticky is resolved during layout, so reading a rect is enough — there is no scroll event to wait for. */
      expect(
        panelTop(),
        `panel drifted at scrollTop ${scrollTop} (was ${resting} at rest)`,
      ).toBe(resting);
    }
  });

  /* The eight-row cap: `max-block-size` and `overflow-y` are exactly the pair jsdom stores and never applies. */
  it('caps a long disclosed group and scrolls it, rather than growing', async () => {
    await browserPage.viewport(1200, 900);
    render(
      <div style={{ blockSize: 900, display: 'flex', flexDirection: 'column' }}>
        <section className={styles.page}>
          <div className={pageHeader.header}>Track</div>
          <div className={styles.workspace}>
            <div className={styles.content}>
              <div className={styles.doc} />
              <aside className={styles.panel} data-nc-panel="">
                <InventoryGroups noun="card" groups={[{
                  key: 'working', label: 'In progress', expanded: true,
                  rows: Array.from({ length: 30 }, (_, index) => index),
                }]} renderRows={rows => <ul className={styles.cards}>
                  {rows.map(index => <li key={index}><button type="button" className={styles.cardRow}>card {index}</button></li>)}
                </ul>} />
              </aside>
            </div>
          </div>
        </section>
      </div>,
    );

    const list = document.querySelector<HTMLElement>('[data-nc-inventory-group] summary + div')!;
    expect(list.scrollHeight).toBeGreaterThan(list.clientHeight * 2);

    expect(list.getBoundingClientRect().height).toBeLessThanOrEqual(180);

    list.scrollTop = 200;
    expect(list.scrollTop).toBe(200);
  });

  /* The other half: a short document must still fill the page. The height lives on the row as `minmax(max-content, 1fr)`; `flex: 1 0 auto` on `.workspace` would make the card board as tall as the report. */
  it('still fills the page when the document is shorter than the window', async () => {
    await browserPage.viewport(1200, 600);
    render(<Page documentHeight={50} />);

    expect(scroller().scrollHeight).toBeLessThanOrEqual(scroller().clientHeight + 1);

    const doc = document.querySelector<HTMLElement>('[data-testid="doc"]')!;
    expect(Math.round(doc.getBoundingClientRect().height))
      .toBeGreaterThan(Math.round(panel().getBoundingClientRect().height));
  });
});
