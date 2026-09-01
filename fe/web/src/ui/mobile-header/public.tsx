import { Heading as AstryxHeading } from '@astryxdesign/core/Heading';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { IconButton as AstryxIconButton } from '@astryxdesign/core/IconButton';
import { useEffect, useRef, type ReactNode } from 'react';

import { useState } from '../state/public.ts';
import styles from './mobile-header.module.css';

function scrollHosts(header: HTMLElement): HTMLElement[] {
  const hosts: HTMLElement[] = [];
  const drawer = header.closest<HTMLElement>('[data-nc-drawer]');
  const drawerScroll = drawer?.querySelector<HTMLElement>('[data-nc-drawer-scroll]');
  if (drawerScroll !== null && drawerScroll !== undefined) hosts.push(drawerScroll);

  let candidate = header.parentElement;
  while (candidate !== null && candidate !== document.body) {
    hosts.push(candidate);
    candidate = candidate.parentElement;
  }
  return hosts;
}

export function MobileHeader({ title, level = 2, backLabel, onBack, actions }: Readonly<{
  title: string;
  level?: 1 | 2;
  backLabel?: string;
  onBack?: () => void;
  actions?: ReactNode;
}>) {
  const headerRef = useRef<HTMLElement | null>(null);
  const [scrolled, setScrolled] = useState(false);

  useEffect(() => {
    const header = headerRef.current;
    if (header === null) return;
    const hosts = scrollHosts(header);
    const sync = () => setScrolled(hosts.some((host) => host.scrollTop > 4));
    sync();
    for (const host of hosts) host.addEventListener('scroll', sync, { passive: true });
    return () => {
      for (const host of hosts) host.removeEventListener('scroll', sync);
    };
  }, []);

  return (
    <header
      ref={headerRef}
      className={styles.header}
      data-nc-mobile-header=""
      data-nc-mobile-scrolled={scrolled ? '' : undefined}
    >
      <span className={styles.leading}>
        {onBack !== undefined && (
          <AstryxIconButton
            className={styles.back}
            label={`Back to ${backLabel ?? 'previous page'}`}
            variant="ghost"
            size="lg"
            icon={<AstryxIcon icon="chevronLeft" size="md" color="inherit" />}
            onClick={onBack}
          />
        )}
      </span>
      <AstryxHeading level={level} color="secondary" maxLines={1} className={styles.title}>
        {title}
      </AstryxHeading>
      <span className={styles.trailing}>{actions}</span>
    </header>
  );
}
