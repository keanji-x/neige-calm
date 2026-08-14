// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { LoginPage } from './public.tsx';

afterEach(cleanup);

describe('login page oracle contracts', () => {
  it('INV-LOGIN-001 reloads the whole document after successful login', async () => {
    const reload = vi.fn();
    render(<LoginPage login={() => Promise.resolve({ userId: 'u', displayName: 'Owner', role: 'admin', sessionId: 's' })} reload={reload} />);
    await userEvent.type(screen.getByLabelText('Username'), 'owner');
    await userEvent.type(screen.getByLabelText('Password'), 'secret');
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect(reload).toHaveBeenCalledOnce();
  });

  it('CAP-LOGIN-002 distinguishes rejected credentials, exceptions, and submitting state', async () => {
    let finish!: (value: null) => void;
    const login = vi.fn(() => new Promise<null>((resolve) => { finish = resolve; }));
    const view = render(<LoginPage login={login} reload={vi.fn()} />);
    await userEvent.type(screen.getByLabelText('Username'), 'owner');
    await userEvent.type(screen.getByLabelText('Password'), 'bad');
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Signing in…' }).disabled).toBe(true);
    finish(null);
    expect((await screen.findByRole('alert')).textContent).toBe('Wrong username or password.');
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Sign in' }).disabled).toBe(false);
    view.unmount();

    render(<LoginPage login={() => Promise.reject(new Error('Server unavailable'))} reload={vi.fn()} />);
    await userEvent.type(screen.getByLabelText('Username'), 'owner');
    await userEvent.type(screen.getByLabelText('Password'), 'secret');
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect((await screen.findByRole('alert')).textContent).toBe('Server unavailable');
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Sign in' }).disabled).toBe(false);
  });

  it('CAP-LOGIN-002 uses the fallback and releases submitting for non-Error exceptions', async () => {
    render(<LoginPage login={() => Promise.reject(new Error())} reload={vi.fn()} />);
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect((await screen.findByRole('alert')).textContent).toBe('Sign-in failed.');
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Sign in' }).disabled).toBe(false);
  });
});
