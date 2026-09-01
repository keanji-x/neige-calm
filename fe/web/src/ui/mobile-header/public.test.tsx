// @vitest-environment jsdom
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';

import { MobileHeader } from './public.tsx';

it('keeps a centered title between optional back and action slots', async () => {
  const onBack = vi.fn();
  render(<MobileHeader title="Cards" backLabel="Report" onBack={onBack} actions={<button type="button">More</button>} />);
  expect(screen.getByRole('heading', { name: 'Cards' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'More' })).toBeTruthy();
  await userEvent.click(screen.getByRole('button', { name: 'Back to Report' }));
  expect(onBack).toHaveBeenCalledOnce();
});
