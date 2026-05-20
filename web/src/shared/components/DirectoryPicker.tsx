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

import { useEffect } from 'react';
import { useState } from '../state';
import * as api from '../../api/calm';
import { CalmApiError } from '../../api/calm';
import type { ListdirResponse } from '../../api/wire';
import { useModalView } from './Modal';

export interface DirectoryPickerProps {
  value: string;
  onChange: (path: string) => void;
  id?: string;
  placeholder?: string;
}

export function DirectoryPicker({
  value,
  onChange,
  id,
  placeholder = 'Choose a directory…',
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
      title: 'Choose a directory',
      onEscape: cancelBrowse,
      body: (
        <DirectoryBrowser
          initialPath={value || null}
          onCancel={cancelBrowse}
          onSelect={commitBrowse}
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
export function DirectoryBrowser({ initialPath, onCancel, onSelect, selectLabel = 'Select this directory' }: DirectoryBrowserProps) {
  // `browsePath === null` on first render → the request hits the
  // server with no path argument and the server canonicalizes to $HOME.
  // We seed it with `initialPath` (the field's current value, if any)
  // so reopening the browser lands where the user left off.
  const [browsePath, setBrowsePath] = useState<string | null>(initialPath);
  const [parent, setParent] = useState<string | null>(null);
  const [entries, setEntries] = useState<ListdirResponse['entries']>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

  const goTo = (path: string) => setBrowsePath(path);

  return (
    <div className="dirpicker-browser" role="dialog" aria-label="Choose a directory">
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
      <ul className="dirpicker-list" role="listbox">
        {loading ? (
          <li className="dirpicker-status">Loading…</li>
        ) : error ? (
          <li className="dirpicker-error">{error}</li>
        ) : entries.length === 0 ? (
          <li className="dirpicker-status">Empty directory</li>
        ) : (
          entries.map((ent) => {
            const child = joinPath(browsePath ?? '', ent.name);
            return (
              <li key={ent.name} role="none">
                <button
                  type="button"
                  role="option"
                  aria-selected={false}
                  className={`dirpicker-entry${ent.is_dir ? '' : ' dirpicker-entry-file'}`}
                  disabled={!ent.is_dir}
                  onClick={() => ent.is_dir && goTo(child)}
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
          onClick={() => browsePath && onSelect(browsePath)}
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
