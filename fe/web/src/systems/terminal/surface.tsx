import { lazy, useEffect } from 'react';

import { useState } from '../../ui/state/public.ts';

const XtermView = lazy(async () => {
  const module = await import('./xterm-view.tsx');
  return { default: module.XtermView };
});

function readDocumentTheme(): 'light' | 'dark' {
  return document.documentElement.dataset.theme === 'light' ? 'light' : 'dark';
}

export function TerminalSurface({ card }: { card: { readonly id: string; readonly terminalId: string | null } }) {
  const [resolved, setResolved] = useState<'light' | 'dark'>(readDocumentTheme);
  useEffect(() => {
    const root = document.documentElement;
    const sync = () => { setResolved(readDocumentTheme()); };
    const observer = new MutationObserver(sync);
    observer.observe(root, { attributes: true, attributeFilter: ['data-theme'] });
    return () => observer.disconnect();
  }, []);
  if (card.terminalId === null) return null;
  return <XtermView terminalId={card.terminalId} theme={resolved} />;
}
