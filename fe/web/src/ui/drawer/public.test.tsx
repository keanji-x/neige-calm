// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Drawer } from './public.tsx';
import { Dialog } from '../dialog/public.tsx';
import { useState } from '../state/public.ts';

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
    const railButton = document.body.appendChild(document.createElement('button'));
    const pageTitle = document.body.appendChild(document.createElement('h1'));
    pageTitle.tabIndex = -1; pageTitle.dataset.ncPageTitle = '';
    railButton.focus();
    render(<Drawer open={false} title="t" onClose={vi.fn()}><p>body</p></Drawer>);
    expect(screen.queryByRole('complementary')).toBeNull();
    expect(document.activeElement).toBe(railButton);
    railButton.remove(); pageTitle.remove();
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
    /*
     * The title is no longer painted — the head band is gone — so the last
     * frame's title is held on the container's accessible name instead of in a
     * heading. The assertion still binds the same bug it was written for: a
     * drawer that re-read its props on the way out would be named by the empty
     * string this rerender passes, and `getByRole('complementary', { name })`
     * only matches the *retained* name. It is in fact stronger now, because
     * the name is what a screen reader announces rather than decoration.
     */
    expect(screen.getByRole('complementary', { name: 'Why the resolver drops a hop' })).toBeTruthy();
    expect(screen.queryByRole('heading')).toBeNull();
  });

  /*
   * The close **collapses**, it does not destroy.
   *
   * This is the shape assertion, and it is here because the page header's
   * delete-wave control is 58px above this one in the same column at the same
   * 28px size: measured at 1512×950, delete centres on y 36.3 and this centres
   * on y 94.0. An X on both would be one glyph meaning "put away" and "destroy"
   * a pointer-flick apart. So the close is the rail's collapse chevron, and the
   * X path is what must never come back.
   */
  it('closes with a collapse, not the delete X', () => {
    const onClose = vi.fn();
    open({ onClose });
    const close = screen.getByRole('button', { name: 'Close conversation' });
    const paths = [...close.querySelectorAll('path')].map((path) => path.getAttribute('d'));
    expect(paths).toEqual(['M6 3.5 10.5 8 6 12.5']);
    expect(paths).not.toContain('M4 4l8 8');
    expect(close.textContent).not.toContain('›');
    close.click();
    expect(onClose).toHaveBeenCalled();
  });

  it('puts the footer outside the scrolling body', () => {
    open({ footer: <form aria-label="composer" /> });
    const bodyInner = screen.getByText('the transcript').parentElement;
    const scroll = bodyInner?.parentElement;
    const drawer = screen.getByRole('complementary');
    /* The scroller holds the transcript and nothing else now: the controls
       float over it as a sibling, so the body is its only child. */
    expect(scroll?.firstElementChild).toBe(bodyInner);
    expect(scroll?.childElementCount).toBe(1);
    expect(scroll?.parentElement).toBe(drawer);
    expect(screen.getByRole('button', { name: 'Close conversation' }).closest('[data-nc-drawer-scroll]'))
      .toBeNull();
    expect(scroll?.hasAttribute('data-nc-drawer-scroll')).toBe(true);
    expect(screen.getByLabelText('composer').parentElement).toBe(drawer);
  });

  it('restores focus to the opener when it closes', () => {
    const opener = document.body.appendChild(document.createElement('button'));
    opener.focus();
    const view = open();
    expect(document.activeElement).toBe(screen.getByRole('complementary'));
    view.rerender(<Drawer open={false} title="t" onClose={vi.fn()}><p>body</p></Drawer>);
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });

  it('moves focus in without asking the browser to scroll the page', () => {
    const opener = document.body.appendChild(document.createElement('button'));
    opener.focus();
    const focus = vi.spyOn(HTMLElement.prototype, 'focus');
    open();
    expect(focus).toHaveBeenCalledWith({ preventScroll: true });
    focus.mockRestore();
    opener.remove();
  });

  it('closes a real topmost dialog without closing the drawer below it', () => {
    const onDrawerClose = vi.fn();
    function Layers() {
      const [dialogOpen, setDialogOpen] = useState(true);
      return <><Drawer open title="Drawer" onClose={onDrawerClose}>body</Drawer>
        <Dialog open={dialogOpen} title="Confirm" onClose={() => setDialogOpen(false)} /></>;
    }
    render(<Layers />);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onDrawerClose).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog', { name: 'Confirm' })).toBeNull();
  });

  it('closes the drawer when it is above a dialog', () => {
    const onDrawerClose = vi.fn();
    render(<Dialog open title="Confirm" onClose={vi.fn()} />);
    open({ title: 'Drawer', onClose: onDrawerClose });
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onDrawerClose).toHaveBeenCalledOnce();
    expect(screen.getByRole('dialog', { name: 'Confirm' })).toBeTruthy();
  });

  it('closes on Escape when it is the topmost layer', () => {
    const onClose = vi.fn();
    open({ onClose });
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('does not consume Escape already handled by an inner menu', () => {
    const onClose = vi.fn();
    open({ onClose });
    const event = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
    event.preventDefault();
    document.dispatchEvent(event);
    expect(onClose).not.toHaveBeenCalled();
  });

  it('leaves the Escape layer stack while its closing frame retracts', () => {
    const view = open();
    view.rerender(<Drawer open={false} title="t" onClose={vi.fn()}><p>body</p></Drawer>);
    expect(screen.getByRole('complementary').hasAttribute('data-nc-escape-layer')).toBe(false);
  });

  it('falls back to the page title when the opener has been removed', () => {
    const opener = document.body.appendChild(document.createElement('button'));
    const pageTitle = document.body.appendChild(document.createElement('h1'));
    pageTitle.tabIndex = -1;
    pageTitle.dataset.ncPageTitle = '';
    opener.focus();
    const view = open();
    opener.remove();
    view.rerender(<Drawer open={false} title="t" onClose={vi.fn()}><p>body</p></Drawer>);
    expect(document.activeElement).toBe(pageTitle);
    pageTitle.remove();
  });

  it('prefers a surviving opener over the page-title fallback', () => {
    const opener = document.body.appendChild(document.createElement('button'));
    const pageTitle = document.body.appendChild(document.createElement('h1'));
    pageTitle.tabIndex = -1; pageTitle.dataset.ncPageTitle = '';
    opener.focus();
    const view = open();
    view.rerender(<Drawer open={false} title="t" onClose={vi.fn()}><p>body</p></Drawer>);
    expect(document.activeElement).toBe(opener);
    opener.remove(); pageTitle.remove();
  });
});
