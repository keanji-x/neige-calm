import { fireEvent, render, screen, cleanup } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MobileAccessPane, type MobileAccessPaneProps } from './mobile-access.tsx';

afterEach(cleanup);

function props(overrides: Partial<MobileAccessPaneProps> = {}): MobileAccessPaneProps {
  return {
    status: { provider: 'funnel', tailnet: null, available: true, publicUrl: null, pending: [], devices: [] },
    login: null, onLogin: vi.fn(), onLogout: vi.fn(),
    invitation: null, busy: false, error: null,
    onBack: vi.fn(), onRefresh: vi.fn(), onEnable: vi.fn(), onDisable: vi.fn(),
    onCreate: vi.fn(), onApprove: vi.fn(), onRevoke: vi.fn(), ...overrides,
  };
}

describe('Mobile access settings', () => {
  it('shows unavailable configuration without offering an enable action', () => {
    render(<MobileAccessPane {...props({ status: { provider: 'unavailable', tailnet: null, available: false, publicUrl: null, pending: [], devices: [] } })} />);
    expect(screen.getByText('Unavailable')).not.toBeNull();
    expect(screen.queryByRole('button', { name: 'Enable' })).toBeNull();
  });

  it('requires an explicit click on the matching claim before approval', () => {
    const p = props({ status: { provider: 'funnel', tailnet: null, available: true, publicUrl: 'https://pair.example.ts.net',
      pending: [{ id: 'claim-1', deviceName: 'My phone', verificationCode: '123456' }], devices: [] } });
    render(<MobileAccessPane {...p} />);
    expect(p.onApprove).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Approve 123456' }));
    expect(p.onApprove).toHaveBeenCalledExactlyOnceWith('claim-1');
  });

  it('revokes the selected device and keeps failures visible', () => {
    const p = props({ error: 'Connection could not start', status: { provider: 'funnel', tailnet: null, available: true,
      publicUrl: 'https://pair.example.ts.net', pending: [], devices: [{ id: 'device-1', deviceName: 'My phone' }] } });
    render(<MobileAccessPane {...p} />);
    expect(screen.getByRole('alert').textContent).toContain('Connection could not start');
    fireEvent.click(screen.getByRole('button', { name: 'Revoke' }));
    expect(p.onRevoke).toHaveBeenCalledExactlyOnceWith('device-1');
  });
});

const privateStatus = Object.freeze({
  provider: 'private-tailnet' as const, available: true, publicUrl: null, pending: [], devices: [],
  tailnet: {
    desiredEnabled: true, phase: 'needs-login' as const, processRunning: true, childPid: 123,
    nodeState: 'needs-login' as const, httpsReady: false, upstreamReady: true,
    origin: null, dnsName: null, nodeId: null, addresses: [], detail: 'Sign in to authorize this node',
  },
});

it('keeps Disable available while a running private node needs login', () => {
  const p = props({ status: privateStatus });
  render(<MobileAccessPane {...p} />);
  expect(screen.queryByRole('button', { name: 'Enable' })).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Disable' }));
  expect(p.onDisable).toHaveBeenCalledOnce();
  fireEvent.click(screen.getByRole('button', { name: 'Start sign-in' }));
  expect(p.onLogin).toHaveBeenCalledOnce();
  expect(p.onLogout).not.toHaveBeenCalled();
});

it('shows a login link only for the current explicit login operation', () => {
  const p = props({ status: privateStatus, login: { loginUrl: 'https://login.tailscale.com/a/fixture', displayForSeconds: 120 } });
  render(<MobileAccessPane {...p} />);
  expect(screen.getByRole('link', { name: 'Continue to Tailscale' }).getAttribute('rel')).toBe('noreferrer');
  fireEvent.click(screen.getByRole('button', { name: 'Sign out of Tailnet' }));
  expect(p.onLogout).toHaveBeenCalledOnce();
});
