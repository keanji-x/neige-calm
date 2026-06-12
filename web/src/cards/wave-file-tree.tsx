import { useMemo, useRef, type KeyboardEvent, type ReactNode } from 'react';
import { CalmApiError, type WaveFsEntry } from '../api/calm';
import { useWaveFileContent, useWaveFileList } from '../api/queries';
import { useState } from '../shared/state';

export interface WaveFileTreeProps {
  waveId: string;
  selectedPath: string | null;
  onSelectedPathChange: (path: string | null) => void;
  /** Tree root aria-label. Defaults to "Wave files". */
  ariaLabel?: string;
  /** Whether to show file suffixes and directory count meta tags. */
  showCounts?: boolean;
  /** Whether to show dot-prefixed entries. Defaults to false. */
  showHidden?: boolean;
  /** Optional fallback UI when the root query is loading or empty. */
  fallback?: ReactNode;
}

export function WaveFileTree({
  waveId,
  selectedPath,
  onSelectedPathChange,
  ariaLabel = 'Wave files',
  showCounts = false,
  showHidden = false,
  fallback,
}: WaveFileTreeProps) {
  return (
    <WaveFileTreeState
      key={waveId}
      waveId={waveId}
      selectedPath={selectedPath}
      onSelectedPathChange={onSelectedPathChange}
      ariaLabel={ariaLabel}
      showCounts={showCounts}
      showHidden={showHidden}
      fallback={fallback}
    />
  );
}

function WaveFileTreeState({
  waveId,
  selectedPath,
  onSelectedPathChange,
  ariaLabel,
  showCounts,
  showHidden,
  fallback,
}: Required<Pick<WaveFileTreeProps, 'ariaLabel' | 'showCounts' | 'showHidden'>> &
  Omit<WaveFileTreeProps, 'ariaLabel' | 'showCounts' | 'showHidden'>) {
  const [expandedDirs, setExpandedDirs] = useState<Set<string>>(() => new Set());
  const [focusedPath, setFocusedPath] = useState<string | null>(null);
  const treeRef = useRef<HTMLUListElement>(null);
  const rootQ = useWaveFileList(waveId, '');
  const cardIndexQ = useWaveFileContent(waveId, 'cards/index.json', {
    enabled: expandedDirs.has('cards'),
  });
  const cardKinds = useMemo(
    () => parseCardKinds(cardIndexQ.data?.content),
    [cardIndexQ.data?.content],
  );
  const visibleRootEntries = useMemo(
    () => filterVisibleEntries(rootQ.data, showHidden),
    [rootQ.data, showHidden],
  );

  const toggleDir = (path: string) => {
    setExpandedDirs((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };
  const defaultFocusedPath = visibleRootEntries?.[0]
    ? joinPath('', visibleRootEntries[0].name)
    : null;
  const visibleFocusedPath =
    !showHidden && focusedPath && pathHasHiddenSegment(focusedPath)
      ? null
      : focusedPath;

  const focusItem = (path: string) => {
    const item = visibleTreeItems(treeRef.current).find(
      (el) => el.dataset.path === path,
    );
    if (!item) return;
    setFocusedPath(path);
    item.focus();
  };

  const handleTreeItemKeyDown = (event: KeyboardEvent<HTMLLIElement>) => {
    const target = event.currentTarget;
    const path = target.dataset.path;
    if (!path) return;
    if (!isTreeNavigationKey(event.key)) return;
    event.stopPropagation();

    const items = visibleTreeItems(treeRef.current);
    const index = items.indexOf(target);
    const isDir = target.dataset.kind === 'dir';
    const expanded = target.getAttribute('aria-expanded') === 'true';

    if (event.key === 'ArrowDown') {
      event.preventDefault();
      const next = items[Math.min(index + 1, items.length - 1)];
      if (next?.dataset.path) focusItem(next.dataset.path);
      return;
    }
    if (event.key === 'ArrowUp') {
      event.preventDefault();
      const prev = items[Math.max(index - 1, 0)];
      if (prev?.dataset.path) focusItem(prev.dataset.path);
      return;
    }
    if (event.key === 'Home') {
      event.preventDefault();
      const first = items[0];
      if (first?.dataset.path) focusItem(first.dataset.path);
      return;
    }
    if (event.key === 'End') {
      event.preventDefault();
      const last = items[items.length - 1];
      if (last?.dataset.path) focusItem(last.dataset.path);
      return;
    }
    if (event.key === 'ArrowRight' && isDir) {
      event.preventDefault();
      if (!expanded) {
        toggleDir(path);
        return;
      }
      const child = items.find((item) => item.dataset.parentPath === path);
      if (child?.dataset.path) focusItem(child.dataset.path);
      return;
    }
    if (event.key === 'ArrowLeft') {
      event.preventDefault();
      if (isDir && expanded) {
        toggleDir(path);
        return;
      }
      const parentPath = target.dataset.parentPath;
      if (parentPath) focusItem(parentPath);
      return;
    }
    if (event.key === 'Enter') {
      event.preventDefault();
      if (isDir) toggleDir(path);
      else onSelectedPathChange(path);
    }
  };

  return (
    <ul
      ref={treeRef}
      role="tree"
      className="wave-report-files-tree"
      aria-label={ariaLabel}
    >
      <DirectoryBody
        waveId={waveId}
        path=""
        entries={rootQ.data}
        error={rootQ.error}
        loading={rootQ.isLoading}
        emptyLabel="No files"
        depth={0}
        expandedDirs={expandedDirs}
        selectedPath={selectedPath}
        focusedPath={visibleFocusedPath}
        defaultFocusedPath={defaultFocusedPath}
        cardKinds={cardKinds}
        showCounts={showCounts}
        showHidden={showHidden}
        rootFallback={fallback}
        onToggleDir={toggleDir}
        onSelectFile={onSelectedPathChange}
        onFocusItem={setFocusedPath}
        onItemKeyDown={handleTreeItemKeyDown}
      />
    </ul>
  );
}

interface DirectoryBodyProps {
  waveId: string;
  path: string;
  entries: WaveFsEntry[] | undefined;
  error: Error | null;
  loading: boolean;
  emptyLabel: string;
  depth: number;
  expandedDirs: Set<string>;
  selectedPath: string | null;
  focusedPath: string | null;
  defaultFocusedPath: string | null;
  cardKinds: Map<string, string>;
  showCounts: boolean;
  showHidden: boolean;
  rootFallback?: ReactNode;
  onToggleDir: (path: string) => void;
  onSelectFile: (path: string) => void;
  onFocusItem: (path: string) => void;
  onItemKeyDown: (event: KeyboardEvent<HTMLLIElement>) => void;
}

function DirectoryBody({
  waveId,
  path,
  entries,
  error,
  loading,
  emptyLabel,
  depth,
  expandedDirs,
  selectedPath,
  focusedPath,
  defaultFocusedPath,
  cardKinds,
  showCounts,
  showHidden,
  rootFallback,
  onToggleDir,
  onSelectFile,
  onFocusItem,
  onItemKeyDown,
}: DirectoryBodyProps) {
  const visibleEntries = useMemo(
    () => filterVisibleEntries(entries, showHidden),
    [entries, showHidden],
  );

  if (loading) {
    return rootFallback && depth === 0 ? (
      <TreeFallback depth={depth}>{rootFallback}</TreeFallback>
    ) : (
      <TreeState depth={depth} label="Loading..." />
    );
  }
  if (error) {
    return <TreeError error={error} depth={depth} />;
  }
  if (!visibleEntries || visibleEntries.length === 0) {
    return rootFallback && depth === 0 ? (
      <TreeFallback depth={depth}>{rootFallback}</TreeFallback>
    ) : (
      <TreeState depth={depth} label={emptyLabel} />
    );
  }
  return (
    <>
      {visibleEntries.map((entry) => {
        const entryPath = joinPath(path, entry.name);
        return (
          <TreeEntry
            key={`${entry.kind}:${entryPath}`}
            waveId={waveId}
            entry={entry}
            path={entryPath}
            parentPath={path}
            depth={depth}
            expandedDirs={expandedDirs}
            selectedPath={selectedPath}
            focusedPath={focusedPath}
            defaultFocusedPath={defaultFocusedPath}
            cardKinds={cardKinds}
            showCounts={showCounts}
            showHidden={showHidden}
            onToggleDir={onToggleDir}
            onSelectFile={onSelectFile}
            onFocusItem={onFocusItem}
            onItemKeyDown={onItemKeyDown}
          />
        );
      })}
    </>
  );
}

interface TreeEntryProps {
  waveId: string;
  entry: WaveFsEntry;
  path: string;
  parentPath: string;
  depth: number;
  expandedDirs: Set<string>;
  selectedPath: string | null;
  focusedPath: string | null;
  defaultFocusedPath: string | null;
  cardKinds: Map<string, string>;
  showCounts: boolean;
  showHidden: boolean;
  onToggleDir: (path: string) => void;
  onSelectFile: (path: string) => void;
  onFocusItem: (path: string) => void;
  onItemKeyDown: (event: KeyboardEvent<HTMLLIElement>) => void;
}

function TreeEntry({
  waveId,
  entry,
  path,
  parentPath,
  depth,
  expandedDirs,
  selectedPath,
  focusedPath,
  defaultFocusedPath,
  cardKinds,
  showCounts,
  showHidden,
  onToggleDir,
  onSelectFile,
  onFocusItem,
  onItemKeyDown,
}: TreeEntryProps) {
  const isDir = isDirectory(entry);
  const expanded = isDir && expandedDirs.has(path);
  const label = entryLabel(entry, parentPath, cardKinds);
  const childQ = useWaveFileList(waveId, path, { enabled: expanded });
  const tabPath = focusedPath ?? defaultFocusedPath;
  const meta = showCounts ? entryMeta(entry, isDir) : null;

  return (
    <li
      role="treeitem"
      aria-label={label}
      aria-level={depth + 1}
      aria-expanded={isDir ? expanded : undefined}
      aria-selected={isDir ? false : selectedPath === path}
      tabIndex={tabPath === path ? 0 : -1}
      data-path={path}
      data-parent-path={parentPath}
      data-kind={isDir ? 'dir' : 'file'}
      className="wave-report-files-item"
      onFocus={(event) => {
        if (event.target === event.currentTarget) onFocusItem(path);
      }}
      onKeyDown={onItemKeyDown}
      onClick={(event) => {
        event.stopPropagation();
        event.currentTarget.focus();
        onFocusItem(path);
        if (isDir) onToggleDir(path);
        else onSelectFile(path);
      }}
    >
      <div
        className={[
          'wave-report-files-row',
          isDir ? 'is-dir' : 'is-file',
          selectedPath === path ? 'is-selected' : '',
        ].join(' ')}
        style={{ paddingLeft: `${8 + depth * 14}px` }}
      >
        <span aria-hidden="true" className="wave-report-files-caret">
          {isDir ? (expanded ? '▾' : '▸') : ''}
        </span>
        <span className="wave-report-files-label">{label}</span>
        {meta && (
          <span aria-hidden="true" className="wave-report-files-meta">
            {meta}
          </span>
        )}
      </div>
      {expanded && (
        <ul role="group" className="wave-report-files-group">
          <DirectoryBody
            waveId={waveId}
            path={path}
            entries={childQ.data}
            error={childQ.error}
            loading={childQ.isLoading}
            emptyLabel="Empty"
            depth={depth + 1}
            expandedDirs={expandedDirs}
            selectedPath={selectedPath}
            focusedPath={focusedPath}
            defaultFocusedPath={defaultFocusedPath}
            cardKinds={cardKinds}
            showCounts={showCounts}
            showHidden={showHidden}
            onToggleDir={onToggleDir}
            onSelectFile={onSelectFile}
            onFocusItem={onFocusItem}
            onItemKeyDown={onItemKeyDown}
          />
        </ul>
      )}
    </li>
  );
}

function InlineApiError({
  error,
  depth = 0,
}: {
  error: Error;
  depth?: number;
}) {
  return (
    <div
      role="alert"
      className="wave-report-files-error"
      style={{ paddingLeft: depth > 0 ? `${8 + depth * 14}px` : undefined }}
    >
      {formatApiError(error)}
    </div>
  );
}

function TreeState({ depth, label }: { depth: number; label: string }) {
  return (
    <li
      role="treeitem"
      aria-disabled="true"
      aria-selected={false}
      className="wave-report-files-state"
      tabIndex={-1}
      style={{ paddingLeft: `${8 + depth * 14}px` }}
    >
      {label}
    </li>
  );
}

function TreeFallback({
  depth,
  children,
}: {
  depth: number;
  children: ReactNode;
}) {
  return (
    <li
      role="treeitem"
      aria-disabled="true"
      aria-selected={false}
      className="wave-report-files-state"
      tabIndex={-1}
      style={{ paddingLeft: `${8 + depth * 14}px` }}
    >
      {children}
    </li>
  );
}

function TreeError({ error, depth }: { error: Error; depth: number }) {
  return (
    <li role="treeitem" aria-disabled="true" aria-selected={false} tabIndex={-1}>
      <InlineApiError error={error} depth={depth} />
    </li>
  );
}

function parseCardKinds(content: string | undefined): Map<string, string> {
  const out = new Map<string, string>();
  if (!content) return out;
  try {
    const parsed = JSON.parse(content);
    if (!Array.isArray(parsed)) return out;
    for (const item of parsed) {
      if (!item || typeof item !== 'object') continue;
      const id = (item as { id?: unknown }).id;
      const kind = (item as { kind?: unknown }).kind;
      if (typeof id === 'string' && typeof kind === 'string') {
        out.set(id, kind);
      }
    }
  } catch {
    return out;
  }
  return out;
}

function formatApiError(error: Error): string {
  if (error instanceof CalmApiError) {
    return error.message || error.code || `HTTP ${error.status}`;
  }
  return error.message || 'Request failed';
}

function isDirectory(entry: WaveFsEntry): boolean {
  return entry.kind === 'dir' || entry.name.endsWith('/');
}

function filterVisibleEntries(
  entries: WaveFsEntry[] | undefined,
  showHidden: boolean,
): WaveFsEntry[] | undefined {
  if (!entries || showHidden) return entries;
  return entries.filter((entry) => !isHiddenEntry(entry));
}

function isHiddenEntry(entry: WaveFsEntry): boolean {
  // Dotfile convention per #664: wave fs names internal lenses with a "."
  // prefix; UI hides them like ls.
  return entry.name.replace(/^\/+/, '').startsWith('.');
}

function pathHasHiddenSegment(path: string): boolean {
  return path.split('/').some((segment) => segment.startsWith('.'));
}

function joinPath(parent: string, name: string): string {
  const cleanName = name.replace(/^\/+|\/+$/g, '');
  return parent ? `${parent}/${cleanName}` : cleanName;
}

function entryLabel(
  entry: WaveFsEntry,
  parentPath: string,
  cardKinds: Map<string, string>,
): string {
  if (parentPath === 'cards' && isDirectory(entry)) {
    const cardId = entry.name.replace(/^\/+|\/+$/g, '');
    return `${cardKinds.get(cardId) ?? 'card'} ${truncateId(cardId)}`;
  }
  return entry.name;
}

function entryMeta(entry: WaveFsEntry, isDir: boolean): string | null {
  if (isDir) {
    return typeof entry.size === 'number' ? String(entry.size) : null;
  }

  const cleanName = entry.name.replace(/^\/+|\/+$/g, '');
  const lastSlash = cleanName.lastIndexOf('/');
  const leaf = lastSlash >= 0 ? cleanName.slice(lastSlash + 1) : cleanName;
  const dot = leaf.lastIndexOf('.');
  if (dot <= 0 || dot === leaf.length - 1) return null;
  return leaf.slice(dot).toLowerCase();
}

function truncateId(id: string): string {
  return id.length <= 8 ? id : id.slice(0, 8);
}

function visibleTreeItems(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(
    root.querySelectorAll<HTMLElement>(
      '[role="treeitem"][data-path]:not([aria-disabled="true"])',
    ),
  );
}

function isTreeNavigationKey(key: string): boolean {
  return (
    key === 'ArrowDown' ||
    key === 'ArrowUp' ||
    key === 'ArrowRight' ||
    key === 'ArrowLeft' ||
    key === 'Home' ||
    key === 'End' ||
    key === 'Enter'
  );
}
