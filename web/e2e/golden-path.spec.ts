// Golden-path e2e: prove that the app loads, the sidebar renders the
// seeded Scratch cove, and clicking it changes the route to `/cove/$id`.
//
// Prereq: `make dev` (or any equivalent) must be serving the full stack
// at http://localhost:4040. The docker MockRepo seeds a "Scratch" cove
// by default — we anchor on its name rather than DOM index so future
// seed reorderings don't flake this test.

import { test, expect } from '@playwright/test';

test('loads the calm shell and navigates into the Scratch cove', async ({ page }) => {
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

  // Find the seeded "Scratch" cove row in the sidebar and click it.
  // The Sidebar button's accessible name is the cove name (plus an
  // optional " <N>" wave-count suffix when waves exist) — see
  // Sidebar.tsx:62-77. A regex tolerates the suffix.
  const scratch = page.getByRole('button', { name: /scratch/i });
  await expect(scratch).toBeVisible();
  await scratch.click();

  // URL changes to /calm/cove/<id>. The router declares basepath '/calm'
  // (matching Vite's base) so internal navigation produces the prefixed
  // URL. We don't pin the id — it depends on seed timestamp / repo.
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

  // And the cove page itself rendered — sidebar still visible alongside it.
  await expect(page.locator('aside.side')).toBeVisible();
});
