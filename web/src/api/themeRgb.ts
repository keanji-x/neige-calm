/**
 * Host-theme RGB tuples (#177) — shared between `XtermView` (theme
 * apply + mid-session `TerminalThemeUpdate` dispatch) and the
 * card/wave-create POSTs in `app/router.tsx` / `hooks/useTodayTerminal.ts`.
 *
 * Wire shape matches the server's `RequestTheme` / the daemon's
 * `TerminalTheme` value type: `{ fg: [r, g, b], bg: [r, g, b] }`. The
 * kernel stamps these onto the spawning daemon's argv so codex's OSC
 * 10/11 startup probe gets matching colors.
 *
 * The fg numbers mirror `LIGHT_THEME.foreground` / `DARK_THEME.foreground`
 * in `XtermView.tsx`. The bg numbers match the host paper (`#fcfeff`
 * light, `#0f1418` dark) rather than xterm's transparent clearColor —
 * the daemon advertises *these* on OSC 10/11 so the codex composer
 * aligns with the surrounding card background, not with whatever
 * xterm.js happens to clear into.
 *
 * The matching Rust sentinel `RequestTheme::default_dark()` lives in
 * `crates/calm-server/src/routes/theme.rs`. Tests use it as a no-op
 * value when they don't care about the theme.
 */
export const LIGHT_THEME_RGB = {
  fg: [42, 47, 58] as [number, number, number],
  bg: [252, 254, 255] as [number, number, number],
};

export const DARK_THEME_RGB = {
  fg: [216, 219, 226] as [number, number, number],
  bg: [15, 20, 24] as [number, number, number],
};
