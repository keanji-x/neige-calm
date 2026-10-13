import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expect, test, type FrameLocator, type Page } from '@playwright/test';
import { createArea } from './helpers/seed.js';

/* #2585: a Track workspace page shown by the `html` report block and the file viewer, served by the
 * real kernel. Everything below the frame is the browser enforcing the kernel's CSP header and the
 * renderer's sandbox attribute; nothing is mocked. The page reports what it could do into its own DOM. */
const PAGE_PATH = 'docs/architecture/overview.html';
const PAGE = `<!doctype html>
<meta charset="utf-8">
<title>Overview</title>
<style>
  body { margin: 16px; font: 14px/1.4 system-ui, sans-serif; background: #fff; color: #123; }
  @media (prefers-color-scheme: dark) { body { background: #10161c; color: #dde; } }
  .layer { fill: #6a8; }
</style>
<h1 id="ran">script did not run</h1>
<svg id="diagram" width="320" height="96" viewBox="0 0 320 96" role="img" aria-label="Layers">
  <rect class="layer" x="8" y="8" width="304" height="36" rx="6"/>
  <rect class="layer" x="8" y="52" width="304" height="36" rx="6"/>
  <text x="24" y="31">Report</text><text x="24" y="75">Kernel</text>
</svg>
<p>origin <span id="origin">?</span></p>
<p>cookie <span id="cookie">?</span></p>
<p>fetch <span id="fetch">pending</span> <span id="violation"></span></p>
<script>
  document.getElementById('ran').textContent = 'script ran';
  document.getElementById('origin').textContent = String(self.origin);
  let cookie;
  try { cookie = 'readable:' + JSON.stringify(document.cookie); } catch (error) { cookie = 'threw:' + error.name; }
  document.getElementById('cookie').textContent = cookie;
  document.addEventListener('securitypolicyviolation', (event) => {
    document.getElementById('violation').textContent = event.effectiveDirective;
  });
  fetch('/api/version').then(
    (response) => { document.getElementById('fetch').textContent = 'allowed:' + response.status; },
    (error) => { document.getElementById('fetch').textContent = 'blocked:' + error.name; },
  );
</script>
`;

const createdAreaIds: string[] = [];
const workspaces: string[] = [];

test.afterEach(async ({ request }) => {
  for (const id of createdAreaIds) await request.delete(`/api/areas/${id}`);
  createdAreaIds.length = 0;
  for (const dir of workspaces) rmSync(dir, { recursive: true, force: true });
  workspaces.length = 0;
});

/** A git work tree holding the page, committed so a track worktree checked out from it has it too. */
function workspaceWithPage(): string {
  const dir = mkdtempSync(join(tmpdir(), 'nc-html-page-'));
  workspaces.push(dir);
  mkdirSync(join(dir, 'docs/architecture'), { recursive: true });
  writeFileSync(join(dir, PAGE_PATH), PAGE);
  const git = (...args: string[]) => execFileSync('git', ['-C', dir, '-c', 'user.name=e2e', '-c', 'user.email=e2e@neige.invalid', ...args]);
  git('init', '-q', '-b', 'main');
  git('add', '-A');
  git('commit', '-q', '-m', 'page');
  return dir;
}

async function assertContained(frame: FrameLocator) {
  await expect(frame.locator('#ran')).toHaveText('script ran');
  const box = await frame.locator('#diagram rect').first().boundingBox();
  expect(box?.width).toBeGreaterThan(100);
  await expect(frame.locator('#origin')).toHaveText('null');
  await expect(frame.locator('#cookie')).toHaveText('threw:SecurityError');
  await expect(frame.locator('#fetch')).toHaveText(/^blocked:/);
  await expect(frame.locator('#violation')).toHaveText('connect-src');
}

async function screenshots(page: Page, name: string, attach: (name: string, body: Buffer) => Promise<void>) {
  for (const [label, size] of [['desktop', { width: 1280, height: 900 }], ['phone', { width: 390, height: 844 }]] as const) {
    for (const colorScheme of ['light', 'dark'] as const) {
      await page.setViewportSize(size);
      await page.emulateMedia({ colorScheme });
      /* The theme switch starts colour transitions; capture their end state, not a frame mid-way. */
      await page.evaluate(() => Promise.all(document.getAnimations().map((animation) => animation.finished.catch(() => undefined))));
      await attach(`${name}-${label}-${colorScheme}`, await page.screenshot({ animations: 'disabled' }));
    }
  }
}

test('an html block and the file viewer run the page sandboxed: scripts and SVG, no cookie, no API', async ({ page, request }, testInfo) => {
  const attach = async (name: string, body: Buffer) => { await testInfo.attach(name, { body, contentType: 'image/png' }); };
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  const created = await request.post('/api/tracks', {
    data: {
      area_id: area.id, planner_provider: 'codex', title: `FE e2e html page ${Date.now()}`,
      cwd: workspaceWithPage(), attach_folder: true, theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
  });
  expect(created.ok(), await created.text()).toBe(true);
  const track = await created.json() as { id: string };

  const raw = await request.get(`/api/tracks/${track.id}/workspace/readfile-raw?path=${encodeURIComponent(PAGE_PATH)}`);
  expect(raw.status()).toBe(200);
  expect(raw.headers()['content-security-policy']).toMatch(/^sandbox allow-scripts; default-src 'none';/);

  const report = await (await request.get(`/api/tracks/${track.id}/report`)).json() as { docRev: number };
  const block = await request.post(`/api/tracks/${track.id}/report/blocks`, {
    data: { kind: 'html', payload: { path: PAGE_PATH, title: 'Architecture overview', height: 360 }, ifDocRev: report.docRev },
  });
  expect(block.ok(), await block.text()).toBe(true);

  await page.goto(`/next/track/${track.id}`);
  const inline = page.locator('iframe[title="Architecture overview"]');
  await expect(inline).toHaveAttribute('sandbox', 'allow-scripts');
  await inline.scrollIntoViewIfNeeded();
  await assertContained(page.frameLocator('iframe[title="Architecture overview"]'));
  await screenshots(page, 'block', attach);

  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto(`/next/track/${track.id}?file=${encodeURIComponent(PAGE_PATH)}`);
  await expect(page.getByRole('button', { name: 'Page', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await assertContained(page.frameLocator(`iframe[title="${PAGE_PATH}"]`));
  await screenshots(page, 'file-viewer', attach);
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.getByRole('button', { name: 'Source', exact: true }).click();
  await expect(page.locator('[data-nc-report-file-source]')).toContainText("fetch('/api/version')");
});
