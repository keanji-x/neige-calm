/** Reads `<html data-theme>` (mirrored synchronously by `app/theme`) rather than the context: subscribing would re-render and remount live editors/terminals. Anything but `light` is dark, the app default. */
export function readHostTheme(): 'light' | 'dark' {
  if (typeof document === 'undefined') return 'dark';
  return document.documentElement.dataset.theme === 'light' ? 'light' : 'dark';
}
