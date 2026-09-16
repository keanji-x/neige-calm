import { render, cleanup } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { MobileAccessPane, type MobileAccessPaneProps } from './mobile-access.tsx';

afterEach(cleanup);

it('shows the private address and separate readiness at phone width', async () => {
  await page.viewport(390, 844);
  const props: MobileAccessPaneProps = {
    status: {
      provider: 'private-tailnet', available: true, publicUrl: 'https://neige.example-tailnet.ts.net', pending: [], devices: [],
      tailnet: {
        desiredEnabled: true, phase: 'online', processRunning: true, childPid: 123,
        nodeState: 'online', httpsReady: true, upstreamReady: true,
        origin: 'https://neige.example-tailnet.ts.net', dnsName: 'neige.example-tailnet.ts.net',
        nodeId: 'fixture-stable-id', addresses: ['100.64.0.10'], detail: 'Private Tailnet HTTPS is ready',
      },
    },
    invitation: null, login: null, busy: false, error: null,
    onBack: vi.fn(), onRefresh: vi.fn(), onEnable: vi.fn(), onDisable: vi.fn(),
    onLogin: vi.fn(), onLogout: vi.fn(), onCreate: vi.fn(), onApprove: vi.fn(), onRevoke: vi.fn(),
  };
  render(<MobileAccessPane {...props} />);
  await expect.element(page.getByRole('button', { name: 'Disable', exact: true })).toBeVisible();
  await expect.element(page.getByText('https://neige.example-tailnet.ts.net', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Sign out of Tailnet', exact: true })).toBeVisible();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(390);
  await page.screenshot({ path: '../../../../test-results/private-tailnet-settings.png' });
});
