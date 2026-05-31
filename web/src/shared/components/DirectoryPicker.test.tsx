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
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
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

function activeOptionIndex() {
  return screen
    .getAllByRole('option')
    .findIndex((option) => option.getAttribute('aria-selected') === 'true');
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

  it('file mode lets file rows select a path without disabling directory navigation', async () => {
    vi.spyOn(api, 'listDir').mockResolvedValue({
      path: '/home/u',
      parent: null,
      entries: [
        { name: 'projects', is_dir: true },
        { name: 'notes.txt', is_dir: false },
      ],
    });
    const onSelect = vi.fn();
    render(
      <DirectoryBrowser
        initialPath={null}
        onCancel={() => {}}
        onSelect={onSelect}
        mode="file"
      />,
    );
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });

    const file = screen.getByRole('option', { name: /notes\.txt/i });
    expect(file).not.toBeDisabled();
    await userEvent.click(file);
    expect(onSelect).toHaveBeenCalledWith('/home/u/notes.txt');

    const dir = screen.getByRole('option', { name: /projects/i });
    expect(dir).not.toBeDisabled();
  });

  it('filters visible options by case-insensitive name prefix', async () => {
    const listDir = vi.spyOn(api, 'listDir').mockResolvedValue({
      path: '/home/u',
      parent: null,
      entries: [
        { name: 'calm', is_dir: true },
        { name: 'Calm.md', is_dir: false },
        { name: 'src', is_dir: true },
      ],
    });
    render(
      <DirectoryBrowser
        initialPath="/home/u"
        onCancel={() => {}}
        onSelect={() => {}}
        mode="file"
      />,
    );

    await waitFor(() => {
      expect(screen.getAllByRole('option')).toHaveLength(3);
    });
    const filter = screen.getByRole('textbox', { name: /filter directory entries/i });
    await userEvent.type(filter, 'CAL');

    expect(screen.getByRole('option', { name: /calm$/i })).toBeTruthy();
    expect(screen.getByRole('option', { name: /calm\.md/i })).toBeTruthy();
    expect(screen.queryByRole('option', { name: /src/i })).toBeNull();
    expect(listDir).toHaveBeenCalledTimes(1);
  });

  it('moves the keyboard highlight with ArrowDown and ArrowUp without wrapping', async () => {
    vi.spyOn(api, 'listDir').mockResolvedValue({
      path: '/home/u',
      parent: null,
      entries: [
        { name: 'alpha', is_dir: true },
        { name: 'beta', is_dir: true },
        { name: 'gamma', is_dir: true },
      ],
    });
    const user = userEvent.setup();
    render(
      <DirectoryBrowser
        initialPath="/home/u"
        onCancel={() => {}}
        onSelect={() => {}}
      />,
    );

    const filter = screen.getByRole('textbox', { name: /filter directory entries/i });
    await screen.findByRole('option', { name: /alpha/i });
    filter.focus();

    await waitFor(() => {
      expect(activeOptionIndex()).toBe(0);
    });
    await user.keyboard('{ArrowDown}');
    expect(activeOptionIndex()).toBe(1);
    await user.keyboard('{ArrowDown}');
    expect(activeOptionIndex()).toBe(2);
    await user.keyboard('{ArrowDown}');
    expect(activeOptionIndex()).toBe(2);
    await user.keyboard('{ArrowUp}');
    expect(activeOptionIndex()).toBe(1);
    await user.keyboard('{ArrowUp}');
    expect(activeOptionIndex()).toBe(0);
    await user.keyboard('{ArrowUp}');
    expect(activeOptionIndex()).toBe(0);
  });

  it('Enter on a highlighted folder descends into that folder', async () => {
    const listDir = vi.spyOn(api, 'listDir').mockImplementation(async (path?: string) => {
      if (path === '/home/u/calm') {
        return { path: '/home/u/calm', parent: '/home/u', entries: [] };
      }
      return {
        path: '/home/u',
        parent: null,
        entries: [{ name: 'calm', is_dir: true }],
      };
    });
    const user = userEvent.setup();
    render(
      <DirectoryBrowser
        initialPath="/home/u"
        onCancel={() => {}}
        onSelect={() => {}}
      />,
    );

    const filter = screen.getByRole('textbox', { name: /filter directory entries/i });
    await screen.findByRole('option', { name: /calm/i });
    filter.focus();
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(listDir).toHaveBeenCalledWith('/home/u/calm');
    });
  });

  it('Enter on a highlighted file selects the joined path in file mode', async () => {
    vi.spyOn(api, 'listDir').mockResolvedValue({
      path: '/home/u',
      parent: null,
      entries: [{ name: 'notes.txt', is_dir: false }],
    });
    const onSelect = vi.fn();
    const user = userEvent.setup();
    render(
      <DirectoryBrowser
        initialPath="/home/u"
        onCancel={() => {}}
        onSelect={onSelect}
        mode="file"
      />,
    );

    const filter = screen.getByRole('textbox', { name: /filter directory entries/i });
    await screen.findByRole('option', { name: /notes\.txt/i });
    filter.focus();
    await user.keyboard('{Enter}');

    expect(onSelect).toHaveBeenCalledWith('/home/u/notes.txt');
  });

  it('Enter with an empty filter and no highlight commits the current directory', async () => {
    vi.spyOn(api, 'listDir').mockResolvedValue({
      path: '/home/u',
      parent: null,
      entries: [],
    });
    const onSelect = vi.fn();
    const user = userEvent.setup();
    render(
      <DirectoryBrowser
        initialPath="/home/u"
        onCancel={() => {}}
        onSelect={onSelect}
      />,
    );

    const filter = screen.getByRole('textbox', { name: /filter directory entries/i });
    await screen.findByText('Empty directory');
    filter.focus();
    await user.keyboard('{Enter}');

    expect(onSelect).toHaveBeenCalledWith('/home/u');
  });

  it('Escape from the focused filter cancels the browser', async () => {
    vi.spyOn(api, 'listDir').mockResolvedValue({
      path: '/home/u',
      parent: null,
      entries: [],
    });
    const onCancel = vi.fn();
    const user = userEvent.setup();
    render(
      <DirectoryBrowser
        initialPath="/home/u"
        onCancel={onCancel}
        onSelect={() => {}}
      />,
    );

    const filter = screen.getByRole('textbox', { name: /filter directory entries/i });
    await screen.findByText('Empty directory');
    filter.focus();
    await user.keyboard('{Escape}');

    expect(onCancel).toHaveBeenCalledOnce();
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
