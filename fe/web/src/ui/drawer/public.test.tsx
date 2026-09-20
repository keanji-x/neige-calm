// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { useEffect, useRef, type ReactNode } from 'react';
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
      mobileBackLabel={props.mobileBackLabel}
      closeLabel={props.closeLabel}
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

  it('stays mounted through the retraction after open goes false', () => {
    const { rerender } = open();
    rerender(<Drawer open={false} title="t" onClose={vi.fn()}><p>body</p></Drawer>);
    expect(screen.getByRole('complementary')).toBeTruthy();
  });

  /* The caller drops its selection the instant it asks for a close; a drawer that re-read its props on the way out would slide away blank. */
  it('keeps the last content it had while retracting', () => {
    const { rerender } = open({ title: 'Why the resolver drops a hop', children: <p>the transcript</p> });
    rerender(<Drawer open={false} title="" onClose={vi.fn()}>{null}</Drawer>);
    expect(screen.getByText('the transcript')).toBeTruthy();
    /* The last frame's title is held on the container's accessible name, and `getByRole` only matches the retained name. */
    expect(screen.getByRole('complementary', { name: 'Why the resolver drops a hop' })).toBeTruthy();
    expect(screen.queryByRole('heading')).toBeNull();
  });

  /* The page header's delete X sits 58px above this control in the same column; an X on both would mean "put away" and "destroy" a flick apart. */
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

  it('lets the caller name what the close control closes', () => {
    const onClose = vi.fn();
    open({ onClose, closeLabel: '关闭来源' });
    expect(screen.queryByRole('button', { name: 'Close conversation' })).toBeNull();
    screen.getByRole('button', { name: '关闭来源' }).click();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('uses the standard compact Header with Back on the left', () => {
    const onClose = vi.fn();
    vi.stubGlobal('matchMedia', vi.fn(() => ({
      matches: true,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })));
    try {
      open({ title: 'Untitled', mobileBackLabel: 'Report', onClose });
      expect(screen.getByRole('heading', { name: 'Untitled' })).toBeTruthy();
      const back = screen.getByRole('button', { name: 'Back to Report' });
      expect(screen.queryByRole('button', { name: 'Close conversation' })).toBeNull();
      back.click();
      expect(onClose).toHaveBeenCalledOnce();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('puts the footer outside the scrolling body', () => {
    open({ footer: <form aria-label="composer" /> });
    const bodyInner = screen.getByText('the transcript').parentElement;
    const scroll = bodyInner?.parentElement;
    const drawer = screen.getByRole('complementary');
    /* The controls float over the scroller as a sibling, so the body is its only child. */
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

  /* `focus()` lands in an `aria-hidden` subtree and reports success, so the outcome cannot be read back. Reduced motion removes the retraction wait so only the fallback decides. */
  function withReducedMotion(run: () => void) {
    vi.stubGlobal('matchMedia', vi.fn(() => ({ matches: true })));
    try { run(); } finally { vi.unstubAllGlobals(); }
  }

  function hiddenOpenerPage(attribute: string, value: string) {
    const shroud = document.body.appendChild(document.createElement('div'));
    shroud.setAttribute(attribute, value);
    const opener = shroud.appendChild(document.createElement('button'));
    const pageTitle = document.body.appendChild(document.createElement('h1'));
    pageTitle.tabIndex = -1; pageTitle.dataset.ncPageTitle = '';
    opener.focus();
    return { shroud, opener, pageTitle };
  }

  it('does not count a landing inside an aria-hidden subtree as a restore', () => {
    withReducedMotion(() => {
      const { shroud, opener, pageTitle } = hiddenOpenerPage('aria-hidden', 'true');
      /* The ask itself succeeds, so an outcome-only test would call this a restore. */
      expect(document.activeElement).toBe(opener);
      const view = open();
      view.rerender(<Drawer open={false} title="t" onClose={vi.fn()}><p>body</p></Drawer>);
      expect(document.activeElement).not.toBe(opener);
      expect(document.activeElement).toBe(pageTitle);
      shroud.remove(); pageTitle.remove();
    });
  });

  /* `inert` for the same reason, through the same door. */
  it('does not count a landing inside an inert subtree as a restore', () => {
    withReducedMotion(() => {
      const { shroud, opener, pageTitle } = hiddenOpenerPage('inert', '');
      const view = open();
      view.rerender(<Drawer open={false} title="t" onClose={vi.fn()}><p>body</p></Drawer>);
      expect(document.activeElement).not.toBe(opener);
      expect(document.activeElement).toBe(pageTitle);
      shroud.remove(); pageTitle.remove();
    });
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

  /* The open effect bows out when focus is already inside the panel; that line also decides who the opener was. */
  function drawerAt(isOpen: boolean, children: ReactNode) {
    return <Drawer open={isOpen} title="Why the resolver drops a hop" onClose={vi.fn()}>{children}</Drawer>;
  }

  /** Content that takes the caret as it mounts, the shape `focusOnMount` gives the composer. */
  function SelfFocusing() {
    const ref = useRef<HTMLInputElement>(null);
    useEffect(() => { ref.current?.focus(); }, []);
    return <input ref={ref} aria-label="Message" />;
  }

  /* A guarded open records no opener, so the close falls back to the page title. */
  it('falls back to the page title after an open its content had already claimed', () => {
    const opener = document.body.appendChild(document.createElement('button'));
    const pageTitle = document.body.appendChild(document.createElement('h1'));
    pageTitle.tabIndex = -1; pageTitle.dataset.ncPageTitle = '';
    opener.focus();

    const view = render(drawerAt(true, <SelfFocusing />));
    expect(document.activeElement).toBe(screen.getByLabelText('Message'));

    view.rerender(drawerAt(false, <SelfFocusing />));
    expect(document.activeElement).toBe(pageTitle);
    expect(document.activeElement).not.toBe(opener);
    opener.remove(); pageTitle.remove();
  });

  /* A reopen mid-retraction reruns the effect with focus still inside the panel; without the guard the panel becomes its own restore target. */
  it('does not record itself as its own opener when it is reopened mid-retraction', () => {
    const column = document.body.appendChild(document.createElement('div'));
    const opener = column.appendChild(document.createElement('button'));
    const pageTitle = document.body.appendChild(document.createElement('h1'));
    pageTitle.tabIndex = -1; pageTitle.dataset.ncPageTitle = '';
    opener.focus();

    const view = render(drawerAt(true, <p>the transcript</p>));
    const panel = screen.getByRole('complementary');
    expect(document.activeElement).toBe(panel);

    // The opener's column hidden the way `app/shell` hides it: the restore declines and waits.
    column.setAttribute('aria-hidden', 'true');
    view.rerender(drawerAt(false, <p>the transcript</p>));
    expect(document.activeElement).toBe(panel);

    // Reopened before it finished retracting, with the caret still in here.
    view.rerender(drawerAt(true, <p>the transcript</p>));
    column.removeAttribute('aria-hidden');

    view.rerender(drawerAt(false, <p>the transcript</p>));
    expect(document.activeElement).toBe(opener);
    column.remove(); pageTitle.remove();
  });
});
