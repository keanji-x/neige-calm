// Tests for the keyboard-entry rename path on AreaPage (slice 3 of #56).
//
// Mirrors `Track.test.tsx`: the EditableTitle in AreaPage shares the same
// keyboard contract (Enter / F2 → edit; Escape / Enter → exit + focus
// restore) but renders as a styled <h1> instead of a plain span.

import { describe, it, expect, vi } from 'vitest';
import { render, screen, act, fireEvent, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import * as api from '../api/calm';
import { AreaPage } from './Area';
import type { Area, Track } from '../types';

function makeArea(): Area {
  return { id: 'c1', name: 'Atlas', subtitle: '', color: '#5a9' };
}

function makeTrack(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1',
    areaId: 'c1',
    title: 'Migrate auth',
    lifecycle: 'draft',
    anyCardNeedsInput: false,
    progress: 0,
    eta: '',
    now: '',
    // Issue #250 PR 5 — required by the calendar-rail integration; the
    // AreaPage tests don't read these but the type-level requirement is
    // load-bearing for spotting forgotten fields in production code.
    createdAt: 0,
    terminalAt: null,
    pinnedAt: null,
    cards: [],
    ...overrides,
  };
}

describe('AreaPage EditableTitle keyboard entry', () => {
  it('renders the area title as a focusable button named after the area', () => {
    render(
      <AreaPage
        area={makeArea()}
        tracks={[]}
        onGo={() => {}}
        onRenameArea={() => {}}
      />,
    );
    // Rendered as an intrinsic <button> nested inside an <h1> so heading
    // semantics survive — no explicit tabindex needed (buttons are
    // focusable by default).
    const title = screen.getByRole('button', { name: 'Atlas' });
    expect(title.tagName).toBe('BUTTON');
    // The wrapping h1 should still be discoverable by heading nav.
    // After #56 followup, its accessible name is just "Atlas." (the
    // visible text, with the period the parent prints) — no "Rename area
    // name:" prefix, so heading-nav narration is clean. The sr-only
    // helper sits *outside* the <h1> so it doesn't pollute the heading's
    // name-from-content computation.
    expect(screen.getByRole('heading', { level: 1, name: 'Atlas.' })).toContainElement(title);
    // The rename verb is conveyed as an aria-describedby helper on the
    // inner button, not as part of its name.
    expect(title).toHaveAccessibleDescription('Rename area name');
  });

  it('falls back to a plain h1 when onRenameArea is absent', () => {
    render(
      <AreaPage
        area={makeArea()}
        tracks={[]}
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
      <AreaPage
        area={makeArea()}
        tracks={[]}
        onGo={() => {}}
        onRenameArea={() => {}}
      />,
    );
    const title = screen.getByRole('button', { name: 'Atlas' });
    title.focus();
    await user.keyboard('{Enter}');
    const input = screen.getByRole('textbox', { name: 'Area name' });
    expect(input).toBeInTheDocument();
    expect(document.activeElement).toBe(input);
  });

  it('F2 on the title opens rename mode', async () => {
    const user = userEvent.setup();
    render(
      <AreaPage
        area={makeArea()}
        tracks={[]}
        onGo={() => {}}
        onRenameArea={() => {}}
      />,
    );
    const title = screen.getByRole('button', { name: 'Atlas' });
    title.focus();
    await user.keyboard('{F2}');
    expect(screen.getByRole('textbox', { name: 'Area name' })).toBeInTheDocument();
  });

  it('Escape exits rename mode and restores focus to the title', async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      <AreaPage
        area={makeArea()}
        tracks={[]}
        onGo={() => {}}
        onRenameArea={onRename}
      />,
    );
    const title = screen.getByRole('button', { name: 'Atlas' });
    title.focus();
    await user.keyboard('{Enter}');
    const input = screen.getByRole('textbox', { name: 'Area name' });
    await user.type(input, ' edits');
    await user.keyboard('{Escape}');

    expect(screen.queryByRole('textbox', { name: 'Area name' })).not.toBeInTheDocument();
    expect(onRename).not.toHaveBeenCalled();
    const restored = screen.getByRole('button', { name: 'Atlas' });
    expect(document.activeElement).toBe(restored);
  });

  it('Enter commits a renamed value and restores focus to the title display', async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      <AreaPage
        area={makeArea()}
        tracks={[]}
        onGo={() => {}}
        onRenameArea={onRename}
      />,
    );
    const title = screen.getByRole('button', { name: 'Atlas' });
    title.focus();
    await user.keyboard('{Enter}');
    const input = screen.getByRole('textbox', { name: 'Area name' });
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
//   - Area × button (DeleteButton) → onDeleteArea. Pattern A: dialog
//     stays open while the async delete is in flight, Confirm is
//     disabled mid-await.
//   - Per-row × on a TrackRow → onDeleteTrack. Pattern B: dialog closes
//     on Confirm, parent's promise resolves out-of-band.
//
// We deliberately don't re-test Cancel-safe default focus, Esc routing,
// or overlay-click here — that's locked in
// `ui/ConfirmDialog/ConfirmDialog.contract.test.tsx` and is the same
// implementation under the hood.
// ============================================================

describe('AreaPage delete-area ConfirmDialog (Pattern A)', () => {
  it('clicking the × opens a ConfirmDialog with the area name in the body', async () => {
    const user = userEvent.setup();
    render(
      <AreaPage
        area={makeArea()}
        tracks={[]}
        onGo={() => {}}
        onDeleteArea={() => {}}
      />,
    );
    // Dialog is not open yet — the trigger button is the only delete
    // affordance present.
    expect(screen.queryByRole('dialog', { name: 'Delete area?' })).toBeNull();
    await user.click(screen.getByRole('button', { name: 'Delete area "Atlas"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete area?' });
    expect(dialog).toBeInTheDocument();
    expect(dialog).toHaveTextContent('Delete area "Atlas"?');
    expect(within(dialog).getByRole('button', { name: 'Delete area' })).toBeInTheDocument();
    expect(within(dialog).getByRole('button', { name: 'Cancel' })).toBeInTheDocument();
  });

  it('Cancel closes the dialog without invoking onDeleteArea', async () => {
    const user = userEvent.setup();
    const onDeleteArea = vi.fn();
    render(
      <AreaPage
        area={makeArea()}
        tracks={[]}
        onGo={() => {}}
        onDeleteArea={onDeleteArea}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Delete area "Atlas"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete area?' });
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog', { name: 'Delete area?' })).toBeNull();
    expect(onDeleteArea).not.toHaveBeenCalled();
  });

  it('Confirm fires onDeleteArea exactly once and closes the dialog', async () => {
    const user = userEvent.setup();
    const onDeleteArea = vi.fn().mockResolvedValue(undefined);
    render(
      <AreaPage
        area={makeArea()}
        tracks={[]}
        onGo={() => {}}
        onDeleteArea={onDeleteArea}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Delete area "Atlas"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete area?' });
    await user.click(within(dialog).getByRole('button', { name: 'Delete area' }));
    expect(onDeleteArea).toHaveBeenCalledTimes(1);
    expect(onDeleteArea).toHaveBeenCalledWith('c1');
    // Resolves with undefined immediately; DeleteButton closes the
    // dialog in its `finally` block after the await resolves.
    expect(screen.queryByRole('dialog', { name: 'Delete area?' })).toBeNull();
  });

  it('Confirm is disabled while onDeleteArea is in flight (stay-open-while-pending)', async () => {
    const user = userEvent.setup();
    // Hold the promise open so we can observe the pending state. We
    // resolve it manually at the end of the test, then flush.
    let resolve: () => void = () => {};
    const pending = new Promise<void>((r) => { resolve = r; });
    const onDeleteArea = vi.fn().mockReturnValue(pending);
    render(
      <AreaPage
        area={makeArea()}
        tracks={[]}
        onGo={() => {}}
        onDeleteArea={onDeleteArea}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Delete area "Atlas"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete area?' });
    const confirm = within(dialog).getByRole('button', { name: 'Delete area' });
    const cancel = within(dialog).getByRole('button', { name: 'Cancel' });
    expect((confirm as HTMLButtonElement).disabled).toBe(false);
    await user.click(confirm);
    // Mid-await: Confirm disabled, Cancel still enabled (Cancel-safe
    // default holds even during a pending confirm).
    expect((confirm as HTMLButtonElement).disabled).toBe(true);
    expect((cancel as HTMLButtonElement).disabled).toBe(false);
    expect(onDeleteArea).toHaveBeenCalledTimes(1);

    // Resolve and flush — dialog should close after the await.
    await act(async () => {
      resolve();
      await pending;
    });
    expect(screen.queryByRole('dialog', { name: 'Delete area?' })).toBeNull();
  });
});

describe('AreaPage delete-track ConfirmDialog (Pattern B)', () => {
  it('clicking the row × opens a ConfirmDialog with the track title in the body', async () => {
    const user = userEvent.setup();
    render(
      <AreaPage
        area={makeArea()}
        tracks={[makeTrack({ title: 'Ship checkout' })]}
        onGo={() => {}}
        onDeleteTrack={() => {}}
      />,
    );
    expect(screen.queryByRole('dialog', { name: 'Delete track?' })).toBeNull();
    await user.click(screen.getByRole('button', { name: 'Delete "Ship checkout"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete track?' });
    expect(dialog).toBeInTheDocument();
    expect(dialog).toHaveTextContent('Delete track "Ship checkout"?');
    expect(within(dialog).getByRole('button', { name: 'Delete track' })).toBeInTheDocument();
    expect(within(dialog).getByRole('button', { name: 'Cancel' })).toBeInTheDocument();
  });

  it('Cancel closes the dialog without invoking onDeleteTrack', async () => {
    const user = userEvent.setup();
    const onDeleteTrack = vi.fn();
    render(
      <AreaPage
        area={makeArea()}
        tracks={[makeTrack({ title: 'Ship checkout' })]}
        onGo={() => {}}
        onDeleteTrack={onDeleteTrack}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Delete "Ship checkout"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete track?' });
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog', { name: 'Delete track?' })).toBeNull();
    expect(onDeleteTrack).not.toHaveBeenCalled();
  });

  it('Confirm closes the dialog and invokes onDeleteTrack with the track id', async () => {
    const user = userEvent.setup();
    const onDeleteTrack = vi.fn();
    render(
      <AreaPage
        area={makeArea()}
        tracks={[makeTrack({ id: 'w-checkout', title: 'Ship checkout' })]}
        onGo={() => {}}
        onDeleteTrack={onDeleteTrack}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Delete "Ship checkout"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete track?' });
    await user.click(within(dialog).getByRole('button', { name: 'Delete track' }));
    // Pattern B: dialog closes immediately on Confirm; parent's promise
    // resolves on its own time.
    expect(screen.queryByRole('dialog', { name: 'Delete track?' })).toBeNull();
    expect(onDeleteTrack).toHaveBeenCalledTimes(1);
    expect(onDeleteTrack).toHaveBeenCalledWith('w-checkout');
  });

  it('reopening after Cancel targets the most recently clicked track', async () => {
    const user = userEvent.setup();
    const onDeleteTrack = vi.fn();
    render(
      <AreaPage
        area={makeArea()}
        tracks={[
          makeTrack({ id: 'w-a', title: 'Ship checkout' }),
          makeTrack({ id: 'w-b', title: 'Migrate auth', lifecycle: 'working' }),
        ]}
        onGo={() => {}}
        onDeleteTrack={onDeleteTrack}
      />,
    );
    // First flow: open + Cancel.
    await user.click(screen.getByRole('button', { name: 'Delete "Ship checkout"' }));
    await user.click(
      within(screen.getByRole('dialog', { name: 'Delete track?' })).getByRole('button', {
        name: 'Cancel',
      }),
    );
    expect(onDeleteTrack).not.toHaveBeenCalled();

    // Second flow: open on the OTHER track + Confirm. The description
    // should now reflect the new track's title, and the id passed to
    // onDeleteTrack should be the new track's id.
    await user.click(screen.getByRole('button', { name: 'Delete "Migrate auth"' }));
    const dialog = screen.getByRole('dialog', { name: 'Delete track?' });
    expect(dialog).toHaveTextContent('Delete track "Migrate auth"?');
    await user.click(within(dialog).getByRole('button', { name: 'Delete track' }));
    expect(onDeleteTrack).toHaveBeenCalledTimes(1);
    expect(onDeleteTrack).toHaveBeenCalledWith('w-b');
  });
});

describe('AreaPage pin button on track rows', () => {
  it('renders no pin button when onPinTrack is not provided', () => {
    render(
      <AreaPage
        area={makeArea()}
        tracks={[makeTrack()]}
        onGo={() => {}}
      />,
    );
    expect(screen.queryByRole('button', { name: /pin track/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /unpin track/i })).toBeNull();
  });

  it('renders a "Pin track" button when onPinTrack is provided and track is unpinned', () => {
    render(
      <AreaPage
        area={makeArea()}
        tracks={[makeTrack({ pinnedAt: null })]}
        onGo={() => {}}
        onPinTrack={() => {}}
      />,
    );
    expect(screen.getByRole('button', { name: 'Pin track' })).toBeTruthy();
  });

  it('renders an "Unpin track" button when the track is already pinned', () => {
    render(
      <AreaPage
        area={makeArea()}
        tracks={[makeTrack({ pinnedAt: 1000 })]}
        onGo={() => {}}
        onPinTrack={() => {}}
      />,
    );
    expect(screen.getByRole('button', { name: 'Unpin track' })).toBeTruthy();
  });

  it('calls onPinTrack(id, true) when Pin track is clicked', async () => {
    const user = userEvent.setup();
    const onPinTrack = vi.fn();
    render(
      <AreaPage
        area={makeArea()}
        tracks={[makeTrack({ id: 'w-area', pinnedAt: null })]}
        onGo={() => {}}
        onPinTrack={onPinTrack}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Pin track' }));
    expect(onPinTrack).toHaveBeenCalledWith('w-area', true);
  });

  it('calls onPinTrack(id, false) when Unpin track is clicked', async () => {
    const user = userEvent.setup();
    const onPinTrack = vi.fn();
    render(
      <AreaPage
        area={makeArea()}
        tracks={[makeTrack({ id: 'w-area', pinnedAt: 9000 })]}
        onGo={() => {}}
        onPinTrack={onPinTrack}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Unpin track' }));
    expect(onPinTrack).toHaveBeenCalledWith('w-area', false);
  });
});

// ---------------------------------------------------------------------------
// NewTrackDialog variant switch — issue #891 slice ③ (live design
// exploration: "Workflow" <select> with the base-select themed drawer;
// supersedes the r3 radio rows).
//
// The dialog hosts a labeled "Workflow" select above NewTaskForm:
// "None" (value `task`, default — plain track, no workflow) and "Issue
// dev" (value `issue-dev`, workflow-bound track). Options carry rich
// content (name + muted description span), so option accessible names
// include the description text — locators match on the leading name.
// The dialog itself has NO visible title row — it's named via
// aria-label ("New track") only. The form is exercised in
// NewTaskForm.issueDev.test.tsx; here we pin the dialog-level wiring —
// default option, switch → issue-dev fields appear, switch back →
// they're gone.
// ---------------------------------------------------------------------------

describe('AreaPage NewTrackDialog variant switch (#891)', () => {
  async function openNewTrackDialog() {
    vi.spyOn(api, 'listAreas').mockResolvedValue([]);
    const user = userEvent.setup();
    const qc = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    render(
      <QueryClientProvider client={qc}>
        <AreaPage
          area={makeArea()}
          tracks={[]}
          onGo={() => {}}
          onTrackCreated={() => {}}
        />
      </QueryClientProvider>,
    );
    await user.click(screen.getByRole('button', { name: 'New track' }));
    const dialog = await screen.findByRole('dialog', { name: 'New track' });
    return { user, dialog };
  }

  /** The labeled Workflow <select> hosted above NewTaskForm. */
  function templateSelect(dialog: HTMLElement) {
    return within(dialog).getByRole('combobox', {
      name: 'Workflow',
    }) as HTMLSelectElement;
  }

  it('opens on the plain "None" template with the select visible, and no visible title row', async () => {
    const { dialog } = await openNewTrackDialog();
    const select = templateSelect(dialog);
    expect(select.value).toBe('task');
    // Both templates are offered as options (extensibility seam:
    // future templates land here as new options). Rich option content
    // folds the muted description into the accessible name.
    expect(
      within(select).getByRole('option', { name: /^None/ }),
    ).toHaveValue('task');
    expect(
      within(select).getByRole('option', { name: /^Issue dev/ }),
    ).toHaveValue('issue-dev');
    // Signoff round 2: the dialog reads as one cohesive card — no
    // visible "New track" title row (and no head × button); the name
    // lives on the dialog's aria-label (already asserted by the
    // findByRole in openNewTrackDialog).
    expect(within(dialog).queryByText('New track')).toBeNull();
    expect(within(dialog).queryByRole('button', { name: 'Close' })).toBeNull();
    // Plain form, no issue-dev fields.
    expect(within(dialog).queryByLabelText(/github issue url/i)).toBeNull();
  });

  it('selecting Issue dev shows the issue-dev form; selecting None restores the plain form', async () => {
    const { user, dialog } = await openNewTrackDialog();
    await user.selectOptions(templateSelect(dialog), 'issue-dev');
    expect(within(dialog).getByLabelText(/github issue url/i)).toBeInTheDocument();
    expect(
      within(dialog).getByRole('checkbox', { name: /auto-merge/i }),
    ).toBeInTheDocument();
    await user.selectOptions(templateSelect(dialog), 'task');
    expect(within(dialog).queryByLabelText(/github issue url/i)).toBeNull();
    expect(
      within(dialog).getByRole('form', { name: /new task/i }),
    ).toBeInTheDocument();
  });

  it('focuses the variant-appropriate first field: title on open, URL input after selecting Issue dev, title again after selecting None', async () => {
    const { user, dialog } = await openNewTrackDialog();
    // Dialog's initial-focus pass lands on the task variant's first
    // field — the title textarea (via the shared initialFieldRef).
    await waitFor(() => {
      expect(document.activeElement).toBe(
        within(dialog).getByLabelText(/task description/i),
      );
    });
    // Changing the select remounts NewTaskForm; the new variant's
    // first required field (the issue URL input) must receive focus —
    // Dialog's open-time pass doesn't re-run, so this pins the
    // variant-change effect.
    await user.selectOptions(templateSelect(dialog), 'issue-dev');
    await waitFor(() => {
      expect(document.activeElement).toBe(
        within(dialog).getByLabelText(/github issue url/i),
      );
    });
    await user.selectOptions(templateSelect(dialog), 'task');
    await waitFor(() => {
      expect(document.activeElement).toBe(
        within(dialog).getByLabelText(/task description/i),
      );
    });
  });

  it('a manual title edit latches against re-prefill; switching variant (remount) un-latches', async () => {
    const { user, dialog } = await openNewTrackDialog();
    await user.selectOptions(templateSelect(dialog), 'issue-dev');
    const title = () =>
      within(dialog).getByLabelText(/task description/i) as HTMLTextAreaElement;
    const urlInput = within(dialog).getByLabelText(/github issue url/i);
    await user.type(urlInput, 'https://github.com/o/r/issues/7');
    await waitFor(() => expect(title().value).toBe('dev #7'));
    // Manual edit latches the title…
    await user.clear(title());
    await user.type(title(), 'my custom title');
    // …so re-pointing the URL must not clobber it.
    await user.clear(urlInput);
    await user.type(urlInput, 'https://github.com/o/r/issues/8');
    expect(title().value).toBe('my custom title');
    // Switching variant remounts NewTaskForm (key={variant}) — all
    // per-variant state resets, including the latch: prefill follows
    // the URL again in the fresh mount.
    await user.selectOptions(templateSelect(dialog), 'task');
    await user.selectOptions(templateSelect(dialog), 'issue-dev');
    expect(title().value).toBe('');
    await user.type(
      within(dialog).getByLabelText(/github issue url/i),
      'https://github.com/o/r/issues/9',
    );
    await waitFor(() => expect(title().value).toBe('dev #9'));
  });
});
