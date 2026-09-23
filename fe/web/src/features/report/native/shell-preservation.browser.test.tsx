import '../../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import { renderPage } from '../../track/page/test-fixtures.tsx';
import { ReportDocument } from '../document/public.tsx';
import { nativeViewPayloadSchema } from '../../../../../core/domain/report-view.ts';
import source from '../../../../../../plugins/paper-trading/examples/native-demo.json?raw';

afterEach(cleanup);

it('keeps the ordinary Track inventory beside a native report', async () => {
  await page.viewport(1440, 1000);
  const payload = nativeViewPayloadSchema.parse(JSON.parse(source));
  renderPage({ report: <ReportDocument report={{ summary: '', body: '', blocks: [{ id: 'native', kind: 'view', payload }] }} empty={<p>Empty</p>} />,
    conversationList: <button type="button">Existing Planner</button> });
  await expect.element(page.getByRole('heading', { name: 'Cards', exact: true })).toBeVisible();
  await expect.element(page.getByRole('heading', { name: 'Tasks', exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Existing Planner' })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Show track panel' })).not.toBeInTheDocument();
  const panel = document.querySelector('[data-nc-panel]')!;
  expect(panel.getBoundingClientRect().width).toBeGreaterThan(200);
  expect(document.getElementById('native')!.getBoundingClientRect().right).toBeLessThanOrEqual(panel.getBoundingClientRect().left);
});
