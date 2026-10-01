import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import '../../../styles/entry.css';
import { NewTrackForm } from './public.tsx';

afterEach(cleanup);

it('sizes the homepage mark with the title font and keeps it on the first text line', async () => {
  await page.viewport(1100, 800);
  const { container } = render(<NewTrackForm submitting={false} error={null} templates={[]} templatesLoaded
    initialTemplateId={null} initialCwd={null} loadTemplate={vi.fn(() => Promise.reject(new Error('No template selected')))}
    onManageRecipes={vi.fn()} onSubmit={vi.fn()}
    listDirectory={vi.fn(() => Promise.resolve({ path: '/', parent: null, entries: [] }))} />);
  const heading = screen.getByRole('heading', { name: 'What would you like to work on?' });
  const mark = container.querySelector<SVGSVGElement>('[data-nc-motion="creation"]')!.parentElement!;
  for (const size of [22, 30]) {
    heading.style.fontSize = `${size}px`;
    expect(mark.getBoundingClientRect().height).toBeCloseTo(size, 1);
    expect(mark.getBoundingClientRect().width).toBeCloseTo(size, 1);
  }
  heading.style.inlineSize = '180px';
  const iconBox = mark.getBoundingClientRect();
  const range = document.createRange();
  range.selectNode(heading.lastChild!);
  const textBox = range.getClientRects()[0];
  expect(iconBox.top).toBeLessThan(textBox.bottom);
  expect(iconBox.bottom).toBeGreaterThan(textBox.top);
  await page.viewport(390, 800);
  expect(mark.getBoundingClientRect().height).toBe(0);
});
