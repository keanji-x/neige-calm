import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { DrawerHeader } from './header.tsx';

afterEach(cleanup);

it('keeps the delete action visible on touch without hover or focus', () => {
  expect(matchMedia('(pointer: coarse)').matches).toBe(true);
  render(<DrawerHeader title="Conversation" closeLabel="Close" onClose={vi.fn()} onDelete={vi.fn()} />);
  expect(getComputedStyle(screen.getByRole('button', { name: 'Delete' })).opacity).toBe('1');
});
