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
    expect(screen.getByRole('heading').textContent).toBe('Why the resolver drops a hop');
  });

  /* The close control points the way it goes and says what it does to a screen
     reader. Its explicit stroke makes it an optical peer of the reset. */
  it('closes with a direction, not a dismissal', () => {
    const onClose = vi.fn();
    open({ onClose });
    const close = screen.getByRole('button', { name: 'Close conversation' });
    const glyph = close.querySelector('svg');
    expect(glyph).toBeTruthy();
    expect(glyph?.querySelector('path')?.getAttribute('d')).toBe('M6 3.5 10.5 8 6 12.5');
    expect(close.textContent).not.toContain('›');
    close.click();
    expect(onClose).toHaveBeenCalled();
  });

  it('puts the footer outside the scrolling body', () => {
    open({ footer: <form aria-label="composer" /> });
    const bodyInner = screen.getByText('the transcript').parentElement;
    const scroll = bodyInner?.parentElement;
    const drawer = screen.getByRole('complementary');
    expect(scroll?.firstElementChild).toBe(screen.getByRole('heading').parentElement);
    expect(scroll?.parentElement).toBe(drawer);
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

  it('moves focus without asking the browser to scroll the page', () => {
    const opener = document.body.appendChild(document.createElement('button'));
    opener.focus();
    const focus = vi.spyOn(HTMLElement.prototype, 'focus');
    const view = open();
    expect(focus).toHaveBeenCalledWith({ preventScroll: true });
    focus.mockClear();
    view.rerender(<Drawer open={false} title="t" onClose={vi.fn()}><p>body</p></Drawer>);
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
