// Unit tests for <CardHead> — the typed slot component (issue #178).
//
// We test the DOM contract that downstream CSS rules in `calm.css` rely
// on: each slot prop lands inside a span carrying the matching slot
// class, slots are omitted when their prop is undefined, the root
// merges `card-head` with any caller-supplied className, and the
// `children` escape-hatch lands between title and status so the
// right-aligned status slot stays pinned to the right.

import { describe, it, expect, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/react';
import { CardHead } from './CardHead';

describe('<CardHead>', () => {
  it('renders title + status slots into their slot wrappers when provided', () => {
    const { container } = render(
      <CardHead
        title="My Title"
        status={<span data-testid="status-inner">live</span>}
      />,
    );
    const root = container.querySelector('.card-head');
    expect(root).not.toBeNull();

    const title = container.querySelector('.card-head-title');
    expect(title?.textContent).toBe('My Title');

    const status = container.querySelector('.card-head-status');
    expect(status).not.toBeNull();
    expect(status?.querySelector('[data-testid="status-inner"]')).not.toBeNull();
  });

  it('omits slot wrappers when the corresponding prop is undefined', () => {
    const { container } = render(<CardHead title="Only title" />);
    expect(container.querySelector('.card-head-title')).not.toBeNull();
    expect(container.querySelector('.card-head-status')).toBeNull();
  });

  it('merges caller className with the base `card-head` class', () => {
    const { container } = render(
      <CardHead className="card-drag-handle" title="X" />,
    );
    const root = container.querySelector('.card-head');
    expect(root).not.toBeNull();
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

  it('omits className artifact when caller passes none', () => {
    // Guard against the `card-head ${undefined}` footgun — when no
    // className is supplied the root's class attribute must be exactly
    // `card-head` (no trailing space, no literal `"undefined"` token).
    const { container } = render(<CardHead title="T" />);
    const root = container.firstChild as HTMLElement;
    expect(root.getAttribute('class')).toBe('card-head');
  });

  it('synthesises a letter-avatar icon when title is a string and `icon` is omitted', () => {
    const { container } = render(<CardHead title="Codex" />);
    const icon = container.querySelector('.card-head-icon');
    expect(icon).not.toBeNull();
    // First-letter uppercase from the title's first word.
    expect(icon?.textContent).toBe('C');
    // Letter variant + a palette modifier class are both applied so the
    // calm.css typography + colour rules can hang off either.
    expect(icon?.classList.contains('card-head-icon--letter')).toBe(true);
    const hasPalette = Array.from(icon!.classList).some((c) =>
      /^card-head-icon--c\d$/.test(c),
    );
    expect(hasPalette).toBe(true);
  });

  it('letter-avatar palette is deterministic per title (same title → same palette class)', () => {
    const a = render(<CardHead title="Terminal" />).container.querySelector(
      '.card-head-icon',
    )!;
    const b = render(<CardHead title="Terminal" />).container.querySelector(
      '.card-head-icon',
    )!;
    const paletteOf = (el: Element) =>
      Array.from(el.classList).find((c) => /^card-head-icon--c\d$/.test(c));
    expect(paletteOf(a)).toBe(paletteOf(b));
  });

  it('uses caller-supplied icon node instead of the letter-avatar fallback', () => {
    const { container } = render(
      <CardHead
        title="Codex"
        icon={<span data-testid="custom-icon">★</span>}
      />,
    );
    const icon = container.querySelector('.card-head-icon');
    expect(icon).not.toBeNull();
    expect(icon?.querySelector('[data-testid="custom-icon"]')).not.toBeNull();
    // No letter-avatar synthesis when a custom icon is provided.
    expect(icon?.classList.contains('card-head-icon--letter')).toBe(false);
  });

  it('omits the icon when title is not a plain string and no `icon` is passed', () => {
    const { container } = render(
      <CardHead title={<span>rich</span>} />,
    );
    expect(container.querySelector('.card-head-icon')).toBeNull();
  });

  it('places the icon before the title in DOM order', () => {
    const { container } = render(<CardHead title="Codex" />);
    const root = container.querySelector('.card-head')!;
    const children = Array.from(root.children);
    const iconIdx = children.findIndex((el) =>
      el.classList.contains('card-head-icon'),
    );
    const titleIdx = children.findIndex((el) =>
      el.classList.contains('card-head-title'),
    );
    expect(iconIdx).toBeGreaterThanOrEqual(0);
    expect(titleIdx).toBeGreaterThan(iconIdx);
  });

  it('omits the close button when `onClose` is undefined', () => {
    const { container } = render(<CardHead title="T" />);
    expect(container.querySelector('.card-grid-close')).toBeNull();
  });

  it('renders the close button inside the head when `onClose` is provided', () => {
    const { container } = render(<CardHead title="T" onClose={() => {}} />);
    const head = container.querySelector('.card-head')!;
    const close = head.querySelector('.card-grid-close');
    expect(close).not.toBeNull();
    // Button is `type="button"` so a click never accidentally submits a form.
    expect((close as HTMLButtonElement).type).toBe('button');
  });

  it('click on close button invokes the `onClose` callback', () => {
    const onClose = vi.fn();
    const { container } = render(<CardHead title="T" onClose={onClose} />);
    const close = container.querySelector(
      '.card-grid-close',
    ) as HTMLButtonElement;
    fireEvent.click(close);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('propagates `closeAriaLabel` to the close button', () => {
    const { container } = render(
      <CardHead title="T" onClose={() => {}} closeAriaLabel="Remove panel" />,
    );
    const close = container.querySelector('.card-grid-close');
    expect(close?.getAttribute('aria-label')).toBe('Remove panel');
  });

  it("defaults the close button's aria-label to 'Close' when not provided", () => {
    const { container } = render(<CardHead title="T" onClose={() => {}} />);
    const close = container.querySelector('.card-grid-close');
    expect(close?.getAttribute('aria-label')).toBe('Close');
  });

  it('mouseDown on the close button stops propagation (drag-init blocker)', () => {
    // The head usually carries `.card-drag-handle` for RGL; a mousedown on
    // the close button that bubbled up would initiate a drag.
    const onParentMouseDown = vi.fn();
    const { container } = render(
      // eslint-disable-next-line jsx-a11y/no-static-element-interactions, jsx-a11y/no-noninteractive-element-interactions
      <div onMouseDown={onParentMouseDown}>
        <CardHead title="T" onClose={() => {}} />
      </div>,
    );
    const close = container.querySelector(
      '.card-grid-close',
    ) as HTMLButtonElement;
    fireEvent.mouseDown(close);
    expect(onParentMouseDown).not.toHaveBeenCalled();
  });

  it('renders the close button structurally after the status slot', () => {
    // Render order inside the head: decor (icon) → title → children →
    // status → close. The close is last so absolute positioning is its
    // own concern and the flex flow of the four named slots is undisturbed.
    const { container } = render(
      <CardHead
        title="T"
        status="S"
        onClose={() => {}}
      >
        <span data-testid="escape">middle</span>
      </CardHead>,
    );
    const root = container.querySelector('.card-head')!;
    const children = Array.from(root.children);
    const statusIdx = children.findIndex((el) =>
      el.classList.contains('card-head-status'),
    );
    const closeIdx = children.findIndex((el) =>
      el.classList.contains('card-grid-close'),
    );
    expect(statusIdx).toBeGreaterThanOrEqual(0);
    expect(closeIdx).toBeGreaterThan(statusIdx);
  });
});
