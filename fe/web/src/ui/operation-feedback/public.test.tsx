// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';

import { OperationFeedback, useOperationFeedback } from './public.tsx';

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
