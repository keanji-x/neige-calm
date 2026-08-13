// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

import { OperationFeedback, useDeleteConfirm, useOperationFeedback } from './public.tsx';

afterEach(cleanup);

function Harness() {
  const feedback = useOperationFeedback();
  return <><button type="button" onClick={() => { void feedback.run(
    Promise.reject(new Error('Delete failed.')), 'Could not delete.',
  ); }}>Delete</button><OperationFeedback feedback={feedback} /></>;
}

it('turns a rejected write into one handled, user-visible feedback channel', async () => {
  render(<Harness />);
  await userEvent.click(screen.getByRole('button', { name: 'Delete' }));
  expect((await screen.findByRole('alert')).textContent).toContain('Delete failed.');
});

it('ignores a successful delete that arrives after cancellation', async () => {
  let resolve!: () => void;
  const onDone = vi.fn();
  function DeleteHarness() {
    const confirm = useDeleteConfirm(() => new Promise<void>((done) => { resolve = done; }), onDone);
    return <><button type="button" onClick={() => confirm.request('w1')}>Delete</button>
      {confirm.open && <><button type="button" onClick={confirm.confirm}>Confirm</button><button type="button" onClick={confirm.cancel}>Cancel</button></>}</>;
  }
  render(<DeleteHarness />);
  await userEvent.click(screen.getByRole('button', { name: 'Delete' }));
  await userEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
  resolve();
  await Promise.resolve();
  expect(onDone).not.toHaveBeenCalled();
});
