// Reusable modal — Portal-mounted overlay + centered card. Styled to
// match the rest of the app (rounded card, app shadow, blurred overlay);
// deliberately not <dialog> because the browser defaults are unstyleable
// across themes.
//
// Modal also exposes a tiny **child-view stack** through React context.
// A descendant (today: DirectoryPicker's browse view) can call
// `pushView(node)` to take over the modal body — the title bar swaps to
// the view's own title and a back button, the original children stay
// mounted but visually hidden so their state is preserved across the
// detour. This keeps us to a *single* modal layer even when an inner
// widget needs a fullscreen-feeling sub-flow; popover-in-modal nesting
// was the structural mistake we're replacing here.

import { createContext, useContext, useEffect, useMemo } from 'react';
import { useState } from '../state';
import { createPortal } from 'react-dom';

export interface ModalProps {
  open: boolean;
  onClose: () => void;
  title?: string;
  children?: React.ReactNode;
  /** Render the modal with the wider/taller panel variant (same sizing
   *  pushed child views get). Used when the modal's *direct* content is
   *  already a fullscreen-ish widget (e.g. a directory browser). */
  wide?: boolean;
}

/**
 * A view pushed onto the modal's child-view stack. Renders in place of
 * the modal's normal `children`, claiming the full body area + replacing
 * the header title. The view is responsible for calling `pop()` when
 * done (Cancel / Select / Esc inside the view).
 */
export interface ModalChildView {
  /** Rendered in place of `modal-title`. */
  title: React.ReactNode;
  /** Rendered in place of `modal-body`'s normal children. */
  body: React.ReactNode;
  /** Optional Esc handler. If omitted, Esc pops the view. */
  onEscape?: () => void;
}

interface ModalViewCtx {
  pushView: (view: ModalChildView) => void;
  popView: () => void;
}

const ModalViewContext = createContext<ModalViewCtx | null>(null);

/**
 * Hook for descendants of `<Modal>` to take over the modal body with a
 * sub-view. Returns `null` outside a Modal — callers should fall back to
 * an inline rendering path.
 */
export function useModalView(): ModalViewCtx | null {
  return useContext(ModalViewContext);
}

export function Modal({ open, onClose, title, children, wide }: ModalProps) {
  const [view, setView] = useState<ModalChildView | null>(null);

  // Clear any pushed view whenever the modal closes — opening again
  // should start from the normal children, not a stale browse view.
  useEffect(() => {
    if (!open) setView(null);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    // Esc: if a child view is up, give it first refusal; otherwise close
    // the modal. Matches AddPanel + browser-wide modal expectations.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      if (view) {
        if (view.onEscape) view.onEscape();
        else setView(null);
        return;
      }
      onClose();
    };
    document.addEventListener('keydown', onKey);
    // Lock scroll on the page underneath while modal is up.
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => {
      document.removeEventListener('keydown', onKey);
      document.body.style.overflow = prevOverflow;
    };
  }, [open, onClose, view]);

  const ctx = useMemo<ModalViewCtx>(
    () => ({
      pushView: (v) => setView(v),
      popView: () => setView(null),
    }),
    [],
  );

  if (!open) return null;

  const showingView = view !== null;
  const headerTitle = showingView ? view.title : title;
  // Wide if a child view is up OR caller explicitly asked for it (direct
  // children that benefit from the larger panel, e.g. a DirectoryBrowser
  // rendered as the modal's main content).
  const widePanel = showingView || !!wide;

  return createPortal(
    <ModalViewContext.Provider value={ctx}>
      <div
        className={`modal-overlay${widePanel ? ' modal-overlay-wide' : ''}`}
        onMouseDown={(e) => {
          // Click on the overlay closes; clicks inside the panel are
          // stopped via onMouseDown there. Disabled while a child view
          // is up — Cancel inside the view is the only way out, prevents
          // accidental loss of a half-filled SchemaForm behind it.
          if (showingView) return;
          if (e.target === e.currentTarget) onClose();
        }}
        role="presentation"
      >
        {/* TODO(a11y-#56-slice-2): replace the onMouseDown stopPropagation
            dance with a proper focus-trap / Escape-to-close dialog. The
            Modal is being rebuilt in Slice 2; the lint rule is suppressed
            here so Slice 1 ships without dragging Modal work in. */}
        {/* eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions -- deferred to slice 2 (see TODO above) */}
        <div
          className={`modal-panel${widePanel ? ' modal-panel-wide' : ''}`}
          role="dialog"
          aria-modal="true"
          aria-label={typeof headerTitle === 'string' ? headerTitle : undefined}
          onMouseDown={(e) => e.stopPropagation()}
        >
          {headerTitle && (
            <div className="modal-head">
              <span className="modal-title">{headerTitle}</span>
              <button
                type="button"
                className="modal-close"
                aria-label="Close"
                onClick={onClose}
              >
                ×
              </button>
            </div>
          )}
          {/* Both panes are always mounted while the modal is open:
              when a child view is showing we hide the normal children
              (display:none) but keep their state intact — closing the
              view returns the user to their half-filled form. */}
          <div
            className="modal-body"
            style={showingView ? { display: 'none' } : undefined}
          >
            {children}
          </div>
          {showingView && <div className="modal-body modal-body-view">{view!.body}</div>}
        </div>
      </div>
    </ModalViewContext.Provider>,
    document.body,
  );
}
