import { lazy, Suspense, useSyncExternalStore } from 'react';
import type { CodeSource } from './language.ts';
import styles from './code.module.css';

export { resolveCodeLanguage, exceedsCodeHighlightLimits, CODE_HIGHLIGHT_LIMITS } from './language.ts';
export type { CodeSource } from './language.ts';
export { useCodeLanguage } from './use-language.ts';

export type CodeTheme = 'light' | 'dark';
export type ReadOnlyCodeProps = Readonly<{ text: string; source: CodeSource; theme?: CodeTheme }>;

const LazyReadOnlyPane = lazy(() => import('./read-only-pane.tsx').then(module => ({ default: module.ReadOnlyPane })));

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
  return <Suspense fallback={<pre className={styles.fallback}><code>{text}</code></pre>}>
    <LazyReadOnlyPane text={text} source={source} theme={theme ?? currentTheme} />
  </Suspense>;
}

/** Astryx Markdown's declared fence renderer contract. */
export function MarkdownCode({ code, language }: Readonly<{ code: string; language?: string }>) {
  return <ReadOnlyCode text={code} source={{ kind: 'language', value: language ?? '' }} />;
}
