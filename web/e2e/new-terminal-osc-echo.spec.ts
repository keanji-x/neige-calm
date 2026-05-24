// E2E regression — a New-terminal card must NOT echo any mid-session
// theme bytes the daemon writes (pre-#305 also OSC 10/11 RGB pairs;
// post-#305 just `ESC[I`) back into the grid as literal caret text.
//
// The bug
// -------
// Clicking "+ Add → terminal" opened a card whose first visible
// line was the literal text:
//
//   ^[]10;rgb:2a2a/2f2f/3a3a^[\^[]11;rgb:fcfc/fefe/ffff^[\^[[I
//
// Root cause: `XtermView.tsx`'s theme effect fires on EVERY mount and
// POSTs a `TerminalThemeUpdate` carrying the host theme. The daemon was
// already spawned with that exact theme (`--terminal-fg/-bg`), so the
// update was a no-op color-wise — yet pre-#305 the daemon still wrote
// an `OSC 10/11 + focus-in` blob to the PTY master. A New terminal runs
// `$SHELL`, and a modern shell prompt is NOT cooked: zsh's ZLE (bash's
// readline) drives the line via a raw-mode editor (ECHO off, ICANON off
// — termios identical to a TUI). The shell's line editor treated the
// injected bytes as INPUT and redrew them at the prompt as `^[]10;rgb:…`
// garbage, which xterm then rendered.
//
// What this pins
// --------------
//  * Fix A (suppress the no-op mount-time TerminalThemeUpdate): after a
//    New terminal becomes ready, the rendered buffer must not contain
//    `]10;rgb:` / `]11;rgb:`.
//  * Fix B (gate the focus-in nudge on DECSET 1004): toggle the app
//    theme light↔dark while the shell sits at its prompt. Post-#305
//    the daemon's only mid-session write is `ESC[I`, gated on whether
//    the child opted into DECSET 1004 (focus event reporting); the
//    OSC 10/11 reply is synthesized later from the model's defaults
//    when the child solicits it. A shell's ZLE/readline never enables
//    1004 (it only turns on bracketed paste, 2004), so the daemon
//    writes nothing — no echo text appears.
//
// How we read the grid
// --------------------
// xterm's canvas/webgl renderer doesn't mirror glyphs into navigable
// DOM, so we read each card's buffer via the `window.__xtermDumps__`
// registry (terminalId → serializer) — a test-only hook XtermView
// installs under `?testMounts=1` (same gating as the `__xtermMounts__`
// counter). The same flag also exposes `window.__calmSetTheme(mode)` so
// we can flip the theme without navigating away (navigation would
// unmount the card under test).
//
// Project / prereqs
// -----------------
// Runs in the `chromium` Playwright project, which targets the
// developer's `make dev` stack at http://localhost:4040/calm/. This
// spec needs a REAL PTY-backed terminal running the host's `$SHELL` to
// reproduce the echo, and the replay binary used by the `a11y` project
// stubs the daemon out (`DaemonClient::new_stub()`), so the chromium /
// `make dev` path is the only env that can exercise it.
//
// NOTE on the anchor's assumption: this spec drives the host's real
// `$SHELL`, so its anchor only holds if that shell does NOT enable
// DECSET 1004 at the prompt (true for zsh/bash, which enable only
// bracketed paste 2004). The deterministic, hermetic CI anchor is
// `crates/calm-server/tests/theme_cooked_shell_no_osc_echo.rs` (it wires
// a `cooked-shell-child` fixture in ZLE raw mode that never enables
// 1004); this e2e is auxiliary coverage of the real `make dev` path.

import { test, expect, type Page } from '@playwright/test';

/** Read and concatenate the rendered buffer text of EVERY xterm-backed
 *  card on the page via the test-only `__xtermDumps__` registry (keyed
 *  by terminalId). A wave auto-mints a codex spec card alongside the
 *  AddPanel New-terminal card; reading all buffers means the assertion
 *  catches an echo in any of them. The codex card enables DECSET 1004
 *  and consumes the OSC reply silently, so only the shell terminal (ZLE
 *  raw mode, no 1004) can ever surface the literal text by redrawing the
 *  injected bytes at its prompt — which is exactly the bug we're pinning. */
async function dumpAllTerminals(page: Page): Promise<string> {
  return page.evaluate(() => {
    const w = window as unknown as {
      __xtermDumps__?: Record<string, () => string>;
    };
    const reg = w.__xtermDumps__;
    if (!reg) return '<no __xtermDumps__ registry>';
    return Object.entries(reg)
      .map(([id, dump]) => `===== terminal ${id} =====\n${dump()}`)
      .join('\n');
  });
}

/** Count of registered xterm dump hooks (one per mounted XtermView). */
async function terminalDumpCount(page: Page): Promise<number> {
  return page.evaluate(() => {
    const w = window as unknown as {
      __xtermDumps__?: Record<string, () => string>;
    };
    return w.__xtermDumps__ ? Object.keys(w.__xtermDumps__).length : 0;
  });
}

/** The literal caret-text fragments the OSC-echo bug surfaces. When the
 *  shell's line editor redraws the injected bytes, the non-printable ESC
 *  (`\x1b`) doesn't land in the grid, leaving the `]10;rgb:` / `]11;rgb:`
 *  tail visible — that tail is our anchor. */
const OSC_ECHO_FRAGMENTS = [']10;rgb:', ']11;rgb:'];

function assertNoOscEcho(dump: string, when: string): void {
  for (const frag of OSC_ECHO_FRAGMENTS) {
    expect(
      dump.includes(frag),
      `terminal grid must not contain echoed OSC text ${JSON.stringify(frag)} (${when}). ` +
        `Full dump:\n${dump}`,
    ).toBe(false);
  }
}

// Multi-step real-server flow (cove → wave → New terminal → daemon
// spawn → theme toggles). The default 30s budget is tight once the
// `make dev` Vite server is compiling lazy chunks on a cold cache; 60s
// gives the first run room.
test.setTimeout(60_000);

test('new terminal does not echo OSC 10/11 color replies (raw-mode shell)', async ({
  page,
}) => {
  // Step 1 — mint a fresh user cove via the sidebar (issue #175). Keep
  // `?testMounts=1` on every navigation so the XtermView test hooks
  // (`__xtermDumps__`, `__calmSetTheme`) stay installed across routes.
  await page.goto('/calm/?testMounts=1');
  const sidebarCoves = page.getByRole('navigation', { name: 'Coves' });
  const coveName = `E2E osc-echo cove ${Date.now()}`;
  await sidebarCoves.getByRole('button', { name: /new cove/i }).click();
  const nameInput = sidebarCoves.getByPlaceholder(/name/i);
  await expect(nameInput).toBeVisible();
  await nameInput.fill(coveName);
  await nameInput.press('Enter');

  // Match the cove-nav entry by EXACT name so we don't also resolve the
  // sibling "Delete cove "<name>"" button (whose accessible name embeds
  // the cove name), which would trip strict-mode's single-match rule.
  const coveBtn = sidebarCoves.getByRole('button', {
    name: coveName,
    exact: true,
  });
  await expect(coveBtn).toBeVisible();
  await coveBtn.click();
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

  // Step 2 — create a wave in this cove via the kernel REST API (same
  // shortcut as new-terminal-card.spec.ts). `theme` is a required
  // NewWave field (#177); dark sentinel mirrors DARK_THEME_RGB.
  const coveId = new URL(page.url()).pathname.split('/').pop()!;
  const waveTitle = `E2E osc-echo ${Date.now()}`;
  const cwd = `/tmp/playwright-cove-${coveId}`;
  const waveRes = await page.request.post('/api/waves', {
    data: {
      cove_id: coveId,
      title: waveTitle,
      cwd,
      attach_folder: true,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!waveRes.ok()) {
    const body = await waveRes.text().catch(() => '<unreadable>');
    throw new Error(
      `POST /api/waves → ${waveRes.status()} ${waveRes.statusText()}: ${body}`,
    );
  }
  const wave = (await waveRes.json()) as { id: string };
  await page.goto(`/calm/wave/${wave.id}?testMounts=1`);
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+\?testMounts=1$/);
  await expect(
    page.getByText(waveTitle, { exact: false }).first(),
  ).toBeVisible();

  // Step 3 — wave-create auto-mints a codex spec card (#136/#182) which
  // brings up its OWN XtermView. Record how many xterm dump hooks exist
  // before we add the terminal card, so Step 4 can wait for exactly one
  // MORE (the New-terminal card) rather than guess an absolute count.
  await expect(page.locator('.xterm-view').first()).toBeVisible({
    timeout: 15_000,
  });
  const dumpsBeforeAdd = await terminalDumpCount(page);

  // Step 4 — open the AddPanel and choose "terminal". The AddPanel
  // entry's accessible name is the lowercase kind word "terminal" (see
  // `web/src/cards/builtins/terminal.tsx` → `addPanel: { label: 'terminal' }`,
  // mapped to MenuItem.label in `web/src/shared/components/AddPanel.tsx`);
  // anchor the regex so it can't accidentally match a future "terminal …"
  // sibling entry. The `.term` wrapper class is specific to the terminal
  // card (the codex spec card uses a different component), so its count
  // tracks our new card.
  const termCardsBefore = await page.locator('.term').count();
  const addBtn = page
    .getByRole('button', { name: /^\s*\+?\s*add(\s|$)/i })
    .first();
  await expect(addBtn).toBeVisible();
  await addBtn.click();
  const termOption = page.getByRole('menuitem', { name: /^terminal$/i });
  await expect(termOption).toBeVisible({ timeout: 5_000 });
  await termOption.click();

  // Step 5 — wait for the New-terminal card's xterm to mount: one more
  // `.term` wrapper AND one more registered dump hook than before.
  await expect(page.locator('.term')).toHaveCount(termCardsBefore + 1, {
    timeout: 10_000,
  });
  await page.waitForFunction(
    (prev) => {
      const w = window as unknown as {
        __xtermDumps__?: Record<string, () => string>;
      };
      const n = w.__xtermDumps__ ? Object.keys(w.__xtermDumps__).length : 0;
      return n >= prev + 1;
    },
    dumpsBeforeAdd,
    { timeout: 15_000 },
  );

  // Give the daemon time to spawn `$SHELL`, paint the prompt, and (in
  // the buggy world) echo the daemon's theme write back into the grid.
  // Wait until at least one terminal grid is non-empty as the readiness
  // signal.
  await page.waitForFunction(
    () => {
      const w = window as unknown as {
        __xtermDumps__?: Record<string, () => string>;
      };
      const reg = w.__xtermDumps__;
      if (!reg) return false;
      return Object.values(reg).some((d) => d().trim().length > 0);
    },
    null,
    { timeout: 15_000 },
  );
  // Settle a beat past the first paint so any (buggy) echo would have
  // landed before we snapshot.
  await page.waitForTimeout(500);

  // Step 6 — ANCHOR A: no terminal grid may contain echoed OSC text.
  // With fix A the mount-time TerminalThemeUpdate is suppressed, so the
  // daemon never writes anything to the shell terminal.
  assertNoOscEcho(await dumpAllTerminals(page), 'after New terminal ready');

  // Step 7 — ANCHOR B: toggle the theme while the raw-mode shell is at
  // its prompt. Even though the colors now genuinely differ, fix B gates
  // the `ESC[I` write on whether the child opted into DECSET 1004
  // (focus event reporting). A shell's ZLE/readline never enables 1004,
  // so the daemon must NOT write anything — still no echo text.
  await page.waitForFunction(
    () =>
      typeof (
        window as unknown as { __calmSetTheme?: (m: string) => void }
      ).__calmSetTheme === 'function',
    null,
    { timeout: 5_000 },
  );
  // Flip to whichever theme differs from the current one, then flip
  // back — exercise both OSC color directions over a raw-mode shell.
  for (const mode of ['dark', 'light', 'dark'] as const) {
    await page.evaluate((m) => {
      (
        window as unknown as { __calmSetTheme?: (mode: string) => void }
      ).__calmSetTheme?.(m);
    }, mode);
    await page.waitForTimeout(300);
    assertNoOscEcho(
      await dumpAllTerminals(page),
      `after theme toggle → ${mode}`,
    );
  }
});
