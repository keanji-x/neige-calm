// Unit tests for <CardHead> — the typed slot component (issue #178).
//
// We test the DOM contract that downstream CSS rules in `calm.css` rely
// on: each slot prop lands inside a span carrying the matching slot
// class, slots are omitted when their prop is undefined, the root
// merges `card-head` with any caller-supplied className, and the
// `children` escape-hatch lands between title and status so the
// right-aligned status slot stays pinned to the right.

import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/react';
import { CardHead } from './CardHead';

describe('<CardHead>', () => {
  it('renders all three slots into their slot wrappers when provided', () => {
    const { container } = render(
      <CardHead
        decor={<span data-testid="decor-inner">dots</span>}
        title="My Title"
        status={<span data-testid="status-inner">live</span>}
      />,
    );
    const root = container.querySelector('.card-head');
    expect(root).not.toBeNull();

    const decor = container.querySelector('.card-head-decor');
    expect(decor).not.toBeNull();
    expect(decor?.querySelector('[data-testid="decor-inner"]')).not.toBeNull();

    const title = container.querySelector('.card-head-title');
    expect(title?.textContent).toBe('My Title');

    const status = container.querySelector('.card-head-status');
    expect(status).not.toBeNull();
    expect(status?.querySelector('[data-testid="status-inner"]')).not.toBeNull();
  });

  it('omits slot wrappers when the corresponding prop is undefined', () => {
    const { container } = render(<CardHead title="Only title" />);
    expect(container.querySelector('.card-head-title')).not.toBeNull();
    expect(container.querySelector('.card-head-decor')).toBeNull();
    expect(container.querySelector('.card-head-status')).toBeNull();
  });

  it('merges caller className with the base `card-head` class', () => {
    const { container } = render(
      <CardHead className="codex-card-head card-drag-handle" title="X" />,
    );
    const root = container.querySelector('.card-head');
    expect(root).not.toBeNull();
    expect(root?.classList.contains('codex-card-head')).toBe(true);
    expect(root?.classList.contains('card-drag-handle')).toBe(true);
  });

  it('renders children (escape hatch) between title and status', () => {
    // DOM order matters: the base CSS uses `justify-content: flex-end` on
    // `.card-head-status` to right-pin it, which requires status to be the
    // last flex child. The escape-hatch slot lives between title and status
    // so unanticipated content can't displace the right-pinning.
    const { container } = render(
      <CardHead
        title="T"
        status="S"
      >
        <span data-testid="escape">middle</span>
      </CardHead>,
    );
    const root = container.querySelector('.card-head')!;
    const children = Array.from(root.children);
    const titleIdx = children.findIndex((el) =>
      el.classList.contains('card-head-title'),
    );
    const escapeIdx = children.findIndex(
      (el) => (el as HTMLElement).dataset.testid === 'escape',
    );
    const statusIdx = children.findIndex((el) =>
      el.classList.contains('card-head-status'),
    );
    expect(titleIdx).toBeGreaterThanOrEqual(0);
    expect(escapeIdx).toBeGreaterThan(titleIdx);
    expect(statusIdx).toBeGreaterThan(escapeIdx);
  });

  it('places decor before title in DOM order', () => {
    // The `.card-head-decor` slot is the optional left-of-title decoration
    // (Terminal's 3 CRT dots); DOM order pins it left of the title in the
    // flex parent.
    const { container } = render(
      <CardHead decor="D" title="T" />,
    );
    const root = container.querySelector('.card-head')!;
    const children = Array.from(root.children);
    const decorIdx = children.findIndex((el) =>
      el.classList.contains('card-head-decor'),
    );
    const titleIdx = children.findIndex((el) =>
      el.classList.contains('card-head-title'),
    );
    expect(decorIdx).toBeGreaterThanOrEqual(0);
    expect(titleIdx).toBeGreaterThan(decorIdx);
  });
});
