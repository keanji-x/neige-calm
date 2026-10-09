import { useEffect, useSyncExternalStore, type ComponentType } from 'react';
import { useState } from '../state/public.ts';
import { CopyCodeButton } from './copy-button.tsx';
import type { CodeSource } from './language.ts';
import styles from './code.module.css';

export { resolveCodeLanguage, exceedsCodeHighlightLimits, CODE_HIGHLIGHT_LIMITS } from './language.ts';
export type { CodeSource } from './language.ts';
export { useCodeLanguage } from './use-language.ts';

export type CodeTheme = 'light' | 'dark';
export type ReadOnlyCodeProps = Readonly<{ text: string; source: CodeSource; theme?: CodeTheme }>;

function hostTheme(): CodeTheme {
  return document.documentElement.dataset.theme === 'light' ? 'light' : 'dark';
}
function subscribeTheme(onChange: () => void) {
  const observer = new MutationObserver(onChange);
  observer.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
  return () => { observer.disconnect(); };
}

/** Owns only presentation. No file reads, navigation, search adapter, or edit lifecycle.
 * The host may set a theme; otherwise changes to its declared data-theme are observed.
 * Source is always preserved while the display/grammar loads. */
export function ReadOnlyCode({ text, source, theme }: ReadOnlyCodeProps) {
  const currentTheme = useSyncExternalStore(subscribeTheme, hostTheme, () => 'dark' as const);
  const [Pane, setPane] = useState<ComponentType<ReadOnlyCodeProps> | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let cancelled = false;
    // A resolved effect import displays immediately; Suspense's reveal delay adds
    // ~300 ms to a transient preview even when its editor chunk is already cached.
    import('./read-only-pane.tsx').then(
      module => { if (!cancelled) setPane(() => module.ReadOnlyPane); },
      () => { if (!cancelled) setFailed(true); },
    );
    return () => { cancelled = true; };
  }, [setPane, setFailed]);
  if (Pane !== null) return <Pane text={text} source={source} theme={theme ?? currentTheme} />;
  return <div className={styles.code}>
    <div className={styles.toolbar}>
      <span>{failed ? 'Plain text · code display unavailable' : 'Loading code…'}</span>
      <CopyCodeButton text={text} />
    </div>
    <pre className={styles.fallback}><code>{text}</code></pre>
  </div>;
}

/** Astryx Markdown's declared fence renderer contract. */
export function MarkdownCode({ code, language }: Readonly<{ code: string; language?: string }>) {
  return <ReadOnlyCode text={code} source={{ kind: 'language', value: language ?? '' }} />;
}
