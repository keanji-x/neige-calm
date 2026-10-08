// @vitest-environment jsdom
import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { Icon, type IconName } from './public.tsx';

afterEach(cleanup);

const names: readonly IconName[] = [
  'chevron-left', 'chevron-right', 'arrow-left', 'arrow-up', 'plus', 'close',
  'agent', 'claude', 'codex', 'terminal', 'tools', 'tasks', 'pin',
  'status-waiting', 'status-running', 'status-failed', 'status-done', 'status-exited',
  'chat', 'notification', 'folder', 'file', 'fullscreen',
];

describe('Icon', () => {
  it.each(names)('renders the %s stroked SVG', (name) => {
    const { container } = render(<Icon name={name} />);
    const svg = container.querySelector('svg');
    expect(svg).toBeTruthy();
    expect(svg?.getAttribute('stroke-width')).toBe('2');
  });

  it('uses distinct CSS classes for the default md and sm sizes', () => {
    const { container } = render(<><Icon name="plus" /><Icon name="plus" size="sm" /></>);
    const [md, sm] = container.querySelectorAll('svg');
    expect(md.getAttribute('class')).not.toBe(sm.getAttribute('class'));
  });

  it.each(names)('keeps %s in the common coordinate system without per-icon transforms', (name) => {
    const { container } = render(<Icon name={name} />);
    const svg = container.querySelector('svg')!;
    expect(svg.getAttribute('viewBox')).toBe('0 0 24 24');
    expect(svg.querySelector('[transform]')).toBeNull();
    expect(svg.getAttribute('fill')).toBe('none');
    expect(svg.getAttribute('stroke-linecap')).toBe('round');
    expect(svg.getAttribute('stroke-linejoin')).toBe('round');
  });
});
