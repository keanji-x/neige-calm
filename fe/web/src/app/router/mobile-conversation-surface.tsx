import { useVisibleViewport } from '../../ui/viewport/public.ts';
import { createPortal } from 'react-dom';
import { useState } from '../../ui/state/public.ts';
import { mobileFontClassName } from '../../ui/mobile-font/public.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { useCallback, useEffect, useLayoutEffect, useRef, type ComponentProps } from 'react';
import { LayerDepthProvider, useLayerDismissal } from '@astryxdesign/core/Layer';
import { BottomSheet } from '@astryxdesign/core/BottomSheet';
import { ChatLayout } from '@astryxdesign/core/Chat';
import { Drawer } from '../../ui/drawer/public.tsx';
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
  const retainedFocus = useRef<Readonly<{ element: HTMLElement; range: Range | null }> | null>(null);
  const retainFooterFocus = useCallback(() => {
    const active = document.activeElement;
    if (!(active instanceof HTMLElement) || !footerHost.contains(active)) return;
    const selection = document.getSelection();
    const range = selection !== null && selection.rangeCount > 0 ? selection.getRangeAt(0) : null;
    retainedFocus.current = { element: active, range: range !== null && footerHost.contains(range.commonAncestorContainer) ? range.cloneRange() : null };
  }, [footerHost]);
  const previousNativeSheet = useRef(nativeSheet);
  if (previousNativeSheet.current !== nativeSheet) {
    // Snapshot the caret before React removes the previous presentation's DOM.
    retainFooterFocus();
    previousNativeSheet.current = nativeSheet;
  }
  const panelRef = useRef<HTMLDivElement | null>(null);
  // The transcript's existing scroll follower is the sole owner of reading
  // intent. ChatLayout supplies layout/dock material without a second scroller.
  const layoutScrollRef = useRef<HTMLElement | null>(null);
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
  }, [nativeSheet, footerAttached]);
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
  const capturePanel = useCallback((panel: HTMLDivElement | null) => {
    const previousDialog = panelRef.current?.closest('dialog');
    if (previousDialog !== undefined && footerHost.parentElement === previousDialog) { retainFooterFocus(); footerHost.remove(); }
    panelRef.current = panel;
    const dialog = panel?.closest('dialog');
    if (dialog != null) dialog.append(footerHost);
    setFooterAttached(dialog != null);
  }, [footerHost, retainFooterFocus]);
  useEffect(() => {
    const dialog = panelRef.current?.closest('dialog');
    if (!nativeSheet || dialog == null) return;
    const sync = () => setNativePresented(dialog.open);
    const observer = new MutationObserver(sync);
    observer.observe(dialog, { attributes: true, attributeFilter: ['open'] });
    sync();
    return () => observer.disconnect();
  }, [nativeSheet, footerAttached]);
  useLayerDismissal({ isActive: nativeSheet && props.open, onDismiss: props.onClose,
    getContainer: () => panelRef.current?.closest('dialog') ?? null,
    isPresent: () => panelRef.current?.closest('dialog')?.open ?? false });
  useEffect(() => {
    if (!props.open || !footerAttached) return;
    const dialog = panelRef.current?.closest('dialog');
    const restore = () => {
      const retained = retainedFocus.current;
      if (retained === null) return;
      if (!footerHost.contains(retained.element)) { retainedFocus.current = null; return; }
      if ((dialog != null && !dialog.open) || retained.element.closest('[inert], [aria-hidden="true"]') !== null) return;
      retained.element.focus({ preventScroll: true });
      if (document.activeElement !== retained.element) return;
      if (retained.range !== null) {
        const selection = document.getSelection();
        selection?.removeAllRanges();
        selection?.addRange(retained.range);
      }
      retainedFocus.current = null;
    };
    restore();
    if (dialog == null) return;
    const observer = new MutationObserver(restore);
    observer.observe(dialog, { attributes: true, attributeFilter: ['open'] });
    return () => observer.disconnect();
  }, [footerAttached, footerHost, nativeSheet, props.open]);
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
      const dialog = panelRef.current?.closest('dialog');
      if (active instanceof HTMLElement && active !== document.body && !dialog?.contains(active)) originalFocus.current = active;
    }
    focusWasOpen.current = props.open;
    if (!props.open) {
      const dock = document.querySelector<HTMLElement>('[data-nc-chat-dock]');
      const origin = originalFocus.current;
      finalFocus.current = dock ?? (origin?.isConnected ? origin : null);
    }
  }, [props.open]);
  useEffect(() => {
    if (!mobileSheet || props.companion !== undefined || !props.open || !focusInput) return;
    const panel = panelRef.current;
    const dialog = panel?.closest('dialog');
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
  }, [focusInput, footerHost, footerAttached, mobileSheet, props.companion, props.open]);
  const requestedHeight = visibleViewport.height * (fullscreen ? 1 : expandedForInput ? 0.92 : 0.8);
  const sheetHeight = fullscreen ? requestedHeight : Math.min(requestedHeight, Math.max(0, visibleViewport.bottomEdge - navigationBottom));
  // Grouped conversations retain the pane renderer that owns companion switching.
  const surface = !nativeSheet ? <Drawer {...props} footer={<div ref={captureDesktop} />} /> : <div className={styles.sheet}><BottomSheet ref={capturePanel} className={`${styles.panel} ${mobileFontClassName}`} data-nc-mobile-chat-panel="" data-nc-fullscreen={fullscreen}
    onKeyDown={(event) => {
      // A composer trigger/edit handler already owns this Escape; native sheet dismissal must stand down.
      if (event.key === 'Escape' && event.defaultPrevented) event.stopPropagation();
    }}
    isOpen={props.open} onOpenChange={(open) => { if (!open) props.onClose(); }} hasScrim={false}
    finalFocusRef={finalFocus} label={frame.title || '对话'} height={`${sheetHeight}px`}
    style={{ translate: `0 -${visibleViewport.bottomInset}px` }} purpose="info">
    <section ref={contentRef} onPointerDown={(event) => event.stopPropagation()} onPointerMove={(event) => event.stopPropagation()} className={styles.content} style={{ paddingBottom: footerHeight }} data-nc-drawer="" id={props.id}>
      <header className={styles.conversationHeader}>
        <h2 className={styles.conversationHeading}>{frame.contextTitle ?? frame.title}</h2>
        <button type="button" className={styles.close} aria-label={fullscreen ? 'Collapse conversation' : 'Expand conversation'} aria-pressed={fullscreen} onClick={() => setFullscreen(value => !value)}><Icon name={fullscreen ? 'compact' : 'fullscreen'} /></button>
        <button type="button" className={styles.close} aria-label={props.closeLabel ?? 'Close conversation'} onClick={props.onClose}><Icon name="close" /></button>
      </header>
      <ChatLayout className={styles.messageViewport} data-nc-native-chat-layout="" density="compact"
        scrollRef={layoutScrollRef} composer={null}
        scrollButton={<div className={styles.scrollOverlay} data-nc-chat-scroll-overlay="" />}>
        <div className={styles.messages} data-nc-drawer-scroll="">{frame.children}</div>
      </ChatLayout>
    </section>
  </BottomSheet></div>;
  return <>{surface}{createPortal(<LayerDepthProvider><div className={nativeSheet ? styles.footer : undefined}
    data-nc-chat-footer={nativeSheet ? '' : undefined} data-nc-conversation-region={props.id}
    style={nativeSheet ? { bottom: visibleViewport.bottomInset } : undefined}
    onFocusCapture={(event) => { if (event.target instanceof HTMLElement && event.target.matches('[contenteditable="true"], textarea')) setEditing(true); }}
  >{props.open || (nativeSheet ? nativePresented : footerAttached) ? frame.footer : null}</div></LayerDepthProvider>, footerHost)}</>;
}
