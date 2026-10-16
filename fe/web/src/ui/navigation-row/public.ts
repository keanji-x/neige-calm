import styles from './public.module.css';

export type NavigationRowPart = 'row' | 'content' | 'labelStart' | 'twoActions'
  | 'leading' | 'inner' | 'outer' | 'chromeOuter' | 'inlineActions' | 'revealActions';

/** Typed geometry slots; keep the CSS import colocated with its UI owner. */
export function navigationRowClass(part: NavigationRowPart): string {
  return styles[part];
}
