// Golden-path e2e: prove the app loads, the Today route bootstraps a
// default terminal, and the user can create + navigate into their own
// cove from the sidebar.
//
// Prereq: `make dev` (or any equivalent) must be serving the full stack
// at http://localhost:4040. Issue #175 — there is no longer a seeded
// `Scratch` cove visible in the sidebar; the kernel mints a hidden
// system cove behind the scenes for the default Today terminal, and
// `GET /api/coves` filters it out of the sidebar surface. We mint our
// own user-visible cove here and navigate into it.

import { test, expect } from '@playwright/test';

test('loads the calm shell, bootstraps Today, then navigates into a new cove', async ({ page }) => {
  await page.goto('/calm/');

  // The sidebar `<aside class="side">` is the first thing the shell paints.
  await expect(page.locator('aside.side')).toBeVisible();

  // The "Today" nav button is always present (and is the default route).
  // Scope by the sidebar's top <nav aria-label="Sidebar navigation"> so a
  // seed/test that produces a "Waiting on you" wave titled "Today" doesn't
  // collide with this button on accessible name. See Sidebar.tsx +
  // docs/a11y-contract.md §2.2.
  await expect(
    page
      .getByRole('navigation', { name: 'Sidebar navigation' })
      .getByRole('button', { name: 'Today' }),
  ).toBeVisible();

  // Bootstrap anchor for issue #175: after the Today page paints,
  // `useTodayTerminal` writes the resolved card id into localStorage.
  // Wait for that to land — it's the signal that the system cove +
  // wave + terminal card all exist, even though none of them shows up
  // in the sidebar surface.
  await expect
    .poll(
      () =>
        page.evaluate(() => window.localStorage.getItem('calm.todayCardId')),
      { timeout: 15_000 },
    )
    .not.toBeNull();

  // Cleanly demonstrate the system cove is NOT in the sidebar: there
  // should be no cove-nav button before we mint our own user cove.
  // (The "New cove" affordance carries label "New cove" — case-sensitive
  // anchor avoids matching that button.)
  const sidebarNav = page.getByRole('navigation', { name: 'Coves' });
  // Locate cove-nav buttons that aren't "New cove" — they should be empty
  // before the user mints anything.
  await expect(
    sidebarNav.locator('button.cove-nav').filter({ hasNotText: 'New cove' }),
  ).toHaveCount(0);

  // Step: create a user cove via the sidebar "+ New cove" affordance.
  const coveName = `E2E cove ${Date.now()}`;
  await sidebarNav.getByRole('button', { name: /new cove/i }).click();
  const nameInput = sidebarNav.getByPlaceholder(/name/i);
  await expect(nameInput).toBeVisible();
  await nameInput.fill(coveName);
  await nameInput.press('Enter');

  // The new cove's nav row should appear in the sidebar, with the cove
  // name as its accessible name (see Sidebar.tsx).
  const coveBtn = sidebarNav.getByRole('button', { name: new RegExp(coveName, 'i') });
  await expect(coveBtn).toBeVisible();
  await coveBtn.click();

  // URL transitions to /calm/cove/<id>. We don't pin the id — it's a
  // kernel-generated UUID.
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

  // And the cove page itself rendered — sidebar still visible alongside it.
  await expect(page.locator('aside.side')).toBeVisible();
});
