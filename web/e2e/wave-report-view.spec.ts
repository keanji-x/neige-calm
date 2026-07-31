import { test, expect, type APIResponse, type Page } from '@playwright/test';

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

test('wave report view renders real report data and report rail controls', async ({
  page,
}) => {
  await login(page);

  const ts = Date.now();
  const cove = await createCove(page, ts);
  const wave = await createWave(page, cove.id, ts);
  const body = 'Report smoke body with **markdown** content.';
  await writeReport(page, wave.id, body);

  await page.goto(`/calm/wave/${wave.id}`);
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/);
  await expect(
    page.getByRole('heading', { level: 1, name: wave.title }),
  ).toBeVisible();
  await expect(page.getByText('Report smoke body with')).toBeVisible();
  await expect(page.getByRole('tree', { name: /Wave files/i })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Outline' })).toBeVisible();
  await expect(
    page.getByRole('region', { name: 'Referenced documents' }),
  ).toBeVisible();
  await expect(page.getByRole('region', { name: 'Backlinks' })).toBeVisible();

  const conversationToggle = page.getByRole('button', {
    name: 'Open conversation drawer',
  });
  await expect(conversationToggle).toBeEnabled();

  // Sending is available only after opening the adjacent drawer.
  await conversationToggle.click();
  await expect(page.getByRole('complementary', { name: 'Conversation drawer' }))
    .toHaveClass(/report-conversation-drawer--open/);

  const followUp = page.getByRole('textbox', { name: /Ask the Spec Agent/ });
  await followUp.fill('Can you summarize the key risk?');
  await expect(followUp).toHaveValue('Can you summarize the key risk?');
});

test('narrow conversation drawer stays docked to the viewport', async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page);

  const ts = Date.now();
  const cove = await createCove(page, ts);
  const wave = await createWave(page, cove.id, ts);
  await writeReport(
    page,
    wave.id,
    Array.from({ length: 80 }, (_, index) => `Long report row ${index}.`).join(
      '\n\n',
    ),
  );

  await page.goto(`/calm/wave/${wave.id}`);

  const drawer = page.getByRole('complementary', {
    name: 'Conversation drawer',
  });
  const panel = drawer.locator('.report-conversation-drawer-panel');
  const openToggle = page.getByRole('button', {
    name: 'Open conversation drawer',
  });
  await expect(panel).toHaveCSS('height', '0px');
  await expect.poll(async () => {
    const box = await openToggle.boundingBox();
    return box == null ? undefined : box.y + box.height;
  }).toBeCloseTo(await page.evaluate(() => window.innerHeight), 0);

  await openToggle.click();
  const closeToggle = page.getByRole('button', {
    name: 'Close conversation drawer',
  });
  await expect.poll(async () => {
    const box = await panel.boundingBox();
    const viewportHeight = await page.evaluate(() => window.innerHeight);
    return box == null ? undefined : box.height / viewportHeight;
  }).toBeCloseTo(0.58, 2);
  await expect.poll(async () => {
    const box = await panel.boundingBox();
    const viewportHeight = await page.evaluate(() => window.innerHeight);
    return box == null ? undefined : box.y + box.height - viewportHeight;
  }).toBeCloseTo(0, 0);
  const scrollPosition = await page.locator('.report-body p').last().evaluate(
    (paragraph) => {
      let root = paragraph.parentElement;
      while (root != null) {
        const overflowY = getComputedStyle(root).overflowY;
        if (
          (overflowY === 'auto' || overflowY === 'scroll') &&
          root.scrollHeight > root.clientHeight
        ) {
          root.scrollTop = root.scrollHeight;
          return {
            scrollTop: root.scrollTop,
            maxScrollTop: root.scrollHeight - root.clientHeight,
          };
        }
        root = root.parentElement;
      }
      throw new Error('report has no effective scrolling ancestor');
    },
  );
  expect(scrollPosition.scrollTop).toBeGreaterThan(0);
  expect(scrollPosition.scrollTop).toBeCloseTo(
    scrollPosition.maxScrollTop,
    0,
  );
  await expect.poll(async () => {
    const paragraph = await page.locator('.report-body p').last().boundingBox();
    const panelBox = await panel.boundingBox();
    return paragraph == null || panelBox == null
      ? undefined
      : paragraph.y + paragraph.height - panelBox.y;
  }).toBeLessThanOrEqual(0);
  await expect.poll(async () => {
    const box = await panel.boundingBox();
    const viewportHeight = await page.evaluate(() => window.innerHeight);
    return box == null ? undefined : box.y + box.height - viewportHeight;
  }).toBeCloseTo(0, 0);

  await closeToggle.click();
  await expect(panel).toHaveCSS('height', '0px');
  await expect.poll(async () => {
    const box = await openToggle.boundingBox();
    const viewportHeight = await page.evaluate(() => window.innerHeight);
    return box == null ? undefined : box.y + box.height - viewportHeight;
  }).toBeCloseTo(0, 0);
});

test('report H2 counters match the outline sequence across prose blocks', async ({
  page,
}) => {
  await login(page);

  const ts = Date.now();
  const cove = await createCove(page, ts);
  const wave = await createWave(page, cove.id, ts);
  await writeReport(
    page,
    wave.id,
    [
      '# Counter report',
      '',
      '## First section',
      '',
      'First section body.',
      '',
      '## Second section',
      '',
      'Second section body.',
      '',
      '## Third section',
      '',
      'Third section body.',
    ].join('\n'),
  );

  await page.goto(`/calm/wave/${wave.id}`);

  const headings = page.locator('.report-body .report-prose h2');
  await expect(headings).toHaveCount(3);
  const computedBeforeContents = await headings.evaluateAll((elements) =>
    elements.map((element) =>
      getComputedStyle(element, '::before').content,
    ),
  );
  // Chromium exposes the computed counter expression, not its painted value.
  expect(computedBeforeContents).toEqual(
    Array(3).fill('counter(report-h2, decimal-leading-zero) " "'),
  );
  const headingLabels = await headings.allTextContents();
  const session = await page.context().newCDPSession(page);
  const { nodes } = await session.send('Accessibility.getFullAXTree');
  const nodesById = new Map(nodes.map((node) => [node.nodeId, node]));
  const renderedBodyCounters = headingLabels.map((label) => {
    const heading = nodes.find(
      (node) =>
        node.role?.value === 'heading' &&
        String(node.name?.value).trim() === label,
    );
    const pending = [...(heading?.childIds ?? [])];
    while (pending.length > 0) {
      const node = nodesById.get(pending.shift()!);
      if (
        node?.role?.value === 'StaticText' &&
        /^\d{2}$/.test(String(node.name?.value))
      ) {
        return node.name?.value;
      }
      pending.push(...(node?.childIds ?? []));
    }
    return undefined;
  });
  expect(renderedBodyCounters).toEqual(['01', '02', '03']);

  const outlineCounters = await page
    .locator('.report-outline-list > li .report-outline-number')
    .allTextContents();
  expect(renderedBodyCounters).toEqual(outlineCounters);
});
