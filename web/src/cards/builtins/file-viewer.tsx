import { lazy, Suspense, useEffect } from 'react';
import { z } from 'zod';
import { CardHead } from '../CardHead';
import type { CardEntry } from '../registry';
import type { FileViewerCardData } from '../../types';
import type {
  GitChangedFile,
  GitDiffResponse,
  ListdirResponse,
} from '../../api/wire';
import * as api from '../../api/calm';
import { CalmApiError } from '../../api/calm';
import { useTheme } from '../../app/theme';
import { useState } from '../../shared/state';

const LazyCodePane = lazy(() =>
  import('./file-viewer-codemirror').then((m) => ({ default: m.CodePane })),
);
const LazyDiffPane = lazy(() =>
  import('./file-viewer-codemirror').then((m) => ({ default: m.DiffPane })),
);

const fileViewerPayloadSchema = z.object({
  path: z.string().min(1),
});

type Tab = 'code' | 'diff';

const SIDEBAR_COLLAPSED_STORAGE_KEY = 'file-viewer:sidebar-collapsed';

function readPersistedSidebarCollapsed(): boolean {
  try {
    const v = window.localStorage.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY);
    return v === '1';
  } catch {
    return false;
  }
}

function writePersistedSidebarCollapsed(collapsed: boolean): void {
  try {
    window.localStorage.setItem(
      SIDEBAR_COLLAPSED_STORAGE_KEY,
      collapsed ? '1' : '0',
    );
  } catch {
    // ignore — storage may be unavailable.
  }
}

function FileViewerCard({
  card,
  onClose,
}: {
  card: FileViewerCardData;
  onClose?: () => void;
}) {
  const { resolved: theme } = useTheme();
  const [tab, setTab] = useState<Tab>('code');
  const [sidebarCollapsed, setSidebarCollapsed] = useState(
    readPersistedSidebarCollapsed,
  );
  const [folderPath, setFolderPath] = useState(card.path);
  const [selectedPath, setSelectedPath] = useState<string | null>(card.path);
  const [listing, setListing] = useState<ListdirResponse | null>(null);
  const [listingLoading, setListingLoading] = useState(false);
  const [listingError, setListingError] = useState<string | null>(null);
  const [fileState, setFileState] = useState<
    | { kind: 'idle' | 'loading' }
    | { kind: 'loaded'; path: string; text: string; truncated: boolean }
    | { kind: 'error'; message: string }
  >({ kind: 'idle' });
  const [gitRoot, setGitRoot] = useState<string | null>(null);
  const [changedFiles, setChangedFiles] = useState<GitChangedFile[]>([]);
  const [diffSelected, setDiffSelected] = useState<string | null>(null);
  const [diffListState, setDiffListState] = useState<
    'idle' | 'loading' | 'loaded'
  >('idle');
  const [diffError, setDiffError] = useState<string | null>(null);
  const [diff, setDiff] = useState<GitDiffResponse | null>(null);
  const [diffLoading, setDiffLoading] = useState(false);

  useEffect(() => {
    setFolderPath(card.path);
    setSelectedPath(card.path);
    setListing(null);
    setDiffSelected(null);
  }, [card.path]);

  useEffect(() => {
    writePersistedSidebarCollapsed(sidebarCollapsed);
  }, [sidebarCollapsed]);

  useEffect(() => {
    let cancelled = false;
    setListingLoading(true);
    setListingError(null);
    api
      .listDir(folderPath)
      .then((res) => {
        if (cancelled) return;
        setListing(res);
        setFolderPath(res.path);
        if (folderPath === card.path) {
          setSelectedPath(null);
        }
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        const parent = parentPath(folderPath);
        if (folderPath === card.path && parent && parent !== folderPath) {
          setFolderPath(parent);
          return;
        }
        setListing(null);
        setListingError(formatError(err, 'Failed to list directory'));
      })
      .finally(() => {
        if (!cancelled) setListingLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [card.path, folderPath]);

  useEffect(() => {
    if (tab !== 'code' || !selectedPath) return;
    let cancelled = false;
    setFileState({ kind: 'loading' });
    api
      .readFile(selectedPath)
      .then((res) => {
        if (cancelled) return;
        setFileState({
          kind: 'loaded',
          path: res.path,
          text: res.text,
          truncated: res.truncated,
        });
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setFileState({
            kind: 'error',
            message: formatError(err, 'Failed to read file'),
          });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [selectedPath, tab]);

  useEffect(() => {
    if (tab !== 'diff') return;
    let cancelled = false;
    setDiffListState('loading');
    setDiffError(null);
    api
      .gitStatus(folderPath)
      .then((res) => {
        if (cancelled) return;
        setGitRoot(res.repo_root);
        setChangedFiles(res.files);
        setDiffListState('loaded');
        setDiffSelected((cur) =>
          cur && res.files.some((f) => f.path === cur)
            ? cur
            : (res.files[0]?.path ?? null),
        );
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setGitRoot(null);
        setChangedFiles([]);
        setDiffSelected(null);
        setDiffListState('loaded');
        setDiffError(formatError(err, 'Failed to load git status'));
      });
    return () => {
      cancelled = true;
    };
  }, [folderPath, tab]);

  useEffect(() => {
    if (tab !== 'diff' || !gitRoot || !diffSelected) {
      setDiff(null);
      return;
    }
    let cancelled = false;
    const selectedFile = changedFiles.find((f) => f.path === diffSelected);
    setDiffLoading(true);
    setDiffError(null);
    api
      .gitDiff(joinPath(gitRoot, diffSelected), selectedFile?.old_path ?? undefined)
      .then((res) => {
        if (!cancelled) setDiff(res);
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setDiff(null);
          setDiffError(formatError(err, 'Failed to load diff'));
        }
      })
      .finally(() => {
        if (!cancelled) setDiffLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [changedFiles, diffSelected, gitRoot, tab]);

  const entries = listing?.entries ?? [];
  const sidebarToggleLabel = sidebarCollapsed
    ? 'Expand file tree'
    : 'Collapse file tree';

  return (
    <div className="file-viewer-card">
      <CardHead
        className="card-drag-handle"
        title="file"
        onClose={onClose}
        closeAriaLabel="Remove panel"
      />
      <div className={`file-viewer-body${sidebarCollapsed ? ' sidebar-collapsed' : ''}`}>
        {!sidebarCollapsed && (
          <aside className="file-viewer-tree" aria-label="Files">
            <div className="file-viewer-tree-head">
              <button
                type="button"
                className="file-viewer-up"
                onClick={() => listing?.parent && setFolderPath(listing.parent)}
                disabled={!listing?.parent || listingLoading}
                title="Parent directory"
                aria-label="Parent directory"
              >
                ↑
              </button>
              <span className="file-viewer-cwd" title={listing?.path ?? folderPath}>
                {listing?.path ?? folderPath}
              </span>
            </div>
            <div className="file-viewer-tree-list">
              {listingLoading ? (
                <div className="file-viewer-state">Loading…</div>
              ) : listingError ? (
                <div className="file-viewer-error">{listingError}</div>
              ) : entries.length === 0 ? (
                <div className="file-viewer-state">Empty directory</div>
              ) : (
                entries.map((ent) => {
                  const path = joinPath(listing?.path ?? folderPath, ent.name);
                  const selected = selectedPath === path;
                  return (
                    <button
                      key={ent.name}
                      type="button"
                      className={`file-viewer-entry${ent.is_dir ? '' : ' file'}${
                        selected ? ' selected' : ''
                      }`}
                      onClick={() => {
                        if (ent.is_dir) {
                          setSelectedPath(null);
                          setFolderPath(path);
                        } else {
                          setSelectedPath(path);
                          setTab('code');
                        }
                      }}
                      title={ent.name}
                    >
                      <span aria-hidden="true">{ent.is_dir ? '▸' : '·'}</span>
                      <span>{ent.name}</span>
                    </button>
                  );
                })
              )}
            </div>
          </aside>
        )}
        <section className="file-viewer-main">
          <div className="file-viewer-toolbar">
            <button
              type="button"
              className="file-viewer-sidebar-toggle"
              aria-expanded={!sidebarCollapsed}
              aria-label={sidebarToggleLabel}
              title={sidebarToggleLabel}
              onClick={() => setSidebarCollapsed((collapsed) => !collapsed)}
            >
              <span aria-hidden="true">{sidebarCollapsed ? '›' : '‹'}</span>
            </button>
            <div
              className="file-viewer-tabs"
              role="tablist"
              aria-label="File viewer mode"
            >
              <button
                type="button"
                role="tab"
                aria-selected={tab === 'code'}
                className={tab === 'code' ? 'active' : ''}
                onClick={() => setTab('code')}
              >
                Code
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={tab === 'diff'}
                className={tab === 'diff' ? 'active' : ''}
                onClick={() => setTab('diff')}
              >
                Diff
              </button>
            </div>
            <span
              className="file-viewer-selection"
              title={tab === 'diff' ? diffSelected ?? '' : selectedPath ?? ''}
            >
              {tab === 'diff'
                ? diffSelected ?? 'No changed file selected'
                : selectedPath ?? 'Select a file'}
            </span>
          </div>
          {tab === 'code' ? (
            <CodeTab state={fileState} selectedPath={selectedPath} theme={theme} />
          ) : (
            <DiffTab
              files={changedFiles}
              selected={diffSelected}
              onSelect={setDiffSelected}
              listState={diffListState}
              error={diffError}
              diff={diff}
              diffLoading={diffLoading}
              theme={theme}
            />
          )}
        </section>
      </div>
    </div>
  );
}

function CodeTab({
  state,
  selectedPath,
  theme,
}: {
  state:
    | { kind: 'idle' | 'loading' }
    | { kind: 'loaded'; path: string; text: string; truncated: boolean }
    | { kind: 'error'; message: string };
  selectedPath: string | null;
  theme: 'light' | 'dark';
}) {
  if (!selectedPath) {
    return <div className="file-viewer-empty">Select a file to view it.</div>;
  }
  switch (state.kind) {
    case 'idle':
    case 'loading':
      return <div className="file-viewer-state">Loading file…</div>;
    case 'error':
      return <div className="file-viewer-error">{state.message}</div>;
  }
  return (
    <div className="file-viewer-code-wrap">
      {state.truncated && (
        <div className="file-viewer-banner">
          Showing the first 2 MiB of this file.
        </div>
      )}
      <Suspense fallback={<div className="file-viewer-state">Loading editor…</div>}>
        <LazyCodePane path={state.path} text={state.text} theme={theme} />
      </Suspense>
    </div>
  );
}

function DiffTab({
  files,
  selected,
  onSelect,
  listState,
  error,
  diff,
  diffLoading,
  theme,
}: {
  files: GitChangedFile[];
  selected: string | null;
  onSelect: (path: string) => void;
  listState: 'idle' | 'loading' | 'loaded';
  error: string | null;
  diff: GitDiffResponse | null;
  diffLoading: boolean;
  theme: 'light' | 'dark';
}) {
  return (
    <div className="file-viewer-diff">
      <div className="file-viewer-changes" aria-label="Changed files">
        {listState === 'loading' ? (
          <div className="file-viewer-state">Loading changes…</div>
        ) : files.length === 0 ? (
          <div className="file-viewer-state">No working-tree changes</div>
        ) : (
          files.map((f) => (
            <button
              key={`${f.status}:${f.path}`}
              type="button"
              className={`file-viewer-change${
                selected === f.path ? ' selected' : ''
              }`}
              onClick={() => onSelect(f.path)}
              title={f.path}
            >
              <span className={`file-viewer-status status-${f.status}`}>
                {statusLabel(f.status)}
              </span>
              <span>{f.path}</span>
            </button>
          ))
        )}
      </div>
      <div className="file-viewer-diff-pane">
        {error ? (
          <div className="file-viewer-error">{error}</div>
        ) : diffLoading || !diff ? (
          <div className="file-viewer-state">
            {selected ? 'Loading diff…' : 'Select a changed file'}
          </div>
        ) : (
          <>
            {diff.truncated && (
              <div className="file-viewer-banner">
                Showing the first 2 MiB of this file.
              </div>
            )}
            <Suspense fallback={<div className="file-viewer-state">Loading diff editor…</div>}>
              <LazyDiffPane
                path={diff.path}
                headText={diff.head_text ?? null}
                workingText={diff.working_text ?? null}
                theme={theme}
              />
            </Suspense>
          </>
        )}
      </div>
    </div>
  );
}

function statusLabel(status: string): string {
  switch (status) {
    case 'added':
      return 'A';
    case 'deleted':
      return 'D';
    case 'renamed':
      return 'R';
    case 'untracked':
      return '?';
    default:
      return 'M';
  }
}

function formatError(err: unknown, fallback: string): string {
  if (err instanceof CalmApiError) return err.message || fallback;
  if (err instanceof Error) return err.message;
  return fallback;
}

function parentPath(path: string): string | null {
  const trimmed = path.replace(/\/+$/, '');
  const idx = trimmed.lastIndexOf('/');
  if (idx <= 0) return idx === 0 ? '/' : null;
  return trimmed.slice(0, idx);
}

function joinPath(base: string, name: string): string {
  if (!base) return name;
  if (base.endsWith('/')) return base + name;
  return base + '/' + name;
}

export const FileViewerEntry: CardEntry<FileViewerCardData> = {
  type: 'file-viewer',
  Component: FileViewerCard,
  defaultSize: { w: 6, h: 12, minW: 4, minH: 6 },
  fromKernel: (k) => {
    if (k.kind !== 'file-viewer') return null;
    const parsed = fileViewerPayloadSchema.safeParse(k.payload ?? {});
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] file-viewer payload invalid for ${k.id}:`,
        parsed.error.issues,
      );
      return null;
    }
    return {
      type: 'file-viewer',
      id: k.id,
      path: parsed.data.path,
    };
  },
  addPanel: {
    label: 'file',
    createSchema: {
      fields: [
        {
          key: 'path',
          label: 'File or folder',
          type: 'file',
          required: true,
          placeholder: 'Choose a file or folder…',
        },
      ],
    },
  },
};
