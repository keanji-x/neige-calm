// OSC 52 clipboard writes from PTY apps. Query (`Pd = ?`) is refused so a
// child cannot read the system clipboard just by printing an escape.

export const OSC52_MAX_DECODED_BYTES = 1024 * 1024;
// 3 bytes → 4 base64 chars; reject before `atob` so a 10MB OSC frame
// does not allocate a decoded buffer we then throw away.
export const OSC52_MAX_ENCODED_CHARS =
  Math.ceil((OSC52_MAX_DECODED_BYTES * 4) / 3) + 4;

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
  if (payload.length > OSC52_MAX_ENCODED_CHARS) return { kind: 'ignore' };
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

export function osc52HostMayWrite(host: ParentNode | null, visible = true): boolean {
  if (!visible) return false;
  if (typeof document === 'undefined' || host === null) return false;
  if (document.hidden) return false;
  if (typeof document.hasFocus === 'function' && !document.hasFocus()) {
    return false;
  }
  const active = document.activeElement;
  if (active === null) return true;
  if (host.contains(active)) return true;
  // After a remount the helper textarea is tabindex=-1, so focus sits
  // on <body> until the user clicks the card. A focused, visible tab is
  // enough to accept a TUI clipboard write.
  return active === document.body || active === document.documentElement;
}

export function createOsc52Handler(
  writeText: (text: string) => void | Promise<unknown> = copyTextToClipboard,
  mayWrite: () => boolean = () => true,
): (data: string) => boolean {
  return (data) => {
    const action = parseOsc52Payload(data);
    if (action.kind === 'write' || action.kind === 'clear') {
      if (!mayWrite()) return true;
      void Promise.resolve(
        writeText(action.kind === 'write' ? action.text : ''),
      );
    }
    return true;
  };
}
