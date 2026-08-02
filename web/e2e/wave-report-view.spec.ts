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

function harnessUserRow(id: number, text: string) {
  return {
    id,
    runtime_id: 'runtime',
    card_id: 'card_spec',
    wave_id: 'wave',
    thread_id: 'thread',
    turn_id: 'turn',
    item_uuid: `msg_${id}`,
    item_type: 'userMessage',
    method: 'item/completed',
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        content: [{ text: `User says:\n${text}`, type: 'text' }],
        id: `msg_${id}`,
        type: 'userMessage',
      },
      threadId: 'thread',
      turnId: 'turn',
    }),
    created_at_ms: 1780977420000 + id,
  };
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

test('report activity panel floats, scrolls internally, and disables with the drawer', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1700, height: 900 });
  await login(page);

  const ts = Date.now();
  const cove = await createCove(page, ts);
  const wave = await createWave(page, cove.id, ts);
  await writeReport(page, wave.id, 'Activity panel geometry report.');
  await page.route('**/api/cards/*/harness/items*', async (route) => {
    await route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify(
        Array.from({ length: 5 }, (_, index) =>
          harnessUserRow(index + 1, `Instruction ${index + 1}`),
        ),
      ),
    });
  });

  await page.goto(`/calm/wave/${wave.id}`);
  // CSS remains resolvable after the aside is removed from the a11y tree.
  const stack = page.locator('.report-activity-stack');
  const panel = stack.locator('.report-activity-panel');
  const rows = stack.locator('.report-activity-rows');
  const document = page.locator('.report-doc');
  await expect(stack.getByRole('button')).toHaveCount(5);

  const scrollRoot = page.locator('.report-document-scroll');
  await scrollRoot.evaluate((node) => {
    node.style.width = '1200px';
  });
  const stickyGeometry = await scrollRoot
    .evaluate((scrollRoot) => {
      const stackNode = scrollRoot.querySelector('.report-activity-stack');
      if (!(stackNode instanceof HTMLElement)) {
        throw new Error('Activity stack not found');
      }
      return {
        stackHeight: stackNode.getBoundingClientRect().height,
        position: getComputedStyle(stackNode).position,
      };
    });
  // At a fixed 1200px container width the sticky overlay never occupies flow.
  expect(stickyGeometry.position).toBe('sticky');
  expect(stickyGeometry.stackHeight).toBe(0);

  await scrollRoot.evaluate((node) => {
    node.style.width = '900px';
  });
  const staticGeometry = await scrollRoot.evaluate((scrollRoot) => {
    const stackNode = scrollRoot.querySelector('.report-activity-stack');
    const panelNode = scrollRoot.querySelector('.report-activity-panel');
    const documentNode = scrollRoot.querySelector('.report-doc');
    if (
      !(stackNode instanceof HTMLElement) ||
      !(panelNode instanceof HTMLElement) ||
      !(documentNode instanceof HTMLElement)
    ) {
      throw new Error('Activity stack, panel, or report document not found');
    }
    const panelBottom = panelNode.getBoundingClientRect().bottom;
    const documentTop = documentNode.getBoundingClientRect().top;
    stackNode.style.display = 'none';
    const documentTopWithoutPanel = documentNode.getBoundingClientRect().top;
    stackNode.style.removeProperty('display');
    return {
      position: getComputedStyle(stackNode).position,
      stackHeight: stackNode.getBoundingClientRect().height,
      panelBottom,
      documentTop,
      documentTopWithoutPanel,
    };
  });
  // At a fixed 900px container width the visible static panel occupies its
  // real height and finishes before the document begins.
  expect(staticGeometry.position).toBe('static');
  expect(staticGeometry.stackHeight).toBeGreaterThan(0);
  expect(staticGeometry.documentTop).toBeGreaterThan(
    staticGeometry.documentTopWithoutPanel,
  );
  expect(staticGeometry.panelBottom).toBeLessThanOrEqual(
    staticGeometry.documentTop,
  );
  const beforeScroll = await rows.evaluate((node) => ({
    clientHeight: node.clientHeight,
    scrollHeight: node.scrollHeight,
  }));
  expect(beforeScroll.clientHeight).toBe(140);
  expect(beforeScroll.scrollHeight).toBeGreaterThan(beforeScroll.clientHeight);
  await rows.evaluate((node) => {
    node.scrollTop = node.scrollHeight;
  });
  await expect.poll(() => rows.evaluate((node) => node.scrollTop))
    .toBeGreaterThan(0);

  await stack.getByRole('button', { name: 'Instruction 5' }).click();
  await expect(stack).toHaveClass(/hide/);
  // The same fixed 900px container now exercises the hidden static branch:
  // it must collapse to the exact document position measured with no stack.
  await expect.poll(async () => (await document.boundingBox())?.y)
    .toBeCloseTo(staticGeometry.documentTopWithoutPanel, 0);
  await expect(stack).toHaveCSS('opacity', '0');
  await expect(panel).toHaveCSS('pointer-events', 'none');
  await expect(stack.locator('.report-activity-card').first()).toHaveAttribute(
    'tabindex',
    '-1',
  );
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
      'Counter report.',
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

test('pure H1 report counters match the outline sequence', async ({ page }) => {
  await login(page);

  const ts = Date.now();
  const cove = await createCove(page, ts);
  const wave = await createWave(page, cove.id, ts);
  await writeReport(
    page,
    wave.id,
    [
      '# Summary',
      '',
      'Summary body.',
      '',
      '# Findings',
      '',
      'Findings body.',
      '',
      '# Recommendation',
      '',
      'Recommendation body.',
    ].join('\n'),
  );

  await page.goto(`/calm/wave/${wave.id}`);

  const headings = page.locator('.report-body .report-prose h1');
  await expect(headings).toHaveCount(3);
  const computedBeforeContents = await headings.evaluateAll((elements) =>
    elements.map((element) => getComputedStyle(element, '::before').content),
  );
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

test('report heading rules preserve heading geometry', async ({ page }) => {
  await login(page);

  const ts = Date.now();
  const cove = await createCove(page, ts);
  const wave = await createWave(page, cove.id, ts);
  await writeReport(
    page,
    wave.id,
    [
      '## [Short heading](https://example.com/spec)',
      '',
      '##',
      '',
      '## Heading with `inline code`',
      '',
      '## A deliberately long heading that must wrap onto another line in the report prose column so the trailing rule follows its final line',
    ].join('\n'),
  );

  await page.goto(`/calm/wave/${wave.id}`);

  const headings = page.locator('.report-body .report-prose h2');
  await expect(headings).toHaveCount(4);
  const boxes = await headings.evaluateAll((elements) =>
    elements.map((element) => ({
      height: element.getBoundingClientRect().height,
    })),
  );
  // The empty second heading only has generated inline content. Returning its
  // counter to a float would leave that heading at 0px high.
  for (const box of boxes) {
    expect(box.height).toBeGreaterThan(0);
  }

  const shortHeading = headings.first();
  const shortMetrics = await shortHeading.evaluate((element) => {
    const style = getComputedStyle(element);
    return {
      height: element.getBoundingClientRect().height,
      lineHeight: Number.parseFloat(style.lineHeight),
      backgroundImage: style.backgroundImage,
      overflowX: style.overflowX,
      overflowY: style.overflowY,
    };
  });
  // Mechanism and geometry contract: making ::after a block, or otherwise
  // moving it onto its own line, makes this short heading taller than one line.
  expect(shortMetrics.height).toBeCloseTo(shortMetrics.lineHeight, 0);
  // This specifically forbids the former background-image implementation of
  // the full-column rule; the geometry assertions above cover its layout.
  expect(shortMetrics.backgroundImage).toBe('none');
  // The generated rule deliberately overflows inline: horizontal clipping is
  // load-bearing, while the visible block axis leaves focus decoration free.
  expect(shortMetrics.overflowX).toBe('clip');
  expect(shortMetrics.overflowY).toBe('visible');
  const headingLink = shortHeading.getByRole('link');
  await headingLink.focus();
  await expect(headingLink).toBeFocused();

  // A heading's own scrollWidth can include clipped ink. The observable crop
  // contract is that it must not enlarge the document scroll surface; deleting
  // overflow-x: clip makes the 100%-wide generated rule fail this assertion.
  const documentOverflow = await page
    .locator('.report-document-scroll')
    .evaluate((element) => ({
      scrollWidth: element.scrollWidth,
      clientWidth: element.clientWidth,
    }));
  expect(documentOverflow.scrollWidth)
    .toBeLessThanOrEqual(documentOverflow.clientWidth + 1);
});

test('collapsed report rail opener stays inside the report shell', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1600, height: 900 });
  await login(page);

  const ts = Date.now();
  const cove = await createCove(page, ts);
  const wave = await createWave(page, cove.id, ts);
  await writeReport(page, wave.id, 'Report body');
  await page.goto(`/calm/wave/${wave.id}`);
  await page.getByRole('button', { name: 'Collapse report rail' }).click();

  const contentGeometry = await page.locator('.report-center').evaluate(
    (center) => {
      const scrollRoot = center.querySelector('.report-document-scroll');
      const document = center.querySelector('.report-doc');
      if (!(scrollRoot instanceof HTMLElement) ||
          !(document instanceof HTMLElement)) {
        throw new Error('Report content hierarchy not found');
      }
      return {
        centerWidth: center.getBoundingClientRect().width,
        scrollWidth: scrollRoot.getBoundingClientRect().width,
        documentWidth: document.getBoundingClientRect().width,
        documentVisible: getComputedStyle(document).visibility === 'visible',
      };
    },
  );
  // At 1600×900, removing the rail grid item must leave the explicitly placed
  // center column and its document visible and measurably wide.
  expect(contentGeometry.centerWidth).toBeGreaterThan(0);
  expect(contentGeometry.scrollWidth).toBeGreaterThan(0);
  expect(contentGeometry.documentWidth).toBeGreaterThan(0);
  expect(contentGeometry.documentVisible).toBe(true);

  const opener = page.getByRole('button', { name: 'Expand report rail' });
  await expect(opener).toBeVisible();
  const geometry = await opener.evaluate((button) => {
    const shell = button.closest('.report-page');
    if (!(shell instanceof HTMLElement)) {
      throw new Error('Report shell not found');
    }
    const shellRect = shell.getBoundingClientRect();
    const buttonRect = button.getBoundingClientRect();
    return {
      hasShellOffsetParent: button.offsetParent === shell,
      left: buttonRect.left - shellRect.left,
      top: buttonRect.top - shellRect.top,
      right: shellRect.right - buttonRect.right,
      bottom: shellRect.bottom - buttonRect.bottom,
    };
  });
  expect(geometry.hasShellOffsetParent).toBe(true);
  expect(geometry.left).toBeGreaterThanOrEqual(-1);
  expect(geometry.left).toBeLessThanOrEqual(1);
  expect(geometry.top).toBeGreaterThanOrEqual(-1);
  expect(geometry.right).toBeGreaterThanOrEqual(-1);
  expect(geometry.bottom).toBeGreaterThanOrEqual(-1);
});
