// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove } from '../../../../../core/domain/cove.ts';
import { NewWaveForm } from './public.tsx';

afterEach(cleanup);

const COVES: readonly Cove[] = [
  { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0 },
  { id: 'c2', name: 'Home', color: '#8B7FE8', sort: 2, kind: 'user', createdAt: 0, updatedAt: 0 },
];

function renderForm(overrides: Partial<Parameters<typeof NewWaveForm>[0]> = {}) {
  const props = {
    coves: COVES,
    defaultCoveId: 'c1',
    submitting: false,
    error: null,
    onCancel: vi.fn(),
    onSubmit: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<NewWaveForm {...props} />) };
}

function submitButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: /Create wave|Creating/ });
}

describe('NewWaveForm validation', () => {
  it('keeps submit disabled while the working directory is empty', async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText('Task'), 'Ship the thing');
    expect(submitButton().disabled).toBe(true);
  });

  it('keeps submit disabled for a relative path and explains why', async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText('Task'), 'Ship the thing');
    await userEvent.type(screen.getByLabelText('Working directory'), 'tmp/x');
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByText(/absolute path/)).toBeTruthy();
  });

  it('enables submit for an absolute path', async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText('Task'), 'Ship the thing');
    await userEvent.type(screen.getByLabelText('Working directory'), '/tmp/x');
    expect(submitButton().disabled).toBe(false);
  });
});

describe('NewWaveForm submission', () => {
  it('hands the caller a trimmed draft with the selected cove', async () => {
    const { props } = renderForm();
    await userEvent.type(screen.getByLabelText('Task'), '  Ship the thing  ');
    await userEvent.type(screen.getByLabelText('Working directory'), '/tmp/x ');
    await userEvent.selectOptions(screen.getByLabelText('Cove'), 'c2');
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({
      coveId: 'c2', title: 'Ship the thing', cwd: '/tmp/x', attachFolder: false,
    });
  });

  it('defaults the cove select to defaultCoveId', () => {
    renderForm({ defaultCoveId: 'c2' });
    expect(screen.getByLabelText('Cove')).toHaveProperty('value', 'c2');
  });

  it('never claims the folder unless the user asks', async () => {
    const { props } = renderForm();
    await userEvent.type(screen.getByLabelText('Task'), 'Ship it');
    await userEvent.type(screen.getByLabelText('Working directory'), '/tmp/x');
    expect(screen.getByLabelText('Claim this folder for the cove')).toHaveProperty('checked', false);
    await userEvent.click(screen.getByLabelText('Claim this folder for the cove'));
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith(expect.objectContaining({ attachFolder: true }));
  });

  it('names the 409 conflict in the checkbox help text', () => {
    renderForm();
    expect(screen.getByText(/409/)).toBeTruthy();
  });

  it('flips the label and blocks submit while submitting', () => {
    renderForm({ submitting: true });
    expect(screen.getByRole('button', { name: 'Creating…' })).toHaveProperty('disabled', true);
  });

  it('surfaces the caller error in an alert region', () => {
    renderForm({ error: 'Directory already claimed by cove Home' });
    expect(screen.getByRole('alert').textContent).toContain('already claimed');
  });

  it('cancels without submitting', async () => {
    const { props } = renderForm();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(props.onCancel).toHaveBeenCalledTimes(1);
    expect(props.onSubmit).not.toHaveBeenCalled();
  });
});
