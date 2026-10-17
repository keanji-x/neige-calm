import { cleanup, render } from '@testing-library/react';
import { commands, page, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';
import type { ReactNode } from 'react';
import { useState } from '../state/public.ts';
import { Dialog } from '@astryxdesign/core/Dialog';
import { BottomSheet, BottomSheetSwitcher } from '@astryxdesign/core/BottomSheet';
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
