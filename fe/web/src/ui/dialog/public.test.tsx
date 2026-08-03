import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useEffect } from 'react';
import { Dialog, useDialogView, type DialogViewController } from './public.tsx';

beforeEach(() => {
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

function Capture({ onController }: { onController: (value: DialogViewController) => void }) {
  const controller = useDialogView();
  useEffect(() => { if (controller) onController(controller); }, [controller, onController]);
  return <button type="button">Original child</button>;
}

describe('Dialog behavior', () => {
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
});
