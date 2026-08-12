// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Drawer } from './public.tsx';

afterEach(cleanup);

function open(props: Partial<Parameters<typeof Drawer>[0]> = {}) {
  return render(
    <Drawer
      open={props.open ?? true}
      title={props.title ?? 'Why the resolver drops a hop'}
      onClose={props.onClose ?? vi.fn()}
      footer={props.footer}
    >
      {props.children ?? <p>the transcript</p>}
    </Drawer>,
  );
}

describe('Drawer', () => {
  it('is not in the tree before it opens', () => {
    render(<Drawer open={false} title="t" onClose={vi.fn()}><p>body</p></Drawer>);
    expect(screen.queryByRole('complementary')).toBeNull();
  });

  /*
   * Closing **retracts**; it does not vanish. The panel is attached to an edge,
   * so it goes back to the edge — and it has to still be mounted to do that.
   */
  it('stays mounted through the retraction after open goes false', () => {
    const { rerender } = open();
    rerender(<Drawer open={false} title="t" onClose={vi.fn()}><p>body</p></Drawer>);
    expect(screen.getByRole('complementary')).toBeTruthy();
  });

  /*
   * …showing what it showed. The caller drops its selection the instant it asks
   * for a close, so a drawer that re-read its props on the way out would slide
   * away blank. This is the assertion that catches that, because "it animates"
   * is not something jsdom can see and "it is empty" is.
   */
  it('keeps the last content it had while retracting', () => {
    const { rerender } = open({ title: 'Why the resolver drops a hop', children: <p>the transcript</p> });
    rerender(<Drawer open={false} title="" onClose={vi.fn()}>{null}</Drawer>);
    expect(screen.getByText('the transcript')).toBeTruthy();
    expect(screen.getByRole('heading').textContent).toBe('Why the resolver drops a hop');
  });

  /* The close control points the way it goes — the same glyph the rail's own
     collapse uses — and says what it does to a screen reader. */
  it('closes with a direction, not a dismissal', () => {
    const onClose = vi.fn();
    open({ onClose });
    const close = screen.getByRole('button', { name: 'Close conversation' });
    expect(close.textContent).toBe('›');
    close.click();
    expect(onClose).toHaveBeenCalled();
  });

  it('puts the footer outside the scrolling body', () => {
    open({ footer: <form aria-label="composer" /> });
    const body = screen.getByText('the transcript').parentElement;
    expect(body?.contains(screen.getByLabelText('composer'))).toBe(false);
  });
});
