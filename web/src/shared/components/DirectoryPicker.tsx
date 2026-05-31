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

import { useEffect, useId, useMemo, useRef } from 'react';
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
  const [browsePath, setBrowsePath] = useState<string | null>(initialPath);
  const [parent, setParent] = useState<string | null>(null);
  const [entries, setEntries] = useState<ListdirResponse['entries']>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filterText, setFilterText] = useState('');
  const [activeIndex, setActiveIndex] = useState<number | null>(null);
  const filterInputRef = useRef<HTMLInputElement | null>(null);
  const refocusAfterLoadRef = useRef(false);
  const optionIdPrefix = useId();

  const visibleEntries = useMemo(() => {
    if (!filterText) return entries;
    const needle = filterText.toLowerCase();
    return entries.filter((entry) => entry.name.toLowerCase().startsWith(needle));
  }, [entries, filterText]);

  const activeEntry =
    loading || error || activeIndex === null
      ? null
      : (visibleEntries[activeIndex] ?? null);

  const listboxId = `dirpicker-list-${optionIdPrefix}`;
  const optionIdFor = (index: number) => `dirpicker-opt-${optionIdPrefix}-${index}`;
  const activeOptionId =
    activeEntry && activeIndex !== null ? optionIdFor(activeIndex) : undefined;

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .listDir(browsePath ?? undefined)
      .then((res) => {
        if (cancelled) return;
        setBrowsePath(res.path);
        setParent(res.parent ?? null);
        setEntries(res.entries);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
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
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [browsePath]);

  useEffect(() => {
    let secondRaf = 0;
    const firstRaf = requestAnimationFrame(() => {
      secondRaf = requestAnimationFrame(() => {
        filterInputRef.current?.focus();
      });
    });
    return () => {
      cancelAnimationFrame(firstRaf);
      if (secondRaf) cancelAnimationFrame(secondRaf);
    };
  }, []);

  useEffect(() => {
    setFilterText('');
  }, [browsePath]);

  useEffect(() => {
    if (loading) return;
    setActiveIndex(visibleEntries.length === 0 ? null : 0);
  }, [loading, visibleEntries]);

  useEffect(() => {
    if (loading || !refocusAfterLoadRef.current) return;
    refocusAfterLoadRef.current = false;
    const raf = requestAnimationFrame(() => {
      filterInputRef.current?.focus();
    });
    return () => cancelAnimationFrame(raf);
  }, [browsePath, loading]);

  const goTo = (path: string) => {
    refocusAfterLoadRef.current = true;
    setBrowsePath(path);
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

  const handleFilterKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    const input = event.currentTarget;

    if (event.key === 'ArrowDown') {
      event.preventDefault();
      setActiveIndex((current) => {
        if (visibleEntries.length === 0) return null;
        if (current === null) return 0;
        return Math.min(current + 1, visibleEntries.length - 1);
      });
      return;
    }

    if (event.key === 'ArrowUp') {
      event.preventDefault();
      setActiveIndex((current) => {
        if (visibleEntries.length === 0) return null;
        if (current === null) return 0;
        return Math.max(current - 1, 0);
      });
      return;
    }

    if (event.key === 'Enter') {
      event.preventDefault();
      if (activeEntry) {
        activateEntry(activeEntry);
      } else if (!filterText && browsePath) {
        onSelect(browsePath);
      }
      return;
    }

    if (event.key === 'ArrowRight') {
      const caretAtEnd =
        input.selectionStart === input.value.length &&
        input.selectionEnd === input.value.length;
      if (caretAtEnd && activeEntry?.is_dir) {
        event.preventDefault();
        activateEntry(activeEntry);
      }
      return;
    }

    if (event.key === '/' && activeEntry?.is_dir) {
      event.preventDefault();
      activateEntry(activeEntry);
      return;
    }

    if (event.key === 'ArrowLeft') {
      const caretAtStart = input.selectionStart === 0 && input.selectionEnd === 0;
      if (caretAtStart && parent) {
        event.preventDefault();
        goTo(parent);
      }
      return;
    }

    if (event.key === 'Escape') {
      event.preventDefault();
      onCancel();
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
        <span className="dirpicker-cwd" title={browsePath ?? ''}>
          {browsePath ?? '…'}
        </span>
      </div>
      <div className="dirpicker-filter">
        <input
          ref={filterInputRef}
          type="text"
          className="dirpicker-filter-input"
          value={filterText}
          onChange={(event) => setFilterText(event.currentTarget.value)}
          onKeyDown={handleFilterKeyDown}
          placeholder="Filter"
          aria-label="Filter directory entries"
          aria-controls={listboxId}
          aria-activedescendant={activeOptionId}
          autoComplete="off"
          spellCheck={false}
        />
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
            const active = index === activeIndex;
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
                  disabled={!ent.is_dir && mode === 'directory'}
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
