// @vitest-environment jsdom
import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { Icon, type IconName } from './public.tsx';

afterEach(cleanup);

const names: readonly IconName[] = [
  'chevron-left', 'chevron-right', 'arrow-left', 'arrow-up', 'plus', 'close', 'reset',
];

describe('Icon', () => {
  it.each(names)('renders the %s stroked SVG', (name) => {
    const { container } = render(<Icon name={name} />);
    const svg = container.querySelector('svg');
    expect(svg).toBeTruthy();
    expect(svg?.getAttribute('stroke-width')).toBe('1.5');
  });

  it('centres the reset line work at y=8 after correcting its source paths', () => {
    const { container } = render(<Icon name="reset" />);
    expect([...container.querySelectorAll('path')].map((path) => path.getAttribute('d')))
      .toEqual(['M3 3.12v3.25h3.25', 'M3.35 6.12a5 5 0 1 1 .15 4.1']);
  });

  it('uses distinct CSS classes for the default md and sm sizes', () => {
    const { container } = render(<><Icon name="plus" /><Icon name="plus" size="sm" /></>);
    const [md, sm] = container.querySelectorAll('svg');
    expect(md.getAttribute('class')).not.toBe(sm.getAttribute('class'));
  });

  it.each(names)('keeps every %s path inside the 0.85 optical-inset group', (name) => {
    const { container } = render(<Icon name={name} />);
    const svg = container.querySelector('svg');
    const inset = svg?.querySelector(':scope > g');
    expect(inset?.getAttribute('transform')).toBe('translate(8 8) scale(0.85) translate(-8 -8)');
    expect(inset?.querySelectorAll('path')).toHaveLength(svg?.querySelectorAll('path').length ?? -1);
  });
});
