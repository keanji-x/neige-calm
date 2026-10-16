// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { RecentFiles } from './public.tsx';

afterEach(cleanup);

describe('RecentFiles', () => {
  it('omits the module until a file has been opened', () => {
    const { container } = render(<RecentFiles paths={[]} onOpen={vi.fn()} />);
    expect(container.childElementCount).toBe(0);
  });

  it('shows only the basename, while the full path still identifies the control', async () => {
    const onOpen = vi.fn();
    render(<RecentFiles paths={['src/app/main.ts', 'README.md']} onOpen={onOpen} />);
    const heading = screen.getByRole('heading', { name: 'Recent files' });
    expect(heading.querySelector('svg')).not.toBeNull();
    expect(heading.querySelector('svg')?.getAttribute('aria-hidden')).toBe('true');
    expect(screen.getByText('main.ts')).toBeTruthy();
    expect(screen.queryByText('src/app/main.ts')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Open src/app/main.ts' }));
    expect(onOpen).toHaveBeenCalledWith('src/app/main.ts');
  });
});
