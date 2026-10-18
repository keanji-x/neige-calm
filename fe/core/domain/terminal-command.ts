/** /terminal is a view command only. No suffix is ever forwarded as shell input. */
export function terminalCommand(text: string): 'open' | 'arguments' | null {
  const value = text.trim();
  if (!/^\/terminal(?:\s|$)/u.test(value)) return null;
  return value === '/terminal' ? 'open' : 'arguments';
}

export const TERMINAL_ARGUMENTS_MESSAGE = 'Use /terminal without arguments. Shell commands are entered in the terminal.';

export const TERMINAL_DESKTOP_MESSAGE = 'Terminal workbench is available on desktop.';
