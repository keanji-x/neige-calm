// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { NewWaveForm } from './public.tsx';

afterEach(cleanup);

function renderForm(overrides: Partial<Parameters<typeof NewWaveForm>[0]> = {}) {
  const props = {
    submitting: false,
    error: null,
    titleRef: { current: null },
    onCancel: vi.fn(),
    onSubmit: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<NewWaveForm {...props} />) };
}

function submitButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: /Create wave|Creating/ });
}

describe('NewWaveForm is title-only', () => {
  it('keeps submit disabled while the task is empty', () => {
    renderForm();
    expect(submitButton().disabled).toBe(true);
  });

  it('enables submit after typing a title', async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText('Task'), 'Ship the thing');
    expect(submitButton().disabled).toBe(false);
  });

  it('calls onSubmit with the trimmed title', async () => {
    const { props } = renderForm();
    await userEvent.type(screen.getByLabelText('Task'), '  Ship the thing  ');
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({ title: 'Ship the thing' });
  });

  it('asks for the task only — no folder, cove, or claim controls', async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText('Task'), 'Ship the thing');
    expect(screen.queryByLabelText('Folder')).toBeNull();
    expect(screen.queryByLabelText('Cove')).toBeNull();
    expect(screen.queryByLabelText(/Working directory/i)).toBeNull();
    expect(screen.queryByLabelText(/Claim this folder/)).toBeNull();
    expect(screen.queryByRole('checkbox')).toBeNull();
    expect(screen.queryByRole('combobox')).toBeNull();
  });

  /*
   * Single line, not a textarea. The value becomes the wave's `title`, and
   * every other surface renders it as one truncated line — sidebar, wave list,
   * page header — while the wave page edits it through the single-line
   * `EditableTitle`. `getByRole('textbox')` is true of both elements, so this
   * asserts the tag itself; anything weaker would stay green on a textarea.
   */
  it('asks for the task on one line, in the form\'s own sans', () => {
    renderForm();
    const task = screen.getByLabelText('Task');
    expect(task).toHaveProperty('tagName', 'INPUT');
    expect(task).toHaveProperty('type', 'text');
  });

  it('flips the label and blocks submit while submitting', () => {
    renderForm({ submitting: true });
    expect(screen.getByRole('button', { name: 'Creating…' })).toHaveProperty('disabled', true);
  });

  it('surfaces the caller error in an alert region', () => {
    renderForm({ error: 'Could not create the wave.' });
    expect(screen.getByRole('alert').textContent).toContain('Could not create');
  });

  it('cancels without submitting', async () => {
    const { props } = renderForm();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(props.onCancel).toHaveBeenCalledTimes(1);
    expect(props.onSubmit).not.toHaveBeenCalled();
  });
});
