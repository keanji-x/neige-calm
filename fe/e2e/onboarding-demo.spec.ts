import { expect, test, type Page } from '@playwright/test';
import { wsUrl } from '../web/src/systems/events/websocket-driver.ts';

// Exercise transport isolation even when the document's defense-in-depth CSP is bypassed.
test.use({ bypassCSP: true });

function isBackendEndpoint(url: URL): boolean {
  const apiNamespace = new URL('.', wsUrl({ protocol: 'http:', host: url.host })).pathname;
  return url.pathname === apiNamespace.slice(0, -1) || url.pathname.startsWith(apiNamespace);
}

const note = (page: Page) => page.locator('[data-nc-guide-hint]:popover-open[aria-hidden="false"], [data-nc-guide-hint]:popover-open:not([aria-hidden])');
const target = async (page: Page) => {
  const id = await note(page).getAttribute('id');
  return page.locator(`[aria-describedby~=${JSON.stringify(id)}]`);
};

/** Every user transition gets a fresh visible instruction; anchored ones also identify their target. */
async function continuity(page: Page, timeout = 300) {
  await expect.poll(() => page.evaluate(() => {
    const visible = (node: Element | null) => {
      if (node === null || node.closest('[hidden], [inert], [aria-hidden="true"]') !== null) return false;
      const rect = node.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0 && getComputedStyle(node).visibility !== 'hidden';
    };
    const hint = document.querySelector('[data-nc-guide-hint]:popover-open:not([aria-hidden="true"])');
    // A menu may replace its outgoing callout with a status in the same frame.
    // Sample the instruction, reference and ring together rather than awaiting a withdrawn note.
    if (!visible(hint)) return visible(document.querySelector('[role="status"][aria-label="引导下一步"]'));
    const reference = Array.from(document.querySelectorAll('[aria-describedby]')).find(node =>
      node.getAttribute('aria-describedby')?.split(' ').includes(hint?.id ?? ''));
    if (reference === undefined || !visible(reference)) return false;
    const box = reference.getBoundingClientRect();
    return Array.from(document.querySelectorAll('[data-nc-guide-ring]:popover-open')).some(ring => {
      const rect = ring.getBoundingClientRect();
      return getComputedStyle(ring).visibility === 'visible'
        && Math.abs(rect.x - box.x) < 2 && Math.abs(rect.y - box.y) < 2
        && Math.abs(rect.width - box.width) < 2 && Math.abs(rect.height - box.height) < 2;
    });
  }), { timeout, intervals: [16, 32, 50] }).toBe(true);
}

for (const width of [1440, 390]) {
  test.describe(`${width}px`, () => {
    test.use({ isMobile: width === 390, hasTouch: width === 390 });
    test(`the visible guide completes the isolated lesson without a silent gap at ${width}px`, async ({ page }) => {
      test.setTimeout(90_000);
      await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 });
      const api: string[] = []; const sockets: string[] = [];
      page.on('request', request => { if (isBackendEndpoint(new URL(request.url()))) api.push(request.url()); });
      page.on('websocket', socket => { if (isBackendEndpoint(new URL(socket.url()))) sockets.push(socket.url()); });
      await page.route(isBackendEndpoint, route => route.abort());
      await page.addInitScript(() => { if (window.location.protocol !== 'data:') localStorage.setItem('demo-isolation-check', 'keep'); });
      await page.goto('/next/?demo=onboarding');
      await expect(page.getByRole('complementary', { name: '演示模式', exact: true })).toBeVisible({ timeout: 30_000 });
      let screenshot = 0;
      const after = async (action: () => Promise<unknown>) => {
        await action();
        await continuity(page);
        await page.screenshot({ path: `test-results/guide-flow-after/${width}-${String(++screenshot).padStart(2, '0')}.png` });
      };
      const advance = () => after(() => note(page).getByRole('button', { name: /^(下一步|知道了)$/ }).click());
      const activate = () => after(async () => (await target(page)).click());
      await expect(note(page)).toHaveAccessibleName('先做出一个能预览的小作品');
      // Cold boot includes loading code and fonts; every lesson transition below retains the 300 ms budget.
      await continuity(page, 5_000);
      await activate(); // Areas +, as named by the callout.
      await after(() => note(page).getByRole('button', { name: '填入这个名字', exact: true }).click());
      await advance();
      await after(async () => (await target(page)).getByRole('button').click());
      await after(() => page.getByRole('menuitem', { name: /^Small change/ }).click());
      await advance();
      await advance();
      await activate(); // Create area.
      if (width === 390) await activate(); // The callout names the newly created project.
      await activate(); // Project +.
      await after(() => note(page).getByRole('button', { name: '填入练习需求', exact: true }).click());
      await expect(page.getByRole('button', { name: /^Track 作品集练习/ })).toHaveCount(0);
      await advance();
      if (width === 390) await activate(); // … opens the options the callout asks us to check.
      await advance();
      if (width === 390) await after(() => page.keyboard.press('Escape')); // 收起选项.
      await activate(); // Native create/send control.
      // Follow location hints (Close conversation / Back to track) rather than route or store APIs.
      const followLocation = async () => {
        for (let count = 0; count < 3; count++) {
          const label = await (await target(page)).getAttribute('aria-label');
          if (label !== 'Close conversation' && label !== 'Back to track' && label !== 'Back to Report') return;
          await activate();
        }
        throw new Error('Location hint did not reach its declared target');
      };
      await followLocation();
      await expect(note(page)).toHaveAccessibleName('进展和成果都汇总在这里');
      await advance();
      await expect(note(page)).toHaveAccessibleName('打开成果预览');
      await activate();
      await expect(page.frameLocator('iframe[title="index.html"]').getByRole('heading', { name: '我的作品集', exact: true })).toBeVisible();
      await followLocation();
      await expect(note(page)).toHaveAccessibleName('当前工作的对话与进展');
      await advance();
      await expect(note(page)).toHaveAccessibleName('继续修改同一个成果');
      if (width === 390) {
        // The callout points at the dock; focus is the native activation that opens its sheet.
        await after(async () => (await target(page)).getByRole('textbox').click());
      } else await activate();
      await expect(note(page)).toHaveAccessibleName('在这里提出修改或回答问题');
      await after(() => note(page).getByRole('button', { name: '填入一次修改需求', exact: true }).click());
      const revision = await page.getByLabel('Message', { exact: true }).innerText();
      await advance();
      await expect(note(page)).toHaveAccessibleName('发送这次修改');
      await activate();
      await expect(page.getByText('Waiting for conversation to update.', { exact: true })).toHaveCount(0);
      await expect(page.getByText(revision, { exact: true })).toHaveCount(1);
      await followLocation();
      await expect(note(page)).toHaveAccessibleName('练习完成，可以重新开始');
      await expect(page.frameLocator('iframe[title="作品集演示"]').getByRole('heading', { name: '我的第一个作品', exact: true })).toBeVisible();
      await page.screenshot({ path: `test-results/guide-flow-after/${width}-complete.png` });
      expect(api).toEqual([]); expect(sockets).toEqual([]);
      expect(await page.evaluate(() => Object.fromEntries(Object.entries(localStorage)))).toEqual({ 'demo-isolation-check': 'keep' });
      await (await target(page)).click(); // Restart is itself the highlighted final target.
      await expect(note(page)).toHaveAccessibleName('先做出一个能预览的小作品');
      await expect(page.getByRole('button', { name: /^Collapse area 我的练习项目/ })).toHaveCount(0);
      expect(api).toEqual([]); expect(sockets).toEqual([]);
      await page.reload();
      await expect(note(page)).toHaveAccessibleName('先做出一个能预览的小作品');
      await expect(page.getByRole('button', { name: /^Collapse area 我的练习项目/ })).toHaveCount(0);
      expect(api).toEqual([]); expect(sockets).toEqual([]);
    });
  });
}
