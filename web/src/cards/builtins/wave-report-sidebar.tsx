import { useMemo, useRef, type KeyboardEvent, type ReactNode } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { CalmApiError, type WaveFsEntry } from '../../api/calm';
import {
  useWaveFileContent,
  useWaveFileList,
} from '../../api/queries';
import { useTheme } from '../../app/theme';
import { useState } from '../../shared/state';
import { CodePane } from './file-viewer-codemirror';

export interface WaveReportSidebarProps {
  waveId: string;
  fallback?: ReactNode;
}

export function WaveReportSidebar({ waveId, fallback }: WaveReportSidebarProps) {
  return <WaveReportSidebarState key={waveId} waveId={waveId} fallback={fallback} />;
}

function WaveReportSidebarState({ waveId, fallback }: WaveReportSidebarProps) {
  const [expandedDirs, setExpandedDirs] = useState<Set<string>>(() => new Set());
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
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

  const toggleDir = (path: string) => {
    setExpandedDirs((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };
  const defaultFocusedPath = rootQ.data?.[0] ? joinPath('', rootQ.data[0].name) : null;

  const focusItem = (path: string) => {
    const item = visibleTreeItems(treeRef.current).find((el) => el.dataset.path === path);
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
      else setSelectedPath(path);
    }
  };

  return (
    <div className="wave-report-files" data-testid="wave-report-files">
      <ul
        ref={treeRef}
        role="tree"
        className="wave-report-files-tree"
        aria-label="Wave files"
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
          focusedPath={focusedPath}
          defaultFocusedPath={defaultFocusedPath}
          cardKinds={cardKinds}
          onToggleDir={toggleDir}
          onSelectFile={setSelectedPath}
          onFocusItem={setFocusedPath}
          onItemKeyDown={handleTreeItemKeyDown}
        />
      </ul>
      <WaveFileViewer waveId={waveId} selectedPath={selectedPath} fallback={fallback} />
    </div>
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
  onToggleDir,
  onSelectFile,
  onFocusItem,
  onItemKeyDown,
}: DirectoryBodyProps) {
  if (loading) {
    return <TreeState depth={depth} label="Loading..." />;
  }
  if (error) {
    return <TreeError error={error} depth={depth} />;
  }
  if (!entries || entries.length === 0) {
    return <TreeState depth={depth} label={emptyLabel} />;
  }
  return (
    <>
      {entries.map((entry) => {
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

function WaveFileViewer({
  waveId,
  selectedPath,
  fallback,
}: {
  waveId: string;
  selectedPath: string | null;
  fallback?: ReactNode;
}) {
  const { resolved: theme } = useTheme();
  const contentQ = useWaveFileContent(waveId, selectedPath);

  if (!selectedPath) {
    if (fallback) {
      return <div className="wave-report-files-viewer">{fallback}</div>;
    }
    return (
      <div className="wave-report-files-viewer wave-report-files-viewer-empty">
        Select a file
      </div>
    );
  }
  if (contentQ.isLoading) {
    return (
      <div className="wave-report-files-viewer wave-report-files-viewer-empty">
        Loading...
      </div>
    );
  }
  if (contentQ.error) {
    return (
      <div className="wave-report-files-viewer">
        <InlineApiError error={contentQ.error} />
      </div>
    );
  }
  if (!contentQ.data) {
    return (
      <div className="wave-report-files-viewer wave-report-files-viewer-empty">
        Select a file
      </div>
    );
  }

  if (contentQ.data.content_type === 'text/markdown') {
    return (
      <div className="wave-report-files-viewer wave-report-files-markdown">
        <ReactMarkdown remarkPlugins={[remarkGfm]}>
          {contentQ.data.content}
        </ReactMarkdown>
      </div>
    );
  }

  return (
    <div className="wave-report-files-viewer wave-report-files-code-wrap">
      <CodePane path={selectedPath} text={contentQ.data.content} theme={theme} />
    </div>
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
    key === 'Enter'
  );
}
