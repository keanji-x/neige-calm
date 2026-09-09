import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';

import { Chart } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

it('keeps donut selection on the same observation after live rows move or disappear', async () => {
  const user = userEvent.setup();
  const props = { kind: 'donut' as const, label: 'Weights', color: '#4a5f9b', height: 220, unit: 'CNY' };
  const view = render(<div style={{ width: 400 }}><Chart {...props} points={[{ x: 'A', value: 10 }, { x: 'B', value: 20 }, { x: 'C', value: 30 }]}/></div>);
  await user.click(screen.getByRole('button', { name: /^B\s*33\.3%$/ }));
  view.rerender(<div style={{ width: 400 }}><Chart {...props} points={[{ x: 'B', value: 20 }, { x: 'C', value: 30 }]}/></div>);
  expect(screen.getByRole('button', { name: /^B\s*40\.0%$/ }).getAttribute('aria-pressed')).toBe('true');
  expect(screen.getByRole('button', { name: /^C\s*60\.0%$/ }).getAttribute('aria-pressed')).toBe('false');
  view.rerender(<div style={{ width: 400 }}><Chart {...props} points={[{ x: 'C', value: 30 }]}/></div>);
  expect(screen.getByRole('button', { name: /^C\s*100\.0%$/ }).getAttribute('aria-pressed')).toBe('false');
});
