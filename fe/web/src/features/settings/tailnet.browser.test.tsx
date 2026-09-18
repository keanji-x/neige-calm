import { render, cleanup } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { MobileAccessPane, type MobileAccessPaneProps } from './mobile-access.tsx';
import qrFixture from './enrollment-qr.fixture.svg';

afterEach(cleanup);

it.each([390, 1280])('shows the private address, scan QR and cancellation at %i pixels', async (width) => {
  await page.viewport(width, 900);
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
    enrollment: { enrollmentId: 'layout-fixture', qrPayload: 'neige-enroll:v2:layout-only', qrImage: qrFixture,
      authKeyExpiresAt: Date.now() + 300_000, pairExpiresAt: Date.now() + 180_000 },
    cleanup: null, onCancelEnrollment: vi.fn(),
    onBack: vi.fn(), onRefresh: vi.fn(), onEnable: vi.fn(), onDisable: vi.fn(),
    onLogin: vi.fn(), onLogout: vi.fn(), onCreate: vi.fn(), onApprove: vi.fn(), onRevoke: vi.fn(),
  };
  render(<MobileAccessPane {...props} />);
  await expect.element(page.getByRole('button', { name: 'Disable', exact: true })).toBeVisible();
  await expect.element(page.getByText('https://neige.example-tailnet.ts.net', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Sign out of Tailnet', exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Add phone', exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Cancel invitation', exact: true })).toBeVisible();
  await expect.element(page.getByAltText('Scan once to join and pair this Neige workspace')).toBeVisible();
  const image = document.querySelector<HTMLImageElement>('img[alt="Scan once to join and pair this Neige workspace"]');
  await expect.poll(() => image?.naturalWidth).toBeGreaterThan(0);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  await page.screenshot({ path: `../../../../test-results/enrollment-settings-${width}.png` });
  await page.getByAltText('Scan once to join and pair this Neige workspace').click();
  if (!image?.parentElement) throw new Error('Missing QR presentation');
  await page.screenshot({ element: image.parentElement, path: `../../../../test-results/enrollment-settings-${width}-qr.png` });
  await page.getByRole('button', { name: 'Cancel invitation', exact: true }).click();
  expect(props.onCancelEnrollment).toHaveBeenCalledOnce();
});
