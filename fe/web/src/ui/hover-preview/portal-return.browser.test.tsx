import { cleanup, render } from '@testing-library/react';
import { BottomSheet, BottomSheetSwitcher } from '@astryxdesign/core/BottomSheet';
import { commands, page, userEvent } from 'vitest/browser';
import { afterEach, it, expect, vi } from 'vitest';

import '../../styles/entry.css';
import { HoverPreview } from './public.tsx';

afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });

  it.each([
    [false, 'content'], [false, 'trigger'], [true, 'content'], [true, 'trigger'],
  ] as const)('keeps the parent on portal-to-trigger return (scrim=%s focus=%s)', async (hasScrim, focus) => {
    await commands.emulateReducedMotion(true);
    await page.viewport(390, 844);
    const close = vi.fn();
    render(<BottomSheetSwitcher activeSheet="reading" onActiveSheetChange={close} hasScrim={hasScrim}>
      <BottomSheet sheetId="reading" label="Reading host" height="650px"><div style={{ padding: 32 }}>
      <HoverPreview title="Return parent" trigger={activate => <button onClick={activate}>Return parent</button>}>
        <HoverPreview title="Return child" trigger={activate => <button onClick={activate}>Return child</button>}>Child body</HoverPreview>
      </HoverPreview>
    </div></BottomSheet>
    </BottomSheetSwitcher>);
    const root = page.getByRole('button', { name: 'Return parent', exact: true });
    const host = root.element().closest('dialog')!;
    await expect.poll(() => host.getAnimations({ subtree: true }).every(animation => animation.playState !== 'running')).toBe(true);
    root.element().focus();
    await userEvent.keyboard('{ArrowDown}');
    const parent = page.getByRole('region', { name: 'Preview content: Return parent' });
    await expect.element(parent).toBeVisible();
    if (focus === 'trigger') root.element().focus();
    else (parent.element() as HTMLElement).focus();
    const nested = page.getByRole('button', { name: 'Return child', exact: true });
    await nested.hover();
    const child = page.getByRole('dialog', { name: 'Preview: Return child' });
    await expect.element(child).toBeVisible();
    await userEvent.keyboard('{Escape}');
    expect(child.query()).toBeNull();
    await root.hover();
    // Observe a held real pointer for 20 native frames: returning to the
    // trigger must not start the card's leave deadline while the driver waits.
    for (let frame = 0; frame < 20; frame++) await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
    expect(root.element().matches(':hover')).toBe(true);
    expect(document.activeElement).toBe(focus === 'trigger' ? root.element() : parent.element());
    expect(parent.query()).not.toBeNull();
    await nested.hover();
    await expect.element(child).toBeVisible();
    await userEvent.keyboard('{Escape}');
    expect(child.query()).toBeNull();
    expect(parent.query()).not.toBeNull();
    expect(close).not.toHaveBeenCalled();
    await userEvent.keyboard('{Escape}');
    expect(parent.query()).toBeNull();
    expect(host.open).toBe(true);
  });
