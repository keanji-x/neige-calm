// @vitest-environment jsdom
import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { Icon, type IconName } from './public.tsx';

afterEach(cleanup);

const names: readonly IconName[] = [
  'chevron-left', 'chevron-right', 'arrow-left', 'arrow-up', 'plus', 'close',
  'chat', 'notification', 'folder', 'file',
];

describe('Icon', () => {
  it.each(names)('renders the %s stroked SVG', (name) => {
    const { container } = render(<Icon name={name} />);
    const svg = container.querySelector('svg');
    expect(svg).toBeTruthy();
    expect(svg?.getAttribute('stroke-width')).toBe('1.5');
  });

  /* The reset arc used to be pinned here, path data and all, because its
     source geometry had been corrected to centre the line work at y=8. That
     glyph is gone with the control it drew (#1139) and nothing else in the set
     had a hand-corrected arc, so the assertion has no subject left. What
     survives it is the two `it.each(names)` cases below and above: they run
     over whatever the set actually contains, so they cover the next icon
     without being edited. */

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
