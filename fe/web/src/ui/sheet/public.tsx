import { LayerDismissal } from '../dialog/layer-dismissal.tsx';
import { useCallback, useEffect, useEffectEvent, useLayoutEffect, useRef, type CSSProperties, type ReactNode, type Ref, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import { LayerDepthProvider, type UseLayerDismissalReturn } from '@astryxdesign/core/Layer';
import { useState } from '../state/public.ts';
import { restoreDrawerFocus, usePortalFocusRetention } from '../drawer/restore-focus.ts';
import { createSheetMotion, type SheetMotion } from './motion.ts';
import { createSheetGesture } from './gesture.ts';
import { notifyAncestorVisualChange } from '../hover-preview/visual-change.ts';
import styles from './sheet.module.css';

export type SheetFrame = Readonly<{ heightPx: number; bottomInsetPx: number }>;
export type SheetCloseReason = 'handle' | 'escape' | 'platform';
export type SheetProps = Readonly<{
  enabled: boolean; open: boolean; label: string; frame: SheetFrame;
  children: ReactNode; footer: ReactNode;
  desktopFooterRef: RefObject<HTMLDivElement | null>;
  dialogRef: Ref<HTMLDialogElement>; panelRef: Ref<HTMLDivElement>;
  finalFocusRef: RefObject<HTMLElement | null>;
  focusInput: boolean;
  resolveInitialFocus: (footerHost: HTMLElement) => HTMLElement | null;
  onCloseRequest: (reason: SheetCloseReason) => void;
  onFooterHeight: (heightPx: number) => void;
}>;
type ExitFocus = { node: HTMLDialogElement; revoked: boolean; stop: () => void };
type Owner = { motion: SheetMotion; gesture: ReturnType<typeof createSheetGesture> };

/** Controlled, nonmodal single Sheet. Owns native lifetime, one paint panel,
 * handle-only leases, retained permissions and one stable footer portal. The
 * caller owns layout frame/contents and refs observe DOM, never write paint. */
export function Sheet(props: SheetProps) {
  const { enabled, open, frame, desktopFooterRef, dialogRef, panelRef } = props;
  const [host] = useState(() => document.createElement('div'));
  const [dialog, setDialog] = useState<HTMLDialogElement | null>(null);
  const [panel, setPanel] = useState<HTMLDivElement | null>(null);
  const [present, setPresent] = useState(enabled && open);
  const [attached, setAttached] = useState(false);
  const latest = useRef(props); latest.current = props;
  const owner = useRef<Owner | null>(null);
  const retain = usePortalFocusRetention(host, dialog, enabled, open, attached);
  const previousEnabled = useRef(enabled);
  const presentationSnapshot = useRef(false);
  if (previousEnabled.current !== enabled) { retain(); presentationSnapshot.current = true; previousEnabled.current = enabled; }
  const retained = useRef({ children: props.children, footer: props.footer, frame, label: props.label });
  if (open) retained.current = { children: props.children, footer: props.footer, frame, label: props.label };
  const visible = open ? { children: props.children, footer: props.footer, frame, label: props.label } : retained.current;
  const intent = useRef<object>({});
  // A geometry rebase replaces playback intent, but never renews a close's
  // default focus authority. Reopen/host disposal ends this separate lifetime.
  const exitFocus = useRef<ExitFocus | null>(null);
  const clearExitFocus = useCallback(() => {
    exitFocus.current?.stop(); exitFocus.current = null;
  }, []);
  const requested = useRef<{ enabled: boolean; open: boolean; height: number; bottom: number; panel: HTMLDivElement | null } | null>(null);
  const active = enabled && open;
  const reduced = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const finish = useEffectEvent((token: object, node: HTMLDialogElement) => {
    if (intent.current !== token || latest.current.open || !latest.current.enabled || dialog !== node || !node.isConnected) return;
    if (!node.open) return;
    const authority = exitFocus.current;
    clearExitFocus(); // Native close's own focus events cannot become a choice.
    node.close();
    if (intent.current !== token || latest.current.open || !latest.current.enabled || dialog !== node || !node.isConnected) return;
    setPresent(false);
    const focus = latest.current.finalFocusRef.current;
    if (authority?.node === node && !authority.revoked && focus?.isConnected) restoreDrawerFocus(focus);
  });
  const settle = useEffectEvent(() => { owner.current?.motion.move(0, reduced(), () => {}); });
  const dismiss = useEffectEvent(() => {
    const token = intent.current;
    latest.current.onCloseRequest('handle');
    // A synchronous controlled commit can revoke this release before returning.
    if (intent.current === token && latest.current.enabled && latest.current.open) settle();
  });
  const dismissal = useRef<UseLayerDismissalReturn | null>(null);
  const dismissalLayer = <LayerDismissal {...{ isActive: active, getContainer: () => dialog,
    isPresent: () => latest.current.enabled && latest.current.open && dialog?.open === true,
    onDismiss: () => props.onCloseRequest('escape') }} ref={dismissal} />;
  const captureDialog = useCallback((node: HTMLDialogElement | null) => { setDialog(node); }, [setDialog]);
  const capturePanel = useCallback((node: HTMLDivElement | null) => { setPanel(node); }, [setPanel]);
  useLayoutEffect(() => {
    if (typeof dialogRef === 'function') {
      const release = dialogRef(dialog);
      return () => { if (typeof release === 'function') release(); else dialogRef(null); };
    }
    if (dialogRef !== null) dialogRef.current = dialog;
    return () => { if (dialogRef !== null) dialogRef.current = null; };
  }, [dialog, dialogRef]);
  useLayoutEffect(() => {
    if (typeof panelRef === 'function') {
      const release = panelRef(panel);
      return () => { if (typeof release === 'function') release(); else panelRef(null); };
    }
    if (panelRef !== null) panelRef.current = panel;
    return () => { if (panelRef !== null) panelRef.current = null; };
  }, [panel, panelRef]);
  useLayoutEffect(() => {
    if (panel === null) return;
    const motion = createSheetMotion(panel);
    const gesture = createSheetGesture(motion, () => latest.current.enabled && latest.current.open && Number.isFinite(latest.current.frame.heightPx) && latest.current.frame.heightPx > 0 && panel.isConnected && dialog?.open === true,
      () => latest.current.frame.heightPx, settle, dismiss);
    const current = { motion, gesture }; owner.current = current; requested.current = null;
    const cancel = () => gesture.cancel();
    const hidden = () => { if (document.hidden) cancel(); };
    window.addEventListener('blur', cancel); document.addEventListener('visibilitychange', hidden);
    return () => {
      intent.current = {}; clearExitFocus(); gesture.revoke(); motion.dispose();
      if (owner.current === current) owner.current = null;
      window.removeEventListener('blur', cancel); document.removeEventListener('visibilitychange', hidden);
    };
  }, [panel, dialog, clearExitFocus]);
  useLayoutEffect(() => {
    const prior = requested.current;
    if (prior?.enabled === enabled && prior.open === open && prior.height === frame.heightPx && prior.bottom === frame.bottomInsetPx && prior.panel === panel) return;
    requested.current = { enabled, open, height: frame.heightPx, bottom: frame.bottomInsetPx, panel };
    const token = {}; intent.current = token;
    if (open || !enabled) clearExitFocus();
    if (!enabled) {
      owner.current?.gesture.revoke();
      if (dialog?.open) dialog.close();
      setPresent(false); return;
    }
    if (!open && dialog?.open && exitFocus.current === null) {
      const node = dialog;
      const doc = node.ownerDocument;
      const authority: ExitFocus = { node, revoked: false, stop: () => doc.removeEventListener('focusin', chosen, true) };
      const chosen = (event: FocusEvent) => {
        const target = event.target;
        if (exitFocus.current !== authority || latest.current.open || !latest.current.enabled
          || !node.isConnected || !node.open || !event.isTrusted || !(target instanceof HTMLElement)
          || doc.activeElement !== target || !target.isConnected || target === doc.body || target === doc.documentElement
          || node.contains(target) || target.closest('[inert], [hidden], [aria-hidden="true"], [aria-disabled="true"]') !== null
          || target.matches(':disabled') || target.getClientRects().length === 0 || getComputedStyle(target).visibility !== 'visible') return;
        authority.revoked = true;
      };
      exitFocus.current = authority;
      doc.addEventListener('focusin', chosen, true);
    }
    if (panel === null || dialog === null || owner.current === null || !Number.isFinite(frame.heightPx) || frame.heightPx <= 0) return;
    // A native-closed host cannot supply a new painted position. In
    // particular, its zero border box is not a closed endpoint to cache.
    if (!open && !dialog.open) { owner.current.gesture.revoke(); return; }
    if (open) {
      setPresent(true);
      if (!dialog.open) dialog.show();
    } else owner.current.gesture.revoke();
    // Frame rebase during a lease retains both q/v and its pointer anchor.
    if (open && owner.current.gesture.dragging()) { owner.current.motion.hold(); return; }
    owner.current.motion.move(open ? 0 : panel.getBoundingClientRect().height, reduced(), () => { if (!open) finish(token, dialog); });
  }, [enabled, open, frame.heightPx, frame.bottomInsetPx, panel, dialog, setPresent, clearExitFocus]);
  useLayoutEffect(() => {
    const preference = window.matchMedia('(prefers-reduced-motion: reduce)');
    const change = () => {
      const current = owner.current;
      if (!preference.matches || !latest.current.enabled || current === null || panel === null || dialog === null || !dialog.open
        || !Number.isFinite(latest.current.frame.heightPx) || latest.current.frame.heightPx <= 0) return;
      current.gesture.revoke(); const token = intent.current;
      current.motion.move(latest.current.open ? 0 : panel.getBoundingClientRect().height, true, () => { if (!latest.current.open) finish(token, dialog); });
    };
    preference.addEventListener('change', change);
    return () => { preference.removeEventListener('change', change); };
  }, [panel, dialog]);
  // The only portal host moves; native close must precede selection restoration
  // at the destination. Capture before either DOM removal or append.
  useLayoutEffect(() => {
    const destination = enabled ? dialog : desktopFooterRef.current;
    if (host.parentElement !== destination) {
      if (!presentationSnapshot.current) retain();
      host.remove(); destination?.append(host);
      if (destination !== null) presentationSnapshot.current = false;
    }
    setAttached(destination !== null);
  }, [enabled, dialog, desktopFooterRef, host, retain, setAttached]);
  useLayoutEffect(() => () => { intent.current = {}; clearExitFocus(); host.remove(); if (dialog?.open) dialog.close(); }, [host, dialog, clearExitFocus]);
  useLayoutEffect(() => {
    // Bottom-inset layout changes move the untransformed footer too. Position
    // changes do not notify ResizeObserver; use the same scoped owner notice.
    if (enabled && open && host.isConnected) notifyAncestorVisualChange(host);
  }, [enabled, open, visible.frame.bottomInsetPx, host]);
  const heightChanged = useEffectEvent((height: number) => props.onFooterHeight(height));
  useLayoutEffect(() => {
    const footer = host.firstElementChild;
    if (footer === null || !attached) return;
    const sync = () => { if (footer.isConnected && host.firstElementChild === footer) heightChanged(footer.getBoundingClientRect().height); };
    const observer = new ResizeObserver(sync); observer.observe(footer); sync();
    return () => { observer.disconnect(); };
  }, [host, attached, enabled, open, present]);
  useEffect(() => {
    if (!enabled || !open || !props.focusInput || !attached || dialog === null || panel === null) return;
    let live = true;
    const stop = () => { live = false; observer.disconnect(); document.removeEventListener('focusin', chosen, true); };
    const chosen = (event: FocusEvent) => {
      if (event.target instanceof HTMLElement && event.target !== panel && event.target !== dialog
        && event.target !== latest.current.resolveInitialFocus(host)) stop();
    };
    const focus = () => {
      if (!live || !latest.current.enabled || !latest.current.open || !dialog.open) return;
      const target = latest.current.resolveInitialFocus(host);
      if (target === null || !target.isConnected || target.closest('[inert], [aria-hidden="true"]') !== null) return;
      restoreDrawerFocus(target); if (document.activeElement === target) stop();
    };
    const observer = new MutationObserver(focus);
    document.addEventListener('focusin', chosen, true);
    observer.observe(host, { subtree: true, childList: true, attributes: true, attributeFilter: ['contenteditable', 'disabled'] });
    focus(); return stop;
  }, [enabled, open, props.focusInput, attached, dialog, panel, host]);

  return <>{dismissalLayer}<LayerDepthProvider><dialog ref={captureDialog} className={styles.dialog} aria-label={visible.label}
    onCancel={event => { event.preventDefault(); if (dismissal.current?.shouldDismissOnCloseRequest()) props.onCloseRequest('platform'); }}>
    {enabled && <div className={styles.frame} data-nc-sheet-frame="" style={{ height: visible.frame.heightPx, bottom: visible.frame.bottomInsetPx, '--sheet-open-top': `calc(100vh - ${visible.frame.heightPx}px - ${visible.frame.bottomInsetPx}px)` } as CSSProperties}>
      <div ref={capturePanel} className={styles.panel} data-nc-sheet-panel="" tabIndex={-1} inert={!active} aria-hidden={!active ? true : undefined}>
        <div className={styles.handle} data-nc-sheet-handle="" aria-hidden="true"
          onPointerDown={event => owner.current?.gesture.down(event, event.currentTarget)}
          onPointerMove={event => owner.current?.gesture.move(event)} onPointerUp={event => owner.current?.gesture.up(event)}
          onPointerCancel={event => owner.current?.gesture.cancelPointer(event)} onLostPointerCapture={event => owner.current?.gesture.cancelPointer(event)}
          onContextMenu={event => { event.preventDefault(); owner.current?.gesture.cancel(); }}><div /></div>
        <div className={styles.content} data-nc-drawer={open || present ? '' : undefined}>{open || present ? visible.children : null}</div>
      </div>
    </div>}
  </dialog>{createPortal(<div className={enabled ? styles.footer : undefined} data-nc-sheet-footer=""
    inert={enabled && !open} aria-hidden={enabled && !open ? true : undefined}
    style={enabled ? { bottom: visible.frame.bottomInsetPx } : undefined}>{open || (enabled ? present : attached) ? visible.footer : null}</div>, host)}</LayerDepthProvider></>;
}
