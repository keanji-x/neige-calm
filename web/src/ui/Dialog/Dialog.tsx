// Reusable dialog — Portal-mounted overlay + centered card. Styled to
// match the rest of the app (rounded card, app shadow, blurred overlay);
// deliberately not <dialog> because the browser defaults are unstyleable
// across themes.
//
// Dialog also exposes a tiny **child-view stack** through React context.
// A descendant (today: DirectoryPicker's browse view) can call
// `pushView(node)` to take over the dialog body — the title bar swaps to
// the view's own title and a back button, the original children stay
// mounted but visually hidden so their state is preserved across the
// detour. This keeps us to a *single* dialog layer even when an inner
// widget needs a fullscreen-feeling sub-flow; popover-in-modal nesting
// was the structural mistake we're replacing here.
//
// Focus contract (Slice 2 of #56)
// -------------------------------
// While the dialog is open it owns keyboard focus and screen-reader
// reachability:
//   1. **Focus trap.** Tab/Shift+Tab cycles within the panel; focus can
//      never leak into background DOM.
//   2. **Initial focus.** When `open` flips true, focus moves into the
//      panel — either to a caller-provided `initialFocusRef`, or to the
//      first focusable element, or to the panel itself if there are none.
//   3. **Focus restore.** When the dialog closes, focus returns to the
//      element that owned it just before opening (typically the trigger
//      button). Callers can override with `restoreFocusRef`.
//   4. **Background inert.** Sibling top-level DOM under `document.body`
//      gets `inert` + `aria-hidden="true"` while the dialog is open;
//      assistive tech cannot linearly traverse into the page underneath.
//
// The trap is hand-rolled rather than pulling a library — see the
// architecture note in issue #56. Querying focusables fresh on every Tab
// keypress keeps us correct across dynamic content (e.g. when a child
// view swaps in).

import { createContext, useContext, useEffect, useMemo, useRef } from 'react';
import type { RefObject } from 'react';
import { useState } from '../../shared/state';
import { createPortal } from 'react-dom';

export interface DialogProps {
  open: boolean;
  onClose: () => void;
  title?: string;
  children?: React.ReactNode;
  /** Render the dialog with the wider/taller panel variant (same sizing
   *  pushed child views get). Used when the dialog's *direct* content is
   *  already a fullscreen-ish widget (e.g. a directory browser). */
  wide?: boolean;
  /** Element to focus when the dialog opens. Defaults to the first
   *  focusable inside the panel (or the panel itself if none). */
  initialFocusRef?: RefObject<HTMLElement | null>;
  /** Element to focus when the dialog closes. Defaults to whatever was
   *  focused right before the dialog opened (usually the trigger). */
  restoreFocusRef?: RefObject<HTMLElement | null>;
}

/**
 * A view pushed onto the dialog's child-view stack. Renders in place of
 * the dialog's normal `children`, claiming the full body area + replacing
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
 * Hook for descendants of `<Dialog>` to take over the dialog body with a
 * sub-view. Returns `null` outside a Dialog — callers should fall back to
 * an inline rendering path.
 */
export function useModalView(): ModalViewCtx | null {
  return useContext(ModalViewContext);
}

// CSS selector matching everything we consider tabbable inside the
// panel. The `[tabindex]:not([tabindex="-1"])` clause is intentional:
// `tabIndex={-1}` means "programmatically focusable, but skipped by Tab"
// — we want it focusable as a fallback target but not part of the trap
// cycle. We further filter the matches with `isFocusable` below to drop
// disabled / hidden elements that querySelector cannot exclude.
const FOCUSABLE_SELECTOR = [
  'a[href]',
  'area[href]',
  'button:not([disabled])',
  'input:not([disabled]):not([type="hidden"])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
  '[contenteditable="true"]',
].join(',');

function isFocusable(el: Element): el is HTMLElement {
  if (!(el instanceof HTMLElement)) return false;
  if (el.hasAttribute('disabled')) return false;
  // `inert` (on the element or any ancestor) makes it untabbable.
  if (el.closest('[inert]')) return false;
  // We deliberately don't filter by visibility here. In a real browser
  // hidden elements (`display:none`, `visibility:hidden`) are also
  // unmatchable by `:focus-visible`, so cycling onto one is rare; in
  // jsdom there's no layout at all, so visibility checks would
  // false-negative every focusable in our tests. The selector itself
  // (which excludes `disabled` and `tabindex="-1"`) is the primary
  // filter; this hook only drops the obviously-bad cases.
  return true;
}

function focusableElementsIn(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    isFocusable,
  );
}

export function Dialog({
  open,
  onClose,
  title,
  children,
  wide,
  initialFocusRef,
  restoreFocusRef,
}: DialogProps) {
  const [view, setView] = useState<ModalChildView | null>(null);
  const panelRef = useRef<HTMLDivElement | null>(null);
  // Captured at open time so we can restore focus on close.
  const previouslyFocusedRef = useRef<HTMLElement | null>(null);

  // Clear any pushed view whenever the dialog closes — opening again
  // should start from the normal children, not a stale browse view.
  useEffect(() => {
    if (!open) setView(null);
  }, [open]);

  // Esc + body scroll lock.
  useEffect(() => {
    if (!open) return;
    // Esc: if a child view is up, give it first refusal; otherwise close
    // the dialog. Matches AddPanel + browser-wide modal expectations.
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
    // Lock scroll on the page underneath while dialog is up.
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => {
      document.removeEventListener('keydown', onKey);
      document.body.style.overflow = prevOverflow;
    };
  }, [open, onClose, view]);

  // Background inert. We mark every direct child of `document.body`
  // *except* the portal root as `inert` + `aria-hidden="true"` so
  // assistive tech and Tab cannot reach the page underneath. Declared
  // *before* the focus-restore effect so React's effect-cleanup ordering
  // (declaration order, not reverse) runs the inert removal first; the
  // focus restore that follows can then land on an element whose
  // ancestors are no longer inert. (Slice 7 surfaced this — when the
  // previously-focused element is a sibling of the dialog portal, like
  // the AddPanel trigger, focusing it while the inert blanket is still
  // applied silently fails.)
  useEffect(() => {
    if (!open) return;
    const panel = panelRef.current;
    // The portal mounts into document.body; the modal-overlay div is the
    // direct child to skip. We walk up from the panel until we hit a
    // direct child of body, which gives us the overlay element even if
    // some future refactor adds an extra wrapper.
    let portalRoot: HTMLElement | null = panel;
    while (portalRoot && portalRoot.parentElement !== document.body) {
      portalRoot = portalRoot.parentElement;
    }
    const siblings = Array.from(document.body.children).filter(
      (el): el is HTMLElement =>
        el instanceof HTMLElement && el !== portalRoot,
    );
    // Remember each sibling's prior state so we can restore exactly.
    const prior = siblings.map((el) => ({
      el,
      hadInert: el.hasAttribute('inert'),
      hadAriaHidden: el.getAttribute('aria-hidden'),
    }));
    for (const el of siblings) {
      el.setAttribute('inert', '');
      el.setAttribute('aria-hidden', 'true');
    }
    return () => {
      for (const { el, hadInert, hadAriaHidden } of prior) {
        if (!hadInert) el.removeAttribute('inert');
        if (hadAriaHidden === null) el.removeAttribute('aria-hidden');
        else el.setAttribute('aria-hidden', hadAriaHidden);
      }
    };
  }, [open]);

  // Initial focus + focus restore. We split this from the Esc effect so
  // the dependencies stay minimal — re-running this on every `view`
  // change would yank focus around when the user pushes a sub-view.
  //
  // Declared AFTER the inert effect (above) on purpose: React runs
  // effect cleanups in declaration order on unmount, so the inert
  // blanket is removed first and the focus restore below can succeed
  // even when the restore target is a sibling of the portal root.
  useEffect(() => {
    if (!open) return;
    // Capture the element that owned focus before we opened. Used in the
    // cleanup below to restore it.
    previouslyFocusedRef.current =
      (document.activeElement as HTMLElement | null) ?? null;

    // Defer the focus call by one frame so the panel is in the DOM and
    // layout has settled. Without this, `panelRef.current` may be set
    // but the focus call lands before the browser is ready to honor it.
    const raf = requestAnimationFrame(() => {
      const panel = panelRef.current;
      if (!panel) return;
      const explicit = initialFocusRef?.current;
      if (explicit) {
        explicit.focus();
        return;
      }
      const first = focusableElementsIn(panel)[0];
      if (first) {
        first.focus();
      } else {
        // No focusable children — focus the panel itself so the trap
        // has somewhere to anchor.
        panel.focus();
      }
    });
    return () => {
      cancelAnimationFrame(raf);
      // Restore on close. Caller override wins; otherwise return focus
      // to wherever it lived before we opened. Both refs are read at
      // cleanup time intentionally — we want the *latest* override the
      // caller has supplied (and `previouslyFocusedRef` is a closure
      // ref written above in the same effect), not a snapshot from
      // mount. The react-hooks rule warns about `.current` in cleanups
      // because the ref's target may have changed; here that's exactly
      // what we rely on.
      // eslint-disable-next-line react-hooks/exhaustive-deps
      const target = restoreFocusRef?.current ?? previouslyFocusedRef.current;
      // The previously-focused element may have been removed from the
      // DOM during the dialog's lifetime (e.g. a re-render). Guard
      // against focusing a detached node — silently noop instead.
      if (target && document.contains(target)) {
        target.focus();
      }
    };
  }, [open, initialFocusRef, restoreFocusRef]);

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
  // rendered as the dialog's main content).
  const widePanel = showingView || !!wide;

  // Focus trap: intercept Tab / Shift+Tab on the panel and wrap focus
  // around the focusables list. We re-query on every keydown — the panel
  // contents can change (e.g. when a child view pushes new body content)
  // and a cached snapshot would go stale.
  const onPanelKeyDown: React.KeyboardEventHandler<HTMLDivElement> = (e) => {
    if (e.key !== 'Tab') return;
    const panel = panelRef.current;
    if (!panel) return;
    const focusables = focusableElementsIn(panel);
    if (focusables.length === 0) {
      // Nothing to cycle through — pin focus on the panel itself.
      e.preventDefault();
      panel.focus();
      return;
    }
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (e.shiftKey) {
      if (active === first || !panel.contains(active)) {
        e.preventDefault();
        last.focus();
      }
    } else {
      if (active === last || !panel.contains(active)) {
        e.preventDefault();
        first.focus();
      }
    }
  };

  // NOTE on the child-view sub-mode: when a descendant calls
  // `pushView(...)`, the panel re-renders with new body content but the
  // outer trap stays mounted. Because `focusableElementsIn` runs on each
  // Tab keypress, the trap automatically picks up the new focusables —
  // the child view doesn't need its own focus management on top of this.
  // Its `onEscape` still gets first refusal for Esc (see effect above).

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
        {/* `jsx-a11y/no-noninteractive-element-interactions` flags this
            because `role="dialog"` is a structural (non-interactive) ARIA
            role and the panel has onMouseDown / onKeyDown listeners. Both
            handlers are legitimate parts of the dialog contract that the
            lint rule cannot model:
              - onMouseDown stops click-through so clicks inside the panel
                don't bubble to the overlay's close-on-click handler.
              - onKeyDown implements the WAI-ARIA-mandated focus trap for
                aria-modal dialogs.
            Neither is a "fake button" — the disable is structural, not
            deferred work. */}
        {/* eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions -- dialog focus trap + click-outside; see comment above */}
        <div
          ref={panelRef}
          className={`modal-panel${widePanel ? ' modal-panel-wide' : ''}`}
          role="dialog"
          aria-modal="true"
          aria-label={typeof headerTitle === 'string' ? headerTitle : undefined}
          tabIndex={-1}
          onMouseDown={(e) => e.stopPropagation()}
          onKeyDown={onPanelKeyDown}
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
          {/* Both panes are always mounted while the dialog is open:
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
