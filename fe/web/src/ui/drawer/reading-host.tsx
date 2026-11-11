import { useCallback, useMemo, useRef, type ReactNode, type RefObject } from 'react';
import styles from './reading-host.module.css';

/** UI-owned mounting ports. The host owns geometry; readers own navigation and
 * scroll controls. Refs are read after layout, including after a portal move. */
export type ReadingHostPorts = Readonly<{
  /** Stable presentation identity within one reader, including host remounts. */
  key: string;
  navigation?: RefObject<HTMLElement | null>;
  overlay?: RefObject<HTMLElement | null>;
}>;
type ReadingHostElement = HTMLElement & { ncReadingHost?: ReadingHostPorts };

/** A local registration belongs to this DOM host, so nested/side readers resolve
 * their nearest owner and a removed host leaves no global registry behind. */
export function registerReadingHost(node: HTMLElement, ports: ReadingHostPorts): () => void {
  const host = node as ReadingHostElement;
  host.ncReadingHost = ports;
  return () => { if (host.ncReadingHost === ports) delete host.ncReadingHost; };
}
export function readingHostAround(inside: Element | null): ReadingHostPorts | null {
  for (let node = inside; node !== null; node = node.parentElement) {
    const ports = (node as ReadingHostElement).ncReadingHost;
    if (ports !== undefined) return ports;
  }
  return null;
}
export function useReadingHostRef<T extends HTMLElement>(ports: ReadingHostPorts, observed?: RefObject<T | null>) {
  return useCallback((node: T | null) => {
    if (observed !== undefined) observed.current = node;
    if (node === null) return;
    const unregister = registerReadingHost(node, ports);
    return () => { unregister(); if (observed?.current === node) observed.current = null; };
  }, [ports, observed]);
}

/** In-flow counterpart of Drawer. It supplies the same reader ports without
 * declaring a Drawer overlay or hiding the surrounding page's panel. */
export function EmbeddedReadingHost({ id, label, hidden, children, footer }: Readonly<{
  id: string | undefined; label: string; hidden: boolean; children: ReactNode; footer: ReactNode;
}>) {
  const navigation = useRef<HTMLDivElement>(null);
  const overlay = useRef<HTMLDivElement>(null);
  const ports = useMemo(() => ({ key: 'embedded', navigation, overlay }), []);
  const hostRef = useReadingHostRef<HTMLElement>(ports);
  return <section ref={hostRef} className={styles.host} aria-label={label} hidden={hidden} data-nc-embedded-reading="" data-nc-conversation-region={hidden ? undefined : id}>
      <h2 className={styles.title}>{label}</h2>
      <div className={styles.viewport}>
        <div className={styles.scroll} data-nc-drawer-scroll={hidden ? undefined : ''}><div className={styles.column}>{children}</div></div>
        <div ref={overlay} className={styles.overlay} data-nc-chat-scroll-overlay={hidden ? undefined : ''} />
      </div>
      <div className={styles.footer}>{footer}</div>
      <div ref={navigation} className={styles.navigation} />
    </section>;
}
