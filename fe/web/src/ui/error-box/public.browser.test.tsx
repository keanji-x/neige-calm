import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';
import { ErrorBox } from './public.tsx';

afterEach(() => { cleanup(); document.body.replaceChildren(); });

describe('ErrorBox browser styles', () => {
  it('vertically centers the inline-block decorative dot', () => {
    const { container } = render(<ErrorBox message="Could not load transcript" onRetry={vi.fn()} />);
    const dot = container.querySelector<HTMLElement>('span[aria-hidden="true"]');
    expect(dot).not.toBeNull();
    expect(getComputedStyle(dot!).verticalAlign).toBe('middle');
  });
});


it('shows a spaced error indicator and a bordered retry control in compact mode', () => {
  const { container, getByRole } = render(<ErrorBox message="Unavailable" onRetry={vi.fn()} />);
  const alert = getByRole('alert');
  const style = getComputedStyle(alert);
  expect(style.display).toBe('flex');
  expect(parseFloat(style.gap)).toBeGreaterThan(0);
  const probe = document.createElement('span');
  probe.style.backgroundColor = 'var(--error)';
  alert.append(probe);
  expect(getComputedStyle(container.querySelector('span[aria-hidden="true"]')!).backgroundColor).toBe(getComputedStyle(probe).backgroundColor);
  const retry = getComputedStyle(getByRole('button', { name: 'Retry' }));
  expect(parseFloat(retry.borderTopWidth)).toBeGreaterThan(0);
  expect(retry.borderTopStyle).toBe('solid');
  expect(retry.backgroundColor).not.toBe('rgba(0, 0, 0, 0)');
});
