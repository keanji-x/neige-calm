import { fireEvent, render, screen, cleanup } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MobileAccessPane, type MobileAccessPaneProps } from './mobile-access.tsx';

afterEach(cleanup);

function props(overrides: Partial<MobileAccessPaneProps> = {}): MobileAccessPaneProps {
  return {
    status: { available: true, publicUrl: null, pending: [], devices: [] },
    invitation: null, busy: false, error: null,
    onBack: vi.fn(), onRefresh: vi.fn(), onEnable: vi.fn(), onDisable: vi.fn(),
    onCreate: vi.fn(), onApprove: vi.fn(), onRevoke: vi.fn(), ...overrides,
  };
}

describe('Mobile access settings', () => {
  it('shows unavailable configuration without offering an enable action', () => {
    render(<MobileAccessPane {...props({ status: { available: false, publicUrl: null, pending: [], devices: [] } })} />);
    expect(screen.getByText('Unavailable')).not.toBeNull();
    expect(screen.queryByRole('button', { name: 'Enable' })).toBeNull();
  });

  it('requires an explicit click on the matching claim before approval', () => {
    const p = props({ status: { available: true, publicUrl: 'https://pair.example.ts.net',
      pending: [{ id: 'claim-1', deviceName: 'My phone', verificationCode: '123456' }], devices: [] } });
    render(<MobileAccessPane {...p} />);
    expect(p.onApprove).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Approve 123456' }));
    expect(p.onApprove).toHaveBeenCalledExactlyOnceWith('claim-1');
  });

  it('revokes the selected device and keeps failures visible', () => {
    const p = props({ error: 'Connection could not start', status: { available: true,
      publicUrl: 'https://pair.example.ts.net', pending: [], devices: [{ id: 'device-1', deviceName: 'My phone' }] } });
    render(<MobileAccessPane {...p} />);
    expect(screen.getByRole('alert').textContent).toContain('Connection could not start');
    fireEvent.click(screen.getByRole('button', { name: 'Revoke' }));
    expect(p.onRevoke).toHaveBeenCalledExactlyOnceWith('device-1');
  });
});
