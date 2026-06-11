import { test, expect, type APIResponse, type Page } from '@playwright/test';
import { seedWaveViewMode } from './helpers/reset';

const createdCoveIds: string[] = [];

test.beforeEach(() => {
  createdCoveIds.length = 0;
});

test.afterEach(async ({ request }) => {
  for (const id of createdCoveIds) {
    const res = await request.delete(`/api/coves/${id}`);
    if (!res.ok() && res.status() !== 404) {
      throw new Error(
        `cleanup: DELETE /api/coves/${id} -> ${res.status()} ${res.statusText()}`,
      );
    }
  }
  createdCoveIds.length = 0;
});

async function expectOk(res: APIResponse, label: string): Promise<void> {
  if (res.ok()) return;
  const body = await res.text().catch(() => '<unreadable>');
  throw new Error(`${label} -> ${res.status()} ${res.statusText()}: ${body}`);
}

async function blockFonts(page: Page): Promise<void> {
  await page.route('**://fonts.googleapis.com/**', (route) => route.abort());
  await page.route('**://fonts.gstatic.com/**', (route) => route.abort());
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

async function createCove(page: Page, ts: number): Promise<{ id: string }> {
  const res = await page.request.post('/api/coves', {
    data: { name: `E2E report view ${ts}`, color: '#4a8' },
    headers: { 'content-type': 'application/json' },
  });
  await expectOk(res, 'POST /api/coves');
  const cove = (await res.json()) as { id: string };
  createdCoveIds.push(cove.id);
  return cove;
}

async function createWave(
  page: Page,
  coveId: string,
  ts: number,
): Promise<{ id: string; title: string }> {
  const title = `E2E report view wave ${ts}`;
  const res = await page.request.post('/api/waves', {
    data: {
      cove_id: coveId,
      title,
      cwd: `/tmp/playwright-report-view-${ts}`,
      attach_folder: true,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  await expectOk(res, 'POST /api/waves');
  const wave = (await res.json()) as { id: string };
  return { id: wave.id, title };
}

async function writeReport(page: Page, waveId: string, body: string): Promise<void> {
  const res = await page.request.post(`/api/waves/${waveId}/report`, {
    data: { summary: 'report view smoke', body },
    headers: { 'content-type': 'application/json' },
  });
  await expectOk(res, 'POST /api/waves/:id/report');
}

test('wave report view renders real report data and staged rail controls', async ({
  page,
}) => {
  await blockFonts(page);
  await login(page);

  const ts = Date.now();
  const cove = await createCove(page, ts);
  const wave = await createWave(page, cove.id, ts);
  const body = 'Report smoke body with **markdown** content.';
  await writeReport(page, wave.id, body);
  await seedWaveViewMode(page.request, wave.id, 'report');

  await page.goto(`/calm/wave/${wave.id}`);
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/);
  await expect(
    page.getByRole('heading', { level: 1, name: wave.title }),
  ).toBeVisible();
  await expect(page.getByText('Report smoke body with')).toBeVisible();
  await expect(page.getByRole('tree', { name: /Wave files/i })).toBeVisible();
  await expect(
    page.getByText('Activity timeline appears here. (Wired in PR-E.)'),
  ).toBeVisible();

  const chat = page.getByRole('button', { name: /Ask the Research Agent/ });
  await expect(chat).toBeVisible();
  await chat.click();

  const chatBox = page.getByRole('region', { name: /Ask the Research Agent/ });
  await expect(chatBox).toBeVisible();
  const followUp = chatBox.getByRole('textbox', { name: /Follow-up/ });
  await followUp.fill('Can you summarize the key risk?');
  await expect(followUp).toHaveValue('Can you summarize the key risk?');
});
