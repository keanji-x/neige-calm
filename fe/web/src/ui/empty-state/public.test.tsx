import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { Icon } from '../icon/public.tsx';
import { EmptyState } from './public.tsx';

afterEach(cleanup);

it.each(['page', 'panel', 'compact'] as const)('keeps the same noninteractive schema in %s', variant => {
  const { container } = render(<EmptyState variant={variant} icon={<Icon name="file" />} title="暂无内容" description="内容会显示在这里。" />);
  const title = screen.getByRole('heading', { name: '暂无内容' });
  const icon = title.previousElementSibling;
  expect(icon?.getAttribute('aria-hidden')).toBe('true');
  expect(icon?.querySelector('svg')).toBeTruthy();
  expect(title.parentElement?.nextElementSibling?.textContent).toBe('内容会显示在这里。');
  expect(container.querySelectorAll('button, a, input, textarea, [tabindex]')).toHaveLength(0);
});

it('annotates only the title with its projection marker', () => {
  const { container } = render(<EmptyState variant="compact" titleFieldMarker="empty" icon={<Icon name="tools" />} title="No cards yet." description="Cards appear here." />);
  expect(screen.getByRole('heading', { name: 'No cards yet.' }).getAttribute('data-nc-field')).toBe('empty');
  expect(container.querySelectorAll('[data-nc-field]')).toHaveLength(1);
  expect(container.querySelector('[data-nc-field]')?.textContent).toBe('No cards yet.');
});

it('omits projection markers when none is supplied', () => {
  const { container } = render(<EmptyState icon={<Icon name="tools" />} title="No cards yet." description="Cards appear here." />);
  expect(container.querySelector('[data-nc-field]')).toBeNull();
});
