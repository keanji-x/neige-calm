import { createContext, useCallback, useContext, useEffect, useMemo, useRef, type KeyboardEventHandler, type ReactNode, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import { useState } from '../state/public.ts';

export interface DialogChildView { title: ReactNode; body: ReactNode; onEscape?: () => void }
export interface DialogViewController { pushView: (view: DialogChildView) => () => void; popView: () => void }
export interface DialogProps {
  open: boolean; onClose: () => void; title?: string; hideTitleRow?: boolean; children?: ReactNode; wide?: boolean;
  initialFocusRef?: RefObject<HTMLElement | null>; restoreFocusRef?: RefObject<HTMLElement | null>;
}
export interface ConfirmDialogProps {
  open: boolean; title: string; description?: ReactNode; confirmLabel?: string; cancelLabel?: string;
  onConfirm: () => void; onCancel: () => void; destructive?: boolean; confirmDisabled?: boolean;
}

const DialogViewContext = createContext<DialogViewController | null>(null);
export function useDialogView(): DialogViewController | null { return useContext(DialogViewContext); }

const focusableSelector = 'a[href],area[href],button:not([disabled]),input:not([disabled]):not([type="hidden"]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"]),[contenteditable="true"]';
function focusables(panel: HTMLElement): HTMLElement[] {
  return Array.from(panel.querySelectorAll<HTMLElement>(focusableSelector)).filter((element) =>
    !element.hasAttribute('disabled') && !element.closest('[inert]'));
}

export function Dialog({ open, onClose, title, hideTitleRow, children, wide, initialFocusRef, restoreFocusRef }: DialogProps) {
  const [views, setViews] = useState<readonly (DialogChildView & { id: number })[]>([]);
  const nextViewId = useRef(0);
  const panelRef = useRef<HTMLDivElement | null>(null);
  const previouslyFocusedRef = useRef<HTMLElement | null>(null);
  const popView = useCallback(() => setViews((current) => current.slice(0, -1)), [setViews]);
  const pushView = useCallback((view: DialogChildView) => {
    const id = ++nextViewId.current;
    setViews((current) => [...current, { ...view, id }]);
    return () => setViews((current) => current.filter((candidate) => candidate.id !== id));
  }, [setViews]);
  const view = views.at(-1) ?? null;
  useEffect(() => { if (!open) setViews([]); }, [open, setViews]);
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      if (view) { if (view.onEscape) view.onEscape(); else popView(); }
      else onClose();
    };
    const overflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    document.addEventListener('keydown', onKeyDown);
    return () => { document.removeEventListener('keydown', onKeyDown); document.body.style.overflow = overflow; };
  }, [onClose, open, popView, view]);

  // Load-bearing declaration order: remove inert before the following focus-restore cleanup runs.
  useEffect(() => {
    if (!open) return;
    let portalRoot: HTMLElement | null = panelRef.current;
    while (portalRoot && portalRoot.parentElement !== document.body) portalRoot = portalRoot.parentElement;
    const prior = Array.from(document.body.children).filter((element): element is HTMLElement =>
      element instanceof HTMLElement && element !== portalRoot).map((element) => ({
        element, inert: element.hasAttribute('inert'), ariaHidden: element.getAttribute('aria-hidden'),
      }));
    for (const { element } of prior) { element.setAttribute('inert', ''); element.setAttribute('aria-hidden', 'true'); }
    return () => { for (const state of prior) {
      if (!state.inert) state.element.removeAttribute('inert');
      if (state.ariaHidden === null) state.element.removeAttribute('aria-hidden');
      else state.element.setAttribute('aria-hidden', state.ariaHidden);
    } };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    previouslyFocusedRef.current = document.activeElement as HTMLElement | null;
    const frame = requestAnimationFrame(() => {
      const panel = panelRef.current;
      if (!panel) return;
      (initialFocusRef?.current ?? focusables(panel)[0] ?? panel).focus();
    });
    return () => {
      cancelAnimationFrame(frame);
      // eslint-disable-next-line react-hooks/exhaustive-deps -- cleanup must consume the caller's latest override, not the mount-time node.
      const target = restoreFocusRef?.current ?? previouslyFocusedRef.current;
      if (target && document.contains(target)) target.focus();
    };
  }, [initialFocusRef, open, restoreFocusRef]);

  const controller = useMemo<DialogViewController>(() => ({ pushView, popView }), [popView, pushView]);
  if (!open) return null;
  const showingView = view !== null;
  const headerTitle = showingView ? view.title : title;
  const onPanelKeyDown: KeyboardEventHandler<HTMLDivElement> = (event) => {
    if (event.key !== 'Tab' || !panelRef.current) return;
    const panel = panelRef.current;
    const items = focusables(panel);
    if (items.length === 0) { event.preventDefault(); panel.focus(); return; }
    const first = items[0]; const last = items[items.length - 1]; const active = document.activeElement;
    if (event.shiftKey ? active === first || !panel.contains(active) : active === last || !panel.contains(active)) {
      event.preventDefault(); (event.shiftKey ? last : first)?.focus();
    }
  };
  return createPortal(<DialogViewContext.Provider value={controller}><div
    className={showingView || wide ? 'dialog-overlay dialog-overlay-wide' : 'dialog-overlay'} role="presentation"
    onMouseDown={(event) => { if (!showingView && event.target === event.currentTarget) onClose(); }}>
    {/* eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions -- the dialog panel owns its required focus trap and click-through guard. */}
    <div ref={panelRef} className={showingView || wide ? 'dialog-panel dialog-panel-wide' : 'dialog-panel'}
      role="dialog" aria-modal="true" aria-label={typeof headerTitle === 'string' ? headerTitle : undefined}
      tabIndex={-1} onMouseDown={(event) => event.stopPropagation()} onKeyDown={onPanelKeyDown}>
      {headerTitle && (showingView || !hideTitleRow) && <header className="dialog-header"><span>{headerTitle}</span><button type="button" aria-label="Close" onClick={onClose}>×</button></header>}
      <div className="dialog-body" style={showingView ? { display: 'none' } : undefined}>{children}</div>
      {showingView && <div className="dialog-body dialog-child-view">{view.body}</div>}
    </div>
  </div></DialogViewContext.Provider>, document.body);
}

export function ConfirmDialog({ open, title, description, confirmLabel = 'Confirm', cancelLabel = 'Cancel', onConfirm, onCancel, destructive = true, confirmDisabled = false }: ConfirmDialogProps) {
  const cancelRef = useRef<HTMLButtonElement | null>(null);
  return <Dialog open={open} title={title} onClose={onCancel} initialFocusRef={cancelRef}>
    {description}<div className="confirm-dialog-actions"><button ref={cancelRef} type="button" onClick={onCancel}>{cancelLabel}</button>
      <button type="button" data-variant={destructive ? 'danger' : 'primary'} disabled={confirmDisabled} onClick={onConfirm}>{confirmLabel}</button></div>
  </Dialog>;
}
