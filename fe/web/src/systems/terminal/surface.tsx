import { lazy, useEffect } from 'react';

import type { TerminalConnectionStatus } from './xterm-view.tsx';
import { useState } from '../../ui/state/public.ts';

export type { TerminalConnectionStatus } from './xterm-view.tsx';

const XtermView = lazy(async () => {
  const module = await import('./xterm-view.tsx');
  return { default: module.XtermView };
});

function readDocumentTheme(): 'light' | 'dark' {
  return document.documentElement.dataset.theme === 'light' ? 'light' : 'dark';
}

export function TerminalSurface({ card, visible = true, onStatusChange }: {
  card: { readonly id: string; readonly terminalId: string | null };
  visible?: boolean;
  onStatusChange?: (status: TerminalConnectionStatus) => void;
}) {
  const [resolved, setResolved] = useState<'light' | 'dark'>(readDocumentTheme);
  useEffect(() => {
    const root = document.documentElement;
    const sync = () => { setResolved(readDocumentTheme()); };
    const observer = new MutationObserver(sync);
    observer.observe(root, { attributes: true, attributeFilter: ['data-theme'] });
    return () => observer.disconnect();
  }, []);
  if (card.terminalId === null) return null;
  return <XtermView terminalId={card.terminalId} theme={resolved} visible={visible} onStatusChange={onStatusChange} />;
}
