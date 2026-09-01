// The file card's body: a folder on the left, and one file — or one file's two
// sides — on the right.
//
// Ported from `web/src/cards/builtins/file-viewer.tsx`, with three deliberate
// differences, all recorded here rather than left to be discovered:
//
//  1. **Its reads arrive as a port.** The old card imported the API module
//     directly. A card here is rendered inside `systems/**`, which holds no
//     transport, so the reads come in as `CardFilesPort` — built once at the
//     composition layer against the same transport and 401 channel as every
//     other read (see `core/domain/fs.ts`). `files === null` is a real state
//     and is rendered as one.
//
//  2. **Its navigation lives in the card's slots, not in a server overlay.**
//     The old card persisted `{tab, folderPath, selectedPath, diffSelected}` as
//     a kernel overlay, so a reload came back where you left off. Slots keep it
//     for the life of the mounted card, which covers what the board actually
//     does to a card (hide it, show it, resize it) and costs no round-trip. A
//     reload starts at the card's own path again. That is a real reduction and
//     the honest place to record it is here.
//
//  3. **Markdown renders as text, not as a preview.** The old card ran
//     `react-markdown` + `remark-gfm` in a second pane, with its own TOC. This
//     repo deleted that dependency on purpose — `core/markdown` is the one
//     markdown path (INV-DUP-004 / INV-DUP-005) — so re-adding it to render a
//     file would be standing up the second implementation the invariant exists
//     to forbid. A `.md` file therefore opens in the code pane, highlighted as
//     markdown. Rendering it through `core/markdown` is a real follow-up; it is
//     not a line of glue, because the report renderer that consumes that AST
//     lives in `features/report` and a system may not import a feature.

import { Suspense, lazy, useCallback, useEffect, useRef, type ReactNode } from 'react';

import type { CardFilesPort, DirectoryListingWire, GitChangedFileWire, GitDiffWire } from '../../../../core/domain/fs.ts';
import { joinDirectoryPath } from '../../ui/directory-browser/public.tsx';
import { useState } from '../../ui/state/public.ts';
import type { PaneSearchAdapter, PaneTheme } from './code-pane.tsx';

const LazyCodePane = lazy(() => import('./code-pane.tsx').then((module) => ({ default: module.CodePane })));
const LazyDiffPane = lazy(() => import('./code-pane.tsx').then((module) => ({ default: module.DiffPane })));

const IMAGE_EXTENSIONS = Object.freeze([
  '.png', '.jpg', '.jpeg', '.gif', '.webp', '.bmp', '.ico', '.svg',
] as const);

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

function isImagePath(path: string): boolean {
  const lower = path.toLowerCase();
  return IMAGE_EXTENSIONS.some((extension) => lower.endsWith(extension));
}

/** `null` at the filesystem root, where there is no parent to climb to. */
function parentPath(path: string): string | null {
  const trimmed = path.replace(/\/+$/, '');
  const index = trimmed.lastIndexOf('/');
  if (index <= 0) return index === 0 ? '/' : null;
  return trimmed.slice(0, index);
}

function messageOf(error: unknown, fallback: string): string {
  return error instanceof Error && error.message !== '' ? error.message : fallback;
}

/** The kernel's status word, as the one letter a list column has room for. */
function statusLabel(status: string): string {
  switch (status) {
    case 'added': return 'A';
    case 'deleted': return 'D';
    case 'renamed': return 'R';
    case 'untracked': return '?';
    default: return 'M';
  }
}

/**
 * The slice of the card host's slot store this viewer uses.
 *
 * Declared here rather than imported from `systems/cards`: the card system is
 * reachable only through its public entry (`cards-public-entry-only`), and
 * importing that entry from a module the entry's own built-ins import would be
 * a cycle. Structural typing is what makes the real `CardSlotStore` assignable
 * to it — and stating only the two methods used is also the honest declaration
 * of what a viewer is allowed to do with a card's slots.
 */
export interface ViewerSlots {
  get<Value>(key: string, initial: Value | (() => Value)): Value;
  set<Value>(key: string, value: Value): void;
}

export type FileViewerProps = Readonly<{
  /** The card's own path: the folder it opens in, and what "reset" means. */
  path: string;
  /** The reads, or `null` on a host assembled without them. */
  files: CardFilesPort | null;
  theme: PaneTheme;
  /** The mounted card's slots, so navigation survives hide/show. */
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

  /*
   * The listing — and exactly one climb, from the card's own path only.
   *
   * A card can be created on a *file*. `seedNav` puts that path in `folderPath`
   * as well as in `selectedPath`, and `listDirectory` answers 400 for a file, so
   * the card's own path is the one place a listing failure is expected rather
   * than informative: the parent is the folder that file lives in, which is what
   * the left column is for. `selectedPath` is deliberately left alone in that
   * branch — the file stays selected, and the effect below reads it into the
   * pane beside the listing, which is the whole content of a file card. The
   * other reason a card's own folder fails to list is that it was moved or
   * deleted under a card that outlived it, and the nearest folder that does
   * exist is the useful answer there too.
   *
   * Anything else shows the error instead. A folder the reader navigated *into*
   * deserves to be told why it could not be read, and because only the card's
   * own path may climb, a chain of unreadable ancestors stops after one step
   * rather than walking the card up to `/`.
   */
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
  }, [files, folderPath, path]);

  /*
   * A card opened on a *folder* selects that folder, and a folder is not a file
   * to read — so the selection only becomes a read once it names something the
   * listing did not just report as the folder itself.
   */
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
  }, [files, selectedCodePath, tab]);

  /* The changed-file list, and the selection inside it. A selection that no
     longer appears in the status falls back to the first row rather than
     leaving the pane pointed at a file that is no longer changed. */
  useEffect(() => {
    if (files === null || tab !== 'diff') return;
    let cancelled = false;
    setDiffListLoading(true);
    setDiffError(null);
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
  }, [files, folderPath, tab]);

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
  }, [changedFiles, diffSelected, files, gitRoot, tab]);

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
              ? <p className="fv-error" role="alert">{listingError}</p>
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
          {/*
            Two tabs over one folder: what a file *is*, and what changed in it.
            `role="tablist"` with no `tabpanel` id wiring, exactly as the panes
            below are swapped wholesale rather than kept mounted — the pane is
            the panel, and pointing at it by id would be a promise that the
            hidden one still exists.
          */}
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
            />
          )
          : (
            <DiffTab
              files={changedFiles}
              selected={diffSelected}
              listLoading={diffListLoading}
              error={diffError}
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

function CodeTab({ state, selectedPath, theme, rawUrl }: {
  state: FileState;
  selectedPath: string | null;
  theme: PaneTheme;
  rawUrl: (path: string) => string;
}): ReactNode {
  if (selectedPath === null) return <p className="fv-empty">Select a file to view it.</p>;
  if (state.kind === 'idle' || state.kind === 'loading') {
    return <p className="fv-state">Loading file…</p>;
  }
  if (state.kind === 'error') return <p className="fv-error" role="alert">{state.message}</p>;
  if (state.kind === 'image') {
    return (
      <div className="fv-image-wrap">
        <img className="fv-image" src={rawUrl(state.path)} alt={state.path} />
      </div>
    );
  }
  return <LoadedFile path={state.path} text={state.text} truncated={state.truncated} theme={theme} />;
}

/**
 * One file, and the find bar over it.
 *
 * The bar is React's and the matching is CodeMirror's: they meet at
 * `PaneSearchAdapter`, which the pane hands over once it has an editor. The
 * handshake is what lets the bar exist at all without this file importing
 * CodeMirror — and what lets a future pane of a different kind (a rendered
 * markdown preview, say) drive the same bar.
 *
 * `/` opens it, from inside the editor, which is why the pane needs
 * `onSlashOpen` rather than this component listening for a key it would never
 * receive: focus is inside CodeMirror while you are reading.
 */
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

  /* A new file is a new document: leaving the previous file's query live would
     report a count against text nobody is looking at. */
  useEffect(() => { closeBar(); }, [closeBar, path]);

  const onAdapter = useCallback((adapter: PaneSearchAdapter | null) => {
    adapterRef.current = adapter;
    // The pane can remount under a live query; re-running it is what keeps the
    // highlights from disappearing while the bar still says "3/12".
    if (adapter !== null && queryRef.current !== '') adapter.setQuery(queryRef.current);
  }, []);

  const onCount = useCallback((current: number, total: number) => {
    setMatchCurrent(current);
    setMatchTotal(total);
  }, []);

  return (
    <div className="fv-code-wrap">
      {/* The cap is the kernel's, and a viewer that did not say so would be
          showing a prefix as though it were the file. */}
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
  /* Nothing typed says nothing at all — `0/0` on an empty box would be a count
     of a search that has not happened. */
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

function DiffTab({ files, selected, listLoading, error, diff, diffLoading, theme, onSelect }: {
  files: readonly GitChangedFileWire[];
  selected: string | null;
  listLoading: boolean;
  error: string | null;
  diff: GitDiffWire | null;
  diffLoading: boolean;
  theme: PaneTheme;
  onSelect: (path: string) => void;
}): ReactNode {
  return (
    <div className="fv-diff">
      <div className="fv-changes" aria-label="Changed files">
        {listLoading
          ? <p className="fv-state">Loading changes…</p>
          : files.length === 0
            ? <p className="fv-state">No working-tree changes</p>
            : files.map((file) => (
              <button
                key={`${file.status}:${file.path}`}
                type="button"
                className={`fv-change ${selected === file.path ? 'fv-change-selected' : ''}`}
                title={`${file.status} ${file.path}`}
                onClick={() => onSelect(file.path)}
              >
                {/* The letter is a shorthand for the word, never the only
                    carrier of it: the row's own title says `modified src/x.rs`
                    in full, so a reader who cannot tell M from R has the word. */}
                <span className="fv-status" data-nc-fs-status={file.status}>
                  {statusLabel(file.status)}
                </span>
                <span className="fv-change-name">{file.path}</span>
              </button>
            ))}
      </div>
      <div className="fv-diff-pane">
        {error !== null
          ? <p className="fv-error" role="alert">{error}</p>
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
