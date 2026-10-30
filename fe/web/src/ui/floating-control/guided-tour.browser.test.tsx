import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, commands, userEvent } from 'vitest/browser';
import { useState } from '../state/public.ts';
import { Dialog } from '../dialog/public.tsx';
import { AnchoredHint } from './anchored-hint.tsx';
import { GuideAnchor, GuideProvider, guideRevealSlot } from './guided-tour.tsx';
import { parseGuideCatalog } from './guide-catalog.ts';

afterEach(cleanup);

function Fixture() {
  const [enabled, enable] = useState(true);
  const [seen, remember] = useState(() => new Set<string>());
  const [modal, showModal] = useState(false);
  const [catalog] = useState(() => parseGuideCatalog({ version: 1, tours: [
    { id: 'page', title: 'Page', priority: 1, exclusive: false, requires: [], steps: [
      { id: 'one', target: 'one', title: 'First action', description: 'The native button still opens a dialog.', advance: 'activate' },
      { id: 'two', target: 'two', title: 'Second group', description: 'Related actions share one hint.', advance: 'next' },
    ] },
    { id: 'other', title: 'Other', priority: 0, exclusive: false, requires: [], steps: [
      { id: 'other', target: 'other', title: 'Another visible area', description: 'Unseen help continues after completed scenes.', advance: 'next' },
    ] },
    { id: 'modal', title: 'Form', priority: 2, exclusive: true, requires: [], steps: [
      { id: 'form', target: 'form', title: 'Native form', description: 'Drafts stay with the form.', advance: 'next' },
    ] },
  ] }));
  return <GuideProvider catalog={catalog} enabled={enabled} sessionKey="fixture" facts={{}} isSeen={id => seen.has(id)}
    onSeen={id => remember(current => new Set([...current, id]))} onSkip={() => enable(false)}>
    <GuideAnchor id="one">{(anchor, describedBy) => <button ref={anchor} aria-describedby={describedBy} onClick={() => showModal(true)}>Open native form</button>}</GuideAnchor>
    <GuideAnchor id="two">{(anchor, describedBy) => <div ref={anchor} aria-describedby={describedBy} tabIndex={-1}><button>Related A</button><button>Related B</button></div>}</GuideAnchor>
    <GuideAnchor id="other">{(anchor, describedBy) => <button ref={anchor} aria-describedby={describedBy}>Another area</button>}</GuideAnchor>
    <Dialog origin={{ kind: 'surface-center' }} open={modal} title="Native dialog" onClose={() => showModal(false)}>
      <GuideAnchor id="form" reserveSpace>{(anchor, describedBy) => <input ref={anchor} aria-describedby={describedBy} aria-label="Draft" />}</GuideAnchor>
      <button onClick={() => showModal(false)}>Close native form</button>
    </Dialog>
  </GuideProvider>;
}

it('follows native actions, yields to dialogs, resumes unseen groups and skips without losing drafts', async () => {
  await page.viewport(1000, 800);
  render(<Fixture />);
  await expect.element(page.getByRole('note', { name: 'First action' })).toBeVisible();
  await page.getByRole('button', { name: 'Open native form' }).click();
  await expect.element(page.getByRole('note', { name: 'Native form' })).toBeVisible();
  await expect.element(page.getByRole('note', { name: 'Second group' })).not.toBeInTheDocument();
  await page.getByRole('textbox', { name: 'Draft' }).fill('Keep my words');
  await page.getByRole('button', { name: '跳过引导' }).click();
  await expect.element(page.getByRole('note')).not.toBeInTheDocument();
  await expect.element(page.getByRole('textbox', { name: 'Draft' })).toHaveValue('Keep my words');
  await page.getByRole('button', { name: 'Close native form' }).click();
  await expect.element(page.getByRole('button', { name: 'Related A' })).toBeVisible();
});

it('supports previous and next without invoking product actions', async () => {
  await page.viewport(1000, 800);
  render(<Fixture />);
  await page.getByRole('button', { name: '略过这一步', exact: true }).click();
  await expect.element(page.getByRole('note', { name: 'Second group' })).toBeVisible();
  await page.getByRole('button', { name: '上一步', exact: true }).click();
  await expect.element(page.getByRole('note', { name: 'First action' })).toBeVisible();
  await page.getByRole('button', { name: '略过这一步', exact: true }).click();
  await expect.element(page.getByRole('note', { name: 'Second group' })).toBeVisible();
  await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
});


it('continues to lower priority unseen tours after ordinary steps are finished', async () => {
  await page.viewport(1000, 800); render(<Fixture />);
  await page.getByRole('button', { name: '略过这一步', exact: true }).click();
  await page.getByRole('button', { name: '知道了', exact: true }).click();
  await expect.element(page.getByRole('note', { name: 'Another visible area' })).toBeVisible();
});

it('offers pending background help after completing an exclusive foreground form', async () => {
  await page.viewport(1000, 800); render(<Fixture />);
  await page.getByRole('button', { name: 'Open native form' }).click();
  await page.getByRole('button', { name: '知道了', exact: true }).click();
  await expect.element(page.getByRole('status', { name: '引导下一步' })).toBeVisible();
  await page.getByRole('button', { name: 'Close native form' }).click();
  await expect.element(page.getByRole('note', { name: 'Second group' })).toBeVisible();
});


it('withdraws semantics immediately and retains the painted explanation until exit completes', async () => {
  await page.viewport(1000, 800);
  const hint = (open: boolean) => <AnchoredHint open={open} title={open ? 'Retained explanation' : ''}
    description={open ? 'Keep this explanation during exit.' : ''} onSkip={() => {}}>
    {(anchor, describedBy) => <button ref={anchor} aria-describedby={describedBy}>Native action</button>}
  </AnchoredHint>;
  const view = render(hint(true));
  await expect.element(page.getByRole('note', { name: 'Retained explanation' })).toBeVisible();
  const positioner = page.getByRole('note', { name: 'Retained explanation' }).element();
  await expect.poll(() => positioner.firstElementChild!.getAnimations().every(animation => animation.playState === 'finished')).toBe(true);
  view.rerender(hint(false));
  expect(positioner.hasAttribute('inert')).toBe(true);
  expect(positioner.getAttribute('aria-hidden')).toBe('true');
  expect(positioner.textContent).toContain('Keep this explanation during exit.');
  await expect.element(page.getByRole('note')).not.toBeInTheDocument();
  await expect.poll(() => positioner.isConnected).toBe(false);
  await expect.element(page.getByRole('button', { name: 'Native action' })).toBeVisible();
});

function EngineFixture({ hidden = false, slot = false, withdraw = false, keyboard = false }: { hidden?: boolean; slot?: boolean; withdraw?: boolean; keyboard?: boolean }) {
  const [seen, remember] = useState(() => new Set<string>());
  const [mounted, mount] = useState(true);
  const [ready, setReady] = useState(false);
  const [enabled, enable] = useState(true);
  const catalog = parseGuideCatalog({ version: 1, tours: [{ id: 'engine', title: 'Engine', priority: 1, exclusive: true, requires: [], steps: [
    { id: 'first', target: ['wide', 'compact'], title: 'First', description: 'Do the action', advance: 'activate', locate: 'Open the panel', via: ['opener'] },
    { id: 'second', target: 'second', title: 'Second', description: 'Finish here', advance: 'next' },
  ] }] });
  return <GuideProvider catalog={catalog} enabled={enabled} sessionKey="engine" facts={{}} isSeen={id => seen.has(id)} onSeen={id => remember(value => new Set([...value, id]))} onSkip={() => enable(false)}>
    <button onClick={() => setReady(true)}>Ready</button>
    <button onClick={() => mount(value => !value)}>Toggle target</button>
    <GuideAnchor id="opener">{(ref, describedBy) => <button ref={ref} aria-describedby={describedBy}>Panel</button>}</GuideAnchor>
    {mounted && <span {...(slot ? guideRevealSlot : {})} style={{ opacity: hidden ? 0 : 1 }}>
      <GuideAnchor id="compact">{(ref, describedBy) => <button ref={ref} aria-describedby={describedBy}
        onPointerDown={() => { if (withdraw && !keyboard) mount(false); }}
        onKeyDown={() => { if (withdraw && keyboard) mount(false); }}>Target</button>}</GuideAnchor>
    </span>}
    <GuideAnchor id="second" disabled={!ready}>{(ref, describedBy) => <button ref={ref} aria-describedby={describedBy}>Finish</button>}</GuideAnchor>
  </GuideProvider>;
}

it('counts disabled anchors and variants once and advances after readiness changes', async () => {
  render(<EngineFixture />);
  await expect.element(page.getByRole('note', { name: 'First' })).toHaveTextContent('1 / 2');
  await page.getByRole('button', { name: 'Ready', exact: true }).click();
  await expect.element(page.getByRole('note', { name: 'First' })).toHaveTextContent('1 / 2');
  await page.getByRole('button', { name: 'Target', exact: true }).click();
  await expect.element(page.getByRole('note', { name: 'Second' })).toHaveTextContent('2 / 2');
});

it('reveals declared slots and rejects effectively transparent targets outside them', async () => {
  const view = render(<EngineFixture hidden />);
  await expect.element(page.getByRole('note', { name: 'First' })).toHaveTextContent('Open the panel');
  expect(page.getByRole('button', { name: 'Target', exact: true }).element().hasAttribute('data-nc-guide-target')).toBe(false);
  view.rerender(<EngineFixture hidden slot />);
  await expect.poll(() => getComputedStyle(page.getByRole('button', { name: 'Target', exact: true }).element().parentElement!).opacity).toBe('1');
  await expect.element(page.getByRole('note', { name: 'First' })).toHaveTextContent('Do the action');
});

for (const keyboard of [false, true]) it(`advances when activation withdraws the anchor on ${keyboard ? 'keydown' : 'pointerdown'}`, async () => {
  render(<EngineFixture withdraw keyboard={keyboard} />);
  await page.getByRole('button', { name: 'Ready', exact: true }).click();
  const target = page.getByRole('button', { name: 'Target', exact: true }).element();
  target.dispatchEvent(keyboard ? new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }) : new PointerEvent('pointerdown', { bubbles: true }));
  await expect.element(page.getByRole('note', { name: 'Second' })).toBeVisible();
});

it('uses via copy until the real target mounts and falls back to a skippable status', async () => {
  render(<EngineFixture />);
  await page.getByRole('button', { name: 'Toggle target' }).click();
  await expect.element(page.getByRole('note', { name: 'First' })).toHaveTextContent('Open the panel');
  await page.getByRole('button', { name: 'Toggle target' }).click();
  await expect.element(page.getByRole('note', { name: 'First' })).toHaveTextContent('Do the action');
  await page.getByRole('button', { name: 'Target', exact: true }).click();
  await expect.element(page.getByRole('status', { name: '引导下一步' })).toHaveTextContent('Second');
  await page.getByRole('button', { name: '跳过引导', exact: true }).click();
  await expect.element(page.getByRole('status', { name: '引导下一步' })).not.toBeInTheDocument();
});

it('tracks target geometry in a noninteractive top-layer focus ring and scrolls both axes into view', async () => {
  await page.viewport(600, 500);
  render(<div style={{ width: 250, height: 150, overflow: 'auto' }}><div style={{ width: 900, height: 800, paddingTop: 500, paddingLeft: 600 }}><EngineFixture /></div></div>);
  await expect.element(page.getByRole('note', { name: 'First' })).toBeVisible();
  const target = page.getByRole('button', { name: 'Target', exact: true }).element();
  const ring = () => document.querySelector<HTMLElement>('[data-nc-guide-ring]:popover-open')!;
  await expect.poll(() => Math.abs(ring().getBoundingClientRect().x - target.getBoundingClientRect().x)).toBeLessThan(1);
  expect(ring().getAttribute('aria-hidden')).toBe('true');
  expect(getComputedStyle(ring()).pointerEvents).toBe('none');
  const outline = getComputedStyle(ring().firstElementChild!);
  expect(outline.outlineWidth).toBe('2px');
  expect(outline.outlineOffset).toBe('2px');
  const scroller = target.closest('div')!.parentElement!;
  expect(scroller.scrollLeft).toBeGreaterThan(0);
  expect(scroller.scrollTop).toBeGreaterThan(0);
  scroller.scrollTop -= 10;
  target.style.width = '100px';
  await expect.poll(() => Math.abs(ring().getBoundingClientRect().width - target.getBoundingClientRect().width)).toBeLessThan(1);
  await expect.poll(() => Math.abs(ring().getBoundingClientRect().y - target.getBoundingClientRect().y)).toBeLessThan(1);
  scroller.scrollTop = 0;
  await expect.poll(() => getComputedStyle(ring()).visibility).toBe('hidden');
});

it('does not animate the highlight under reduced motion', async () => {
  await commands.emulateReducedMotion(true);
  try {
    render(<EngineFixture />);
    await expect.element(page.getByRole('note', { name: 'First' })).toBeVisible();
    const ring = document.querySelector('[data-nc-guide-ring]')!;
    expect(ring.getAnimations({ subtree: true })).toHaveLength(0);
  } finally { await commands.emulateReducedMotion(false); }
});

it('continues to a usable lower tour while a completed exclusive target remains mounted', async () => {
  function ExclusiveFixture() {
    const [seen, remember] = useState(() => new Set<string>());
    const catalog = parseGuideCatalog({ version: 1, tours: [
      { id: 'front', title: 'Front', priority: 2, exclusive: true, requires: [], steps: [{ id: 'front', target: 'front', title: 'Foreground', description: 'Read', advance: 'next' }] },
      { id: 'back', title: 'Back', priority: 1, exclusive: false, requires: [], steps: [{ id: 'back', target: 'back', title: 'Background', description: 'Continue', advance: 'next' }] },
    ] });
    return <GuideProvider catalog={catalog} enabled sessionKey="exclusive" facts={{}} isSeen={id => seen.has(id)} onSeen={id => remember(value => new Set([...value, id]))} onSkip={() => {}}>
      <GuideAnchor id="front">{ref => <button ref={ref}>Foreground target</button>}</GuideAnchor>
      <GuideAnchor id="back">{ref => <button ref={ref}>Background target</button>}</GuideAnchor>
    </GuideProvider>;
  }
  render(<ExclusiveFixture />);
  await expect.element(page.getByRole('note', { name: 'Foreground' })).toBeVisible();
  await page.getByRole('button', { name: '知道了', exact: true }).click();
  await expect.element(page.getByRole('note', { name: 'Background' })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Foreground target' })).toBeVisible();
});

for (const role of ['menu', 'listbox'] as const) for (const via of [false, true]) it(`yields ${via ? 'via' : 'status'} location help to a visible ${role}`, async () => {
  const catalog = parseGuideCatalog({ version: 1, tours: [{ id: 'pending', title: 'Pending', priority: 1, exclusive: false, requires: [], steps: [
    { id: 'pending', target: 'absent', via: ['opener'], locate: 'Open the panel', title: 'Pending action', description: 'Read', advance: 'next' },
  ] }] });
  const fixture = (open: boolean) => <GuideProvider catalog={catalog} enabled sessionKey="attention" facts={{}} isSeen={() => false} onSeen={() => {}} onSkip={() => {}}>
    {via && <GuideAnchor id="opener">{(ref, describedBy) => <button ref={ref} aria-describedby={describedBy}>Open panel</button>}</GuideAnchor>}
    {open && <div role={role} aria-label="Native picker">Native options</div>}
  </GuideProvider>;
  const instruction = via ? page.getByRole('note', { name: 'Pending action' }) : page.getByRole('status', { name: '引导下一步' });
  const view = render(fixture(false));
  await expect.element(instruction).toBeVisible();
  view.rerender(fixture(true));
  await expect.element(page.getByRole(role, { name: 'Native picker' })).toBeVisible();
  await expect.element(page.getByRole('status', { name: '引导下一步' })).not.toBeInTheDocument();
  await expect.element(page.getByRole('note', { name: 'Pending action' })).not.toBeInTheDocument();
  view.rerender(fixture(false));
  await expect.element(instruction).toBeVisible();
});

it('does not invent a location for an unrelated unmounted tour', async () => {
  const catalog = parseGuideCatalog({ version: 1, tours: [{ id: 'absent', title: 'Absent', priority: 1, exclusive: false, requires: [], steps: [
    { id: 'absent', target: 'absent', title: 'Unrelated', description: 'Read', advance: 'next' },
  ] }] });
  render(<GuideProvider catalog={catalog} enabled sessionKey="absent" facts={{}} isSeen={() => false} onSeen={() => {}} onSkip={() => {}}><button>Product control</button></GuideProvider>);
  await expect.element(page.getByRole('button', { name: 'Product control' })).toBeVisible();
  await expect.element(page.getByRole('status', { name: '引导下一步' })).not.toBeInTheDocument();
});

for (const surface of ['status', 'anchored'] as const) it(`lets pointer input through ${surface} copy while keeping Skip pointer and keyboard reachable`, async () => {
  await page.viewport(1000, 800);
  const catalog = parseGuideCatalog({ version: 1, tours: [{ id: 'pending', title: 'Pending', priority: 1, exclusive: false, requires: [], steps: [
    { id: 'pending', target: surface === 'status' ? 'absent' : 'product', locate: 'Open the panel', title: 'Pending', description: 'Read', advance: 'next' },
  ] }] });
  function StatusFixture() {
    const [enabled, enable] = useState(true);
    return <GuideProvider catalog={catalog} enabled={enabled} sessionKey="pointer" facts={{}} isSeen={() => false} onSeen={() => {}} onSkip={() => enable(false)}>
      <button style={{ position: 'fixed', inset: 0 }}>Underlying product</button>
      <GuideAnchor id="product">{(ref, describedBy) => <button ref={ref} aria-describedby={describedBy} style={{ position: 'fixed', top: 10, left: 10 }}>Anchor</button>}</GuideAnchor>
    </GuideProvider>;
  }
  render(<StatusFixture />);
  const status = surface === 'status' ? page.getByRole('status', { name: '引导下一步' }) : page.getByRole('note', { name: 'Pending' });
  await expect.element(status).toBeVisible();
  const copy = status.element().querySelector('p')!.getBoundingClientRect();
  const product = page.getByRole('button', { name: 'Underlying product' }).element();
  expect(product.contains(document.elementFromPoint(copy.x + copy.width / 2, copy.y + copy.height / 2))).toBe(true);
  const skip = page.getByRole('button', { name: '跳过引导' }).element();
  const bounds = skip.getBoundingClientRect();
  expect(skip.contains(document.elementFromPoint(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2))).toBe(true);
  page.getByRole('button', { name: 'Anchor', exact: true }).element().focus();
  await userEvent.keyboard('{Tab}');
  expect(document.activeElement).toBe(skip);
  await userEvent.keyboard('{Enter}');
  await expect.element(status).not.toBeInTheDocument();
});

it('prefers a mounted pending step over an undeclared unmounted step in a started tour', async () => {
  const catalog = parseGuideCatalog({ version: 1, tours: [{ id: 'started', title: 'Started', priority: 1, exclusive: false, requires: [], steps: [
    { id: 'read', target: 'read', title: 'Already read', description: 'Read', advance: 'next' },
    { id: 'absent', target: 'absent', title: 'Unavailable preview', description: 'Read', advance: 'next' },
    { id: 'mounted', target: 'mounted', title: 'Visible progress', description: 'Read', advance: 'next' },
  ] }] });
  render(<GuideProvider catalog={catalog} enabled sessionKey="started" facts={{}} isSeen={id => id === 'read'} onSeen={() => {}} onSkip={() => {}}>
    <GuideAnchor id="mounted">{(ref, describedBy) => <button ref={ref} aria-describedby={describedBy}>Progress</button>}</GuideAnchor>
  </GuideProvider>);
  await expect.element(page.getByRole('note', { name: 'Visible progress' })).toBeVisible();
  await expect.element(page.getByRole('status', { name: '引导下一步' })).not.toBeInTheDocument();
});
