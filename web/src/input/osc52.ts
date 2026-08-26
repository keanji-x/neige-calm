// OSC 52 clipboard writes from PTY apps (tmux, vim, grok `y`, …).
// xterm.js does not implement this by default; without a handler the
// sequence is dropped (or worse, painted) and never reaches the browser
// clipboard. Query (`Pd = ?`) is refused: a page must not be able to
// read the system clipboard just because a child printed an escape.

export const OSC52_MAX_DECODED_BYTES = 1024 * 1024;

export type Osc52Action =
  | { kind: 'write'; text: string }
  | { kind: 'clear' }
  | { kind: 'ignore' };

export function parseOsc52Payload(data: string): Osc52Action {
  const sep = data.indexOf(';');
  if (sep < 0) return { kind: 'ignore' };
  const selection = data.slice(0, sep);
  if (
    selection !== '' &&
    selection !== 'c' &&
    selection !== 'p' &&
    selection !== 's'
  ) {
    return { kind: 'ignore' };
  }
  const payload = data.slice(sep + 1).replace(/\s+/g, '');
  if (payload === '?') return { kind: 'ignore' };
  if (payload === '') return { kind: 'clear' };
  let binary: string;
  try {
    binary = atob(payload);
  } catch {
    return { kind: 'ignore' };
  }
  if (binary.length > OSC52_MAX_DECODED_BYTES) return { kind: 'ignore' };
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return {
    kind: 'write',
    text: new TextDecoder('utf-8', { fatal: false }).decode(bytes),
  };
}

export function copyTextToClipboard(text: string): Promise<boolean> {
  const clipboard = navigator.clipboard;
  if (clipboard && typeof clipboard.writeText === 'function') {
    return clipboard.writeText(text).then(
      () => true,
      () => fallbackExecCommandCopy(text),
    );
  }
  return Promise.resolve(fallbackExecCommandCopy(text));
}

function fallbackExecCommandCopy(text: string): boolean {
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.setAttribute('readonly', '');
  ta.style.position = 'fixed';
  ta.style.top = '0';
  ta.style.left = '-9999px';
  document.body.append(ta);
  ta.select();
  let ok = false;
  try {
    ok = document.execCommand('copy');
  } catch {
    ok = false;
  }
  ta.remove();
  return ok;
}

export function createOsc52Handler(
  writeText: (text: string) => void | Promise<unknown> = copyTextToClipboard,
): (data: string) => boolean {
  return (data) => {
    const action = parseOsc52Payload(data);
    if (action.kind === 'write') {
      void Promise.resolve(writeText(action.text));
    } else if (action.kind === 'clear') {
      void Promise.resolve(writeText(''));
    }
    return true;
  };
}
