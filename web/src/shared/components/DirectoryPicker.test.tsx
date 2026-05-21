// DirectoryPicker / DirectoryBrowser tests.
//
// Focus of this file: the ARIA shape after #60 slice-1 cleanup removed the
// inner `role="dialog"` + `aria-label="Choose a directory"` from
// DirectoryBrowser. Every in-app caller renders the browser inside an outer
// `<Dialog>` — either pushed via `useModalView()` or as a direct child of a
// `<Dialog title=...>`. Nested ARIA dialogs are not allowed, so the inner
// role had to go; this test pins down both that it's gone AND that the
// outer Dialog still owns the accessible name in each path.
//
// The fallback inline path (DirectoryPicker rendered outside a Dialog) also
// needs to NOT advertise a dialog role — it's not modal.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { DirectoryBrowser, DirectoryPicker } from './DirectoryPicker';
import { Dialog } from '../../ui/Dialog/Dialog';
import * as api from '../../api/calm';

beforeEach(() => {
  cleanup();
  document.body.innerHTML = '';
});

afterEach(() => {
  vi.restoreAllMocks();
});

function stubListDir() {
  vi.spyOn(api, 'listDir').mockResolvedValue({
    path: '/home/u',
    parent: null,
    entries: [{ name: 'projects', is_dir: true }],
  });
}

describe('DirectoryBrowser ARIA shape', () => {
  it('does not advertise its own role="dialog" — the outer Dialog owns it', async () => {
    stubListDir();
    render(
      <Dialog open onClose={() => {}} title="New codex" wide>
        <DirectoryBrowser
          initialPath={null}
          onCancel={() => {}}
          onSelect={() => {}}
          selectLabel="Create here"
        />
      </Dialog>,
    );
    // Flush the focus rAF + listDir microtask.
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });

    // Exactly one role=dialog, and it carries the outer Dialog's title.
    const dialogs = screen.getAllByRole('dialog');
    expect(dialogs).toHaveLength(1);
    expect(dialogs[0]).toHaveAttribute('aria-label', 'New codex');

    // The dirpicker-browser container is rendered but is just a layout box.
    expect(document.querySelector('.dirpicker-browser')).toBeTruthy();
    expect(
      document.querySelector('.dirpicker-browser[role="dialog"]'),
    ).toBeNull();
  });

  it('renders without a role when used inline (no outer Dialog) — the inline path is not modal', async () => {
    stubListDir();
    render(
      <DirectoryBrowser
        initialPath={null}
        onCancel={() => {}}
        onSelect={() => {}}
      />,
    );
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });

    // No dialog anywhere in this tree.
    expect(screen.queryByRole('dialog')).toBeNull();
    // Container still present (it's just the layout box now).
    expect(document.querySelector('.dirpicker-browser')).toBeTruthy();
  });
});

describe('DirectoryPicker + useModalView', () => {
  it('clicking Browse pushes a view whose title becomes the dialog accessible name', async () => {
    stubListDir();
    render(
      <Dialog open onClose={() => {}} title="Outer">
        <DirectoryPicker value="" onChange={() => {}} />
      </Dialog>,
    );
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });

    // Initial state: outer title.
    expect(screen.getByRole('dialog', { name: 'Outer' })).toBeTruthy();

    // Click Browse — DirectoryPicker pushes a "Choose a directory" view.
    const trigger = screen.getByRole('button', { name: /choose a directory/i });
    await act(async () => {
      await userEvent.click(trigger);
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });

    // Dialog's accessible name now reflects the pushed view's title, and
    // there is still exactly ONE dialog in the tree (no nested role).
    const dialogs = screen.getAllByRole('dialog');
    expect(dialogs).toHaveLength(1);
    expect(dialogs[0]).toHaveAttribute('aria-label', 'Choose a directory');
  });
});
