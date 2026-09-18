import { onlineManager, QueryClient } from '@tanstack/react-query';
import { expect, it, vi } from 'vitest';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { coordinateRecoveryQueries } from './recovery-queries.ts';

it('joins platform reachability with recovery and releases its global listeners before an ordinary web mount', () => {
  const add = vi.spyOn(window, 'addEventListener'); const remove = vi.spyOn(window, 'removeEventListener');
  const client = new QueryClient(); const access = new RecoveryAccess();
  client.mount(); const release = coordinateRecoveryQueries(access, client);
  try {
    access.change('connected'); expect(onlineManager.isOnline()).toBe(true);
    window.dispatchEvent(new Event('offline')); expect(onlineManager.isOnline()).toBe(false);
    access.invalidate('recovering'); access.change('connected'); expect(onlineManager.isOnline()).toBe(false);
    window.dispatchEvent(new Event('online')); expect(onlineManager.isOnline()).toBe(true);
    access.invalidate('recovering'); window.dispatchEvent(new Event('online')); expect(onlineManager.isOnline()).toBe(false);
  } finally { release(); client.unmount(); }
  const networkAdds = add.mock.calls.filter(([name]) => name === 'online' || name === 'offline');
  const networkRemoves = remove.mock.calls.filter(([name]) => name === 'online' || name === 'offline');
  for (const [name, listener] of networkAdds) expect(networkRemoves.some(([removedName, removedListener]) => name === removedName && listener === removedListener)).toBe(true);
  client.mount();
  try {
    window.dispatchEvent(new Event('offline')); expect(onlineManager.isOnline()).toBe(false);
    window.dispatchEvent(new Event('online')); expect(onlineManager.isOnline()).toBe(true);
  } finally { client.unmount(); add.mockRestore(); remove.mockRestore(); }
});
