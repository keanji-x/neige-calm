import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page } from 'vitest/browser';
import { NewTrackForm } from '../../features/area/new-track/public.tsx';
import { DepthFeedback } from '../../ui/motion/depth-feedback.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import '../../styles/entry.css';

afterEach(cleanup);

it('opens the isolation menu by touch and keeps private Worktree disabled at 390px', async () => {
  await page.viewport(390, 844);
  render(<ThemeProvider><NewTrackForm submitting={false} error={null} templates={[]} templatesLoaded
    initialTemplateId={null} initialCwd={null} loadTemplate={vi.fn()} onManageRecipes={vi.fn()}
    listDirectory={vi.fn()} onSubmit={vi.fn()} /></ThemeProvider>);
  await commands.tap('button[aria-label="Workspace isolation: None"]');
  await expect.element(page.getByRole('menuitem', { name: /Worktree/ })).toHaveAttribute('aria-disabled', 'true');
  await expect.element(page.getByText('Private Neige folder: no worktree. Choose a Git folder to use one.')).toBeVisible();
  await expect.element(page.getByRole('menuitem', { name: /Worktree/ })).toBeDisabled();
  await commands.tap('[role="menu"][aria-label="Workspace isolation: None"] [role="menuitem"]:not([aria-disabled="true"])');
  await expect.element(page.getByRole('button', { name: 'Workspace isolation: None' })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Not isolated · no guarantees' })).not.toBeInTheDocument();
  expect(document.documentElement.scrollWidth).toBe(390);
});

it('hands compact Folder touch ownership to the directory modal and restores reachable options', async () => {
  await page.viewport(390, 844);
  render(<ThemeProvider><DepthFeedback /><NewTrackForm submitting={false} error={null} templates={[]} templatesLoaded
    initialTemplateId={null} initialCwd="/repo" loadTemplate={vi.fn()} onManageRecipes={vi.fn()}
    listDirectory={path => Promise.resolve({ path: path ?? '/repo', parent: '/', entries: Array.from({ length: 16 }, (_, i) => ({
      name: `folder-${i}`, path: `/repo/folder-${i}`, isDirectory: true,
    })) })} onSubmit={vi.fn()} /></ThemeProvider>);
  for (const select of [false, true]) {
    await commands.tap('button[aria-label="Track options"]');
    await expect.element(page.getByRole('dialog', { name: 'Track options', exact: true })).toBeVisible();
    await commands.tap('button[aria-label^="Folder:"]');
    const dialog = page.getByRole('dialog', { name: 'Choose a directory' });
    await expect.element(dialog).toBeVisible();
    await expect.poll(() => document.querySelectorAll(':popover-open').length).toBe(0);
    await expect.element(page.getByRole('combobox', { name: 'Directory path' })).toHaveFocus();
    const list = page.getByRole('listbox', { name: 'Directory entries' }).element();
    const bounds = list.getBoundingClientRect();
    const rows = [...list.querySelectorAll<HTMLElement>('[role="option"]')].filter(e => {
      const r = e.getBoundingClientRect(); return r.top >= bounds.top && r.bottom <= bounds.bottom;
    });
    expect(rows.length).toBeGreaterThan(5);
    for (const row of rows) {
      const r = row.getBoundingClientRect();
      expect(row.contains(document.elementFromPoint(r.left + 40, r.top + r.height / 2))).toBe(true);
    }
    await commands.tap(`[role="option"][id="${rows.at(-1)!.id}"]`);
    await expect.element(page.getByRole('combobox', { name: 'Directory path' })).not.toHaveValue('/repo/');
    await commands.tap(select ? 'button:has-text("Select this directory")' : 'button:has-text("Cancel")');
    await expect.element(dialog).not.toBeInTheDocument();
    await expect.element(page.getByRole('button', { name: 'Track options', exact: true })).toHaveFocus();
  }
});
