import type { ListTextRole } from '../list-typography/public.tsx';
import styles from './public.module.css';

export type NavigationRowPart = 'row' | 'content' | 'labelStart' | 'twoActions'
  | 'leading' | 'inner' | 'outer' | 'chromeOuter' | 'inlineActions' | 'revealActions';
export type NavigationRowSpec = Readonly<
  { kind: 'caption'; depth: 0 } | { kind: 'entry'; depth: 0 | 1 }
>;
export type NavigationEntrySpec = Extract<NavigationRowSpec, { kind: 'entry' }>;

const kinds = Object.freeze({
  caption: Object.freeze({ tone: 'secondary' as const, className: styles.caption }),
  entry: Object.freeze({ tone: 'primary' as const, className: '' }),
});

/** Required content-region schema. CSS alone owns columns and the single depth relationship. */
export function navigationRowPresentation(presentation: NavigationRowSpec): Readonly<{ className: string; tone: ListTextRole }> {
  return { className: [styles.row, kinds[presentation.kind].className, presentation.depth === 1 ? styles.nested : ''].filter(Boolean).join(' '),
    tone: kinds[presentation.kind].tone };
}

/** Geometry-only slots remain available for surrounding chrome. */
export function navigationRowClass(part: NavigationRowPart): string {
  return styles[part];
}
