import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { useRef } from 'react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import { useState } from '../state/public.ts';
import { Dialog } from './public.tsx';

afterEach(cleanup);

function Disclosures() {
  const [open, setOpen] = useState(false);
  return <><button type="button" onClick={() => setOpen(true)}>Open disclosures</button>
    <Dialog open={open} title="Disclosures" onClose={() => setOpen(false)}>
      <details><summary>Evidence</summary><p>Evidence text</p></details>
      <details><summary>Snapshot</summary><p>Snapshot text</p></details>
    </Dialog>
  </>;
}

it.each([1280, 390])('cycles native summaries in both directions and restores the opener at %i', async width => {
  await page.viewport(width, 800);
  render(<Disclosures />);
  await page.getByRole('button', { name: 'Open disclosures' }).click();
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
  await userEvent.keyboard('{Tab}');
  await expect.element(page.getByText('Evidence', { exact: true })).toHaveFocus();
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByText('Evidence text', { exact: true })).toBeVisible();
  await userEvent.keyboard('{Tab}');
  await expect.element(page.getByText('Snapshot', { exact: true })).toHaveFocus();
  await userEvent.keyboard('{Tab}');
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
  await expect.element(page.getByText('Snapshot', { exact: true })).toHaveFocus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
  await expect.element(page.getByText('Evidence', { exact: true })).toHaveFocus();
  await userEvent.keyboard('{Escape}');
  await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
  await expect.element(page.getByRole('button', { name: 'Open disclosures' })).toHaveFocus();
});

it('skips closed disclosure descendants but keeps controls in the first direct summary subtree', async () => {
  await page.viewport(1280, 800);
  render(<Dialog open title="Details" onClose={vi.fn()}>
    <details>
      <p>Content before the summary</p>
      <summary>Evidence <a href="#summary">Summary reference</a></summary>
      <button type="button">Hidden button</button>
      <summary tabIndex={0}>Hidden second summary</summary>
      <details open><summary>Hidden nested summary</summary><input aria-label="Hidden field" /></details>
      <a href="#hidden">Hidden last link</a>
    </details>
  </Dialog>);
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
  await expect.element(page.getByRole('link', { name: 'Summary reference' })).toHaveFocus();
  await userEvent.keyboard('{Tab}');
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
});

function HiddenInitialTarget() {
  const target = useRef<HTMLButtonElement | null>(null);
  return <Dialog open title="Hidden target" onClose={vi.fn()} initialFocusRef={target}>
    <details><summary>Closed section</summary><button type="button" ref={target}>Hidden target</button></details>
  </Dialog>;
}

it('rejects a named initial focus target hidden by a closed details ancestor', async () => {
  render(<HiddenInitialTarget />);
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
});

it('updates traversal when nested disclosures open and close without admitting hidden inner controls', async () => {
  render(<Dialog open title="Nested details" onClose={vi.fn()}>
    <details open><summary>Outer</summary>
      <details><summary>Inner</summary><button type="button">Inner action</button></details>
    </details>
  </Dialog>);
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
  await expect.element(page.getByText('Inner', { exact: true })).toHaveFocus();
  await userEvent.keyboard('{Enter}{Tab}');
  await expect.element(page.getByRole('button', { name: 'Inner action' })).toHaveFocus();
  await userEvent.keyboard('{Tab}');
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
  await userEvent.keyboard('{Tab}');
  await expect.element(page.getByText('Outer', { exact: true })).toHaveFocus();
  await userEvent.keyboard('{Enter}{Tab}');
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
  await expect.element(page.getByText('Outer', { exact: true })).toHaveFocus();
});

it('does not promote orphan or secondary summaries, and respects an explicit tabindex opt-out', async () => {
  render(<Dialog open title="Summary semantics" onClose={vi.fn()}>
    <button type="button">Last control</button>
    <summary>Orphan summary</summary>
    <details open><summary tabIndex={-1}>Opted out</summary><summary>Secondary summary</summary></details>
  </Dialog>);
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
  await expect.element(page.getByRole('button', { name: 'Last control' })).toHaveFocus();
  await userEvent.keyboard('{Tab}');
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
});

it('retains a focusable closed details container itself while excluding its hidden content', async () => {
  render(<Dialog open title="Focusable details" onClose={vi.fn()}>
    {/* eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- Regression fixture: an explicitly focusable details container remains visible while its body is closed. */}
    <details tabIndex={0}><summary tabIndex={-1}>Container label</summary><a href="#hidden">Hidden link</a></details>
  </Dialog>);
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
  expect(document.activeElement).toBe(document.querySelector('details'));
  await userEvent.keyboard('{Tab}');
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
});

it('preserves an orphan summary made independently focusable by contenteditable', async () => {
  render(<Dialog open title="Editable content" onClose={vi.fn()}>
    <button type="button">Other control</button>
    <summary contentEditable suppressContentEditableWarning aria-label="Editable summary">Editable note</summary>
  </Dialog>);
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
  await expect.element(page.getByLabelText('Editable summary')).toHaveFocus();
  await userEvent.keyboard('{Tab}');
  await expect.element(page.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
});
