// Filesystem directory picker.
//
// Read-only path field + a fullscreen browse view that walks the host
// filesystem via `GET /api/fs/listdir`. Backs the `'directory'` field
// type in `SchemaForm`; today's only call site is the codex card's
// `cwd` field.
//
// UX shape:
//
//   1. Field view (default): shows the currently-selected path or a
//      "Choose a directory…" placeholder and a "Browse…" affordance.
//   2. Browser view: invoked by clicking Browse. When the picker is
//      nested inside a `<Modal>`, the browser **takes over the modal
//      body** via `useModalView()` — no popover, no second layer, no
//      clipping. When the picker is rendered outside a modal (not
//      currently used in-app), we render the same browser inline below
//      the field as a graceful fallback.
//
// We deliberately do NOT use a portal popover anymore. The previous
// implementation rendered the directory list as a fixed-position
// floating panel anchored to the field; it constantly fought the
// modal's flex sizing and max-height, leaving users with a tiny clipped
// list. Promoting the browser into the modal body sidesteps that whole
// class of layout bugs.

import { useCallback, useEffect, useId, useMemo, useRef } from 'react';
import type { KeyboardEvent } from 'react';
import { useState } from '../state';
import * as api from '../../api/calm';
import { CalmApiError } from '../../api/calm';
import type { ListdirResponse } from '../../api/wire';
import { useModalView } from '../../ui/Dialog/Dialog';

export interface DirectoryPickerProps {
  value: string;
  onChange: (path: string) => void;
  id?: string;
  placeholder?: string;
  mode?: 'directory' | 'file';
}

export function DirectoryPicker({
  value,
  onChange,
  id,
  placeholder = 'Choose a directory…',
  mode = 'directory',
}: DirectoryPickerProps) {
  const [browsing, setBrowsing] = useState(false);
  const modalView = useModalView();

  const startBrowse = () => setBrowsing(true);
  const cancelBrowse = () => setBrowsing(false);
  const commitBrowse = (path: string) => {
    onChange(path);
    setBrowsing(false);
  };

  // When inside a Modal: push the browser into the modal body. When
  // outside (no context): render inline below the field as a fallback.
  // Either way, only one "surface" is visible at a time.
  useEffect(() => {
    if (!modalView) return;
    if (!browsing) {
      modalView.popView();
      return;
    }
    modalView.pushView({
      title: mode === 'file' ? 'Choose a file or folder' : 'Choose a directory',
      onEscape: cancelBrowse,
      body: (
        <DirectoryBrowser
          initialPath={value || null}
          onCancel={cancelBrowse}
          onSelect={commitBrowse}
          mode={mode}
        />
      ),
    });
    // popView on unmount so navigating away while the browser is up
    // (e.g. modal closes) doesn't leak the view-state.
    return () => {
      modalView.popView();
    };
    // We only want to react to the *browsing* toggle. The browser
    // captures `value` at push-time as its starting directory; further
    // value changes shouldn't replay the push.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [browsing, modalView]);

  return (
    <div className="dirpicker-wrap">
      <button
        type="button"
        id={id}
        className={`dirpicker-field${browsing ? ' dirpicker-field-open' : ''}`}
        onClick={startBrowse}
        aria-haspopup="dialog"
        title={value || placeholder}
      >
        <span className={`dirpicker-value${value ? '' : ' dirpicker-value-empty'}`}>
          {value || placeholder}
        </span>
        <span className="dirpicker-browse">Browse…</span>
      </button>
      {/* Fallback path: outside a Modal we still need somewhere to put
          the browser. Inline-below is functional even if not as roomy
          as the modal-takeover path; no in-app caller uses this today. */}
      {browsing && !modalView && (
        <div className="dirpicker-inline">
          <DirectoryBrowser
            initialPath={value || null}
            onCancel={cancelBrowse}
            onSelect={commitBrowse}
            mode={mode}
          />
        </div>
      )}
    </div>
  );
}

export interface DirectoryBrowserProps {
  /** Where to start the listing. `null` → server defaults to $HOME. */
  initialPath: string | null;
  onCancel: () => void;
  onSelect: (path: string) => void;
  mode?: 'directory' | 'file';
  /** Label on the confirm button. Default "Select this directory"; the
   *  shortcut path uses "Create here" since selecting *is* the create. */
  selectLabel?: string;
}

type DirEntry = ListdirResponse['entries'][number];

/**
 * Stateful directory walker. Owns the current path / listing / loading
 * / error state; emits Cancel + Select back to the parent picker. Lives
 * inside the modal body when used through `useModalView()`, so it can
 * happily fill the available height.
 */
export function DirectoryBrowser({
  initialPath,
  onCancel,
  onSelect,
  mode = 'directory',
  selectLabel = mode === 'file' ? 'Select current folder' : 'Select this directory',
}: DirectoryBrowserProps) {
  // `browsePath === null` on first render → the request hits the
  // server with no path argument and the server canonicalizes to $HOME.
  // We seed it with `initialPath` (the field's current value, if any)
  // so reopening the browser lands where the user left off.
  const [browsePath, setBrowsePath] = useState<string | null>(
    initialPath ? normalizeDirectoryPath(initialPath) : null,
  );
  const [parent, setParent] = useState<string | null>(null);
  const [entries, setEntries] = useState<ListdirResponse['entries']>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pathText, setPathText] = useState(() =>
    initialPath ? seedPath(initialPath) : '',
  );
  const [activeIndex, setActiveIndex] = useState<number | null>(null);
  const pathInputRef = useRef<HTMLInputElement | null>(null);
  const pathTextRef = useRef(pathText);
  const requestSeqRef = useRef(0);
  const refocusAfterLoadRef = useRef(false);
  const optionIdPrefix = useId();
  const pathParts = useMemo(() => splitPathText(pathText), [pathText]);
  const basenameFilter = pathParts.basename;
  const updatePathText = useCallback((nextPathText: string) => {
    pathTextRef.current = nextPathText;
    setPathText(nextPathText);
  }, []);

  const isInteractive = useCallback(
    (entry: DirEntry) => entry.is_dir || mode === 'file',
    [mode],
  );

  const visibleEntries = useMemo(() => {
    if (!basenameFilter) return entries;
    const needle = basenameFilter.toLowerCase();
    return entries.filter((entry) => entry.name.toLowerCase().startsWith(needle));
  }, [basenameFilter, entries]);

  const findFirstInteractiveIndex = useCallback(
    (candidateEntries: DirEntry[]) => {
      const index = candidateEntries.findIndex(isInteractive);
      return index === -1 ? null : index;
    },
    [isInteractive],
  );

  const findNextInteractiveIndex = useCallback(
    (current: number | null, direction: 1 | -1) => {
      if (visibleEntries.length === 0) return null;
      if (current === null || current < 0 || current >= visibleEntries.length) {
        return findFirstInteractiveIndex(visibleEntries);
      }

      for (
        let index = current + direction;
        index >= 0 && index < visibleEntries.length;
        index += direction
      ) {
        if (isInteractive(visibleEntries[index])) return index;
      }

      return isInteractive(visibleEntries[current])
        ? current
        : findFirstInteractiveIndex(visibleEntries);
    },
    [findFirstInteractiveIndex, isInteractive, visibleEntries],
  );

  const activeCandidate =
    loading || error || activeIndex === null
      ? null
      : (visibleEntries[activeIndex] ?? null);
  const activeEntry =
    activeCandidate && isInteractive(activeCandidate) ? activeCandidate : null;

  const listboxId = `dirpicker-list-${optionIdPrefix}`;
  const optionIdFor = useCallback(
    (index: number) => `dirpicker-opt-${optionIdPrefix}-${index}`,
    [optionIdPrefix],
  );
  const activeOptionId =
    activeEntry && activeIndex !== null ? optionIdFor(activeIndex) : undefined;

  useEffect(() => {
    pathTextRef.current = pathText;
  }, [pathText]);

  const fetchListing = useCallback(
    (
      path: string | null,
      options: {
        guardParent?: string | null;
        syncPathText?: 'empty' | 'canonical-parent';
        focusAfterLoad?: boolean;
      } = {},
    ) => {
      const requestSeq = ++requestSeqRef.current;
      const requestPath = path ? normalizeDirectoryPath(path) : null;
      if (options.focusAfterLoad) refocusAfterLoadRef.current = true;

      setLoading(true);
      setError(null);
      api
        .listDir(requestPath ?? undefined)
        .then((res) => {
          if (requestSeq !== requestSeqRef.current) return;

          if (options.guardParent !== undefined) {
            const currentParent = splitPathText(pathTextRef.current).parentPath;
            if (currentParent !== options.guardParent) return;
          } else if (path === null && pathTextRef.current !== '') {
            return;
          }

          const currentParts = splitPathText(pathTextRef.current);
          setBrowsePath(res.path);
          setParent(res.parent ?? null);
          setEntries(res.entries);
          setError(null);

          if (options.syncPathText === 'empty' && pathTextRef.current === '') {
            updatePathText(seedPath(res.path));
          } else if (options.syncPathText === 'canonical-parent') {
            const suffix =
              currentParts.parentPath === requestPath ? currentParts.basename : '';
            updatePathText(`${seedPath(res.path)}${suffix}`);
          }
        })
        .catch((err: unknown) => {
          if (requestSeq !== requestSeqRef.current) return;
          if (options.guardParent !== undefined) {
            const currentParent = splitPathText(pathTextRef.current).parentPath;
            if (currentParent !== options.guardParent) return;
          }

          const msg =
            err instanceof CalmApiError
              ? `${err.code === 'forbidden' ? 'Permission denied' : err.message}`
              : err instanceof Error
                ? err.message
                : 'Failed to list directory';
          setError(msg);
          setEntries([]);
        })
        .finally(() => {
          if (requestSeq === requestSeqRef.current) setLoading(false);
        });
    },
    [updatePathText],
  );

  useEffect(() => {
    fetchListing(browsePath, {
      guardParent: browsePath,
      syncPathText: initialPath ? 'canonical-parent' : 'empty',
    });
    // Initial listing only. Subsequent parent changes are driven by `pathText`
    // edits or explicit navigation clicks.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    let secondRaf = 0;
    // Dialog runs its own first-rAF focus pass; our second rAF wins focus back to the path input.
    const firstRaf = requestAnimationFrame(() => {
      secondRaf = requestAnimationFrame(() => {
        const input = pathInputRef.current;
        input?.focus();
        if (input) input.setSelectionRange(input.value.length, input.value.length);
      });
    });
    return () => {
      cancelAnimationFrame(firstRaf);
      if (secondRaf) cancelAnimationFrame(secondRaf);
    };
  }, []);

  useEffect(() => {
    if (pathText === '' && browsePath === null) return;
    if (!pathParts.parentPath) {
      requestSeqRef.current += 1;
      setLoading(false);
      setError('Enter an absolute path');
      return;
    }

    if (pathParts.parentPath === browsePath) {
      setError(null);
      return;
    }

    const targetParent = pathParts.parentPath;
    setLoading(true);
    setError(null);
    const timeout = window.setTimeout(() => {
      fetchListing(targetParent, {
        guardParent: targetParent,
        syncPathText: 'canonical-parent',
      });
    }, 120);

    return () => window.clearTimeout(timeout);
  }, [browsePath, fetchListing, pathParts.parentPath, pathText]);

  useEffect(() => {
    if (loading) return;
    setActiveIndex(findFirstInteractiveIndex(visibleEntries));
  }, [findFirstInteractiveIndex, loading, visibleEntries]);

  useEffect(() => {
    if (activeIndex === null) return;
    const activeOption = document.getElementById(optionIdFor(activeIndex));
    if (typeof activeOption?.scrollIntoView !== 'function') return;
    activeOption.scrollIntoView({ block: 'nearest' });
  }, [activeIndex, optionIdFor]);

  useEffect(() => {
    if (loading || !refocusAfterLoadRef.current) return;
    refocusAfterLoadRef.current = false;
    const raf = requestAnimationFrame(() => {
      const input = pathInputRef.current;
      input?.focus();
      if (input) input.setSelectionRange(input.value.length, input.value.length);
    });
    return () => cancelAnimationFrame(raf);
  }, [browsePath, loading]);

  const goTo = (path: string) => {
    const normalizedPath = normalizeDirectoryPath(path);
    updatePathText(seedPath(normalizedPath));
    setBrowsePath(normalizedPath);
    fetchListing(normalizedPath, {
      guardParent: normalizedPath,
      syncPathText: 'canonical-parent',
      focusAfterLoad: true,
    });
  };

  const activateEntry = (entry: ListdirResponse['entries'][number]) => {
    const child = joinPath(browsePath ?? '', entry.name);
    if (entry.is_dir) {
      goTo(child);
    } else if (mode === 'file') {
      onSelect(child);
    }
  };

  const selectCurrentDirectory = () => {
    if (browsePath) onSelect(browsePath);
  };

  const handlePathKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      setActiveIndex((current) => findNextInteractiveIndex(current, 1));
      return;
    }

    if (event.key === 'ArrowUp') {
      event.preventDefault();
      setActiveIndex((current) => findNextInteractiveIndex(current, -1));
      return;
    }

    if (event.key === 'Enter') {
      event.preventDefault();
      if (activeEntry) {
        activateEntry(activeEntry);
      } else if (browsePath && pathText === seedPath(browsePath)) {
        onSelect(browsePath);
      }
      return;
    }

    if (event.key === '/' && activeEntry?.is_dir) {
      event.preventDefault();
      activateEntry(activeEntry);
      return;
    }

    if (event.key === 'Escape') {
      event.preventDefault();
      onCancel();
      return;
    }
  };

  return (
    // No `role="dialog"` here: every in-app caller renders this browser
    // inside an outer `<Dialog>` — either pushed via `useModalView()` (which
    // swaps the dialog's title to "Choose a directory") or as the direct
    // child of a `<Dialog title=...>` (the codex "create here" shortcut in
    // Wave.tsx). Nested ARIA dialogs are not allowed; the outer Dialog
    // already provides the accessible name and the modal semantics. The
    // fallback inline path in DirectoryPicker also lands here, but it is
    // not modal, so a dialog role would be a lie in that case too.
    <div className="dirpicker-browser">
      <div className="dirpicker-browser-head">
        <button
          type="button"
          className="dirpicker-up"
          onClick={() => parent && goTo(parent)}
          disabled={!parent || loading}
          title="Parent directory"
          aria-label="Parent directory"
        >
          ↑
        </button>
        <div className="dirpicker-path">
          <input
            ref={pathInputRef}
            type="text"
            className="dirpicker-path-input"
            value={pathText}
            onChange={(event) => updatePathText(event.currentTarget.value)}
            onKeyDown={handlePathKeyDown}
            placeholder="Absolute path"
            aria-label="Directory path"
            aria-controls={listboxId}
            aria-activedescendant={activeOptionId}
            autoComplete="off"
            spellCheck={false}
          />
        </div>
      </div>
      <ul
        id={listboxId}
        className="dirpicker-list"
        role="listbox"
        aria-label="Directory entries"
      >
        {loading ? (
          <li className="dirpicker-status">Loading…</li>
        ) : error ? (
          <li className="dirpicker-error">{error}</li>
        ) : entries.length === 0 ? (
          <li className="dirpicker-status">Empty directory</li>
        ) : visibleEntries.length === 0 ? (
          <li className="dirpicker-status">No matches</li>
        ) : (
          visibleEntries.map((ent, index) => {
            const interactive = isInteractive(ent);
            const active = interactive && index === activeIndex;
            const className = `dirpicker-entry${ent.is_dir ? '' : ' dirpicker-entry-file'}${
              active ? ' dirpicker-entry-active' : ''
            }`;
            return (
              <li key={ent.name} role="none">
                <button
                  id={optionIdFor(index)}
                  type="button"
                  role="option"
                  aria-selected={active}
                  className={className}
                  disabled={!interactive}
                  onClick={() => {
                    activateEntry(ent);
                  }}
                  title={ent.name}
                >
                  <span className="dirpicker-entry-icon" aria-hidden="true">
                    {ent.is_dir ? '📁' : '📄'}
                  </span>
                  <span className="dirpicker-entry-name">{ent.name}</span>
                </button>
              </li>
            );
          })
        )}
      </ul>
      <div className="dirpicker-actions">
        <button type="button" className="dirpicker-cancel" onClick={onCancel}>
          Cancel
        </button>
        <button
          type="button"
          className="dirpicker-select"
          onClick={selectCurrentDirectory}
          disabled={!browsePath || loading}
        >
          {selectLabel}
        </button>
      </div>
    </div>
  );
}

/**
 * Best-effort path join for the *click-through* case. The server
 * canonicalizes the path on the next `listDir` anyway, so a small
 * roughness here (trailing slash, double slash) gets normalized on the
 * wire. We just want a reasonable string for the request.
 */
function joinPath(base: string, name: string): string {
  if (!base) return name;
  if (base.endsWith('/')) return base + name;
  return base + '/' + name;
}

function seedPath(path: string): string {
  const normalized = normalizeDirectoryPath(path);
  return normalized === '/' ? '/' : `${normalized}/`;
}

function normalizeDirectoryPath(path: string): string {
  if (path === '/') return '/';
  const trimmed = path.replace(/\/+$/, '');
  return trimmed || '/';
}

function splitPathText(path: string): { parentPath: string | null; basename: string } {
  const slashIndex = path.lastIndexOf('/');
  if (slashIndex === -1) return { parentPath: null, basename: path };

  const parentText = path.slice(0, slashIndex + 1);
  if (!parentText.startsWith('/')) {
    return { parentPath: null, basename: path.slice(slashIndex + 1) };
  }

  return {
    parentPath: normalizeDirectoryPath(parentText),
    basename: path.slice(slashIndex + 1),
  };
}
