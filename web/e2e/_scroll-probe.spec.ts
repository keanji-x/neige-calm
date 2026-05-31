// Scroll-routing probe — investigation only, NOT a CI gate.
// Underscored filename + skipped from CI by manual project filter at run time.
//
// Goal: capture how mouse-wheel events route in the current build across
// every kind of scroll container (page .scroll, .wave-report-body,
// .term-body, .file-viewer-*, .codex-card-pty / xterm, .cal-agenda) so we
// can decide between cursor-routed vs. focus-routed wheel models.
//
// Usage:
//   PLAYWRIGHT_BASE_URL=http://localhost:4198/calm/ \
//   npx playwright test e2e/_scroll-probe.spec.ts --project=chromium --reporter=line

import { test, expect, type Page } from '@playwright/test';

const PROBE_DUMP = process.env.PROBE_DUMP_PATH ?? '/tmp/scroll-probe-dump.json';

type Snapshot = {
  label: string;
  t: number;
  focus: string | null;
  activeCardSelector: string | null;
  tops: Record<string, { exists: boolean; scrollTop?: number; scrollHeight?: number; clientHeight?: number }>;
};

type WheelLogEntry = {
  t: number;
  kind: 'wheel';
  deltaY: number;
  defaultPrevented: boolean;
  target: string;
  path: string[];
};

declare global {
  interface Window {
    __wheelLog: (Snapshot | WheelLogEntry)[];
    __snapshot: (label: string) => void;
  }
}

const TARGETS = [
  '.scroll',
  '.wave-report-body',
  '.term-body',
  '.file-viewer-tree-list',
  '.file-viewer-changes',
  '.file-viewer-code-wrap .cm-scroller',
  '.codex-card-pty',
  '.xterm-viewport',
  '.cal-agenda',
];

async function installProbe(page: Page) {
  await page.addInitScript(({ targets }) => {
    (window as any).__wheelLog = [];
    (window as any).__snapshot = (label: string) => {
      const tops: Record<string, any> = {};
      for (const sel of targets) {
        const el = document.querySelector(sel);
        tops[sel] = el
          ? { exists: true, scrollTop: el.scrollTop, scrollHeight: el.scrollHeight, clientHeight: el.clientHeight }
          : { exists: false };
      }
      const fwElems = document.querySelectorAll('.wave-card');
      let activeCardSelector: string | null = null;
      fwElems.forEach((el, i) => {
        if (el.matches(':focus-within')) {
          activeCardSelector = `.wave-card:nth-of-type(${i + 1})`;
        }
      });
      (window as any).__wheelLog.push({
        label,
        t: performance.now(),
        focus: document.activeElement?.outerHTML?.slice(0, 160) ?? null,
        activeCardSelector,
        tops,
      });
    };
    document.addEventListener(
      'wheel',
      (ev) => {
        const path = (ev.composedPath() || [])
          .slice(0, 6)
          .map((n: any) => {
            if (n instanceof Element) {
              const cls = (n.className && typeof n.className === 'string') ? n.className.split(' ')[0] : '';
              return `${n.tagName.toLowerCase()}${cls ? '.' + cls : ''}`;
            }
            return String(n);
          });
        (window as any).__wheelLog.push({
          t: performance.now(),
          kind: 'wheel',
          deltaY: ev.deltaY,
          defaultPrevented: ev.defaultPrevented,
          target: path[0] ?? '?',
          path,
        });
      },
      { capture: true, passive: true },
    );
  }, { targets: TARGETS });
}

async function dumpLog(page: Page, label: string) {
  return await page.evaluate((lbl) => {
    (window as any).__snapshot(lbl);
    const log = (window as any).__wheelLog;
    (window as any).__wheelLog = [];
    return log;
  }, label);
}

async function wheelOverCenter(page: Page, selector: string, deltas: number[]): Promise<{ x: number; y: number } | null> {
  const handle = await page.locator(selector).first();
  if ((await handle.count()) === 0) return null;
  const box = await handle.boundingBox();
  if (!box) return null;
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  for (const dy of deltas) {
    await page.mouse.wheel(0, dy);
    await page.waitForTimeout(50);
  }
  return { x, y };
}

test('scroll-routing probe — capture wheel behavior across all scroll surfaces', async ({ page }, testInfo) => {
  test.setTimeout(180_000);
  await installProbe(page);

  // Log in (probe stack uses owner/dev by default; override via env).
  const username = process.env.PROBE_USERNAME ?? 'owner';
  const password = process.env.PROBE_PASSWORD ?? 'dev';
  await page.goto('/calm/');
  const loginResp = await page.request.post('/api/auth/login', {
    data: { username, password },
  });
  if (!loginResp.ok()) throw new Error(`login failed: ${loginResp.status()} ${await loginResp.text()}`);
  await page.goto('/calm/');

  // Wait for the shell to paint.
  await expect(page.locator('aside.side')).toBeVisible();

  // Wait for Today card to bootstrap (per golden-path).
  await expect
    .poll(
      () => page.evaluate(() => window.localStorage.getItem('calm.todayCardId')),
      { timeout: 30_000 },
    )
    .not.toBeNull();

  type Region = {
    label: string;
    setup?: () => Promise<void>;
    selector: string;
    deltas: number[];
  };

  const allLogs: Record<string, any[]> = {};

  async function runRegion(r: Region) {
    try {
      if (r.setup) await r.setup();
      await page.evaluate((lbl) => (window as any).__snapshot(`pre:${lbl}`), r.label);
      const center = await wheelOverCenter(page, r.selector, r.deltas);
      await page.evaluate((lbl) => (window as any).__snapshot(`post:${lbl}`), r.label);
      const log = await dumpLog(page, `dump:${r.label}`);
      allLogs[r.label] = log;
      // eslint-disable-next-line no-console
      console.log(`\n=== ${r.label} ===`);
      console.log(`  center: ${center ? JSON.stringify(center) : 'NOT FOUND'}`);
      console.log(`  events: ${log.length}`);
    } catch (e: any) {
      allLogs[r.label] = [{ error: String(e?.message ?? e) }];
      console.log(`\n=== ${r.label} === ERROR: ${String(e?.message ?? e)}`);
    }
    const fs = await import('node:fs/promises');
    await fs.writeFile(PROBE_DUMP, JSON.stringify(allLogs, null, 2), 'utf8');
  }

  // ============================================================
  // PHASE 1A — Today page, wheel over various regions WITHOUT focus
  // ============================================================
  await page.goto('/calm/');
  await page.waitForLoadState('networkidle');
  await page.waitForTimeout(800);

  await runRegion({
    label: 'today_page_scroll_no_focus',
    selector: '.scroll',
    deltas: [120, 120, 120, 120, -120, -120, -120, -120],
  });

  // Cal-agenda exists on Today page if calendar data is present; the
  // probe will just record exists:false if there's no data.
  await runRegion({
    label: 'today_cal_agenda_no_focus',
    selector: '.cal-agenda',
    deltas: [120, 120, 120, -120, -120, -120],
  });

  await runRegion({
    label: 'today_term_no_focus',
    selector: '.today-term .xterm-viewport, .today-term-body, .today-term',
    deltas: [120, 120, -120, -120],
  });

  // Click the today terminal and try again — does xterm focus change wheel routing?
  await runRegion({
    label: 'today_term_after_click',
    setup: async () => {
      const sel = '.today-term .xterm-screen, .today-term .xterm-viewport, .today-term';
      const loc = page.locator(sel).first();
      if ((await loc.count()) > 0) await loc.click({ position: { x: 100, y: 100 } }).catch(() => {});
    },
    selector: '.today-term .xterm-viewport, .today-term-body',
    deltas: [120, 120, -120, -120],
  });

  // ============================================================
  // PHASE 1B — Navigate to a wave (if any exist), wheel over each card
  // ============================================================

  // Try to find a wave to enter. We look for any wave-row / cove-nav.
  // The probe stops here if no wave is reachable — note in findings.
  const firstWaveId = await page.evaluate(async () => {
    const r = await fetch('/api/waves');
    if (!r.ok) return null;
    const j = await r.json();
    // The API returns either `Wave[]` or `{ waves: Wave[] }` depending on path.
    const list = Array.isArray(j) ? j : (Array.isArray(j?.waves) ? j.waves : []);
    // Skip the auto-minted Today wave (cwd === '/').
    const real = list.find((w: any) => w.cwd && w.cwd !== '/') ?? list[0];
    return real?.id ?? null;
  });
  if (firstWaveId) {
    {
      await page.goto(`/calm/wave/${firstWaveId}`);
      await page.waitForLoadState('networkidle');
      await page.waitForTimeout(1500);

      await runRegion({
        label: 'wave_page_scroll_no_focus',
        selector: '.scroll',
        deltas: [120, 120, 120, 120, -120, -120, -120, -120],
      });

      // Each card type — best-effort, skips if not present.
      for (const card of [
        { lbl: 'wave_report_body_no_focus', sel: '.wave-report-body' },
        { lbl: 'term_live_xterm_no_focus', sel: '.term.live .xterm-viewport' },
        { lbl: 'term_static_body_no_focus', sel: '.term:not(.live) .term-body' },
        { lbl: 'codex_pty_no_focus', sel: '.codex-card-pty .xterm-viewport' },
        { lbl: 'file_viewer_tree_no_focus', sel: '.file-viewer-tree-list' },
        { lbl: 'file_viewer_cm_no_focus', sel: '.file-viewer-code-wrap .cm-scroller' },
        { lbl: 'file_viewer_changes_no_focus', sel: '.file-viewer-changes' },
      ]) {
        await runRegion({
          label: card.lbl,
          selector: card.sel,
          deltas: [120, 120, 120, 120, -120, -120, -120, -120],
        });
      }

      // === Edge-of-card chaining: scroll wave-report all the way down then wheel more ===
      const hadReport = await page.locator('.wave-report-body').count();
      if (hadReport > 0) {
        await page.evaluate(() => {
          const el = document.querySelector('.wave-report-body') as HTMLElement | null;
          if (el) el.scrollTop = el.scrollHeight;
        });
        await runRegion({
          label: 'wave_report_at_bottom_wheel_down',
          selector: '.wave-report-body',
          deltas: [120, 120, 120, 120],
        });
        await page.evaluate(() => {
          const el = document.querySelector('.wave-report-body') as HTMLElement | null;
          if (el) el.scrollTop = 0;
        });
        await runRegion({
          label: 'wave_report_at_top_wheel_up',
          selector: '.wave-report-body',
          deltas: [-120, -120, -120, -120],
        });
      }

      // === Focus survey: click each card, capture document.activeElement ===
      const focusSurvey: Record<string, any> = {};
      const cardSelectors = [
        '.wave-report-card',
        '.term.live',
        '.term:not(.live)',
        '.codex-card',
        '.file-viewer-card',
      ];
      for (const sel of cardSelectors) {
        try {
          const loc = page.locator(sel).first();
          if ((await loc.count()) === 0) {
            focusSurvey[sel] = { skipped: 'not present in DOM' };
            continue;
          }
          // click head
          const head = loc.locator('.card-head').first();
          if ((await head.count()) > 0) {
            await head.click({ timeout: 2000 }).catch(() => {});
            focusSurvey[`${sel} > .card-head click`] = await page.evaluate(() => ({
              active: document.activeElement?.outerHTML?.slice(0, 200) ?? null,
              cardHasFocusWithin: !!document.querySelector('.wave-card:focus-within'),
            }));
          }
          // click body center — bail fast if hidden or no box
          const box = await loc.boundingBox({ timeout: 2000 }).catch(() => null);
          if (box && box.width > 0 && box.height > 0) {
            await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
            focusSurvey[`${sel} body click`] = await page.evaluate(() => ({
              active: document.activeElement?.outerHTML?.slice(0, 200) ?? null,
              cardHasFocusWithin: !!document.querySelector('.wave-card:focus-within'),
            }));
          } else {
            focusSurvey[`${sel} body click`] = { skipped: 'no bounding box' };
          }
          // click outside
          await page.mouse.click(20, 20);
          focusSurvey[`${sel} click-outside`] = await page.evaluate(() => ({
            active: document.activeElement?.outerHTML?.slice(0, 200) ?? null,
            cardHasFocusWithin: !!document.querySelector('.wave-card:focus-within'),
          }));
        } catch (e: any) {
          focusSurvey[sel] = { error: String(e?.message ?? e) };
        }
        // After every selector, persist progress to disk so a later
        // hang/timeout doesn't lose what we've collected.
        allLogs['_focus_survey'] = [focusSurvey];
        const fs = await import('node:fs/promises');
        await fs.writeFile(PROBE_DUMP, JSON.stringify(allLogs, null, 2), 'utf8');
      }
      allLogs['_focus_survey'] = [focusSurvey];
    }
  } else {
    allLogs['_note'] = [{ msg: 'no waves found via /api/waves; phase 1B skipped' }];
  }

  // ============================================================
  // Dump everything to disk for the human report.
  // ============================================================
  const fs = await import('node:fs/promises');
  await fs.writeFile(PROBE_DUMP, JSON.stringify(allLogs, null, 2), 'utf8');
  // eslint-disable-next-line no-console
  console.log(`\nWrote probe dump to ${PROBE_DUMP}`);
  await testInfo.attach('probe-dump', { path: PROBE_DUMP, contentType: 'application/json' });
});
