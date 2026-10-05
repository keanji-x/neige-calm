import { useState } from '../../ui/state/public.ts';

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

/** One keyed create's request: its key, the draft it was made for, and the body sent under the key on every attempt. */
export type KeyedRequest<Draft, Body> = Readonly<{ key: string; draft: Draft; body: Body }>;

/**
 * One keyed create's intent (#2131): the request held from its first press until its outcome is final. `request` for a
 * draft `sameDraft` reads as the held one's gives the held key and body (built once, at the first press), so a retry
 * after a lost answer joins what the first attempt made; any other draft, and every `restart`, is a new intent under a
 * new key. `release` drops a request whose outcome is final (made, or refused), so the next press mints a new key; a
 * newer held request is kept.
 */
export function useKeyedIntent<Draft, Body>(sameDraft: (held: Draft, next: Draft) => boolean) {
  const [held, setHeld] = useState<KeyedRequest<Draft, Body> | null>(null);
  const hold = (request: KeyedRequest<Draft, Body>) => { setHeld(request); return request; };
  const mint = (draft: Draft, build: () => Body) => Object.freeze({ key: mintIdempotencyKey(), draft, body: build() });
  return {
    held,
    request: (draft: Draft, build: () => Body) => hold(held !== null && sameDraft(held.draft, draft) ? held : mint(draft, build)),
    restart: (draft: Draft, build: () => Body) => hold(mint(draft, build)),
    release: (request: KeyedRequest<Draft, Body>) => { setHeld((current) => (current === request ? null : current)); },
  };
}
