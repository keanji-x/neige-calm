// @vitest-environment jsdom
//
// The panes are mocked. Not to make the suite faster: CodeMirror measures a
// layout jsdom does not have, so a real pane here would assert nothing about
// the viewer and would fail for reasons that have nothing to do with it. What
// is under test is the shell around the panes — which read runs when, what the
// selection means, and what each failure looks like on screen.

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('./code-pane.tsx', () => ({
  CodePane: ({ path, text }: { path: string; text: string }) => (
    <div data-testid="code-pane" data-path={path}>{text}</div>
  ),
  DiffPane: ({ path, headText, workingText }: {
    path: string; headText: string | null; workingText: string | null;
  }) => (
    <div
      data-testid="diff-pane"
      data-path={path}
      data-head={headText ?? ''}
      data-working={workingText ?? ''}
    />
  ),
}));

import type { CardFilesPort } from '../../../../core/domain/fs.ts';
import { FileViewer, type ViewerSlots } from './public.tsx';

afterEach(cleanup);

function slots(): ViewerSlots {
  const values = new Map<string, unknown>();
  return {
    get<Value>(key: string, initial: Value | (() => Value)): Value {
      if (!values.has(key)) values.set(key, typeof initial === 'function' ? (initial as () => Value)() : initial);
      return values.get(key) as Value;
    },
    set<Value>(key: string, value: Value) { values.set(key, value); },
  };
}

function port(overrides: Partial<CardFilesPort> = {}): CardFilesPort {
  return Object.freeze({
    listDirectory: () => Promise.resolve({
      path: '/repo',
      parent: '/',
      entries: [{ name: 'src', is_dir: true }, { name: 'notes.txt', is_dir: false }],
    }),
    readFile: () => Promise.resolve({
      path: '/repo/notes.txt', size: 4, text: 'body', truncated: false,
    }),
    gitStatus: () => Promise.resolve({
      repo_root: '/repo',
      files: [{ path: 'src/main.rs', status: 'modified' }],
    }),
    gitDiff: () => Promise.resolve({
      path: 'src/main.rs', status: 'modified', head_text: 'was', working_text: 'is', truncated: false,
    }),
    rawUrl: (path: string) => `/api/fs/readfile-raw?path=${encodeURIComponent(path)}`,
    ...overrides,
  });
}

function renderViewer(files: CardFilesPort | null, path = '/repo') {
  return render(<FileViewer path={path} files={files} theme="dark" slots={slots()} />);
}

describe('FileViewer', () => {
  it('lists the card\'s folder', async () => {
    renderViewer(port());
    expect(await screen.findByRole('button', { name: /src/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /notes\.txt/ })).toBeTruthy();
  });

  it('opens a file into the code pane, and a folder into the listing instead', async () => {
    const listDirectory = vi.fn((requested: string) => Promise.resolve({
      path: requested,
      parent: '/',
      entries: requested === '/repo'
        ? [{ name: 'src', is_dir: true }, { name: 'notes.txt', is_dir: false }]
        : [{ name: 'main.rs', is_dir: false }],
    }));
    renderViewer(port({ listDirectory }));

    await userEvent.click(await screen.findByRole('button', { name: /notes\.txt/ }));
    const pane = await screen.findByTestId('code-pane');
    expect(pane.getAttribute('data-path')).toBe('/repo/notes.txt');
    expect(pane.textContent).toBe('body');

    // A folder navigates: it is not a file, and reading it would 400.
    await userEvent.click(screen.getByRole('button', { name: /src/ }));
    await waitFor(() => { expect(listDirectory).toHaveBeenCalledWith('/repo/src'); });
    expect(await screen.findByRole('button', { name: /main\.rs/ })).toBeTruthy();
  });

  /* The card's own path is a folder here, so nothing is selected until the
     reader picks something — the alternative (reading the folder as a file)
     is a guaranteed 400 on every open. */
  it('reads nothing until a file is picked', async () => {
    const readFile = vi.fn(() => Promise.resolve({
      path: '/repo', size: 0, text: '', truncated: false,
    }));
    renderViewer(port({ readFile }));
    expect(await screen.findByText('Select a file to view it.')).toBeTruthy();
    expect(readFile).not.toHaveBeenCalled();
  });

  it('says the file was truncated, because the pane is showing a prefix', async () => {
    renderViewer(port({
      readFile: () => Promise.resolve({
        path: '/repo/notes.txt', size: 9_000_000, text: 'body', truncated: true,
      }),
    }));
    await userEvent.click(await screen.findByRole('button', { name: /notes\.txt/ }));
    expect(await screen.findByText('Showing the first 2 MiB of this file.')).toBeTruthy();
  });

  it('renders an image by URL rather than reading it as text', async () => {
    const readFile = vi.fn();
    renderViewer(port({
      readFile,
      listDirectory: () => Promise.resolve({
        path: '/repo', parent: '/', entries: [{ name: 'logo.png', is_dir: false }],
      }),
    }));
    await userEvent.click(await screen.findByRole('button', { name: /logo\.png/ }));
    const image = await screen.findByRole('img', { name: '/repo/logo.png' });
    expect(image.getAttribute('src')).toBe('/api/fs/readfile-raw?path=%2Frepo%2Flogo.png');
    expect(readFile).not.toHaveBeenCalled();
  });

  it('shows the read failure instead of an empty pane', async () => {
    renderViewer(port({ readFile: () => Promise.reject(new Error('Permission denied')) }));
    await userEvent.click(await screen.findByRole('button', { name: /notes\.txt/ }));
    expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Permission denied');
  });

  describe('the diff tab', () => {
    it('selects the first changed file and loads both of its sides', async () => {
      renderViewer(port());
      await userEvent.click(await screen.findByRole('tab', { name: 'Diff' }));
      const pane = await screen.findByTestId('diff-pane');
      expect(pane.getAttribute('data-head')).toBe('was');
      expect(pane.getAttribute('data-working')).toBe('is');
      expect(screen.getByRole('button', { name: /src\/main\.rs/ })).toBeTruthy();
    });

    it('asks for the diff by repository-root-joined path, not by the relative one', async () => {
      const gitDiff = vi.fn(() => Promise.resolve({
        path: 'src/main.rs', status: 'modified', head_text: null, working_text: 'is', truncated: false,
      }));
      renderViewer(port({ gitDiff }));
      await userEvent.click(await screen.findByRole('tab', { name: 'Diff' }));
      // The status list is relative to the repo root; the diff endpoint takes an
      // absolute path. Sending the relative one 400s on every row.
      await waitFor(() => { expect(gitDiff).toHaveBeenCalledWith('/repo/src/main.rs', undefined); });
    });

    it('carries old_path so a rename can be diffed against what it was', async () => {
      const gitDiff = vi.fn(() => Promise.resolve({
        path: 'src/new.rs', status: 'renamed', head_text: 'was', working_text: 'is', truncated: false,
      }));
      renderViewer(port({
        gitDiff,
        gitStatus: () => Promise.resolve({
          repo_root: '/repo',
          files: [{ path: 'src/new.rs', status: 'renamed', old_path: 'src/old.rs' }],
        }),
      }));
      await userEvent.click(await screen.findByRole('tab', { name: 'Diff' }));
      await waitFor(() => { expect(gitDiff).toHaveBeenCalledWith('/repo/src/new.rs', 'src/old.rs'); });
    });

    it('reports a folder that is not a repository rather than showing no changes', async () => {
      renderViewer(port({
        gitStatus: () => Promise.reject(new Error('not inside a git repository')),
      }));
      await userEvent.click(await screen.findByRole('tab', { name: 'Diff' }));
      expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'not inside a git repository');
    });

    it('says so when the tree is clean', async () => {
      renderViewer(port({ gitStatus: () => Promise.resolve({ repo_root: '/repo', files: [] }) }));
      await userEvent.click(await screen.findByRole('tab', { name: 'Diff' }));
      expect(await screen.findByText('No working-tree changes')).toBeTruthy();
    });
  });

  /* A host assembled without the port is a real configuration, and the card has
     to say what is missing rather than render an empty frame or throw. */
  it('states the missing capability when the host has no filesystem port', () => {
    renderViewer(null);
    expect(screen.getByText('This board was built without filesystem access.')).toBeTruthy();
  });
});
