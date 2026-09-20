/** Host-theme RGB tuples, wire shape of the daemon's `TerminalTheme`. bg matches the host paper rather than xterm's clearColor, because the daemon advertises these on OSC 10/11. */
export const LIGHT_THEME_RGB = {
  fg: [42, 47, 58] as [number, number, number],
  bg: [252, 254, 255] as [number, number, number],
};

export const DARK_THEME_RGB = {
  fg: [216, 219, 226] as [number, number, number],
  bg: [15, 20, 24] as [number, number, number],
};
