/**
 * Default theme RGB values used as a placeholder by callers that
 * don't yet have a host-browser-derived theme to forward to the
 * server (#177).
 *
 * The matching Rust sentinel lives at `RequestTheme::default_dark()`
 * in `crates/calm-server/src/routes/theme.rs`. PR4 of the #177 split
 * wires the real host-theme read on the frontend; until then every
 * caller passes this dark default so the new required `theme` field
 * on `NewWave` / `NewCodexCardBody` / `NewTerminalCardBody` is
 * satisfied at the type layer.
 *
 * Values: foreground `216,219,226`, background `15,20,24` — chosen
 * to match the dark-mode browser palette neige-calm ships with.
 */
export const DARK_THEME_RGB = {
  fg: [216, 219, 226] as [number, number, number],
  bg: [15, 20, 24] as [number, number, number],
};
