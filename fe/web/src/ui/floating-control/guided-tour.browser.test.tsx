import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { useState } from '../state/public.ts';
import { Dialog } from '../dialog/public.tsx';
import { AnchoredHint } from './anchored-hint.tsx';
import { GuideAnchor, GuideProvider } from './guided-tour.tsx';
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

it('pauses background tours after completing an exclusive foreground form', async () => {
  await page.viewport(1000, 800); render(<Fixture />);
  await page.getByRole('button', { name: 'Open native form' }).click();
  await page.getByRole('button', { name: '知道了', exact: true }).click();
  await expect.element(page.getByRole('note')).not.toBeInTheDocument();
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
