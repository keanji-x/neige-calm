import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from '@tanstack/react-router';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';

import { Dialog } from '../../ui/dialog/public.tsx';
import { PendingRoute } from './pending-route.tsx';

afterEach(cleanup);

it('restores focus to the missing PendingRoute title when the Dialog opener is removed', async () => {
  const router = createRouter({
    routeTree: createRootRoute({ component: () => <PendingRoute label="Track" owner="track" missing /> }),
    history: createMemoryHistory({ initialEntries: ['/'] }),
  });
  await router.load();
  render(<RouterProvider router={router} />);
  const title = await screen.findByRole('heading', { name: "That track isn't here anymore." });
  const opener = document.body.appendChild(document.createElement('button'));
  opener.focus();
  const dialog = render(<Dialog origin={{ kind: 'surface-center' }} open title="Confirm" onClose={vi.fn()} />);
  opener.remove();
  dialog.unmount();
  expect(document.activeElement).toBe(title);
});
