// E2E: a one-shot terminal worker whose child exits within milliseconds
// must surface the "process exited" overlay (with a Restart button) in
// the card UI — NOT "disconnected" + Reconnect.
//
// Why this matters: when `cmd` finishes faster than the browser's WS
// attach round-trip, the daemon has already unlinked its unix socket
// by the time the WS upgrade lands. The original behaviour took the
// `ws::terminal::resolve_live_sock` 500 path, the WS never reached 101,
// and the browser saw a code-1006 close with no `child-exited` reason
// — rendering the generic "disconnected" overlay. The server now
// detects this case (terminal row exists, `daemon_handle` set, socket
// probe fails) and emits `Close(1000, "child-exited")` at upgrade
// time so the JS client renders the clean-exit overlay instead. This
// spec pins that contract end-to-end.
//
// Chromium project only — the replay backend doesn't spawn daemons.

import { test, expect } from '@playwright/test';

test('terminal worker that exits cleanly shows "process exited" overlay, not "disconnected"', async ({ page }) => {
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

  // Step 4 — small breather so the daemon's spawn → child exit → unlink
  // cycle finishes BEFORE we open the page. `printf` is sub-50ms but
  // we give the kernel a generous half-second so the test is robust to
  // a loaded CI box. The WHOLE point of this spec is reproducing the
  // "child already gone by the time WS attaches" path that the fix
  // addresses.
  await page.waitForTimeout(500);

  // Step 5 — open the wave detail page. The XtermView mounts, opens
  // its WS, and the server should accept the upgrade and immediately
  // emit Close(1000, "child-exited") because the daemon socket is
  // gone. The JS client renders the "process exited" overlay.
  await page.goto(`/calm/wave/${wave.id}`);
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/);

  // Step 6 — the `.xterm-status-closed` overlay should say "process
  // exited" with a Restart button — see `XtermView.tsx` line ~700.
  // Generous timeout because the WS round-trip + onclose handler has
  // to complete before status flips to `exited`.
  const overlay = page.locator('.xterm-status-closed');
  await expect(overlay).toBeVisible({ timeout: 15_000 });
  await expect(overlay).toContainText('process exited');
  await expect(
    page.getByRole('button', { name: /^restart$/i }),
  ).toBeVisible();

  // Step 7 — negative assertion: the "disconnected" / Reconnect
  // overlay (the 1006 fallback) must NOT show up. We assert the
  // overlay text doesn't contain "disconnected" and that no
  // Reconnect button is present. Together with the positive
  // assertion above, this pins the contract.
  await expect(overlay).not.toContainText('disconnected');
  await expect(
    page.getByRole('button', { name: /^reconnect$/i }),
  ).toHaveCount(0);
});
