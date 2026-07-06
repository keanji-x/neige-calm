import { lazy, Suspense, useCallback, useEffect, useMemo, useRef } from 'react';
import type {
  KeyboardEvent as ReactKeyboardEvent,
  RefObject,
} from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { z } from 'zod';
import { CardHead } from '../CardHead';
import { useCardInstanceCtx, type CardEntry } from '../registry';
import type {
  GitChangedFile,
  GitDiffResponse,
  ListdirResponse,
} from '../../api/wire';
import * as api from '../../api/calm';
import { CalmApiError } from '../../api/calm';
import { useTheme } from '../../app/theme';
import {
  overlayStateQueryKey,
  useOverlayState,
} from '../../hooks/useOverlayState';
import { useState } from '../../shared/state';
import { dlog } from '../../util/debug';
import type { PaneSearchAdapter } from './file-viewer-markdown';

declare module '../../types' {
  interface WaveCardDataMap {
    'file-viewer': FileViewerCardData;
  }
}

export interface FileViewerCardData {
  type: 'file-viewer';
  id: string;
  path: string;
}

const LazyCodePane = lazy(() =>
  import('./file-viewer-codemirror').then((m) => ({ default: m.CodePane })),
);
const LazyDiffPane = lazy(() =>
  import('./file-viewer-codemirror').then((m) => ({ default: m.DiffPane })),
);
const LazyMarkdownPane = lazy(() =>
  import('./file-viewer-markdown').then((m) => ({ default: m.MarkdownPane })),
);

function createPaneRefSlot(): { current: HTMLElement | null } {
  return { current: null };
}

const fileViewerPayloadSchema = z.object({
  path: z.string().min(1),
});

type Tab = 'code' | 'diff';

const FILE_VIEWER_NAV_SCHEMA_VERSION = 1;
const SIDEBAR_COLLAPSED_STORAGE_KEY = 'file-viewer:sidebar-collapsed';
const IMAGE_EXTENSIONS = [
  '.png',
  '.jpg',
  '.jpeg',
  '.gif',
  '.webp',
  '.bmp',
  '.ico',
  '.svg',
];
const MARKDOWN_EXTENSIONS = ['.md', '.markdown'];

type FileState =
  | { kind: 'idle' | 'loading' }
  | { kind: 'loaded'; path: string; text: string; truncated: boolean }
  | { kind: 'image'; path: string }
  | { kind: 'error'; message: string };

interface FileViewerNavOverlay {
  schemaVersion: 1;
  tab: Tab;
  folderPath: string;
  selectedPath: string | null;
  diffSelected: string | null;
}

function seedNav(path: string): FileViewerNavOverlay {
  return {
    schemaVersion: FILE_VIEWER_NAV_SCHEMA_VERSION,
    tab: 'code',
    folderPath: path,
    selectedPath: path,
    diffSelected: null,
  };
}

function isImagePath(path: string): boolean {
  const lower = path.toLowerCase();
  return IMAGE_EXTENSIONS.some((ext) => lower.endsWith(ext));
}

function isMarkdownPath(path: string): boolean {
  const lower = path.toLowerCase();
  return MARKDOWN_EXTENSIONS.some((ext) => lower.endsWith(ext));
}

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

function activeFileViewerPane(root: HTMLElement, tab: Tab): HTMLElement | null {
  if (tab === 'code') {
    return (
      root.querySelector<HTMLElement>('.file-viewer-markdown-body') ??
      root.querySelector<HTMLElement>('.cm-scroller') ??
      root.querySelector<HTMLElement>('.file-viewer-tree-list')
    );
  }
  return (
    root.querySelector<HTMLElement>('.file-viewer-merge') ??
    root.querySelector<HTMLElement>('.file-viewer-changes')
  );
}

function FileViewerCard({
  card,
  onClose,
}: {
  card: FileViewerCardData;
  onClose?: () => void;
}) {
  const { resolved: theme } = useTheme();
  const queryClient = useQueryClient();
  const cardRef = useRef<HTMLDivElement | null>(null);
  const [paneRefSlot] = useCardInstanceCtx().useCardSlot<{
    current: HTMLElement | null;
  }>('fvPaneRef', createPaneRefSlot);
  const defaultNav = useMemo(() => seedNav(card.path), [card.path]);
  const navOverlayQueryKey = overlayStateQueryKey(
    'kernel',
    'card',
    card.id,
    'file-viewer-nav',
  );
  // The hook returns this seed for one render while persisted nav hydrates,
  // matching the pre-overlay first paint.
  const [nav, setNav] = useOverlayState<FileViewerNavOverlay>({
    entity_kind: 'card',
    entity_id: card.id,
    kind: 'file-viewer-nav',
    default: defaultNav,
  });
  const navOverlayReady =
    queryClient.getQueryState(navOverlayQueryKey)?.status === 'success';
  const { tab, folderPath, selectedPath, diffSelected } = nav;
  const didMountRef = useRef(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(
    readPersistedSidebarCollapsed,
  );
  const [listing, setListing] = useState<ListdirResponse | null>(null);
  const [listingLoading, setListingLoading] = useState(false);
  const [listingError, setListingError] = useState<string | null>(null);
  const [fileState, setFileState] = useState<FileState>({ kind: 'idle' });
  const [gitRoot, setGitRoot] = useState<string | null>(null);
  const [changedFiles, setChangedFiles] = useState<GitChangedFile[]>([]);
  const [diffListState, setDiffListState] = useState<
    'idle' | 'loading' | 'loaded'
  >('idle');
  const [diffError, setDiffError] = useState<string | null>(null);
  const [diff, setDiff] = useState<GitDiffResponse | null>(null);
  const [diffLoading, setDiffLoading] = useState(false);

  useEffect(() => {
    if (!didMountRef.current) {
      didMountRef.current = true;
      return;
    }
    setNav(seedNav(card.path));
    setListing(null);
  }, [card.path, setNav]);

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
        if (navOverlayReady && res.path !== folderPath) {
          setNav((cur) => ({
            ...cur,
            folderPath: res.path,
            selectedPath:
              cur.selectedPath === folderPath ? res.path : cur.selectedPath,
          }));
        }
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        const parent = parentPath(folderPath);
        if (navOverlayReady && parent && parent !== folderPath) {
          setNav((cur) => ({
            ...cur,
            folderPath: parent,
            selectedPath:
              folderPath !== card.path && cur.selectedPath === folderPath
                ? parent
                : cur.selectedPath,
          }));
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
  }, [card.path, folderPath, navOverlayReady, setNav]);

  const selectedCodePath =
    selectedPath === folderPath &&
    (listingLoading || !listing || listing.path === selectedPath)
      ? null
      : selectedPath;

  useEffect(() => {
    const root = cardRef.current;
    if (!root) {
      paneRefSlot.current = null;
      return;
    }
    const updatePaneRef = () => {
      paneRefSlot.current = activeFileViewerPane(root, tab);
    };
    updatePaneRef();
    const observer = new MutationObserver(updatePaneRef);
    observer.observe(root, { childList: true, subtree: true });
    return () => {
      observer.disconnect();
      if (paneRefSlot.current && root.contains(paneRefSlot.current)) {
        paneRefSlot.current = null;
      }
    };
  }, [diffSelected, paneRefSlot, selectedCodePath, sidebarCollapsed, tab]);

  useEffect(() => {
    if (tab !== 'code' || !selectedCodePath) return;
    if (isImagePath(selectedCodePath)) {
      setFileState({ kind: 'image', path: selectedCodePath });
      return;
    }
    let cancelled = false;
    setFileState({ kind: 'loading' });
    api
      .readFile(selectedCodePath)
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
  }, [selectedCodePath, tab]);

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
        setNav((cur) => ({
          ...cur,
          diffSelected:
            cur.diffSelected && res.files.some((f) => f.path === cur.diffSelected)
              ? cur.diffSelected
              : (res.files[0]?.path ?? null),
        }));
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setGitRoot(null);
        setChangedFiles([]);
        setNav((cur) => ({ ...cur, diffSelected: null }));
        setDiffListState('loaded');
        setDiffError(formatError(err, 'Failed to load git status'));
      });
    return () => {
      cancelled = true;
    };
  }, [folderPath, setNav, tab]);

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
  const codeSelectionLabel = selectedCodePath ?? 'Select a file';

  return (
    <div ref={cardRef} className="file-viewer-card">
      <CardHead
        card={card}
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
                onClick={() =>
                  listing?.parent &&
                  setNav((cur) => ({ ...cur, folderPath: listing.parent! }))
                }
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
            <div className="file-viewer-tree-list" data-wheel-pane="tree">
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
                          setNav((cur) => ({
                            ...cur,
                            folderPath: path,
                            selectedPath: null,
                          }));
                        } else {
                          setNav((cur) => ({
                            ...cur,
                            tab: 'code',
                            selectedPath: path,
                          }));
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
                onClick={() => setNav((cur) => ({ ...cur, tab: 'code' }))}
              >
                Code
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={tab === 'diff'}
                className={tab === 'diff' ? 'active' : ''}
                onClick={() => setNav((cur) => ({ ...cur, tab: 'diff' }))}
              >
                Diff
              </button>
            </div>
            <span
              className="file-viewer-selection"
              title={tab === 'diff' ? diffSelected ?? '' : selectedCodePath ?? ''}
            >
              {tab === 'diff'
                ? diffSelected ?? 'No changed file selected'
                : codeSelectionLabel}
            </span>
          </div>
          {tab === 'code' ? (
            <CodeTab state={fileState} selectedPath={selectedCodePath} theme={theme} />
          ) : (
            <DiffTab
              files={changedFiles}
              selected={diffSelected}
              onSelect={(path) =>
                setNav((cur) => ({ ...cur, diffSelected: path }))
              }
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
  state: FileState;
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
    case 'image':
      return (
        <div className="file-viewer-image-wrap">
          <img
            className="file-viewer-image"
            src={api.readFileRaw(state.path)}
            alt={state.path}
          />
        </div>
      );
  }
  return (
    <LoadedFileContent
      path={state.path}
      text={state.text}
      truncated={state.truncated}
      theme={theme}
    />
  );
}

type MarkdownMode = 'preview' | 'source';

function LoadedFileContent({
  path,
  text,
  truncated,
  theme,
}: {
  path: string;
  text: string;
  truncated: boolean;
  theme: 'light' | 'dark';
}) {
  const isMd = isMarkdownPath(path);
  const [mode, setMode] = useState<MarkdownMode>('preview');
  // Mode is a local useState — it survives Preview/Source toggling within
  // the same loaded file, but resets to 'preview' when the file changes,
  // because CodeTab returns a "Loading file…" placeholder between files
  // (unmounting LoadedFileContent). That per-file reset is what we want:
  // opening a new .md file should start on Preview by default.
  //
  // Non-markdown files force `effectiveMode` to 'source' so the pane still
  // renders through CodeMirror even if we ever mount LoadedFileContent
  // with a stale 'preview' mode.
  const effectiveMode: MarkdownMode = isMd ? mode : 'source';

  // Search state — see docs/superpowers/specs/2026-07-06-file-viewer-search-and-prose-design.md.
  // Kept local: search never persists across file swaps, tab changes, or
  // remounts. The active pane's adapter is registered via a callback prop
  // and stored in a ref so callbacks stay stable.
  const [barOpen, setBarOpen] = useState(false);
  const [matchCurrent, setMatchCurrent] = useState(0);
  const [matchTotal, setMatchTotal] = useState(0);
  const adapterRef = useRef<PaneSearchAdapter | null>(null);
  const barInputRef = useRef<HTMLInputElement | null>(null);
  const queryRef = useRef<string>('');

  const closeBar = useCallback(() => {
    setBarOpen(false);
    setMatchCurrent(0);
    setMatchTotal(0);
    queryRef.current = '';
    adapterRef.current?.setQuery('');
  }, []);

  const openBar = useCallback(() => {
    setBarOpen(true);
  }, []);

  // Reset the bar when the file identity changes.
  useEffect(() => {
    closeBar();
  }, [path, closeBar]);

  const onAdapter = useCallback(
    (adapter: PaneSearchAdapter | null) => {
      adapterRef.current = adapter;
      if (adapter && queryRef.current) {
        // If the pane remounted (e.g. Preview↔Source toggle) while a query
        // was live, re-run it against the fresh adapter.
        adapter.setQuery(queryRef.current);
      }
    },
    [],
  );

  const onCount = useCallback((current: number, total: number) => {
    setMatchCurrent(current);
    setMatchTotal(total);
  }, []);

  return (
    <div className="file-viewer-code-wrap">
      {truncated && (
        <div className="file-viewer-banner">
          Showing the first 2 MiB of this file.
        </div>
      )}
      {isMd && (
        <div
          className="file-viewer-md-mode"
          role="tablist"
          aria-label="Markdown view mode"
        >
          <button
            type="button"
            role="tab"
            aria-selected={effectiveMode === 'preview'}
            className={effectiveMode === 'preview' ? 'active' : ''}
            onClick={() => setMode('preview')}
          >
            Preview
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={effectiveMode === 'source'}
            className={effectiveMode === 'source' ? 'active' : ''}
            onClick={() => setMode('source')}
          >
            Source
          </button>
        </div>
      )}
      {effectiveMode === 'preview' ? (
        <Suspense
          fallback={
            <div className="file-viewer-state">Loading preview…</div>
          }
        >
          <LazyMarkdownPane
            path={path}
            text={text}
            onSearchAdapterReady={onAdapter}
            onSearchCount={onCount}
            onSlashOpen={openBar}
          />
        </Suspense>
      ) : (
        <Suspense
          fallback={<div className="file-viewer-state">Loading editor…</div>}
        >
          <LazyCodePane
            path={path}
            text={text}
            theme={theme}
            onSearchAdapterReady={onAdapter}
            onSearchCount={onCount}
            onSlashOpen={openBar}
          />
        </Suspense>
      )}
      {barOpen && (
        <SearchBar
          inputRef={barInputRef}
          current={matchCurrent}
          total={matchTotal}
          queryRef={queryRef}
          onChange={(value) => {
            queryRef.current = value;
            adapterRef.current?.setQuery(value);
          }}
          onNext={() => adapterRef.current?.next()}
          onPrev={() => adapterRef.current?.prev()}
          onClose={closeBar}
        />
      )}
    </div>
  );
}

interface SearchBarProps {
  inputRef: RefObject<HTMLInputElement | null>;
  queryRef: RefObject<string>;
  current: number;
  total: number;
  onChange: (value: string) => void;
  onNext: () => void;
  onPrev: () => void;
  onClose: () => void;
}

function SearchBar({
  inputRef,
  queryRef,
  current,
  total,
  onChange,
  onNext,
  onPrev,
  onClose,
}: SearchBarProps) {
  useEffect(() => {
    inputRef.current?.focus();
    // Re-run the persisted query on remount so the highlights come back if
    // the pane switched under us with a query still live.
    if (queryRef.current) {
      onChange(queryRef.current);
    }
    // We intentionally only mount-focus; not-in-deps below is deliberate.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onInputKey = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      onClose();
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      if (e.shiftKey) onPrev();
      else onNext();
    }
  };

  const countLabel =
    total === 0
      ? queryRef.current
        ? 'no match'
        : ''
      : `${current || 1}/${total}`;

  return (
    <div className="fv-search-bar" role="search">
      <input
        ref={inputRef}
        type="search"
        aria-label="Search in file"
        placeholder="Search…"
        defaultValue={queryRef.current}
        onChange={(e) => onChange(e.currentTarget.value)}
        onKeyDown={onInputKey}
      />
      <span className="fv-search-count" aria-live="polite">
        {countLabel}
      </span>
      <button
        type="button"
        aria-label="Previous match"
        title="Previous match"
        onClick={onPrev}
        disabled={total === 0}
      >
        ↑
      </button>
      <button
        type="button"
        aria-label="Next match"
        title="Next match"
        onClick={onNext}
        disabled={total === 0}
      >
        ↓
      </button>
      <button type="button" aria-label="Close search" title="Close search" onClick={onClose}>
        ×
      </button>
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
      <div
        className="file-viewer-changes"
        data-wheel-pane="changes"
        aria-label="Changed files"
      >
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
  refreshBacking: 'none',
  createController({ card }) {
    return {
      onVisibleChange(visible) {
        dlog('FileViewerCard', 'visibility', { cardId: card.id, visible });
      },
    };
  },
  wheelTarget(_card, instance) {
    const [paneRefSlot] = instance.useCardSlot<{ current: HTMLElement | null }>(
      'fvPaneRef',
      createPaneRefSlot,
    );
    return { kind: 'native-scroll', ref: paneRefSlot };
  },
  claim: { mode: 'exact', kind: 'file-viewer' },
  title: () => 'file',
  accessibleName: (card) => `File: ${card.path}`,
  create: {
    mode: 'generic',
    buildPayload(input: { path: string }) {
      return { path: input.path };
    },
  },
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
