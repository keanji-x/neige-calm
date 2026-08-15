// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Breadcrumb } from './public.tsx';

afterEach(cleanup);

describe('Breadcrumb', () => {
  it('renders the back control with the shared stroked icon instead of a text glyph', () => {
    render(<Breadcrumb ancestor="Today" onNavigate={vi.fn()} onBack={vi.fn()} />);
    const back = screen.getByRole('button', { name: 'Back' });
    expect(back.querySelector('svg')?.querySelector('path')?.getAttribute('d')).toBe('M13 8H3.5');
    expect(back.textContent).not.toContain('←');
  });
});
