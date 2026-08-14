// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { LoginPage } from '../../features/auth/login-page/public.tsx';
import { loginWithTransport } from './login.ts';

afterEach(cleanup);

describe('production login assembly', () => {
  it('CAP-LOGIN-002 maps a production 401 to credential copy without broadcasting', async () => {
    const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
    const broadcast = vi.fn(); unauthorized.subscribe(broadcast);
    const transport = { unauthorized, send: vi.fn().mockResolvedValue({
      status: 401, statusText: 'Unauthorized', body: { code: 'unauthorized', error: 'unauthorized' },
    }) };
    render(<LoginPage login={(username, password) => loginWithTransport(transport, username, password)} reload={vi.fn()} />);
    await userEvent.type(screen.getByLabelText('Username'), 'owner');
    await userEvent.type(screen.getByLabelText('Password'), 'wrong');
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect((await screen.findByRole('alert')).textContent).toBe('Wrong username or password.');
    expect(broadcast).not.toHaveBeenCalled();
  });
});
