import { useVisibleViewport } from '../../ui/viewport/public.ts';
import { createPortal } from 'react-dom';
import { useState } from '../../ui/state/public.ts';
import { mobileFontClassName } from '../../ui/mobile-font/public.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { useCallback, useEffect, useLayoutEffect, useRef, type ComponentProps } from 'react';
import { BottomSheet } from '@astryxdesign/core/BottomSheet';
import { Drawer } from '../../ui/drawer/public.tsx';
import styles from './mobile-chat.module.css';

/** One host owns mobile presentation, focus and visible-viewport geometry for the panel and stationary footer. */
export function ConversationSurface({ mobileSheet, contextTitle, focusInput = false, ...props }: ComponentProps<typeof Drawer> & Readonly<{ mobileSheet: boolean; contextTitle?: string; focusInput?: boolean }>) {
  const [editing, setEditing] = useState(false);
  const previousOpen = useRef(props.open);
  if (previousOpen.current !== props.open) { previousOpen.current = props.open; if (props.open) setEditing(false); }
  const [footerHost, setFooterHost] = useState<HTMLDivElement | null>(null);
  const [navigationBottom, setNavigationBottom] = useState(0);
  const [footerHeight, setFooterHeight] = useState(0);
  const visibleViewport = useVisibleViewport(mobileSheet && (props.open || footerHost !== null) && props.companion === undefined);
  const restingViewportHeight = useRef(visibleViewport.height);
  if (!props.open && footerHost === null) restingViewportHeight.current = visibleViewport.height;
  const expandedForInput = (focusInput || editing) && visibleViewport.height < restingViewportHeight.current;
  const footerHostRef = useRef<HTMLDivElement | null>(null);
  const panelRef = useRef<HTMLDivElement | null>(null);
  const finalFocus = useRef<HTMLElement | null>(null);
  const originalFocus = useRef<HTMLElement | null>(null);
  const focusWasOpen = useRef(false);
  const lastFrame = useRef({ title: props.title, contextTitle, children: props.children, footer: props.footer });
  if (props.open) lastFrame.current = { title: props.title, contextTitle, children: props.children, footer: props.footer };
  const frame = lastFrame.current;
  const capturePanel = useCallback((panel: HTMLDivElement | null) => {
    panelRef.current = panel;
    footerHostRef.current?.remove();
    footerHostRef.current = null;
    if (panel === null) { setFooterHost(null); return; }
    const dialog = panel.closest('dialog');
    if (dialog === null) return;
    const host = document.createElement('div');
    dialog.append(host);
    footerHostRef.current = host;
    setFooterHost(host);
  }, []);
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
    const footer = footerHost?.firstElementChild;
    if (footer === null || footer === undefined) return;
    const sync = () => { setFooterHeight(footer.getBoundingClientRect().height); };
    const observer = new ResizeObserver(sync);
    observer.observe(footer);
    sync();
    return () => { observer.disconnect(); };
  }, [footerHost]);
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
    if (panel === null || dialog == null || footerHost === null) return;
    const observer = new MutationObserver(focusEditor);
    function focusEditor() {
      if (!dialog?.open) return;
      const active = document.activeElement;
      if (active !== document.body && active !== panel && active != null && dialog?.contains(active) && !active.matches('[contenteditable="true"], textarea')) { observer.disconnect(); return; }
      const editor = footerHost?.querySelector<HTMLElement>('[contenteditable="true"], textarea:not([disabled])');
      if (editor === null || editor === undefined) return;
      editor.focus({ preventScroll: true });
      if (document.activeElement === editor) observer.disconnect();
    }
    observer.observe(footerHost, { subtree: true, childList: true, attributes: true, attributeFilter: ['contenteditable', 'disabled'] });
    observer.observe(dialog, { attributes: true, attributeFilter: ['open'] });
    focusEditor();
    return () => { observer.disconnect(); };
  }, [focusInput, footerHost, mobileSheet, props.companion, props.open]);
  const requestedHeight = visibleViewport.height * (expandedForInput ? 0.92 : 2 / 3);
  const sheetHeight = Math.min(requestedHeight, Math.max(0, visibleViewport.bottomEdge - navigationBottom));
  // Grouped conversations retain the pane renderer that owns companion switching.
  if (!mobileSheet || props.companion !== undefined) return <Drawer {...props} />;
  return <div className={styles.sheet}><BottomSheet ref={capturePanel} className={`${styles.panel} ${mobileFontClassName}`} data-nc-mobile-chat-panel=""
    onKeyDown={(event) => {
      // A composer trigger/edit handler already owns this Escape; native sheet dismissal must stand down.
      if (event.key === 'Escape' && event.defaultPrevented) event.stopPropagation();
    }}
    isOpen={props.open} onOpenChange={(open) => { if (!open) props.onClose(); }} hasScrim={false}
    finalFocusRef={finalFocus} label={frame.title || '对话'} height={`${sheetHeight}px`}
    style={{ translate: `0 -${visibleViewport.bottomInset}px` }} purpose="info">
    <section className={styles.content} style={{ paddingBottom: footerHeight }} data-nc-drawer="" id={props.id}>
      <header className={styles.conversationHeader}>
        <div className={styles.conversationHeading}><h2>对话</h2><p>{frame.contextTitle ?? frame.title}</p></div>
        <button type="button" className={styles.close} aria-label={props.closeLabel ?? 'Close conversation'} onClick={props.onClose}><Icon name="close" /></button>
      </header>
      <div className={styles.messages} data-nc-drawer-scroll="">{frame.children}</div>
      {footerHost !== null && createPortal(<div className={styles.footer} data-nc-chat-footer="" data-nc-conversation-region={props.id} style={{ bottom: visibleViewport.bottomInset }} onFocusCapture={(event) => { if (event.target instanceof HTMLElement && event.target.matches('[contenteditable="true"], textarea')) setEditing(true); }}>{frame.footer}</div>, footerHost)}
    </section>
  </BottomSheet></div>;
}
