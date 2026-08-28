// Crypto-strong UUID v4 with a fallback path for non-secure contexts.
//
// Why this exists: `crypto.randomUUID()` is restricted to secure contexts
// (https + localhost). When the app is served over plain http from a LAN
// IP (e.g. http://192.168.x.x:4040 in a dev cluster), the method is
// `undefined` and any caller throws `TypeError: crypto.randomUUID is not
// a function` — see XtermView's WS client_id and plugin-iframe's MCP
// call_id. `crypto.getRandomValues` IS available in non-secure contexts,
// so we synthesise a v4 UUID from 16 random bytes when the high-level
// helper isn't there.
export function makeUuid(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
  bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 10xx
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
