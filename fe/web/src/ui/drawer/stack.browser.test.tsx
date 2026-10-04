import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import '../../styles/entry.css';
import { Drawer } from './public.tsx';
import { useState } from '../state/public.ts';
import { useCompactViewport } from '../viewport/public.ts';

afterEach(async () => { document.body.replaceChildren(); await page.viewport(1280, 720); });

function Harness() {
  const [side, setSide] = useState(false);
  const compact = useCompactViewport();
  return <div style={{ position: 'relative', height: '900px', width: '100%', containerType: 'inline-size' }}>
    <Drawer open stacked title="Main conversation" onClose={() => {}}
      footer={<textarea aria-label="Main message" />}
      companion={side && !compact ? (group) => <Drawer resizeGroup={group} inline open title="Side conversation" closeLabel="Close side conversation"
        onClose={() => setSide(false)} footer={<textarea aria-label="Side message" />}>
        <p>Independent discussion</p>
      </Drawer> : undefined}>
      {!compact && <button type="button" onClick={() => setSide(true)}>Open discussion</button>}
      <p>Current work continues here</p>
    </Drawer>
  </div>;
}

it('stacks independent cards and preserves the parent DOM and draft across opening and closing', async () => {
  await page.viewport(1400, 950);
  const view = render(<Harness />);
  try {
    const original = await page.getByRole('textbox', { name: 'Main message' }).findElement();
    await page.getByRole('textbox', { name: 'Main message' }).fill('Unsent main words');
    await page.getByRole('button', { name: 'Open discussion' }).click();
    const cards = document.querySelectorAll<HTMLElement>('[data-nc-drawer]');
    const main = cards[0].getBoundingClientRect();
    const side = cards[1].getBoundingClientRect();
    expect(side.top).toBeGreaterThan(main.bottom);
    expect(side.left).toBe(main.left);
    expect(side.width).toBe(main.width);
    expect(main.height).toBeGreaterThan(300);
    expect(side.height).toBeGreaterThan(300);
    expect(await page.getByRole('textbox', { name: 'Main message' }).findElement()).toBe(original);
    await page.getByRole('textbox', { name: 'Side message' }).fill('Unsent side words');
    expect((original as HTMLTextAreaElement).value).toBe('Unsent main words');
    await page.screenshot({ path: '../../../../test-results/side-conversation-desktop.png' });
    await page.getByRole('button', { name: 'Close side conversation' }).click();
    await expect.poll(() => document.querySelectorAll('[data-nc-drawer]').length).toBe(1);
    expect(await page.getByRole('textbox', { name: 'Main message' }).findElement()).toBe(original);
  } finally { view.unmount(); }
});

it('keeps the ordinary single-card layout on compact screens', async () => {
  await page.viewport(390, 844);
  const view = render(<Harness />);
  try {
    const card = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    expect(document.querySelectorAll('[data-nc-drawer]')).toHaveLength(1);
    expect(document.querySelector('[aria-label="Conversation view"]')).toBeNull();
    expect(card.getBoundingClientRect().width).toBe(390);
    expect(card.getBoundingClientRect().bottom).toBeLessThanOrEqual(844);
  } finally { view.unmount(); }
});
