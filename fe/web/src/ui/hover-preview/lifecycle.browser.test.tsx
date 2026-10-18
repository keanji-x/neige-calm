import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { commands, page, userEvent } from 'vitest/browser';
import '../../styles/entry.css';
import { StrictMode } from 'react';
import { HoverPreview } from './public.tsx';

beforeEach(async () => { await page.viewport(1800, 1000); await commands.emulateReducedMotion(true); vi.useFakeTimers(); });
afterEach(async () => { cleanup(); vi.useRealTimers(); await commands.emulateReducedMotion(false); });
function mount() {
  return render(<HoverPreview title="Notes" trigger={(activate) => <button onClick={activate}>Notes</button>}><p>Preview body</p></HoverPreview>);
}
function advance(ms: number) { act(() => { vi.advanceTimersByTime(ms); }); }
function hover() { fireEvent.pointerEnter(screen.getByRole('button', { name: 'Notes' }).parentElement!); }

describe('HoverPreview lifecycle', () => {
  it('dismisses nested previews together for navigation while preserving ordinary local Escape', () => {
    const navigate = vi.fn();
    const focusedAtNavigation: Element[] = [];
    render(<HoverPreview title="Parent" trigger={activate => <button onClick={activate}>Parent</button>}>
      <HoverPreview title="Child" trigger={activate => <button onClick={activate}>Child</button>}>
        {dismiss => <button onClick={() => { dismiss(); focusedAtNavigation.push(document.activeElement!); navigate(); }}>Open destination</button>}
      </HoverPreview>
    </HoverPreview>);
    fireEvent.click(screen.getByRole('button', { name: 'Parent' }));
    fireEvent.click(screen.getByRole('button', { name: 'Child' }));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.getByRole('dialog', { name: 'Preview: Parent' })).toBeTruthy();
    expect(screen.queryByRole('dialog', { name: 'Preview: Child' })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Child' }));
    screen.getByRole('button', { name: 'Open destination' }).focus();
    fireEvent.click(screen.getByRole('button', { name: 'Open destination' }));
    advance(2000);
    expect(screen.queryAllByRole('dialog')).toHaveLength(0);
    expect(navigate).toHaveBeenCalledOnce();
    expect(focusedAtNavigation).toEqual([screen.getByRole('button', { name: 'Parent' })]);
  });

  it('delays preview, opens an interactive card without redundant controls, then dismisses on leave', () => {
    mount(); hover(); advance(299);
    expect(screen.queryByRole('dialog')).toBeNull();
    advance(1);
    const dialog = screen.getByRole('dialog');
    expect(dialog.querySelector('button, svg')).toBeNull();
    expect(dialog.textContent).toContain('Preview body');
    advance(2000);
    expect(screen.getByRole('dialog')).toBe(dialog);
    fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!);
    advance(180);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it.each([{ isComposing: true }, { keyCode: 229 }])('keeps waiting activation and cancellation with IME flags inert: %j', flags => {
    mount(); hover(); advance(100);
    const trigger = screen.getByRole('button', { name: 'Notes' });
    fireEvent.keyDown(trigger, { key: 'Escape', ...flags });
    advance(200);
    expect(screen.getByRole('dialog')).toBeTruthy();
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.keyDown(trigger, { key: 'ArrowDown', ...flags });
    expect(screen.queryByRole('dialog')).toBeNull();
    fireEvent.keyDown(trigger, { key: 'ArrowDown' });
    expect(screen.getByRole('dialog')).toBeTruthy();
  });
  it('consumes waiting Escape and cancels the pending preview', () => {
    mount(); hover(); advance(100);
    const event = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
    act(() => { screen.getByRole('button', { name: 'Notes' }).dispatchEvent(event); });
    expect(event.defaultPrevented).toBe(true);
    advance(1000);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it('cancels an accidental hover before opening', () => {
    mount(); hover(); advance(100);
    fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!);
    advance(2000);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it('allows pointer transfer into the card before the leave grace ends', () => {
    mount(); hover(); advance(300);
    fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!);
    advance(100);
    fireEvent.pointerEnter(screen.getByRole('dialog'));
    advance(1000);
    expect(screen.getByRole('dialog')).toBeTruthy();
  });
  it('keeps an open card while entering it, then closes when leaving the card', () => {
    mount(); hover(); advance(1300);
    fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!);
    advance(100);
    fireEvent.pointerEnter(screen.getByRole('dialog'));
    advance(2000);
    expect(screen.getByRole('dialog')).toBeTruthy();
    fireEvent.pointerLeave(screen.getByRole('dialog'));
    advance(180);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it('enables interaction by keyboard, restores focus on close, and does not reopen', () => {
    mount();
    const trigger = screen.getByRole('button', { name: 'Notes' });
    trigger.focus();
    fireEvent.keyDown(trigger, { key: 'ArrowDown' });
    expect(screen.getByRole('dialog')).toBeTruthy();
    fireEvent.keyDown(document, { key: 'Escape' });
    advance(2000);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });
  it('Escape closes only the topmost card', () => {
    mount();
    fireEvent.click(screen.getByRole('button', { name: 'Notes' }));
    render(<HoverPreview title="Second" trigger={(activate) => <button onClick={activate}>Second</button>}>Second body</HoverPreview>);
    fireEvent.click(screen.getByRole('button', { name: 'Second' }));
    fireEvent.keyDown(screen.getByRole('button', { name: 'Notes' }), { key: 'Escape' });
    expect(screen.queryByRole('dialog', { name: 'Preview: Second' })).toBeNull();
    expect(screen.getByRole('dialog', { name: 'Preview: Notes' })).toBeTruthy();
  });
  it('keeps nested preview scrolling open and dismisses only when its anchor scrolls', () => {
    render(<HoverPreview title="Parent" trigger={(activate) => <button onClick={activate}>Parent</button>}>
      <HoverPreview title="Child" trigger={(activate) => <button onClick={activate}>Child</button>}>
        <div data-testid="child-scroll">Long child contents</div>
      </HoverPreview>
    </HoverPreview>);
    fireEvent.click(screen.getByRole('button', { name: 'Parent' }));
    fireEvent.click(screen.getByRole('button', { name: 'Child' }));
    fireEvent.scroll(screen.getByTestId('child-scroll'));
    expect(screen.getAllByRole('dialog')).toHaveLength(2);
    const parent = screen.getByRole('dialog', { name: 'Preview: Parent' });
    fireEvent.scroll(parent.children[1]);
    expect(screen.queryByRole('dialog', { name: 'Preview: Child' })).toBeNull();
    expect(screen.getByRole('dialog', { name: 'Preview: Parent' })).toBeTruthy();
    fireEvent.scroll(document);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it('dismisses on outside press without relying on mouse leave or focus blur', () => {
    mount();
    fireEvent.click(screen.getByRole('button', { name: 'Notes' }));
    const outside = document.createElement('button');
    document.body.append(outside);
    fireEvent.pointerDown(outside, { pointerType: 'touch' });
    expect(screen.queryByRole('dialog')).toBeNull();
    outside.remove();
  });
  it('does not dismiss ancestors when pressing inside a nested preview', () => {
    render(<HoverPreview title="Parent" trigger={(activate) => <button onClick={activate}>Parent</button>}>
      <HoverPreview title="Child" trigger={(activate) => <button onClick={activate}>Child</button>}>
        <p>Child content</p>
      </HoverPreview>
    </HoverPreview>);
    fireEvent.click(screen.getByRole('button', { name: 'Parent' }));
    fireEvent.click(screen.getByRole('button', { name: 'Child' }));
    fireEvent.pointerDown(screen.getByText('Child content'));
    expect(screen.getAllByRole('dialog')).toHaveLength(2);
  });
  it('unmount disposes all timers and its portal', () => {
    const view = mount(); hover(); advance(300);
    view.unmount(); advance(3000);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(vi.getTimerCount()).toBe(0);
  });
});

it('revokes all nested semantic ownership before paint disposal and clears pending descendant timers', () => {
  render(<section data-testid="ancestor"><HoverPreview title="Parent" trigger={activate => <button onClick={activate}>Parent</button>}>
    <HoverPreview title="Child" trigger={activate => <button onClick={activate}>Child</button>}>
      <HoverPreview title="Grandchild" trigger={activate => <button onClick={activate}>Grandchild</button>}>Content</HoverPreview>
    </HoverPreview>
  </HoverPreview></section>);
  fireEvent.click(screen.getByRole('button', { name: 'Parent' }));
  fireEvent.click(screen.getByRole('button', { name: 'Child' }));
  fireEvent.click(screen.getByRole('button', { name: 'Grandchild' }));
  const cards = screen.getAllByRole('dialog');
  fireEvent.scroll(screen.getByTestId('ancestor'));
  for (const card of cards) {
    // Reduced motion releases admitted paint synchronously on close.
    expect(card.isConnected).toBe(false);
  }
  advance(2000);
  fireEvent.keyDown(screen.getByRole('button', { name: 'Parent' }), { key: 'ArrowDown' });
  expect(screen.getAllByRole('dialog')).toHaveLength(1);
  expect(screen.getByRole('dialog', { name: 'Preview: Parent' })).not.toBe(cards[0]);
  fireEvent.pointerEnter(screen.getByRole('button', { name: 'Child' }).parentElement!);
  advance(100); fireEvent.scroll(screen.getByTestId('ancestor')); advance(1000);
  fireEvent.keyDown(screen.getByRole('button', { name: 'Parent' }), { key: 'ArrowDown' });
  expect(screen.getAllByRole('dialog')).toHaveLength(1);
  expect(screen.getByRole('dialog', { name: 'Preview: Parent' })).not.toBe(cards[0]);
});


for (const reduced of [false, true]) {
  it.each(['inert', 'hidden', 'dialog close', 'ancestor detach', 'ancestor replacement', 'ancestor relocation', 'trigger replacement'] as const)(
    `revokes native host scope and descendants without restoring old intent: %s (reduced=${reduced})`, async change => {
      vi.useRealTimers();
      await commands.emulateReducedMotion(reduced);
      const action = vi.fn();
      const view = render(<StrictMode><dialog open><section style={{ padding: 80 }}>
        <HoverPreview title="Scope parent" trigger={activate => <button onClick={activate}>Scope parent</button>}>
          <HoverPreview title="Scope child" trigger={activate => <button onClick={activate}>Scope child</button>}>
            <button onClick={action}>Scope action</button>
          </HoverPreview>
        </HoverPreview>
      </section></dialog></StrictMode>);
      const root = page.getByRole('button', { name: 'Scope parent', exact: true });
      await root.click();
      await page.getByRole('button', { name: 'Scope child', exact: true }).click();
      const actionButton = page.getByRole('button', { name: 'Scope action' });
      await expect.element(actionButton).toBeVisible();
      (actionButton.element() as HTMLElement).focus();
      const cards = Array.from(document.querySelectorAll<HTMLElement>('[data-nc-link-preview]'));
      const trigger = root.element(); const host = trigger.closest('section')!; const dialog = host.closest('dialog')!;
      const parent = host.parentElement!; const next = host.nextSibling;
      const replacement = trigger.cloneNode(true) as HTMLElement;
      const hostReplacement = host.cloneNode(true) as HTMLElement;
      const destination = document.createElement('section');
      const restore = () => {
        host.inert = false; host.hidden = false;
        if (host.parentElement !== parent) parent.insertBefore(host, next?.parentNode === parent ? next : null);
        hostReplacement.remove(); destination.remove();
        if (!trigger.isConnected) replacement.replaceWith(trigger);
        if (!dialog.open) dialog.show();
      };
      try {
        if (change === 'inert') host.inert = true;
        if (change === 'hidden') host.hidden = true;
        if (change === 'dialog close') dialog.close();
        if (change === 'ancestor detach') host.remove();
        if (change === 'ancestor replacement') host.replaceWith(hostReplacement);
        if (change === 'ancestor relocation') { parent.append(destination); destination.append(host); }
        if (change === 'trigger replacement') trigger.replaceWith(replacement);
        await Promise.resolve();
        for (const card of cards) {
          expect(card.inert).toBe(true);
          expect(card.hasAttribute('data-nc-link-preview')).toBe(false);
          expect(card.hasAttribute('data-nc-escape-layer')).toBe(false);
          expect(card.contains(document.activeElement)).toBe(false);
          const box = card.getBoundingClientRect();
          expect(card.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2))).toBe(false);
        }
        restore();
        await Promise.resolve();
        await new Promise(resolve => setTimeout(resolve, 400));
        expect(document.querySelector('[data-nc-link-preview]')).toBeNull();
        expect(action).not.toHaveBeenCalled();
        await root.click();
        await expect.element(page.getByRole('dialog', { name: 'Preview: Scope parent' })).toBeVisible();
        expect(document.querySelectorAll('[data-nc-link-preview]')).toHaveLength(1);
        root.element().focus(); await userEvent.keyboard('{ArrowDown}');
        await userEvent.keyboard('{Escape}');
        expect(document.querySelector('[data-nc-link-preview]')).toBeNull();
      } finally { restore(); view.unmount(); }
    });
  it(`fences native focus before host observer delivery (reduced=${reduced})`, async () => {
    vi.useRealTimers(); await commands.emulateReducedMotion(reduced);
    const focused = vi.fn();
    render(<section style={{ padding: 80 }}><HoverPreview title="Focus scope"
      trigger={activate => <button onClick={activate}>Focus scope</button>}>
      <button onFocus={focused}>Focus action</button>
    </HoverPreview></section>);
    await page.getByRole('button', { name: 'Focus scope' }).click();
    const action = page.getByRole('button', { name: 'Focus action' }).element() as HTMLElement;
    const card = action.closest<HTMLElement>('[data-nc-preview-owner]')!;
    const host = screen.getByRole('button', { name: 'Focus scope' }).closest('section')!;
    // Native focus runs synchronously, before MutationObserver's checkpoint.
    host.inert = true; action.focus();
    expect(focused).not.toHaveBeenCalled();
    expect(document.activeElement).not.toBe(action);
    expect(card.inert).toBe(true);
    expect(card.hasAttribute('data-nc-link-preview')).toBe(false);
    host.inert = false;
  });
  it(`cancels native pending hover across transient host disable (reduced=${reduced})`, async () => {
    vi.useRealTimers(); await commands.emulateReducedMotion(reduced);
    render(<><button>Before scope</button><section style={{ padding: 80 }}><HoverPreview title="Pending scope"
      trigger={activate => <button onClick={activate}>Pending scope</button>}>Body</HoverPreview></section></>);
    const trigger = page.getByRole('button', { name: 'Pending scope' });
    await trigger.hover();
    const host = trigger.element().closest('section')!;
    host.inert = true; host.inert = false;
    await new Promise(resolve => setTimeout(resolve, 400));
    expect(document.querySelector('[data-nc-link-preview]')).toBeNull();
    await trigger.unhover();
    screen.getByRole('button', { name: 'Before scope' }).focus();
    await userEvent.keyboard('{Tab}');
    await expect.element(page.getByRole('dialog', { name: 'Preview: Pending scope' })).toBeVisible();
    await userEvent.keyboard('{Escape}'); await trigger.unhover();
  });
}
