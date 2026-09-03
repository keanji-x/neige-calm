// Golden-path e2e: prove the app loads, the Today route bootstraps a
// default terminal, and the user can create + navigate into their own
// area from the sidebar.
//
// Prereq: `make dev` (or any equivalent) must be serving the full stack
// at http://localhost:4040. Issue #175 — there is no longer a seeded
// `Scratch` area visible in the sidebar; the kernel mints a hidden
// system area behind the scenes for the default Today terminal, and
// `GET /api/areas` filters it out of the sidebar surface. We mint our
// own user-visible area here and navigate into it.

import { test, expect } from '@playwright/test';

test('loads the calm shell, bootstraps Today, then navigates into a new area', async ({ page }) => {
  await page.goto('/calm/');

  // The sidebar `<aside class="side">` is the first thing the shell paints.
  await expect(page.locator('aside.side')).toBeVisible();

  // The "Today" nav button is always present (and is the default route).
  // Scope by the sidebar's top <nav aria-label="Sidebar navigation"> so a
  // seed/test that produces a "Waiting on you" track titled "Today" doesn't
  // collide with this button on accessible name. See Sidebar.tsx +
  await expect(
    page
      .getByRole('navigation', { name: 'Sidebar navigation' })
      .getByRole('button', { name: 'Today' }),
  ).toBeVisible();

  // Bootstrap anchor for issue #175: after the Today page paints,
  // `useTodayTerminal` writes the resolved card id into localStorage.
  // Wait for that to land — it's the signal that the system area +
  // track + terminal card all exist, even though none of them shows up
  // in the sidebar surface.
  await expect
    .poll(
      () =>
        page.evaluate(() => window.localStorage.getItem('calm.todayCardId')),
      { timeout: 15_000 },
    )
    .not.toBeNull();

  // Cleanly demonstrate the system area is NOT in the sidebar: there
  // should be no area-nav button before we mint our own user area.
  // (The `+ New area` trigger now lives on the Areas header as an icon
  // button — `button.area-nav` no longer matches it, so no filter needed.)
  const sidebarNav = page.getByRole('navigation', { name: 'Areas' });
  await expect(sidebarNav.locator('button.area-nav')).toHaveCount(0);

  // Step: create a user area via the sidebar "+ New area" affordance.
  const areaName = `E2E area ${Date.now()}`;
  await sidebarNav.getByRole('button', { name: /new area/i }).click();
  const nameInput = sidebarNav.getByPlaceholder(/name/i);
  await expect(nameInput).toBeVisible();
  await nameInput.fill(areaName);
  await nameInput.press('Enter');

  // The new area's nav row should appear in the sidebar, with the area
  // name as its accessible name (see Sidebar.tsx). `exact: true` excludes
  // the per-row "Delete area \"<name>\"" button that also includes the
  // area name in its accessible name (strict-mode would otherwise pick
  // up both).
  const areaBtn = sidebarNav.getByRole('button', { name: areaName, exact: true });
  await expect(areaBtn).toBeVisible();
  await areaBtn.click();

  // URL transitions to /calm/area/<id>. We don't pin the id — it's a
  // kernel-generated UUID.
  await expect(page).toHaveURL(/\/calm\/area\/[^/]+$/);

  // And the area page itself rendered — sidebar still visible alongside it.
  await expect(page.locator('aside.side')).toBeVisible();
});
