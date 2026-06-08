import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { test, expect, type Page } from '@playwright/test';
import { LIGHT_THEME_RGB, DARK_THEME_RGB } from '../src/api/themeRgb';

type ThemeName = 'light' | 'dark' | 'system';
type XtermDumps = Record<string, () => string>;

const hex = (rgb: readonly [number, number, number]) =>
  '#' + rgb.map((n) => n.toString(16).padStart(2, '0')).join('');

async function terminalDumpIds(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const w = window as unknown as { __xtermDumps__?: XtermDumps };
    return Object.keys(w.__xtermDumps__ ?? {});
  });
}

async function terminalDumpCount(page: Page): Promise<number> {
  return (await terminalDumpIds(page)).length;
}

async function dumpTerminal(page: Page, id: string): Promise<string> {
  return page.evaluate((terminalId) => {
    const w = window as unknown as { __xtermDumps__?: XtermDumps };
    return w.__xtermDumps__?.[terminalId]?.() ?? '';
  }, id);
}

async function dumpAllTerminals(page: Page): Promise<string> {
  return page.evaluate(() => {
    const w = window as unknown as { __xtermDumps__?: XtermDumps };
    const reg = w.__xtermDumps__;
    if (!reg) return '<no __xtermDumps__ registry>';
    return Object.entries(reg)
      .map(([id, dump]) => `===== terminal ${id} =====\n${dump()}`)
      .join('\n');
  });
}

function latestTheme(buffer: string): { fg: string | null; bg: string | null } {
  const fg = [...buffer.matchAll(/THEME_FG=#([0-9a-f]{6})/g)].at(-1)?.[1];
  const bg = [...buffer.matchAll(/THEME_BG=#([0-9a-f]{6})/g)].at(-1)?.[1];
  return { fg: fg ? `#${fg}` : null, bg: bg ? `#${bg}` : null };
}

async function setTheme(page: Page, mode: ThemeName): Promise<void> {
  await page.waitForFunction(
    () =>
      typeof (
        window as unknown as { __calmSetTheme?: (theme: ThemeName) => void }
      ).__calmSetTheme === 'function',
    null,
    { timeout: 5_000 },
  );
  await page.evaluate((next) => {
    (
      window as unknown as { __calmSetTheme?: (theme: ThemeName) => void }
    ).__calmSetTheme?.(next);
  }, mode);
  await page.waitForFunction(
    (next) => document.documentElement.dataset.theme === next,
    mode,
    { timeout: 5_000 },
  );
}

test.describe.serial('tui theme protocol', () => {
  test.setTimeout(60_000);

  test('reports boot and mid-session OSC 10/11 theme changes', async ({
    page,
  }) => {
    await page.route('**://fonts.googleapis.com/**', (route) => route.abort());
    await page.route('**://fonts.gstatic.com/**', (route) => route.abort());

    await page.goto('/calm/?testMounts=1');
    const sidebarCoves = page.getByRole('navigation', { name: 'Coves' });
    const coveName = `E2E tui-theme cove ${Date.now()}`;
    await sidebarCoves.getByRole('button', { name: /new cove/i }).click();
    const nameInput = sidebarCoves.getByPlaceholder(/name/i);
    await expect(nameInput).toBeVisible();
    await nameInput.fill(coveName);
    await nameInput.press('Enter');

    const coveBtn = sidebarCoves.getByRole('button', {
      name: coveName,
      exact: true,
    });
    await expect(coveBtn).toBeVisible();
    await coveBtn.click();
    await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

    const coveId = new URL(page.url()).pathname.split('/').pop()!;
    const waveTitle = `E2E tui-theme ${Date.now()}`;
    const waveRes = await page.request.post('/api/waves', {
      data: {
        cove_id: coveId,
        title: waveTitle,
        cwd: `/tmp/playwright-cove-${coveId}`,
        attach_folder: true,
        theme: { fg: DARK_THEME_RGB.fg, bg: DARK_THEME_RGB.bg },
      },
      headers: { 'content-type': 'application/json' },
    });
    if (!waveRes.ok()) {
      const body = await waveRes.text().catch(() => '<unreadable>');
      throw new Error(
        `POST /api/waves -> ${waveRes.status()} ${waveRes.statusText()}: ${body}`,
      );
    }
    const wave = (await waveRes.json()) as { id: string };
    await page.goto(`/calm/wave/${wave.id}?testMounts=1`);
    await expect(page).toHaveURL(/\/calm\/wave\/[^/]+\?testMounts=1$/);
    await expect(
      page.getByText(waveTitle, { exact: false }).first(),
    ).toBeVisible();
    // Post-#510 PR-del: spec card is a chat panel (no XtermView). The
    // terminal-card created below is the first xterm-view in the wave.
    const beforeIds = await terminalDumpIds(page);
    const dumpsBeforeAdd = await terminalDumpCount(page);
    await setTheme(page, 'light');

    const absFixturePath = path.resolve(
      fileURLToPath(import.meta.url),
      '../fixtures/test-tui-theme.py',
    );
    const cardRes = await page.request.post(
      `/api/waves/${wave.id}/terminal-cards`,
      {
        data: {
          program: `python3 '${absFixturePath}'`,
          cwd: '/tmp',
          env: {},
          theme: { fg: LIGHT_THEME_RGB.fg, bg: LIGHT_THEME_RGB.bg },
        },
        headers: { 'content-type': 'application/json' },
      },
    );
    if (!cardRes.ok()) {
      const body = await cardRes.text().catch(() => '<unreadable>');
      throw new Error(
        `POST terminal-cards -> ${cardRes.status()} ${cardRes.statusText()}: ${body}`,
      );
    }

    await page.waitForFunction(
      (prev) => {
        const w = window as unknown as { __xtermDumps__?: XtermDumps };
        return Object.keys(w.__xtermDumps__ ?? {}).length === prev + 1;
      },
      dumpsBeforeAdd,
      { timeout: 15_000 },
    );

    const afterIds = await terminalDumpIds(page);
    const terminalId = afterIds.find((id) => !beforeIds.includes(id));
    if (!terminalId) {
      throw new Error(
        `No new terminal dump id found. Dumps:\n${await dumpAllTerminals(page)}`,
      );
    }

    await expect
      .poll(async () => latestTheme(await dumpTerminal(page, terminalId)), {
        timeout: 15_000,
      })
      .toEqual({ fg: hex(LIGHT_THEME_RGB.fg), bg: hex(LIGHT_THEME_RGB.bg) });
    const bootTheme = latestTheme(await dumpTerminal(page, terminalId));
    expect(bootTheme.fg).toBe(hex(LIGHT_THEME_RGB.fg));
    expect(bootTheme.bg).toBe(hex(LIGHT_THEME_RGB.bg));

    const t0 = Date.now();
    await setTheme(page, 'dark');
    await expect
      .poll(async () => latestTheme(await dumpTerminal(page, terminalId)), {
        timeout: 5_000,
      })
      .toEqual({ fg: hex(DARK_THEME_RGB.fg), bg: hex(DARK_THEME_RGB.bg) });
    const t1 = Date.now();
    const darkTheme = latestTheme(await dumpTerminal(page, terminalId));
    expect(darkTheme.fg).toBe(hex(DARK_THEME_RGB.fg));
    expect(darkTheme.bg).toBe(hex(DARK_THEME_RGB.bg));
    console.log('[tui-theme-protocol] mid-session round-trip ms:', t1 - t0);
    expect.soft(t1 - t0).toBeLessThan(2000);

    await setTheme(page, 'light');
    await expect
      .poll(async () => latestTheme(await dumpTerminal(page, terminalId)), {
        timeout: 5_000,
      })
      .toEqual({ fg: hex(LIGHT_THEME_RGB.fg), bg: hex(LIGHT_THEME_RGB.bg) });
    const finalTheme = latestTheme(await dumpTerminal(page, terminalId));
    expect(finalTheme.fg).toBe(hex(LIGHT_THEME_RGB.fg));
    expect(finalTheme.bg).toBe(hex(LIGHT_THEME_RGB.bg));

    // Anchor D - Bug B regression guard. The wave-switch remount must
    // reclaim Owner after WS close; fixed by e8d85122.
    // Invariants pinned:
    //   1. useTheme() is read at mount in cards/builtins/terminal.tsx.
    //   2. RenderPlane is registry-scoped (terminal_renderer/mod.rs:206),
    //      so the daemon's persisted set_default_colors survives WS reconnect.
    const waveBRes = await page.request.post('/api/waves', {
      data: {
        cove_id: coveId,
        title: `E2E tui-theme wave-B ${Date.now()}`,
        cwd: `/tmp/playwright-cove-${coveId}-B`,
        attach_folder: true,
        theme: { fg: DARK_THEME_RGB.fg, bg: DARK_THEME_RGB.bg },
      },
      headers: { 'content-type': 'application/json' },
    });
    if (!waveBRes.ok()) {
      const body = await waveBRes.text().catch(() => '<unreadable>');
      throw new Error(
        `POST /api/waves (wave B) -> ${waveBRes.status()} ${waveBRes.statusText()}: ${body}`,
      );
    }
    const waveB = (await waveBRes.json()) as { id: string };

    // Confirm wave A's terminal dump is currently mounted before navigating.
    const beforeSwitchPresent = await page.evaluate((id) => {
      const w = window as unknown as { __xtermDumps__?: XtermDumps };
      return typeof w.__xtermDumps__?.[id] === 'function';
    }, terminalId);
    expect(beforeSwitchPresent).toBe(true);

    // Navigate to wave B; wave A's XtermView for terminalId unmounts and
    // its __xtermDumps__ entry disappears - proves the React fiber really
    // tore down rather than staying hidden.
    await page.goto(`/calm/wave/${waveB.id}?testMounts=1`);
    await expect(page).toHaveURL(/\/calm\/wave\/[^/]+\?testMounts=1$/);
    await page.waitForFunction(
      (id) => {
        const w = window as unknown as { __xtermDumps__?: XtermDumps };
        return w.__xtermDumps__?.[id] === undefined;
      },
      terminalId,
      { timeout: 10_000 },
    );

    // On wave B, flip to dark - the "user changed theme while away" case.
    await setTheme(page, 'dark');

    // Navigate back to wave A. The terminal card remounts; a fresh
    // XtermView fiber re-registers a dump fn under the same terminalId.
    await page.goto(`/calm/wave/${wave.id}?testMounts=1`);
    await expect(page).toHaveURL(/\/calm\/wave\/[^/]+\?testMounts=1$/);
    await page.waitForFunction(
      (id) => {
        const w = window as unknown as { __xtermDumps__?: XtermDumps };
        return typeof w.__xtermDumps__?.[id] === 'function';
      },
      terminalId,
      { timeout: 15_000 },
    );

    // Mount-time TerminalThemeUpdate carries dark; daemon updates
    // RenderPlane.default_colors; the fixture's focus-in driven re-probe
    // prints fresh THEME_FG/BG lines reflecting dark.
    await expect
      .poll(async () => latestTheme(await dumpTerminal(page, terminalId)), {
        timeout: 5_000,
      })
      .toEqual({ fg: hex(DARK_THEME_RGB.fg), bg: hex(DARK_THEME_RGB.bg) });
    const switchTheme = latestTheme(await dumpTerminal(page, terminalId));
    expect(switchTheme.fg).toBe(hex(DARK_THEME_RGB.fg));
    expect(switchTheme.bg).toBe(hex(DARK_THEME_RGB.bg));
  });
});
