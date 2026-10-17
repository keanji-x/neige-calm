import { useVisibleViewport } from '../../ui/viewport/public.ts';
import { createPortal } from 'react-dom';
import { useState } from '../../ui/state/public.ts';
import { mobileFontClassName } from '../../ui/mobile-font/public.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { useCallback, useEffect, useLayoutEffect, useRef, type ComponentProps } from 'react';
import { BottomSheet, BottomSheetSwitcher } from '@astryxdesign/core/BottomSheet';
import { Drawer } from '../../ui/drawer/public.tsx';
import { useNativeDialogFinalFocus, usePortalFocusRetention } from '../../ui/drawer/restore-focus.ts';
import styles from './mobile-chat.module.css';

/** One host owns mobile presentation, focus and visible-viewport geometry for the panel and stationary footer. */
export function ConversationSurface({ mobileSheet, contextTitle, focusInput = false, ...props }: ComponentProps<typeof Drawer> & Readonly<{ mobileSheet: boolean; contextTitle?: string; focusInput?: boolean }>) {
  const nativeSheet = mobileSheet && props.companion === undefined;
  const [editing, setEditing] = useState(false);
  const [fullscreen, setFullscreen] = useState(false);
  const previousOpen = useRef(props.open);
  if (previousOpen.current !== props.open) { previousOpen.current = props.open; if (props.open) { setEditing(false); setFullscreen(false); } }
  // The footer's React owner stays fixed while the presentation host changes.
  const [footerHost] = useState(() => document.createElement('div'));
  const [footerAttached, setFooterAttached] = useState(false);
  const [nativePresented, setNativePresented] = useState(false);
  const [navigationBottom, setNavigationBottom] = useState(0);
  const [footerHeight, setFooterHeight] = useState(0);
  const visibleViewport = useVisibleViewport(mobileSheet && (props.open || footerAttached) && props.companion === undefined);
  const restingViewportHeight = useRef(visibleViewport.height);
  if (!props.open && !footerAttached) restingViewportHeight.current = visibleViewport.height;
  const expandedForInput = (focusInput || editing) && visibleViewport.height < restingViewportHeight.current;
  const desktopSlot = useRef<HTMLDivElement | null>(null);
  const [dialog, setDialog] = useState<HTMLDialogElement | null>(null);
  const retainFooterFocus = usePortalFocusRetention(footerHost, dialog, nativeSheet, props.open, footerAttached);
  const previousNativeSheet = useRef(nativeSheet);
  if (previousNativeSheet.current !== nativeSheet) {
    // Snapshot the caret before React removes the previous presentation's DOM.
    retainFooterFocus();
    previousNativeSheet.current = nativeSheet;
  }
  const panelRef = useRef<HTMLDivElement | null>(null);
  const contentRef = useRef<HTMLElement | null>(null);
  // Astryx owns handle drag/settling. Message and header gestures stay local:
  // native touch listeners must stop before its body listener, without cancelling scrolling.
  useEffect(() => {
    const content = contentRef.current;
    if (!nativeSheet || content === null) return;
    const stop = (event: TouchEvent) => event.stopPropagation();
    content.addEventListener('touchstart', stop, { passive: true });
    content.addEventListener('touchmove', stop, { passive: true });
    return () => {
      content.removeEventListener('touchstart', stop);
      content.removeEventListener('touchmove', stop);
    };
  }, [nativeSheet, dialog]);
  const finalFocus = useRef<HTMLElement | null>(null);
  const originalFocus = useRef<HTMLElement | null>(null);
  const focusWasOpen = useRef(false);
  const lastFrame = useRef({ title: props.title, contextTitle, children: props.children, footer: props.footer });
  if (props.open) lastFrame.current = { title: props.title, contextTitle, children: props.children, footer: props.footer };
  const frame = lastFrame.current;
  const captureDesktop = useCallback((slot: HTMLDivElement | null) => {
    if (footerHost.parentElement === desktopSlot.current) { retainFooterFocus(); footerHost.remove(); }
    desktopSlot.current = slot;
    if (slot !== null) slot.append(footerHost);
    setFooterAttached(slot !== null);
  }, [footerHost, retainFooterFocus]);
  const captureDialog = useCallback((next: HTMLDialogElement | null) => {
    if (footerHost.parentElement !== desktopSlot.current) {
      retainFooterFocus();
      footerHost.remove();
    }
    setDialog(next);
    if (previousNativeSheet.current) {
      if (next !== null) next.append(footerHost);
      setFooterAttached(next !== null);
    }
  }, [footerHost, retainFooterFocus]);
  useLayoutEffect(() => {
    if (nativeSheet && dialog !== null) {
      dialog.append(footerHost);
      setFooterAttached(true);
    }
  }, [nativeSheet, dialog, footerHost]);
  useEffect(() => {
    if (!nativeSheet || dialog == null) return;
    const sync = () => setNativePresented(dialog.open);
    const observer = new MutationObserver(sync);
    observer.observe(dialog, { attributes: true, attributeFilter: ['open'] });
    sync();
    return () => observer.disconnect();
  }, [nativeSheet, dialog]);
  useLayoutEffect(() => {
    if (!mobileSheet || !props.open || props.companion !== undefined) return;
    const navigation = document.querySelector<HTMLElement>('[data-nc-workspace-header] [data-nc-mobile-header]');
    if (navigation === null) return;
    const sync = () => {
      const spacing = Number.parseFloat(getComputedStyle(navigation).paddingBottom);
      setNavigationBottom(navigation.getBoundingClientRect().bottom + spacing);
    };
    const observer = new ResizeObserver(sync);
    observer.observe(navigation);
    sync();
    return () => { observer.disconnect(); };
  }, [mobileSheet, props.companion, props.open]);
  useLayoutEffect(() => {
    const footer = footerHost.firstElementChild;
    if (footer === null || footer === undefined) return;
    const sync = () => { setFooterHeight(footer.getBoundingClientRect().height); };
    const observer = new ResizeObserver(sync);
    observer.observe(footer);
    sync();
    return () => { observer.disconnect(); };
  }, [footerHost, footerAttached, mobileSheet]);
  useLayoutEffect(() => {
    if (props.open && !focusWasOpen.current) {
      const active = document.activeElement;
      if (active instanceof HTMLElement && active !== document.body && !dialog?.contains(active)) originalFocus.current = active;
    }
    focusWasOpen.current = props.open;
    if (!props.open) {
      const dock = document.querySelector<HTMLElement>('[data-nc-chat-dock]');
      const origin = originalFocus.current;
      finalFocus.current = dock ?? (origin?.isConnected ? origin : null);
    }
  }, [dialog, props.open]);
  useEffect(() => {
    if (!mobileSheet || props.companion !== undefined || !props.open || !focusInput) return;
    const panel = panelRef.current;
    if (panel === null || dialog == null || !footerAttached) return;
    const observer = new MutationObserver(focusEditor);
    function focusEditor() {
      if (!dialog?.open) return;
      const active = document.activeElement;
      if (active !== document.body && active !== panel && active != null && dialog?.contains(active) && !active.matches('[contenteditable="true"], textarea')) { observer.disconnect(); return; }
      const editor = footerHost.querySelector<HTMLElement>('[contenteditable="true"], textarea:not([disabled])');
      if (editor === null || editor === undefined) return;
      editor.focus({ preventScroll: true });
      if (document.activeElement === editor) observer.disconnect();
    }
    observer.observe(footerHost, { subtree: true, childList: true, attributes: true, attributeFilter: ['contenteditable', 'disabled'] });
    observer.observe(dialog, { attributes: true, attributeFilter: ['open'] });
    focusEditor();
    return () => { observer.disconnect(); };
  }, [dialog, focusInput, footerHost, footerAttached, mobileSheet, props.companion, props.open]);
  useNativeDialogFinalFocus(dialog, props.open, finalFocus);
  const requestedHeight = visibleViewport.height * (fullscreen ? 1 : expandedForInput ? 0.92 : 0.8);
  const sheetHeight = fullscreen ? requestedHeight : Math.min(requestedHeight, Math.max(0, visibleViewport.bottomEdge - navigationBottom));
  // Keep the portal at one React location across presentation changes. Switcher
  // supplies the native shared depth to both messages and the stationary footer.
  const footer = createPortal(<div className={nativeSheet ? styles.footer : undefined}
    data-nc-chat-footer={nativeSheet ? '' : undefined} data-nc-conversation-region={props.id}
    style={nativeSheet ? { bottom: visibleViewport.bottomInset } : undefined}
    onFocusCapture={(event) => { if (event.target instanceof HTMLElement && event.target.matches('[contenteditable="true"], textarea')) setEditing(true); }}
  >{props.open || (nativeSheet ? nativePresented : footerAttached) ? frame.footer : null}</div>, footerHost);
  return <><div className={nativeSheet ? styles.sheet : undefined}>
    <BottomSheetSwitcher ref={captureDialog} activeSheet={nativeSheet && props.open ? 'conversation' : null}
      onActiveSheetChange={(sheetId) => { if (sheetId === null) props.onClose(); }} hasScrim={false}>
    {nativeSheet && <BottomSheet ref={panelRef} sheetId="conversation" className={`${styles.panel} ${mobileFontClassName}`} data-nc-mobile-chat-panel="" data-nc-fullscreen={fullscreen}
      label={frame.title || '对话'} height={`${sheetHeight}px`}
      style={{ translate: `0 -${visibleViewport.bottomInset}px` }} purpose="info">
    <section ref={contentRef} onPointerDown={(event) => event.stopPropagation()} onPointerMove={(event) => event.stopPropagation()} className={styles.content} data-nc-drawer="" id={props.id}>
      <header className={styles.conversationHeader}>
        <h2 className={styles.conversationHeading}>{frame.contextTitle ?? frame.title}</h2>
        <button type="button" className={styles.close} aria-label={fullscreen ? 'Collapse conversation' : 'Expand conversation'} aria-pressed={fullscreen} onClick={() => setFullscreen(value => !value)}><Icon name={fullscreen ? 'compact' : 'fullscreen'} /></button>
        <button type="button" className={styles.close} aria-label={props.closeLabel ?? 'Close conversation'} onClick={props.onClose}><Icon name="close" /></button>
      </header>
      <div className={styles.messageViewport}>
        <div className={styles.messages} style={{ paddingBottom: `calc(${footerHeight}px + var(--space-4))`, scrollPaddingBottom: footerHeight }} data-nc-drawer-scroll="">{frame.children}</div>
        <div className={styles.scrollOverlay} style={{ bottom: footerHeight }} data-nc-chat-scroll-overlay="" />
      </div>
    </section>
    </BottomSheet>}
    {footer}
    </BottomSheetSwitcher>
  </div>{!nativeSheet && <Drawer {...props} footer={<div ref={captureDesktop} />} />}</>;
}
