import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { commands, page, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';
import type { ReactNode } from 'react';
import { useState } from '../state/public.ts';
import { Dialog } from '@astryxdesign/core/Dialog';
import { BottomSheet, BottomSheetSwitcher } from '@astryxdesign/core/BottomSheet';
import { placePreview } from './placement.ts';
vi.mock('./placement.ts', { spy: true });
import { HoverPreview } from './public.tsx';

afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });
function mount(onRead = () => {}) {
  return render(<div style={{ padding: 80 }}>
    <HoverPreview title="Long notes" trigger={(activate) => <button onClick={activate}>Long notes</button>}>
      <button type="button" onClick={onRead}>Continue reading</button>
      <div>{Array.from({ length: 70 }, (_, i) => <p key={i}>Paragraph {i} — a long preview has its own scroll area.</p>)}</div>
    </HoverPreview>
    <button type="button" data-testid="outside" style={{ position: 'fixed', left: 800, top: 80 }}>Elsewhere</button>
  </div>);
}

// Native previews require the public shared-dismissal Switcher contract in 0.6.3.
// Standalone BottomSheet has a separate element Escape handler; it is not a supported host.
function NativeHost({ children, hasScrim = false, purpose = 'info', onClose = () => {} }: {
  children: ReactNode; hasScrim?: boolean; purpose?: 'info' | 'form' | 'required'; onClose?: () => void;
}) {
  const [open, setOpen] = useState(true);
  return <><button onClick={() => setOpen(true)}>Reopen host</button>
    <BottomSheetSwitcher activeSheet={open ? 'reading' : null} onActiveSheetChange={() => { onClose(); setOpen(false); }} hasScrim={hasScrim}>
      <BottomSheet sheetId="reading" label="Reading host" height="650px" purpose={purpose}><div style={{ padding: 32 }}>{children}</div></BottomSheet>
    </BottomSheetSwitcher></>;
}

describe('preview in a real browser', () => {
  it.each([true])('revokes the old sheet preview on public handoff (reduced=%s)', async reduced => {
    await commands.emulateReducedMotion(reduced);
    await page.viewport(390, 844);
    const action = vi.fn();
    let switchSheet: (value: string) => void = () => {};
    function Sheets() {
      const [active, setActive] = useState<string | null>('one');
      switchSheet = setActive;
      return <BottomSheetSwitcher activeSheet={active} onActiveSheetChange={setActive} hasScrim>
        <BottomSheet sheetId="one" label="One" height="650px"><div style={{ padding: 40 }}>
          <HoverPreview title="Old sheet" trigger={activate => <button onClick={activate}>Open old sheet</button>}>
            <button onClick={action}>Old action</button>
          </HoverPreview>
        </div></BottomSheet>
        <BottomSheet sheetId="two" label="Two" height="650px"><button>New control</button></BottomSheet>
      </BottomSheetSwitcher>;
    }
    render(<Sheets />);
    const trigger = page.getByRole('button', { name: 'Open old sheet' });
    await trigger.click();
    const old = page.getByRole('button', { name: 'Old action' });
    await expect.element(old).toBeVisible();
    const button = old.element() as HTMLElement;
    const card = button.closest<HTMLElement>('[data-nc-preview-owner]')!;
    try {
      await act(async () => { switchSheet('two'); await Promise.resolve(); });
      await new Promise(resolve => setTimeout(resolve, 80));
      const rect = button.getBoundingClientRect();
      const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2);
      // Exercise the actual native hit when stale paint still owns it. No forced dispatch.
      if (hit !== null && button.contains(hit)) { await old.hover(); await old.click(); }
      expect(action).not.toHaveBeenCalled();
      expect(card.hasAttribute('data-nc-link-preview')).toBe(false);
      expect(card.inert).toBe(true);
      await act(async () => { switchSheet('one'); await Promise.resolve(); });
      await new Promise(resolve => setTimeout(resolve, 400));
      expect(document.querySelector('[data-nc-link-preview]')).toBeNull();
      await trigger.click();
      await expect.element(page.getByRole('button', { name: 'Old action' })).toBeVisible();
      await userEvent.keyboard('{Escape}');
      expect(document.querySelector('[data-nc-link-preview]')).toBeNull();
    } finally { await trigger.unhover(); }
  });

  it.each([false, true])('uses the native Layer host inside public BottomSheetSwitcher (scrim=%s)', async hasScrim => {
    await commands.emulateReducedMotion(hasScrim);
    await page.viewport(390, 844);
    const onClose = vi.fn();
    const onRead = vi.fn();
    render(<NativeHost onClose={onClose} hasScrim={hasScrim}>
      <p><HoverPreview title="Hosted notes" trigger={activate => <button onClick={activate}>Hosted notes</button>}>
        <HoverPreview title="Nested notes" trigger={activate => <button onClick={activate}>Nested notes</button>}>
          <button onClick={onRead}>Read nested body</button>
        </HoverPreview>
        {Array.from({ length: 50 }, (_, i) => <p key={i}>Hosted paragraph {i}</p>)}
      </HoverPreview></p>
    </NativeHost>);
    const trigger = page.getByRole('button', { name: 'Hosted notes', exact: true });
    await expect.element(trigger).toBeVisible();
    await expect.poll(() => trigger.element().closest('dialog')?.open).toBe(true);
    await expect.poll(() => trigger.element().closest('dialog')?.getAnimations({ subtree: true }).every(animation => animation.playState !== 'running')).toBe(true);
    trigger.element().focus();
    await userEvent.keyboard('{ArrowDown}');
    const parent = page.getByRole('dialog', { name: 'Preview: Hosted notes' });
    await expect.element(parent).toBeVisible();
    const body = parent.getByRole('region', { name: 'Preview content: Hosted notes' }).element() as HTMLElement;
    await expect.poll(() => document.activeElement).toBe(body);
    expect(body.closest('[popover]')?.matches(':popover-open')).toBe(true);
    expect(body.closest('dialog')?.open).toBe(true);
    const rect = body.getBoundingClientRect();
    expect(body.contains(document.elementFromPoint(rect.left + rect.width / 2, rect.bottom - 4))).toBe(true);
    if (hasScrim) expect(getComputedStyle(parent.element()).animationName).toBe('none');
    const nested = parent.getByRole('button', { name: 'Nested notes', exact: true });
    await nested.click();
    await expect.element(page.getByRole('dialog', { name: 'Preview: Nested notes' })).toBeVisible();
    await userEvent.keyboard('{Escape}');
    expect(page.getByRole('dialog', { name: 'Preview: Nested notes' }).query()).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    await nested.click();
    const read = page.getByRole('button', { name: 'Read nested body' });
    await read.click();
    expect(onRead).toHaveBeenCalledOnce();
    await userEvent.keyboard('{Escape}');
    expect(page.getByRole('dialog', { name: 'Preview: Nested notes' }).query()).toBeNull();
    expect(parent.query()).not.toBeNull();
    expect(document.activeElement).toBe(parent.getByRole('button', { name: 'Nested notes', exact: true }).element());
    await parent.getByText('Hosted paragraph 0', { exact: true }).wheel({ delta: { y: 240 } });
    await expect.poll(() => body.scrollTop).toBeGreaterThan(0);
    body.focus();
    await userEvent.keyboard('{Escape}');
    expect(parent.query()).toBeNull();
    expect(document.activeElement).toBe(trigger.element());
    expect(onClose).not.toHaveBeenCalled();
  });
  it.each([false, true])('routes hovered child Escape from both parent focus locations (scrim=%s)', async hasScrim => {
    await commands.emulateReducedMotion(true);
    await page.viewport(390, 844);
    const close = vi.fn();
    render(<NativeHost hasScrim={hasScrim} onClose={close}>
      <HoverPreview title="Parent" trigger={activate => <button onClick={activate}>Parent</button>}>
        <HoverPreview title="Child" trigger={activate => <button onClick={activate}>Child</button>}><button>Read child</button></HoverPreview>
      </HoverPreview>
    </NativeHost>);
    const root = page.getByRole('button', { name: 'Parent', exact: true });
    await expect.element(root).toBeVisible();
    const host = root.element().closest('dialog')!;
    await expect.poll(() => host.getAnimations({ subtree: true }).every(animation => animation.playState !== 'running')).toBe(true);
    for (const focus of ['content', 'trigger']) {
      root.element().focus();
      await userEvent.keyboard('{ArrowDown}');
      const parent = page.getByRole('region', { name: 'Preview content: Parent' });
      await expect.element(parent).toBeVisible();
      if (focus === 'trigger') root.element().focus();
      else (parent.element() as HTMLElement).focus();
      const nested = page.getByRole('button', { name: 'Child', exact: true });
      await nested.hover();
      const child = page.getByRole('dialog', { name: 'Preview: Child' });
      await expect.element(child).toBeVisible();
      await userEvent.keyboard('{Escape}');
      expect(child.query()).toBeNull();
      expect(parent.query()).not.toBeNull();
      expect(close).not.toHaveBeenCalled();
      await root.hover();
      await nested.hover();
      await expect.element(child).toBeVisible();
      await userEvent.keyboard('{Escape}');
      expect(child.query()).toBeNull();
      await userEvent.keyboard('{Escape}');
      expect(parent.query()).toBeNull();
      expect(host.open).toBe(true);
    }
    await userEvent.keyboard('{Escape}');
    expect(close).toHaveBeenCalledOnce();
    await expect.poll(() => host.open).toBe(false);
    await page.getByRole('button', { name: 'Reopen host' }).click();
    await expect.poll(() => host.open).toBe(true);
  });

  it.each(['preventDefault', 'stopPropagation'])('preserves editor consumption before shared dismissal: %s', async mode => {
    await page.viewport(390, 844);
    const edit = vi.fn(); const close = vi.fn();
    render(<NativeHost onClose={close}><HoverPreview title="Editor notes" trigger={activate => <button onClick={activate}>Editor notes</button>}>
      <input aria-label="Local editor" onKeyDown={event => {
        if (event.key !== 'Escape') return;
        edit();
        if (mode === 'preventDefault') event.preventDefault(); else event.stopPropagation();
      }} />
      <HoverPreview title="Editor child" trigger={activate => <button onClick={activate}>Editor child</button>}>Child body</HoverPreview>
    </HoverPreview></NativeHost>);
    await page.getByRole('button', { name: 'Editor notes', exact: true }).click();
    page.getByRole('textbox', { name: 'Local editor' }).element().focus();
    await page.getByRole('button', { name: 'Editor child', exact: true }).hover();
    await expect.element(page.getByRole('dialog', { name: 'Preview: Editor child' })).toBeVisible();
    await userEvent.keyboard('{Escape}');
    expect(edit).toHaveBeenCalledOnce();
    expect(document.querySelectorAll('[data-nc-link-preview]')).toHaveLength(2);
    expect(close).not.toHaveBeenCalled();
  });

  it.each(['isComposing', 'keyCode229'])('keeps previews open for IME key flags at child and parent: %s', async mode => {
    await page.viewport(390, 844);
    const close = vi.fn();
    render(<NativeHost onClose={close}><HoverPreview title="IME parent" trigger={activate => <button onClick={activate}>IME parent</button>}>
      <HoverPreview title="IME child" trigger={activate => <button onClick={activate}>IME child</button>}>Child body</HoverPreview>
    </HoverPreview></NativeHost>);
    await page.getByRole('button', { name: 'IME parent', exact: true }).click();
    await page.getByRole('button', { name: 'IME child', exact: true }).click();
    for (const name of ['IME child', 'IME parent']) {
      const content = page.getByRole('region', { name: `Preview content: ${name}` }).element() as HTMLElement;
      content.focus();
      // Event flags are a browser contract stimulus, not an OS IME simulation.
      const event = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true,
        isComposing: mode === 'isComposing', keyCode: mode === 'keyCode229' ? 229 : 27 });
      content.dispatchEvent(event);
      expect(event.defaultPrevented).toBe(true);
      expect(document.querySelectorAll('[data-nc-link-preview]')).toHaveLength(2);
    }
    expect(close).not.toHaveBeenCalled();
  });

  it.each(['info', 'form', 'required'] as const)('gates non-keyboard native cancel by shared topmost and purpose: %s', async purpose => {
    await page.viewport(390, 844);
    const close = vi.fn();
    render(<NativeHost purpose={purpose} onClose={close}><HoverPreview title="Cancel child" trigger={activate => <button onClick={activate}>Cancel child</button>}>Child body</HoverPreview></NativeHost>);
    const trigger = page.getByRole('button', { name: 'Cancel child', exact: true });
    const host = trigger.element().closest('dialog')!;
    await trigger.click();
    const requestCancel = () => {
      const event = new Event('cancel', { cancelable: true }); host.dispatchEvent(event); expect(event.defaultPrevented).toBe(true);
    };
    requestCancel();
    expect(close).not.toHaveBeenCalled();
    expect(page.getByRole('dialog', { name: 'Preview: Cancel child' }).query()).not.toBeNull();
    await userEvent.keyboard('{Escape}');
    // Composition belongs to the shared owner for non-keyboard close requests too.
    trigger.element().dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
    requestCancel();
    expect(close).not.toHaveBeenCalled();
    trigger.element().dispatchEvent(new CompositionEvent('compositionend', { bubbles: true }));
    requestCancel();
    if (purpose === 'required') {
      expect(close).not.toHaveBeenCalled(); expect(host.open).toBe(true);
    } else {
      expect(close).toHaveBeenCalledOnce(); await expect.poll(() => host.open).toBe(false);
    }
  });

  it.each(['info', 'form', 'required'] as const)('preserves public purpose semantics for Escape, backdrop and swipe: %s', async purpose => {
    await commands.emulateReducedMotion(true);
    await page.viewport(390, 844);
    const close = vi.fn();
    render(<NativeHost hasScrim purpose={purpose} onClose={close}><button>Host control</button></NativeHost>);
    const control = page.getByRole('button', { name: 'Host control' });
    await expect.element(control).toBeVisible();
    const host = control.element().closest('dialog')!;
    const reopen = async () => {
      await expect.poll(() => host.open).toBe(false);
      await page.getByRole('button', { name: 'Reopen host' }).click();
      await expect.poll(() => host.open).toBe(true);
    };
    control.element().focus();
    await userEvent.keyboard('{Escape}');
    expect(close).toHaveBeenCalledTimes(purpose === 'required' ? 0 : 1);
    if (purpose !== 'required') await reopen();
    close.mockClear();
    // The dialog occupies the viewport; this point is above the sheet panel.
    await userEvent.click(host, { position: { x: 10, y: 10 } });
    expect(close).toHaveBeenCalledTimes(purpose === 'info' ? 1 : 0);
    if (purpose === 'info') await reopen();
    close.mockClear();
    const handle = host.querySelector<HTMLElement>('[aria-hidden="true"]')!;
    await userEvent.dragAndDrop(handle, host, { targetPosition: { x: 195, y: 810 } });
    expect(close).toHaveBeenCalledTimes(purpose === 'info' ? 1 : 0);
    if (purpose === 'info') await expect.poll(() => host.open).toBe(false);
    else expect(host.open).toBe(true);
  });

  it('leaves a foreign public Dialog top layer in charge of Escape', async () => {
    await page.viewport(390, 844);
    const close = vi.fn(); const foreignClose = vi.fn();
    function Content() {
      const [open, setOpen] = useState(false);
      return <HoverPreview title="Foreign parent" trigger={activate => <button onClick={activate}>Foreign parent</button>}>
        <button onClick={() => setOpen(true)}>Open foreign dialog</button>
        <Dialog isOpen={open} onOpenChange={() => { foreignClose(); setOpen(false); }} aria-label="Foreign dialog"><button>Foreign content</button></Dialog>
      </HoverPreview>;
    }
    render(<NativeHost onClose={close}><Content /></NativeHost>);
    await page.getByRole('button', { name: 'Foreign parent', exact: true }).click();
    await page.getByRole('button', { name: 'Open foreign dialog' }).click();
    await expect.element(page.getByRole('button', { name: 'Foreign content' })).toBeVisible();
    await userEvent.keyboard('{Escape}');
    expect(foreignClose).toHaveBeenCalledOnce();
    expect(close).not.toHaveBeenCalled();
    expect(page.getByRole('dialog', { name: 'Preview: Foreign parent' }).query()).not.toBeNull();
  });

  it('keeps the link-to-card edge open during slow transfer, supports interaction, then dismisses', async () => {
    await page.viewport(1200, 900);
    const onRead = vi.fn(); mount(onRead);
    await page.getByRole('button', { name: 'Long notes', exact: true }).hover();
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).not.toBeNull();
    const card = document.querySelector<HTMLElement>('[data-nc-link-preview]')!;
    expect(card.querySelector('button[aria-pressed], svg')).toBeNull();
    const before = performance.now();
    await page.getByRole('dialog').hover({ position: { x: 30, y: -6 }, force: true });
    await expect.poll(() => performance.now() - before).toBeGreaterThan(350);
    expect(card.isConnected).toBe(true);
    await page.getByRole('button', { name: 'Continue reading' }).click();
    expect(onRead).toHaveBeenCalledOnce();
    const body = card.children[1] as HTMLElement;
    expect(body.scrollHeight).toBeGreaterThan(body.clientHeight);
    await page.getByText('Paragraph 0 — a long preview has its own scroll area.').wheel({ delta: { y: 240 } });
    await expect.poll(() => body.scrollTop).toBeGreaterThan(0);
    await page.getByTestId('outside').hover();
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).toBeNull();
  });
  it('hands keyboard focus into the preview and dismisses after native Tab leaves', async () => {
    await page.viewport(1200, 900);
    const onRead = vi.fn(); mount(onRead);
    await userEvent.tab();
    expect(document.activeElement).toBe(page.getByRole('button', { name: 'Long notes', exact: true }).element());
    await userEvent.keyboard('{ArrowDown}');
    const content = page.getByRole('region', { name: 'Preview content: Long notes' });
    await expect.poll(() => document.activeElement).toBe(content.element());
    await userEvent.tab();
    expect(document.activeElement).toBe(page.getByRole('button', { name: 'Continue reading' }).element());
    await userEvent.keyboard('{Enter}');
    expect(onRead).toHaveBeenCalledOnce();
    const before = performance.now();
    await expect.poll(() => performance.now() - before).toBeGreaterThan(350);
    expect(document.querySelector('[data-nc-link-preview]')).not.toBeNull();
    for (let step = 0; step < 6 && document.activeElement !== page.getByTestId('outside').element(); step += 1) {
      await userEvent.tab({ shift: true });
    }
    expect(document.activeElement).toBe(page.getByTestId('outside').element());
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).toBeNull();
  });
  it('keeps both cards open while scrolling a nested portal preview', async () => {
    await page.viewport(1200, 900);
    render(<div style={{ padding: 80 }}>
      <HoverPreview title="Parent" trigger={(activate) => <button onClick={activate}>Parent</button>}>
        <HoverPreview title="Child" trigger={(activate) => <button onClick={activate}>Child</button>}>
          <div>{Array.from({ length: 70 }, (_, i) => <p key={i}>Nested paragraph {i}</p>)}</div>
        </HoverPreview>
      </HoverPreview>
    </div>);
    await page.getByRole('button', { name: 'Parent', exact: true }).click();
    await page.getByRole('button', { name: 'Child', exact: true }).click();
    const child = page.getByRole('dialog', { name: 'Preview: Child' });
    const body = child.element().children[1] as HTMLElement;
    await child.getByText('Nested paragraph 0', { exact: true }).wheel({ delta: { y: 240 } });
    await expect.poll(() => body.scrollTop).toBeGreaterThan(0);
    const before = performance.now();
    await expect.poll(() => performance.now() - before).toBeGreaterThan(350);
    expect(page.getByRole('dialog').length).toBe(2);
  });
  it('keeps nested keyboard scrolling in the child instead of activating its parent', async () => {
    await page.viewport(1200, 900);
    render(<div style={{ padding: 80 }}>
      <HoverPreview title="Parent" trigger={(activate) => <button onClick={activate}>Parent</button>}>
        <HoverPreview title="Child" trigger={(activate) => <button onClick={activate}>Child</button>}>
          <div>{Array.from({ length: 70 }, (_, i) => <p key={i}>Child reading {i}</p>)}</div>
        </HoverPreview>
      </HoverPreview>
    </div>);
    await userEvent.tab();
    await userEvent.keyboard('{ArrowDown}');
    await userEvent.tab();
    expect(document.activeElement).toBe(page.getByRole('button', { name: 'Child', exact: true }).element());
    await userEvent.keyboard('{ArrowDown}');
    const childContent = page.getByRole('region', { name: 'Preview content: Child' }).element() as HTMLElement;
    await expect.poll(() => document.activeElement).toBe(childContent);
    await userEvent.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(childContent);
    await expect.poll(() => childContent.scrollTop).toBeGreaterThan(0);
    const before = performance.now();
    await expect.poll(() => performance.now() - before).toBeGreaterThan(350);
    expect(page.getByRole('dialog').length).toBe(2);
  });
  it('allows slow travel to a distant side card without intercepting the original link', async () => {
    await page.viewport(1440, 900);
    const onRead = vi.fn();
    render(<><section style={{ position: 'absolute', left: 100, top: 100, width: 480, paddingTop: 100 }}>
      <HoverPreview title="Side notes" getReadingSurface={(trigger) => trigger.closest<HTMLElement>('section')}
        trigger={(activate) => <button onClick={activate}>Side notes</button>}>
        <button type="button" onClick={onRead}>Read details</button>
      </HoverPreview>
    </section><button type="button" data-testid="far-outside" style={{ position: 'fixed', left: 1100, top: 80 }}>Elsewhere</button></>);
    const trigger = page.getByRole('button', { name: 'Side notes', exact: true });
    await trigger.hover();
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).not.toBeNull();
    const anchor = trigger.element().getBoundingClientRect();
    const dialog = page.getByRole('dialog');
    const card = dialog.element().getBoundingClientRect();
    const x = (anchor.right + card.left) / 2;
    const y = (anchor.top + anchor.bottom) / 2;
    expect(card.left - anchor.right).toBeGreaterThan(200);
    await dialog.hover({ position: { x: x - card.left, y: y - card.top }, force: true });
    expect(document.elementFromPoint(x, y)?.closest('[data-nc-link-preview]')).toBeNull();
    const before = performance.now();
    await expect.poll(() => performance.now() - before).toBeGreaterThan(400);
    expect(dialog.query()).not.toBeNull();
    await page.getByRole('button', { name: 'Read details' }).click();
    expect(onRead).toHaveBeenCalledOnce();
    await trigger.click();
    expect(document.activeElement).toBe(trigger.element());
    await page.getByTestId('far-outside').hover();
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).toBeNull();
  });
  it('closes on a click outside the preview and keeps its content usable without toolbar buttons', async () => {
    await page.viewport(1200, 900);
    const onRead = vi.fn(); mount(onRead);
    await page.getByRole('button', { name: 'Long notes', exact: true }).click();
    const card = page.getByRole('dialog');
    expect(card.getByRole('button').length).toBe(1);
    await card.getByRole('button', { name: 'Continue reading' }).click();
    expect(onRead).toHaveBeenCalledOnce();
    await page.getByTestId('outside').click();
    expect(card.query()).toBeNull();
  });
  it('places a child preview beside its parent without covering either card trigger', async () => {
    await page.viewport(1600, 900);
    render(<div style={{ padding: 80 }}><HoverPreview title="Parent" trigger={(activate) => <button onClick={activate}>Parent</button>}>
      <HoverPreview title="Child" trigger={(activate) => <button onClick={activate}>Child</button>}>
        <div>{Array.from({ length: 70 }, (_, i) => <p key={i}>Child paragraph {i}</p>)}</div>
      </HoverPreview>
    </HoverPreview></div>);
    await page.getByRole('button', { name: 'Parent', exact: true }).click();
    await page.getByRole('button', { name: 'Child', exact: true }).click();
    const parent = page.getByRole('dialog', { name: 'Preview: Parent' }).element().getBoundingClientRect();
    const child = page.getByRole('dialog', { name: 'Preview: Child' }).element().getBoundingClientRect();
    expect(child.left >= parent.right || child.right <= parent.left || child.top >= parent.bottom || child.bottom <= parent.top).toBe(true);
  });
  it('clamps an open preview after viewport resize and closes with Escape', async () => {
    await page.viewport(1200, 900); mount();
    await page.getByRole('button', { name: 'Long notes', exact: true }).click();
    const card = document.querySelector<HTMLElement>('[data-nc-link-preview]')!;
    await page.viewport(390, 650);
    await expect.poll(() => card.getBoundingClientRect().right).toBeLessThanOrEqual(379);
    expect(card.getBoundingClientRect().left).toBeGreaterThanOrEqual(11);
    expect(card.getBoundingClientRect().bottom).toBeLessThanOrEqual(639);
    await userEvent.keyboard('{Escape}');
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).toBeNull();
  });
});

it('uses one source lift per nested portal and dismisses navigation before retained paint settles', () => {
  const navigate = vi.fn();
  render(<div style={{ padding: 80 }}><HoverPreview title="Root" trigger={activate => <button onClick={activate}>Root source</button>}>
    <HoverPreview title="Nested" trigger={activate => <button onClick={activate}>Nested source</button>}>
      {dismiss => <button onClick={() => { dismiss(); navigate(document.activeElement); }}>Navigate now</button>}
    </HoverPreview>
  </HoverPreview></div>);
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Root source' })); });
  const root = screen.getByRole('dialog', { name: 'Preview: Root' });
  root.getAnimations()[0].pause(); root.getAnimations()[0].currentTime = 80;
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Nested source' })); });
  const nested = screen.getByRole('dialog', { name: 'Preview: Nested' });
  const paintedSource = screen.getByRole('button', { name: 'Nested source' }).parentElement!.getBoundingClientRect();
  const source = { x: paintedSource.left + paintedSource.width / 2, y: paintedSource.top + paintedSource.height / 2 };
  const animation = nested.getAnimations()[0]; animation.pause(); animation.currentTime = 0;
  const layout = nested.parentElement!.getBoundingClientRect();
  const matrix = new DOMMatrix(getComputedStyle(nested).transform);
  expect(matrix.e).toBeCloseTo((1 - 800 / 808) * (source.x - layout.left), 3);
  expect(matrix.f).toBeCloseTo((1 - 800 / 808) * (source.y - layout.top) + 0.56, 3);
  expect(root.getAnimations()).toHaveLength(1); expect(nested.getAnimations()).toHaveLength(1);
  expect(animation instanceof CSSAnimation).toBe(false);
  const action = screen.getByRole('button', { name: 'Navigate now' });
  act(() => { action.focus(); action.click(); });
  expect(navigate).toHaveBeenCalledExactlyOnceWith(screen.getByRole('button', { name: 'Root source' }));
  expect(screen.queryAllByRole('dialog')).toHaveLength(0);
  expect(root.isConnected).toBe(true); expect(nested.isConnected).toBe(true);
  expect(root.inert).toBe(true); expect(nested.inert).toBe(true);
  expect(root.getAttribute('aria-hidden')).toBe('true'); expect(nested.getAttribute('aria-hidden')).toBe('true');
});

it('passes real pointer hits through retained positioners and hands focus over on rapid keyboard reopen', async () => {
  await page.viewport(1200, 900);
  mount();
  const trigger = screen.getByRole('button', { name: 'Long notes' });
  fireEvent.keyDown(trigger, { key: 'ArrowDown' });
  const card = screen.getByRole('dialog');
  expect(document.activeElement).toBe(screen.getByRole('region'));
  const entering = card.getAnimations()[0]; entering.pause(); entering.currentTime = 80;
  fireEvent.keyDown(document, { key: 'Escape' });
  const closing = card.getAnimations()[0]; closing.pause(); closing.currentTime = 20;
  const rect = card.parentElement!.getBoundingClientRect();
  const behind = screen.getByTestId('outside');
  Object.assign(behind.style, { left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px`, zIndex: '0' });
  const hit = vi.fn(); behind.addEventListener('click', hit);
  expect(document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2)).toBe(behind);
  await page.getByTestId('outside').click(); expect(hit).toHaveBeenCalledOnce();
  fireEvent.keyDown(trigger, { key: 'ArrowDown' });
  expect(card.inert).toBe(false); expect(document.activeElement).toBe(screen.getByRole('region'));
});

it.each(['scroll', 'outside', 'leave', 'navigation'])('revokes nested portal interaction on parent %s and does not reopen an old child', async reason => {
  await page.viewport(1600, 1000);
  const action = vi.fn();
  render(<section data-testid="ancestor" style={{ padding: 80 }}>
    <HoverPreview title="Parent" trigger={(activate, dismiss) => <><button onClick={activate}>Parent</button><button onClick={dismiss}>Go away</button></>}>
      <HoverPreview title="Child" trigger={activate => <button onClick={activate}>Child</button>}>
        <button onClick={action}>Child action</button>
      </HoverPreview>
    </HoverPreview><button data-testid="away">Away</button>
  </section>);
  fireEvent.click(screen.getByRole('button', { name: 'Parent' }));
  const parent = screen.getByRole('dialog', { name: 'Preview: Parent' });
  fireEvent.click(screen.getByRole('button', { name: 'Child' }));
  const cards = screen.getAllByRole('dialog');
  for (const card of cards) { const animation = card.getAnimations()[0]; animation.pause(); animation.currentTime = 80; }
  const childAction = screen.getByRole('button', { name: 'Child action' });
  if (reason === 'scroll') fireEvent.scroll(screen.getByTestId('ancestor'));
  if (reason === 'outside') fireEvent.pointerDown(screen.getByTestId('away'));
  if (reason === 'leave') { fireEvent.pointerOut(parent, { relatedTarget: screen.getByTestId('away') }); await expect.poll(() => parent.inert).toBe(true); }
  if (reason === 'navigation') fireEvent.click(screen.getByRole('button', { name: 'Go away' }));
  // Pause exits so semantic assertions cannot pass by waiting for unmount.
  for (const card of cards) {
    expect(card.isConnected).toBe(true); expect(card.inert).toBe(true);
    expect(card.getAttribute('aria-hidden')).toBe('true');
    expect(card.hasAttribute('data-nc-link-preview')).toBe(false); expect(card.hasAttribute('data-nc-escape-layer')).toBe(false);
    for (const animation of card.getAnimations()) animation.pause();
  }
  childAction.click(); expect(action).not.toHaveBeenCalled();
  fireEvent.keyDown(document, { key: 'Escape' });
  fireEvent.keyDown(screen.getByRole('button', { name: 'Parent' }), { key: 'ArrowDown' });
  expect(screen.getAllByRole('dialog')).toEqual([parent]);
  expect(document.activeElement).toBe(screen.getByRole('region', { name: 'Preview content: Parent' }));
});


it('focuses committed interactive content on ArrowDown during a retained exit', () => {
  mount(); const trigger = screen.getByRole('button', { name: 'Long notes' });
  fireEvent.keyDown(trigger, { key: 'ArrowDown' }); const card = screen.getByRole('dialog');
  const entering = card.getAnimations()[0]; entering.pause(); entering.currentTime = 80;
  fireEvent.keyDown(document, { key: 'Escape' });
  const closing = card.getAnimations()[0]; closing.pause(); closing.currentTime = 20;
  fireEvent.keyDown(trigger, { key: 'ArrowDown' });
  expect(card.inert).toBe(false); expect(document.activeElement).toBe(screen.getByRole('region'));
});


it('revokes nested semantics on ancestor close with reduced motion enabled', async () => {
  await commands.emulateReducedMotion(true);
  try {
    render(<section data-testid="ancestor" style={{ padding: 80 }}>
      <HoverPreview title="Parent" trigger={activate => <button onClick={activate}>Parent</button>}>
        <HoverPreview title="Child" trigger={activate => <button onClick={activate}>Child</button>}><button>Read child</button></HoverPreview>
      </HoverPreview>
    </section>);
    fireEvent.click(screen.getByRole('button', { name: 'Parent' })); fireEvent.click(screen.getByRole('button', { name: 'Child' }));
    fireEvent.scroll(screen.getByTestId('ancestor')); expect(screen.queryAllByRole('dialog')).toHaveLength(0);
    expect(document.querySelector('[data-nc-link-preview], [data-nc-escape-layer]')).toBeNull();
    fireEvent.keyDown(screen.getByRole('button', { name: 'Parent' }), { key: 'ArrowDown' });
    expect(screen.getAllByRole('dialog')).toHaveLength(1); expect(document.activeElement).toBe(screen.getByRole('region'));
  } finally { await commands.emulateReducedMotion(false); }
});

it('revokes old children and admits new interaction after a parent close and reopen in one commit', () => {
  render(<section data-testid="ancestor" style={{ padding: 80 }}>
    <HoverPreview title="Parent" trigger={activate => <button onClick={activate}>Parent</button>}>
      <HoverPreview title="Child" trigger={activate => <button onClick={activate}>Child</button>}>Content</HoverPreview>
    </HoverPreview>
  </section>);
  const trigger = screen.getByRole('button', { name: 'Parent' }); fireEvent.click(trigger);
  fireEvent.click(screen.getByRole('button', { name: 'Child' }));
  act(() => { screen.getByTestId('ancestor').dispatchEvent(new Event('scroll')); trigger.click(); });
  expect(screen.queryByRole('dialog', { name: 'Preview: Child' })).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Child' }));
  expect(screen.getByRole('dialog', { name: 'Preview: Child' })).toBeTruthy();
});

it('observes fractional intrinsic growth after clamp and shrink without moving its slot or replaying entry', async () => {
  await page.viewport(1040, 900);
  render(<section style={{ position: 'absolute', left: 200, top: 100, width: 504 }}>
    <HoverPreview title="Intrinsic" getReadingSurface={trigger => trigger.closest('section')}
      trigger={activate => <button onClick={activate}>Intrinsic</button>}>
      <div data-testid="flow" style={{ height: 1000.25 }}>Long content</div>
    </HoverPreview>
  </section>);
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Intrinsic' })); });
  const card = screen.getByRole('dialog');
  const actual = card.parentElement!;
  const slot = actual.parentElement!;
  const flow = screen.getByTestId('flow');
  const intrinsic = flow.parentElement!;
  const css = vi.spyOn(window, 'getComputedStyle');
  await expect.poll(() => css.mock.calls.filter(([node]) => node === intrinsic).length).toBeGreaterThan(0);
  for (const animation of card.getAnimations()) animation.finish();
  await Promise.resolve();
  const reservation = slot.getBoundingClientRect();
  expect(reservation.width).toBe(312);
  const frame = actual.getBoundingClientRect();
  const region = card.querySelector<HTMLElement>('[role="region"]')!;
  const range = region.scrollHeight;
  const calls = css.mock.calls.filter(([node]) => node === intrinsic).length;
  flow.style.height = '1500.75px';
  await expect.poll(() => css.mock.calls.filter(([node]) => node === intrinsic).length).toBeGreaterThan(calls);
  expect(region.scrollHeight).toBeGreaterThan(range);
  expect(actual.getBoundingClientRect()).toEqual(frame);
  expect(slot.getBoundingClientRect()).toEqual(reservation);
  expect(card.getAnimations()).toHaveLength(0);
  flow.style.height = '61.25px';
  await expect.poll(() => actual.getBoundingClientRect().height).toBeLessThan(frame.height);
  const header = card.children[0].getBoundingClientRect().height;
  expect(actual.getBoundingClientRect().height).toBe(header + intrinsic.getBoundingClientRect().height + 2);
  expect(slot.getBoundingClientRect()).toEqual(reservation);
  const image = document.createElement('img');
  const loaded = new Promise<void>(resolve => { image.onload = () => resolve(); });
  flow.style.height = 'auto'; flow.append(image);
  image.src = 'data:image/svg+xml,' + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="200" height="700"><rect width="200" height="700" fill="blue"/></svg>');
  await loaded;
  await expect.poll(() => actual.getBoundingClientRect().height).toBe(frame.height);
  expect(slot.getBoundingClientRect()).toEqual(reservation);
  image.remove(); flow.textContent = 'xx '.repeat(80);
  flow.style.fontFamily = 'IntrinsicProbe, monospace';
  await expect.poll(() => actual.getBoundingClientRect().height).toBeLessThan(frame.height);
  // A minimal valid font resource with a deliberately wide x advances. Loading
  // it exercises native font layout/RO; it contains no placement implementation.
  const font = new FontFace('IntrinsicProbe', 'url(data:font/ttf;base64,AAEAAAAKAIAAAwAgT1MvMkUAST0AAAEoAAAAYGNtYXAA4gAlAAABlAAAADxnbHlmFpz6xgAAAdgAAAAWaGVhZC3SNgoAAACsAAAANmhoZWED6iWEAAAA5AAAACRobXR4FXwAAAAAAYgAAAAMbG9jYQAAAAsAAAHQAAAACG1heHAABQAGAAABCAAAACBuYW1lgUx9DgAAAfAAAAC9cG9zdAAIAFsAAAKwAAAAKAABAAAAAQAAW3Zhgl8PPPUAAwPoAAAAAOb5+TcAAAAA5vn5NwAAAAAAyAK8AAAAAwACAAAAAAAAAAEAAAMg/zgAABOIAAASwADIAAEAAAAAAAAAAAAAAAAAAAADAAEAAAADAAQAAQAAAAAAAgAAAAAAAAAAAAAAAAAAAAAAAwcpAZAABQAEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAPz8/PwAAACAAeAMg/zgAAAMgAMgAAAAAAAAAAAAAAAAAAAAgAAAA+gAAAPoAABOIAAAAAAACAAAAAwAAABQAAwABAAAAFAAEACgAAAAGAAQAAQACACAAeP//AAAAIAB4////4f+KAAEAAAAAAAAAAAAAAAAACwABAAAAAADIArwAAwAAMTMRI8jIArwAAAAAAAoAfgABAAAAAAABAA4AAAABAAAAAAACAAcADgABAAAAAAADAA4AAAABAAAAAAAEAA4AAAABAAAAAAAGAA4AAAADAAEECQABABwAFQADAAEECQACAA4AMQADAAEECQADABwAFQADAAEECQAEABwAFQADAAEECQAGABwAFUludHJpbnNpY1Byb2JlUmVndWxhcgBJAG4AdAByAGkAbgBzAGkAYwBQAHIAbwBiAGUAUgBlAGcAdQBsAGEAcgAAAAACAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAMAAAADAFs=)');
  document.fonts.add(font);
  try {
    await font.load(); await document.fonts.ready;
    await expect.poll(() => actual.getBoundingClientRect().height).toBe(frame.height);
    expect(slot.getBoundingClientRect()).toEqual(reservation);
  } finally { document.fonts.delete(font); }
  css.mockRestore();
});


it('uses only earlier interactive portal reservations and preserves retained reopen priority', async () => {
  await page.viewport(1440, 900);
  render(<div style={{ padding: 80 }}>
    {['First', 'Second', 'Third'].map(title => <HoverPreview key={title} title={title}
      trigger={activate => <button onClick={activate}>{title}</button>}><p>Content</p></HoverPreview>)}
  </div>);
  const inputs = vi.mocked(placePreview);
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'First' })); });
  const first = screen.getByRole('dialog', { name: 'Preview: First' });
  const firstSlot = first.closest<HTMLElement>('[data-nc-preview-slot]')!;
  const original = firstSlot.getBoundingClientRect();
  inputs.mockClear();
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Second' })); });
  const second = screen.getByRole('dialog', { name: 'Preview: Second' });
  act(() => { window.dispatchEvent(new Event('resize')); });
  expect(firstSlot.getBoundingClientRect()).toEqual(original);
  const admitted = inputs.mock.calls.at(-1)![0];
  expect(admitted.obstacles).toEqual([original]);
  expect(placePreview(admitted)).toEqual(placePreview(admitted));
  act(() => { fireEvent.keyDown(document, { key: 'Escape' }); });
  expect(second.inert).toBe(true);
  second.getAnimations().forEach(animation => { animation.pause(); animation.currentTime = 40; });
  inputs.mockClear();
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Third' })); });
  expect(inputs.mock.calls.at(-1)![0].obstacles).toEqual([original]);
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Second' })); });
  const secondAnchor = screen.getByRole('button', { name: 'Second' }).parentElement!.getBoundingClientRect();
  expect(inputs.mock.calls.filter(([input]) => input.anchor.left === secondAnchor.left).at(-1)![0].obstacles).toEqual([original]);
  // Native Layer wraps each slot; priority still follows adjacent slot DOM order.
  const slots = Array.from(document.querySelectorAll('[data-nc-preview-slot]'));
  expect(slots).toHaveLength(3);
  expect(slots[slots.indexOf(firstSlot) + 1]).toBe(second.closest('[data-nc-preview-slot]'));
});

it('reads direct visual inputs and only earlier reservations while native paint changes', async () => {
  await page.viewport(1200, 900);
  render(<div style={{ padding: 80 }}><HoverPreview title="Paint parent" trigger={activate => <button onClick={activate}>Paint parent</button>}>
    <HoverPreview title="Paint child" trigger={activate => <button onClick={activate}>Paint child</button>}><p>Child text</p></HoverPreview>
  </HoverPreview></div>);
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Paint parent' })); });
  const parent = screen.getByRole('dialog', { name: 'Preview: Paint parent' });
  parent.getAnimations()[0].pause(); parent.getAnimations()[0].currentTime = 0;
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Paint child' })); });
  const child = screen.getByRole('dialog', { name: 'Preview: Paint child' });
  const parentSlot = parent.closest('[data-nc-preview-slot]')!.getBoundingClientRect();
  const inputs = vi.mocked(placePreview);
  const paints = new Set<string>(); const sources = new Set<number>();
  for (const time of [0, 40, 80]) {
    for (const card of [parent, child]) { const animation = card.getAnimations()[0]; animation.pause(); animation.currentTime = time; }
    paints.add(getComputedStyle(parent).transform);
    act(() => { window.dispatchEvent(new Event('resize')); });
    const next = inputs.mock.calls.filter(([input]) => input.reading !== null).at(-1)![0];
    const source = screen.getByRole('button', { name: 'Paint child' }).parentElement!.getBoundingClientRect();
    expect(next.anchor).toEqual(source); sources.add(source.top);
    expect(next.reading).toEqual(parent.getBoundingClientRect());
    expect(next.obstacles).toEqual([parentSlot]);
    expect(parent.closest('[data-nc-preview-slot]')!.getBoundingClientRect()).toEqual(parentSlot);
  }
  expect(paints.size).toBe(3); expect(sources.size).toBe(3);
});

it('follows live parent visual source through the terminal sample and settles without another input', async () => {
  await page.viewport(1440, 1000);
  const frames = async (count: number) => { for (let i = 0; i < count; i++) await new Promise(requestAnimationFrame); };
  render(<div style={{ position: 'absolute', left: 950, top: 140 }}><HoverPreview title="Live parent" trigger={open => <button onClick={open}>Live parent</button>}>
    <div style={{ height: 30 }} /><HoverPreview title="Live child" trigger={open => <button onClick={open}>Live child</button>}><p>Content</p></HoverPreview><div style={{ height: 800 }} />
  </HoverPreview></div>);
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Live parent' })); });
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Live child' })); });
  const parent = screen.getByRole('dialog', { name: 'Preview: Live parent' });
  const child = screen.getByRole('dialog', { name: 'Preview: Live child' });
  const slot = child.closest<HTMLElement>('[data-nc-preview-slot]')!;
  await frames(3);
  const initial = slot.getBoundingClientRect().top;
  const inputs = vi.mocked(placePreview); inputs.mockClear();
  // No resize/seek/input is delivered while the native parent moves.
  await frames(5);
  expect(slot.getBoundingClientRect().top).not.toBe(initial);
  expect(inputs.mock.calls.every(([input]) => input.reading !== null)).toBe(true);
  expect(inputs.mock.calls.some(([input]) => Math.abs(input.anchor.top - screen.getByRole('button', { name: 'Live child' }).parentElement!.getBoundingClientRect().top) < 1)).toBe(true);
  parent.getAnimations().forEach(animation => animation.finish());
  await frames(5);
  const source = screen.getByRole('button', { name: 'Live child' }).parentElement!.getBoundingClientRect();
  const childInputs = inputs.mock.calls.filter(([input]) => input.reading !== null);
  expect(childInputs.at(-1)![0].anchor).toEqual(source);
  await expect.poll(() => document.getAnimations().length).toBe(0);
  await frames(5); inputs.mockClear(); const writes: MutationRecord[] = [];
  const observer = new MutationObserver(records => writes.push(...records)); observer.observe(slot, { attributes: true, subtree: true });
  await frames(20); observer.disconnect(); expect(inputs).not.toHaveBeenCalled(); expect(writes).toEqual([]);
});

it('consumes real source moves and side changes with the same card and immediate focus', async () => {
  await page.viewport(1440, 1000);
  const fixture = (left: number) => <div style={{ position: 'absolute', left, top: 140 }}><HoverPreview title="Moving source"
    trigger={open => <button onClick={open}>Moving source</button>}><button>Action</button></HoverPreview></div>;
  const view = render(fixture(80));
  act(() => { fireEvent.keyDown(screen.getByRole('button', { name: 'Moving source' }), { key: 'ArrowDown' }); });
  const card = screen.getByRole('dialog'); const slot = card.closest<HTMLElement>('[data-nc-preview-slot]')!;
  expect(document.activeElement).toBe(card.querySelector('[role=region]'));
  const source = () => screen.getByRole('button', { name: 'Moving source' }).parentElement!.getBoundingClientRect();
  expect(slot.getBoundingClientRect().left).toBeGreaterThan(source().right);
  const before = slot.getBoundingClientRect().top;
  view.rerender(fixture(1250));
  expect(screen.getByRole('dialog')).toBe(card);
  expect(slot.getBoundingClientRect().right).toBeLessThan(source().left);
  expect(slot.getBoundingClientRect().top).toBe(before);
  const inputs = vi.mocked(placePreview).mock.calls;
  expect(inputs.at(-1)![0].anchor).toEqual(source());
  expect(card.getAnimations()).toHaveLength(1);
  act(() => { fireEvent.keyDown(card.querySelector('[role=region]')!, { key: 'Escape' }); });
  expect(card.inert).toBe(true); expect(slot.inert).toBe(true);
  expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Moving source' }));
});


it.each(['seek', 'resume'] as const)('follows native parent %s after drained pause without another geometry input', async control => {
  await page.viewport(1440, 1000);
  const frames = async (count: number) => { for (let i = 0; i < count; i++) await new Promise(requestAnimationFrame); };
  render(<div style={{ position: 'absolute', left: 950, top: 140 }}><HoverPreview title="Resume parent" trigger={open => <button onClick={open}>Resume parent</button>}>
    <div style={{ height: 30 }} /><HoverPreview title="Resume child" trigger={open => <button onClick={open}>Resume child</button>}><p>Content</p></HoverPreview><div style={{ height: 800 }} />
  </HoverPreview></div>);
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Resume parent' })); });
  const parent = screen.getByRole('dialog', { name: 'Preview: Resume parent' });
  const native = parent.getAnimations()[0]; native.pause(); await native.ready; native.currentTime = 0;
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Resume child' })); });
  const child = screen.getByRole('dialog', { name: 'Preview: Resume child' });
  child.getAnimations().forEach(animation => animation.finish());
  const slot = child.closest<HTMLElement>('[data-nc-preview-slot]')!;
  const inputs = vi.mocked(placePreview); await frames(8);
  const before = slot.getBoundingClientRect().top;
  if (control === 'seek') {
    native.currentTime = 80; await frames(8);
    expect(slot.getBoundingClientRect().top).not.toBe(before);
  }
  await frames(10); const held = slot.getBoundingClientRect(); inputs.mockClear();
  const request = vi.spyOn(window, 'requestAnimationFrame'); const writes: MutationRecord[] = [];
  const observer = new MutationObserver(records => writes.push(...records)); observer.observe(slot, { attributes: true, subtree: true });
  await frames(20); observer.disconnect(); expect(request).toHaveBeenCalledTimes(20); expect(writes).toEqual([]); expect(inputs).not.toHaveBeenCalled();
  native.playbackRate = 0.1; native.play(); await native.ready; await frames(10);
  expect(slot.getBoundingClientRect().top).not.toBe(held.top); expect(parent.getAnimations()[0]).toBe(native);
  native.finish(); await frames(8);
  const source = screen.getByRole('button', { name: 'Resume child' }).parentElement!.getBoundingClientRect();
  expect(inputs.mock.calls.filter(([input]) => input.reading !== null).at(-1)![0].anchor).toEqual(source);
  await expect.poll(() => document.getAnimations().length).toBe(0);
  await frames(5); request.mockClear(); inputs.mockClear(); writes.length = 0; observer.observe(slot, { attributes: true, subtree: true });
  await frames(20); observer.disconnect(); expect(writes).toEqual([]); expect(request).toHaveBeenCalledTimes(20); expect(inputs).not.toHaveBeenCalled();
  request.mockRestore();
});
