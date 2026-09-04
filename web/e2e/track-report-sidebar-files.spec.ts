import { test, expect, type APIResponse, type Page } from '@playwright/test';

const createdAreaIds: string[] = [];

test.beforeEach(() => {
  createdAreaIds.length = 0;
});

test.afterEach(async ({ page }) => {
  for (const id of createdAreaIds) {
    const res = await page.request.delete(`/api/areas/${id}`);
    if (!res.ok() && res.status() !== 404) {
      throw new Error(
        `cleanup: DELETE /api/areas/${id} -> ${res.status()} ${res.statusText()}`,
      );
    }
  }
  createdAreaIds.length = 0;
});

async function expectOk(res: APIResponse, label: string): Promise<void> {
  if (res.ok()) return;
  const body = await res.text().catch(() => '<unreadable>');
  throw new Error(`${label} -> ${res.status()} ${res.statusText()}: ${body}`);
}

async function login(page: Page): Promise<void> {
  const res = await page.request.post('/api/auth/login', {
    data: {
      username: process.env.PROBE_USERNAME ?? 'owner',
      password: process.env.PROBE_PASSWORD ?? 'dev',
    },
    headers: { 'content-type': 'application/json' },
  });
  await expectOk(res, 'POST /api/auth/login');
}

async function createArea(page: Page, ts: number): Promise<{ id: string }> {
  const res = await page.request.post('/api/areas', {
    data: { name: `E2E report files ${ts}`, color: '#4a8' },
    headers: { 'content-type': 'application/json' },
  });
  await expectOk(res, 'POST /api/areas');
  const area = (await res.json()) as { id: string };
  createdAreaIds.push(area.id);
  return area;
}

async function createTrack(
  page: Page,
  areaId: string,
  ts: number,
): Promise<{ id: string; title: string }> {
  const title = `E2E report file track ${ts}`;
  const res = await page.request.post('/api/tracks', {
    data: {
      area_id: areaId,
      title,
      // #1147 S3 — no `cwd`: take the kernel-managed workspace branch.
      // This test is about the report sidebar, not working directories.
      // See `helpers/reset.ts::createTrackInArea` for why the invented
      // `/tmp/playwright-report-files-<ts>` attached path was never valid.
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  await expectOk(res, 'POST /api/tracks');
  const track = (await res.json()) as { id: string };
  return { id: track.id, title };
}

async function writeReport(page: Page, trackId: string, body: string): Promise<void> {
  const res = await page.request.post(`/api/tracks/${trackId}/report`, {
    data: { summary: 'report files smoke', body, ifDocRev: 0 },
    headers: { 'content-type': 'application/json' },
  });
  await expectOk(res, 'POST /api/tracks/:id/report');
}

test('TrackReportPage Files rail renders a selectable track-fs tree', async ({
  page,
}) => {
  await login(page);

  const ts = Date.now();
  const area = await createArea(page, ts);
  const track = await createTrack(page, area.id, ts);
  await writeReport(page, track.id, 'Report file tree body.');

  await page.goto(`/calm/track/${track.id}`);
  await expect(page).toHaveURL(/\/calm\/track\/[^/]+$/);

  // H1 only renders inside TrackReportPage.
  await expect(
    page.getByRole('heading', { level: 1, name: track.title }),
  ).toBeVisible();

  const filesSection = page.getByRole('region', { name: 'Files' });
  await expect(filesSection).toBeVisible();

  const tree = filesSection.getByRole('tree', { name: /Track files/i });
  await expect(tree).toBeVisible();

  const reportFile = tree.getByRole('treeitem', { name: /report\.md/ });
  await expect(reportFile).toBeVisible();
  await reportFile.click();
  await expect(reportFile).toHaveAttribute('aria-selected', 'true');
  await expect(filesSection.locator('.track-report-files-viewer')).toHaveCount(0);
});
