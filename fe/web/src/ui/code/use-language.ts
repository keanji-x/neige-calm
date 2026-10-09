import { useEffect, useMemo } from 'react';
import type { LanguageDescription, LanguageSupport } from '@codemirror/language';
import { useState } from '../state/public.ts';
import { exceedsCodeHighlightLimits, resolveCodeLanguage, type CodeSource } from './language.ts';

export function useCodeLanguage(source: CodeSource, text: string) {
  const description = useMemo(() => resolveCodeLanguage({ kind: source.kind, value: source.value }), [source.kind, source.value]);
  const limited = useMemo(() => exceedsCodeHighlightLimits(text), [text]);
  const [loaded, setLoaded] = useState<Readonly<{
    description: LanguageDescription | null; support: LanguageSupport | null; failed: boolean;
  }>>({ description: null, support: null, failed: false });
  useEffect(() => {
    if (description === null || limited) return;
    let cancelled = false;
    description.load().then(
      support => { if (!cancelled) setLoaded({ description, support, failed: false }); },
      () => { if (!cancelled) setLoaded({ description, support: null, failed: true }); },
    );
    return () => { cancelled = true; };
  }, [description, limited, setLoaded]);
  const support = limited || loaded.description !== description ? null : loaded.support;
  const status = limited ? 'limited' : description === null ? 'unknown'
    : loaded.description === description && loaded.failed ? 'unavailable'
      : support === null ? 'loading' : 'ready';
  return { description, support, status };
}
