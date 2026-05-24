// Unit tests for <LetterAvatar> — the shared card-identity glyph.
//
// These pin the DOM contract that `calm.css` (and the CardHead tests)
// depend on: the first-letter-uppercase glyph, the deterministic palette
// class, and the `aria-hidden` flag that keeps the avatar out of the
// accessible name. Extracted from CardHead (issue: AddPanel card-head menu)
// so AddPanel can render the identical glyph; CardHead delegates here.

import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/react';
import { LetterAvatar } from './LetterAvatar';

describe('<LetterAvatar>', () => {
  it('renders the uppercased first letter of the title', () => {
    const { container } = render(<LetterAvatar title="terminal" />);
    const icon = container.querySelector('.card-head-icon');
    expect(icon?.textContent).toBe('T');
  });

  it('carries the letter variant + a palette modifier class', () => {
    const { container } = render(<LetterAvatar title="codex" />);
    const icon = container.querySelector('.card-head-icon')!;
    expect(icon.classList.contains('card-head-icon--letter')).toBe(true);
    const hasPalette = Array.from(icon.classList).some((c) =>
      /^card-head-icon--c\d$/.test(c),
    );
    expect(hasPalette).toBe(true);
  });

  it('is aria-hidden so the glyph stays out of the accessible name', () => {
    const { container } = render(<LetterAvatar title="codex" />);
    const icon = container.querySelector('.card-head-icon');
    expect(icon?.getAttribute('aria-hidden')).toBe('true');
  });

  it('palette is deterministic per title (djb2 hash)', () => {
    const paletteOf = (el: Element) =>
      Array.from(el.classList).find((c) => /^card-head-icon--c\d$/.test(c));
    const a = render(<LetterAvatar title="terminal" />).container.querySelector(
      '.card-head-icon',
    )!;
    const b = render(<LetterAvatar title="terminal" />).container.querySelector(
      '.card-head-icon',
    )!;
    expect(paletteOf(a)).toBe(paletteOf(b));
  });

  it('renders nothing for a blank title', () => {
    const { container } = render(<LetterAvatar title="   " />);
    expect(container.querySelector('.card-head-icon')).toBeNull();
  });
});
