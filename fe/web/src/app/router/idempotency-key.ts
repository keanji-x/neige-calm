// `randomUUID` is unavailable on the app's supported insecure LAN origin.
let fallbackMints = 0;

export function mintIdempotencyKey(): string {
  const bytes = new Uint8Array(16);
  const source: Crypto | undefined = globalThis.crypto;
  if (typeof source?.getRandomValues === 'function') source.getRandomValues(bytes);
  else {
    for (let index = 0; index < bytes.length; index += 1) bytes[index] = Math.floor(Math.random() * 256);
    // Assignment guarantees distinct fallback mints even if Math.random repeats.
    fallbackMints += 1;
    bytes[0] = (fallbackMints >>> 24) & 0xff;
    bytes[1] = (fallbackMints >>> 16) & 0xff;
    bytes[2] = (fallbackMints >>> 8) & 0xff;
    bytes[3] = fallbackMints & 0xff;
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
