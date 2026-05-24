// E2E: a one-shot terminal worker whose child exits within milliseconds
// must surface a small `exit 0` badge on the card header — NOT a full-
// card overlay, and NOT a "disconnected — 1006" fallback.
//
// Why this matters: when `cmd` finishes faster than the browser's WS
// attach round-trip, the daemon has already unlinked its unix socket
// by the time the WS upgrade lands. The original pre-#304 behaviour
// took the `ws::terminal::resolve_live_sock` 500 path, the WS never
// reached 101, and the browser saw a code-1006 close with no
// `child-exited` reason — rendering the generic "disconnected" overlay.
// PR #304 fixed the close code but kept a full-card "process exited +
// Restart" overlay. Issue #306 then collapsed that overlay too: the
// terminal buffer stays visible, and a small header badge (`exit 0` /
// `exit 137` / `signal`) carries the exit info. This spec pins the
// post-#306 contract end-to-end.
//
// Chromium project only — the replay backend doesn't spawn daemons.

import { test, expect } from '@playwright/test';

test('terminal worker that exits cleanly shows the exit 0 header badge, no overlay', async ({ page }) => {
  // Block Google Fonts. `index.html` loads a `<link rel="stylesheet"
  // href="https://fonts.googleapis.com/...">` that, in restricted-network
  // CI / sandboxed test environments, can hang for tens of seconds
  // before failing — blocking `domcontentloaded` and module-script
  // execution. The fallback `system-ui` chain is good enough for the
  // assertions below.
  await page.route('**://fonts.googleapis.com/**', (route) => route.abort());
  await page.route('**://fonts.gstatic.com/**', (route) => route.abort());

  // Step 1 — mint a fresh user cove via the sidebar (issue #175).
  await page.goto('/calm/');
  const sidebarCoves = page.getByRole('navigation', { name: 'Coves' });
  const coveName = `E2E clean-exit ${Date.now()}`;
  await sidebarCoves.getByRole('button', { name: /new cove/i }).click();
  const nameInput = sidebarCoves.getByPlaceholder(/name/i);
  await expect(nameInput).toBeVisible();
  await nameInput.fill(coveName);
  await nameInput.press('Enter');
  const coveBtn = sidebarCoves.getByRole('button', { name: coveName, exact: true });
  await expect(coveBtn).toBeVisible();
  await coveBtn.click();
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);
  const coveId = new URL(page.url()).pathname.split('/').pop()!;

  // Step 2 — mint a wave inside the cove via REST. Same shape as
  // `new-terminal-card.spec.ts`.
  const waveTitle = `E2E clean-exit ${Date.now()}`;
  const cwd = `/tmp/playwright-clean-exit-${coveId}`;
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
    throw new Error(`POST /api/waves → ${waveRes.status()} ${waveRes.statusText()}: ${body}`);
  }
  const wave = (await waveRes.json()) as { id: string };

  // Step 3 — mint a terminal card whose program prints one line and
  // exits with code 0. `/bin/sh -c "printf 'done\\n'"` reproduces the
  // user-reported race deterministically: the child exits ~20ms after
  // spawn, well before the browser can establish the WS for the card
  // we navigate to next. The kernel wraps `program` in `/bin/sh -c`
  // server-side (`routes/terminal.rs` `spawn_daemon_with_parts`), so
  // we just pass the shell snippet.
  const cardRes = await page.request.post(
    `/api/waves/${wave.id}/terminal-cards`,
    {
      data: {
        program: `printf 'done\\n'`,
        cwd,
        env: {},
        theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
      },
      headers: { 'content-type': 'application/json' },
    },
  );
  if (!cardRes.ok()) {
    const body = await cardRes.text().catch(() => '<unreadable>');
    throw new Error(`POST terminal-cards → ${cardRes.status()} ${cardRes.statusText()}: ${body}`);
  }
  const card = (await cardRes.json()) as { payload?: { terminal_id?: string } };
  const terminalId = card.payload?.terminal_id;
  if (!terminalId) {
    throw new Error(`terminal card POST missing payload.terminal_id: ${JSON.stringify(card)}`);
  }

  // Step 4 — small breather so the daemon's spawn → child exit → unlink
  // cycle finishes BEFORE we open the page. `printf` is sub-50ms but
  // we give the kernel a generous half-second so the test is robust to
  // a loaded CI box. The WHOLE point of this spec is reproducing the
  // "child already gone by the time WS attaches" path that #306
  // persists across kernel restarts.
  await page.waitForTimeout(500);

  // Step 5 — open the wave detail page. The XtermView mounts, opens
  // its WS, and the server accepts the upgrade + closes with
  // 1000+child-exited. After #306 the buffer stays visible and the
  // card header badge surfaces `exit 0` (success palette).
  await page.goto(`/calm/wave/${wave.id}`);
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/);

  // Step 6 — scope assertions to the worker terminal card we minted.
  // The wave's auto-created spec card also renders an XtermView, so a
  // page-wide selector would hit both. `[data-terminal-id="..."]` on
  // the XtermView root pins the locator to our worker.
  const ourView = page.locator(`[data-terminal-id="${terminalId}"]`);
  await expect(ourView).toBeVisible({ timeout: 15_000 });
  // The card head (one DOM up — `.term > .card-head + .term-body >
  // .xterm-view`). Walking up via XPath keeps the locator robust to
  // future class-name tweaks on the wrapper.
  const ourCard = ourView.locator('xpath=ancestor::div[contains(@class, "term")][1]');
  const exitBadge = ourCard.locator('.card-head-exit-badge');

  // (a) The exit badge appears, reads "exit 0", and uses the success
  //     palette class.
  await expect(exitBadge).toBeVisible({ timeout: 15_000 });
  await expect(exitBadge).toHaveText(/exit 0/i);
  await expect(exitBadge).toHaveClass(/card-head-exit-badge-success/);

  // (b) The terminal buffer stays mounted: xterm.js's container is
  //     present (the OSC-echo / clean-close model now leaves the
  //     surface visible rather than swapping in an overlay).
  await expect(ourView.locator('.xterm-container')).toBeVisible();

  // (c) No pre-#306 overlays: no "process exited" text, no Restart
  //     button, no Reconnect button, no "disconnected" text on our
  //     card.
  await expect(
    ourCard.locator('.xterm-status-closed'),
  ).toHaveCount(0);
  await expect(
    ourCard.getByRole('button', { name: /^restart$/i }),
  ).toHaveCount(0);
  await expect(
    ourCard.getByRole('button', { name: /^reconnect$/i }),
  ).toHaveCount(0);
});
