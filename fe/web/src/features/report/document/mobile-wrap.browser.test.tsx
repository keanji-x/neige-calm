import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';

import styles from './document.module.css';

afterEach(() => { document.body.replaceChildren(); });

describe('Report mobile measure', () => {
  it('collapses the desktop document grid and wraps long text inside the viewport', async () => {
    await page.viewport(390, 844);
    render(
      <article className={styles.doc} data-testid="report-document">
        <div className={styles.row}>
          <section className={styles.block}>
            <p>A_long_unbroken_report_identifier_that_must_wrap_instead_of_widening_the_mobile_viewport</p>
          </section>
        </div>
      </article>,
    );

    const document = page.getByTestId('report-document');
    const element = await document.findElement();
    expect(element.scrollWidth).toBeLessThanOrEqual(element.clientWidth);
    expect(element.getBoundingClientRect().width).toBeLessThanOrEqual(window.innerWidth);
  });
});
