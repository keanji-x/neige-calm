export type ThemeRgb = Readonly<{
  fg: readonly [number, number, number];
  bg: readonly [number, number, number];
}>;

export { LIGHT_THEME_RGB, DARK_THEME_RGB } from '../../styles/theme-values.ts';
import { LIGHT_THEME_RGB, DARK_THEME_RGB } from '../../styles/theme-values.ts';

/** Synchronous escape hatch for card creation paths that must not subscribe to ThemeContext. */
export function readHostThemeRgb(root: Pick<HTMLElement, 'dataset'> = document.documentElement): ThemeRgb {
  return root.dataset.theme === 'light' ? LIGHT_THEME_RGB : DARK_THEME_RGB;
}
