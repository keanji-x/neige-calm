import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page } from 'vitest/browser';
import { NewTrackForm } from '../../features/area/new-track/public.tsx';
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
