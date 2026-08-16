import { render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { LoginPage } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('login page browser contract', () => {
  it('INV-LOGIN-003 focuses username and declares both credential autocomplete tokens', () => {
    render(<LoginPage login={() => Promise.resolve(null)} reload={vi.fn()} />);
    const username = document.querySelector<HTMLInputElement>('input[name="username"]');
    const password = document.querySelector<HTMLInputElement>('input[name="password"]');
    expect(document.activeElement).toBe(username);
    expect(username?.autocomplete).toBe('username');
    expect(password?.autocomplete).toBe('current-password');
  });
});
