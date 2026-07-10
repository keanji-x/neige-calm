// Tests for the keyboard-entry rename path on CovePage (slice 3 of #56).
//
// Mirrors `Wave.test.tsx`: the EditableTitle in CovePage shares the same
// keyboard contract (Enter / F2 → edit; Escape / Enter → exit + focus
// restore) but renders as a styled <h1> instead of a plain span.

import { describe, it, expect, vi } from 'vitest';
import { render, screen, act, fireEvent, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import * as api from '../api/calm';
import { CovePage } from './Cove';
import type { Cove, Wave } from '../types';

function makeCove(): Cove {
  return { id: 'c1', name: 'Atlas', subtitle: '', color: '#5a9' };
}

function makeWave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1',
    coveId: 'c1',
    title: 'Migrate auth',
    lifecycle: 'draft',
    anyCardNeedsInput: false,
    progress: 0,
    eta: '',
    now: '',
    // Issue #250 PR 5 — required by the calendar-rail integration; the
    // CovePage tests don't read these but the type-level requirement is
    // load-bearing for spotting forgotten fields in production code.
    createdAt: 0,
    terminalAt: null,
    pinnedAt: null,
    cards: [],
    ...overrides,
  };
}

describe('CovePage EditableTitle keyboard entry', () => {
  it('renders the cove title as a focusable button named after the cove', () => {
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onRenameCove={() => {}}
      />,
    );
    // Rendered as an intrinsic <button> nested inside an <h1> so heading
    // semantics survive — no explicit tabindex needed (buttons are
    // focusable by default).
    const title = screen.getByRole('button', { name: 'Atlas' });
    expect(title.tagName).toBe('BUTTON');
    // The wrapping h1 should still be discoverable by heading nav.
    // After #56 followup, its accessible name is just "Atlas." (the
    // visible text, with the period the parent prints) — no "Rename cove
    // name:" prefix, so heading-nav narration is clean. The sr-only
    // helper sits *outside* the <h1> so it doesn't pollute the heading's
    // name-from-content computation.
    expect(screen.getByRole('heading', { level: 1, name: 'Atlas.' })).toContainElement(title);
    // The rename verb is conveyed as an aria-describedby helper on the
    // inner button, not as part of its name.
    expect(title).toHaveAccessibleDescription('Rename cove name');
  });

  it('falls back to a plain h1 when onRenameCove is absent', () => {
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
      />,
    );
    // Heading exists but is not interactive — no button inside the title.
    expect(screen.queryByRole('button', { name: 'Atlas' })).toBeNull();
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Atlas.');
  });

  it('Enter on the title opens rename mode and focuses the input', async () => {
    const user = userEvent.setup();
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onRenameCove={() => {}}
      />,
    );
    const title = screen.getByRole('button', { name: 'Atlas' });
    title.focus();
    await user.keyboard('{Enter}');
    const input = screen.getByRole('textbox', { name: 'Cove name' });
    expect(input).toBeInTheDocument();
    expect(document.activeElement).toBe(input);
  });

  it('F2 on the title opens rename mode', async () => {
    const user = userEvent.setup();
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onRenameCove={() => {}}
      />,
    );
    const title = screen.getByRole('button', { name: 'Atlas' });
    title.focus();
    await user.keyboard('{F2}');
    expect(screen.getByRole('textbox', { name: 'Cove name' })).toBeInTheDocument();
  });

  it('Escape exits rename mode and restores focus to the title', async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onRenameCove={onRename}
      />,
    );
    const title = screen.getByRole('button', { name: 'Atlas' });
    title.focus();
    await user.keyboard('{Enter}');
    const input = screen.getByRole('textbox', { name: 'Cove name' });
    await user.type(input, ' edits');
    await user.keyboard('{Escape}');

    expect(screen.queryByRole('textbox', { name: 'Cove name' })).not.toBeInTheDocument();
    expect(onRename).not.toHaveBeenCalled();
    const restored = screen.getByRole('button', { name: 'Atlas' });
    expect(document.activeElement).toBe(restored);
  });

  it('Enter commits a renamed value and restores focus to the title display', async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onRenameCove={onRename}
      />,
    );
    const title = screen.getByRole('button', { name: 'Atlas' });
    title.focus();
    await user.keyboard('{Enter}');
    const input = screen.getByRole('textbox', { name: 'Cove name' });
    // Change the value via fireEvent so we don't depend on userEvent
    // simulating the controlled-input lifecycle around an immediate
    // re-render on Enter, then dispatch the Enter key directly on the
    // input — the same path the production onKeyDown handles.
    fireEvent.change(input, { target: { value: 'Beacon' } });
    fireEvent.keyDown(input, { key: 'Enter' });

    // useEffect-driven focus restore happens after the render flush.
    await act(async () => {
      await Promise.resolve();
    });

    expect(onRename).toHaveBeenCalledWith('c1', 'Beacon');
    const restored = screen.getByRole('button', { name: 'Atlas' });
    expect(document.activeElement).toBe(restored);
  });
});

// ============================================================
// ConfirmDialog adoption tests (#60 followup).
//
// These tests pin down the migration from window.confirm() to the
// <ConfirmDialog> primitive for the two destructive flows on this page:
//   - Cove × button (DeleteButton) → onDeleteCove. Pattern A: dialog
//     stays open while the async delete is in flight, Confirm is
//     disabled mid-await.
//   - Per-row × on a WaveRow → onDeleteWave. Pattern B: dialog closes
//     on Confirm, parent's promise resolves out-of-band.
//
// We deliberately don't re-test Cancel-safe default focus, Esc routing,
// or overlay-click here — that's locked in
// `ui/ConfirmDialog/ConfirmDialog.contract.test.tsx` and is the same
// implementation under the hood.
// ============================================================

describe('CovePage delete-cove ConfirmDialog (Pattern A)', () => {
  it('clicking the × opens a ConfirmDialog with the cove name in the body', async () => {
    const user = userEvent.setup();
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onDeleteCove={() => {}}
      />,
    );
    // Dialog is not open yet — the trigger button is the only delete
    // affordance present.
    expect(screen.queryByRole('dialog', { name: 'Delete cove?' })).toBeNull();
    await user.click(screen.getByRole('button', { name: 'Delete cove "Atlas"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete cove?' });
    expect(dialog).toBeInTheDocument();
    expect(dialog).toHaveTextContent('Delete cove "Atlas"?');
    expect(within(dialog).getByRole('button', { name: 'Delete cove' })).toBeInTheDocument();
    expect(within(dialog).getByRole('button', { name: 'Cancel' })).toBeInTheDocument();
  });

  it('Cancel closes the dialog without invoking onDeleteCove', async () => {
    const user = userEvent.setup();
    const onDeleteCove = vi.fn();
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onDeleteCove={onDeleteCove}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Delete cove "Atlas"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete cove?' });
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog', { name: 'Delete cove?' })).toBeNull();
    expect(onDeleteCove).not.toHaveBeenCalled();
  });

  it('Confirm fires onDeleteCove exactly once and closes the dialog', async () => {
    const user = userEvent.setup();
    const onDeleteCove = vi.fn().mockResolvedValue(undefined);
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onDeleteCove={onDeleteCove}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Delete cove "Atlas"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete cove?' });
    await user.click(within(dialog).getByRole('button', { name: 'Delete cove' }));
    expect(onDeleteCove).toHaveBeenCalledTimes(1);
    expect(onDeleteCove).toHaveBeenCalledWith('c1');
    // Resolves with undefined immediately; DeleteButton closes the
    // dialog in its `finally` block after the await resolves.
    expect(screen.queryByRole('dialog', { name: 'Delete cove?' })).toBeNull();
  });

  it('Confirm is disabled while onDeleteCove is in flight (stay-open-while-pending)', async () => {
    const user = userEvent.setup();
    // Hold the promise open so we can observe the pending state. We
    // resolve it manually at the end of the test, then flush.
    let resolve: () => void = () => {};
    const pending = new Promise<void>((r) => { resolve = r; });
    const onDeleteCove = vi.fn().mockReturnValue(pending);
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onDeleteCove={onDeleteCove}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Delete cove "Atlas"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete cove?' });
    const confirm = within(dialog).getByRole('button', { name: 'Delete cove' });
    const cancel = within(dialog).getByRole('button', { name: 'Cancel' });
    expect((confirm as HTMLButtonElement).disabled).toBe(false);
    await user.click(confirm);
    // Mid-await: Confirm disabled, Cancel still enabled (Cancel-safe
    // default holds even during a pending confirm).
    expect((confirm as HTMLButtonElement).disabled).toBe(true);
    expect((cancel as HTMLButtonElement).disabled).toBe(false);
    expect(onDeleteCove).toHaveBeenCalledTimes(1);

    // Resolve and flush — dialog should close after the await.
    await act(async () => {
      resolve();
      await pending;
    });
    expect(screen.queryByRole('dialog', { name: 'Delete cove?' })).toBeNull();
  });
});

describe('CovePage delete-wave ConfirmDialog (Pattern B)', () => {
  it('clicking the row × opens a ConfirmDialog with the wave title in the body', async () => {
    const user = userEvent.setup();
    render(
      <CovePage
        cove={makeCove()}
        waves={[makeWave({ title: 'Ship checkout' })]}
        onGo={() => {}}
        onDeleteWave={() => {}}
      />,
    );
    expect(screen.queryByRole('dialog', { name: 'Delete wave?' })).toBeNull();
    await user.click(screen.getByRole('button', { name: 'Delete "Ship checkout"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete wave?' });
    expect(dialog).toBeInTheDocument();
    expect(dialog).toHaveTextContent('Delete wave "Ship checkout"?');
    expect(within(dialog).getByRole('button', { name: 'Delete wave' })).toBeInTheDocument();
    expect(within(dialog).getByRole('button', { name: 'Cancel' })).toBeInTheDocument();
  });

  it('Cancel closes the dialog without invoking onDeleteWave', async () => {
    const user = userEvent.setup();
    const onDeleteWave = vi.fn();
    render(
      <CovePage
        cove={makeCove()}
        waves={[makeWave({ title: 'Ship checkout' })]}
        onGo={() => {}}
        onDeleteWave={onDeleteWave}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Delete "Ship checkout"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete wave?' });
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog', { name: 'Delete wave?' })).toBeNull();
    expect(onDeleteWave).not.toHaveBeenCalled();
  });

  it('Confirm closes the dialog and invokes onDeleteWave with the wave id', async () => {
    const user = userEvent.setup();
    const onDeleteWave = vi.fn();
    render(
      <CovePage
        cove={makeCove()}
        waves={[makeWave({ id: 'w-checkout', title: 'Ship checkout' })]}
        onGo={() => {}}
        onDeleteWave={onDeleteWave}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Delete "Ship checkout"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete wave?' });
    await user.click(within(dialog).getByRole('button', { name: 'Delete wave' }));
    // Pattern B: dialog closes immediately on Confirm; parent's promise
    // resolves on its own time.
    expect(screen.queryByRole('dialog', { name: 'Delete wave?' })).toBeNull();
    expect(onDeleteWave).toHaveBeenCalledTimes(1);
    expect(onDeleteWave).toHaveBeenCalledWith('w-checkout');
  });

  it('reopening after Cancel targets the most recently clicked wave', async () => {
    const user = userEvent.setup();
    const onDeleteWave = vi.fn();
    render(
      <CovePage
        cove={makeCove()}
        waves={[
          makeWave({ id: 'w-a', title: 'Ship checkout' }),
          makeWave({ id: 'w-b', title: 'Migrate auth', lifecycle: 'working' }),
        ]}
        onGo={() => {}}
        onDeleteWave={onDeleteWave}
      />,
    );
    // First flow: open + Cancel.
    await user.click(screen.getByRole('button', { name: 'Delete "Ship checkout"' }));
    await user.click(
      within(screen.getByRole('dialog', { name: 'Delete wave?' })).getByRole('button', {
        name: 'Cancel',
      }),
    );
    expect(onDeleteWave).not.toHaveBeenCalled();

    // Second flow: open on the OTHER wave + Confirm. The description
    // should now reflect the new wave's title, and the id passed to
    // onDeleteWave should be the new wave's id.
    await user.click(screen.getByRole('button', { name: 'Delete "Migrate auth"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete wave?' });
    expect(dialog).toHaveTextContent('Delete wave "Migrate auth"?');
    await user.click(within(dialog).getByRole('button', { name: 'Delete wave' }));
    expect(onDeleteWave).toHaveBeenCalledTimes(1);
    expect(onDeleteWave).toHaveBeenCalledWith('w-b');
  });
});

describe('CovePage pin button on wave rows', () => {
  it('renders no pin button when onPinWave is not provided', () => {
    render(
      <CovePage
        cove={makeCove()}
        waves={[makeWave()]}
        onGo={() => {}}
      />,
    );
    expect(screen.queryByRole('button', { name: /pin wave/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /unpin wave/i })).toBeNull();
  });

  it('renders a "Pin wave" button when onPinWave is provided and wave is unpinned', () => {
    render(
      <CovePage
        cove={makeCove()}
        waves={[makeWave({ pinnedAt: null })]}
        onGo={() => {}}
        onPinWave={() => {}}
      />,
    );
    expect(screen.getByRole('button', { name: 'Pin wave' })).toBeTruthy();
  });

  it('renders an "Unpin wave" button when the wave is already pinned', () => {
    render(
      <CovePage
        cove={makeCove()}
        waves={[makeWave({ pinnedAt: 1000 })]}
        onGo={() => {}}
        onPinWave={() => {}}
      />,
    );
    expect(screen.getByRole('button', { name: 'Unpin wave' })).toBeTruthy();
  });

  it('calls onPinWave(id, true) when Pin wave is clicked', async () => {
    const user = userEvent.setup();
    const onPinWave = vi.fn();
    render(
      <CovePage
        cove={makeCove()}
        waves={[makeWave({ id: 'w-cove', pinnedAt: null })]}
        onGo={() => {}}
        onPinWave={onPinWave}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Pin wave' }));
    expect(onPinWave).toHaveBeenCalledWith('w-cove', true);
  });

  it('calls onPinWave(id, false) when Unpin wave is clicked', async () => {
    const user = userEvent.setup();
    const onPinWave = vi.fn();
    render(
      <CovePage
        cove={makeCove()}
        waves={[makeWave({ id: 'w-cove', pinnedAt: 9000 })]}
        onGo={() => {}}
        onPinWave={onPinWave}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Unpin wave' }));
    expect(onPinWave).toHaveBeenCalledWith('w-cove', false);
  });
});

// ---------------------------------------------------------------------------
// NewWaveDialog variant switch — issue #891 slice ③.
//
// The dialog hosts a two-button "Wave kind" toggle above NewTaskForm:
// "Task" (default, plain wave) and "Issue dev" (workflow-bound wave).
// The form itself is exercised in NewTaskForm.issueDev.test.tsx; here we
// pin the dialog-level wiring — default tab, switch → issue-dev fields
// appear, switch back → they're gone.
// ---------------------------------------------------------------------------

describe('CovePage NewWaveDialog variant switch (#891)', () => {
  async function openNewWaveDialog() {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    const user = userEvent.setup();
    const qc = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    render(
      <QueryClientProvider client={qc}>
        <CovePage
          cove={makeCove()}
          waves={[]}
          onGo={() => {}}
          onWaveCreated={() => {}}
        />
      </QueryClientProvider>,
    );
    await user.click(screen.getByRole('button', { name: 'New wave' }));
    const dialog = await screen.findByRole('dialog', { name: 'New wave' });
    return { user, dialog };
  }

  it('opens on the plain Task variant with the toggle visible', async () => {
    const { dialog } = await openNewWaveDialog();
    const tabs = within(dialog).getByRole('group', { name: 'Wave kind' });
    expect(within(tabs).getByRole('button', { name: 'Task' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    expect(
      within(tabs).getByRole('button', { name: 'Issue dev' }),
    ).toHaveAttribute('aria-pressed', 'false');
    // Plain form, no issue-dev fields.
    expect(within(dialog).queryByLabelText(/github issue url/i)).toBeNull();
  });

  it('switching to Issue dev shows the issue-dev form; switching back restores the plain form', async () => {
    const { user, dialog } = await openNewWaveDialog();
    await user.click(within(dialog).getByRole('button', { name: 'Issue dev' }));
    expect(within(dialog).getByLabelText(/github issue url/i)).toBeInTheDocument();
    expect(within(dialog).getByLabelText(/merge policy/i)).toBeInTheDocument();
    await user.click(within(dialog).getByRole('button', { name: 'Task' }));
    expect(within(dialog).queryByLabelText(/github issue url/i)).toBeNull();
    expect(
      within(dialog).getByRole('form', { name: /new task/i }),
    ).toBeInTheDocument();
  });
});
