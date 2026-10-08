// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Breadcrumb, PageHeader } from './public.tsx';

afterEach(cleanup);

describe('Breadcrumb', () => {
  it('renders the back control with the shared stroked icon instead of a text glyph', () => {
    render(<Breadcrumb ancestor="Today" onNavigate={vi.fn()} onBack={vi.fn()} />);
    const back = screen.getByRole('button', { name: 'Back' });
    expect(back.querySelector('svg.lucide-arrow-left')).not.toBeNull();
    expect(back.textContent).not.toContain('←');
  });
});

it('renders the primary title row before optional secondary rows', () => {
  const view = render(<PageHeader title={<h1>Primary title</h1>} breadcrumb="Breadcrumb" identity="Identity" />);
  const header = view.container.querySelector('header')!;
  expect([...header.children].map((row) => row.textContent)).toEqual(['Primary title', 'Breadcrumb', 'Identity']);
});
