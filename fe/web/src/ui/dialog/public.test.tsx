import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPortal } from 'react-dom';
import { useEffect, useRef } from 'react';
import { Dialog, useDialogView, type DialogViewController } from './public.tsx';

beforeEach(() => {
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

function DisabledTargetDialog() {
  const target = useRef<HTMLButtonElement | null>(null);
  return (
    <Dialog open title="Test" onClose={vi.fn()} initialFocusRef={target}>
      <button type="button" ref={target} disabled>Disabled target</button>
      <input aria-label="Task" />
    </Dialog>
  );
}

/** Disabled only via the ancestor `<fieldset>`, so `focusables` accepts it; `hideClose` removes the control that would otherwise rescue focus. */
function InheritedDisabledDialog() {
  const target = useRef<HTMLInputElement | null>(null);
  return (
    <Dialog open title="Test" hideClose onClose={vi.fn()} initialFocusRef={target}>
      <fieldset disabled><input aria-label="Task" ref={target} /></fieldset>
    </Dialog>
  );
}

function Capture({ onController }: { onController: (value: DialogViewController) => void }) {
  const controller = useDialogView();
  useEffect(() => { if (controller) onController(controller); }, [controller, onController]);
  return <button type="button">Original child</button>;
}

describe('Dialog behavior', () => {
  it('traps Tab from a portal owned outside the dialog React tree', () => {
    const host = document.createElement('div');
    render(<>
      <Dialog open title="Settings" onClose={vi.fn()}>
        <div ref={(slot) => { slot?.appendChild(host); }} />
      </Dialog>
      {createPortal(<button type="button">Portal last</button>, host)}
    </>);
    const last = screen.getByRole('button', { name: 'Portal last' });
    const first = screen.getByRole('button', { name: 'Close' });
    last.focus();
    fireEvent.keyDown(last, { key: 'Tab' });
    expect(document.activeElement).toBe(first);
    fireEvent.keyDown(first, { key: 'Tab', shiftKey: true });
    expect(document.activeElement).toBe(last);
  });

  it('renders the close control with the shared stroked icon instead of a text glyph', () => {
    render(<Dialog open title="Test" onClose={vi.fn()} />);
    const close = screen.getByRole('button', { name: 'Close' });
    expect(close.querySelector('svg')?.querySelector('path')?.getAttribute('d')).toBe('M4 4l8 8');
    expect(close.textContent).not.toContain('×');
  });

  it('restores exact inert state and skips a detached restore target', () => {
    const background = document.body.appendChild(document.createElement('main'));
    background.setAttribute('inert', '');
    const trigger = document.body.appendChild(document.createElement('button'));
    trigger.focus();
    const focus = vi.spyOn(trigger, 'focus');
    const result = render(<Dialog open title="Test" onClose={vi.fn()}/>);
    trigger.remove();
    expect(() => result.unmount()).not.toThrow();
    expect(background.hasAttribute('inert')).toBe(true);
    expect(focus).not.toHaveBeenCalled();
    background.remove();
  });

  /* The rest of this file runs `requestAnimationFrame` synchronously; holding the frame makes the open-focus ordering chosen rather than raced. */
  const heldFrames = (): FrameRequestCallback[] => {
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
      frames.push(callback);
      return frames.length;
    });
    return frames;
  };

  it('focuses the first focusable when the frame lands and focus is still outside', () => {
    const frames = heldFrames();
    render(<Dialog open title="Test" onClose={vi.fn()}><input aria-label="Task" /></Dialog>);

    // Positive control for the guard below.
    frames.forEach((frame) => { frame(0); });
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close' }));
  });

  it('does not take focus away from a field the reader already clicked into', () => {
    const frames = heldFrames();
    render(<Dialog open title="Test" onClose={vi.fn()}><input aria-label="Task" /></Dialog>);
    const field = screen.getByLabelText('Task');
    field.focus();

    frames.forEach((frame) => { frame(0); });

    expect(document.activeElement).toBe(field);
  });

  /* The panel is focusable (`tabIndex={-1}`), so a mousedown on chrome makes it the active element inside the opening frame. */
  it('does not treat the panel itself as a place the reader chose', () => {
    const frames = heldFrames();
    render(<Dialog open title="Test" onClose={vi.fn()}><input aria-label="Task" /></Dialog>);
    screen.getByRole('dialog').focus();
    expect(document.activeElement).toBe(screen.getByRole('dialog'));

    frames.forEach((frame) => { frame(0); });

    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close' }));
  });

  /* A base view stays mounted under `display: none` once a child view is pushed, so `panel.contains(activeElement)` keeps saying yes. */
  it('does not yield to focus stranded on content that has been hidden', () => {
    const frames = heldFrames();
    render(
      <Dialog open title="Test" onClose={vi.fn()}>
        <div data-testid="region"><button type="button">Stranded</button></div>
      </Dialog>,
    );
    const stranded = screen.getByRole('button', { name: 'Stranded' });
    stranded.focus();
    screen.getByTestId('region').style.display = 'none';

    frames.forEach((frame) => { frame(0); });

    expect(document.activeElement).not.toBe(stranded);
    expect(screen.getByRole('dialog').contains(document.activeElement)).toBe(true);
  });

  /* `.focus()` on a disabled element is a silent no-op and the background is `inert` by then. */
  it('falls back into the panel when the named target cannot take focus', () => {
    const outside = document.body.appendChild(document.createElement('button'));
    outside.textContent = 'Opener';
    outside.focus();
    render(<DisabledTargetDialog />);

    /* Containment alone is satisfied by the verify-after-focus fallback; the specific landing place discriminates the membership check. */
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close' }));
    outside.remove();
  });

  it('keeps focus inside the panel when the target is disabled by an ancestor', () => {
    const outside = document.body.appendChild(document.createElement('button'));
    outside.focus();
    render(<InheritedDisabledDialog />);

    expect(screen.getByRole('dialog').contains(document.activeElement)).toBe(true);
    outside.remove();
  });

  /* An SVG anchor matches `a[href]` but is an `SVGElement`, not an `HTMLElement`. */
  it('yields to a focused SVG anchor, which is an Element but not an HTMLElement', () => {
    const frames = heldFrames();
    render(
      <Dialog open title="Test" onClose={vi.fn()}>
        <svg><a href="#test" tabIndex={0} aria-label="Chart link"><text>Link</text></a></svg>
        <input aria-label="Task" />
      </Dialog>,
    );
    const anchor = screen.getByLabelText('Chart link');
    anchor.focus();
    expect(document.activeElement).toBe(anchor);

    frames.forEach((frame) => { frame(0); });

    expect(document.activeElement).toBe(anchor);
  });

  it('re-queries focusables after dynamically inserting an item', () => {
    render(<Dialog open title="Test" onClose={vi.fn()}><button>First</button></Dialog>);
    const panel = screen.getByRole('dialog');
    const dynamic = panel.appendChild(document.createElement('button'));
    dynamic.textContent = 'Dynamic'; dynamic.focus();
    fireEvent.keyDown(dynamic, { key: 'Tab' });
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close' }));
  });

  it('keeps original children mounted while a child view is shown', () => {
    const unmounted = vi.fn(); let controller: DialogViewController | null = null;
    function StatefulChild() { useEffect(() => unmounted, []); return <Capture onController={(value) => { controller = value; }}/>; }
    render(<Dialog open title="Parent" onClose={vi.fn()}><StatefulChild/></Dialog>);
    act(() => { controller!.pushView({ title: 'Child', body: <p>Child body</p> }); });
    expect(screen.getByRole('dialog', { name: 'Child' })).toBeTruthy();
    expect(screen.getByText('Original child')).toBeTruthy();
    expect(unmounted).not.toHaveBeenCalled();
  });

  it('hands focus into a child view and restores the control that opened it', () => {
    let controller: DialogViewController | null = null;
    render(
      <Dialog open title="Parent" onClose={vi.fn()}>
        <Capture onController={(value) => { controller = value; }} />
      </Dialog>,
    );
    const opener = screen.getByRole('button', { name: 'Original child' });
    opener.focus();
    let dispose!: () => void;
    act(() => {
      dispose = controller!.pushView({
        title: 'Child',
        body: <><input aria-label="Child path" /><button type="button">Cancel child</button></>,
      });
    });
    expect(document.activeElement).toBe(screen.getByRole('textbox', { name: 'Child path' }));

    act(dispose);
    expect(screen.getByRole('dialog', { name: 'Parent' })).toBeTruthy();
    expect(document.activeElement).toBe(opener);
  });

  it('preserves every opener across a nested child-view focus stack', () => {
    let controller: DialogViewController | null = null;
    render(
      <Dialog open title="Parent" onClose={vi.fn()}>
        <Capture onController={(value) => { controller = value; }} />
      </Dialog>,
    );
    const baseOpener = screen.getByRole('button', { name: 'Original child' });
    baseOpener.focus();
    let disposeFirst!: () => void;
    act(() => {
      disposeFirst = controller!.pushView({
        title: 'First child', body: <button type="button">Open second child</button>,
      });
    });
    const firstOpener = screen.getByRole('button', { name: 'Open second child' });
    expect(document.activeElement).toBe(firstOpener);

    let disposeSecond!: () => void;
    act(() => {
      disposeSecond = controller!.pushView({
        title: 'Second child', body: <input aria-label="Second child field" />,
      });
    });
    expect(document.activeElement).toBe(screen.getByRole('textbox', { name: 'Second child field' }));

    act(disposeSecond);
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Open second child' }));
    act(disposeFirst);
    expect(document.activeElement).toBe(baseOpener);
  });

  it('falls back inside the visible view when a popped child opener disappeared', () => {
    let controller: DialogViewController | null = null;
    render(
      <Dialog open title="Parent" onClose={vi.fn()}>
        <Capture onController={(value) => { controller = value; }} />
      </Dialog>,
    );
    screen.getByRole('button', { name: 'Original child' }).focus();
    let disposeFirst!: () => void;
    act(() => {
      disposeFirst = controller!.pushView({
        title: 'First child', body: <button type="button">Open second child</button>,
      });
    });
    let disposeSecond!: () => void;
    act(() => {
      disposeSecond = controller!.pushView({
        title: 'Second child', body: <input aria-label="Second child field" />,
      });
    });
    // Child 2's recorded opener is detached, so focus must still land inside the visible parent.
    act(disposeFirst);
    act(disposeSecond);
    const dialog = screen.getByRole('dialog', { name: 'Parent' });
    expect(dialog.contains(document.activeElement)).toBe(true);
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close' }));
  });

  it('falls back when a recorded opener becomes fieldset-disabled while the child is open', () => {
    let controller: DialogViewController | null = null;
    render(
      <Dialog open title="Parent" onClose={vi.fn()}>
        <fieldset aria-label="Base controls">
          <button type="button">Open child</button>
        </fieldset>
        <Capture onController={(value) => { controller = value; }} />
      </Dialog>,
    );
    const opener = screen.getByRole('button', { name: 'Open child' });
    opener.focus();
    let dispose!: () => void;
    act(() => {
      dispose = controller!.pushView({ title: 'Child', body: <button type="button">Child action</button> });
    });
    const fieldset = document.querySelector<HTMLFieldSetElement>('fieldset[aria-label="Base controls"]')!;
    fieldset.disabled = true;
    act(dispose);

    const dialog = screen.getByRole('dialog', { name: 'Parent' });
    expect(dialog.contains(document.activeElement)).toBe(true);
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close' }));
  });

  it('uses a disposable LIFO child-view stack', () => {
    let controller: DialogViewController | null = null;
    render(<Dialog open title="Parent" onClose={vi.fn()}><Capture onController={(value) => { controller = value; }}/></Dialog>);
    let disposeFirst!: () => void; let disposeSecond!: () => void;
    act(() => { disposeFirst = controller!.pushView({ title: 'First child', body: 'First body' }); });
    act(() => { disposeSecond = controller!.pushView({ title: 'Second child', body: 'Second body' }); });
    expect(screen.getByRole('dialog', { name: 'Second child' })).toBeTruthy();
    act(disposeSecond);
    expect(screen.getByRole('dialog', { name: 'First child' })).toBeTruthy();
    act(disposeFirst);
    expect(screen.getByRole('dialog', { name: 'Parent' })).toBeTruthy();
  });

  it('allows an earlier child-view owner to dispose without popping the latest view', () => {
    let controller: DialogViewController | null = null;
    render(<Dialog open title="Parent" onClose={vi.fn()}><Capture onController={(value) => { controller = value; }}/></Dialog>);
    let disposeFirst!: () => void; let disposeSecond!: () => void;
    act(() => { disposeFirst = controller!.pushView({ title: 'First child', body: 'First body' }); });
    act(() => { disposeSecond = controller!.pushView({ title: 'Second child', body: 'Second body' }); });
    act(disposeFirst);
    expect(screen.getByRole('dialog', { name: 'Second child' })).toBeTruthy();
    expect(screen.getByText('Second body')).toBeTruthy();
    act(disposeSecond);
    expect(screen.getByRole('dialog', { name: 'Parent' })).toBeTruthy();
  });

  it('traps Tab on the close button when a child view has no focusable controls', () => {
    let controller: DialogViewController | null = null;
    render(<Dialog open title="Parent" onClose={vi.fn()}><Capture onController={(value) => { controller = value; }}/></Dialog>);
    screen.getByRole('button', { name: 'Original child' }).focus();
    act(() => { controller!.pushView({ title: 'Child', body: <p>No controls</p> }); });
    const close = screen.getByRole('button', { name: 'Close' });
    expect(document.activeElement).toBe(close);
    const event = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    expect(close.dispatchEvent(event)).toBe(false);
    expect(document.activeElement).toBe(close);
  });

  it('uses a child view JSX title as the dialog accessible name', () => {
    let controller: DialogViewController | null = null;
    render(<Dialog open title="Parent" onClose={vi.fn()}><Capture onController={(value) => { controller = value; }}/></Dialog>);
    act(() => { controller!.pushView({ title: <><strong>Choose</strong> directory</>, body: 'Body' }); });
    expect(screen.getByRole('dialog', { name: 'Choose directory' })).toBeTruthy();
  });

  it('closes the current child view before closing the parent dialog', () => {
    const onClose = vi.fn(); let controller: DialogViewController | null = null;
    render(<Dialog open title="Parent" onClose={onClose}><Capture onController={(value) => { controller = value; }}/></Dialog>);
    act(() => { controller!.pushView({ title: 'Child', body: 'Child body' }); });
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(screen.getByRole('dialog', { name: 'Parent' })).toBeTruthy();
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('ignores an already-handled bubbling Escape', () => {
    const onClose = vi.fn();
    render(<Dialog open title="Parent" onClose={onClose}><button onKeyDown={(event) => event.preventDefault()}>Nested control</button></Dialog>);
    fireEvent.keyDown(screen.getByRole('button', { name: 'Nested control' }), { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
  });

  it('restores focus to the surviving inline opener before the page-title fallback', () => {
    const opener = document.body.appendChild(document.createElement('button'));
    const pageTitle = document.body.appendChild(document.createElement('h1'));
    pageTitle.tabIndex = -1; pageTitle.dataset.ncPageTitle = '';
    opener.focus();
    const result = render(<Dialog open title="Parent" onClose={vi.fn()} />);
    result.unmount();
    expect(document.activeElement).toBe(opener);
    opener.remove(); pageTitle.remove();
  });

  it('falls back to the page title when the opener has been removed', () => {
    const opener = document.body.appendChild(document.createElement('button'));
    const pageTitle = document.body.appendChild(document.createElement('h1'));
    pageTitle.tabIndex = -1; pageTitle.dataset.ncPageTitle = '';
    opener.focus();
    const result = render(<Dialog open title="Parent" onClose={vi.fn()} />);
    opener.remove();
    result.unmount();
    expect(document.activeElement).toBe(pageTitle);
    pageTitle.remove();
  });
});
