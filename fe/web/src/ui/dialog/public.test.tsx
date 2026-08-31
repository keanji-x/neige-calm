import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useEffect, useRef } from 'react';
import { Dialog, useDialogView, type DialogViewController } from './public.tsx';

beforeEach(() => {
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

/** A dialog whose named focus target is disabled, i.e. cannot receive focus. */
function DisabledTargetDialog() {
  const target = useRef<HTMLButtonElement | null>(null);
  return (
    <Dialog open title="Test" onClose={vi.fn()} initialFocusRef={target}>
      <button type="button" ref={target} disabled>Disabled target</button>
      <input aria-label="Task" />
    </Dialog>
  );
}

/**
 * The named target is only disabled by its ancestor `<fieldset>`, so it carries
 * no `disabled` attribute of its own and `focusables` accepts it. `hideClose`
 * removes the one control that would otherwise rescue the focus.
 */
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

  /*
   * #1161. The rest of this file stubs `requestAnimationFrame` to run its
   * callback *synchronously*, which collapses the window this pair is about:
   * in a browser the open-focus effect lands a frame later, and a reader who
   * clicks into a field before it does had focus taken away from them. The
   * frame is held here instead of run, so the ordering is chosen rather than
   * raced.
   */
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

    // Positive control for the guard below: without this the guard could be
    // "never focus anything" and the pair would still pass.
    frames.forEach((frame) => { frame(0); });
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close' }));
  });

  it('does not take focus away from a field the reader already clicked into', () => {
    const frames = heldFrames();
    render(<Dialog open title="Test" onClose={vi.fn()}><input aria-label="Task" /></Dialog>);
    const field = screen.getByLabelText('Task');
    field.focus();

    frames.forEach((frame) => { frame(0); });

    /*
     * The concrete harm when this fails: `focusables(panel)[0]` is the header's
     * Close button, so every keystroke goes to a button, and the first space
     * activates it and throws the half-typed dialog away. That is #1161's
     * flake, and it is what a reader typing quickly gets in a real browser.
     */
    expect(document.activeElement).toBe(field);
  });

  /*
   * The panel itself is focusable (`tabIndex={-1}`), so a mousedown on chrome —
   * the title, the padding — makes it the active element inside the opening
   * frame. Yielding to that would leave the reader with no field focused and
   * nothing to type into, which is the original complaint in a quieter form.
   */
  it('still honours the named target when focus landed on the panel chrome', () => {
    const frames = heldFrames();
    render(<Dialog open title="Test" onClose={vi.fn()}><input aria-label="Task" /></Dialog>);
    screen.getByRole('dialog').focus();
    expect(document.activeElement).toBe(screen.getByRole('dialog'));

    frames.forEach((frame) => { frame(0); });

    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Close' }));
  });

  /*
   * A base-view element stays mounted under `display: none` once a child view
   * is pushed, so `panel.contains(activeElement)` keeps saying yes about
   * something nobody can see or reach. Reproduced here by hiding the container
   * after focusing it, which is the same DOM state the pushed view produces.
   */
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

  /*
   * A named target that cannot take focus. `.focus()` on a disabled element is
   * a silent no-op and the background is `inert` by then, so focus stayed
   * outside the dialog entirely — a modal with focus on `body`. Predates #1161;
   * fixed alongside it because it is the same failure family.
   */
  it('falls back into the panel when the named target cannot take focus', () => {
    const outside = document.body.appendChild(document.createElement('button'));
    outside.textContent = 'Opener';
    outside.focus();
    render(<DisabledTargetDialog />);

    const panel = screen.getByRole('dialog');
    expect(panel.contains(document.activeElement)).toBe(true);
    expect(document.activeElement).not.toBe(outside);
    outside.remove();
  });

  it('keeps focus inside the panel when the target is disabled by an ancestor', () => {
    const outside = document.body.appendChild(document.createElement('button'));
    outside.focus();
    render(<InheritedDisabledDialog />);

    expect(screen.getByRole('dialog').contains(document.activeElement)).toBe(true);
    outside.remove();
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
    act(() => { controller!.pushView({ title: 'Child', body: <p>No controls</p> }); });
    const close = screen.getByRole('button', { name: 'Close' });
    close.focus();
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
