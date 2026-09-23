import { createContext, useCallback, useContext, useEffect, useId, useMemo, useRef, type KeyboardEventHandler, type ReactNode, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import { Icon } from '../icon/public.tsx';
import { useState } from '../state/public.ts';

export interface DialogChildView { title: ReactNode; body: ReactNode; onEscape?: () => void }
export interface DialogViewController { pushView: (view: DialogChildView) => () => void; popView: () => void }
export interface DialogProps {
  open: boolean; onClose: () => void; title?: string; hideTitleRow?: boolean; children?: ReactNode; wide?: boolean;
  /** Drop the `×` while keeping the title; `hideTitleRow` can only remove both. */
  hideClose?: boolean;
  initialFocusRef?: RefObject<HTMLElement | null>;
}
/** `blocked` is real `disabled`; `busy` keeps the button focusable so the focus trap does not lose a member mid-flight. */
export type ConfirmState = 'ready' | 'blocked' | 'busy';
export interface ConfirmDialogProps {
  open: boolean; title: string; description?: ReactNode; confirmLabel?: string; cancelLabel?: string;
  onConfirm: () => void; onCancel: () => void; destructive?: boolean; confirmState?: ConfirmState;
  /** Second label node, so busy can swap text without changing width. */
  confirmBusyLabel?: string;
  /** Override the initial focus target; defaults to Cancel. */
  initialFocusRef?: RefObject<HTMLElement | null>;
}

const DialogViewContext = createContext<DialogViewController | null>(null);
export function useDialogView(): DialogViewController | null { return useContext(DialogViewContext); }

const focusableSelector = 'a[href],area[href],button:not([disabled]),input:not([disabled]):not([type="hidden"]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"]),[contenteditable="true"],summary:first-of-type:not([tabindex])';
function focusables(panel: HTMLElement): HTMLElement[] {
  return Array.from(panel.querySelectorAll<HTMLElement>(focusableSelector)).filter((element) => {
    // Only the first direct summary gains a native tab stop without tabindex.
    if (element.matches('summary:not([tabindex]):not([contenteditable="true"])')
      && !element.parentElement?.matches('details')) return false;
    return !element.hasAttribute('disabled') && !element.closest('[inert]') && isVisibleWithin(element, panel);
  });
}
function isVisibleWithin(element: HTMLElement, panel: HTMLElement): boolean {
  for (let current: HTMLElement | null = element; current; current = current.parentElement) {
    const style = getComputedStyle(current);
    if (current.hidden || style.display === 'none' || style.visibility === 'hidden') return false;
    // Closed details hide everything except their first direct summary subtree;
    // computed display/visibility on those hidden descendants do not reflect it.
    if (current !== element && current.matches('details:not([open])')
      && !current.querySelector(':scope > summary')?.contains(element)) return false;
    if (current === panel) break;
  }
  return true;
}

export function Dialog({ open, onClose, title, hideTitleRow, hideClose, children, wide, initialFocusRef }: DialogProps) {
  const [views, setViews] = useState<readonly (DialogChildView & { id: number })[]>([]);
  const nextViewId = useRef(0);
  const viewOpenersRef = useRef(new Map<number, HTMLElement>());
  const previousViewRef = useRef<Readonly<{ depth: number; id: number | null }>>({ depth: 0, id: null });
  const titleId = `${useId()}-title`;
  const panelRef = useRef<HTMLDivElement | null>(null);
  const previouslyFocusedRef = useRef<HTMLElement | null>(null);
  const popView = useCallback(() => setViews((current) => current.slice(0, -1)), [setViews]);
  const pushView = useCallback((view: DialogChildView) => {
    const id = ++nextViewId.current;
    const active = document.activeElement;
    if (active instanceof HTMLElement && panelRef.current?.contains(active)) {
      viewOpenersRef.current.set(id, active);
    }
    setViews((current) => [...current, { ...view, id }]);
    return () => setViews((current) => current.filter((candidate) => candidate.id !== id));
  }, [setViews]);
  const view = views.at(-1) ?? null;
  useEffect(() => {
    if (open) return;
    setViews([]);
    viewOpenersRef.current.clear();
    previousViewRef.current = { depth: 0, id: null };
  }, [open, setViews]);

  /* The opening-focus effect keys only on `open`; without this handoff focus stays on the now-`display:none` opener and popping the child drops focus to <body>. Disposing a non-top LIFO entry is a focus no-op. */
  useEffect(() => {
    if (!open) return;
    const current = { depth: views.length, id: view?.id ?? null };
    const previous = previousViewRef.current;
    previousViewRef.current = current;
    if (current.depth === previous.depth && current.id === previous.id) return;
    if (current.id === previous.id) return;
    const restoring = current.depth < previous.depth && previous.id !== null
      ? viewOpenersRef.current.get(previous.id) ?? null
      : null;
    const frame = requestAnimationFrame(() => {
      const panel = panelRef.current;
      if (panel === null) return;
      const reachable = focusables(panel);
      if (restoring !== null && reachable.includes(restoring)) {
        restoring.focus();
      } else {
        const child = current.depth > 0
          ? panel.querySelector<HTMLElement>('[data-nc-dialog-child-view]')
          : null;
        (child === null ? reachable : focusables(child))[0]?.focus();
      }
      // `.focus()` can silently fail for an element the syntactic filter accepted (e.g. inside a newly-disabled fieldset); verify.
      const active = document.activeElement;
      if (active === null || !(reachable as readonly Element[]).includes(active)) {
        (reachable[0] ?? panel).focus();
      }
      if (current.depth < previous.depth && previous.id !== null && previous.id !== current.id) {
        viewOpenersRef.current.delete(previous.id);
      }
    });
    return () => cancelAnimationFrame(frame);
  }, [open, view?.id, views.length]);
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      const layers = document.querySelectorAll<HTMLElement>('[data-nc-escape-layer]');
      if (layers.item(layers.length - 1) !== panelRef.current) return;
      if (view) { if (view.onEscape) view.onEscape(); else popView(); }
      else onClose();
    };
    const overflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    document.addEventListener('keydown', onKeyDown);
    return () => { document.removeEventListener('keydown', onKeyDown); document.body.style.overflow = overflow; };
  }, [onClose, open, popView, view]);

  // Capture before inert: browsers run unfocusing steps when inert is applied.
  useEffect(() => {
    if (open) previouslyFocusedRef.current = document.activeElement as HTMLElement | null;
  }, [open]);

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
    const frame = requestAnimationFrame(() => {
      const panel = panelRef.current;
      if (!panel) return;
      /* Opening focus yields to a reader who already landed on a real focusable — membership of `focusables(panel)`, not `panel.contains(…)`, which would also yield to the `tabIndex={-1}` panel itself and to a base view hidden under a pushed child. */
      const reachable = focusables(panel);
      const active = document.activeElement;
      // Compared as `Element`, not `HTMLElement`: an SVG anchor matches `a[href]` but is an `SVGElement`.
      if (active !== null && (reachable as readonly Element[]).includes(active)) return;
      /* The named target is checked against the same list: `.focus()` on a disabled or hidden element is a silent no-op that leaves focus outside the modal. */
      const named = initialFocusRef?.current ?? null;
      (named !== null && reachable.includes(named) ? named : reachable[0] ?? panel).focus();
      /* Disability is inherited (`<fieldset disabled>`) and `focusables` only tests the attribute, so read the result back rather than predict it. */
      if (!panel.contains(document.activeElement)) panel.focus();
    });
    return () => {
      cancelAnimationFrame(frame);
      const prior = previouslyFocusedRef.current;
      const target = (prior && document.contains(prior) ? prior : null)
        ?? document.querySelector<HTMLElement>('[data-nc-page-title]');
      if (target && document.contains(target)) target.focus();
    };
  }, [initialFocusRef, open]);

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
    <div ref={panelRef} className={showingView || wide ? 'dialog-panel dialog-panel-wide' : 'dialog-panel'} data-nc-escape-layer=""
      role="dialog" aria-modal="true" aria-label={typeof headerTitle === 'string' ? headerTitle : undefined}
      aria-labelledby={headerTitle && typeof headerTitle !== 'string' ? titleId : undefined}
      tabIndex={-1} onMouseDown={(event) => event.stopPropagation()} onKeyDown={onPanelKeyDown}>
      {headerTitle && (showingView || !hideTitleRow) && <header className="dialog-header"><span id={titleId}>{headerTitle}</span>{(showingView || !hideClose) && <button type="button" data-nc-role="icon" aria-label="Close" onClick={() => {
        if (view) { if (view.onEscape) view.onEscape(); else popView(); }
        else onClose();
      }}><Icon name="close" /></button>}</header>}
      <div className="dialog-body" style={showingView ? { display: 'none' } : undefined}>{children}</div>
      {showingView && (
        <div className="dialog-body dialog-child-view" data-nc-dialog-child-view="">{view.body}</div>
      )}
    </div>
  </div></DialogViewContext.Provider>, document.body);
}

export function ConfirmDialog({ open, title, description, confirmLabel = 'Confirm', cancelLabel = 'Cancel', onConfirm, onCancel, destructive = true, confirmState = 'ready', confirmBusyLabel, initialFocusRef }: ConfirmDialogProps) {
  const cancelRef = useRef<HTMLButtonElement | null>(null);
  const confirmRef = useRef<HTMLButtonElement | null>(null);

  // A really `disabled` Confirm leaves the focus trap, so move focus to Cancel first; `busy` stays focusable and must not yank focus.
  useEffect(() => {
    if (confirmState !== 'blocked') return;
    if (document.activeElement === confirmRef.current) cancelRef.current?.focus();
  }, [confirmState]);

  const busy = confirmState === 'busy';
  const label = confirmBusyLabel === undefined ? confirmLabel : (
    <span className="confirm-dialog-label">
      <span aria-hidden={busy}>{confirmLabel}</span>
      <span aria-hidden={!busy}>{confirmBusyLabel}</span>
    </span>
  );
  return <Dialog open={open} title={title} onClose={onCancel} hideClose
    initialFocusRef={initialFocusRef ?? cancelRef}>
    {description}{busy && <p>Closing this dialog cancels the delete request.</p>}
    <div className="confirm-dialog-actions"><button ref={cancelRef} type="button" data-nc-action="secondary"
      onClick={onCancel}>{cancelLabel}</button>
      <button ref={confirmRef} type="button" data-nc-action={destructive ? 'destructive' : 'primary'}
        disabled={confirmState === 'blocked'}
        aria-busy={busy ? true : undefined} aria-disabled={busy ? true : undefined}
        data-nc-state={busy ? 'busy' : undefined}
        onClick={() => { if (confirmState !== 'ready') return; onConfirm(); }}>{label}</button></div>
  </Dialog>;
}
