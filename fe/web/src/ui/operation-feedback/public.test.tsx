// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

import { useState } from '../state/public.ts';
import { OperationFeedback, useDeleteConfirm, useOperationFeedback } from './public.tsx';

afterEach(cleanup);

function Harness({ read }: { read: (reason: unknown) => string | null }) {
  const feedback = useOperationFeedback();
  const [settled, setSettled] = useState<boolean | null>(null);
  return <><button type="button" onClick={() => { void feedback.run(
    Promise.reject(new Error('Transport request failed')), read,
  ).then(setSettled); }}>Delete</button><OperationFeedback feedback={feedback} /><span>{`settled ${String(settled)}`}</span></>;
}

it('shows the caller’s reading of a rejected write, never the error’s own text', async () => {
  render(<Harness read={() => 'The delete is unconfirmed.'} />);
  await userEvent.click(screen.getByRole('button', { name: 'Delete' }));
  const alert = await screen.findByRole('alert');
  expect(within(alert).getByText('The delete is unconfirmed.')).toBeTruthy();
  expect(alert.textContent).not.toContain('Transport request failed');
  expect(screen.getByText('settled false')).toBeTruthy();
});

it('settles a rejection the caller reads as done as a success, with nothing shown', async () => {
  render(<Harness read={() => null} />);
  await userEvent.click(screen.getByRole('button', { name: 'Delete' }));
  expect(await screen.findByText('settled true')).toBeTruthy();
  expect(screen.queryByRole('alert')).toBeNull();
});

it('closes a delete the caller reads as done and runs its follow-up', async () => {
  const onDone = vi.fn();
  function DeleteHarness() {
    const confirm = useDeleteConfirm(() => Promise.reject(new Error('gone')), () => null, onDone);
    return <><button type="button" onClick={() => confirm.request('w1')}>Delete</button>
      {confirm.open && <button type="button" onClick={confirm.confirm}>Confirm</button>}<OperationFeedback feedback={confirm.feedback} /></>;
  }
  render(<DeleteHarness />);
  await userEvent.click(screen.getByRole('button', { name: 'Delete' }));
  await userEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  await waitFor(() => expect(onDone).toHaveBeenCalledOnce());
  expect(screen.queryByRole('button', { name: 'Confirm' })).toBeNull();
  expect(screen.queryByRole('alert')).toBeNull();
});

it('ignores a successful delete that arrives after cancellation', async () => {
  let resolve!: () => void;
  const onDone = vi.fn();
  function DeleteHarness() {
    const confirm = useDeleteConfirm(() => new Promise<void>((done) => { resolve = done; }), () => 'failed', onDone);
    return <><button type="button" onClick={() => confirm.request('w1')}>Delete</button>
      {confirm.open && <><button type="button" onClick={confirm.confirm}>Confirm</button><button type="button" onClick={confirm.cancel}>Cancel</button></>}</>;
  }
  render(<DeleteHarness />);
  await userEvent.click(screen.getByRole('button', { name: 'Delete' }));
  await userEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
  resolve();
  await new Promise((done) => { setTimeout(done, 10); });
  expect(onDone).not.toHaveBeenCalled();
});

it('keeps a new delete target when the cancelled request settles', async () => {
  let resolve!: () => void;
  function DeleteHarness() {
    const confirm = useDeleteConfirm(() => new Promise<void>((done) => { resolve = done; }), () => 'failed');
    return <><button type="button" onClick={() => confirm.request('w1')}>Delete first</button>
      <button type="button" onClick={() => confirm.request('w2')}>Delete second</button>
      {confirm.open && <><span>{confirm.target}</span><button type="button" onClick={confirm.confirm}>Confirm</button>
        <button type="button" onClick={confirm.cancel}>Cancel</button></>}</>;
  }
  render(<DeleteHarness />);
  await userEvent.click(screen.getByRole('button', { name: 'Delete first' }));
  await userEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
  await userEvent.click(screen.getByRole('button', { name: 'Delete second' }));
  resolve();
  await new Promise((done) => { setTimeout(done, 10); });
  expect(screen.getByText('w2')).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Confirm' })).toBeTruthy();
});

/* #2175-17: the route-level alert is the Astryx error banner: the caller's way out beside the sentence, and Dismiss clears it. */
it('offers the caller’s action and a Dismiss that clears the failure', async () => {
  const onTryAgain = vi.fn();
  function ActionHarness() {
    const feedback = useOperationFeedback();
    return <><button type="button" onClick={() => { void feedback.run(Promise.reject(new Error('x')), () => 'Not created.'); }}>Create</button>
      <OperationFeedback feedback={feedback} action={<button type="button" onClick={onTryAgain}>Try again</button>} /></>;
  }
  render(<ActionHarness />);
  await userEvent.click(screen.getByRole('button', { name: 'Create' }));
  const alert = await screen.findByRole('alert');
  expect(document.querySelectorAll('[data-nc-operation-feedback]')).toHaveLength(1);
  await userEvent.click(within(alert).getByRole('button', { name: 'Try again' }));
  expect(onTryAgain).toHaveBeenCalledOnce();
  await userEvent.click(within(alert).getByRole('button', { name: /^Dismiss/ }));
  expect(screen.queryByRole('alert')).toBeNull();
  /* Cleared, not hidden: the next failure shows again. */
  await userEvent.click(screen.getByRole('button', { name: 'Create' }));
  expect(within(await screen.findByRole('alert')).getByText('Not created.')).toBeTruthy();
});
