// Component tests for ConfirmDialog — rendering + visual variant.
//
// The behavioral contract (focus routing, Esc, Enter, overlay click) is
// covered separately in `ConfirmDialog.contract.test.tsx`. This file
// pins down what callers see in the DOM: the title, description, two
// buttons with the expected labels, the warn class on Confirm when
// destructive, and that the underlying dialog is still selectable by
// `getByRole('dialog', { name: title })` (inherited from Dialog).

import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { ConfirmDialog } from './ConfirmDialog';

beforeEach(() => {
  cleanup();
  document.body.innerHTML = '';
});

describe('ConfirmDialog rendering', () => {
  it('renders the title, description, and default button labels', () => {
    render(
      <ConfirmDialog
        open
        title="Delete wave"
        description="This cannot be undone."
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );

    // Dialog accessible name comes from the title prop (inherited from
    // Dialog's `aria-label`).
    expect(screen.getByRole('dialog', { name: 'Delete wave' })).toBeTruthy();
    expect(screen.getByText('This cannot be undone.')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Confirm' })).toBeTruthy();
  });

  it('honors custom confirmLabel and cancelLabel', () => {
    render(
      <ConfirmDialog
        open
        title="Remove cove"
        confirmLabel="Yes, remove"
        cancelLabel="Keep it"
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );

    expect(screen.getByRole('button', { name: 'Yes, remove' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Keep it' })).toBeTruthy();
  });

  it('Confirm has `go warn` class by default (destructive)', () => {
    render(
      <ConfirmDialog
        open
        title="Delete wave"
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );

    const confirm = screen.getByRole('button', { name: 'Confirm' });
    expect(confirm.classList.contains('go')).toBe(true);
    expect(confirm.classList.contains('warn')).toBe(true);
  });

  it('Confirm omits `warn` class when destructive=false', () => {
    render(
      <ConfirmDialog
        open
        title="Save changes"
        destructive={false}
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );

    const confirm = screen.getByRole('button', { name: 'Confirm' });
    expect(confirm.classList.contains('go')).toBe(true);
    expect(confirm.classList.contains('warn')).toBe(false);
  });

  it('Cancel uses the outline (non-destructive) styling', () => {
    render(
      <ConfirmDialog
        open
        title="Delete wave"
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );

    const cancel = screen.getByRole('button', { name: 'Cancel' });
    expect(cancel.classList.contains('go')).toBe(true);
    expect(cancel.classList.contains('warn')).toBe(false);
  });

  it('renders nothing when closed', () => {
    render(
      <ConfirmDialog
        open={false}
        title="Delete wave"
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );

    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Confirm' })).toBeNull();
  });
});
