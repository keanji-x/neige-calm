import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ProviderAuthenticationNotice } from './provider-authentication.tsx';
afterEach(() => { cleanup(); document.body.replaceChildren(); });
for (const width of [390, 1280]) {
  it(`shows complete notice and Settings at ${width}px`, async () => {
    await page.viewport(width, 844);
    const text = 'Codex sign-in needs renewal. Sign in again for this server, then retry.';
    const transport: ApiTransportPort = { send: () => Promise.resolve({ status: 200, statusText: 'OK', body: [
      { provider: 'codex', status: 'unavailable', reason: text, checked_at_ms: 1, authentication_notice: { kind: 'sign_in_required', revision: '1', text } },
    ] }) };
    const open = vi.fn();
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(<QueryClientProvider client={client}><ProviderAuthenticationNotice transport={transport}
      unauthorized={createUnauthorizedChannel({ enqueue: (task) => task() })} onOpenPlanners={open} /></QueryClientProvider>);
    await expect.element(page.getByText(text)).toBeVisible();
    await expect.element(page.getByRole('button', { name: 'Settings' })).toBeVisible();
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
    const notice = page.getByRole('alert').element();
    expect(notice.scrollWidth).toBeLessThanOrEqual(notice.clientWidth);
    await userEvent.click(page.getByRole('button', { name: 'Settings' }));
    expect(open).toHaveBeenCalledOnce();
    await page.screenshot({ path: `../../../../test-results/provider-authentication-${width}.png` });
  });
}
