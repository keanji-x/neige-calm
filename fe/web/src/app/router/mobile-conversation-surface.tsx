import { GuideAnchor } from '../../ui/floating-control/public.ts';
import { useVisibleViewport } from '../../ui/viewport/public.ts';
import { useState } from '../../ui/state/public.ts';
import { mobileFontClassName } from '../../ui/mobile-font/public.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { useCallback, useLayoutEffect, useMemo, useRef, type ComponentProps, type RefCallback } from 'react';
import { Drawer } from '../../ui/drawer/public.tsx';
import { Sheet } from '../../ui/sheet/public.tsx';
import styles from './mobile-chat.module.css';

/** Conversation assembly: viewport-derived layout and business intent. Sheet
 * owns native presentation, motion, portal migration and focus timing. */
export function ConversationSurface({ mobileSheet, contextTitle, focusInput = false, ...props }: ComponentProps<typeof Drawer> & Readonly<{ mobileSheet: boolean; contextTitle?: string; focusInput?: boolean }>) {
  const [editing, setEditing] = useState(false);
  const [fullscreen, setFullscreen] = useState(false);
  const previousOpen = useRef(props.open);
  if (previousOpen.current !== props.open) { previousOpen.current = props.open; if (props.open) { setEditing(false); setFullscreen(false); } }
  const [navigationBottom, setNavigationBottom] = useState(0);
  const [footerHeight, setFooterHeight] = useState(0);
  const visibleViewport = useVisibleViewport(mobileSheet);
  const restingViewportHeight = useRef(visibleViewport.height);
  if (!props.open) restingViewportHeight.current = visibleViewport.height;
  const expandedForInput = (focusInput || editing) && visibleViewport.height < restingViewportHeight.current;
  const [desktopNode, setDesktopNode] = useState<HTMLDivElement | null>(null);
  const desktopSlot = useMemo(() => ({ current: desktopNode }), [desktopNode]);
  const dialog = useRef<HTMLDialogElement | null>(null);
  const panelRef = useRef<HTMLDivElement | null>(null);
  // Preserve the application's public surface marker on the observed panel;
  // this ref never writes geometry or paint.
  const capturePanel = useCallback((node: HTMLDivElement | null) => {
    panelRef.current = node;
    node?.setAttribute('data-nc-mobile-chat-panel', '');
  }, []);
  const finalFocus = useRef<HTMLElement | null>(null);
  const originalFocus = useRef<HTMLElement | null>(null);
  const focusWasOpen = useRef(false);
  if (props.open && !focusWasOpen.current) {
    const active = document.activeElement;
    if (active instanceof HTMLElement && active !== document.body && !dialog.current?.contains(active)) originalFocus.current = active;
  }
  focusWasOpen.current = props.open;
  if (!props.open) {
    const origin = originalFocus.current;
    finalFocus.current = document.querySelector<HTMLElement>('[data-nc-chat-dock]') ?? (origin?.isConnected ? origin : null);
  }
  useLayoutEffect(() => {
    if (!mobileSheet || !props.open) return;
    const navigation = document.querySelector<HTMLElement>('[data-nc-workspace-header] [data-nc-mobile-header]');
    if (navigation === null) return;
    const sync = () => {
      if (!navigation.isConnected || navigation.getClientRects().length === 0) return;
      const spacing = Number.parseFloat(getComputedStyle(navigation).paddingBottom);
      setNavigationBottom(navigation.getBoundingClientRect().bottom + spacing);
    };
    const observer = new ResizeObserver(sync); observer.observe(navigation); sync();
    return () => { observer.disconnect(); };
  }, [mobileSheet, props.open, setNavigationBottom]);
  const resolveInitialFocus = useCallback((host: HTMLElement) => host.querySelector<HTMLElement>('[contenteditable="true"], textarea:not([disabled])'), []);
  const requestedHeight = visibleViewport.height * (fullscreen ? 1 : expandedForInput ? 0.92 : 0.8);
  const sheetHeight = fullscreen ? requestedHeight : Math.min(requestedHeight, Math.max(0, visibleViewport.bottomEdge - navigationBottom));
  const frame = { title: props.title, contextTitle, children: props.children };
  const close = (anchor?: RefCallback<HTMLElement>, describedBy?: string) => <button ref={anchor} aria-describedby={describedBy} type="button" className={styles.close} aria-label={props.closeLabel ?? 'Close conversation'} onClick={props.onClose}><Icon name="close" /></button>;
  return <><Drawer {...props} open={!mobileSheet && props.open} companionVisible={!mobileSheet && props.companionVisible}
    footer={mobileSheet ? undefined : <div ref={setDesktopNode} />}>{mobileSheet ? null : props.children}</Drawer>
    <Sheet enabled={mobileSheet} open={props.open} label={props.title || '对话'} frame={{ heightPx: sheetHeight, bottomInsetPx: visibleViewport.bottomInset }}
      desktopFooterRef={desktopSlot} dialogRef={dialog} panelRef={capturePanel} finalFocusRef={finalFocus}
      focusInput={focusInput} resolveInitialFocus={resolveInitialFocus} onCloseRequest={props.onClose} onFooterHeight={setFooterHeight}
      footer={<div className={mobileSheet ? `${styles.footerContent} ${mobileFontClassName}` : undefined} data-nc-chat-footer={mobileSheet ? '' : undefined} data-nc-conversation-region={props.id}
        onFocusCapture={event => { if (event.target instanceof HTMLElement && event.target.matches('[contenteditable="true"], textarea')) setEditing(true); }}>{props.footer}</div>}>
      <section className={`${styles.content} ${mobileFontClassName}`} data-nc-fullscreen={fullscreen} id={props.id}>
      <header className={styles.conversationHeader}>
        <h2 className={styles.conversationHeading}>{frame.contextTitle ?? frame.title}</h2>
        <button type="button" className={styles.close} aria-label={fullscreen ? 'Collapse conversation' : 'Expand conversation'} aria-pressed={fullscreen} onClick={() => setFullscreen(value => !value)}><Icon name={fullscreen ? 'compact' : 'fullscreen'} /></button>
        {props.closeGuideId === undefined ? close() : <GuideAnchor id={props.closeGuideId}>{close}</GuideAnchor>}
      </header>
      <div className={styles.messageViewport}>
        <div className={styles.messages} style={{ paddingBottom: `calc(${footerHeight}px + var(--space-4))`, scrollPaddingBottom: footerHeight }} data-nc-drawer-scroll="">{frame.children}</div>
        <div className={styles.scrollOverlay} style={{ bottom: footerHeight }} data-nc-chat-scroll-overlay="" />
      </div>
      </section>
    </Sheet></>;
}
