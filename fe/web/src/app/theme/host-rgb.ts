export type ThemeRgb = Readonly<{
  fg: readonly [number, number, number];
  bg: readonly [number, number, number];
}>;

export const LIGHT_THEME_RGB: ThemeRgb = Object.freeze({
  fg: Object.freeze([42, 47, 58] as const),
  bg: Object.freeze([252, 254, 255] as const),
});

export const DARK_THEME_RGB: ThemeRgb = Object.freeze({
  fg: Object.freeze([216, 219, 226] as const),
  bg: Object.freeze([15, 20, 24] as const),
});

/** Synchronous escape hatch for card creation paths that must not subscribe to ThemeContext (#177). */
export function readHostThemeRgb(root: Pick<HTMLElement, 'dataset'> = document.documentElement): ThemeRgb {
  return root.dataset.theme === 'light' ? LIGHT_THEME_RGB : DARK_THEME_RGB;
}
