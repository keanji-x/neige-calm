// The file card's body: a folder on the left, and one file — or one file's two sides — on the right.

import { Suspense, lazy, useCallback, useEffect, useRef, type ReactNode } from 'react';

import type { CardFilesPort, DirectoryListingWire, GitChangedFileWire, GitDiffWire } from '../../../../core/domain/fs.ts';
import { joinDirectoryPath } from '../../ui/directory-browser/public.tsx';
import { useState } from '../../ui/state/public.ts';
import type { PaneSearchAdapter, PaneTheme } from './code-pane.tsx';
import { isImagePath } from './file-kind.ts';
import { FileReadError } from './read-error.tsx';

export { FileReadError } from './read-error.tsx';

export { useReportFileResource } from './report-file-resource.ts';
export type { ReportFileResource } from './report-file-resource.ts';

const LazyCodePane = lazy(() => import('./code-pane.tsx').then((module) => ({ default: module.CodePane })));
const LazyDiffPane = lazy(() => import('./code-pane.tsx').then((module) => ({ default: module.DiffPane })));

type Tab = 'code' | 'diff';

type Nav = Readonly<{
  tab: Tab;
  folderPath: string;
  selectedPath: string | null;
  diffSelected: string | null;
}>;

type FileState =
  | Readonly<{ kind: 'idle' }>
  | Readonly<{ kind: 'loading' }>
  | Readonly<{ kind: 'loaded'; path: string; text: string; truncated: boolean }>
  | Readonly<{ kind: 'image'; path: string }>
  | Readonly<{ kind: 'error'; message: string }>;

const NAV_SLOT = 'fs-viewer-nav';

function seedNav(path: string): Nav {
  return { tab: 'code', folderPath: path, selectedPath: path, diffSelected: null };
}

function parentPath(path: string): string | null {
  const trimmed = path.replace(/\/+$/, '');
  const index = trimmed.lastIndexOf('/');
  if (index <= 0) return index === 0 ? '/' : null;
  return trimmed.slice(0, index);
}

function messageOf(error: unknown, fallback: string): string {
  return error instanceof Error && error.message !== '' ? error.message : fallback;
}

function statusLabel(status: string): string {
  switch (status) {
    case 'added': return 'A';
    case 'deleted': return 'D';
    case 'renamed': return 'R';
    case 'untracked': return '?';
    default: return 'M';
  }
}

/** Declared here rather than imported from `systems/cards`: importing the public entry from a module its built-ins import would be a cycle. */
export interface ViewerSlots {
  get<Value>(key: string, initial: Value | (() => Value)): Value;
  set<Value>(key: string, value: Value): void;
}

export type FileViewerProps = Readonly<{
  path: string;
  files: CardFilesPort | null;
  theme: PaneTheme;
  slots: ViewerSlots;
}>;

export function FileViewer({ path, files, theme, slots }: FileViewerProps) {
  const [nav, setNavState] = useState<Nav>(() => slots.get<Nav>(NAV_SLOT, () => seedNav(path)));
  const setNav = (next: (current: Nav) => Nav) => {
    setNavState((current) => {
      const value = next(current);
      slots.set(NAV_SLOT, value);
      return value;
    });
  };
  const { tab, folderPath, selectedPath, diffSelected } = nav;

  const [listingRetry, setListingRetry] = useState(0);
  const [fileRetry, setFileRetry] = useState(0);
  const [diffListRetry, setDiffListRetry] = useState(0);
  const [diffRetry, setDiffRetry] = useState(0);
  const [listing, setListing] = useState<DirectoryListingWire | null>(null);
  const [listingLoading, setListingLoading] = useState(false);
  const [listingError, setListingError] = useState<string | null>(null);
  const [fileState, setFileState] = useState<FileState>({ kind: 'idle' });
  const [gitRoot, setGitRoot] = useState<string | null>(null);
  const [changedFiles, setChangedFiles] = useState<readonly GitChangedFileWire[]>([]);
  const [diffListLoading, setDiffListLoading] = useState(false);
  const [diffError, setDiffError] = useState<string | null>(null);
  const [diff, setDiff] = useState<GitDiffWire | null>(null);
  const [diffLoading, setDiffLoading] = useState(false);

  /* A card can be created on a file, and `listDirectory` answers 400 for one — so the card's own path (and only it) climbs to its parent once. */
  useEffect(() => {
    if (files === null) return;
    let cancelled = false;
    setListingLoading(true);
    setListingError(null);
    files.listDirectory(folderPath)
      .then((result) => {
        if (cancelled) return;
        setListing(result);
        if (result.path !== folderPath) {
          setNav((current) => ({
            ...current,
            folderPath: result.path,
            selectedPath: current.selectedPath === folderPath ? result.path : current.selectedPath,
          }));
        }
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        const parent = parentPath(folderPath);
        if (folderPath === path && parent !== null && parent !== folderPath) {
          setNav((current) => ({ ...current, folderPath: parent }));
          return;
        }
        setListing(null);
        setListingError(messageOf(error, 'Failed to list directory'));
      })
      .finally(() => { if (!cancelled) setListingLoading(false); });
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `setNav` is rebuilt every render by design; re-running the read on it would loop.
  }, [files, folderPath, path, listingRetry]);

  /* A folder is not a file to read; the selection becomes a read only once it names something other than the listed folder itself. */
  const selectedCodePath = selectedPath === folderPath
    && (listingLoading || listing === null || listing.path === selectedPath)
    ? null
    : selectedPath;

  useEffect(() => {
    if (files === null || tab !== 'code' || selectedCodePath === null) return;
    if (isImagePath(selectedCodePath)) {
      setFileState({ kind: 'image', path: selectedCodePath });
      return;
    }
    let cancelled = false;
    setFileState({ kind: 'loading' });
    files.readFile(selectedCodePath)
      .then((result) => {
        if (cancelled) return;
        setFileState({
          kind: 'loaded', path: result.path, text: result.text, truncated: result.truncated,
        });
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        setFileState({ kind: 'error', message: messageOf(error, 'Failed to read file') });
      });
    return () => { cancelled = true; };
  }, [files, selectedCodePath, tab, fileRetry]);

  /* `diffSelected` is dropped before the read and the loading flag raised, so nothing from the previous folder outlives the move. */
  useEffect(() => {
    if (files === null || tab !== 'diff') return;
    let cancelled = false;
    setDiffListLoading(true);
    setDiffError(null);
    setNav((current) => (current.diffSelected === null ? current : { ...current, diffSelected: null }));
    files.gitStatus(folderPath)
      .then((result) => {
        if (cancelled) return;
        setGitRoot(result.repo_root);
        setChangedFiles(result.files);
        setNav((current) => ({
          ...current,
          diffSelected: current.diffSelected !== null
            && result.files.some((file) => file.path === current.diffSelected)
            ? current.diffSelected
            : (result.files[0]?.path ?? null),
        }));
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        setGitRoot(null);
        setChangedFiles([]);
        setNav((current) => ({ ...current, diffSelected: null }));
        setDiffError(messageOf(error, 'Failed to load git status'));
      })
      .finally(() => { if (!cancelled) setDiffListLoading(false); });
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- as above: `setNav` identity changes every render.
  }, [files, folderPath, tab, diffListRetry]);

  useEffect(() => {
    if (files === null || tab !== 'diff' || gitRoot === null || diffSelected === null) {
      setDiff(null);
      return;
    }
    let cancelled = false;
    const selectedFile = changedFiles.find((file) => file.path === diffSelected);
    setDiffLoading(true);
    setDiffError(null);
    files.gitDiff(joinDirectoryPath(gitRoot, diffSelected), selectedFile?.old_path)
      .then((result) => { if (!cancelled) setDiff(result); })
      .catch((error: unknown) => {
        if (cancelled) return;
        setDiff(null);
        setDiffError(messageOf(error, 'Failed to load diff'));
      })
      .finally(() => { if (!cancelled) setDiffLoading(false); });
    return () => { cancelled = true; };
  }, [changedFiles, diffSelected, files, gitRoot, tab, diffRetry]);

  if (files === null) {
    return (
      <div className="fv-body">
        <p className="fv-state">This board was built without filesystem access.</p>
      </div>
    );
  }

  const entries = listing?.entries ?? [];
  const listingPath = listing?.path ?? folderPath;

  return (
    <div className="fv-body" data-nc-fs-viewer="">
      <aside className="fv-tree" aria-label="Files">
        <div className="fv-tree-head">
          <button
            type="button"
            className="fv-up"
            disabled={listing?.parent == null || listingLoading}
            title="Parent directory"
            aria-label="Parent directory"
            onClick={() => {
              const parent = listing?.parent;
              if (parent != null) setNav((current) => ({ ...current, folderPath: parent }));
            }}
          >
            <span aria-hidden="true">↑</span>
          </button>
          <span className="fv-cwd" title={listingPath}>{listingPath}</span>
        </div>
        <div className="fv-tree-list">
          {listingLoading
            ? <p className="fv-state">Loading…</p>
            : listingError !== null
              ? <FileReadError message={listingError} resource="folder" onRetry={() => setListingRetry((value) => value + 1)} />
              : entries.length === 0
                ? <p className="fv-state">Empty directory</p>
                : entries.map((entry) => {
                  const entryPath = joinDirectoryPath(listingPath, entry.name);
                  return (
                    <button
                      key={entry.name}
                      type="button"
                      className={`fv-entry ${selectedPath === entryPath ? 'fv-entry-selected' : ''}`}
                      title={entry.name}
                      onClick={() => setNav((current) => (entry.is_dir
                        ? { ...current, folderPath: entryPath, selectedPath: null }
                        : { ...current, tab: 'code', selectedPath: entryPath }))}
                    >
                      <span aria-hidden="true">{entry.is_dir ? '▸' : '·'}</span>
                      <span className="fv-entry-name">{entry.name}</span>
                    </button>
                  );
                })}
        </div>
      </aside>

      <section className="fv-main">
        <div className="fv-toolbar">
          {/* No `tabpanel` id wiring: the panes are swapped wholesale, so the hidden one does not exist. */}
          <div className="fv-tabs" role="tablist" aria-label="File viewer mode">
            <button
              type="button"
              role="tab"
              aria-selected={tab === 'code'}
              className={tab === 'code' ? 'fv-tab-active' : 'fv-tab'}
              onClick={() => setNav((current) => ({ ...current, tab: 'code' }))}
            >
              Code
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === 'diff'}
              className={tab === 'diff' ? 'fv-tab-active' : 'fv-tab'}
              onClick={() => setNav((current) => ({ ...current, tab: 'diff' }))}
            >
              Diff
            </button>
          </div>
          <span
            className="fv-selection"
            title={tab === 'diff' ? diffSelected ?? '' : selectedCodePath ?? ''}
          >
            {tab === 'diff'
              ? diffSelected ?? 'No changed file selected'
              : selectedCodePath ?? 'Select a file'}
          </span>
        </div>

        {tab === 'code'
          ? (
            <CodeTab
              state={fileState}
              selectedPath={selectedCodePath}
              theme={theme}
              rawUrl={files.rawUrl}
              onRetry={() => setFileRetry((value) => value + 1)}
              onImageError={() => setFileState({ kind: 'error', message: 'Could not read this image.' })}
            />
          )
          : (
            <DiffTab
              files={changedFiles}
              selected={diffSelected}
              listLoading={diffListLoading}
              error={diffError}
              onRetry={() => gitRoot === null
                ? setDiffListRetry((value) => value + 1)
                : setDiffRetry((value) => value + 1)}
              diff={diff}
              diffLoading={diffLoading}
              theme={theme}
              onSelect={(selected) => setNav((current) => ({ ...current, diffSelected: selected }))}
            />
          )}
      </section>
    </div>
  );
}

function CodeTab({ state, selectedPath, theme, rawUrl, onRetry, onImageError }: {
  state: FileState;
  selectedPath: string | null;
  theme: PaneTheme;
  rawUrl: (path: string) => string;
  onRetry: () => void;
  onImageError: () => void;
}): ReactNode {
  if (selectedPath === null) return <p className="fv-empty">Select a file to view it.</p>;
  if (state.kind === 'idle' || state.kind === 'loading') {
    return <p className="fv-state">Loading file…</p>;
  }
  if (state.kind === 'error') return <FileReadError message={state.message} onRetry={onRetry} />;
  if (state.kind === 'image') {
    return (
      <div className="fv-image-wrap">
        <img className="fv-image" src={rawUrl(state.path)} alt={state.path} onError={onImageError} />
      </div>
    );
  }
  return <LoadedFile path={state.path} text={state.text} truncated={state.truncated} theme={theme} />;
}

/** One file and the find bar over it; the bar is React's and the matching CodeMirror's, meeting at `PaneSearchAdapter`. `/` opens it from inside the editor, where focus is. */
function LoadedFile({ path, text, truncated, theme }: {
  path: string; text: string; truncated: boolean; theme: PaneTheme;
}) {
  const [barOpen, setBarOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [matchCurrent, setMatchCurrent] = useState(0);
  const [matchTotal, setMatchTotal] = useState(0);
  const adapterRef = useRef<PaneSearchAdapter | null>(null);
  const queryRef = useRef('');

  const closeBar = useCallback(() => {
    setBarOpen(false);
    setQuery('');
    setMatchCurrent(0);
    setMatchTotal(0);
    queryRef.current = '';
    adapterRef.current?.setQuery('');
  }, []);

  useEffect(() => { closeBar(); }, [closeBar, path]);

  const onAdapter = useCallback((adapter: PaneSearchAdapter | null) => {
    adapterRef.current = adapter;
    // The pane can remount under a live query; re-running it keeps the highlights.
    if (adapter !== null && queryRef.current !== '') adapter.setQuery(queryRef.current);
  }, []);

  const onCount = useCallback((current: number, total: number) => {
    setMatchCurrent(current);
    setMatchTotal(total);
  }, []);

  return (
    <div className="fv-code-wrap">
      {truncated && <p className="fv-banner">Showing the first 2 MiB of this file.</p>}
      <Suspense fallback={<p className="fv-state">Loading editor…</p>}>
        <LazyCodePane
          path={path}
          text={text}
          theme={theme}
          onSearchAdapterReady={onAdapter}
          onSearchCount={onCount}
          onSlashOpen={() => setBarOpen(true)}
        />
      </Suspense>
      {barOpen && (
        <SearchBar
          query={query}
          current={matchCurrent}
          total={matchTotal}
          onChange={(value) => {
            queryRef.current = value;
            setQuery(value);
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

function SearchBar({ query, current, total, onChange, onNext, onPrev, onClose }: {
  query: string;
  current: number;
  total: number;
  onChange: (value: string) => void;
  onNext: () => void;
  onPrev: () => void;
  onClose: () => void;
}) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  useEffect(() => { inputRef.current?.focus(); }, []);
  const countLabel = total === 0
    ? (query === '' ? '' : 'no match')
    : `${current === 0 ? 1 : current}/${total}`;
  return (
    <div className="fv-search-bar" role="search">
      <input
        ref={inputRef}
        type="search"
        aria-label="Search in file"
        placeholder="Search…"
        value={query}
        onChange={(event) => onChange(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key === 'Escape') { event.preventDefault(); onClose(); return; }
          if (event.key !== 'Enter') return;
          event.preventDefault();
          if (event.shiftKey) onPrev();
          else onNext();
        }}
      />
      <span className="fv-search-count" aria-live="polite">{countLabel}</span>
      <button type="button" aria-label="Previous match" title="Previous match" disabled={total === 0} onClick={onPrev}>
        <span aria-hidden="true">↑</span>
      </button>
      <button type="button" aria-label="Next match" title="Next match" disabled={total === 0} onClick={onNext}>
        <span aria-hidden="true">↓</span>
      </button>
      <button type="button" aria-label="Close search" title="Close search" onClick={onClose}>
        <span aria-hidden="true">×</span>
      </button>
    </div>
  );
}

function DiffTab({ files, selected, listLoading, error, diff, diffLoading, theme, onSelect, onRetry }: {
  files: readonly GitChangedFileWire[];
  selected: string | null;
  listLoading: boolean;
  error: string | null;
  diff: GitDiffWire | null;
  diffLoading: boolean;
  theme: PaneTheme;
  onSelect: (path: string) => void;
  onRetry: () => void;
}): ReactNode {
  return (
    <div className="fv-diff">
      <div className="fv-changes" aria-label="Changed files">
        {listLoading
          ? <p className="fv-state">Loading changes…</p>
          : files.length === 0
            ? error === null && <p className="fv-state">No working-tree changes</p>
            : files.map((file) => (
              <button
                key={`${file.status}:${file.path}`}
                type="button"
                className={`fv-change ${selected === file.path ? 'fv-change-selected' : ''}`}
                title={`${file.status} ${file.path}`}
                onClick={() => onSelect(file.path)}
              >
                <span className="fv-status" data-nc-fs-status={file.status}>
                  {statusLabel(file.status)}
                </span>
                <span className="fv-change-name">{file.path}</span>
              </button>
            ))}
      </div>
      <div className="fv-diff-pane">
        {error !== null
          ? <FileReadError message={error} resource="changes" onRetry={onRetry} />
          : diffLoading || diff === null
            ? <p className="fv-state">{selected === null ? 'Select a changed file' : 'Loading diff…'}</p>
            : (
              <>
                {diff.truncated && <p className="fv-banner">Showing the first 2 MiB of this file.</p>}
                <Suspense fallback={<p className="fv-state">Loading diff editor…</p>}>
                  <LazyDiffPane
                    path={diff.path}
                    headText={diff.head_text}
                    workingText={diff.working_text}
                    theme={theme}
                  />
                </Suspense>
              </>
            )}
      </div>
    </div>
  );
}
