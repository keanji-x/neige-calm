import { render, cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import '../../styles/entry.css';
import { Icon } from './public.tsx';

afterEach(cleanup);

it('draws the complete closed Codex outline inside the common 14px graphic box', () => {
  const { container } = render(<Icon name="codex" size="sm" />);
  const svg = container.querySelector('svg')!;
  const outline = svg.querySelector('path')!;
  const length = outline.getTotalLength();
  const first = outline.getPointAtLength(0);
  const last = outline.getPointAtLength(length);
  expect(length).toBeGreaterThan(65);
  expect(Math.hypot(last.x - first.x, last.y - first.y)).toBeLessThan(.01);
  expect(svg.getBoundingClientRect().width).toBe(14);
  expect(svg.getBoundingClientRect().height).toBe(14);
  const bounds = outline.getBBox();
  expect(bounds.width).toBeGreaterThan(20);
  expect(bounds.height).toBeGreaterThan(20);
});
