import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import { renderPage } from '../../features/track/page/test-fixtures.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { nativeViewPayloadSchema } from '../../../../core/domain/report-view.ts';
import { trackOverlayPayload, type OverlayWire } from '../../../../core/domain/track.ts';
import demoSource from '../../../../../plugins/paper-trading/examples/native-demo.json?raw';
import manifestSource from '../../../../../plugins/paper-trading/manifest.json?raw';

afterEach(cleanup);

it('keeps the ordinary Track inventory beside a native report', async () => {
  await page.viewport(1440, 1000);
  const demo = JSON.parse(demoSource) as { views: unknown[]; overlays: Record<string, unknown> };
  const plugin = (JSON.parse(manifestSource) as { id: string }).id;
  const overlays: OverlayWire[] = Object.entries(demo.overlays).map(([kind, unit]) => ({
    id: kind, plugin_id: plugin, entity_kind: 'track', entity_id: 'example', kind, payload: unit, updated_at: 0,
  }));
  const payload = nativeViewPayloadSchema.parse(demo.views[0]);
  renderPage({ report: <ReportDocument report={{ summary: '', body: '', blocks: [{ id: 'native', kind: 'view', payload }] }}
    resolveOverlay={source => trackOverlayPayload('example', overlays, source)} empty={<p>Empty</p>} />,
    conversationList: <button type="button">Existing Planner</button> });
  await expect.element(page.getByRole('heading', { name: 'Cards', exact: true })).toBeVisible();
  await expect.element(page.getByText('总资产', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('heading', { name: 'Tasks', exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Existing Planner' })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Show track panel' })).not.toBeInTheDocument();
  const panel = document.querySelector('[data-nc-panel]')!;
  expect(panel.getBoundingClientRect().width).toBeGreaterThan(200);
  expect(document.getElementById('native')!.getBoundingClientRect().right).toBeLessThanOrEqual(panel.getBoundingClientRect().left);
});
