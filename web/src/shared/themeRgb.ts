// Host-theme RGB tuples (#177) — shared between `XtermView` (theme apply
// + mid-session `TerminalThemeUpdate` dispatch) and the codex-card
// creation POST in `app/router.tsx`.
//
// Lives in its own file so importing the constants doesn't drag in the
// heavy `XtermView` module statically and defeat its `lazy(...)` import
// from the codex / terminal cards.
//
// The fg numbers mirror `LIGHT_THEME.foreground` / `DARK_THEME.foreground`
// from XtermView.tsx; the bg numbers match the host paper (`#fcfeff`
// light, `#0f1418` dark) rather than xterm's transparent clearColor —
// they're what the daemon advertises to codex on OSC 10/11 so the TUI
// composer aligns with the surrounding card background.

export const LIGHT_THEME_RGB = {
  fg: [42, 47, 58] as [number, number, number],
  bg: [252, 254, 255] as [number, number, number],
};

export const DARK_THEME_RGB = {
  fg: [216, 219, 226] as [number, number, number],
  bg: [15, 20, 24] as [number, number, number],
};
