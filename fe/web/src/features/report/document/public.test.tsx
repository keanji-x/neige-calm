// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { ReportDocument } from './public.tsx';

afterEach(cleanup);

const EMPTY = <p>Nothing yet.</p>;

describe('ReportDocument', () => {
  it('renders the empty state when there is no body', () => {
    render(<ReportDocument body={null} empty={EMPTY} />);
    expect(screen.getByText('Nothing yet.')).toBeTruthy();
  });

  it('renders H1 as a section heading below the page title, never as an h1', () => {
    // The page title is the wave's name in the header. A document that emitted
    // its own h1 would give the page two, which is the heading-order failure
    // axe reports and the reason this maps depth 1 to h2.
    const { container } = render(<ReportDocument body={'# Goal\n\nBody text.'} empty={EMPTY} />);
    expect(container.querySelectorAll('h1').length).toBe(0);
    expect(screen.getByRole('heading', { level: 2 }).textContent).toBe('Goal');
  });

  it('renders a tight list item inline, so its text sits on the marker line', () => {
    const { container } = render(<ReportDocument body={'- one\n- two'} empty={EMPTY} />);
    const items = container.querySelectorAll('li');
    expect(items.length).toBe(2);
    // No block wrapper inside the item: that is what put every bullet's text on
    // the line below its marker.
    expect(items[0]?.querySelector('p')).toBeNull();
    expect(items[0]?.textContent).toBe('one');
  });

  it('keeps a task item checkbox disabled, because this surface does not write back', () => {
    render(<ReportDocument body={'- [x] done\n- [ ] open'} empty={EMPTY} />);
    const boxes = screen.getAllByRole('checkbox');
    expect(boxes.length).toBe(2);
    expect(boxes.every((box) => (box as HTMLInputElement).disabled)).toBe(true);
    expect((boxes[0] as HTMLInputElement).checked).toBe(true);
    expect((boxes[1] as HTMLInputElement).checked).toBe(false);
  });

  describe('INV-A11Y-061 — a report emits no native link', () => {
    it('keeps a link label and drops its destination', () => {
      const { container } = render(
        <ReportDocument body={'See [the spec](https://example.com/spec) for details.'} empty={EMPTY} />,
      );
      expect(container.querySelectorAll('a').length).toBe(0);
      expect(container.textContent).toContain('the spec');
      expect(container.innerHTML).not.toContain('example.com');
    });

    it('renders an image as its alt text and never requests the source', () => {
      const { container } = render(<ReportDocument body={'![a diagram](https://example.com/x.png)'} empty={EMPTY} />);
      expect(container.querySelectorAll('img').length).toBe(0);
      expect(container.textContent).toContain('a diagram');
      expect(container.innerHTML).not.toContain('example.com');
    });
  });

  it('drops raw HTML rather than rendering it', () => {
    const { container } = render(
      <ReportDocument body={'<script>alert(1)</script>\n\nAfter.'} empty={EMPTY} />,
    );
    expect(container.querySelectorAll('script').length).toBe(0);
    expect(container.innerHTML).not.toContain('alert(1)');
    expect(container.textContent).toContain('After.');
  });

  it('falls back to the source when the markdown will not parse', () => {
    // A report that exceeds the normalizer's limits is still what the agent
    // wrote; showing it beats showing an error about it.
    const body = `${'> '.repeat(80)}too deep`;
    const { container } = render(<ReportDocument body={body} empty={EMPTY} />);
    expect(container.querySelector('[data-nc-report]')).toBeNull();
    expect(container.textContent).toContain('too deep');
  });
});
